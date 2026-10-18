//! LED feedback: turns console state into the MIDI messages that light
//! the controller, sending only what changed.

use std::collections::HashMap;

use crate::mapper::MidiMessage;
use crate::profile::{LedSource, Profile};

/// Per-deck state relevant to LEDs.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DeckLeds {
    pub loaded: bool,
    pub playing: bool,
    pub at_cue: bool,
    pub pfl: bool,
    pub sync: bool,
    pub hot_cues: [bool; 8],
    pub loop_active: bool,
    /// Length of the active loop in beats.
    pub loop_beats: Option<f64>,
    /// Level 0..1.
    pub vu: f32,
}

/// Console state relevant to LEDs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LedInputs {
    pub decks: Vec<DeckLeds>,
    pub ai_on: bool,
}

/// Remembers what was sent last.
#[derive(Debug, Clone, Default)]
pub struct LedState {
    last: HashMap<(u8, u8), u8>,
}

impl LedState {
    /// Forgets everything (forces a full refresh on the next update).
    pub fn reset(&mut self) {
        self.last.clear();
    }

    /// Messages to send for the new state.
    pub fn update(&mut self, profile: &Profile, s: &LedInputs) -> Vec<MidiMessage> {
        let mut out = Vec::new();
        let deck = |d: usize| s.decks.get(d).copied().unwrap_or_default();
        for o in &profile.outputs {
            let lit = |b: bool| if b { o.on } else { o.off };
            let value = match o.source {
                LedSource::Playing { deck: d } => lit(deck(d).playing),
                LedSource::CueIndicator { deck: d } => lit(deck(d).at_cue && !deck(d).playing),
                LedSource::Pfl { deck: d } => lit(deck(d).pfl),
                LedSource::Sync { deck: d } => lit(deck(d).sync),
                LedSource::HotCueSet { deck: d, index } => {
                    lit(deck(d).hot_cues.get(index).copied().unwrap_or(false))
                }
                LedSource::LoopActive { deck: d } => lit(deck(d).loop_active),
                LedSource::BeatLoop { deck: d, beats } => lit(deck(d).loop_active
                    && deck(d).loop_beats.is_some_and(|b| (b - beats).abs() < 1e-6)),
                LedSource::Loaded { deck: d } => lit(deck(d).loaded),
                LedSource::Vu { deck: d } => {
                    (deck(d).vu.clamp(0.0, 1.0) * f32::from(o.on)).round() as u8
                }
                LedSource::Ai => lit(s.ai_on),
            };
            let key = (o.status.0, o.data1.0);
            if self.last.get(&key) != Some(&value) {
                self.last.insert(key, value);
                out.push(MidiMessage::new(o.status.0, o.data1.0, value));
            }
        }
        out
    }
}
