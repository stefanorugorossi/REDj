use std::sync::Arc;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use redj_core::{BeatGrid, TrackId};
use redj_engine::{Command, DeckParam, LiveConfig, LiveEngine, TrackAudio};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

const SR: u32 = 44_100;

/// Clicks on every beat (louder on downbeats).
fn clicks(bpm: f64, seconds: f64, offset_sec: f64) -> Arc<TrackAudio> {
    let frames = (seconds * f64::from(SR)) as usize;
    let mut s = vec![0f32; frames * 2];
    let beat = 60.0 / bpm * f64::from(SR);
    let mut b = 0;
    loop {
        let i = (offset_sec * f64::from(SR) + b as f64 * beat).round() as usize;
        if i >= frames {
            break;
        }
        let v = if b % 4 == 0 { 0.8 } else { 0.4 };
        s[2 * i] = v;
        s[2 * i + 1] = v;
        b += 1;
    }
    Arc::new(TrackAudio::new(
        TrackId("t".into()),
        SR,
        BeatGrid::constant(bpm, offset_sec),
        s,
    ))
}

fn engine() -> (LiveEngine, redj_engine::EngineHandle) {
    LiveEngine::new(LiveConfig {
        sample_rate: SR,
        decks: 2,
        ceiling_db: -1.0,
    })
}

fn run(e: &mut LiveEngine, blocks: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(blocks * 512 * 2);
    let mut m = vec![0f32; 1024];
    let mut p = vec![0f32; 1024];
    for _ in 0..blocks {
        assert_no_alloc(|| e.process(&mut m, &mut p));
        out.extend_from_slice(&m);
    }
    out
}

#[test]
fn play_pause_and_cue() {
    let (mut e, mut h) = engine();
    h.send(Command::Load {
        deck: 0,
        audio: clicks(120.0, 30.0, 0.0),
        cue_frame: 0.0,
        generation: 1,
    })
    .unwrap();
    h.send(Command::Play { deck: 0, on: true }).unwrap();
    run(&mut e, 86); // ~1 s
    let st = h.status();
    assert!(st.decks[0].playing && st.decks[0].loaded);
    let pos = st.decks[0].position;
    assert!((pos - 86.0 * 512.0).abs() < 1.0, "{pos}");
    assert!((st.decks[0].bpm - 120.0).abs() < 1e-9);
    // CUE while playing: back to the cue point, paused.
    h.send(Command::Cue {
        deck: 0,
        pressed: true,
    })
    .unwrap();
    run(&mut e, 1);
    let st = h.status();
    assert!(!st.decks[0].playing);
    assert_eq!(st.decks[0].position, 0.0);
    // Holding CUE at the cue point previews; releasing returns.
    run(&mut e, 1);
    h.send(Command::Cue {
        deck: 0,
        pressed: false,
    })
    .unwrap();
    h.send(Command::Cue {
        deck: 0,
        pressed: true,
    })
    .unwrap();
    run(&mut e, 10);
    assert!(h.status().decks[0].playing);
    h.send(Command::Cue {
        deck: 0,
        pressed: false,
    })
    .unwrap();
    run(&mut e, 1);
    let st = h.status();
    assert!(!st.decks[0].playing);
    assert_eq!(st.decks[0].position, 0.0);
}

#[test]
fn beat_loop_wraps_on_the_grid() {
    let (mut e, mut h) = engine();
    h.send(Command::Load {
        deck: 0,
        audio: clicks(120.0, 30.0, 0.0),
        cue_frame: 0.0,
        generation: 1,
    })
    .unwrap();
    h.send(Command::Seek {
        deck: 0,
        frame: 22_050.0 * 4.3,
    })
    .unwrap();
    h.send(Command::BeatLoop {
        deck: 0,
        beats: 2.0,
    })
    .unwrap();
    h.send(Command::Play { deck: 0, on: true }).unwrap();
    run(&mut e, 400);
    let st = h.status();
    assert!(st.decks[0].loop_active);
    let (s, end) = st.decks[0].loop_range.unwrap();
    assert_eq!((s, end), (22_050.0 * 4.0, 22_050.0 * 6.0));
    assert!(st.decks[0].position >= s && st.decks[0].position < end);
    // Halving keeps the start.
    h.send(Command::LoopScale {
        deck: 0,
        factor: 0.5,
    })
    .unwrap();
    run(&mut e, 1);
    assert_eq!(
        h.status().decks[0].loop_range.unwrap(),
        (88_200.0, 110_250.0)
    );
    h.send(Command::Reloop { deck: 0 }).unwrap();
    run(&mut e, 1);
    assert!(!h.status().decks[0].loop_active);
}

#[test]
fn sync_matches_tempo_and_phase() {
    let (mut e, mut h) = engine();
    // Deck A at 124 BPM is the master; deck B at 128 BPM with a different
    // grid offset syncs to it.
    h.send(Command::Load {
        deck: 0,
        audio: clicks(124.0, 60.0, 0.0),
        cue_frame: 0.0,
        generation: 1,
    })
    .unwrap();
    h.send(Command::Load {
        deck: 1,
        audio: clicks(128.0, 60.0, 0.137),
        cue_frame: 0.0,
        generation: 1,
    })
    .unwrap();
    h.send(Command::Sync { deck: 0, on: true }).unwrap();
    h.send(Command::Play { deck: 0, on: true }).unwrap();
    run(&mut e, 100);
    h.send(Command::Seek {
        deck: 1,
        frame: 30_000.0,
    })
    .unwrap();
    h.send(Command::Sync { deck: 1, on: true }).unwrap();
    h.send(Command::Play { deck: 1, on: true }).unwrap();
    run(&mut e, 2000); // ~23 s
    let st = h.status();
    assert!(st.decks[0].is_master && !st.decks[1].is_master);
    assert!(
        (st.decks[1].bpm - 124.0).abs() < 0.2,
        "bpm {}",
        st.decks[1].bpm
    );
    let pa = st.decks[0].beat.rem_euclid(1.0);
    let pb = st.decks[1].beat.rem_euclid(1.0);
    let d = (pa - pb).abs();
    assert!(d.min(1.0 - d) < 0.01, "phase {pa} vs {pb}");
}

#[test]
fn faders_and_crossfader_mute_decks() {
    let (mut e, mut h) = engine();
    h.send(Command::Load {
        deck: 0,
        audio: clicks(120.0, 30.0, 0.0),
        cue_frame: 0.0,
        generation: 1,
    })
    .unwrap();
    h.send(Command::Play { deck: 0, on: true }).unwrap();
    let loud = run(&mut e, 200);
    assert!(loud.iter().any(|s| s.abs() > 0.1));
    h.send(Command::Crossfader(1.0)).unwrap(); // deck 0 is on the left
    run(&mut e, 10);
    let muted = run(&mut e, 200);
    assert!(muted.iter().all(|s| s.abs() < 1e-3));
    h.send(Command::Crossfader(0.5)).unwrap();
    h.send(Command::Param {
        deck: 0,
        param: DeckParam::Volume,
        value: 0.0,
    })
    .unwrap();
    run(&mut e, 10);
    let muted = run(&mut e, 200);
    assert!(muted.iter().all(|s| s.abs() < 1e-3));
}

#[test]
fn tempo_fader_changes_speed() {
    let (mut e, mut h) = engine();
    h.send(Command::Load {
        deck: 0,
        audio: clicks(125.0, 30.0, 0.0),
        cue_frame: 0.0,
        generation: 1,
    })
    .unwrap();
    h.send(Command::Param {
        deck: 0,
        param: DeckParam::Tempo,
        value: 0.5,
    })
    .unwrap();
    h.send(Command::Play { deck: 0, on: true }).unwrap();
    run(&mut e, 100);
    let st = h.status();
    assert!(
        (st.decks[0].bpm - 130.0).abs() < 1e-6,
        "{}",
        st.decks[0].bpm
    );
    assert!((st.decks[0].position - 100.0 * 512.0 * 1.04).abs() < 1.0);
}
