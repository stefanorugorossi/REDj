//! Plan-driven transitions in the live engine.
//!
//! A [`LiveTransition`] is a compiled two-entry plan (the playing track and
//! the next one). Its master beat is defined by the outgoing deck's own
//! playhead (`master = outgoing track beat - beat_offset`), so the deck keeps
//! its tempo and the plan simply follows it. While the transition runs, the
//! incoming deck's playhead is locked to the plan and the automation lanes
//! drive both channel strips, except for parameters the human has touched.

use std::sync::Arc;

use redj_plan::{Param, Timeline, TransitionStyle};

use crate::strip::StripParams;

/// A transition prepared off the audio thread.
#[derive(Debug, Clone)]
pub struct LiveTransition {
    /// Identifies the transition in status reports.
    pub id: u32,
    pub out_deck: usize,
    pub in_deck: usize,
    /// Plan with clip 0 = outgoing track, clip 1 = incoming track.
    pub timeline: Arc<Timeline>,
    /// Outgoing track beat at master beat 0.
    pub beat_offset: f64,
}

impl LiveTransition {
    /// Master beat where the incoming track starts.
    pub fn start_beat(&self) -> f64 {
        self.timeline
            .clips
            .get(1)
            .map(|c| c.start_beat)
            .unwrap_or(0.0)
    }

    /// Master beat where the transition is over (outgoing audio done).
    pub fn end_beat(&self) -> f64 {
        let tl = &self.timeline;
        match tl.markers.get(1).and_then(|m| m.style) {
            // The outgoing deck is silent once the incoming one starts (the
            // echo tail keeps ringing on its own).
            Some(TransitionStyle::EchoOut | TransitionStyle::Cut) => self.start_beat(),
            _ => tl.clips.first().map(|c| c.end_beat).unwrap_or(0.0),
        }
    }
}

/// Lifecycle of the engine's transition.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TransitionState {
    #[default]
    Idle,
    /// Waiting for the outgoing deck to reach the start.
    Armed,
    Active,
}

/// Transition part of the engine status.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TransitionStatus {
    pub state: TransitionState,
    pub id: u32,
    pub out_deck: usize,
    pub in_deck: usize,
    /// Current master beat of the transition plan.
    pub beat: f64,
    pub start_beat: f64,
    pub end_beat: f64,
    /// Bit mask of parameters taken over by the human (bit = param index
    /// × deck: bits 0..8 outgoing, 8..16 incoming).
    pub human: u16,
    /// Transitions completed so far.
    pub completed: u32,
    /// Transitions cancelled because the human took over the transport.
    pub cancelled: u32,
    /// Id of the last transition that completed or was cancelled.
    pub last_id: u32,
}

/// Index of a plan parameter in the human-ownership mask.
pub(crate) fn param_bit(p: Param) -> u16 {
    1 << p.index()
}

/// Effective strip parameters: lane values except where the human owns
/// the parameter.
pub(crate) fn plan_params(
    timeline: &Timeline,
    clip: usize,
    beat: f64,
    human: &StripParams,
    owned: u16,
    bpm: f64,
    sr: f64,
) -> StripParams {
    let c = &timeline.clips[clip];
    let ev = |p: Param| c.lane(p).eval(beat) as f32;
    let pick = |p: Param, h: f32| if owned & param_bit(p) != 0 { h } else { ev(p) };
    StripParams {
        volume: pick(Param::Volume, human.volume),
        trim_db: human.trim_db,
        eq_db: [
            pick(Param::EqLow, human.eq_db[0]),
            pick(Param::EqMid, human.eq_db[1]),
            pick(Param::EqHigh, human.eq_db[2]),
        ],
        filter: pick(Param::Filter, human.filter),
        echo_send: pick(Param::Echo, human.echo_send),
        echo_delay_samples: (c.echo_beats * 60.0 / bpm.max(1.0) * sr) as f32,
        echo_feedback: c.echo_feedback as f32,
    }
}
