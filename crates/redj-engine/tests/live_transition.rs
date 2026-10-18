//! Plan-driven transitions in the live engine.

use std::sync::Arc;

use redj_core::{BeatGrid, Library, Loudness, TrackId, TrackInfo, TrackTags};
use redj_engine::{
    Command, DeckParam, LiveConfig, LiveEngine, LiveTransition, TrackAudio, TransitionState,
};
use redj_plan::*;

const SR: u32 = 44_100;

fn track(id: &str, bpm: f64, bars: usize) -> (TrackInfo, Arc<TrackAudio>) {
    let beat = 60.0 / bpm * f64::from(SR);
    let frames = (bars as f64 * 4.0 * beat) as usize + SR as usize;
    let mut s = vec![0f32; frames * 2];
    for b in 0..bars * 4 {
        let i = (b as f64 * beat).round() as usize;
        s[2 * i] = 0.5;
        s[2 * i + 1] = 0.5;
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
    (
        info,
        Arc::new(TrackAudio::new(TrackId(id.into()), SR, grid, s)),
    )
}

/// A: 124 BPM playing from bar 0; B: 128 BPM mixed in at A's bar 8 over
/// 4 bars, at A's tempo.
fn setup() -> (LiveEngine, redj_engine::EngineHandle, Box<LiveTransition>) {
    let (ia, a) = track("aaaa000000000001", 124.0, 32);
    let (ib, b) = track("bbbb000000000002", 128.0, 32);
    let lib: Library = [ia, ib].into_iter().collect();
    let plan = MixPlan {
        version: 1,
        title: None,
        tempo: TempoPolicy::Fixed { bpm: 124.0 },
        keylock: true,
        loudness: LoudnessPolicy {
            auto_gain: false,
            target_lufs: -9.0,
        },
        entries: vec![
            Entry {
                track: TrackId("aaaa".into()),
                start_bar: 0.0,
                end_bar: None,
                fade_out_bars: None,
                gain_db: None,
                loops: vec![],
                transition: None,
                rationale: None,
            },
            Entry {
                track: TrackId("bbbb".into()),
                start_bar: 0.0,
                end_bar: None,
                fade_out_bars: None,
                gain_db: None,
                loops: vec![],
                transition: Some(Transition {
                    at_bar: 8.0,
                    length_bars: 4.0,
                    style: TransitionStyle::BassSwap,
                    tempo: TempoChange::Glide,
                    automation: vec![],
                    rationale: None,
                }),
                rationale: None,
            },
        ],
    };
    let c = compile(&plan, &lib);
    let tl = Arc::new(c.timeline.unwrap_or_else(|| panic!("{:?}", c.issues)));
    let (e, mut h) = LiveEngine::new(LiveConfig {
        sample_rate: SR,
        decks: 2,
        ceiling_db: -1.0,
    });
    for (deck, audio) in [(0, a), (1, b)] {
        h.send(Command::Load {
            deck,
            audio,
            cue_frame: 0.0,
            generation: 1,
        })
        .unwrap();
    }
    let t = Box::new(LiveTransition {
        id: 7,
        out_deck: 0,
        in_deck: 1,
        timeline: tl,
        beat_offset: 0.0,
    });
    (e, h, t)
}

fn run(e: &mut LiveEngine, blocks: usize) {
    let mut m = vec![0f32; 1024];
    let mut p = vec![0f32; 1024];
    for _ in 0..blocks {
        e.process(&mut m, &mut p);
    }
}

/// Blocks of 512 frames per bar at 124 BPM.
const BAR: usize = (4.0 * 60.0 / 124.0 * 44_100.0 / 512.0) as usize + 1;

#[test]
fn transition_runs_and_hands_back() {
    let (mut e, mut h, t) = setup();
    h.send(Command::Play { deck: 0, on: true }).unwrap();
    h.send(Command::ArmTransition(t)).unwrap();
    run(&mut e, 2);
    let st = h.status();
    assert_eq!(st.transition.state, TransitionState::Armed);
    assert!(!st.decks[1].playing, "incoming waits");
    run(&mut e, 8 * BAR + 2 * BAR);
    let st = h.status();
    assert_eq!(st.transition.state, TransitionState::Active);
    assert!(st.decks[1].playing);
    // Incoming beats are locked to the outgoing ones.
    let pa = st.decks[0].beat - 32.0;
    let pb = st.decks[1].beat;
    assert!((pa - pb).abs() < 0.01, "outgoing {pa} incoming {pb}");
    // Mid-transition (bass swap at bar 2 of 4): incoming low is back.
    assert_eq!(st.decks[1].eq_db[0], 0.0);
    assert_eq!(st.decks[0].eq_db[0], -40.0);
    run(&mut e, 3 * BAR);
    let st = h.status();
    assert_eq!(st.transition.state, TransitionState::Idle);
    assert_eq!(st.transition.completed, 1);
    assert_eq!(st.transition.last_id, 7);
    assert!(!st.decks[0].playing && st.decks[1].playing);
    // The incoming deck keeps the 124 BPM master tempo on its own fader.
    assert!(
        (st.decks[1].bpm - 124.0).abs() < 1e-6,
        "{}",
        st.decks[1].bpm
    );
    assert_eq!(st.decks[1].volume, 1.0);
    assert!(st.decks[1].is_master);
}

#[test]
fn human_takes_parameters_and_transport() {
    let (mut e, mut h, t) = setup();
    h.send(Command::Play { deck: 0, on: true }).unwrap();
    h.send(Command::ArmTransition(t)).unwrap();
    run(&mut e, 9 * BAR);
    assert_eq!(h.status().transition.state, TransitionState::Active);
    // Moving the incoming filter takes it over; the rest stays automated.
    h.send(Command::Param {
        deck: 1,
        param: DeckParam::Filter,
        value: 0.4,
    })
    .unwrap();
    run(&mut e, 2);
    let st = h.status();
    assert_ne!(st.transition.human & (1 << (8 + Param::Filter.index())), 0);
    assert_eq!(st.decks[1].filter, 0.4);
    // Seeking the incoming deck cancels the transition.
    h.send(Command::BeatJump {
        deck: 1,
        beats: 4.0,
    })
    .unwrap();
    run(&mut e, 2);
    let st = h.status();
    assert_eq!(st.transition.state, TransitionState::Idle);
    assert_eq!(st.transition.cancelled, 1);
    assert!(
        st.decks[0].playing && st.decks[1].playing,
        "decks stay as they are"
    );
}

#[test]
fn cancel_before_start() {
    let (mut e, mut h, t) = setup();
    h.send(Command::Play { deck: 0, on: true }).unwrap();
    h.send(Command::ArmTransition(t)).unwrap();
    run(&mut e, 4);
    h.send(Command::CancelTransition).unwrap();
    run(&mut e, 12 * BAR);
    let st = h.status();
    assert_eq!(st.transition.state, TransitionState::Idle);
    assert_eq!(
        st.transition.cancelled, 0,
        "cancelled by request, not by override"
    );
    assert!(!st.decks[1].playing);
}
