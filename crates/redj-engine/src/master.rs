use redj_dsp::{Smoother, TruePeakLimiter};

/// Master section: gain and true-peak limiter.
#[derive(Debug, Clone)]
pub struct Master {
    gain: Smoother,
    limiter: TruePeakLimiter,
    peak: f32,
}

impl Master {
    pub fn new(sample_rate: u32, ceiling_db: f32) -> Self {
        let sr = sample_rate as f32;
        Self {
            gain: Smoother::new(1.0, 10.0, sr),
            limiter: TruePeakLimiter::new(sr, ceiling_db),
            peak: 0.0,
        }
    }

    /// Master gain in dB (applied before the limiter).
    pub fn set_gain_db(&mut self, db: f32) {
        self.gain.set(redj_dsp::db_to_gain(db));
    }

    /// Latency added by the limiter look-ahead, in frames.
    pub fn latency(&self) -> usize {
        self.limiter.latency()
    }

    /// Processes interleaved stereo in place.
    pub fn process(&mut self, buf: &mut [f32]) {
        for f in buf.chunks_exact_mut(2) {
            let g = self.gain.next_value();
            let (l, r) = self.limiter.process(f[0] * g, f[1] * g);
            self.peak = self.peak.max(l.abs()).max(r.abs());
            f[0] = l;
            f[1] = r;
        }
    }

    /// Peak level and lowest limiter gain since the last call (meters).
    pub fn take_meters(&mut self) -> (f32, f32) {
        (std::mem::take(&mut self.peak), self.limiter.take_min_gain())
    }
}
