//! Short-time spectral features shared by the analysis stages.

use realfft::RealFftPlanner;

/// Analysis sample rate: enough bandwidth for onsets, cheap to process.
pub const ANALYSIS_RATE: u32 = 22_050;
pub const N_FFT: usize = 1024;
pub const HOP: usize = 128;
/// Number of log-spaced energy bands per frame.
pub const N_BANDS: usize = 8;
/// Number of low-frequency FFT bins kept per frame (bass detail).
pub const N_LOW: usize = 24;

/// Band edges in Hz for [`Features::bands`].
const BAND_EDGES: [f64; N_BANDS + 1] = [
    20.0, 60.0, 120.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 11025.0,
];
/// Upper edge of the kick band for [`Features::low_onset`].
const KICK_HZ: f64 = 150.0;

/// Per-frame features of a mono signal at [`ANALYSIS_RATE`].
#[derive(Debug, Clone)]
pub struct Features {
    pub fps: f64,
    /// Spectral flux over the full band (onset strength).
    pub onset: Vec<f32>,
    /// Spectral flux of the kick band only.
    pub low_onset: Vec<f32>,
    /// Log energy per band.
    pub bands: Vec<[f32; N_BANDS]>,
    /// Log magnitude of the lowest FFT bins (bass notes).
    pub low_spec: Vec<[f32; N_LOW]>,
    /// Total power per frame in dB.
    pub power_db: Vec<f32>,
}

impl Features {
    pub fn len(&self) -> usize {
        self.onset.len()
    }

    /// Fractional frame index at time `t`.
    #[inline]
    pub fn time_to_frame(&self, t: f64) -> f64 {
        (t * f64::from(ANALYSIS_RATE) - (N_FFT / 2) as f64) / HOP as f64
    }

    /// Frame index range covering `[t0, t1)`, clamped to the signal.
    pub fn frame_range(&self, t0: f64, t1: f64) -> std::ops::Range<usize> {
        let a = self.time_to_frame(t0).ceil().max(0.0) as usize;
        let b = self.time_to_frame(t1).ceil().max(0.0) as usize;
        a.min(self.len())..b.min(self.len())
    }
}

/// Computes features of a mono signal sampled at [`ANALYSIS_RATE`].
pub fn compute(mono: &[f32]) -> Features {
    let sr = f64::from(ANALYSIS_RATE);
    let n_frames = if mono.len() >= N_FFT {
        (mono.len() - N_FFT) / HOP + 1
    } else {
        0
    };
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(N_FFT);
    let window: Vec<f32> = (0..N_FFT)
        .map(|i| {
            let x = std::f32::consts::TAU * i as f32 / N_FFT as f32;
            0.5 - 0.5 * x.cos()
        })
        .collect();
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    let n_bins = spectrum.len();
    let bin_hz = sr / N_FFT as f64;
    let kick_bin = (KICK_HZ / bin_hz).ceil() as usize;
    let band_of: Vec<Option<usize>> = (0..n_bins)
        .map(|k| {
            let f = k as f64 * bin_hz;
            BAND_EDGES.windows(2).position(|w| f >= w[0] && f < w[1])
        })
        .collect();

    let mut f = Features {
        fps: sr / HOP as f64,
        onset: Vec::with_capacity(n_frames),
        low_onset: Vec::with_capacity(n_frames),
        bands: Vec::with_capacity(n_frames),
        low_spec: Vec::with_capacity(n_frames),
        power_db: Vec::with_capacity(n_frames),
    };
    let mut prev = vec![0f32; n_bins];
    let mut cur = vec![0f32; n_bins];
    let scale = 2.0 / N_FFT as f32;
    for i in 0..n_frames {
        let start = i * HOP;
        for (k, x) in input.iter_mut().enumerate() {
            *x = mono[start + k] * window[k];
        }
        fft.process_with_scratch(&mut input, &mut spectrum, &mut scratch)
            .expect("fft buffer sizes match");
        let mut flux = 0f32;
        let mut low_flux = 0f32;
        let mut bands = [0f32; N_BANDS];
        let mut power = 0f32;
        let mut low = [0f32; N_LOW];
        for k in 0..n_bins {
            let mag = spectrum[k].norm() * scale;
            let p = mag * mag;
            power += p;
            if let Some(b) = band_of[k] {
                bands[b] += p;
            }
            let lm = (1.0 + 1000.0 * mag).ln();
            cur[k] = lm;
            let d = lm - prev[k];
            if d > 0.0 {
                flux += d;
                if k <= kick_bin {
                    low_flux += d;
                }
            }
            if (1..=N_LOW).contains(&k) {
                low[k - 1] = lm;
            }
        }
        std::mem::swap(&mut prev, &mut cur);
        f.onset.push(if i == 0 { 0.0 } else { flux });
        f.low_onset.push(if i == 0 { 0.0 } else { low_flux });
        for b in &mut bands {
            *b = 10.0 * (*b + 1e-10).log10();
        }
        f.bands.push(bands);
        f.low_spec.push(low);
        f.power_db.push(10.0 * (power + 1e-10).log10());
    }
    f
}

/// Removes the local mean (moving average over `win` frames) and
/// half-wave rectifies, turning raw flux into a peaky onset envelope.
pub fn detrend(x: &[f32], win: usize) -> Vec<f32> {
    let n = x.len();
    if n == 0 {
        return Vec::new();
    }
    let half = win / 2;
    let mut prefix = vec![0f64; n + 1];
    for i in 0..n {
        prefix[i + 1] = prefix[i] + f64::from(x[i]);
    }
    (0..n)
        .map(|i| {
            let a = i.saturating_sub(half);
            let b = (i + half + 1).min(n);
            let mean = (prefix[b] - prefix[a]) / (b - a) as f64;
            (f64::from(x[i]) - mean).max(0.0) as f32
        })
        .collect()
}
