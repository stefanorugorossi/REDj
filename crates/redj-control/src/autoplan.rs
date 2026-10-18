//! Deterministic planner: a sensible set without any AI.
//!
//! Used when no model is available, as the live autopilot's default, and as
//! a starting point the AI can refine (`suggest_plan`).

use redj_core::{Library, TrackId, TrackInfo};
use redj_plan::{
    Entry, LoudnessPolicy, MixPlan, PLAN_VERSION, TempoChange, TempoPolicy, Transition,
    TransitionStyle, compile,
};

use crate::summary::{Compatibility, mix_in_range, mix_out_range};

/// Planner settings.
#[derive(Debug, Clone)]
pub struct AutoPlanOptions {
    /// Stop adding tracks after this many.
    pub max_tracks: Option<usize>,
    /// Stop adding tracks once the set is about this long.
    pub target_minutes: Option<f64>,
    /// Preferred transition length in bars.
    pub transition_bars: f64,
    /// First track (default: the slowest, calmest one).
    pub start_with: Option<TrackId>,
    pub title: Option<String>,
}

impl Default for AutoPlanOptions {
    fn default() -> Self {
        Self {
            max_tracks: None,
            target_minutes: None,
            transition_bars: 16.0,
            start_with: None,
            title: None,
        }
    }
}

fn mean_energy(t: &TrackInfo) -> f64 {
    if t.bar_energy.is_empty() {
        return 0.5;
    }
    t.bar_energy.iter().map(|&e| f64::from(e)).sum::<f64>() / t.bar_energy.len() as f64
}

/// Cost of playing `b` after `a` (lower is better).
fn cost(a: &TrackInfo, b: &TrackInfo) -> f64 {
    let c = Compatibility::new(a, b);
    let tempo = c.tempo_change_pct.abs();
    let key = c.key_distance.map(f64::from).unwrap_or(2.0);
    let mut cost = tempo / 2.0 + key * 0.6 + (-c.energy_delta).max(0.0) * 2.0;
    if tempo > 12.0 {
        cost += 20.0;
    }
    cost
}

/// Greedy running order: start calm and slow, then always the most
/// compatible remaining track, preferring rising energy.
pub fn order_tracks(lib: &Library, opts: &AutoPlanOptions) -> Vec<TrackId> {
    let mut left: Vec<&TrackInfo> = lib.iter().collect();
    if left.is_empty() {
        return Vec::new();
    }
    let first = opts
        .start_with
        .as_ref()
        .and_then(|id| left.iter().position(|t| &t.id == id))
        .unwrap_or_else(|| {
            (0..left.len())
                .min_by(|&i, &j| {
                    let k = |t: &TrackInfo| t.grid.bpm + mean_energy(t) * 4.0;
                    k(left[i]).total_cmp(&k(left[j]))
                })
                .unwrap_or(0)
        });
    let mut order = vec![left.remove(first)];
    let mut minutes = order[0].duration_sec / 60.0;
    while !left.is_empty() {
        if opts.max_tracks.is_some_and(|m| order.len() >= m)
            || opts.target_minutes.is_some_and(|m| minutes >= m)
        {
            break;
        }
        let last = order[order.len() - 1];
        let next = (0..left.len())
            .min_by(|&i, &j| cost(last, left[i]).total_cmp(&cost(last, left[j])))
            .expect("non-empty");
        let t = left.remove(next);
        // Overlaps shorten the set; count roughly 80% of each new track.
        minutes += t.duration_sec / 60.0 * 0.8;
        order.push(t);
    }
    order.into_iter().map(|t| t.id.clone()).collect()
}

/// Chooses where and how to mix from `out` (whose earliest allowed
/// transition bar is `min_bar`) into `inc`.
pub fn plan_transition(
    out: &TrackInfo,
    min_bar: f64,
    inc: &TrackInfo,
    index: usize,
    preferred_len: f64,
) -> (Transition, f64) {
    let end = out.end_bar().floor();
    let (o0, _) = mix_out_range(out);
    let (i0, _) = mix_in_range(inc);
    let ceil4 = |b: f64| (b / 4.0).ceil() * 4.0;
    let floor4 = |b: f64| (b / 4.0).floor() * 4.0;
    let c = Compatibility::new(out, inc);
    let tempo_far = c.tempo_change_pct.abs() > 6.0;
    let key_clash = c.key_distance.is_some_and(|d| d >= 3);

    let mut at = ceil4(o0.max(min_bar));
    let mut len = floor4((end - at).min(preferred_len));
    if len < 4.0 {
        at = ceil4(min_bar.max(end - preferred_len));
        len = floor4(end - at);
    }
    let (style, tempo, len) = if tempo_far || len < 4.0 {
        // Too far apart to beat-match comfortably: echo out on a downbeat
        // and switch tempo.
        at = floor4((end - 2.0).max(min_bar));
        (TransitionStyle::EchoOut, TempoChange::Incoming, 1.0)
    } else if key_clash {
        (TransitionStyle::Filter, TempoChange::Glide, len)
    } else {
        let s = match index % 3 {
            0 => TransitionStyle::BassSwap,
            1 => TransitionStyle::EqSwap,
            _ => TransitionStyle::Blend,
        };
        (s, TempoChange::Glide, len)
    };
    let rationale = format!(
        "{} at the outgoing outro (tempo {:+.1}%, key distance {})",
        match style {
            TransitionStyle::EchoOut => "echo out",
            TransitionStyle::Filter => "filter blend to hide the key clash",
            TransitionStyle::BassSwap => "bass swap",
            TransitionStyle::EqSwap => "EQ swap",
            TransitionStyle::Blend => "blend",
            TransitionStyle::Cut => "cut",
        },
        c.tempo_change_pct,
        c.key_distance
            .map(|d| d.to_string())
            .unwrap_or_else(|| "?".into())
    );
    (
        Transition {
            at_bar: at,
            length_bars: len,
            style,
            tempo,
            automation: Vec::new(),
            rationale: Some(rationale),
        },
        i0,
    )
}

/// Builds a valid plan for the given running order.
pub fn plan_for_order(lib: &Library, order: &[TrackId], opts: &AutoPlanOptions) -> MixPlan {
    let tracks: Vec<&TrackInfo> = order.iter().filter_map(|id| lib.get(id)).collect();
    let mut entries: Vec<Entry> = Vec::with_capacity(tracks.len());
    let mut prev_len_in = 0.0;
    for (i, t) in tracks.iter().enumerate() {
        let mut entry = Entry {
            track: t.id.clone(),
            start_bar: 0.0,
            end_bar: None,
            fade_out_bars: None,
            gain_db: None,
            loops: Vec::new(),
            transition: None,
            rationale: None,
        };
        if i > 0 {
            let out = tracks[i - 1];
            let out_start = entries[i - 1].start_bar;
            let (tr, start) =
                plan_transition(out, out_start + prev_len_in, t, i - 1, opts.transition_bars);
            entry.start_bar = start;
            prev_len_in = if tr.style == TransitionStyle::EchoOut {
                0.0
            } else {
                tr.length_bars
            };
            entry.transition = Some(tr);
        }
        entries.push(entry);
    }
    let mut plan = MixPlan {
        version: PLAN_VERSION,
        title: opts.title.clone(),
        tempo: TempoPolicy::Follow,
        keylock: true,
        loudness: LoudnessPolicy::default(),
        entries,
    };
    // Repair whatever the heuristics got wrong: fall back to an echo out on
    // the failing transition.
    for _ in 0..4 {
        let c = compile(&plan, lib);
        if c.is_ok() {
            break;
        }
        let mut changed = false;
        for issue in c.errors() {
            let Some(i) = issue
                .path
                .strip_prefix("/entries/")
                .and_then(|p| p.split('/').next())
                .and_then(|n| n.parse::<usize>().ok())
            else {
                continue;
            };
            if i == 0 || i >= plan.entries.len() {
                continue;
            }
            let out = tracks[i - 1];
            let prev_start = plan.entries[i - 1].start_bar;
            if let Some(tr) = plan.entries[i].transition.as_mut()
                && tr.style != TransitionStyle::EchoOut
            {
                tr.style = TransitionStyle::EchoOut;
                tr.tempo = TempoChange::Incoming;
                tr.length_bars = 1.0;
                tr.at_bar = (out.end_bar().floor() - 2.0).max(prev_start + 1.0);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    plan
}

/// Orders the library and plans the whole set.
pub fn auto_plan(lib: &Library, opts: &AutoPlanOptions) -> MixPlan {
    let order = order_tracks(lib, opts);
    plan_for_order(lib, &order, opts)
}
