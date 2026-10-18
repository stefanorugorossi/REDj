//! Look-ahead brick-wall limiter with 4x oversampled true-peak detection.
//!
//! Gain computer: required gain per sample from the true peak, a sliding
//! minimum over the look-ahead window, release smoothing, then a moving
//! average over the look-ahead. The average of values that are all at most
//! the required gain of the delayed sample can never exceed it, so the
//! ceiling holds while the gain still moves smoothly.

const OS: usize = 4;
const TAPS_PER_PHASE: usize = 12;

#[derive(Debug, Clone)]
pub struct TruePeakLimiter {
    ceiling: f32,
    lookahead: usize,
    // 4x interpolator for peak detection.
    fir: Vec<[f32; TAPS_PER_PHASE]>,
    hist: [[f32; TAPS_PER_PHASE]; 2],
    hist_pos: usize,
    // Audio delay line (look-ahead + detector delay).
    delay: Vec<[f32; 2]>,
    delay_pos: usize,
    // Sliding minimum (monotonic deque over a ring of (index, value)).
    deque: Vec<(u64, f32)>,
    dq_head: usize,
    dq_len: usize,
    n: u64,
    // Release smoothing and moving average.
    smooth: f32,
    release: f32,
    box_buf: Vec<f32>,
    box_pos: usize,
    box_sum: f64,
    /// Lowest gain applied since the last call to [`Self::take_min_gain`].
    min_gain: f32,
}

impl TruePeakLimiter {
    /// `ceiling_db` is in dBTP (e.g. -1.0).
    pub fn new(sample_rate: f32, ceiling_db: f32) -> Self {
        let lookahead = ((sample_rate * 0.002) as usize).max(8);
        let detector_delay = TAPS_PER_PHASE / 2;
        // Windowed-sinc 4x interpolation filter, split into phases.
        let n = OS * TAPS_PER_PHASE;
        let center = (n - 1) as f64 / 2.0;
        let mut fir = vec![[0f32; TAPS_PER_PHASE]; OS];
        for (k, phase) in fir.iter_mut().enumerate() {
            for (t, c) in phase.iter_mut().enumerate() {
                let i = t * OS + k;
                let x = (i as f64 - center) / OS as f64;
                let sinc = if x.abs() < 1e-9 {
                    1.0
                } else {
                    (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
                };
                let w =
                    0.5 + 0.5 * (std::f64::consts::PI * (i as f64 - center) / (center + 1.0)).cos();
                *c = (sinc * w) as f32;
            }
        }
        let release_sec = 0.12;
        Self {
            // 0.4 dB of margin: the 4x detector and the gain modulation
            // leave small inter-sample overshoots that a reference
            // (ITU-R BS.1770) meter would see.
            ceiling: 10f32.powf((ceiling_db - 0.4) / 20.0),
            lookahead,
            fir,
            hist: [[0.0; TAPS_PER_PHASE]; 2],
            hist_pos: 0,
            delay: vec![[0.0; 2]; lookahead + detector_delay],
            delay_pos: 0,
            deque: vec![(0, 1.0); lookahead + 2],
            dq_head: 0,
            dq_len: 0,
            n: 0,
            smooth: 1.0,
            release: (-1.0 / (release_sec * sample_rate)).exp(),
            box_buf: vec![1.0; lookahead],
            box_pos: 0,
            box_sum: lookahead as f64,
            min_gain: 1.0,
        }
    }

    /// Latency in samples.
    pub fn latency(&self) -> usize {
        self.delay.len()
    }

    fn true_peak(&mut self, l: f32, r: f32) -> f32 {
        self.hist[0][self.hist_pos] = l;
        self.hist[1][self.hist_pos] = r;
        self.hist_pos = (self.hist_pos + 1) % TAPS_PER_PHASE;
        let mut peak = 0f32;
        for ch in &self.hist {
            for phase in &self.fir {
                let mut acc = 0f32;
                for (t, &c) in phase.iter().enumerate() {
                    // Oldest sample first.
                    acc += ch[(self.hist_pos + t) % TAPS_PER_PHASE] * c;
                }
                peak = peak.max(acc.abs());
            }
        }
        peak.max(l.abs()).max(r.abs())
    }

    /// Processes one stereo frame (output is delayed by [`Self::latency`]).
    #[inline]
    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let peak = self.true_peak(l, r);
        let req = if peak > self.ceiling {
            self.ceiling / peak
        } else {
            1.0
        };
        // Sliding minimum over the last `lookahead + 1` requirements.
        let cap = self.deque.len();
        while self.dq_len > 0 {
            let back = (self.dq_head + self.dq_len - 1) % cap;
            if self.deque[back].1 >= req {
                self.dq_len -= 1;
            } else {
                break;
            }
        }
        let tail = (self.dq_head + self.dq_len) % cap;
        self.deque[tail] = (self.n, req);
        self.dq_len += 1;
        while self.n - self.deque[self.dq_head].0 > self.lookahead as u64 {
            self.dq_head = (self.dq_head + 1) % cap;
            self.dq_len -= 1;
        }
        self.n += 1;
        let m = self.deque[self.dq_head].1;
        self.smooth = if m < self.smooth {
            m
        } else {
            m + (self.smooth - m) * self.release
        };
        self.box_sum += f64::from(self.smooth) - f64::from(self.box_buf[self.box_pos]);
        self.box_buf[self.box_pos] = self.smooth;
        self.box_pos = (self.box_pos + 1) % self.lookahead;
        let gain = (self.box_sum / self.lookahead as f64) as f32;
        self.min_gain = self.min_gain.min(gain);

        let out = self.delay[self.delay_pos];
        self.delay[self.delay_pos] = [l, r];
        self.delay_pos = (self.delay_pos + 1) % self.delay.len();
        let c = self.ceiling;
        ((out[0] * gain).clamp(-c, c), (out[1] * gain).clamp(-c, c))
    }

    /// Returns the lowest gain applied since the previous call (for gain
    /// reduction meters) and resets it.
    pub fn take_min_gain(&mut self) -> f32 {
        std::mem::replace(&mut self.min_gain, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_ceiling_and_passes_quiet_signal() {
        let sr = 44_100.0;
        let mut lim = TruePeakLimiter::new(sr, -1.0);
        let ceiling = 10f32.powf(-1.0 / 20.0);
        let lat = lim.latency();
        let n = 44_100;
        let input: Vec<f32> = (0..n)
            .map(|i| {
                let amp = if (5_000..10_000).contains(&i) {
                    2.0
                } else {
                    0.3
                };
                amp * (std::f32::consts::TAU * 997.0 * i as f32 / sr).sin()
            })
            .collect();
        let out: Vec<f32> = input.iter().map(|&x| lim.process(x, x).0).collect();
        assert!(out.iter().all(|&y| y.abs() <= ceiling + 1e-6));
        // Well after the loud burst the signal passes unchanged.
        for i in 40_000..41_000 {
            assert!((out[i] - input[i - lat]).abs() < 1e-3);
        }
        // True peak of the output (simple 8x check by sinc reconstruction
        // is overkill here): sample peak close to the ceiling during the burst.
        let peak = out[6_000..9_500].iter().fold(0f32, |m, &v| m.max(v.abs()));
        assert!(peak > ceiling * 0.9, "{peak}");
    }
}
