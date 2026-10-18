//! Plan validation and compilation into a [`Timeline`].
//!
//! Validation and compilation are one pass: every check is made where the
//! compiler needs the value, so the two can never disagree. Issues carry a
//! JSON pointer into the plan, a message and, where possible, a concrete
//! fix, so a language model can repair its own plan.

use redj_core::{Library, TrackInfo, bars_to_beats, beats_to_bars};
use serde::Serialize;

use crate::model::*;
use crate::timeline::*;

/// Severity of a plan issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The plan cannot be played.
    Error,
    /// The plan plays but probably not as intended.
    Warning,
}

/// One problem found in a plan.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Issue {
    pub severity: Severity,
    /// JSON pointer to the offending field, e.g. `/entries/2/transition/at_bar`.
    pub path: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sev = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        write!(f, "{sev} at {}: {}", self.path, self.message)?;
        if let Some(h) = &self.hint {
            write!(f, " (hint: {h})")?;
        }
        Ok(())
    }
}

/// Result of compiling a plan.
#[derive(Debug, Clone)]
pub struct Compiled {
    /// `None` when there are errors.
    pub timeline: Option<Timeline>,
    pub issues: Vec<Issue>,
}

impl Compiled {
    pub fn errors(&self) -> impl Iterator<Item = &Issue> {
        self.issues.iter().filter(|i| i.severity == Severity::Error)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &Issue> {
        self.issues
            .iter()
            .filter(|i| i.severity == Severity::Warning)
    }

    pub fn is_ok(&self) -> bool {
        self.timeline.is_some()
    }
}

/// Echo settings used by the echo send.
const ECHO_BEATS: f64 = 0.75;
const ECHO_FEEDBACK: f64 = 0.55;
/// Beats an echo tail is kept alive after the clip falls silent.
const ECHO_TAIL_BEATS: f64 = 16.0;
/// Playback rate deviations above these get a warning / an error.
const RATE_WARN: f64 = 0.06;
const RATE_ERROR: f64 = 0.16;

#[derive(Default)]
struct Issues(Vec<Issue>);

impl Issues {
    fn err(&mut self, path: impl Into<String>, msg: impl Into<String>, hint: Option<String>) {
        self.0.push(Issue {
            severity: Severity::Error,
            path: path.into(),
            message: msg.into(),
            hint,
        });
    }

    fn warn(&mut self, path: impl Into<String>, msg: impl Into<String>, hint: Option<String>) {
        self.0.push(Issue {
            severity: Severity::Warning,
            path: path.into(),
            message: msg.into(),
            hint,
        });
    }

    fn has_errors(&self) -> bool {
        self.0.iter().any(|i| i.severity == Severity::Error)
    }
}

/// Playhead map of one entry: master beats to track beats, loops expanded.
#[derive(Debug, Clone)]
struct EntryMap {
    segs: Vec<PlayheadSeg>,
}

impl EntryMap {
    fn build(master_start: f64, start_beat: f64, loops: &[Loop], track_end_beat: f64) -> Self {
        let mut segs = Vec::new();
        let mut m = master_start;
        let mut tb = start_beat;
        let mut sorted: Vec<&Loop> = loops.iter().collect();
        sorted.sort_by(|a, b| a.at_bar.total_cmp(&b.at_bar));
        for l in sorted {
            let at = bars_to_beats(l.at_bar);
            let len = bars_to_beats(l.length_bars);
            if at < tb || len <= 0.0 {
                continue;
            }
            let end = at + len;
            segs.push(PlayheadSeg {
                master_start: m,
                master_end: m + (end - tb),
                track_start: tb,
            });
            m += end - tb;
            for _ in 0..l.repeats {
                segs.push(PlayheadSeg {
                    master_start: m,
                    master_end: m + len,
                    track_start: at,
                });
                m += len;
            }
            tb = end;
        }
        let tail = (track_end_beat - tb).max(0.0);
        segs.push(PlayheadSeg {
            master_start: m,
            master_end: m + tail,
            track_start: tb,
        });
        Self { segs }
    }

    /// Master beat at which the playhead reaches `track_beat` for the last
    /// time (after any loops around it), or `None` past the end.
    fn track_to_master(&self, track_beat: f64) -> Option<f64> {
        self.segs.iter().rev().find_map(|s| {
            let len = s.master_end - s.master_start;
            (track_beat >= s.track_start && track_beat <= s.track_start + len)
                .then_some(s.master_start + (track_beat - s.track_start))
        })
    }

    fn master_to_track(&self, master: f64) -> Option<f64> {
        self.segs.iter().find_map(|s| {
            (master >= s.master_start && master <= s.master_end)
                .then_some(s.track_start + (master - s.master_start))
        })
    }

    fn end_master(&self) -> f64 {
        self.segs.last().map(|s| s.master_end).unwrap_or(0.0)
    }
}

/// Automation points generated by a transition style, in beats relative to
/// the transition start: `(side, param, points)`.
type StyleLane = (Side, Param, Vec<(f64, f64, Curve)>);

fn style_lanes(style: TransitionStyle, len: f64) -> Vec<StyleLane> {
    use Curve::*;
    use Param::*;
    use Side::*;
    const KILL: f64 = -40.0;
    // Swap points land on bar lines.
    let bar_floor = |b: f64| (b / 4.0).floor() * 4.0;
    let mid = bar_floor(len / 2.0).max(0.0);
    let q1 = bar_floor(len / 4.0);
    let q3 = bar_floor(len * 0.75).max(mid);
    match style {
        TransitionStyle::Blend => vec![
            (
                Incoming,
                Volume,
                vec![(0.0, 0.0, Linear), (mid.max(1.0), 1.0, Smooth)],
            ),
            (
                Outgoing,
                Volume,
                vec![(mid, 1.0, Linear), (len, 0.0, Smooth)],
            ),
            (Incoming, EqLow, vec![(0.0, KILL, Linear), (mid, 0.0, Step)]),
            (Outgoing, EqLow, vec![(0.0, 0.0, Linear), (mid, KILL, Step)]),
        ],
        TransitionStyle::BassSwap => vec![
            (
                Incoming,
                Volume,
                vec![(0.0, 0.0, Linear), (q1.max(1.0), 1.0, Smooth)],
            ),
            (
                Outgoing,
                Volume,
                vec![(q3, 1.0, Linear), (len, 0.0, Smooth)],
            ),
            (Incoming, EqLow, vec![(0.0, KILL, Linear), (mid, 0.0, Step)]),
            (Outgoing, EqLow, vec![(0.0, 0.0, Linear), (mid, KILL, Step)]),
        ],
        TransitionStyle::EqSwap => vec![
            (
                Incoming,
                Volume,
                vec![(0.0, 0.0, Linear), (q1.max(1.0), 1.0, Smooth)],
            ),
            (Incoming, EqLow, vec![(0.0, KILL, Linear), (q3, 0.0, Step)]),
            (
                Incoming,
                EqMid,
                vec![(q1, KILL, Linear), (mid, 0.0, Smooth)],
            ),
            (
                Incoming,
                EqHigh,
                vec![(0.0, -12.0, Linear), (q1.max(1.0), 0.0, Smooth)],
            ),
            (
                Outgoing,
                EqMid,
                vec![(mid, 0.0, Linear), (q3, KILL, Smooth)],
            ),
            (Outgoing, EqLow, vec![(0.0, 0.0, Linear), (q3, KILL, Step)]),
            (
                Outgoing,
                EqHigh,
                vec![(q3, 0.0, Linear), (len, KILL, Smooth)],
            ),
            (
                Outgoing,
                Volume,
                vec![(q3, 1.0, Linear), (len, 0.0, Linear)],
            ),
        ],
        TransitionStyle::Filter => vec![
            (
                Incoming,
                Volume,
                vec![(0.0, 0.0, Linear), (mid.max(1.0), 1.0, Smooth)],
            ),
            (
                Incoming,
                Filter,
                vec![(0.0, -0.75, Linear), (len, 0.0, Smooth)],
            ),
            (
                Outgoing,
                Filter,
                vec![(0.0, 0.0, Linear), (len, 0.7, Exponential)],
            ),
            (
                Outgoing,
                Volume,
                vec![(mid, 1.0, Linear), (len, 0.0, Smooth)],
            ),
            (Incoming, EqLow, vec![(0.0, KILL, Linear), (mid, 0.0, Step)]),
            (Outgoing, EqLow, vec![(0.0, 0.0, Linear), (mid, KILL, Step)]),
        ],
        TransitionStyle::EchoOut => vec![
            (
                Outgoing,
                Echo,
                vec![(0.0, 0.0, Linear), (len, 0.8, Smooth), (len, 0.0, Step)],
            ),
            (Outgoing, Volume, vec![(0.0, 1.0, Linear), (len, 0.0, Step)]),
        ],
        TransitionStyle::Cut => {
            vec![(Outgoing, Volume, vec![(0.0, 1.0, Linear), (0.0, 0.0, Step)])]
        }
    }
}

fn entry_path(i: usize) -> String {
    format!("/entries/{i}")
}

/// Validates a plan against a library and compiles it.
pub fn compile(plan: &MixPlan, lib: &Library) -> Compiled {
    let mut iss = Issues::default();
    if plan.version != PLAN_VERSION {
        iss.err(
            "/version",
            format!("unsupported plan version {}", plan.version),
            Some("use \"version\": 1".into()),
        );
    }
    if plan.entries.is_empty() {
        iss.err(
            "/entries",
            "the plan has no entries",
            Some("add at least one track".into()),
        );
        return Compiled {
            timeline: None,
            issues: iss.0,
        };
    }
    if let TempoPolicy::Fixed { bpm } = plan.tempo
        && !(40.0..=250.0).contains(&bpm)
    {
        iss.err(
            "/tempo/bpm",
            format!("fixed tempo {bpm} BPM is out of range"),
            Some("use 40..250".into()),
        );
    }
    if !(-30.0..=0.0).contains(&plan.loudness.target_lufs) {
        iss.err(
            "/loudness/target_lufs",
            format!(
                "target loudness {} LUFS is out of range",
                plan.loudness.target_lufs
            ),
            Some("use a value between -30 and 0, typically -9".into()),
        );
    }

    // Resolve tracks first; nothing else can be checked without them.
    let tracks: Vec<Option<&TrackInfo>> = plan
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let t = lib.resolve(&e.track.0);
            if t.is_none() {
                iss.err(
                    format!("{}/track", entry_path(i)),
                    format!("unknown track id {:?}", e.track.0),
                    Some("use an id from the library listing".into()),
                );
            }
            t
        })
        .collect();
    if iss.has_errors() {
        return Compiled {
            timeline: None,
            issues: iss.0,
        };
    }
    let tracks: Vec<&TrackInfo> = tracks.into_iter().map(Option::unwrap).collect();
    let n = plan.entries.len();

    // Per-entry static checks.
    for (i, (e, t)) in plan.entries.iter().zip(&tracks).enumerate() {
        let p = entry_path(i);
        let end_bar = t.end_bar();
        let first_bar = beats_to_bars(t.grid.sec_to_beat(0.0)).floor();
        if !e.start_bar.is_finite() || e.start_bar < first_bar || e.start_bar >= end_bar - 1.0 {
            iss.err(
                format!("{p}/start_bar"),
                format!(
                    "start_bar {} is outside the track (bars {first_bar}..{end_bar:.1})",
                    e.start_bar
                ),
                Some(format!(
                    "use a bar between {first_bar} and {:.0}",
                    (end_bar - 8.0).max(first_bar)
                )),
            );
        }
        if i == 0 && e.transition.is_some() {
            iss.warn(
                format!("{p}/transition"),
                "the first entry's transition is ignored",
                None,
            );
        }
        if i > 0 && e.transition.is_none() {
            iss.err(
                format!("{p}/transition"),
                "every entry after the first needs a transition",
                Some("add e.g. {\"at_bar\": <bar of the previous track>, \"length_bars\": 16, \"style\": \"blend\"}".into()),
            );
        }
        if i + 1 < n && (e.end_bar.is_some() || e.fade_out_bars.is_some()) {
            iss.warn(
                format!("{p}/end_bar"),
                "end_bar/fade_out_bars only apply to the last entry; the next transition decides where this track ends",
                None,
            );
        }
        if let Some(eb) = e.end_bar
            && i + 1 == n
            && (eb <= e.start_bar || eb > end_bar + 1e-6)
        {
            iss.err(
                format!("{p}/end_bar"),
                format!("end_bar {eb} must be after start_bar {} and within the track (ends at bar {end_bar:.1})", e.start_bar),
                None,
            );
        }
        if let Some(g) = e.gain_db
            && !(-24.0..=12.0).contains(&g)
        {
            iss.err(
                format!("{p}/gain_db"),
                format!("gain {g} dB is out of range"),
                Some("use -24..12".into()),
            );
        }
        let mut sorted: Vec<(usize, &Loop)> = e.loops.iter().enumerate().collect();
        sorted.sort_by(|a, b| a.1.at_bar.total_cmp(&b.1.at_bar));
        let mut prev_end = f64::MIN;
        for (k, l) in sorted {
            let lp = format!("{p}/loops/{k}");
            if l.length_bars <= 0.0 || l.length_bars > 64.0 {
                iss.err(
                    format!("{lp}/length_bars"),
                    "loop length must be in (0, 64] bars",
                    None,
                );
            } else if ![0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0].contains(&l.length_bars) {
                iss.warn(
                    format!("{lp}/length_bars"),
                    "loop length is not a power of two; it may sound unnatural",
                    None,
                );
            }
            if l.repeats == 0 {
                iss.warn(format!("{lp}/repeats"), "repeats = 0 does nothing", None);
            }
            if l.at_bar < e.start_bar {
                iss.err(
                    format!("{lp}/at_bar"),
                    format!(
                        "loop at bar {} starts before the entry's start_bar {}",
                        l.at_bar, e.start_bar
                    ),
                    None,
                );
            }
            if l.at_bar + l.length_bars > end_bar + 1e-6 {
                iss.err(
                    format!("{lp}/at_bar"),
                    format!("loop runs past the end of the track (bar {end_bar:.1})"),
                    None,
                );
            }
            if l.at_bar < prev_end {
                iss.err(
                    format!("{lp}/at_bar"),
                    "loops overlap",
                    Some("loops of one entry must not overlap".into()),
                );
            }
            prev_end = l.at_bar + l.length_bars;
            if l.at_bar.fract() != 0.0 {
                iss.warn(
                    format!("{lp}/at_bar"),
                    "loop does not start on a downbeat",
                    None,
                );
            }
        }
        if let Some(tr) = &e.transition {
            let tp = format!("{p}/transition");
            if tr.style != TransitionStyle::Cut && !(tr.length_bars > 0.0 && tr.length_bars <= 64.0)
            {
                iss.err(
                    format!("{tp}/length_bars"),
                    format!("length_bars {} must be in (0, 64]", tr.length_bars),
                    None,
                );
            }
            if tr.at_bar.fract() != 0.0 {
                iss.warn(
                    format!("{tp}/at_bar"),
                    "transition does not start on a downbeat",
                    Some("use a whole bar number".into()),
                );
            }
            for (k, lane) in tr.automation.iter().enumerate() {
                let ap = format!("{tp}/automation/{k}");
                let (lo, hi) = lane.param.range();
                if lane.points.is_empty() {
                    iss.err(
                        format!("{ap}/points"),
                        "automation lane has no points",
                        None,
                    );
                }
                for (j, pt) in lane.points.iter().enumerate() {
                    if pt.value < lo || pt.value > hi {
                        iss.err(
                            format!("{ap}/points/{j}/value"),
                            format!("{:?} value {} is outside {lo}..{hi}", lane.param, pt.value),
                            None,
                        );
                    }
                    if j > 0 && pt.bar < lane.points[j - 1].bar {
                        iss.err(
                            format!("{ap}/points/{j}/bar"),
                            "points must be in time order",
                            None,
                        );
                    }
                }
            }
        }
    }
    if iss.has_errors() {
        return Compiled {
            timeline: None,
            issues: iss.0,
        };
    }

    // Lay the entries out on the master timeline.
    struct Placed {
        map: EntryMap,
        /// Transition into this entry: (start, length) in master beats.
        t_start: f64,
        t_len: f64,
        /// Master beat where the incoming audio starts.
        audio_start: f64,
    }
    let mut placed: Vec<Placed> = Vec::with_capacity(n);
    for (i, (e, t)) in plan.entries.iter().zip(&tracks).enumerate() {
        let track_end = bars_to_beats(t.end_bar());
        let start_beat = bars_to_beats(e.start_bar);
        if i == 0 {
            placed.push(Placed {
                map: EntryMap::build(0.0, start_beat, &e.loops, track_end),
                t_start: 0.0,
                t_len: 0.0,
                audio_start: 0.0,
            });
            continue;
        }
        let tr = e.transition.as_ref().expect("checked above");
        let tp = format!("{}/transition", entry_path(i));
        let prev = &placed[i - 1];
        let prev_track = tracks[i - 1];
        let len = if tr.style == TransitionStyle::Cut {
            0.0
        } else {
            bars_to_beats(tr.length_bars)
        };
        let Some(t_start) = prev.map.track_to_master(bars_to_beats(tr.at_bar)) else {
            iss.err(
                format!("{tp}/at_bar"),
                format!(
                    "at_bar {} is not reached by the previous track (it plays bars {}..{:.1})",
                    tr.at_bar,
                    plan.entries[i - 1].start_bar,
                    prev_track.end_bar()
                ),
                None,
            );
            break;
        };
        let prev_free = prev.t_start + prev.t_len;
        if t_start < prev_free - 1e-9 {
            let min_bar = prev
                .map
                .master_to_track(prev_free)
                .map(|b| beats_to_bars(b).ceil())
                .unwrap_or(0.0);
            iss.err(
                format!("{tp}/at_bar"),
                "this transition starts before the transition into the previous track has finished"
                    .to_string(),
                Some(format!("use at_bar >= {min_bar}")),
            );
        }
        let out_end_master = t_start + len;
        if out_end_master > prev.map.end_master() + 1e-6 {
            let max_bar = prev
                .map
                .master_to_track(prev.map.end_master() - len)
                .map(|b| beats_to_bars(b).floor())
                .unwrap_or(0.0);
            iss.err(
                format!("{tp}/at_bar"),
                format!(
                    "the previous track ends (bar {:.1}) before this {}-bar transition is over",
                    prev_track.end_bar(),
                    tr.length_bars
                ),
                Some(format!(
                    "use at_bar <= {max_bar}, a shorter transition, or a loop on the previous track"
                )),
            );
        }
        let audio_start = match tr.style {
            TransitionStyle::EchoOut => t_start + len,
            _ => t_start,
        };
        let map = EntryMap::build(audio_start, start_beat, &e.loops, track_end);
        if map.end_master() < out_end_master + 4.0 {
            iss.err(
                format!("{}/start_bar", entry_path(i)),
                "the incoming track ends before the transition is over",
                Some("start the incoming track earlier".into()),
            );
        }
        placed.push(Placed {
            map,
            t_start,
            t_len: len,
            audio_start,
        });
    }
    if iss.has_errors() {
        return Compiled {
            timeline: None,
            issues: iss.0,
        };
    }

    // End of the set.
    let last = n - 1;
    let last_entry = &plan.entries[last];
    let set_end = match last_entry.end_bar {
        Some(eb) => placed[last]
            .map
            .track_to_master(bars_to_beats(eb))
            .unwrap_or_else(|| placed[last].map.end_master()),
        None => placed[last].map.end_master(),
    };
    let fade_out = bars_to_beats(last_entry.fade_out_bars.unwrap_or(
        if last_entry.end_bar.is_some() {
            4.0
        } else {
            0.0
        },
    ));

    // Tempo map.
    let tempo = match plan.tempo {
        TempoPolicy::Fixed { bpm } => TempoMap::constant(bpm),
        TempoPolicy::Follow => {
            let mut parts = Vec::new();
            let mut cursor = 0.0;
            let mut bpm = tracks[0].grid.bpm;
            for i in 1..n {
                let (ts, tl) = (placed[i].t_start, placed[i].t_len);
                let next = tracks[i].grid.bpm;
                let change = plan.entries[i]
                    .transition
                    .as_ref()
                    .map(|t| t.tempo)
                    .unwrap_or_default();
                let (hold_until, ramp_end) = match change {
                    TempoChange::Glide => (ts, ts + tl),
                    TempoChange::Incoming => (ts, ts),
                    TempoChange::Outgoing => (ts + tl, ts + tl),
                };
                parts.push((cursor, hold_until, bpm, bpm));
                parts.push((hold_until, ramp_end, bpm, next));
                cursor = ramp_end;
                bpm = next;
            }
            parts.push((cursor, set_end.max(cursor) + 1.0, bpm, bpm));
            TempoMap::from_segments(&parts)
        }
    };

    // Clips with automation.
    let mut clips = Vec::with_capacity(n);
    let mut markers = Vec::with_capacity(n);
    for i in 0..n {
        let e = &plan.entries[i];
        let t = tracks[i];
        let p = &placed[i];
        let mut lanes: Vec<Lane> = Param::ALL
            .iter()
            .map(|q| Lane::new(q.default_value()))
            .collect();
        let mut add = |side_is_in: bool, tr: &Transition, base: f64, len: f64| {
            let side = if side_is_in {
                Side::Incoming
            } else {
                Side::Outgoing
            };
            let custom: Vec<&LaneSpec> = tr.automation.iter().filter(|l| l.side == side).collect();
            for (s, param, pts) in style_lanes(tr.style, len) {
                if s != side || custom.iter().any(|c| c.param == param) {
                    continue;
                }
                lanes[param.index()]
                    .points
                    .extend(pts.iter().map(|&(b, v, c)| LanePoint {
                        beat: base + b,
                        value: v,
                        curve: c,
                    }));
            }
            for c in custom {
                lanes[c.param.index()]
                    .points
                    .extend(c.points.iter().map(|pt| LanePoint {
                        beat: base + bars_to_beats(pt.bar),
                        value: pt.value,
                        curve: pt.curve,
                    }));
            }
        };
        if i > 0 {
            let tr = e.transition.as_ref().expect("checked");
            // The incoming side's automation is relative to the transition
            // start, like the outgoing side's.
            add(true, tr, p.t_start, p.t_len);
        }
        let mut end = set_end;
        let mut tail = 0.0;
        if i + 1 < n {
            let next = &placed[i + 1];
            let tr = plan.entries[i + 1].transition.as_ref().expect("checked");
            add(false, tr, next.t_start, next.t_len);
            end = next.t_start + next.t_len;
            if tr.style == TransitionStyle::EchoOut {
                tail = ECHO_TAIL_BEATS;
            }
        } else if fade_out > 0.0 {
            lanes[Param::Volume.index()].points.extend([
                LanePoint {
                    beat: set_end - fade_out,
                    value: 1.0,
                    curve: Curve::Linear,
                },
                LanePoint {
                    beat: set_end,
                    value: 0.0,
                    curve: Curve::Smooth,
                },
            ]);
        }
        for lane in &mut lanes {
            // Stable sort keeps same-beat points in insertion order (steps).
            lane.points.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        }

        // Playback rate check over the clip's audible span.
        let bpm_track = t.grid.bpm;
        let mut worst: f64 = 1.0;
        let mut b = p.audio_start;
        while b <= end {
            let r = tempo.bpm_at(b) / bpm_track;
            if (r - 1.0).abs() > (worst - 1.0).abs() {
                worst = r;
            }
            b += 4.0;
        }
        let dev = (worst - 1.0).abs();
        let path = if i > 0 {
            format!("{}/transition/tempo", entry_path(i))
        } else {
            "/tempo".to_owned()
        };
        if dev > RATE_ERROR {
            iss.err(
                path,
                format!(
                    "{} (at {:.1} BPM) would play at {:.0}% of its speed",
                    t.display_name(),
                    bpm_track,
                    worst * 100.0
                ),
                Some("pick tracks with closer tempos, or use a cut/echo_out transition with tempo \"incoming\"".into()),
            );
        } else if dev > RATE_WARN {
            iss.warn(
                path,
                format!(
                    "{} plays at {:.1}% of its speed{}",
                    t.display_name(),
                    worst * 100.0,
                    if plan.keylock {
                        "; time-stretch artifacts may be audible"
                    } else {
                        " and its pitch shifts"
                    }
                ),
                None,
            );
        }

        let trim_db = e.gain_db.unwrap_or(if plan.loudness.auto_gain {
            (plan.loudness.target_lufs - t.loudness.integrated_lufs).clamp(-12.0, 6.0)
        } else {
            0.0
        });
        clips.push(Clip {
            entry: i,
            track: t.id.clone(),
            slot: i % 4,
            start_beat: p.audio_start,
            end_beat: end + tail,
            playhead: p.map.segs.clone(),
            trim_db,
            lanes,
            echo_beats: ECHO_BEATS,
            echo_feedback: ECHO_FEEDBACK,
        });
        let style = e.transition.as_ref().filter(|_| i > 0).map(|t| t.style);
        let main_beat = match style {
            None => 0.0,
            Some(TransitionStyle::EchoOut) | Some(TransitionStyle::Cut) => p.audio_start,
            Some(_) => p.t_start + p.t_len / 2.0,
        };
        markers.push(Marker {
            entry: i,
            track: t.id.clone(),
            transition_start_beat: p.t_start,
            main_beat,
            style,
        });
    }
    if iss.has_errors() {
        return Compiled {
            timeline: None,
            issues: iss.0,
        };
    }
    let end_beat = clips.iter().map(|c| c.end_beat).fold(set_end, f64::max);
    Compiled {
        timeline: Some(Timeline {
            tempo,
            clips,
            markers,
            end_beat,
            keylock: plan.keylock,
        }),
        issues: iss.0,
    }
}
