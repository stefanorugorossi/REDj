//! Time-stretching with constant pitch (keylock).
//!
//! [`Wsola`] is position driven: instead of a stretch ratio it receives, for
//! every output frame, the source position the playhead should be at. Each
//! new grain is placed near that position at the offset that best continues
//! the previous grain (waveform similarity), so tempo changes, loops and
//! jumps never accumulate drift.

use crate::source::Source;

/// A keylock time-stretcher fed with the desired source position per
/// output frame.
pub trait TimeStretcher {
    /// Produces the next output frame. `src` is the stereo source and
    /// `pos` the desired source frame position for this output frame.
    fn next_frame(&mut self, src: &dyn Source, pos: f64) -> (f32, f32);
    /// Forgets all history (next frame starts a fresh grain).
    fn reset(&mut self);
}

/// Waveform-similarity overlap-add stretcher.
#[derive(Debug, Clone)]
pub struct Wsola {
    frame: usize,
    hop: usize,
    search: i64,
    window: Vec<f32>,
    ola: Vec<[f32; 2]>,
    read: usize,
    prev_start: Option<i64>,
    prev_pos: f64,
}

impl Wsola {
    pub fn new(sample_rate: f32) -> Self {
        // ~46 ms grains at 44.1 kHz, power of two for cheap modulo.
        let frame = ((sample_rate * 0.046) as usize).next_power_of_two();
        let hop = frame / 2;
        let window = (0..frame)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / frame as f32).cos())
            .collect();
        Self {
            frame,
            hop,
            search: (sample_rate * 0.012) as i64,
            window,
            ola: vec![[0.0; 2]; frame],
            read: hop, // forces a grain on the first call
            prev_start: None,
            prev_pos: 0.0,
        }
    }

    /// Cross-correlation of the mid signal at two source offsets over the
    /// overlap length, sampled every `step` frames.
    fn similarity(src: &dyn Source, a: i64, b: i64, len: usize, step: usize) -> f32 {
        let mut acc = 0f32;
        let mut j = 0;
        while j < len {
            let x = src.frame(a + j as i64);
            let y = src.frame(b + j as i64);
            acc += (x[0] + x[1]) * (y[0] + y[1]);
            j += step;
        }
        acc
    }

    fn next_grain(&mut self, src: &dyn Source, pos: f64) {
        // Shift the overlap-add buffer by one hop.
        self.ola.copy_within(self.hop.., 0);
        for s in &mut self.ola[self.frame - self.hop..] {
            *s = [0.0; 2];
        }
        let hop = self.hop as i64;
        let rate = match self.prev_start {
            Some(_) => ((pos - self.prev_pos) / self.hop as f64).clamp(0.25, 4.0),
            None => 1.0,
        };
        // Centre of the new grain (output now + hop) should read the source
        // at pos + rate * hop.
        let target = (pos + (rate - 1.0) * self.hop as f64).round() as i64;
        let start = match self.prev_start {
            Some(prev) => {
                let natural = prev + hop;
                if (target - natural).abs() > 8 * hop {
                    target // a jump: no continuity to preserve
                } else {
                    // Coarse search, then refine around the best offset. A
                    // mild penalty on the distance from the target keeps
                    // the stretcher transparent when periodic material
                    // makes several offsets equally similar.
                    let search = self.search as f32;
                    let weight = |s: f32, at: i64| {
                        s * (1.0 - 0.2 * ((at - target).abs() as f32 / search).min(1.0))
                    };
                    let mut best = (target, f32::MIN);
                    let mut off = -self.search;
                    while off <= self.search {
                        let at = target + off;
                        let s = weight(Self::similarity(src, natural, at, self.hop, 4), at);
                        if s > best.1 {
                            best = (at, s);
                        }
                        off += 8;
                    }
                    let coarse = best.0;
                    let mut fine = (coarse, f32::MIN);
                    for off in -8..=8 {
                        let at = coarse + off;
                        let s = weight(Self::similarity(src, natural, at, self.hop, 1), at);
                        if s > fine.1 {
                            fine = (at, s);
                        }
                    }
                    fine.0
                }
            }
            None => target,
        };
        for (j, o) in self.ola.iter_mut().enumerate() {
            let x = src.frame(start + j as i64);
            let w = self.window[j];
            o[0] += x[0] * w;
            o[1] += x[1] * w;
        }
        self.prev_start = Some(start);
        self.prev_pos = pos;
        self.read = 0;
    }
}

impl TimeStretcher for Wsola {
    #[inline]
    fn next_frame(&mut self, src: &dyn Source, pos: f64) -> (f32, f32) {
        if self.read >= self.hop {
            self.next_grain(src, pos);
        }
        let s = self.ola[self.read];
        self.read += 1;
        (s[0], s[1])
    }

    fn reset(&mut self) {
        self.ola.iter_mut().for_each(|s| *s = [0.0; 2]);
        self.read = self.hop;
        self.prev_start = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, sr: f64, n: usize) -> Vec<f32> {
        (0..n)
            .flat_map(|i| [(std::f64::consts::TAU * freq * i as f64 / sr).sin() as f32 * 0.5; 2])
            .collect()
    }

    /// Dominant frequency by zero-crossing count.
    fn zc_freq(x: &[f32], sr: f64) -> f64 {
        let mono: Vec<f32> = x.chunks(2).map(|c| c[0]).collect();
        let crossings = mono
            .windows(2)
            .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
            .count();
        crossings as f64 * sr / mono.len() as f64
    }

    #[test]
    fn unity_rate_is_transparent() {
        let sr = 44_100.0;
        let src = sine(440.0, sr, 44_100);
        let mut w = Wsola::new(sr as f32);
        let mut out = Vec::new();
        for i in 0..30_000 {
            let (l, r) = w.next_frame(&src, i as f64);
            out.push(l);
            out.push(r);
        }
        for i in 5_000..25_000 {
            assert!((out[i * 2] - src[i * 2]).abs() < 1e-3, "frame {i}");
        }
    }

    #[test]
    fn stretching_keeps_pitch_and_follows_position() {
        let sr = 44_100.0;
        let src = sine(440.0, sr, 200_000);
        for rate in [0.92, 1.08] {
            let mut w = Wsola::new(sr as f32);
            let mut out = Vec::new();
            for i in 0..100_000 {
                let (l, r) = w.next_frame(&src, i as f64 * rate);
                out.push(l);
                out.push(r);
            }
            let f = zc_freq(&out[20_000..180_000], sr);
            assert!((f - 440.0).abs() < 3.0, "rate {rate}: {f} Hz");
        }
    }
}
