//! Key detection: chroma profile correlated with Krumhansl-Kessler key
//! profiles. Accuracy on real music is modest (roughly 60-75% exact,
//! more within one Camelot step); the confidence value says how clear the
//! winner was.

use realfft::RealFftPlanner;
use redj_core::{Key, Mode};

use crate::features::ANALYSIS_RATE;

const MAJOR: [f64; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const MINOR: [f64; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

fn pearson(a: &[f64; 12], b: &[f64; 12]) -> f64 {
    let ma = a.iter().sum::<f64>() / 12.0;
    let mb = b.iter().sum::<f64>() / 12.0;
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        num += (a[i] - ma) * (b[i] - mb);
        da += (a[i] - ma).powi(2);
        db += (b[i] - mb).powi(2);
    }
    num / (da * db).sqrt().max(1e-12)
}

/// Average chroma vector of a mono signal at [`ANALYSIS_RATE`].
pub fn chroma(mono: &[f32]) -> [f64; 12] {
    const N: usize = 8192;
    const HOP: usize = 4096;
    let sr = f64::from(ANALYSIS_RATE);
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(N);
    let mut input = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let window: Vec<f32> = (0..N)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / N as f32).cos())
        .collect();
    let bin_pc: Vec<Option<usize>> = (0..spec.len())
        .map(|k| {
            let f = k as f64 * sr / N as f64;
            if !(55.0..=2000.0).contains(&f) {
                return None;
            }
            let midi = 69.0 + 12.0 * (f / 440.0).log2();
            let r = midi.round();
            // Skip bins that fall between semitones.
            if (midi - r).abs() > 0.4 {
                return None;
            }
            Some((r as i64).rem_euclid(12) as usize)
        })
        .collect();
    let mut acc = [0f64; 12];
    let mut start = 0;
    while start + N <= mono.len() {
        for (k, x) in input.iter_mut().enumerate() {
            *x = mono[start + k] * window[k];
        }
        fft.process(&mut input, &mut spec).expect("fft sizes match");
        let mut frame = [0f64; 12];
        let mut total = 0.0;
        for (k, c) in spec.iter().enumerate() {
            if let Some(pc) = bin_pc[k] {
                let m = f64::from(c.norm());
                frame[pc] += m;
                total += m;
            }
        }
        if total > 1e-6 {
            // Per-frame normalization so loud parts do not dominate.
            for i in 0..12 {
                acc[i] += (frame[i] / total).sqrt();
            }
        }
        start += HOP;
    }
    acc
}

/// Detects the key from a chroma vector. Returns `None` for silence.
pub fn detect(chroma: &[f64; 12]) -> Option<Key> {
    if chroma.iter().sum::<f64>() <= 1e-9 {
        return None;
    }
    let mut scores: Vec<(f64, u8, Mode)> = Vec::with_capacity(24);
    for tonic in 0..12u8 {
        let mut rotated = [0f64; 12];
        for (i, r) in rotated.iter_mut().enumerate() {
            *r = chroma[(i + usize::from(tonic)) % 12];
        }
        scores.push((pearson(&rotated, &MAJOR), tonic, Mode::Major));
        scores.push((pearson(&rotated, &MINOR), tonic, Mode::Minor));
    }
    scores.sort_by(|a, b| b.0.total_cmp(&a.0));
    let (best, tonic, mode) = scores[0];
    let second = scores[1].0;
    let mut key = Key::new(tonic, mode);
    // Clear winner (large margin) and good fit (high correlation) both count.
    key.confidence = (best.max(0.0) * (0.3 + 7.0 * (best - second))).clamp(0.0, 1.0) as f32;
    Some(key)
}
