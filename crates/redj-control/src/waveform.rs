//! Three-band waveform summaries for display.

/// Frames per waveform bin.
pub const BIN_FRAMES: usize = 256;

/// One bin: peak level and band energies, each 0..255.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WaveBin {
    pub peak: u8,
    pub low: u8,
    pub mid: u8,
    pub high: u8,
}

/// Waveform of a whole track.
#[derive(Debug, Clone, Default)]
pub struct Waveform {
    pub bins: Vec<WaveBin>,
    pub sample_rate: u32,
}

impl Waveform {
    /// Computes the summary of interleaved stereo audio.
    pub fn compute(samples: &[f32], sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let a_low = (-std::f32::consts::TAU * 200.0 / sr).exp();
        let a_high = (-std::f32::consts::TAU * 2500.0 / sr).exp();
        let (mut lp, mut lp2) = (0f32, 0f32);
        let mut bins = Vec::with_capacity(samples.len() / 2 / BIN_FRAMES + 1);
        for chunk in samples.chunks(BIN_FRAMES * 2) {
            let (mut peak, mut el, mut em, mut eh) = (0f32, 0f32, 0f32, 0f32);
            for f in chunk.chunks_exact(2) {
                let x = 0.5 * (f[0] + f[1]);
                peak = peak.max(f[0].abs()).max(f[1].abs());
                lp = a_low * lp + (1.0 - a_low) * x;
                lp2 = a_high * lp2 + (1.0 - a_high) * x;
                let low = lp;
                let high = x - lp2;
                let mid = lp2 - lp;
                el += low * low;
                em += mid * mid;
                eh += high * high;
            }
            let n = (chunk.len() / 2).max(1) as f32;
            let q = |e: f32, gain: f32| ((e / n).sqrt() * gain * 255.0).min(255.0) as u8;
            bins.push(WaveBin {
                peak: (peak.min(1.0) * 255.0) as u8,
                low: q(el, 1.6),
                mid: q(em, 2.2),
                high: q(eh, 3.5),
            });
        }
        Self { bins, sample_rate }
    }

    /// Bin index of a source frame.
    pub fn bin_at(&self, frame: f64) -> f64 {
        frame / BIN_FRAMES as f64
    }
}
