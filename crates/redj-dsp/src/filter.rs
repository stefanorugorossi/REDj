//! One-knob DJ filter: low-pass to the left, high-pass to the right.

/// Topology-preserving-transform state-variable filter (stable under fast
/// modulation), stereo.
#[derive(Debug, Clone)]
pub struct DjFilter {
    sr: f32,
    ic1: [f32; 2],
    ic2: [f32; 2],
    /// Wet amount, cross-faded in and out of bypass.
    wet: f32,
}

/// Knob values with magnitude below this are bypass.
const DEAD_ZONE: f32 = 0.02;
/// 1/Q: slightly resonant.
const RESONANCE_K: f32 = 1.1;

impl DjFilter {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sr: sample_rate,
            ic1: [0.0; 2],
            ic2: [0.0; 2],
            wet: 0.0,
        }
    }

    /// Cutoff for a knob position: -1 => 80 Hz low-pass, 1 => 9 kHz
    /// high-pass, sweeping exponentially.
    pub fn cutoff_hz(knob: f32) -> f32 {
        let a = knob.abs().clamp(0.0, 1.0);
        if knob < 0.0 {
            20_000.0 * (80.0f32 / 20_000.0).powf(a)
        } else {
            20.0 * (9_000.0f32 / 20.0).powf(a)
        }
    }

    /// Processes one stereo frame with knob position `knob` in -1..1.
    #[inline]
    pub fn process(&mut self, l: f32, r: f32, knob: f32) -> (f32, f32) {
        let target_wet = if knob.abs() < DEAD_ZONE { 0.0 } else { 1.0 };
        // ~5 ms bypass cross-fade.
        self.wet += (target_wet - self.wet) * (1.0 / (0.005 * self.sr)).min(1.0);
        if self.wet < 1e-6 && target_wet == 0.0 {
            self.ic1 = [0.0; 2];
            self.ic2 = [0.0; 2];
            return (l, r);
        }
        let fc = Self::cutoff_hz(knob).min(self.sr * 0.45);
        let g = (std::f32::consts::PI * fc / self.sr).tan();
        let a1 = 1.0 / (1.0 + g * (g + RESONANCE_K));
        let a2 = g * a1;
        let a3 = g * a2;
        let highpass = knob > 0.0;
        let mut out = [l, r];
        for (c, x) in out.iter_mut().enumerate() {
            let v3 = *x - self.ic2[c];
            let v1 = a1 * self.ic1[c] + a2 * v3;
            let v2 = self.ic2[c] + a2 * self.ic1[c] + a3 * v3;
            self.ic1[c] = 2.0 * v1 - self.ic1[c];
            self.ic2[c] = 2.0 * v2 - self.ic2[c];
            let y = if highpass {
                *x - RESONANCE_K * v1 - v2
            } else {
                v2
            };
            *x += (y - *x) * self.wet;
        }
        (out[0], out[1])
    }

    pub fn reset(&mut self) {
        self.ic1 = [0.0; 2];
        self.ic2 = [0.0; 2];
        self.wet = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gain(freq: f32, knob: f32) -> f32 {
        let sr = 44_100.0;
        let mut f = DjFilter::new(sr);
        let (mut a, mut b) = (0.0, 0.0);
        for i in 0..44_100 {
            let x = (std::f32::consts::TAU * freq * i as f32 / sr).sin();
            let (y, _) = f.process(x, x, knob);
            if i > 22_050 {
                a += x * x;
                b += y * y;
            }
        }
        (b / a).sqrt()
    }

    #[test]
    fn bypass_lowpass_highpass() {
        assert!((gain(1000.0, 0.0) - 1.0).abs() < 1e-3);
        assert!(gain(5000.0, -0.8) < 0.05);
        assert!((gain(60.0, -0.8) - 1.0).abs() < 0.1);
        assert!(gain(60.0, 0.8) < 0.05);
        assert!((gain(12_000.0, 0.8) - 1.0).abs() < 0.15);
    }
}
