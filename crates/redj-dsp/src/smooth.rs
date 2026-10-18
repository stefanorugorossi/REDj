//! Parameter smoothing against zipper noise.

/// One-pole exponential smoother.
#[derive(Debug, Clone, Copy)]
pub struct Smoother {
    value: f32,
    target: f32,
    coeff: f32,
}

impl Smoother {
    /// `time_ms` is the time constant (63% of a step).
    pub fn new(initial: f32, time_ms: f32, sample_rate: f32) -> Self {
        Self {
            value: initial,
            target: initial,
            coeff: (-1.0 / (time_ms * 0.001 * sample_rate).max(1.0)).exp(),
        }
    }

    #[inline]
    pub fn set(&mut self, target: f32) {
        self.target = target;
    }

    /// Jumps to a value without smoothing.
    #[inline]
    pub fn reset(&mut self, value: f32) {
        self.value = value;
        self.target = value;
    }

    #[inline]
    pub fn next_value(&mut self) -> f32 {
        self.value = self.target + (self.value - self.target) * self.coeff;
        self.value
    }

    #[inline]
    pub fn value(&self) -> f32 {
        self.value
    }
}
