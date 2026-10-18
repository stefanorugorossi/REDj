//! The mix plan: what a human or the AI writes.
//!
//! Positions are in **bars of the track they refer to** (bar 0 = the
//! track's first downbeat, as reported by analysis), never in seconds, so a
//! plan stays valid whatever the playback tempo.

use redj_core::TrackId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Current plan format version.
pub const PLAN_VERSION: u32 = 1;

/// A complete DJ set: tracks in playing order and how each one is mixed in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MixPlan {
    /// Plan format version; use 1.
    #[serde(default = "default_version")]
    pub version: u32,
    /// Optional title of the set (written to output tags).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// How the master tempo is chosen.
    #[serde(default)]
    pub tempo: TempoPolicy,
    /// Keep the original pitch when a track plays faster or slower than its
    /// own tempo (time-stretching). Without keylock, pitch follows speed
    /// like a turntable.
    #[serde(default = "default_true")]
    pub keylock: bool,
    /// Loudness matching between tracks.
    #[serde(default)]
    pub loudness: LoudnessPolicy,
    /// Tracks in playing order. Every entry after the first must have a
    /// `transition`.
    pub entries: Vec<Entry>,
}

fn default_version() -> u32 {
    PLAN_VERSION
}

fn default_true() -> bool {
    true
}

/// Master tempo policy.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum TempoPolicy {
    /// The master tempo follows the playing track; during a transition it
    /// moves from the outgoing to the incoming tempo (see
    /// `Transition::tempo`).
    #[default]
    Follow,
    /// The whole set plays at one tempo.
    Fixed {
        /// Master tempo in BPM.
        bpm: f64,
    },
}

/// Loudness matching.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoudnessPolicy {
    /// Apply a gain to every track so its integrated loudness matches
    /// `target_lufs` (entries can override it with `gain_db`).
    #[serde(default = "default_true")]
    pub auto_gain: bool,
    /// Target integrated loudness per track in LUFS.
    #[serde(default = "default_target_lufs")]
    pub target_lufs: f64,
}

fn default_target_lufs() -> f64 {
    -9.0
}

impl Default for LoudnessPolicy {
    fn default() -> Self {
        Self {
            auto_gain: true,
            target_lufs: default_target_lufs(),
        }
    }
}

/// One track of the set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// Track id from the library (a unique prefix of at least 4 characters
    /// is accepted).
    pub track: TrackId,
    /// Bar of this track where playback starts.
    #[serde(default)]
    pub start_bar: f64,
    /// Last entry only: bar where the set ends (with a fade over
    /// `fade_out_bars`). Defaults to the end of the track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_bar: Option<f64>,
    /// Last entry only: length of the final fade-out in bars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_out_bars: Option<f64>,
    /// Fixed track gain in dB, overriding loudness matching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_db: Option<f64>,
    /// Loops to extend parts of the track (e.g. a drum intro or outro).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loops: Vec<Loop>,
    /// How this track is mixed in over the previous one. Required for
    /// every entry except the first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition: Option<Transition>,
    /// Why this track was chosen here (shown to the user).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

/// Repeats a span of the track. When the playhead reaches `at_bar` the span
/// `[at_bar, at_bar + length_bars)` plays `repeats` extra times.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Loop {
    /// Bar of the track where the loop starts.
    pub at_bar: f64,
    /// Loop length in bars (1, 2, 4, 8 or 16 sound natural).
    pub length_bars: f64,
    /// Extra repetitions (1 = the span plays twice in total).
    pub repeats: u32,
}

/// How an entry is mixed in over the previous entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Transition {
    /// Bar of the **outgoing** (previous) track where the transition
    /// starts. With loops, this is the playhead position after the loops
    /// before it have finished.
    pub at_bar: f64,
    /// Length of the transition in bars (for `echo_out` the echo lead-in;
    /// ignored for `cut`).
    pub length_bars: f64,
    /// Mixing technique.
    pub style: TransitionStyle,
    /// What the master tempo does during the transition.
    #[serde(default)]
    pub tempo: TempoChange,
    /// Extra automation that replaces the style's automation for the same
    /// deck and parameter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub automation: Vec<LaneSpec>,
    /// Why this transition was chosen (shown to the user).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

/// Mixing technique of a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransitionStyle {
    /// Volume crossfade with a bass swap in the middle.
    Blend,
    /// Both tracks at full volume; the basslines swap at the middle bar.
    BassSwap,
    /// Gradual three-band swap: mids, then lows, then highs.
    EqSwap,
    /// Outgoing is high-pass filtered away while the incoming low-pass
    /// filter opens.
    Filter,
    /// Outgoing echoes out; the incoming starts on the downbeat after the
    /// lead-in.
    EchoOut,
    /// Hard cut on the downbeat at `at_bar`.
    Cut,
}

/// Master tempo behaviour during a transition (with `tempo.mode =
/// follow`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum TempoChange {
    /// Glide linearly from the outgoing to the incoming tempo.
    #[default]
    Glide,
    /// Switch to the incoming tempo when the transition starts.
    Incoming,
    /// Keep the outgoing tempo until the transition ends.
    Outgoing,
}

/// Which deck of a transition an automation lane controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Outgoing,
    Incoming,
}

/// An automatable deck parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Param {
    /// Channel volume, linear amplitude 0..1.
    Volume,
    /// Low band (below 250 Hz) in dB, -40 (kill) .. +6.
    EqLow,
    /// Mid band in dB, -40 (kill) .. +6.
    EqMid,
    /// High band (above 2.5 kHz) in dB, -40 (kill) .. +6.
    EqHigh,
    /// Filter knob -1 (low-pass) .. 0 (off) .. 1 (high-pass).
    Filter,
    /// Echo send 0..1 (tempo-synced, repeats ring out after the fader).
    Echo,
}

impl Param {
    pub const ALL: [Param; 6] = [
        Param::Volume,
        Param::EqLow,
        Param::EqMid,
        Param::EqHigh,
        Param::Filter,
        Param::Echo,
    ];

    /// Neutral value.
    pub fn default_value(self) -> f64 {
        match self {
            Param::Volume => 1.0,
            _ => 0.0,
        }
    }

    /// Valid range.
    pub fn range(self) -> (f64, f64) {
        match self {
            Param::Volume | Param::Echo => (0.0, 1.0),
            Param::EqLow | Param::EqMid | Param::EqHigh => (-40.0, 6.0),
            Param::Filter => (-1.0, 1.0),
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }
}

/// Shape of the curve leading into an automation point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum Curve {
    #[default]
    Linear,
    /// S-shaped ease in/out.
    Smooth,
    /// Slow start, fast end (good for fade-ins).
    Exponential,
    /// Hold the previous value, then jump at this point.
    Step,
}

/// An automation lane inside a transition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LaneSpec {
    pub side: Side,
    pub param: Param,
    /// Points in time order. Before the first point the lane holds the
    /// first value; after the last point, the last value.
    pub points: Vec<PointSpec>,
}

/// One automation point.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PointSpec {
    /// Position in bars from the start of the transition.
    pub bar: f64,
    pub value: f64,
    /// Curve from the previous point to this one.
    #[serde(default)]
    pub curve: Curve,
}
