//! Tempo-synced stereo echo with trails.
//!
//! The deck sends `send * signal` into the echo; the echo output is added
//! after the channel fader, so closing the fader lets the repeats ring out
//! (the classic "echo out" transition).

#[derive(Debug, Clone)]
pub struct Echo {
    buf: Vec<[f32; 2]>,
    write: usize,
    delay: f32,
    /// One-pole high-pass state in the feedback path (thins the repeats).
    hp_state: [f32; 2],
    hp_prev: [f32; 2],
    hp_coeff: f32,
}

impl Echo {
    /// `max_delay_sec` bounds the delay time (buffer allocated here).
    pub fn new(sample_rate: f32, max_delay_sec: f32) -> Self {
        let len = (sample_rate * max_delay_sec) as usize + 4;
        Self {
            buf: vec![[0.0; 2]; len],
            write: 0,
            delay: sample_rate * 0.375,
            hp_state: [0.0; 2],
            hp_prev: [0.0; 2],
            hp_coeff: (-std::f32::consts::TAU * 250.0 / sample_rate).exp(),
        }
    }

    /// Sets the delay in samples (clamped to the buffer).
    #[inline]
    pub fn set_delay_samples(&mut self, d: f32) {
        self.delay = d.clamp(1.0, (self.buf.len() - 3) as f32);
    }

    /// Feeds one frame of send signal; returns the wet output.
    #[inline]
    pub fn process(&mut self, l: f32, r: f32, feedback: f32) -> (f32, f32) {
        let len = self.buf.len();
        let read = (self.write as f32 - self.delay).rem_euclid(len as f32);
        let i = read as usize;
        let f = read - i as f32;
        let a = self.buf[i % len];
        let b = self.buf[(i + 1) % len];
        let wet = [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f];
        let mut fb = [0f32; 2];
        for c in 0..2 {
            // y[n] = k * (y[n-1] + x[n] - x[n-1])
            let y = self.hp_coeff * (self.hp_state[c] + wet[c] - self.hp_prev[c]);
            self.hp_prev[c] = wet[c];
            self.hp_state[c] = y;
            fb[c] = y * feedback;
        }
        self.buf[self.write] = [l + fb[0], r + fb[1]];
        self.write = (self.write + 1) % len;
        (wet[0], wet[1])
    }

    pub fn reset(&mut self) {
        self.buf.iter_mut().for_each(|s| *s = [0.0; 2]);
        self.hp_state = [0.0; 2];
        self.hp_prev = [0.0; 2];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeats_at_delay_and_decays() {
        let mut e = Echo::new(1000.0, 2.0);
        e.set_delay_samples(100.0);
        let mut out = Vec::new();
        for n in 0..1000 {
            let x = if n == 0 { 1.0 } else { 0.0 };
            out.push(e.process(x, x, 0.5).0);
        }
        assert!(out[100] > 0.5);
        assert!(out[200].abs() > 0.1 && out[200].abs() < out[100]);
        assert!(out[50].abs() < 1e-6);
    }
}
