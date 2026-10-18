use redj_control::{AutoPlanOptions, Session, auto_plan};
use redj_core::{
    BeatGrid, Key, Library, Loudness, Mode, Section, SectionKind, TrackId, TrackInfo, TrackTags,
};
use redj_plan::{TransitionStyle, compile};

fn track(id: &str, bpm: f64, key: Key, energy: f32) -> TrackInfo {
    let bars = 128.0;
    let s = |kind, a: f64, b: f64, e| Section {
        kind,
        start_bar: a,
        end_bar: b,
        energy: e,
    };
    TrackInfo {
        id: TrackId(id.into()),
        path: format!("{id}.wav").into(),
        tags: TrackTags {
            title: Some(format!("Track {id}")),
            ..TrackTags::default()
        },
        duration_sec: bars * 240.0 / bpm + 1.0,
        sample_rate: 44_100,
        channels: 2,
        grid: BeatGrid::constant(bpm, 0.0),
        sections: vec![
            s(SectionKind::Intro, 0.0, 16.0, 0.4),
            s(SectionKind::Drop, 16.0, 112.0, 0.9),
            s(SectionKind::Outro, 112.0, 128.0, 0.4),
        ],
        bar_energy: vec![energy; 128],
        loudness: Loudness {
            integrated_lufs: -8.0,
            true_peak_dbtp: -0.3,
            range_lu: 4.0,
        },
        key: Some(key),
        analyzer_version: 1,
    }
}

fn library() -> Library {
    [
        track("a000000000000001", 124.0, Key::new(9, Mode::Minor), 0.5),
        track("b000000000000002", 126.0, Key::new(4, Mode::Minor), 0.6),
        track("c000000000000003", 125.0, Key::new(0, Mode::Major), 0.55),
        track("d000000000000004", 128.0, Key::new(3, Mode::Minor), 0.7),
        track("e000000000000005", 145.0, Key::new(9, Mode::Minor), 0.8),
    ]
    .into_iter()
    .collect()
}

#[test]
fn auto_plan_is_valid_and_sensibly_ordered() {
    let lib = library();
    let plan = auto_plan(&lib, &AutoPlanOptions::default());
    let c = compile(&plan, &lib);
    assert!(c.is_ok(), "{:?}", c.issues);
    assert_eq!(plan.entries.len(), 5);
    // Slowest, calmest first; the 145 BPM outlier last, reached by echo out.
    assert_eq!(plan.entries[0].track.0, "a000000000000001");
    let last = plan.entries.last().unwrap();
    assert_eq!(last.track.0, "e000000000000005");
    assert_eq!(
        last.transition.as_ref().unwrap().style,
        TransitionStyle::EchoOut
    );
    // Regular transitions start at the outgoing outro.
    let t1 = plan.entries[1].transition.as_ref().unwrap();
    assert_eq!(t1.at_bar, 112.0);
    assert_eq!(t1.length_bars, 16.0);
}

#[test]
fn limits_by_count_and_minutes() {
    let lib = library();
    let p = auto_plan(
        &lib,
        &AutoPlanOptions {
            max_tracks: Some(3),
            ..AutoPlanOptions::default()
        },
    );
    assert_eq!(p.entries.len(), 3);
    let p = auto_plan(
        &lib,
        &AutoPlanOptions {
            target_minutes: Some(8.0),
            ..AutoPlanOptions::default()
        },
    );
    assert!(
        p.entries.len() >= 2 && p.entries.len() <= 3,
        "{}",
        p.entries.len()
    );
}

#[test]
fn session_submit_reports_and_accepts() {
    let session = Session::new(library());
    let mut plan = session.suggest(&AutoPlanOptions {
        max_tracks: Some(2),
        ..AutoPlanOptions::default()
    });
    let report = session.submit(plan.clone());
    assert!(report.ok, "{report:?}");
    assert_eq!(report.tracklist.len(), 2);
    assert!(session.accepted_plan().is_some());

    plan.entries[1].transition.as_mut().unwrap().at_bar = 400.0;
    let bad = session.check(&plan);
    assert!(!bad.ok);
    assert!(bad.errors[0].path.contains("at_bar"));
    let compat = session
        .compatibility(&["a000".into(), "e000".into()], false)
        .unwrap();
    assert!(compat[0].tempo_change_pct > 16.0);
    assert!(session.detail("b000").unwrap().summary.mix_out == (112.0, 128.0));
}
