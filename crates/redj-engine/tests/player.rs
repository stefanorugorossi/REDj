use std::sync::Arc;

use redj_core::{BeatGrid, Library, Loudness, TrackId, TrackInfo, TrackTags};
use redj_engine::{PlanPlayer, TrackAudio};
use redj_plan::*;

const SR: u32 = 44_100;

/// A track of clicks: a single-sample impulse on every beat, 0.5 amplitude,
/// doubled on downbeats.
fn click_track(id: &str, bpm: f64, bars: usize) -> (TrackInfo, Arc<TrackAudio>) {
    let beat_frames = 60.0 / bpm * f64::from(SR);
    let frames = (bars as f64 * 4.0 * beat_frames) as usize + SR as usize;
    let mut s = vec![0f32; frames * 2];
    for b in 0..bars * 4 {
        let i = (b as f64 * beat_frames).round() as usize;
        let v = if b % 4 == 0 { 0.8 } else { 0.4 };
        s[2 * i] = v;
        s[2 * i + 1] = v;
    }
    let grid = BeatGrid::constant(bpm, 0.0);
    let info = TrackInfo {
        id: TrackId(id.into()),
        path: format!("{id}.wav").into(),
        tags: TrackTags::default(),
        duration_sec: frames as f64 / f64::from(SR),
        sample_rate: SR,
        channels: 2,
        grid: grid.clone(),
        sections: vec![],
        bar_energy: vec![],
        loudness: Loudness {
            integrated_lufs: -9.0,
            true_peak_dbtp: 0.0,
            range_lu: 0.0,
        },
        key: None,
        analyzer_version: 1,
    };
    let audio = Arc::new(TrackAudio::new(TrackId(id.into()), SR, grid, s));
    (info, audio)
}

fn entry(id: &str, transition: Option<Transition>) -> Entry {
    Entry {
        track: TrackId(id.into()),
        start_bar: 0.0,
        end_bar: None,
        fade_out_bars: None,
        gain_db: Some(0.0),
        loops: vec![],
        transition,
        rationale: None,
    }
}

fn player(plan: &MixPlan, tracks: &[(TrackInfo, Arc<TrackAudio>)]) -> PlanPlayer {
    let lib: Library = tracks.iter().map(|t| t.0.clone()).collect();
    let c = compile(plan, &lib);
    let tl = Arc::new(c.timeline.unwrap_or_else(|| panic!("{:?}", c.issues)));
    let sources = tl
        .clips
        .iter()
        .map(|c| tracks.iter().find(|t| t.0.id == c.track).unwrap().1.clone())
        .collect();
    PlanPlayer::new(tl, sources, SR)
}

fn render_all(p: &mut PlanPlayer, block: usize) -> Vec<f32> {
    let mut out = Vec::new();
    let mut buf = vec![0f32; block * 2];
    while !p.is_finished() {
        p.process(&mut buf);
        out.extend_from_slice(&buf);
    }
    out
}

/// Frames of maxima above `thr` on the left channel, at most one per 10 ms
/// (an impulse is smeared over a few samples by the EQ's all-pass phase).
fn peaks(x: &[f32], thr: f32) -> Vec<usize> {
    let l: Vec<f32> = x.chunks(2).map(|c| c[0].abs()).collect();
    let w = 441;
    (0..l.len())
        .filter(|&i| {
            let a = i.saturating_sub(w);
            let b = (i + w).min(l.len());
            l[i] > thr
                && l[a..b]
                    .iter()
                    .enumerate()
                    .all(|(j, &v)| v < l[i] || (v == l[i] && a + j >= i))
        })
        .collect()
}

fn base_plan(entries: Vec<Entry>, keylock: bool) -> MixPlan {
    MixPlan {
        version: 1,
        title: None,
        tempo: TempoPolicy::Follow,
        keylock,
        loudness: LoudnessPolicy::default(),
        entries,
    }
}

#[test]
fn loop_jumps_land_on_exact_samples() {
    // 120 BPM: a beat is exactly 22050 frames.
    let t = click_track("aaaa000000000001", 120.0, 16);
    let mut e = entry("aaaa", None);
    e.end_bar = Some(12.0);
    e.fade_out_bars = Some(0.0);
    e.loops.push(Loop {
        at_bar: 4.0,
        length_bars: 1.0,
        repeats: 2,
    });
    let mut p = player(&base_plan(vec![e], false), &[t]);
    let out = render_all(&mut p, 512);
    let pk = peaks(&out, 0.1);
    // Every master beat has a click, exactly 22050 frames apart (the EQ's
    // all-pass phase delays all of them by the same sample or two).
    assert!(pk[0] <= 2, "first click at {}", pk[0]);
    for (k, &f) in pk.iter().take(40).enumerate() {
        assert_eq!(f - pk[0], k * 22_050, "click {k}");
    }
    // Downbeat pattern repeats inside the loop: bars 4, 4, 4, 5, ...
    // (downbeat clicks are twice as loud as the other beats).
    let l: Vec<f32> = out.chunks(2).map(|c| c[0].abs()).collect();
    for bar in [4usize, 5, 6, 7] {
        let down = l[bar * 4 * 22_050 + pk[0]];
        let next = l[(bar * 4 + 1) * 22_050 + pk[0]];
        assert!(
            down > 1.8 * next,
            "downbeat at master bar {bar}: {down} vs {next}"
        );
    }
}

#[test]
fn output_does_not_depend_on_block_size() {
    let a = click_track("aaaa000000000001", 124.0, 32);
    let b = click_track("bbbb000000000002", 128.0, 32);
    let plan = base_plan(
        vec![
            entry("aaaa", None),
            entry(
                "bbbb",
                Some(Transition {
                    at_bar: 16.0,
                    length_bars: 8.0,
                    style: TransitionStyle::EqSwap,
                    tempo: TempoChange::Glide,
                    automation: vec![],
                    rationale: None,
                }),
            ),
        ],
        true,
    );
    let tracks = [a, b];
    let x = render_all(&mut player(&plan, &tracks), 64);
    let y = render_all(&mut player(&plan, &tracks), 1000);
    let n = x.len().min(y.len());
    assert!(n > 1_000_000);
    assert!(x[..n] == y[..n], "renders differ");
}

#[test]
fn decks_stay_beat_locked_through_a_tempo_glide() {
    let a = click_track("aaaa000000000001", 124.0, 32);
    let b = click_track("bbbb000000000002", 128.0, 32);
    let plan = base_plan(
        vec![
            entry("aaaa", None),
            entry(
                "bbbb",
                Some(Transition {
                    at_bar: 16.0,
                    length_bars: 8.0,
                    style: TransitionStyle::BassSwap,
                    tempo: TempoChange::Glide,
                    automation: vec![],
                    rationale: None,
                }),
            ),
        ],
        false,
    );
    let mut p = player(&plan, &[a, b]);
    let tl = p.timeline().clone();
    let out = render_all(&mut p, 512);
    // During the transition both decks click on every master beat: the
    // peaks must sit within a millisecond of the master beat positions
    // and never come in pairs.
    let pk = peaks(&out, 0.2);
    let (t0, t1) = (64.0 + 4.0, 96.0 - 4.0);
    let mut checked = 0;
    for beat in (t0 as usize)..(t1 as usize) {
        let expect = tl.tempo.beat_to_sec(beat as f64) * f64::from(SR);
        let near: Vec<&usize> = pk
            .iter()
            .filter(|&&f| (f as f64 - expect).abs() < 0.02 * f64::from(SR))
            .collect();
        assert!(!near.is_empty(), "no click near beat {beat}");
        for &&f in &near {
            assert!(
                (f as f64 - expect).abs() <= 44.0,
                "beat {beat}: click at {f}, expected {expect:.1}"
            );
        }
        checked += 1;
    }
    assert!(checked > 20);
}
