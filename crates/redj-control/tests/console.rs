//! Console driven by a virtual DDJ-FLX4, with the engine run by hand.

use std::sync::Arc;
use std::time::{Duration, Instant};

use redj_analysis::{AnalysisOptions, analyze_file};
use redj_control::{Console, Session};
use redj_core::{Key, Library, Mode};
use redj_engine::{LiveConfig, LiveEngine};
use redj_midi::{Mapper, builtin_profiles, virtual_flx4 as vc};
use redj_testkit::{SynthSpec, render};

fn step(engine: &mut LiveEngine, console: &mut Console, blocks: usize) {
    // Poll first so queued MIDI becomes engine commands before processing.
    console.poll();
    let mut m = vec![0f32; 1024];
    let mut p = vec![0f32; 1024];
    for _ in 0..blocks {
        engine.process(&mut m, &mut p);
    }
    console.poll();
}

#[test]
fn virtual_controller_plays_a_loaded_track() {
    let dir = std::env::temp_dir().join(format!("redj-console-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.wav");
    render(&SynthSpec::short(126.0, Key::new(9, Mode::Minor)))
        .write_wav(&path)
        .unwrap();
    let info = analyze_file(&path, &AnalysisOptions::default()).unwrap();
    let id = info.id.clone();
    let lib: Library = [info].into_iter().collect();
    let session = Arc::new(Session::new(lib));

    let (mut engine, handle) = LiveEngine::new(LiveConfig {
        sample_rate: 44_100,
        decks: 2,
        ceiling_db: -1.0,
    });
    let mut console = Console::new(session, handle, 44_100, 2);
    let (tx, rx) = std::sync::mpsc::channel();
    let leds = Arc::new(std::sync::Mutex::new(Vec::new()));
    let leds2 = leds.clone();
    console.attach_midi(
        Mapper::new(builtin_profiles().into_iter().next()),
        rx,
        Some(Box::new(move |m| leds2.lock().unwrap().push(m))),
    );

    // LOAD via the browse encoder + LOAD button of deck 1.
    assert_eq!(console.browse_order(), std::slice::from_ref(&id));
    tx.send(redj_midi::MidiMessage::new(0x96, 0x46, 0x7F))
        .unwrap();
    let t0 = Instant::now();
    loop {
        step(&mut engine, &mut console, 1);
        if console.status().decks[0].loaded {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "track did not load");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(console.loaded(0).unwrap().info.id, id);
    assert!(!console.loaded(0).unwrap().waveform.bins.is_empty());
    // Auto-cue at the first downbeat.
    assert!(console.status().decks[0].cue >= 0.0);

    for m in vc::play(0) {
        tx.send(m).unwrap();
    }
    step(&mut engine, &mut console, 1);
    step(&mut engine, &mut console, 100);
    let st = *console.status();
    assert!(st.decks[0].playing);
    assert!((st.decks[0].bpm - 126.0).abs() < 0.01);
    assert!(st.decks[0].peak > 0.0 || st.master_peak > 0.0);

    // Hot cue pad 1 stores a cue; its LED lights.
    for m in vc::hot_cue_pad(0, 0) {
        tx.send(m).unwrap();
    }
    step(&mut engine, &mut console, 2);
    assert!(console.status().decks[0].hot_cues[0].is_some());
    assert!(
        leds.lock()
            .unwrap()
            .contains(&redj_midi::MidiMessage::new(0x97, 0x00, 0x7F))
    );
    // Play LED is lit.
    assert!(
        leds.lock()
            .unwrap()
            .contains(&redj_midi::MidiMessage::new(0x90, 0x0B, 0x7F))
    );

    // Tempo fader to the "+" end speeds the deck up by the range (6%).
    for m in vc::tempo(0, 16_383) {
        tx.send(m).unwrap();
    }
    step(&mut engine, &mut console, 2);
    let bpm = console.status().decks[0].bpm;
    assert!(
        (bpm - 126.0 * 0.94).abs() < 0.01 || (bpm - 126.0 * 1.06).abs() < 0.01,
        "{bpm}"
    );
    std::fs::remove_dir_all(&dir).ok();
}
