//! Live state shared between the console (UI thread) and AI clients (MCP
//! server threads).

use std::sync::mpsc::{Receiver, Sender, channel};

use redj_plan::TransitionStyle;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One deck as seen by the AI.
#[derive(Debug, Clone, Serialize)]
pub struct LiveDeck {
    pub deck: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub playing: bool,
    /// Current playhead position in bars of the loaded track.
    pub bar: f64,
    /// Current playback tempo.
    pub bpm: f64,
    /// Bar where the loaded track ends.
    pub end_bar: f64,
    /// Outro bars of the loaded track.
    pub mix_out: (f64, f64),
}

/// The next transition, scheduled or running.
#[derive(Debug, Clone, Serialize)]
pub struct PendingView {
    pub track: String,
    pub name: String,
    /// Bar of the playing track where the transition starts.
    pub at_bar: f64,
    pub length_bars: f64,
    pub style: TransitionStyle,
    /// "ai", "autopilot" or "external".
    pub by: String,
    /// "loading", "armed" or "running".
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

/// What the AI needs to plan the next transition.
#[derive(Debug, Clone, Serialize)]
pub struct LiveSnapshot {
    pub ai_on: bool,
    /// Deck playing the main track.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_deck: Option<usize>,
    pub master_bpm: f64,
    pub decks: Vec<LiveDeck>,
    /// Track ids already played (do not repeat them).
    pub played: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<PendingView>,
    /// A transition may not start before this bar of the main track.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub earliest_at_bar: Option<f64>,
    /// If nothing is scheduled when the main track reaches this bar, the
    /// autopilot mixes on its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline_bar: Option<f64>,
    /// Recommended start (the outro of the playing track) and length of
    /// the next transition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_at_bar: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_length_bars: Option<f64>,
    /// True when the AI should schedule the next transition now.
    pub needs_plan: bool,
}

/// A transition requested by the AI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransitionRequest {
    /// Track id (or unique prefix) of the next track.
    pub track: String,
    /// Bar of the PLAYING track where the transition starts (at least
    /// `earliest_at_bar`; usually the start of its mix_out range).
    pub at_bar: f64,
    /// Transition length in bars (typically 16; 1 for echo_out).
    pub length_bars: f64,
    pub style: TransitionStyle,
    /// Bar of the next track where it starts (default: its intro start).
    #[serde(default)]
    pub start_bar: Option<f64>,
    /// Why (shown to the user).
    #[serde(default)]
    pub rationale: Option<String>,
}

/// Answer to a [`TransitionRequest`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProposalReply {
    pub ok: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl ProposalReply {
    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            errors: vec![msg.into()],
            warnings: Vec::new(),
        }
    }
}

/// A request waiting for the console, with its reply channel.
#[derive(Debug)]
pub struct Proposal {
    pub request: TransitionRequest,
    /// "ai" for the built-in director, "external" for other MCP clients.
    pub by: String,
    pub reply: Sender<ProposalReply>,
}

impl Proposal {
    pub fn new(
        request: TransitionRequest,
        by: impl Into<String>,
    ) -> (Self, Receiver<ProposalReply>) {
        let (tx, rx) = channel();
        (
            Self {
                request,
                by: by.into(),
                reply: tx,
            },
            rx,
        )
    }
}
