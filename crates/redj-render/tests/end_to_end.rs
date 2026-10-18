//! Synthetic tracks -> analysis -> hand-written plan -> rendered files.

use std::path::PathBuf;

use redj_analysis::{AnalysisOptions, analyze_file};
use redj_core::{Key, Library, Mode};
use redj_plan::parse_plan;
use redj_render::{RenderOptions, render_plan};
use redj_testkit::{SynthSpec, render};

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("redj-render-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn renders_a_two_track_mix() {
    let dir = temp_dir("e2e");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    render(&SynthSpec::short(124.0, Key::new(9, Mode::Minor)).with_seed(1))
        .write_wav(&a)
        .unwrap();
    render(
        &SynthSpec::short(128.0, Key::new(4, Mode::Minor))
            .with_seed(2)
            .with_lead_in(0.21),
    )
    .write_wav(&b)
    .unwrap();
    let opts = AnalysisOptions::default();
    let ta = analyze_file(&a, &opts).unwrap();
    let tb = analyze_file(&b, &opts).unwrap();
    assert!((ta.grid.bpm - 124.0).abs() < 0.01 && (tb.grid.bpm - 128.0).abs() < 0.01);
    let plan_json = format!(
        r#"{{
            "title": "Test mix",
            "entries": [
                {{ "track": "{}" }},
                {{ "track": "{}", "transition": {{ "at_bar": 24, "length_bars": 8, "style": "eq_swap" }} }}
            ]
        }}"#,
        ta.id, tb.id
    );
    let plan = parse_plan(&plan_json).unwrap();
    let lib: Library = [ta.clone(), tb.clone()].into_iter().collect();

    let out = dir.join("mix.wav");
    let ropts = RenderOptions {
        mp3: true,
        ..RenderOptions::default()
    };
    let rep = render_plan(&plan, &lib, &out, &ropts).unwrap();
    // A solo for 24 bars, then B plays all its 40 bars (the first 8 during
    // the transition, at a tempo gliding from 124 to 128) plus its tail.
    let expected_sec = 24.0 * 240.0 / 124.0 + 40.0 * 240.0 / 128.0;
    assert!(
        rep.duration_sec > expected_sec && rep.duration_sec < expected_sec + 2.0,
        "{} vs {expected_sec}",
        rep.duration_sec
    );
    assert!(
        rep.true_peak_dbtp <= -1.0,
        "true peak {}",
        rep.true_peak_dbtp
    );
    assert!(
        rep.integrated_lufs > -20.0,
        "loudness {}",
        rep.integrated_lufs
    );
    assert_eq!(rep.tracklist.len(), 2);
    assert!(rep.tracklist[1].start_sec > 40.0);

    let wav = hound::WavReader::open(&out).unwrap();
    assert_eq!(wav.spec().bits_per_sample, 24);
    assert_eq!(wav.spec().sample_rate, 44_100);
    let cue = std::fs::read_to_string(rep.cue.as_ref().unwrap()).unwrap();
    assert!(cue.contains("TRACK 02 AUDIO"));
    assert!(rep.plan.as_ref().unwrap().exists());
    if let Some(mp3) = &rep.mp3 {
        let decoded = redj_media::decode_file(mp3).unwrap();
        assert!((decoded.duration_sec() - rep.duration_sec).abs() < 0.2);
    }

    // Determinism: a second render is bit-identical.
    let out2 = dir.join("mix2.wav");
    let ropts2 = RenderOptions {
        mp3: false,
        sidecars: false,
        ..RenderOptions::default()
    };
    render_plan(&plan, &lib, &out2, &ropts2).unwrap();
    let pcm = |p: &PathBuf| -> Vec<i32> {
        hound::WavReader::open(p)
            .unwrap()
            .samples::<i32>()
            .map(Result::unwrap)
            .collect()
    };
    assert!(pcm(&out) == pcm(&out2), "renders are not deterministic");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rejects_invalid_plans_with_readable_errors() {
    let plan = parse_plan(r#"{"entries":[{"track":"nope"}]}"#).unwrap();
    let err = render_plan(
        &plan,
        &Library::new(),
        &std::env::temp_dir().join("x.wav"),
        &RenderOptions::default(),
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("/entries/0/track") && msg.contains("unknown track"),
        "{msg}"
    );
}
