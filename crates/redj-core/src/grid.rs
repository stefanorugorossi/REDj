use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Beats, Seconds};

/// Beat grid of a track: maps track beats to track time.
///
/// Beat `0` is the first **downbeat** of the track (bar 0, beat 1 in DJ
/// speak). It may lie before the start of the audio (negative `origin_sec`)
/// when the track starts on a pickup. Bars start at multiples of
/// [`crate::BEATS_PER_BAR`] beats.
///
/// A constant-tempo grid (the primary case for 4/4 EDM) uses only `bpm` and
/// `origin_sec`. A variable-tempo grid also lists `anchors`: the time of each
/// listed beat, interpolated linearly in between and extrapolated with
/// `bpm` outside.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BeatGrid {
    /// Tempo in beats per minute (the average tempo for variable grids).
    pub bpm: f64,
    /// Time of beat 0 in seconds from the start of the audio.
    pub origin_sec: Seconds,
    /// Beat anchors for variable-tempo tracks, sorted by beat. Empty for a
    /// constant grid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<BeatAnchor>,
}

/// One point of a variable-tempo beat grid.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BeatAnchor {
    pub beat: Beats,
    pub sec: Seconds,
}

impl BeatGrid {
    /// Constant-tempo grid.
    pub fn constant(bpm: f64, origin_sec: Seconds) -> Self {
        Self {
            bpm,
            origin_sec,
            anchors: Vec::new(),
        }
    }

    pub fn is_constant(&self) -> bool {
        self.anchors.len() < 2
    }

    /// Seconds per beat at the grid's nominal tempo.
    #[inline]
    pub fn beat_len_sec(&self) -> Seconds {
        60.0 / self.bpm
    }

    /// Time of a (fractional) beat.
    pub fn beat_to_sec(&self, beat: Beats) -> Seconds {
        if self.is_constant() {
            return self.origin_sec + beat * self.beat_len_sec();
        }
        let a = &self.anchors;
        let first = a[0];
        let last = a[a.len() - 1];
        if beat <= first.beat {
            return first.sec + (beat - first.beat) * self.beat_len_sec();
        }
        if beat >= last.beat {
            return last.sec + (beat - last.beat) * self.beat_len_sec();
        }
        let i = a.partition_point(|p| p.beat <= beat) - 1;
        let (p, q) = (a[i], a[i + 1]);
        p.sec + (beat - p.beat) * (q.sec - p.sec) / (q.beat - p.beat)
    }

    /// Beat at a given time (inverse of [`Self::beat_to_sec`]).
    pub fn sec_to_beat(&self, sec: Seconds) -> Beats {
        if self.is_constant() {
            return (sec - self.origin_sec) / self.beat_len_sec();
        }
        let a = &self.anchors;
        let first = a[0];
        let last = a[a.len() - 1];
        if sec <= first.sec {
            return first.beat + (sec - first.sec) / self.beat_len_sec();
        }
        if sec >= last.sec {
            return last.beat + (sec - last.sec) / self.beat_len_sec();
        }
        let i = a.partition_point(|p| p.sec <= sec) - 1;
        let (p, q) = (a[i], a[i + 1]);
        p.beat + (sec - p.sec) * (q.beat - p.beat) / (q.sec - p.sec)
    }

    /// Local tempo (BPM) at a beat.
    pub fn bpm_at(&self, beat: Beats) -> f64 {
        if self.is_constant() {
            return self.bpm;
        }
        let a = &self.anchors;
        if beat <= a[0].beat || beat >= a[a.len() - 1].beat {
            return self.bpm;
        }
        let i = a.partition_point(|p| p.beat <= beat) - 1;
        let (p, q) = (a[i], a[i + 1]);
        60.0 * (q.beat - p.beat) / (q.sec - p.sec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_grid_round_trips() {
        let g = BeatGrid::constant(128.0, 0.25);
        assert!((g.beat_to_sec(0.0) - 0.25).abs() < 1e-12);
        assert!((g.beat_to_sec(128.0) - 60.25).abs() < 1e-9);
        for b in [-3.5, 0.0, 17.25, 1000.0] {
            assert!((g.sec_to_beat(g.beat_to_sec(b)) - b).abs() < 1e-9);
        }
    }

    #[test]
    fn variable_grid_interpolates_and_extrapolates() {
        let g = BeatGrid {
            bpm: 120.0,
            origin_sec: 0.0,
            anchors: vec![
                BeatAnchor {
                    beat: 0.0,
                    sec: 0.0,
                },
                BeatAnchor {
                    beat: 4.0,
                    sec: 2.0,
                },
                BeatAnchor {
                    beat: 8.0,
                    sec: 3.6,
                },
            ],
        };
        assert!((g.beat_to_sec(2.0) - 1.0).abs() < 1e-12);
        assert!((g.beat_to_sec(6.0) - 2.8).abs() < 1e-12);
        assert!((g.beat_to_sec(10.0) - 4.6).abs() < 1e-12);
        assert!((g.beat_to_sec(-2.0) + 1.0).abs() < 1e-12);
        assert!((g.bpm_at(6.0) - 150.0).abs() < 1e-9);
        for b in [-1.0, 1.0, 5.5, 9.0] {
            assert!((g.sec_to_beat(g.beat_to_sec(b)) - b).abs() < 1e-9);
        }
    }
}
