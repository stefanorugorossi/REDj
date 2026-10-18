use redj_core::{BeatGrid, Library, Loudness, TrackId, TrackInfo, TrackTags};
use redj_plan::*;

fn track(id: &str, bpm: f64, bars: f64) -> TrackInfo {
    TrackInfo {
        id: TrackId(id.into()),
        path: format!("{id}.wav").into(),
        tags: TrackTags::default(),
        duration_sec: bars * 240.0 / bpm,
        sample_rate: 44_100,
        channels: 2,
        grid: BeatGrid::constant(bpm, 0.0),
        sections: vec![],
        bar_energy: vec![],
        loudness: Loudness {
            integrated_lufs: -8.0,
            true_peak_dbtp: -0.5,
            range_lu: 5.0,
        },
        key: None,
        analyzer_version: 1,
    }
}

fn library() -> Library {
    [
        track("aaaa000000000001", 124.0, 128.0),
        track("bbbb000000000002", 128.0, 128.0),
        track("cccc000000000003", 128.0, 160.0),
        track("dddd000000000004", 150.0, 128.0),
    ]
    .into_iter()
    .collect()
}

fn entry(id: &str, start_bar: f64, transition: Option<Transition>) -> Entry {
    Entry {
        track: TrackId(id.into()),
        start_bar,
        end_bar: None,
        fade_out_bars: None,
        gain_db: None,
        loops: vec![],
        transition,
        rationale: None,
    }
}

fn tr(at_bar: f64, length_bars: f64, style: TransitionStyle) -> Option<Transition> {
    Some(Transition {
        at_bar,
        length_bars,
        style,
        tempo: TempoChange::Glide,
        automation: vec![],
        rationale: None,
    })
}

fn plan(entries: Vec<Entry>) -> MixPlan {
    MixPlan {
        version: 1,
        title: None,
        tempo: TempoPolicy::Follow,
        keylock: true,
        loudness: LoudnessPolicy::default(),
        entries,
    }
}

#[test]
fn two_track_blend() {
    let p = plan(vec![
        entry("aaaa", 0.0, None),
        entry("bbbb", 0.0, tr(96.0, 16.0, TransitionStyle::Blend)),
    ]);
    let c = compile(&p, &library());
    assert!(c.is_ok(), "{:?}", c.issues);
    let tl = c.timeline.unwrap();
    assert_eq!(tl.clips.len(), 2);
    let (a, b) = (&tl.clips[0], &tl.clips[1]);
    assert_eq!(a.end_beat, 384.0 + 64.0);
    assert_eq!(b.start_beat, 384.0);
    assert_eq!(b.track_beat(384.0), Some(0.0));
    assert_eq!(b.track_beat(400.0), Some(16.0));
    // Mid transition: bass swapped, incoming at full volume.
    let mid = 384.0 + 32.0;
    assert_eq!(b.lane(Param::Volume).eval(mid), 1.0);
    assert_eq!(b.lane(Param::EqLow).eval(mid - 0.01), -40.0);
    assert_eq!(b.lane(Param::EqLow).eval(mid), 0.0);
    assert_eq!(a.lane(Param::EqLow).eval(mid), -40.0);
    assert_eq!(a.lane(Param::Volume).eval(384.0 + 64.0), 0.0);
    assert_eq!(a.lane(Param::Volume).eval(100.0), 1.0);
    // Tempo glides from 124 to 128 over the transition.
    assert_eq!(tl.tempo.bpm_at(100.0), 124.0);
    assert!((tl.tempo.bpm_at(mid) - 126.0).abs() < 1e-9);
    assert_eq!(tl.tempo.bpm_at(500.0), 128.0);
    // Auto gain: -9 target vs -8 measured.
    assert_eq!(a.trim_db, -1.0);
    assert_eq!(tl.markers[1].main_beat, mid);
    // The set ends when the second track ends (128 bars from its start).
    assert_eq!(tl.end_beat, 384.0 + 512.0);
}

#[test]
fn loops_shift_the_transition() {
    let mut first = entry("aaaa", 0.0, None);
    first.loops.push(Loop {
        at_bar: 64.0,
        length_bars: 8.0,
        repeats: 2,
    });
    let p = plan(vec![
        first,
        entry("bbbb", 0.0, tr(96.0, 16.0, TransitionStyle::BassSwap)),
    ]);
    let tl = compile(&p, &library()).timeline.unwrap();
    let a = &tl.clips[0];
    let t = (96.0 + 16.0) * 4.0;
    assert_eq!(tl.clips[1].start_beat, t);
    // Inside the first repeat the playhead is back at bar 64.
    assert_eq!(a.track_beat(72.0 * 4.0), Some(64.0 * 4.0));
    assert_eq!(a.track_beat(t), Some(96.0 * 4.0));
}

#[test]
fn transition_inside_a_loop_region_uses_last_pass() {
    let mut first = entry("aaaa", 0.0, None);
    first.loops.push(Loop {
        at_bar: 96.0,
        length_bars: 16.0,
        repeats: 1,
    });
    let p = plan(vec![
        first,
        entry("bbbb", 0.0, tr(96.0, 16.0, TransitionStyle::Blend)),
    ]);
    let tl = compile(&p, &library()).timeline.unwrap();
    // Bar 96 is reached twice; the transition uses the second pass.
    assert_eq!(tl.clips[1].start_beat, (96.0 + 16.0) * 4.0);
}

#[test]
fn echo_out_starts_incoming_after_lead_in() {
    let p = plan(vec![
        entry("bbbb", 0.0, None),
        entry("cccc", 0.0, tr(112.0, 1.0, TransitionStyle::EchoOut)),
    ]);
    let tl = compile(&p, &library()).timeline.unwrap();
    let (a, b) = (&tl.clips[0], &tl.clips[1]);
    assert_eq!(b.start_beat, 113.0 * 4.0);
    assert!(a.end_beat > b.start_beat + 8.0, "echo tail kept alive");
    assert_eq!(a.lane(Param::Volume).eval(113.0 * 4.0), 0.0);
    assert!(a.lane(Param::Echo).eval(113.0 * 4.0 - 0.1) > 0.7);
    assert_eq!(a.lane(Param::Echo).eval(113.0 * 4.0), 0.0);
}

#[test]
fn custom_automation_replaces_style_lane() {
    let mut t = tr(96.0, 8.0, TransitionStyle::Blend).unwrap();
    t.automation.push(LaneSpec {
        side: Side::Incoming,
        param: Param::Filter,
        points: vec![
            PointSpec {
                bar: 0.0,
                value: -0.5,
                curve: Curve::Linear,
            },
            PointSpec {
                bar: 8.0,
                value: 0.0,
                curve: Curve::Linear,
            },
        ],
    });
    t.automation.push(LaneSpec {
        side: Side::Incoming,
        param: Param::Volume,
        points: vec![PointSpec {
            bar: 0.0,
            value: 0.8,
            curve: Curve::Linear,
        }],
    });
    let p = plan(vec![entry("aaaa", 0.0, None), {
        let mut e = entry("bbbb", 0.0, None);
        e.transition = Some(t);
        e
    }]);
    let tl = compile(&p, &library()).timeline.unwrap();
    let b = &tl.clips[1];
    assert_eq!(b.lane(Param::Filter).eval(384.0 + 16.0), -0.25);
    assert_eq!(b.lane(Param::Volume).eval(384.0 + 30.0), 0.8);
}

fn errors(p: &MixPlan) -> Vec<Issue> {
    let c = compile(p, &library());
    assert!(!c.is_ok());
    c.errors().cloned().collect()
}

#[test]
fn reports_unknown_track() {
    let e = errors(&plan(vec![entry("zzzz", 0.0, None)]));
    assert_eq!(e[0].path, "/entries/0/track");
}

#[test]
fn reports_transition_past_track_end_with_fix() {
    let e = errors(&plan(vec![
        entry("aaaa", 0.0, None),
        entry("bbbb", 0.0, tr(120.0, 16.0, TransitionStyle::Blend)),
    ]));
    assert_eq!(e[0].path, "/entries/1/transition/at_bar");
    assert_eq!(
        e[0].hint.as_deref().unwrap().split(',').next().unwrap(),
        "use at_bar <= 112"
    );
}

#[test]
fn reports_overlapping_transitions() {
    let e = errors(&plan(vec![
        entry("aaaa", 0.0, None),
        entry("bbbb", 0.0, tr(96.0, 16.0, TransitionStyle::Blend)),
        entry("cccc", 0.0, tr(8.0, 16.0, TransitionStyle::Blend)),
    ]));
    assert_eq!(e[0].path, "/entries/2/transition/at_bar");
    assert_eq!(e[0].hint.as_deref(), Some("use at_bar >= 16"));
}

#[test]
fn reports_missing_transition_and_bad_tempo() {
    let e = errors(&plan(vec![
        entry("aaaa", 0.0, None),
        entry("bbbb", 0.0, None),
    ]));
    assert_eq!(e[0].path, "/entries/1/transition");
    let e = errors(&plan(vec![
        entry("aaaa", 0.0, None),
        entry("dddd", 0.0, tr(96.0, 16.0, TransitionStyle::Blend)),
    ]));
    assert!(
        e.iter().any(|i| i.path.ends_with("/transition/tempo")),
        "{e:?}"
    );
}

#[test]
fn fixed_tempo_and_final_fade() {
    let mut p = plan(vec![{
        let mut e = entry("cccc", 0.0, None);
        e.end_bar = Some(64.0);
        e
    }]);
    p.tempo = TempoPolicy::Fixed { bpm: 126.0 };
    let tl = compile(&p, &library()).timeline.unwrap();
    assert_eq!(tl.end_beat, 256.0);
    assert!((tl.duration_sec() - 256.0 * 60.0 / 126.0).abs() < 1e-9);
    let v = tl.clips[0].lane(Param::Volume);
    assert_eq!(v.eval(240.0), 1.0);
    assert_eq!(v.eval(256.0), 0.0);
}

#[test]
fn schema_and_json() {
    let schema = plan_schema().to_string();
    assert!(schema.contains("entries") && schema.contains("eq_swap"));
    let json = r#"{"entries":[{"track":"aaaa"},{"track":"bbbb","transition":{"at_bar":96,"length_bars":16,"style":"blend"}}]}"#;
    let p = parse_plan(json).unwrap();
    assert!(p.keylock);
    assert!(compile(&p, &library()).is_ok());
    let bad = parse_plan(r#"{"entries":[{"track":"aaaa","stat_bar":3}]}"#).unwrap_err();
    assert!(bad.message.contains("stat_bar"), "{}", bad.message);
}
