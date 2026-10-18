//! Tempo estimation: autocorrelation for the coarse tempo, then a
//! whole-track fit of a constant beat period ("folding") for precision.

use realfft::RealFftPlanner;

/// Autocorrelation of `x` (mean removed) for lags `0..max_lag`, via FFT.
fn autocorrelation(x: &[f32], max_lag: usize) -> Vec<f64> {
    let n = x.len();
    let size = (2 * n).next_power_of_two().max(2);
    let mean = x.iter().map(|&v| f64::from(v)).sum::<f64>() / n.max(1) as f64;
    let mut planner = RealFftPlanner::<f64>::new();
    let fwd = planner.plan_fft_forward(size);
    let inv = planner.plan_fft_inverse(size);
    let mut buf = fwd.make_input_vec();
    for (b, &v) in buf.iter_mut().zip(x) {
        *b = f64::from(v) - mean;
    }
    let mut spec = fwd.make_output_vec();
    fwd.process(&mut buf, &mut spec).expect("fft sizes match");
    for c in &mut spec {
        *c = realfft::num_complex::Complex::new(c.norm_sqr(), 0.0);
    }
    let mut out = inv.make_output_vec();
    inv.process(&mut spec, &mut out).expect("fft sizes match");
    let norm = out[0].max(1e-12);
    out.iter().take(max_lag.min(n)).map(|v| v / norm).collect()
}

fn interp(v: &[f64], x: f64) -> f64 {
    if x < 0.0 {
        return 0.0;
    }
    let i = x.floor() as usize;
    if i + 1 >= v.len() {
        return 0.0;
    }
    let f = x - i as f64;
    v[i] * (1.0 - f) + v[i + 1] * f
}

/// Coarse tempo from the autocorrelation of the onset envelope, restricted
/// to `[min_bpm, max_bpm)`, with a mild preference for club tempos.
pub fn coarse_bpm(env: &[f32], fps: f64, min_bpm: f64, max_bpm: f64) -> Option<f64> {
    let max_lag = (60.0 * fps / min_bpm * 4.5).ceil() as usize + 2;
    if env.len() < max_lag * 2 {
        return None;
    }
    let acf = autocorrelation(env, max_lag);
    let mut best = None;
    let mut best_score = f64::MIN;
    let mut bpm = min_bpm;
    while bpm < max_bpm {
        let lag = 60.0 * fps / bpm;
        let comb: f64 = (1..=4)
            .map(|m| interp(&acf, lag * m as f64) / m as f64)
            .sum();
        // Log-normal prior around 128 BPM, one octave wide.
        let prior = (-0.5 * ((bpm / 128.0).log2() / 0.8).powi(2)).exp();
        let score = comb * (0.5 + 0.5 * prior);
        if score > best_score {
            best_score = score;
            best = Some(bpm);
        }
        bpm += 0.25;
    }
    best
}

/// Resolves octave errors of a coarse tempo using the kick envelope. In
/// four-on-the-floor music the kick hits every beat, so the fastest
/// in-range candidate whose kick pulse is (nearly) as clear as the best one
/// is the beat; claps every other beat otherwise pull the estimate to half
/// tempo.
pub fn resolve_octave(kick_env: &[f32], fps: f64, bpm: f64, min_bpm: f64, max_bpm: f64) -> f64 {
    let sharp = |b: f64| fold(kick_env, 60.0 * fps / b, 64).0;
    let mut cands: Vec<f64> = [bpm / 2.0, bpm, bpm * 2.0]
        .into_iter()
        .filter(|b| (min_bpm..max_bpm).contains(b))
        .collect();
    if cands.len() < 2 {
        return bpm;
    }
    let best = cands.iter().map(|&b| sharp(b)).fold(f64::MIN, f64::max);
    cands.sort_by(|a, b| b.total_cmp(a));
    cands
        .into_iter()
        .find(|&b| sharp(b) >= 0.75 * best)
        .unwrap_or(bpm)
}

/// Folds the envelope modulo a beat period of `period` frames into `bins`
/// phase bins. Returns the peak-to-mean ratio of the folded histogram
/// (sharpness of the alignment) and the peak phase in frames.
pub fn fold(env: &[f32], period: f64, bins: usize) -> (f64, f64) {
    let mut hist = vec![0f64; bins];
    let mut count = vec![0u32; bins];
    let inv = 1.0 / period;
    for (i, &v) in env.iter().enumerate() {
        let ph = (i as f64 * inv).fract();
        let b = ((ph * bins as f64) as usize).min(bins - 1);
        hist[b] += f64::from(v);
        count[b] += 1;
    }
    for (h, &c) in hist.iter_mut().zip(&count) {
        if c > 0 {
            *h /= f64::from(c);
        }
    }
    let smooth: Vec<f64> = (0..bins)
        .map(|b| 0.25 * hist[(b + bins - 1) % bins] + 0.5 * hist[b] + 0.25 * hist[(b + 1) % bins])
        .collect();
    let mean = smooth.iter().sum::<f64>() / bins as f64;
    let (peak, &max) = smooth
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .expect("bins > 0");
    // Parabolic interpolation of the peak position.
    let y0 = smooth[(peak + bins - 1) % bins];
    let y2 = smooth[(peak + 1) % bins];
    let denom = y0 - 2.0 * max + y2;
    let delta = if denom.abs() > 1e-12 {
        (0.5 * (y0 - y2) / denom).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    let phase_bins = (peak as f64 + 0.5 + delta).rem_euclid(bins as f64);
    (max / mean.max(1e-12), phase_bins / bins as f64 * period)
}

/// Phase-invariant periodicity strength of `env` at a beat period of
/// `period` frames: weighted magnitudes of the first four harmonics of the
/// beat frequency. Continuous in `period` (no binning), so its maximum is an
/// unbiased tempo estimate. Also returns the phase of the fundamental as
/// the position of a beat in frames.
pub fn harmonic_score(env: &[f32], period: f64) -> (f64, f64) {
    let mut total = 0.0;
    let mut phase = 0.0;
    for h in 1..=4 {
        let w = std::f64::consts::TAU * h as f64 / period;
        let (s, c) = w.sin_cos();
        // Phasor recurrence instead of a sin/cos per sample.
        let (mut pr, mut pi) = (1.0f64, 0.0f64);
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, &v) in env.iter().enumerate() {
            let v = f64::from(v);
            re += v * pr;
            im -= v * pi;
            let npr = pr * c - pi * s;
            pi = pr * s + pi * c;
            pr = npr;
            if i % 4096 == 4095 {
                let n = (pr * pr + pi * pi).sqrt();
                pr /= n;
                pi /= n;
            }
        }
        if h == 1 {
            phase = ((-im).atan2(re) / std::f64::consts::TAU * period).rem_euclid(period);
        }
        total += (re * re + im * im).sqrt() / h as f64;
    }
    (total, phase)
}

/// Refines a coarse tempo by maximizing the harmonic score over the
/// whole track. Returns `(bpm, phase_frames)` where `phase_frames` is the
/// position of a beat modulo the period.
pub fn refine(env: &[f32], fps: f64, bpm0: f64) -> (f64, f64) {
    let score = |bpm: f64| harmonic_score(env, 60.0 * fps / bpm).0;
    let search = |center: f64, span: f64, step: f64| {
        let mut best = (center, f64::MIN);
        let n = (span / step).round() as i64;
        for k in -n..=n {
            let bpm = center + k as f64 * step;
            let s = score(bpm);
            if s > best.1 {
                best = (bpm, s);
            }
        }
        best
    };
    let (b1, _) = search(bpm0, bpm0 * 0.02, 0.01);
    let (b2, _) = search(b1, 0.03, 0.002);
    let (mut bpm, s2) = search(b2, 0.002, 0.0002);
    // Producers almost always use integer (or half) tempos: snap when the
    // snapped value fits essentially as well.
    for cand in [bpm.round(), (bpm * 2.0).round() / 2.0] {
        if (cand - bpm).abs() < 0.03 && score(cand) >= s2 * 0.98 {
            bpm = cand;
            break;
        }
    }
    let (_, phase) = harmonic_score(env, 60.0 * fps / bpm);
    (bpm, phase)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pulse_train(bpm: f64, fps: f64, n: usize, phase: f64) -> Vec<f32> {
        let period = 60.0 * fps / bpm;
        (0..n)
            .map(|i| {
                let x = (i as f64 - phase).rem_euclid(period);
                let d = x.min(period - x);
                (-d * d / 2.0).exp() as f32
            })
            .collect()
    }

    #[test]
    fn finds_tempo_of_pulse_train() {
        let fps = 172.265625;
        for bpm in [122.0, 128.0, 140.0, 174.0] {
            let env = pulse_train(bpm, fps, 40_000, 17.3);
            let c = coarse_bpm(&env, fps, 86.0, 176.0).unwrap();
            assert!((c - bpm).abs() < 1.0, "coarse {c} vs {bpm}");
            let (r, phase) = refine(&env, fps, c);
            assert!((r - bpm).abs() < 0.01, "refined {r} vs {bpm}");
            let period = 60.0 * fps / bpm;
            let err = (phase - 17.3).rem_euclid(period);
            assert!(err.min(period - err) < 0.5, "phase {phase}");
        }
    }
}
