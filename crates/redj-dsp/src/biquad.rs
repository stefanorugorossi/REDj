//! Second-order IIR sections (RBJ audio EQ cookbook), transposed direct
//! form II with `f64` state.

use std::f64::consts::PI;

#[derive(Debug, Clone, Copy, Default)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    fn from_raw(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn prewarp(freq: f64, sr: f64, q: f64) -> (f64, f64) {
        let w = 2.0 * PI * (freq / sr).clamp(1e-6, 0.4999);
        (w.cos(), w.sin() / (2.0 * q))
    }

    pub fn lowpass(freq: f64, q: f64, sr: f64) -> Self {
        let (c, alpha) = Self::prewarp(freq, sr, q);
        Self::from_raw(
            (1.0 - c) / 2.0,
            1.0 - c,
            (1.0 - c) / 2.0,
            1.0 + alpha,
            -2.0 * c,
            1.0 - alpha,
        )
    }

    pub fn highpass(freq: f64, q: f64, sr: f64) -> Self {
        let (c, alpha) = Self::prewarp(freq, sr, q);
        Self::from_raw(
            (1.0 + c) / 2.0,
            -(1.0 + c),
            (1.0 + c) / 2.0,
            1.0 + alpha,
            -2.0 * c,
            1.0 - alpha,
        )
    }

    pub fn allpass(freq: f64, q: f64, sr: f64) -> Self {
        let (c, alpha) = Self::prewarp(freq, sr, q);
        Self::from_raw(
            1.0 - alpha,
            -2.0 * c,
            1.0 + alpha,
            1.0 + alpha,
            -2.0 * c,
            1.0 - alpha,
        )
    }

    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    /// Magnitude response at `freq` (for tests and plots).
    pub fn magnitude(&self, freq: f64, sr: f64) -> f64 {
        let w = 2.0 * PI * freq / sr;
        let (c1, s1) = (w.cos(), w.sin());
        let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
    }
}

/// Fourth-order Linkwitz-Riley filter: two cascaded Butterworth sections.
#[derive(Debug, Clone, Copy)]
pub struct Lr4 {
    a: Biquad,
    b: Biquad,
}

impl Lr4 {
    const Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

    pub fn lowpass(freq: f64, sr: f64) -> Self {
        let s = Biquad::lowpass(freq, Self::Q, sr);
        Self { a: s, b: s }
    }

    pub fn highpass(freq: f64, sr: f64) -> Self {
        let s = Biquad::highpass(freq, Self::Q, sr);
        Self { a: s, b: s }
    }

    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        self.b.process(self.a.process(x))
    }

    pub fn reset(&mut self) {
        self.a.reset();
        self.b.reset();
    }
}

/// Second-order all-pass with the phase response of an LR4 crossover sum
/// at `freq` (LR4 low + high = this all-pass).
pub fn lr4_allpass(freq: f64, sr: f64) -> Biquad {
    Biquad::allpass(freq, std::f64::consts::FRAC_1_SQRT_2, sr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowpass_response() {
        let f = Biquad::lowpass(1000.0, std::f64::consts::FRAC_1_SQRT_2, 48_000.0);
        assert!((f.magnitude(10.0, 48_000.0) - 1.0).abs() < 1e-3);
        assert!((f.magnitude(1000.0, 48_000.0) - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-3);
        assert!(f.magnitude(10_000.0, 48_000.0) < 0.02);
    }

    #[test]
    fn lr4_sum_matches_allpass() {
        // Feed an impulse through LP+HP and through the all-pass: identical.
        let sr = 44_100.0;
        let mut lp = Lr4::lowpass(300.0, sr);
        let mut hp = Lr4::highpass(300.0, sr);
        let mut ap = lr4_allpass(300.0, sr);
        for n in 0..2000 {
            let x = if n == 0 { 1.0 } else { 0.0 };
            let s = lp.process(x) + hp.process(x);
            let a = ap.process(x);
            assert!((s - a).abs() < 1e-9, "n={n} {s} {a}");
        }
    }
}
