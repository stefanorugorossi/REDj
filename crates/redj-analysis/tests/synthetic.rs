//! Analysis accuracy on synthetic tracks with known ground truth.

use redj_analysis::{AnalysisOptions, analyze_audio};
use redj_core::{Key, Mode, SectionKind};
use redj_media::Audio;
use redj_testkit::{SynthSpec, SynthTrack, render};

fn audio(t: &SynthTrack) -> Audio {
    Audio {
        sample_rate: t.sample_rate,
        channels: 2,
        samples: t.samples.clone(),
    }
}

fn check(spec: SynthSpec) {
    let t = render(&spec);
    let a = analyze_audio(&audio(&t), &AnalysisOptions::default()).unwrap();
    let truth = &t.truth;
    println!(
        "bpm {} (truth {}), origin {:.4} (truth {:.4}), key {:?}",
        a.grid.bpm, truth.bpm, a.grid.origin_sec, truth.first_downbeat_sec, a.key
    );
    for s in &a.sections {
        println!(
            "  {:?} {}..{} e={:.2}",
            s.kind, s.start_bar, s.end_bar, s.energy
        );
    }
    assert!(a.grid.is_constant(), "expected a constant grid");
    assert!(
        (a.grid.bpm - truth.bpm).abs() < 0.01,
        "bpm {} vs {}",
        a.grid.bpm,
        truth.bpm
    );
    let bar = 240.0 / truth.bpm;
    let err = (a.grid.origin_sec - truth.first_downbeat_sec).rem_euclid(bar);
    let err = err.min(bar - err);
    assert!(err < 0.005, "downbeat off by {err:.4}s");
    // Same bar numbering as the truth.
    assert!((a.grid.origin_sec - truth.first_downbeat_sec).abs() < 0.005);

    let key = a.key.expect("key");
    assert!(
        key.camelot_distance(&truth.key) <= 1,
        "key {key} vs {}",
        truth.key
    );

    // Every true boundary is found within one bar, with the right kind
    // just after it.
    for &(kind, start, _) in &truth.sections {
        let found = a
            .sections
            .iter()
            .find(|s| (s.start_bar - f64::from(start)).abs() <= 1.0);
        let found = found.unwrap_or_else(|| panic!("no section starting near bar {start}"));
        assert_eq!(found.kind, kind, "section at bar {start}");
    }
}

#[test]
fn club_track_128_a_minor() {
    check(SynthSpec::club(128.0, Key::new(9, Mode::Minor)));
}

#[test]
fn club_track_with_lead_in_124_f_major() {
    check(
        SynthSpec::club(124.0, Key::new(5, Mode::Major))
            .with_lead_in(0.37)
            .with_seed(7),
    );
}

#[test]
fn club_track_140_at_48k() {
    check(
        SynthSpec::club(140.0, Key::new(2, Mode::Minor))
            .with_sample_rate(48_000)
            .with_lead_in(0.05)
            .with_seed(3),
    );
}

#[test]
fn fractional_tempo_174_5() {
    let mut spec = SynthSpec::club(174.5, Key::new(7, Mode::Minor)).with_seed(11);
    spec.sections = vec![
        (SectionKind::Intro, 16),
        (SectionKind::Drop, 32),
        (SectionKind::Breakdown, 16),
        (SectionKind::Drop, 32),
        (SectionKind::Outro, 16),
    ];
    check(spec);
}

#[test]
fn cache_round_trip_and_tags_fallback() {
    use redj_analysis::{Cache, analyze_cached, scan_dir};
    let dir = std::env::temp_dir().join(format!("redj-analysis-cache-{}", std::process::id()));
    let music = dir.join("music");
    std::fs::create_dir_all(&music).unwrap();
    let t = render(&SynthSpec::short(126.0, Key::new(0, Mode::Minor)));
    let path = music.join("Some Track.wav");
    t.write_wav(&path).unwrap();
    std::fs::write(music.join("notes.txt"), "not audio").unwrap();
    assert_eq!(scan_dir(&music).unwrap(), vec![path.clone()]);

    let cache = Cache::new(dir.join("cache"));
    let opts = AnalysisOptions::default();
    let first = analyze_cached(&path, Some(&cache), &opts).unwrap();
    assert_eq!(first.display_name(), "Some Track");
    assert_eq!(first.id.0.len(), 16);
    assert!((first.grid.bpm - 126.0).abs() < 0.01, "{:?}", first.grid);
    assert!(std::fs::read_dir(cache.dir()).unwrap().count() == 1);

    // Moving the file keeps the id and hits the cache.
    let moved = music.join("renamed.wav");
    std::fs::rename(&path, &moved).unwrap();
    let second = analyze_cached(&moved, Some(&cache), &opts).unwrap();
    assert_eq!(second.id, first.id);
    assert_eq!(second.grid, first.grid);
    assert!(second.path.ends_with("renamed.wav"));
    std::fs::remove_dir_all(&dir).ok();
}
