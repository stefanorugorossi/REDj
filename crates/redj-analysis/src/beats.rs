//! Beat tracking for tracks whose tempo is not constant.

use crate::tempo::fold;

/// Fraction of 32-beat windows whose local pulse phase deviates from a
/// constant grid of `period` frames by more than 10% of a beat. Windows
/// without a clear pulse (breakdowns) are ignored. Values above ~0.25 mean
/// the constant grid does not fit the track.
///
/// Pass the kick-band envelope: in builds and breaks, snare rolls and
/// off-beat hats can locally out-weigh the beat in the full-band envelope.
pub fn grid_misfit(env: &[f32], period: f64) -> f64 {
    let win = (32.0 * period) as usize;
    if win == 0 || env.len() < 2 * win {
        return 0.0;
    }
    let bins = (period.round() as usize * 2).max(16);
    let (_, phase) = fold(env, period, bins);
    let energy: Vec<f64> = env
        .chunks(win)
        .map(|c| c.iter().map(|&v| f64::from(v)).sum::<f64>() / c.len() as f64)
        .collect();
    let mut sorted = energy.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    let (mut valid, mut bad) = (0usize, 0usize);
    for (w, chunk) in env.chunks(win).enumerate() {
        if chunk.len() < win / 2 || energy[w] < 0.3 * median {
            continue;
        }
        let (sharp, local) = fold(chunk, period, bins);
        if sharp < 2.0 {
            continue; // no clear pulse in this window
        }
        valid += 1;
        let start = (w * win) as f64;
        let expected = (phase - start).rem_euclid(period);
        let d = (local - expected).rem_euclid(period);
        if d.min(period - d) / period > 0.1 {
            bad += 1;
        }
    }
    if valid == 0 {
        0.0
    } else {
        bad as f64 / valid as f64
    }
}

/// Dynamic-programming beat tracker (Ellis 2007). Returns beat positions in
/// frames. `period` is the expected beat period in frames.
pub fn track(env: &[f32], period: f64) -> Vec<f64> {
    let n = env.len();
    if n == 0 || period < 2.0 {
        return Vec::new();
    }
    let tightness = 100.0;
    let lo = (period / 2.0).round().max(1.0) as usize;
    let hi = (period * 2.0).round() as usize;
    let mut score = vec![0f64; n];
    let mut back = vec![usize::MAX; n];
    for t in 0..n {
        let mut best = 0.0;
        let mut arg = usize::MAX;
        if t >= lo {
            let first = t.saturating_sub(hi);
            for (tau, &prev) in score.iter().enumerate().take(t - lo + 1).skip(first) {
                let r = ((t - tau) as f64 / period).ln();
                let s = prev - tightness * r * r;
                if arg == usize::MAX || s > best {
                    best = s;
                    arg = tau;
                }
            }
        }
        score[t] = f64::from(env[t])
            + if arg == usize::MAX {
                0.0
            } else {
                best.max(0.0)
            };
        back[t] = if best > 0.0 { arg } else { usize::MAX };
    }
    let tail = n.saturating_sub(period.ceil() as usize);
    let mut t = (tail..n)
        .max_by(|&a, &b| score[a].total_cmp(&score[b]))
        .unwrap_or(n - 1);
    let mut beats = vec![t as f64];
    while back[t] != usize::MAX {
        t = back[t];
        beats.push(t as f64);
    }
    beats.reverse();
    beats
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_accelerating_pulses() {
        // Period glides from 80 to 70 frames.
        let mut env = vec![0f32; 30_000];
        let mut pos: f64 = 50.0;
        let mut truth = Vec::new();
        while pos < 29_900.0 {
            env[pos as usize] = 1.0;
            truth.push(pos.floor());
            let p = 80.0 - 10.0 * pos / 30_000.0;
            pos += p;
        }
        assert!(grid_misfit(&env, 75.0) > 0.25);
        let beats = track(&env, 75.0);
        let hits = truth
            .iter()
            .filter(|&&t| beats.iter().any(|&b| (b - t).abs() <= 1.0))
            .count();
        assert!(
            hits as f64 > truth.len() as f64 * 0.95,
            "{hits}/{}",
            truth.len()
        );
    }

    #[test]
    fn constant_pulses_have_no_deviation() {
        let mut env = vec![0f32; 30_000];
        let mut pos: f64 = 20.0;
        while pos < 29_990.0 {
            env[pos.round() as usize] = 1.0;
            pos += 80.0;
        }
        assert!(grid_misfit(&env, 80.0) < 0.01);
    }
}
