//! Downbeat detection: which of the four beat phases starts the bars.
//!
//! In four-on-the-floor music every beat has a kick, so the kick alone does
//! not reveal the bar. What does: harmony and bass change on bar lines,
//! crashes and new elements enter on downbeats, and arrangement changes
//! happen at phrase starts. Each cue is measured as spectral novelty at every
//! beat, normalized, and summed per phase.

use crate::features::{Features, N_BANDS, N_LOW};

fn beat_vector(f: &Features, t0: f64, t1: f64) -> Option<Vec<f32>> {
    let r = f.frame_range(t0, t1);
    if r.is_empty() {
        return None;
    }
    let n = r.len() as f32;
    let mut v = vec![0f32; N_BANDS + N_LOW];
    for i in r {
        for (b, &x) in f.bands[i].iter().enumerate() {
            v[b] += x / 10.0;
        }
        for (k, &x) in f.low_spec[i].iter().enumerate() {
            v[N_BANDS + k] += x;
        }
    }
    for x in &mut v {
        *x /= n;
    }
    Some(v)
}

fn mean_of(vs: &[Vec<f32>]) -> Vec<f32> {
    let mut m = vec![0f32; vs[0].len()];
    for v in vs {
        for (a, &b) in m.iter_mut().zip(v) {
            *a += b;
        }
    }
    for a in &mut m {
        *a /= vs.len() as f32;
    }
    m
}

fn dist(a: &[f32], b: &[f32]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| f64::from(x - y).powi(2))
        .sum::<f64>()
        .sqrt()
}

fn normalized(v: Vec<f64>) -> Vec<f64> {
    let mean = v.iter().sum::<f64>() / v.len().max(1) as f64;
    if mean <= 1e-12 {
        return vec![0.0; v.len()];
    }
    v.into_iter().map(|x| x / mean).collect()
}

/// Returns the phase `p` in `0..4` such that beats `p, p+4, p+8, ...` of
/// `beat_times` are downbeats.
pub fn detect(f: &Features, beat_times: &[f64], onset_env: &[f32]) -> usize {
    let n = beat_times.len();
    if n < 16 {
        return 0;
    }
    let vecs: Vec<Option<Vec<f32>>> = (0..n - 1)
        .map(|j| beat_vector(f, beat_times[j], beat_times[j + 1]))
        .collect();
    let mut nov1 = vec![0f64; n];
    let mut nov4 = vec![0f64; n];
    let mut accent = vec![0f64; n];
    for j in 1..n - 1 {
        if let (Some(a), Some(b)) = (&vecs[j - 1], &vecs[j]) {
            nov1[j] = dist(a, b);
        }
        if j >= 4 && j + 4 < n {
            let left: Option<Vec<Vec<f32>>> = vecs[j - 4..j].iter().cloned().collect();
            let right: Option<Vec<Vec<f32>>> = vecs[j..j + 4].iter().cloned().collect();
            if let (Some(l), Some(r)) = (left, right) {
                nov4[j] = dist(&mean_of(&l), &mean_of(&r));
            }
        }
        // Onset strength right at the beat (crashes, new elements).
        let c = f.time_to_frame(beat_times[j]).round();
        if c >= 0.0 {
            let c = c as usize;
            let a = c.saturating_sub(2);
            let b = (c + 3).min(onset_env.len());
            accent[j] = onset_env[a..b].iter().fold(0f32, |m, &v| m.max(v)).into();
        }
    }
    let (nov1, nov4, accent) = (normalized(nov1), normalized(nov4), normalized(accent));
    let mut scores = [0f64; 4];
    let mut counts = [0usize; 4];
    for j in 1..n - 1 {
        scores[j % 4] += nov1[j] + 1.5 * nov4[j] + 0.5 * accent[j];
        counts[j % 4] += 1;
    }
    (0..4)
        .max_by(|&a, &b| {
            (scores[a] / counts[a].max(1) as f64).total_cmp(&(scores[b] / counts[b].max(1) as f64))
        })
        .unwrap_or(0)
}
