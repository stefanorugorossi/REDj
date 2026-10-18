//! The compiled form of a plan: what the engine evaluates.
//!
//! Everything is a function of the **master beat**. The tempo map turns
//! master beats into time; each clip's playhead map turns master beats into
//! track beats (loops and jumps are segments); automation lanes give every
//! parameter's value at any master beat. Evaluating at an arbitrary
//! position therefore always yields the correct state.

use redj_core::TrackId;
use serde::Serialize;

use crate::model::{Curve, Param, TransitionStyle};

/// Master tempo as piecewise-linear BPM over master beats.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TempoMap {
    segs: Vec<TempoSeg>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct TempoSeg {
    pub start_beat: f64,
    pub end_beat: f64,
    pub bpm0: f64,
    pub bpm1: f64,
    pub start_sec: f64,
}

impl TempoSeg {
    fn slope(&self) -> f64 {
        if self.end_beat > self.start_beat {
            (self.bpm1 - self.bpm0) / (self.end_beat - self.start_beat)
        } else {
            0.0
        }
    }

    fn sec_at(&self, beat: f64) -> f64 {
        let k = self.slope();
        let db = beat - self.start_beat;
        if k.abs() < 1e-12 {
            self.start_sec + db * 60.0 / self.bpm0
        } else {
            let bpm = self.bpm0 + k * db;
            self.start_sec + 60.0 / k * (bpm / self.bpm0).ln()
        }
    }

    fn beat_at(&self, sec: f64) -> f64 {
        let k = self.slope();
        let dt = sec - self.start_sec;
        if k.abs() < 1e-12 {
            self.start_beat + dt * self.bpm0 / 60.0
        } else {
            self.start_beat + self.bpm0 / k * ((k * dt / 60.0).exp() - 1.0)
        }
    }
}

impl TempoMap {
    /// Builds a map from `(start_beat, end_beat, bpm0, bpm1)` segments that
    /// tile `[0, ∞)`; the last segment is extended at its final tempo.
    pub fn from_segments(parts: &[(f64, f64, f64, f64)]) -> Self {
        let mut segs: Vec<TempoSeg> = Vec::with_capacity(parts.len() + 1);
        let mut sec = 0.0;
        for &(a, b, v0, v1) in parts {
            if b <= a {
                continue;
            }
            let s = TempoSeg {
                start_beat: a,
                end_beat: b,
                bpm0: v0,
                bpm1: v1,
                start_sec: sec,
            };
            sec = s.sec_at(b);
            segs.push(s);
        }
        let (last_beat, last_bpm) = segs
            .last()
            .map(|s| (s.end_beat, s.bpm1))
            .unwrap_or((0.0, parts.first().map(|p| p.2).unwrap_or(120.0)));
        segs.push(TempoSeg {
            start_beat: last_beat,
            end_beat: f64::INFINITY,
            bpm0: last_bpm,
            bpm1: last_bpm,
            start_sec: sec,
        });
        Self { segs }
    }

    pub fn constant(bpm: f64) -> Self {
        Self::from_segments(&[]).with_first_bpm(bpm)
    }

    fn with_first_bpm(mut self, bpm: f64) -> Self {
        if self.segs.len() == 1 {
            self.segs[0].bpm0 = bpm;
            self.segs[0].bpm1 = bpm;
        }
        self
    }

    fn seg_for_beat(&self, beat: f64) -> &TempoSeg {
        let i = self.segs.partition_point(|s| s.end_beat <= beat);
        &self.segs[i.min(self.segs.len() - 1)]
    }

    pub fn bpm_at(&self, beat: f64) -> f64 {
        let s = self.seg_for_beat(beat);
        s.bpm0 + s.slope() * (beat.max(s.start_beat) - s.start_beat)
    }

    pub fn beat_to_sec(&self, beat: f64) -> f64 {
        self.seg_for_beat(beat).sec_at(beat)
    }

    pub fn sec_to_beat(&self, sec: f64) -> f64 {
        let i = self
            .segs
            .partition_point(|s| s.end_beat.is_finite() && s.sec_at(s.end_beat) <= sec);
        self.segs[i.min(self.segs.len() - 1)].beat_at(sec)
    }

    pub fn segments(&self) -> &[TempoSeg] {
        &self.segs
    }
}

/// Linear map from a master-beat range to track beats (slope 1: decks are
/// always beat-locked to the master).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PlayheadSeg {
    pub master_start: f64,
    pub master_end: f64,
    pub track_start: f64,
}

/// One automation point in master beats.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LanePoint {
    pub beat: f64,
    pub value: f64,
    pub curve: Curve,
}

/// Piecewise automation curve over master beats.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Lane {
    pub default: f64,
    pub points: Vec<LanePoint>,
}

fn shape(curve: Curve, t: f64) -> f64 {
    match curve {
        Curve::Linear => t,
        Curve::Smooth => t * t * (3.0 - 2.0 * t),
        Curve::Exponential => t * t * t,
        Curve::Step => {
            if t >= 1.0 {
                1.0
            } else {
                0.0
            }
        }
    }
}

impl Lane {
    pub fn new(default: f64) -> Self {
        Self {
            default,
            points: Vec::new(),
        }
    }

    /// Value at a master beat.
    pub fn eval(&self, beat: f64) -> f64 {
        let p = &self.points;
        if p.is_empty() {
            return self.default;
        }
        let i = p.partition_point(|q| q.beat <= beat);
        if i == 0 {
            return p[0].value;
        }
        if i == p.len() {
            return p[i - 1].value;
        }
        let (a, b) = (p[i - 1], p[i]);
        let span = b.beat - a.beat;
        let t = if span > 0.0 {
            (beat - a.beat) / span
        } else {
            1.0
        };
        a.value + (b.value - a.value) * shape(b.curve, t)
    }
}

/// A track playing on one deck slot for a span of the set.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Clip {
    /// Index of the plan entry.
    pub entry: usize,
    pub track: TrackId,
    /// Deck slot (independent EQ/filter/echo state).
    pub slot: usize,
    /// Master-beat span during which the clip is processed (including
    /// echo tails).
    pub start_beat: f64,
    pub end_beat: f64,
    pub playhead: Vec<PlayheadSeg>,
    pub trim_db: f64,
    /// Lanes indexed by [`Param::index`].
    pub lanes: Vec<Lane>,
    pub echo_beats: f64,
    pub echo_feedback: f64,
}

impl Clip {
    /// Track beat at a master beat, or `None` outside the playhead map.
    pub fn track_beat(&self, master: f64) -> Option<f64> {
        let i = self.playhead.partition_point(|s| s.master_end <= master);
        let s = self.playhead.get(i)?;
        (master >= s.master_start).then_some(s.track_start + (master - s.master_start))
    }

    pub fn lane(&self, p: Param) -> &Lane {
        &self.lanes[p.index()]
    }
}

/// A point of interest in the set (for the tracklist and the UI).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Marker {
    pub entry: usize,
    pub track: TrackId,
    /// Master beat where the transition into this entry starts.
    pub transition_start_beat: f64,
    /// Master beat where this entry becomes the main track.
    pub main_beat: f64,
    pub style: Option<TransitionStyle>,
}

/// A compiled, engine-ready set.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Timeline {
    pub tempo: TempoMap,
    pub clips: Vec<Clip>,
    pub markers: Vec<Marker>,
    /// Master beat where the set ends.
    pub end_beat: f64,
    pub keylock: bool,
}

impl Timeline {
    pub fn duration_sec(&self) -> f64 {
        self.tempo.beat_to_sec(self.end_beat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tempo_map_ramp_round_trips() {
        let m = TempoMap::from_segments(&[(0.0, 64.0, 120.0, 120.0), (64.0, 128.0, 120.0, 130.0)]);
        assert!((m.beat_to_sec(64.0) - 32.0).abs() < 1e-9);
        assert!((m.bpm_at(96.0) - 125.0).abs() < 1e-9);
        // Ramp duration = 60/k * ln(130/120) with k = 10/64.
        let expect = 32.0 + 60.0 / (10.0 / 64.0) * (130.0f64 / 120.0).ln();
        assert!((m.beat_to_sec(128.0) - expect).abs() < 1e-9);
        assert!((m.bpm_at(500.0) - 130.0).abs() < 1e-9);
        for b in [0.0, 10.0, 64.0, 80.5, 127.9, 300.0] {
            assert!((m.sec_to_beat(m.beat_to_sec(b)) - b).abs() < 1e-9, "{b}");
        }
    }

    #[test]
    fn constant_tempo_map() {
        let m = TempoMap::constant(128.0);
        assert!((m.beat_to_sec(128.0) - 60.0).abs() < 1e-9);
        assert!((m.sec_to_beat(30.0) - 64.0).abs() < 1e-9);
    }

    #[test]
    fn lane_curves() {
        let lane = Lane {
            default: 1.0,
            points: vec![
                LanePoint {
                    beat: 0.0,
                    value: 0.0,
                    curve: Curve::Linear,
                },
                LanePoint {
                    beat: 4.0,
                    value: 1.0,
                    curve: Curve::Linear,
                },
                LanePoint {
                    beat: 8.0,
                    value: 0.0,
                    curve: Curve::Step,
                },
            ],
        };
        assert_eq!(lane.eval(-1.0), 0.0);
        assert_eq!(lane.eval(2.0), 0.5);
        assert_eq!(lane.eval(7.99), 1.0);
        assert_eq!(lane.eval(8.0), 0.0);
        assert_eq!(Lane::new(0.3).eval(5.0), 0.3);
    }
}
