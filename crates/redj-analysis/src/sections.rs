//! Per-bar energy and phrase-aligned section segmentation.

use redj_core::{BeatGrid, Section, SectionKind, bars_to_beats};

use crate::features::{Features, N_BANDS};

/// Per-bar measurements.
#[derive(Debug, Clone)]
pub struct BarFeatures {
    pub energy: Vec<f32>,
    pub kick: Vec<bool>,
    vectors: Vec<Vec<f64>>,
}

fn percentile(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    s[((s.len() - 1) as f64 * p).round() as usize]
}

fn peak_near(x: &[f32], f: &Features, t: f64) -> f64 {
    let c = f.time_to_frame(t).round();
    if c < 0.0 {
        return 0.0;
    }
    let c = c as usize;
    let a = c.saturating_sub(2).min(x.len());
    let b = (c + 3).min(x.len());
    x[a..b].iter().fold(0f32, |m, &v| m.max(v)).into()
}

/// Measures every bar from bar 0 to the end of the audio.
pub fn bar_features(f: &Features, grid: &BeatGrid, duration_sec: f64) -> BarFeatures {
    let end_beat = grid.sec_to_beat(duration_sec);
    let n_bars = (end_beat / 4.0).floor().max(0.0) as usize;
    let mut loud = Vec::with_capacity(n_bars);
    let mut density = Vec::with_capacity(n_bars);
    let mut kick_on = Vec::with_capacity(n_bars);
    let mut kick_ratio = Vec::with_capacity(n_bars);
    let mut bands = Vec::with_capacity(n_bars);
    for b in 0..n_bars {
        let beat0 = bars_to_beats(b as f64);
        let r = f.frame_range(grid.beat_to_sec(beat0), grid.beat_to_sec(beat0 + 4.0));
        if r.is_empty() {
            loud.push(-100.0);
            density.push(0.0);
            kick_on.push(0.0);
            kick_ratio.push(0.0);
            bands.push([-100.0f64; N_BANDS]);
            continue;
        }
        let n = r.len() as f64;
        let power: f64 = r
            .clone()
            .map(|i| 10f64.powf(f64::from(f.power_db[i]) / 10.0))
            .sum::<f64>()
            / n;
        loud.push(10.0 * (power + 1e-10).log10());
        density.push(r.clone().map(|i| f64::from(f.onset[i])).sum::<f64>() / n);
        let mut bm = [0f64; N_BANDS];
        for i in r {
            for (k, &v) in f.bands[i].iter().enumerate() {
                bm[k] += f64::from(v) / n;
            }
        }
        bands.push(bm);
        let mut on = 0.0;
        let mut off = 0.0;
        for q in 0..4 {
            let beat = beat0 + q as f64;
            on += peak_near(&f.low_onset, f, grid.beat_to_sec(beat));
            off += peak_near(&f.low_onset, f, grid.beat_to_sec(beat + 0.5));
        }
        kick_on.push(on / 4.0);
        kick_ratio.push(on / (off + 1e-6));
    }

    let on_ref = percentile(&kick_on, 0.9);
    let kick: Vec<bool> = (0..n_bars)
        .map(|b| kick_on[b] > 0.25 * on_ref && kick_ratio[b] > 1.5)
        .collect();

    let (l_lo, l_hi) = (percentile(&loud, 0.05), percentile(&loud, 0.95));
    let (d_lo, d_hi) = (percentile(&density, 0.05), percentile(&density, 0.95));
    let scale = |v: f64, lo: f64, hi: f64| ((v - lo) / (hi - lo).max(1e-9)).clamp(0.0, 1.0);
    let energy: Vec<f32> = (0..n_bars)
        .map(|b| {
            let e = 0.7 * scale(loud[b], l_lo, l_hi) + 0.3 * scale(density[b], d_lo, d_hi);
            e as f32
        })
        .collect();

    // Feature vectors for segmentation, z-normalized per dimension.
    let mut vectors: Vec<Vec<f64>> = (0..n_bars)
        .map(|b| {
            let mut v: Vec<f64> = bands[b].to_vec();
            v.push(density[b]);
            v
        })
        .collect();
    if n_bars > 1 {
        let dims = vectors[0].len();
        for d in 0..dims {
            let mean = vectors.iter().map(|v| v[d]).sum::<f64>() / n_bars as f64;
            let var = vectors.iter().map(|v| (v[d] - mean).powi(2)).sum::<f64>() / n_bars as f64;
            let sd = var.sqrt().max(1e-6);
            for v in &mut vectors {
                v[d] = (v[d] - mean) / sd;
            }
        }
    }
    for (v, &k) in vectors.iter_mut().zip(&kick) {
        v.push(if k { 2.0 } else { -2.0 });
    }
    BarFeatures {
        energy,
        kick,
        vectors,
    }
}

fn dist(a: &[f64], b: &[f64]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).powi(2))
        .sum::<f64>()
        .sqrt()
}

/// Checkerboard-kernel novelty at every bar boundary.
fn novelty(vectors: &[Vec<f64>], w: usize) -> Vec<f64> {
    let n = vectors.len();
    let mut nov = vec![0f64; n];
    for (b, out) in nov.iter_mut().enumerate().skip(1) {
        let l = b.saturating_sub(w)..b;
        let r = b..(b + w).min(n);
        let mut cross = 0.0;
        for i in l.clone() {
            for j in r.clone() {
                cross += dist(&vectors[i], &vectors[j]);
            }
        }
        cross /= (l.len() * r.len()) as f64;
        let within = |rg: std::ops::Range<usize>| {
            let mut s = 0.0;
            let mut c = 0;
            for i in rg.clone() {
                for j in rg.clone() {
                    if i < j {
                        s += dist(&vectors[i], &vectors[j]);
                        c += 1;
                    }
                }
            }
            if c == 0 { 0.0 } else { s / c as f64 }
        };
        *out = (cross - 0.5 * (within(l) + within(r))).max(0.0);
    }
    nov
}

/// Segments the track into labelled, phrase-aligned sections.
pub fn segment(bf: &BarFeatures) -> Vec<Section> {
    let n = bf.energy.len();
    if n == 0 {
        return Vec::new();
    }
    if n < 8 {
        return vec![Section {
            kind: SectionKind::Body,
            start_bar: 0.0,
            end_bar: n as f64,
            energy: mean(&bf.energy[..]),
        }];
    }
    let nov = novelty(&bf.vectors, 4);
    // Phrase offset: which bar (mod 8) the arrangement changes on.
    let off = (0..8)
        .max_by(|&a, &b| {
            let s = |o: usize| (o..n).step_by(8).map(|i| nov[i]).sum::<f64>();
            s(a).total_cmp(&s(b))
        })
        .unwrap_or(0);
    let m = nov.iter().sum::<f64>() / n as f64;
    let sd = (nov.iter().map(|v| (v - m).powi(2)).sum::<f64>() / n as f64).sqrt();
    let thr = m + 0.5 * sd;
    let mut cands: Vec<usize> = (1..n)
        .filter(|&b| b >= 4 && b + 4 <= n && (b + 8 - off % 4) % 4 == 0)
        .filter(|&b| nov[b] > thr)
        .filter(|&b| {
            (b.saturating_sub(3)..(b + 4).min(n))
                .all(|o| o == b || nov[o] <= nov[b] || (o + 8 - off % 4) % 4 != 0)
        })
        .collect();
    cands.sort_by(|&a, &b| nov[b].total_cmp(&nov[a]));
    let mut bounds: Vec<usize> = Vec::new();
    for c in cands {
        if bounds.iter().all(|&b| c.abs_diff(b) >= 8) {
            bounds.push(c);
        }
    }
    bounds.push(0);
    bounds.push(n);
    bounds.sort_unstable();
    bounds.dedup();

    let segs: Vec<(usize, usize)> = bounds.windows(2).map(|w| (w[0], w[1])).collect();
    let stats: Vec<(f32, f32, f32)> = segs
        .iter()
        .map(|&(a, b)| {
            let e = &bf.energy[a..b];
            let k = bf.kick[a..b].iter().filter(|&&k| k).count() as f32 / (b - a) as f32;
            let half = (b - a) / 2;
            let slope = if half > 0 {
                mean(&e[half..]) - mean(&e[..half])
            } else {
                0.0
            };
            (mean(e), k, slope)
        })
        .collect();
    let e_drop = stats
        .iter()
        .filter(|s| s.1 >= 0.5)
        .map(|s| s.0)
        .fold(0f32, f32::max);
    let high = |i: usize| stats[i].1 >= 0.5 && stats[i].0 >= e_drop - 0.15;
    let first_high = (0..segs.len()).find(|&i| high(i));
    let last_high = (0..segs.len()).rev().find(|&i| high(i));

    let kinds: Vec<SectionKind> = (0..segs.len())
        .map(|i| {
            let (e, k, slope) = stats[i];
            let _ = e;
            if i > 0 && k < 0.5 && i + 1 < segs.len() && high(i + 1) {
                let len = segs[i].1 - segs[i].0;
                if slope > 0.05 || len <= 8 {
                    return SectionKind::Buildup;
                }
            }
            if first_high.is_none_or(|f| i < f) && !high(i) {
                return SectionKind::Intro;
            }
            if last_high.is_none_or(|l| i > l) && !high(i) {
                return SectionKind::Outro;
            }
            if high(i) {
                return SectionKind::Drop;
            }
            if k < 0.5 {
                let next_is_drop = i + 1 < segs.len() && high(i + 1);
                let len = segs[i].1 - segs[i].0;
                if next_is_drop && (slope > 0.05 || len <= 8) {
                    return SectionKind::Buildup;
                }
                return SectionKind::Breakdown;
            }
            SectionKind::Body
        })
        .collect();

    let mut out: Vec<Section> = Vec::new();
    for (i, &(a, b)) in segs.iter().enumerate() {
        if let Some(last) = out.last_mut()
            && last.kind == kinds[i]
        {
            last.end_bar = b as f64;
            continue;
        }
        out.push(Section {
            kind: kinds[i],
            start_bar: a as f64,
            end_bar: b as f64,
            energy: 0.0,
        });
    }
    for s in &mut out {
        s.energy = mean(&bf.energy[s.start_bar as usize..s.end_bar as usize]);
    }
    out
}

fn mean(v: &[f32]) -> f32 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f32>() / v.len() as f32
    }
}
