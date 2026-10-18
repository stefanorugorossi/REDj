//! Band-limited (windowed-sinc) interpolation for reading audio at
//! fractional positions: varispeed playback and tempo matching without
//! keylock.

use crate::source::Source;

/// Number of taps of the interpolation kernel.
pub const TAPS: usize = 32;
const PHASES: usize = 512;

/// Precomputed polyphase windowed-sinc kernel.
#[derive(Debug, Clone)]
pub struct SincTable {
    /// `PHASES + 1` rows of `TAPS` coefficients (last row = first row shifted
    /// by one sample, so phase interpolation needs no wrap-around).
    table: Vec<f32>,
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..50 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

impl Default for SincTable {
    fn default() -> Self {
        Self::new(0.92)
    }
}

impl SincTable {
    /// `cutoff` is relative to Nyquist (0..1).
    pub fn new(cutoff: f64) -> Self {
        let beta = 8.6;
        let half = (TAPS / 2) as f64;
        let mut table = Vec::with_capacity((PHASES + 1) * TAPS);
        for p in 0..=PHASES {
            let frac = p as f64 / PHASES as f64;
            let mut row = [0f64; TAPS];
            for (k, c) in row.iter_mut().enumerate() {
                // Tap k reads sample i + k - (TAPS/2 - 1).
                let x = k as f64 - (half - 1.0) - frac;
                let sinc = if x.abs() < 1e-12 {
                    1.0
                } else {
                    (std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
                };
                let t = x / half;
                let w = if t.abs() >= 1.0 {
                    0.0
                } else {
                    bessel_i0(beta * (1.0 - t * t).sqrt()) / bessel_i0(beta)
                };
                *c = sinc * w;
            }
            // Normalize DC gain to exactly 1.
            let sum: f64 = row.iter().sum();
            table.extend(row.iter().map(|c| (c / sum) as f32));
        }
        Self { table }
    }

    /// Reads stereo audio at fractional frame `pos`. Frames outside the
    /// source read as silence.
    #[inline]
    pub fn read_stereo<S: Source + ?Sized>(&self, src: &S, pos: f64) -> (f32, f32) {
        let base = pos.floor();
        let frac = pos - base;
        let i = base as i64;
        let pf = frac * PHASES as f64;
        let p = (pf as usize).min(PHASES - 1);
        let t = (pf - p as f64) as f32;
        let r0 = &self.table[p * TAPS..(p + 1) * TAPS];
        let r1 = &self.table[(p + 1) * TAPS..(p + 2) * TAPS];
        let first = i - (TAPS as i64 / 2 - 1);
        let (mut l, mut r) = (0f32, 0f32);
        if let Some(s) = src.contiguous(first, TAPS) {
            for k in 0..TAPS {
                let c = r0[k] + (r1[k] - r0[k]) * t;
                l += s[2 * k] * c;
                r += s[2 * k + 1] * c;
            }
        } else {
            for k in 0..TAPS {
                let x = src.frame(first + k as i64);
                let c = r0[k] + (r1[k] - r0[k]) * t;
                l += x[0] * c;
                r += x[1] * c;
            }
        }
        (l, r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_positions_are_nearly_exact() {
        let t = SincTable::default();
        let src: Vec<f32> = (0..200).flat_map(|i| [(i as f32 * 0.1).sin(); 2]).collect();
        for i in 20..180 {
            let (l, _) = t.read_stereo(&src, i as f64);
            assert!((l - src[i * 2]).abs() < 2e-3, "{i}");
        }
    }

    #[test]
    fn fractional_sine_is_accurate() {
        let t = SincTable::default();
        let f = 0.05; // cycles per sample, well below cutoff
        let src: Vec<f32> = (0..4000)
            .flat_map(|i| [(std::f64::consts::TAU * f * i as f64).sin() as f32; 2])
            .collect();
        let mut err = 0f32;
        for k in 0..1000 {
            let pos = 1000.0 + k as f64 * 1.37;
            let (l, _) = t.read_stereo(&src, pos);
            let ideal = (std::f64::consts::TAU * f * pos).sin() as f32;
            err = err.max((l - ideal).abs());
        }
        assert!(err < 1e-3, "{err}");
    }

    #[test]
    fn out_of_range_is_silent() {
        let t = SincTable::default();
        let src = vec![1.0f32; 100];
        assert_eq!(t.read_stereo(&src, -100.0), (0.0, 0.0));
        assert_eq!(t.read_stereo(&src, 500.0), (0.0, 0.0));
    }
}
