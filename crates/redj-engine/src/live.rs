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
use crate::transition::{
    LiveTransition, TransitionState, TransitionStatus, param_bit, plan_params,
};

pub const MAX_DECKS: usize = 4;
pub const HOT_CUES: usize = 8;
/// Jog wheel resolution: ticks per revolution at 33 1/3 rpm.
const JOG_TICKS_PER_REV: f64 = 720.0;
const JOG_SEC_PER_REV: f64 = 1.8;
/// Largest block processed in one pass (longer buffers are split).
pub const MAX_BLOCK: usize = 512;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum XfSide {
    Left,
    #[default]
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
    /// Arms a plan-driven transition (replaces an armed one; refused while
    /// another is active). The incoming track must already be loaded on
    /// `in_deck`.
    ArmTransition(Box<LiveTransition>),
    /// Cancels the armed or active transition, leaving decks as they are.
    CancelTransition,
}

/// Objects handed back to be dropped outside the audio thread.
#[derive(Debug)]
pub enum Garbage {
    Track(Arc<TrackAudio>),
    Transition(Box<LiveTransition>),
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
    /// Channel strip values in effect (including AI automation).
    pub volume: f32,
    pub eq_db: [f32; 3],
    pub filter: f32,
    pub echo: f32,
    /// Tempo fader position -1..1.
    pub tempo: f32,
    pub xf: XfSide,
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
    pub transition: TransitionStatus,
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
    /// Strip parameters in effect at the end of the last block.
    eff: StripParams,
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
            eff: params,
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
    transition: Option<Box<LiveTransition>>,
    tr_state: TransitionState,
    /// Human-owned parameters (bits 0..8 outgoing deck, 8..16 incoming).
    tr_human: u16,
    tr_beat: f64,
    tr_completed: u32,
    tr_cancelled: u32,
    tr_last_id: u32,
    /// Master beat of the transition plan per frame of the block.
    mbuf: Box<[f64]>,
}

impl LiveEngine {
    pub fn new(cfg: LiveConfig) -> (LiveEngine, EngineHandle) {
        let (cmd_tx, cmd_rx) = rtrb::RingBuffer::new(1024);
        let st = Arc::new(std::sync::Mutex::new(Status::default()));
        // On macOS the std mutex allocates its pthread mutex on first use;
        // take that allocation here rather than in the audio callback.
        drop(st.lock());
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
            transition: None,
            tr_state: TransitionState::Idle,
            tr_human: 0,
            tr_beat: 0.0,
            tr_completed: 0,
            tr_cancelled: 0,
            tr_last_id: 0,
            mbuf: vec![0.0; MAX_BLOCK].into_boxed_slice(),
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

    /// Deck whose transport a command touches.
    fn transport_deck(c: &Command) -> Option<usize> {
        match *c {
            Command::Play { deck, .. }
            | Command::TogglePlay { deck }
            | Command::Cue { deck, .. }
            | Command::Seek { deck, .. }
            | Command::BeatJump { deck, .. }
            | Command::HotCue { deck, .. }
            | Command::BeatLoop { deck, .. }
            | Command::LoopIn { deck }
            | Command::LoopOut { deck }
            | Command::Reloop { deck }
            | Command::LoopScale { deck, .. }
            | Command::Jog { deck, .. }
            | Command::JogTouch { deck, on: true }
            | Command::Sync { deck, .. }
            | Command::Load { deck, .. }
            | Command::Eject { deck } => Some(deck),
            _ => None,
        }
    }

    /// Human input during a transition: parameter moves take ownership of
    /// that parameter; transport actions on the decks involved cancel the
    /// transition (on the outgoing deck only once it is running, since the
    /// plan follows the outgoing playhead anyway).
    fn arbitrate(&mut self, c: &Command) {
        let Some(t) = &self.transition else {
            return;
        };
        let (o, i) = (t.out_deck, t.in_deck);
        let active = self.tr_state == TransitionState::Active;
        if let Command::Param { deck, param, .. } = *c
            && (deck == o || deck == i)
        {
            let p = match param {
                DeckParam::Volume => Some(redj_plan::Param::Volume),
                DeckParam::EqLow => Some(redj_plan::Param::EqLow),
                DeckParam::EqMid => Some(redj_plan::Param::EqMid),
                DeckParam::EqHigh => Some(redj_plan::Param::EqHigh),
                DeckParam::Filter => Some(redj_plan::Param::Filter),
                DeckParam::Echo => Some(redj_plan::Param::Echo),
                DeckParam::Trim | DeckParam::Tempo => None,
            };
            if let Some(p) = p {
                self.tr_human |= param_bit(p) << if deck == o { 0 } else { 8 };
            }
            return;
        }
        if let Some(d) = Self::transport_deck(c) {
            let load = matches!(c, Command::Load { .. } | Command::Eject { .. });
            if d == i || (d == o && (active || load)) {
                self.cancel_transition(true);
            }
        }
    }

    fn cancel_transition(&mut self, by_human: bool) {
        if let Some(t) = self.transition.take() {
            self.tr_last_id = t.id;
            if by_human {
                self.tr_cancelled += 1;
            }
            let _ = self.garbage_tx.push(Garbage::Transition(t));
        }
        self.tr_state = TransitionState::Idle;
    }

    fn arm_transition(&mut self, t: Box<LiveTransition>) {
        if self.tr_state == TransitionState::Active
            || t.out_deck >= self.decks.len()
            || t.in_deck >= self.decks.len()
            || t.out_deck == t.in_deck
            || t.timeline.clips.len() < 2
        {
            let _ = self.garbage_tx.push(Garbage::Transition(t));
            return;
        }
        self.cancel_transition(false);
        let d = &mut self.decks[t.in_deck];
        if let Some(a) = d.audio.clone() {
            let start = t.start_beat();
            let tb = t.timeline.clips[1]
                .track_beat(start)
                .unwrap_or(t.timeline.clips[1].playhead[0].track_start);
            d.position = a.beat_to_frame(tb).max(0.0);
            d.playing = false;
            d.loop_active = false;
            d.sync = false;
        }
        self.tr_human = 0;
        self.tr_state = TransitionState::Armed;
        self.transition = Some(t);
    }

    fn apply(&mut self, c: Command) {
        self.arbitrate(&c);
        match c {
            Command::ArmTransition(t) => self.arm_transition(t),
            Command::CancelTransition => self.cancel_transition(false),
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

    /// Renders audio: `master` and `phones` are interleaved stereo of the
    /// same length (overwritten). Long buffers are processed in blocks of
    /// at most [`MAX_BLOCK`] frames.
    pub fn process(&mut self, master: &mut [f32], phones: &mut [f32]) {
        let frames = master.len().min(phones.len()) / 2;
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(MAX_BLOCK);
            self.process_block(
                &mut master[done * 2..(done + n) * 2],
                &mut phones[done * 2..(done + n) * 2],
            );
            done += n;
        }
        if frames == 0 {
            self.process_block(&mut [], &mut []);
        }
    }

    /// Fills `mbuf` with the transition plan's master beat for every frame
    /// of the block and updates the transition state. Returns whether the
    /// transition is running in this block.
    fn transition_beats(&mut self, t: &LiveTransition, frames: usize) -> bool {
        let rate = self.deck_rate(t.out_deck);
        let d = &self.decks[t.out_deck];
        let Some(a) = d.audio.as_ref() else {
            return false;
        };
        let step = if d.playing { rate } else { 0.0 };
        for k in 0..frames {
            self.mbuf[k] = a.frame_to_beat(d.position + step * k as f64) - t.beat_offset;
        }
        let last = if frames > 0 {
            self.mbuf[frames - 1]
        } else {
            a.frame_to_beat(d.position) - t.beat_offset
        };
        self.tr_beat = last;
        if self.tr_state == TransitionState::Armed && last >= t.start_beat() {
            self.tr_state = TransitionState::Active;
            // The plan follows the outgoing playhead: a loop would rewind it.
            self.decks[t.out_deck].loop_active = false;
            let i = &mut self.decks[t.in_deck];
            i.playing = true;
            i.vari.reset();
            i.lock.reset();
        }
        self.tr_state == TransitionState::Active
    }

    /// Hands the decks back to the human at the end of a transition.
    fn finish_transition(&mut self, t: Box<LiveTransition>) {
        let end = t.end_beat();
        let bpm = self.master_bpm;
        let sr = self.sr;
        let (o, i) = (t.out_deck, t.in_deck);
        {
            let d = &mut self.decks[o];
            d.playing = false;
            d.params.volume = 0.0;
            d.params.eq_db = [0.0; 3];
            d.params.filter = 0.0;
            d.params.echo_send = 0.0;
        }
        let owned_in = (self.tr_human >> 8) & 0xFF;
        let final_params = plan_params(
            &t.timeline,
            1,
            end,
            &self.decks[i].params,
            owned_in,
            bpm,
            sr,
        );
        let d = &mut self.decks[i];
        d.params.volume = final_params.volume;
        d.params.eq_db = final_params.eq_db;
        d.params.filter = final_params.filter;
        d.params.echo_send = final_params.echo_send;
        // Keep the incoming deck at the current tempo with its own fader.
        let ratio = bpm / d.track_bpm().max(1.0) - 1.0;
        let range = [0.06f32, 0.10, 0.16, 0.50]
            .into_iter()
            .find(|r| f64::from(*r) >= ratio.abs())
            .unwrap_or(1.0);
        d.tempo_range = range;
        d.tempo = (ratio / f64::from(range)) as f32;
        if self.master_deck == Some(o) || self.master_deck.is_none() {
            self.master_deck = Some(i);
        }
        self.tr_completed += 1;
        self.tr_last_id = t.id;
        self.tr_state = TransitionState::Idle;
        let _ = self.garbage_tx.push(Garbage::Transition(t));
    }

    fn process_block(&mut self, master: &mut [f32], phones: &mut [f32]) {
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

        let tr = self.transition.take();
        let active = match &tr {
            Some(t) => self.transition_beats(t, frames),
            None => false,
        };
        if active
            && let Some(t) = &tr
            && !self.decks[t.out_deck].playing
        {
            // The human stopped the outgoing deck mid-transition.
            self.transition = tr;
            self.cancel_transition(true);
            self.process_block_decks(master, phones, frames, None, false);
            self.publish();
            return;
        }
        self.process_block_decks(master, phones, frames, tr.as_deref(), active);
        if let Some(t) = tr {
            if active && self.tr_beat >= t.end_beat() {
                self.finish_transition(t);
            } else {
                self.transition = Some(t);
            }
        }
        self.publish();
    }

    fn process_block_decks(
        &mut self,
        master: &mut [f32],
        phones: &mut [f32],
        frames: usize,
        tr: Option<&LiveTransition>,
        active: bool,
    ) {
        let (xl, xr) = crossfader_gains(self.crossfader, self.xf_curve);
        let bpm = self.master_bpm;
        let sr = self.sr;
        for i in 0..self.decks.len() {
            let rate = self.deck_rate(i);
            let fpt = JOG_SEC_PER_REV * self.sr / JOG_TICKS_PER_REV;
            // Role in the running transition: clip index and ownership mask.
            let role = tr.filter(|_| active).and_then(|t| {
                if i == t.out_deck {
                    Some((0usize, self.tr_human & 0xFF))
                } else if i == t.in_deck {
                    Some((1usize, (self.tr_human >> 8) & 0xFF))
                } else {
                    None
                }
            });
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
            let locked = role.is_some_and(|r| r.0 == 1);
            for k in 0..frames {
                let params = match (role, tr) {
                    (Some((clip, owned)), Some(t)) => {
                        plan_params(&t.timeline, clip, self.mbuf[k], &d.params, owned, bpm, sr)
                    }
                    _ => d.params,
                };
                let mut moving = d.playing || scratch;
                if locked && let Some(t) = tr {
                    // The incoming playhead follows the plan exactly.
                    match t.timeline.clips[1].track_beat(self.mbuf[k]) {
                        Some(tb) => d.position = audio.beat_to_frame(tb),
                        None => moving = false,
                    }
                }
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
                let o = d.strip.process(l, r, &params);
                if k + 1 == frames {
                    d.eff = params;
                }
                master[2 * k] += o.main[0] * xf;
                master[2 * k + 1] += o.main[1] * xf;
                if d.pfl {
                    phones[2 * k] += o.pfl[0];
                    phones[2 * k + 1] += o.pfl[1];
                }
                d.peak = d.peak.max(o.main[0].abs()).max(o.main[1].abs());
                if locked {
                    // Position already set from the plan.
                } else if scratch {
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
            if frames == 0 {
                d.eff = d.params;
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
    }

    fn publish(&mut self) {
        let Ok(mut slot) = self.status_tx.try_lock() else {
            return; // the UI is reading: keep the peaks for the next block
        };
        let transition = match &self.transition {
            Some(t) => TransitionStatus {
                state: self.tr_state,
                id: t.id,
                out_deck: t.out_deck,
                in_deck: t.in_deck,
                beat: self.tr_beat,
                start_beat: t.start_beat(),
                end_beat: t.end_beat(),
                human: self.tr_human,
                completed: self.tr_completed,
                cancelled: self.tr_cancelled,
                last_id: self.tr_last_id,
            },
            None => TransitionStatus {
                completed: self.tr_completed,
                cancelled: self.tr_cancelled,
                last_id: self.tr_last_id,
                ..TransitionStatus::default()
            },
        };
        let mut st = Status {
            master_bpm: self.master_bpm,
            master_beat: self.master_beat,
            master_peak: std::mem::take(&mut self.master_peak),
            limiter_gain: self.master.take_meters().1,
            phones_peak: std::mem::take(&mut self.phones_peak),
            frames: self.frames,
            transition,
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
                volume: d.eff.volume,
                eq_db: d.eff.eq_db,
                filter: d.eff.filter,
                echo: d.eff.echo_send,
                tempo: d.tempo,
                xf: d.xf,
            };
        }
        *slot = st;
    }
}
