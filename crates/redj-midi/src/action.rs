//! What a control does, independent of any controller.

use serde::{Deserialize, Serialize};

/// Continuous mixer/deck controls (absolute knobs and faders).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Knob {
    Volume,
    Trim,
    EqHigh,
    EqMid,
    EqLow,
    Filter,
    Tempo,
    Echo,
}

/// Global continuous controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GlobalKnob {
    Crossfader,
    /// Headphone cue/master mix.
    CueMix,
    MasterLevel,
    PhonesLevel,
}

/// A mappable action.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    PlayToggle {
        deck: usize,
    },
    /// CUE button (press and release both matter).
    Cue {
        deck: usize,
    },
    /// Jump to the start of the track.
    ToStart {
        deck: usize,
    },
    Sync {
        deck: usize,
    },
    /// Make this deck the tempo master.
    SetMaster {
        deck: usize,
    },
    Pfl {
        deck: usize,
    },
    HotCue {
        deck: usize,
        index: usize,
    },
    HotCueClear {
        deck: usize,
        index: usize,
    },
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
    Reloop {
        deck: usize,
    },
    LoopHalve {
        deck: usize,
    },
    LoopDouble {
        deck: usize,
    },
    BeatJump {
        deck: usize,
        beats: f64,
    },
    TempoRangeCycle {
        deck: usize,
    },
    KeylockToggle {
        deck: usize,
    },
    QuantizeToggle {
        deck: usize,
    },
    /// Jog platter touch (press/release).
    JogTouch {
        deck: usize,
    },
    /// Relative jog movement (scratch when touched, bend otherwise).
    Jog {
        deck: usize,
    },
    /// Fast search with the jog (shift + platter).
    JogSearch {
        deck: usize,
    },
    /// Load the selected library track into a deck.
    Load {
        deck: usize,
    },
    /// Relative library browse encoder.
    Browse,
    BrowsePress,
    Shift {
        deck: usize,
    },
    Knob {
        deck: usize,
        knob: Knob,
    },
    Global {
        knob: GlobalKnob,
    },
    /// AI on/off (handy on a spare button).
    AiToggle,
}

impl Action {
    /// True for actions whose release also matters.
    pub fn wants_release(&self) -> bool {
        matches!(
            self,
            Action::Cue { .. } | Action::JogTouch { .. } | Action::Shift { .. }
        )
    }

    /// True for continuous (knob/fader) actions.
    pub fn is_continuous(&self) -> bool {
        matches!(self, Action::Knob { .. } | Action::Global { .. })
    }

    /// True for relative (encoder/jog) actions.
    pub fn is_relative(&self) -> bool {
        matches!(
            self,
            Action::Jog { .. } | Action::JogSearch { .. } | Action::Browse
        )
    }
}

/// An action with its value, produced by the mapper.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ControlEvent {
    /// Button press (`true`) or release (`false`).
    Button { action: Action, pressed: bool },
    /// Absolute control normalized to 0..1 (14-bit when available).
    Value { action: Action, value: f32 },
    /// Relative movement in ticks (signed).
    Delta { action: Action, ticks: f32 },
}
