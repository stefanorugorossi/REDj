//! Three-band DJ isolator EQ.
//!
//! LR4 crossovers at 250 Hz and 2.5 kHz split the signal into bands whose
//! sum is flat in magnitude (an all-pass), so at 0 dB the EQ is transparent
//! and each band can be fully killed. Band gains are plain multipliers: no
//! coefficient recalculation when knobs move, so automation is cheap and
//! sample accurate.

use crate::biquad::{Biquad, Lr4, lr4_allpass};

/// Gains at or below this (dB) are a full kill.
pub const KILL_DB: f32 = -40.0;
pub const LOW_HZ: f64 = 250.0;
pub const HIGH_HZ: f64 = 2500.0;

#[derive(Debug, Clone, Copy)]
struct Channel {
    lp_low: Lr4,
    hp_low: Lr4,
    lp_high: Lr4,
    hp_high: Lr4,
    /// Phase compensation of the low band for the upper crossover.
    ap_low: Biquad,
}

impl Channel {
    fn new(sr: f64) -> Self {
        Self {
            lp_low: Lr4::lowpass(LOW_HZ, sr),
            hp_low: Lr4::highpass(LOW_HZ, sr),
            lp_high: Lr4::lowpass(HIGH_HZ, sr),
            hp_high: Lr4::highpass(HIGH_HZ, sr),
            ap_low: lr4_allpass(HIGH_HZ, sr),
        }
    }

    #[inline]
    fn process(&mut self, x: f64, g: [f64; 3]) -> f64 {
        let low = self.ap_low.process(self.lp_low.process(x));
        let rest = self.hp_low.process(x);
        let mid = self.lp_high.process(rest);
        let high = self.hp_high.process(rest);
        low * g[0] + mid * g[1] + high * g[2]
    }

    fn reset(&mut self) {
        self.lp_low.reset();
        self.hp_low.reset();
        self.lp_high.reset();
        self.hp_high.reset();
        self.ap_low.reset();
    }
}

/// Stereo three-band isolator.
#[derive(Debug, Clone)]
pub struct IsolatorEq {
    ch: [Channel; 2],
}

/// Converts an EQ knob value in dB to a band gain (0 at or below
/// [`KILL_DB`]).
#[inline]
pub fn band_gain(db: f32) -> f64 {
    if db <= KILL_DB {
        0.0
    } else {
        10f64.powf(f64::from(db) / 20.0)
    }
}

impl IsolatorEq {
    pub fn new(sample_rate: f64) -> Self {
        let c = Channel::new(sample_rate);
        Self { ch: [c, c] }
    }

    /// Processes one stereo frame with linear band gains `[low, mid, high]`.
    #[inline]
    pub fn process(&mut self, l: f32, r: f32, gains: [f64; 3]) -> (f32, f32) {
        (
            self.ch[0].process(f64::from(l), gains) as f32,
            self.ch[1].process(f64::from(r), gains) as f32,
        )
    }

    pub fn reset(&mut self) {
        for c in &mut self.ch {
            c.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms_gain(freq: f64, gains: [f64; 3]) -> f64 {
        let sr = 44_100.0;
        let mut eq = IsolatorEq::new(sr);
        let n = 44_100;
        let (mut e_in, mut e_out) = (0.0, 0.0);
        for i in 0..n {
            let x = (std::f64::consts::TAU * freq * i as f64 / sr).sin() as f32;
            let (y, _) = eq.process(x, x, gains);
            if i > n / 2 {
                e_in += f64::from(x * x);
                e_out += f64::from(y * y);
            }
        }
        (e_out / e_in).sqrt()
    }

    #[test]
    fn flat_at_unity() {
        for f in [40.0, 250.0, 1000.0, 2500.0, 9000.0] {
            let g = rms_gain(f, [1.0, 1.0, 1.0]);
            assert!((g - 1.0).abs() < 0.01, "{f} Hz: {g}");
        }
    }

    #[test]
    fn kills_bands() {
        assert!(rms_gain(50.0, [0.0, 1.0, 1.0]) < 0.05);
        assert!(rms_gain(900.0, [1.0, 0.0, 1.0]) < 0.12);
        assert!(rms_gain(12_000.0, [1.0, 1.0, 0.0]) < 0.05);
        assert!((rms_gain(12_000.0, [0.0, 1.0, 1.0]) - 1.0).abs() < 0.02);
    }
}
