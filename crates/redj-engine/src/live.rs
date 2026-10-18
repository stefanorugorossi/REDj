//! Live engine: decks with transport under human control, mixer, headphone
//! cue, master clock.
//!
//! The UI and MIDI threads talk to the audio thread only through lock-free
//! single-producer/single-consumer queues: [`Command`]s in, [`Status`]
//! snapshots out, and replaced objects (tracks, plans) sent back on a
//! garbage queue so they are dropped off the audio thread. `process` never
//! allocates, locks or blocks.

use std::sync::Arc;

use redj_dsp::{CrossfaderCurve, SincTable, crossfader_gains};

use crate::audio::TrackAudio;
use crate::master::Master;
use crate::reader::DeckReader;
use crate::strip::{Strip, StripParams};

pub const MAX_DECKS: usize = 4;
pub const HOT_CUES: usize = 8;
/// Jog wheel resolution: ticks per revolution at 33 1/3 rpm.
const JOG_TICKS_PER_REV: f64 = 720.0;
const JOG_SEC_PER_REV: f64 = 1.8;

/// Continuous deck controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeckParam {
    /// Channel fader 0..1.
    Volume,
    /// Trim in dB (-12..12).
    Trim,
    /// EQ bands in dB (-40 kill .. +6).
    EqLow,
    EqMid,
    EqHigh,
    /// Filter knob -1..1.
    Filter,
    /// Echo send 0..1.
    Echo,
    /// Tempo fader -1..1 (scaled by the pitch range).
    Tempo,
}

/// Crossfader assignment of a deck.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XfSide {
    Left,
    Thru,
    Right,
}

/// Messages from the control threads to the audio thread.
#[derive(Debug)]
pub enum Command {
    /// Loads a track; the cue point is set at `cue_frame`.
    Load {
        deck: usize,
        audio: Arc<TrackAudio>,
        cue_frame: f64,
        generation: u32,
    },
    Eject {
        deck: usize,
    },
    Play {
        deck: usize,
        on: bool,
    },
    TogglePlay {
        deck: usize,
    },
    /// Pioneer-style CUE button: press and release.
    Cue {
        deck: usize,
        pressed: bool,
    },
    /// Jump to an absolute source frame.
    Seek {
        deck: usize,
        frame: f64,
    },
    BeatJump {
        deck: usize,
        beats: f64,
    },
    /// Hot cue pad: jumps if set, otherwise stores the current position.
    HotCue {
        deck: usize,
        index: usize,
    },
    ClearHotCue {
        deck: usize,
        index: usize,
    },
    /// Toggles a quantized loop of `beats` from the current beat.
    BeatLoop {
        deck: usize,
        beats: f64,
    },
    LoopIn {
        deck: usize,
    },
    LoopOut {
        deck: usize,
    },
    /// Exits an active loop or re-enables the last one.
    Reloop {
        deck: usize,
    },
    /// Halves (`factor` 0.5) or doubles the active loop.
    LoopScale {
        deck: usize,
        factor: f64,
    },
    Sync {
        deck: usize,
        on: bool,
    },
    SetMaster {
        deck: usize,
    },
    Keylock {
        deck: usize,
        on: bool,
    },
    Quantize {
        deck: usize,
        on: bool,
    },
    /// Tempo fader range as a fraction (0.08 = ±8%).
    TempoRange {
        deck: usize,
        range: f32,
    },
    Param {
        deck: usize,
        param: DeckParam,
        value: f32,
    },
    Pfl {
        deck: usize,
        on: bool,
    },
    JogTouch {
        deck: usize,
        on: bool,
    },
    /// Jog movement in ticks (720 per revolution, signed).
    Jog {
        deck: usize,
        ticks: f32,
    },
    XfAssign {
        deck: usize,
        side: XfSide,
    },
    Crossfader(f32),
    CrossfaderCurve(CrossfaderCurve),
    /// Master gain in dB.
    MasterGain(f32),
    /// Headphone mix: 0 = cue only, 1 = master only.
    CueMix(f32),
    /// Headphone gain in dB.
    PhonesGain(f32),
}

/// Objects handed back to be dropped outside the audio thread.
#[derive(Debug)]
pub enum Garbage {
    Track(Arc<TrackAudio>),
}

/// Snapshot of one deck.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DeckStatus {
    pub loaded: bool,
    /// Load counter, to match the snapshot with the track the UI loaded.
    pub generation: u32,
    pub position: f64,
    pub frames: usize,
    pub playing: bool,
    /// Track beat at the playhead.
    pub beat: f64,
    /// Current playback tempo (track BPM × rate).
    pub bpm: f64,
    /// Track's own tempo.
    pub track_bpm: f64,
    /// Playback rate (1 = original speed).
    pub rate: f64,
    pub tempo_range: f32,
    pub cue: f64,
    pub hot_cues: [Option<f64>; HOT_CUES],
    pub loop_active: bool,
    /// Loop in/out (source frames) of the active or last loop.
    pub loop_range: Option<(f64, f64)>,
    pub sync: bool,
    pub is_master: bool,
    pub keylock: bool,
    pub quantize: bool,
    pub pfl: bool,
    /// Peak post-fader level since the previous snapshot.
    pub peak: f32,
    pub loaded_fraction: f32,
}

/// Snapshot of the engine.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Status {
    pub decks: [DeckStatus; MAX_DECKS],
    pub master_bpm: f64,
    pub master_beat: f64,
    pub master_peak: f32,
    /// Lowest limiter gain since the previous snapshot.
    pub limiter_gain: f32,
    pub phones_peak: f32,
    /// Output frames processed so far.
    pub frames: u64,
}

#[derive(Debug)]
struct Deck {
    audio: Option<Arc<TrackAudio>>,
    generation: u32,
    vari: DeckReader,
    lock: DeckReader,
    keylock: bool,
    /// Which reader produced the previous frame.
    used_lock: bool,
    strip: Strip,
    params: StripParams,
    tempo: f32,
    tempo_range: f32,
    position: f64,
    playing: bool,
    cue: f64,
    cue_held: bool,
    hot_cues: [Option<f64>; HOT_CUES],
    loop_range: Option<(f64, f64)>,
    loop_active: bool,
    loop_in_pending: Option<f64>,
    sync: bool,
    quantize: bool,
    pfl: bool,
    xf: XfSide,
    jog_touched: bool,
    jog_pending: f64,
    scratch_velocity: f64,
    nudge: f64,
    /// Phase correction from beat sync (rate offset).
    sync_correction: f64,
    peak: f32,
}

impl Deck {
    fn new(sample_rate: u32, sinc: Arc<SincTable>, xf: XfSide) -> Self {
        let vari = DeckReader::new(sample_rate, sinc.clone());
        let mut lock = DeckReader::new(sample_rate, sinc);
        lock.set_keylock(true);
        let params = StripParams {
            echo_feedback: 0.55,
            ..StripParams::default()
        };
        Self {
            audio: None,
            generation: 0,
            vari,
            lock,
            keylock: true,
            used_lock: false,
            strip: Strip::new(sample_rate),
            params,
            tempo: 0.0,
            tempo_range: 0.08,
            position: 0.0,
            playing: false,
            cue: 0.0,
            cue_held: false,
            hot_cues: [None; HOT_CUES],
            loop_range: None,
            loop_active: false,
            loop_in_pending: None,
            sync: false,
            quantize: true,
            pfl: false,
            xf,
            jog_touched: false,
            jog_pending: 0.0,
            scratch_velocity: 0.0,
            nudge: 0.0,
            sync_correction: 0.0,
            peak: 0.0,
        }
    }

    fn track_bpm(&self) -> f64 {
        self.audio
            .as_ref()
            .map(|a| a.grid.bpm_at(self.beat()))
            .unwrap_or(0.0)
    }

    fn beat(&self) -> f64 {
        self.audio
            .as_ref()
            .map(|a| a.frame_to_beat(self.position))
            .unwrap_or(0.0)
    }

    /// Fader rate without sync corrections and nudges.
    fn base_rate(&self) -> f64 {
        1.0 + f64::from(self.tempo * self.tempo_range)
    }

    fn quantized(&self, frame: f64) -> f64 {
        match &self.audio {
            Some(a) if self.quantize => a.beat_to_frame(a.frame_to_beat(frame).round()),
            _ => frame,
        }
    }

    fn jump(&mut self, frame: f64) {
        let max = self
            .audio
            .as_ref()
            .map(|a| a.frames() as f64)
            .unwrap_or(0.0);
        self.position = frame.clamp(0.0, max);
    }
}

/// Control-side handle: send commands, receive status and garbage.
#[derive(Debug)]
pub struct EngineHandle {
    commands: rtrb::Producer<Command>,
    status: Arc<std::sync::Mutex<Status>>,
    garbage: rtrb::Consumer<Garbage>,
    last: Status,
}

impl EngineHandle {
    /// Queues a command; returns it back if the queue is full.
    pub fn send(&mut self, c: Command) -> Result<(), Command> {
        self.commands.push(c).map_err(|rtrb::PushError::Full(c)| c)
    }

    /// Latest status (drains the queue) and drops returned garbage.
    pub fn status(&mut self) -> Status {
        if let Ok(s) = self.status.lock() {
            self.last = *s;
        }
        while self.garbage.pop().is_ok() {}
        self.last
    }
}

/// Engine configuration for live use.
#[derive(Debug, Clone, Copy)]
pub struct LiveConfig {
    pub sample_rate: u32,
    pub decks: usize,
    pub ceiling_db: f32,
}

/// The real-time engine.
#[derive(Debug)]
pub struct LiveEngine {
    sr: f64,
    decks: Vec<Deck>,
    master: Master,
    commands: rtrb::Consumer<Command>,
    /// Latest snapshot; written with `try_lock` so the audio thread never
    /// waits for the UI.
    status_tx: Arc<std::sync::Mutex<Status>>,
    garbage_tx: rtrb::Producer<Garbage>,
    master_deck: Option<usize>,
    master_bpm: f64,
    master_beat: f64,
    crossfader: f32,
    xf_curve: CrossfaderCurve,
    cue_mix: f32,
    phones_gain: f32,
    frames: u64,
    master_peak: f32,
    phones_peak: f32,
}

impl LiveEngine {
    pub fn new(cfg: LiveConfig) -> (LiveEngine, EngineHandle) {
        let (cmd_tx, cmd_rx) = rtrb::RingBuffer::new(1024);
        let st = Arc::new(std::sync::Mutex::new(Status::default()));
        let (gb_tx, gb_rx) = rtrb::RingBuffer::new(64);
        let sinc = Arc::new(SincTable::default());
        let n = cfg.decks.clamp(1, MAX_DECKS);
        let decks = (0..n)
            .map(|i| {
                let side = match (n, i) {
                    (2, 0) | (4, 0) | (4, 2) => XfSide::Left,
                    (2, 1) | (4, 1) | (4, 3) => XfSide::Right,
                    _ => XfSide::Thru,
                };
                Deck::new(cfg.sample_rate, sinc.clone(), side)
            })
            .collect();
        let engine = LiveEngine {
            sr: f64::from(cfg.sample_rate),
            decks,
            master: Master::new(cfg.sample_rate, cfg.ceiling_db),
            commands: cmd_rx,
            status_tx: st.clone(),
            garbage_tx: gb_tx,
            master_deck: None,
            master_bpm: 120.0,
            master_beat: 0.0,
            crossfader: 0.5,
            xf_curve: CrossfaderCurve::ConstantPower,
            cue_mix: 0.0,
            phones_gain: 1.0,
            frames: 0,
            master_peak: 0.0,
            phones_peak: 0.0,
        };
        let handle = EngineHandle {
            commands: cmd_tx,
            status: st,
            garbage: gb_rx,
            last: Status::default(),
        };
        (engine, handle)
    }

    pub fn sample_rate(&self) -> f64 {
        self.sr
    }

    fn deck(&mut self, i: usize) -> Option<&mut Deck> {
        self.decks.get_mut(i)
    }

    fn apply(&mut self, c: Command) {
        match c {
            Command::Load {
                deck,
                audio,
                cue_frame,
                generation,
            } => {
                let Some(d) = self.deck(deck) else {
                    return;
                };
                let old = d.audio.replace(audio);
                d.generation = generation;
                d.position = cue_frame;
                d.cue = cue_frame;
                d.playing = false;
                d.hot_cues = [None; HOT_CUES];
                d.loop_range = None;
                d.loop_active = false;
                d.vari.reset();
                d.lock.reset();
                if let Some(old) = old {
                    let _ = self.garbage_tx.push(Garbage::Track(old));
                }
            }
            Command::Eject { deck } => {
                if let Some(d) = self.deck(deck)
                    && !d.playing
                    && let Some(old) = d.audio.take()
                {
                    let _ = self.garbage_tx.push(Garbage::Track(old));
                }
            }
            Command::Play { deck, on } => {
                if let Some(d) = self.deck(deck)
                    && d.audio.is_some()
                {
                    d.playing = on;
                    d.cue_held = false;
                }
            }
            Command::TogglePlay { deck } => {
                if let Some(d) = self.deck(deck)
                    && d.audio.is_some()
                {
                    d.playing = !d.playing || d.cue_held;
                    d.cue_held = false;
                }
            }
            Command::Cue { deck, pressed } => {
                let Some(d) = self.deck(deck) else {
                    return;
                };
                if d.audio.is_none() {
                    return;
                }
                if pressed {
                    if d.playing && !d.cue_held {
                        // Back to the cue point and stop.
                        d.playing = false;
                        d.jump(d.cue);
                    } else if (d.position - d.cue).abs() < 1.0 {
                        // Preview from the cue point while held.
                        d.playing = true;
                        d.cue_held = true;
                    } else {
                        d.cue = d.quantized(d.position);
                        d.jump(d.cue);
                    }
                } else if d.cue_held {
                    d.cue_held = false;
                    d.playing = false;
                    d.jump(d.cue);
                }
            }
            Command::Seek { deck, frame } => {
                if let Some(d) = self.deck(deck) {
                    d.jump(frame);
                }
            }
            Command::BeatJump { deck, beats } => {
                if let Some(d) = self.deck(deck)
                    && let Some(a) = d.audio.clone()
                {
                    let target = a.beat_to_frame(a.frame_to_beat(d.position) + beats);
                    if let Some((s, e)) = d.loop_range.filter(|_| d.loop_active) {
                        // Move the loop with the playhead.
                        let shift = target - d.position;
                        d.loop_range = Some((s + shift, e + shift));
                    }
                    d.jump(target);
                }
            }
            Command::HotCue { deck, index } => {
                if let Some(d) = self.deck(deck)
                    && index < HOT_CUES
                    && d.audio.is_some()
                {
                    match d.hot_cues[index] {
                        Some(f) => {
                            d.jump(f);
                            d.loop_active = false;
                        }
                        None => d.hot_cues[index] = Some(d.quantized(d.position)),
                    }
                }
            }
            Command::ClearHotCue { deck, index } => {
                if let Some(d) = self.deck(deck)
                    && index < HOT_CUES
                {
                    d.hot_cues[index] = None;
                }
            }
            Command::BeatLoop { deck, beats } => {
                if let Some(d) = self.deck(deck)
                    && let Some(a) = d.audio.clone()
                {
                    let len = |r: (f64, f64)| a.frame_to_beat(r.1) - a.frame_to_beat(r.0);
                    if d.loop_active && d.loop_range.is_some_and(|r| (len(r) - beats).abs() < 1e-6)
                    {
                        d.loop_active = false;
                    } else {
                        let b = a.frame_to_beat(d.position);
                        let start = if d.quantize { b.floor() } else { b };
                        d.loop_range =
                            Some((a.beat_to_frame(start), a.beat_to_frame(start + beats)));
                        d.loop_active = true;
                    }
                }
            }
            Command::LoopIn { deck } => {
                if let Some(d) = self.deck(deck) {
                    d.loop_in_pending = Some(d.quantized(d.position));
                }
            }
            Command::LoopOut { deck } => {
                if let Some(d) = self.deck(deck)
                    && let Some(start) = d.loop_in_pending.take()
                {
                    let end = d.quantized(d.position);
                    if end > start + 1.0 {
                        d.loop_range = Some((start, end));
                        d.loop_active = true;
                    }
                }
            }
            Command::Reloop { deck } => {
                if let Some(d) = self.deck(deck) {
                    if d.loop_active {
                        d.loop_active = false;
                    } else if let Some((s, _)) = d.loop_range {
                        d.loop_active = true;
                        d.jump(s);
                    }
                }
            }
            Command::LoopScale { deck, factor } => {
                if let Some(d) = self.deck(deck)
                    && let (Some((s, e)), Some(a)) = (d.loop_range, d.audio.clone())
                {
                    let bs = a.frame_to_beat(s);
                    let len = (a.frame_to_beat(e) - bs) * factor;
                    if (1.0 / 32.0..=64.0).contains(&len) {
                        let ne = a.beat_to_frame(bs + len);
                        d.loop_range = Some((s, ne));
                        if d.loop_active && d.position >= ne {
                            d.position = s + (d.position - s).rem_euclid(ne - s);
                        }
                    }
                }
            }
            Command::Sync { deck, on } => {
                if deck >= self.decks.len() {
                    return;
                }
                self.decks[deck].sync = on;
                if on {
                    // The first synced deck becomes the master.
                    if self.master_deck.is_none() {
                        self.master_deck = Some(deck);
                    }
                    self.snap_phase(deck);
                }
            }
            Command::SetMaster { deck } => {
                if deck < self.decks.len() {
                    self.master_deck = Some(deck);
                }
            }
            Command::Keylock { deck, on } => {
                if let Some(d) = self.deck(deck) {
                    d.keylock = on;
                }
            }
            Command::Quantize { deck, on } => {
                if let Some(d) = self.deck(deck) {
                    d.quantize = on;
                }
            }
            Command::TempoRange { deck, range } => {
                if let Some(d) = self.deck(deck) {
                    d.tempo_range = range.clamp(0.02, 1.0);
                }
            }
            Command::Param { deck, param, value } => {
                if let Some(d) = self.deck(deck) {
                    let p = &mut d.params;
                    match param {
                        DeckParam::Volume => p.volume = value.clamp(0.0, 1.0),
                        DeckParam::Trim => p.trim_db = value.clamp(-24.0, 12.0),
                        DeckParam::EqLow => p.eq_db[0] = value.clamp(-40.0, 6.0),
                        DeckParam::EqMid => p.eq_db[1] = value.clamp(-40.0, 6.0),
                        DeckParam::EqHigh => p.eq_db[2] = value.clamp(-40.0, 6.0),
                        DeckParam::Filter => p.filter = value.clamp(-1.0, 1.0),
                        DeckParam::Echo => p.echo_send = value.clamp(0.0, 1.0),
                        DeckParam::Tempo => d.tempo = value.clamp(-1.0, 1.0),
                    }
                }
            }
            Command::Pfl { deck, on } => {
                if let Some(d) = self.deck(deck) {
                    d.pfl = on;
                }
            }
            Command::JogTouch { deck, on } => {
                if let Some(d) = self.deck(deck) {
                    d.jog_touched = on;
                    d.jog_pending = 0.0;
                    d.scratch_velocity = if on { 0.0 } else { d.scratch_velocity };
                }
            }
            Command::Jog { deck, ticks } => {
                if let Some(d) = self.deck(deck) {
                    if d.jog_touched {
                        d.jog_pending += f64::from(ticks);
                    } else {
                        // Pitch bend: a temporary speed change.
                        d.nudge = (d.nudge + f64::from(ticks) * 0.004).clamp(-0.5, 0.5);
                    }
                }
            }
            Command::XfAssign { deck, side } => {
                if let Some(d) = self.deck(deck) {
                    d.xf = side;
                }
            }
            Command::Crossfader(x) => self.crossfader = x.clamp(0.0, 1.0),
            Command::CrossfaderCurve(c) => self.xf_curve = c,
            Command::MasterGain(db) => self.master.set_gain_db(db.clamp(-40.0, 12.0)),
            Command::CueMix(x) => self.cue_mix = x.clamp(0.0, 1.0),
            Command::PhonesGain(db) => self.phones_gain = 10f32.powf(db.clamp(-60.0, 12.0) / 20.0),
        }
    }

    /// Aligns the beat phase of `deck` with the master clock (small jump).
    fn snap_phase(&mut self, deck: usize) {
        if self.master_deck == Some(deck) {
            return;
        }
        let master_phase = self.master_beat.rem_euclid(1.0);
        let d = &mut self.decks[deck];
        let Some(a) = d.audio.clone() else {
            return;
        };
        let b = a.frame_to_beat(d.position);
        let mut diff = master_phase - b.rem_euclid(1.0);
        if diff > 0.5 {
            diff -= 1.0;
        } else if diff < -0.5 {
            diff += 1.0;
        }
        d.position = a.beat_to_frame(b + diff);
    }

    /// Master tempo and beat: follow the master deck while it plays.
    fn update_clock(&mut self, frames: usize) {
        // Without an explicit master, the first playing deck leads the clock.
        let leader = self.master_deck.or_else(|| {
            self.decks
                .iter()
                .position(|d| d.playing && d.audio.is_some())
        });
        if let Some(m) = leader
            && let Some(d) = self.decks.get(m)
            && d.audio.is_some()
        {
            let rate = d.base_rate();
            self.master_bpm = d.track_bpm() * rate;
            if d.playing {
                self.master_beat = d.beat();
                return;
            }
        }
        self.master_beat += self.master_bpm / 60.0 * frames as f64 / self.sr;
    }

    /// Playback rate of a deck for this block (source frames per output
    /// frame), including sync, phase correction and nudge.
    fn deck_rate(&self, i: usize) -> f64 {
        let d = &self.decks[i];
        let Some(a) = &d.audio else {
            return 0.0;
        };
        let src = f64::from(a.sample_rate) / self.sr;
        let mut rate = if d.sync && self.master_deck != Some(i) && self.master_bpm > 0.0 {
            self.master_bpm / d.track_bpm().max(1.0) * (1.0 + d.sync_correction)
        } else {
            d.base_rate()
        };
        rate *= 1.0 + d.nudge;
        rate * src
    }

    /// Renders one block: `master` and `phones` are interleaved stereo of
    /// the same length (overwritten).
    pub fn process(&mut self, master: &mut [f32], phones: &mut [f32]) {
        while let Ok(c) = self.commands.pop() {
            self.apply(c);
        }
        let frames = master.len().min(phones.len()) / 2;
        master[..frames * 2].iter_mut().for_each(|s| *s = 0.0);
        phones[..frames * 2].iter_mut().for_each(|s| *s = 0.0);
        self.update_clock(frames);

        // Beat-sync phase correction: a gentle proportional controller.
        let master_phase = self.master_beat.rem_euclid(1.0);
        for i in 0..self.decks.len() {
            let is_slave = self.decks[i].sync && self.master_deck != Some(i);
            let d = &mut self.decks[i];
            d.sync_correction = 0.0;
            if is_slave && d.playing && !d.jog_touched {
                let mut err = master_phase - d.beat().rem_euclid(1.0);
                if err > 0.5 {
                    err -= 1.0;
                } else if err < -0.5 {
                    err += 1.0;
                }
                d.sync_correction = (err * 0.5).clamp(-0.02, 0.02);
            }
            d.nudge *= 0.9;
        }

        let (xl, xr) = crossfader_gains(self.crossfader, self.xf_curve);
        for i in 0..self.decks.len() {
            let rate = self.deck_rate(i);
            let fpt = JOG_SEC_PER_REV * self.sr / JOG_TICKS_PER_REV;
            let d = &mut self.decks[i];
            let Some(audio) = d.audio.clone() else {
                continue;
            };
            // Scratching: the jog drives the playhead.
            let scratch = d.jog_touched;
            if scratch {
                let target = d.jog_pending * fpt / frames.max(1) as f64;
                d.scratch_velocity += (target - d.scratch_velocity) * 0.6;
                d.jog_pending = 0.0;
            }
            let xf = match d.xf {
                XfSide::Left => xl,
                XfSide::Right => xr,
                XfSide::Thru => 1.0,
            };
            for k in 0..frames {
                let moving = d.playing || scratch;
                let (l, r) = if moving || d.cue_held {
                    // Scratching is always varispeed.
                    let use_lock = d.keylock && !scratch;
                    if use_lock != d.used_lock {
                        d.used_lock = use_lock;
                        d.vari.reset();
                        d.lock.reset();
                    }
                    if use_lock {
                        d.lock.read(&*audio, d.position)
                    } else {
                        d.vari.read(&*audio, d.position)
                    }
                } else {
                    (0.0, 0.0)
                };
                let o = d.strip.process(l, r, &d.params);
                master[2 * k] += o.main[0] * xf;
                master[2 * k + 1] += o.main[1] * xf;
                if d.pfl {
                    phones[2 * k] += o.pfl[0];
                    phones[2 * k + 1] += o.pfl[1];
                }
                d.peak = d.peak.max(o.main[0].abs()).max(o.main[1].abs());
                if scratch {
                    d.position += d.scratch_velocity;
                } else if d.playing {
                    d.position += rate;
                }
                if d.loop_active
                    && let Some((s, e)) = d.loop_range
                    && d.position >= e
                    && e > s
                {
                    d.position = s + (d.position - e).rem_euclid(e - s);
                }
                if d.position >= audio.frames() as f64 {
                    d.position = audio.frames() as f64;
                    d.playing = false;
                }
                if d.position < 0.0 {
                    d.position = 0.0;
                }
            }
        }

        // Headphones: cue bus blended with the pre-limiter master.
        let cm = self.cue_mix;
        for k in 0..frames * 2 {
            phones[k] = (phones[k] * (1.0 - cm) + master[k] * cm) * self.phones_gain;
            phones[k] = phones[k].clamp(-1.0, 1.0);
            self.phones_peak = self.phones_peak.max(phones[k].abs());
        }
        self.master.process(&mut master[..frames * 2]);
        for s in &master[..frames * 2] {
            self.master_peak = self.master_peak.max(s.abs());
        }
        self.frames += frames as u64;
        self.publish();
    }

    fn publish(&mut self) {
        let Ok(mut slot) = self.status_tx.try_lock() else {
            return; // the UI is reading: keep the peaks for the next block
        };
        let mut st = Status {
            master_bpm: self.master_bpm,
            master_beat: self.master_beat,
            master_peak: std::mem::take(&mut self.master_peak),
            limiter_gain: self.master.take_meters().1,
            phones_peak: std::mem::take(&mut self.phones_peak),
            frames: self.frames,
            ..Status::default()
        };
        for (i, d) in self.decks.iter_mut().enumerate() {
            let src_rate = d
                .audio
                .as_ref()
                .map(|a| f64::from(a.sample_rate))
                .unwrap_or(self.sr);
            let rate = if d.sync && self.master_deck != Some(i) && self.master_bpm > 0.0 {
                self.master_bpm / d.track_bpm().max(1.0)
            } else {
                d.base_rate()
            } * (src_rate / self.sr).max(0.0);
            st.decks[i] = DeckStatus {
                loaded: d.audio.is_some(),
                generation: d.generation,
                position: d.position,
                frames: d.audio.as_ref().map(|a| a.frames()).unwrap_or(0),
                playing: d.playing,
                beat: d.beat(),
                bpm: d.track_bpm() * rate,
                track_bpm: d.track_bpm(),
                rate,
                tempo_range: d.tempo_range,
                cue: d.cue,
                hot_cues: d.hot_cues,
                loop_active: d.loop_active,
                loop_range: d.loop_range,
                sync: d.sync,
                is_master: self.master_deck == Some(i),
                keylock: d.keylock,
                quantize: d.quantize,
                pfl: d.pfl,
                peak: std::mem::take(&mut d.peak),
                loaded_fraction: d.audio.as_ref().map(|a| a.loaded_fraction()).unwrap_or(0.0),
            };
        }
        *slot = st;
    }
}
