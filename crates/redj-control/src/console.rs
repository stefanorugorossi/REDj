//! The live console: track loading, controller input, LED feedback and the
//! command channel to the audio engine. Runs on the UI thread (`poll` once
//! per frame); heavy work happens on loader threads.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use redj_core::{TrackId, TrackInfo};
use redj_engine::{Command, DeckParam, EngineHandle, MAX_DECKS, Status, TrackAudio};
use redj_midi::{
    Action, ControlEvent, DeckLeds, GlobalKnob, Knob, LedInputs, LedState, Mapper, MidiMessage,
};

#[path = "director.rs"]
mod director;

pub use director::{Director, DirectorSettings, Fallback, LogEntry, Scheduled};

use crate::session::Session;
use crate::waveform::Waveform;

/// Tempo fader ranges cycled by the TEMPO RANGE control.
pub const TEMPO_RANGES: [f32; 4] = [0.06, 0.10, 0.16, 0.50];

/// What is loaded on a deck (UI side).
#[derive(Debug, Clone)]
pub struct LoadedTrack {
    pub info: TrackInfo,
    pub audio: Arc<TrackAudio>,
    pub waveform: Arc<Waveform>,
    pub generation: u32,
}

struct LoadResult {
    deck: usize,
    generation: u32,
    result: Result<(Arc<TrackAudio>, Arc<Waveform>), String>,
    info: TrackInfo,
}

/// Fader taper: linear position to amplitude (roughly logarithmic).
pub fn fader_curve(v: f32) -> f32 {
    v.clamp(0.0, 1.0).powf(2.0)
}

/// EQ knob position (0..1, centre 0.5) to dB (-40 kill .. +6).
pub fn eq_knob_db(v: f32) -> f32 {
    if v >= 0.5 {
        (v - 0.5) / 0.5 * 6.0
    } else if v < 0.01 {
        -40.0
    } else {
        (40.0 * (v / 0.5).log10()).max(-40.0)
    }
}

/// Inverse of [`eq_knob_db`] (for drawing knobs).
pub fn eq_db_knob(db: f32) -> f32 {
    if db >= 0.0 {
        0.5 + db / 12.0
    } else if db <= -40.0 {
        0.0
    } else {
        0.5 * 10f32.powf(db / 40.0)
    }
}

/// The live console.
pub struct Console {
    engine: EngineHandle,
    session: Arc<Session>,
    sample_rate: u32,
    decks: usize,
    loaded: Vec<Option<LoadedTrack>>,
    pending: Vec<Option<TrackId>>,
    generation: u32,
    load_tx: Sender<LoadResult>,
    load_rx: Receiver<LoadResult>,
    mapper: Mapper,
    leds: LedState,
    midi_rx: Option<Receiver<MidiMessage>>,
    midi_out: Option<Box<dyn FnMut(MidiMessage) + Send>>,
    shift: [bool; MAX_DECKS],
    tempo_range: [usize; MAX_DECKS],
    /// Library cursor for the browse encoder (index into `browse_order`).
    pub browse_cursor: usize,
    browse_order: Vec<TrackId>,
    status: Status,
    pub ai_on: bool,
    /// Messages for the UI status bar.
    pub notices: Vec<String>,
    /// Normalized knob positions (0..1) per deck, indexed like [`KNOBS`].
    knob_pos: [[f32; 8]; MAX_DECKS],
    /// Normalized global control positions, indexed like [`GLOBALS`].
    global_pos: [f32; 4],
    director: Director,
}

/// Order of [`Console::knob`] storage.
pub const KNOBS: [Knob; 8] = [
    Knob::Volume,
    Knob::Trim,
    Knob::EqHigh,
    Knob::EqMid,
    Knob::EqLow,
    Knob::Filter,
    Knob::Tempo,
    Knob::Echo,
];

/// Order of [`Console::global`] storage.
pub const GLOBALS: [GlobalKnob; 4] = [
    GlobalKnob::Crossfader,
    GlobalKnob::CueMix,
    GlobalKnob::MasterLevel,
    GlobalKnob::PhonesLevel,
];

fn knob_index(k: Knob) -> usize {
    KNOBS.iter().position(|&x| x == k).unwrap_or(0)
}

fn global_index(k: GlobalKnob) -> usize {
    GLOBALS.iter().position(|&x| x == k).unwrap_or(0)
}

impl std::fmt::Debug for Console {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Console")
            .field("decks", &self.decks)
            .finish()
    }
}

impl Console {
    pub fn new(
        session: Arc<Session>,
        engine: EngineHandle,
        sample_rate: u32,
        decks: usize,
    ) -> Self {
        let (load_tx, load_rx) = channel();
        let mut c = Self {
            engine,
            session,
            sample_rate,
            decks: decks.clamp(1, MAX_DECKS),
            loaded: vec![None; MAX_DECKS],
            pending: vec![None; MAX_DECKS],
            generation: 0,
            load_tx,
            load_rx,
            mapper: Mapper::new(None),
            leds: LedState::default(),
            midi_rx: None,
            midi_out: None,
            shift: [false; MAX_DECKS],
            tempo_range: [0; MAX_DECKS],
            browse_cursor: 0,
            browse_order: Vec::new(),
            status: Status::default(),
            ai_on: false,
            notices: Vec::new(),
            knob_pos: [[1.0, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.0]; MAX_DECKS],
            global_pos: [0.5, 0.0, 0.75, 0.75],
            director: Director::default(),
        };
        c.refresh_browse();
        for d in 0..c.decks {
            c.send(Command::TempoRange {
                deck: d,
                range: TEMPO_RANGES[0],
            });
        }
        c
    }

    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }

    pub fn deck_count(&self) -> usize {
        self.decks
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn loaded(&self, deck: usize) -> Option<&LoadedTrack> {
        self.loaded.get(deck).and_then(|l| l.as_ref())
    }

    /// Track id being loaded on a deck.
    pub fn pending(&self, deck: usize) -> Option<&TrackId> {
        self.pending.get(deck).and_then(|p| p.as_ref())
    }

    /// Library in browse order (sorted by name).
    pub fn refresh_browse(&mut self) {
        let lib = self.session.library();
        let mut v: Vec<&TrackInfo> = lib.iter().collect();
        v.sort_by_key(|t| t.display_name().to_lowercase());
        self.browse_order = v.into_iter().map(|t| t.id.clone()).collect();
        self.browse_cursor = self
            .browse_cursor
            .min(self.browse_order.len().saturating_sub(1));
    }

    pub fn browse_order(&self) -> &[TrackId] {
        &self.browse_order
    }

    /// Sends a command to the engine (dropped with a notice if the queue
    /// is full, which only happens if the audio thread stalls).
    pub fn send(&mut self, c: Command) {
        if self.engine.send(c).is_err() {
            self.notices.push("engine command queue full".into());
        }
    }

    /// Starts loading a library track onto a deck.
    pub fn load(&mut self, deck: usize, id: &TrackId) {
        if deck >= self.decks {
            return;
        }
        if self.status.decks[deck].playing {
            self.notices.push(format!(
                "deck {} is playing; stop it before loading",
                deck + 1
            ));
            return;
        }
        let Some(info) = self.session.library().get(id).cloned() else {
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        self.pending[deck] = Some(id.clone());
        let tx = self.load_tx.clone();
        let sr = self.sample_rate;
        std::thread::spawn(move || {
            let result = redj_media::decode_file(&info.path)
                .and_then(|a| a.into_stereo().resampled(sr))
                .map(|a| {
                    let wf = Arc::new(Waveform::compute(&a.samples, sr));
                    let audio = Arc::new(TrackAudio::new(
                        info.id.clone(),
                        sr,
                        info.grid.clone(),
                        a.samples,
                    ));
                    (audio, wf)
                })
                .map_err(|e| e.to_string());
            let _ = tx.send(LoadResult {
                deck,
                generation,
                result,
                info,
            });
        });
    }

    /// Connects controller input. `out` sends LED messages.
    pub fn attach_midi(
        &mut self,
        mapper: Mapper,
        input: Receiver<MidiMessage>,
        out: Option<Box<dyn FnMut(MidiMessage) + Send>>,
    ) {
        self.mapper = mapper;
        self.midi_rx = Some(input);
        self.midi_out = out;
        self.leds.reset();
        if let (Some(p), Some(o)) = (self.mapper.profile().cloned(), self.midi_out.as_mut()) {
            for m in &p.init {
                o(MidiMessage::new(m[0].0, m[1].0, m[2].0));
            }
        }
    }

    pub fn detach_midi(&mut self) {
        self.midi_rx = None;
        self.midi_out = None;
    }

    pub fn mapper_mut(&mut self) -> &mut Mapper {
        &mut self.mapper
    }

    /// Feeds one MIDI message directly (virtual controller, tests).
    pub fn midi(&mut self, m: MidiMessage) {
        if let Some(ev) = self.mapper.handle(m) {
            self.control(ev);
        }
        if let Some(b) = self.mapper.take_learned() {
            self.notices.push(format!("learned {:?}", b.action));
        }
    }

    /// Call once per UI frame.
    pub fn poll(&mut self) -> &Status {
        while let Ok(r) = self.load_rx.try_recv() {
            if r.generation != self.generation && self.pending[r.deck].as_ref() != Some(&r.info.id)
            {
                continue; // superseded by a later load
            }
            self.pending[r.deck] = None;
            match r.result {
                Ok((audio, waveform)) => {
                    let cue = audio.beat_to_frame(0.0).max(0.0);
                    self.send(Command::Load {
                        deck: r.deck,
                        audio: audio.clone(),
                        cue_frame: cue,
                        generation: r.generation,
                    });
                    self.loaded[r.deck] = Some(LoadedTrack {
                        info: r.info,
                        audio,
                        waveform,
                        generation: r.generation,
                    });
                }
                Err(e) => self
                    .notices
                    .push(format!("cannot load {}: {e}", r.info.display_name())),
            }
        }
        let msgs: Vec<MidiMessage> = self
            .midi_rx
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default();
        for m in msgs {
            self.midi(m);
        }
        self.status = self.engine.status();
        self.direct();
        self.update_leds();
        &self.status
    }

    fn update_leds(&mut self) {
        let (Some(profile), Some(out)) = (self.mapper.profile(), self.midi_out.as_mut()) else {
            return;
        };
        let inputs = LedInputs {
            decks: (0..self.decks)
                .map(|d| {
                    let s = &self.status.decks[d];
                    let beats = match (s.loop_range, self.loaded[d].as_ref()) {
                        (Some((a, b)), Some(l)) => {
                            Some(l.audio.frame_to_beat(b) - l.audio.frame_to_beat(a))
                        }
                        _ => None,
                    };
                    DeckLeds {
                        loaded: s.loaded,
                        playing: s.playing,
                        at_cue: s.loaded && (s.position - s.cue).abs() < 1.0,
                        pfl: s.pfl,
                        sync: s.sync,
                        hot_cues: std::array::from_fn(|i| s.hot_cues[i].is_some()),
                        loop_active: s.loop_active,
                        loop_beats: beats.map(|b| (b * 4.0).round() / 4.0),
                        vu: s.peak,
                    }
                })
                .collect(),
            ai_on: self.ai_on,
        };
        for m in self.leds.update(profile, &inputs) {
            out(m);
        }
    }

    /// Applies a control event (from MIDI or the UI).
    pub fn control(&mut self, ev: ControlEvent) {
        match ev {
            ControlEvent::Button { action, pressed } => self.button(action, pressed),
            ControlEvent::Value { action, value } => self.value(action, value),
            ControlEvent::Delta { action, ticks } => self.delta(action, ticks),
        }
    }

    fn button(&mut self, a: Action, pressed: bool) {
        let st = self.status;
        match a {
            Action::Shift { deck } => {
                if deck < MAX_DECKS {
                    self.shift[deck] = pressed;
                }
            }
            Action::Cue { deck } => self.send(Command::Cue { deck, pressed }),
            Action::JogTouch { deck } => self.send(Command::JogTouch { deck, on: pressed }),
            _ if !pressed => {}
            Action::PlayToggle { deck } => self.send(Command::TogglePlay { deck }),
            Action::ToStart { deck } => self.send(Command::Seek { deck, frame: 0.0 }),
            Action::Sync { deck } => self.send(Command::Sync {
                deck,
                on: !st.decks[deck.min(MAX_DECKS - 1)].sync,
            }),
            Action::SetMaster { deck } => self.send(Command::SetMaster { deck }),
            Action::Pfl { deck } => self.send(Command::Pfl {
                deck,
                on: !st.decks[deck.min(MAX_DECKS - 1)].pfl,
            }),
            Action::HotCue { deck, index } => self.send(Command::HotCue { deck, index }),
            Action::HotCueClear { deck, index } => self.send(Command::ClearHotCue { deck, index }),
            Action::BeatLoop { deck, beats } => self.send(Command::BeatLoop { deck, beats }),
            Action::LoopIn { deck } => self.send(Command::LoopIn { deck }),
            Action::LoopOut { deck } => self.send(Command::LoopOut { deck }),
            Action::Reloop { deck } => self.send(Command::Reloop { deck }),
            Action::LoopHalve { deck } => self.send(Command::LoopScale { deck, factor: 0.5 }),
            Action::LoopDouble { deck } => self.send(Command::LoopScale { deck, factor: 2.0 }),
            Action::BeatJump { deck, beats } => self.send(Command::BeatJump { deck, beats }),
            Action::TempoRangeCycle { deck } => {
                if deck < MAX_DECKS {
                    self.tempo_range[deck] = (self.tempo_range[deck] + 1) % TEMPO_RANGES.len();
                    let range = TEMPO_RANGES[self.tempo_range[deck]];
                    self.send(Command::TempoRange { deck, range });
                    self.notices.push(format!(
                        "deck {} tempo range ±{:.0}%",
                        deck + 1,
                        range * 100.0
                    ));
                }
            }
            Action::KeylockToggle { deck } => self.send(Command::Keylock {
                deck,
                on: !st.decks[deck.min(MAX_DECKS - 1)].keylock,
            }),
            Action::QuantizeToggle { deck } => self.send(Command::Quantize {
                deck,
                on: !st.decks[deck.min(MAX_DECKS - 1)].quantize,
            }),
            Action::Load { deck } => {
                if let Some(id) = self.browse_order.get(self.browse_cursor).cloned() {
                    self.load(deck, &id);
                }
            }
            Action::AiToggle => {
                self.ai_on = !self.ai_on;
                self.notices
                    .push(format!("AI {}", if self.ai_on { "on" } else { "off" }));
            }
            Action::BrowsePress => {}
            Action::Jog { .. }
            | Action::JogSearch { .. }
            | Action::Browse
            | Action::Knob { .. }
            | Action::Global { .. } => {}
        }
    }

    /// Position of a deck knob (0..1).
    pub fn knob(&self, deck: usize, k: Knob) -> f32 {
        self.knob_pos[deck.min(MAX_DECKS - 1)][knob_index(k)]
    }

    /// Position of a global control (0..1).
    pub fn global(&self, k: GlobalKnob) -> f32 {
        self.global_pos[global_index(k)]
    }

    /// Moves a deck knob (UI).
    pub fn set_knob(&mut self, deck: usize, knob: Knob, v: f32) {
        self.value(Action::Knob { deck, knob }, v);
    }

    /// Moves a global control (UI).
    pub fn set_global(&mut self, knob: GlobalKnob, v: f32) {
        self.value(Action::Global { knob }, v);
    }

    fn value(&mut self, a: Action, v: f32) {
        let v = v.clamp(0.0, 1.0);
        match a {
            Action::Knob { deck, knob } => {
                if deck >= MAX_DECKS {
                    return;
                }
                self.knob_pos[deck][knob_index(knob)] = v;
                let (param, value) = match knob {
                    Knob::Volume => (DeckParam::Volume, fader_curve(v)),
                    Knob::Trim => (DeckParam::Trim, (v - 0.5) * 24.0),
                    Knob::EqHigh => (DeckParam::EqHigh, eq_knob_db(v)),
                    Knob::EqMid => (DeckParam::EqMid, eq_knob_db(v)),
                    Knob::EqLow => (DeckParam::EqLow, eq_knob_db(v)),
                    Knob::Filter => (DeckParam::Filter, v * 2.0 - 1.0),
                    Knob::Tempo => (DeckParam::Tempo, v * 2.0 - 1.0),
                    Knob::Echo => (DeckParam::Echo, v),
                };
                self.send(Command::Param { deck, param, value });
            }
            Action::Global { knob } => {
                self.global_pos[global_index(knob)] = v;
                let c = match knob {
                    GlobalKnob::Crossfader => Command::Crossfader(v),
                    GlobalKnob::CueMix => Command::CueMix(v),
                    GlobalKnob::MasterLevel => Command::MasterGain((v - 0.75) * 40.0),
                    GlobalKnob::PhonesLevel => Command::PhonesGain((v - 0.75) * 40.0),
                };
                self.send(c);
            }
            _ => {}
        }
    }

    fn delta(&mut self, a: Action, ticks: f32) {
        match a {
            Action::Jog { deck } => self.send(Command::Jog { deck, ticks }),
            Action::JogSearch { deck } => self.send(Command::BeatJump {
                deck,
                beats: f64::from(ticks) * 0.5,
            }),
            Action::Browse => {
                let n = self.browse_order.len();
                if n > 0 {
                    let c = self.browse_cursor as i64 + ticks.round() as i64;
                    self.browse_cursor = c.clamp(0, n as i64 - 1) as usize;
                }
            }
            _ => {}
        }
    }
}
