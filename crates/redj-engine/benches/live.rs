//! Audio callback cost: four decks playing with keylock at different
//! tempos, compared with the real-time budget of a 512-frame block.
//!
//! `cargo bench -p redj-engine --bench live`

use std::sync::Arc;
use std::time::Instant;

use redj_core::{BeatGrid, Key, Mode, TrackId};
use redj_engine::{Command, DeckParam, LiveConfig, LiveEngine, TrackAudio};
use redj_testkit::{SynthSpec, render};

fn main() {
    let sr = 44_100;
    let t = render(&SynthSpec::short(125.0, Key::new(9, Mode::Minor)));
    let audio = Arc::new(TrackAudio::new(
        TrackId("t".into()),
        sr,
        BeatGrid::constant(125.0, 0.0),
        t.samples,
    ));
    for (decks, keylock) in [(2, false), (2, true), (4, true)] {
        let (mut e, mut h) = LiveEngine::new(LiveConfig {
            sample_rate: sr,
            decks,
            ceiling_db: -1.0,
        });
        for d in 0..decks {
            h.send(Command::Load {
                deck: d,
                audio: audio.clone(),
                cue_frame: 0.0,
                generation: 1,
            })
            .unwrap();
            h.send(Command::Keylock {
                deck: d,
                on: keylock,
            })
            .unwrap();
            // Different tempos so every deck really stretches/resamples.
            h.send(Command::Param {
                deck: d,
                param: DeckParam::Tempo,
                value: 0.3 * d as f32 - 0.4,
            })
            .unwrap();
            h.send(Command::Param {
                deck: d,
                param: DeckParam::Filter,
                value: 0.3,
            })
            .unwrap();
            h.send(Command::Play { deck: d, on: true }).unwrap();
        }
        let mut m = vec![0f32; 1024];
        let mut p = vec![0f32; 1024];
        for _ in 0..50 {
            e.process(&mut m, &mut p);
        }
        let blocks = 2000;
        let mut worst = 0f64;
        let t0 = Instant::now();
        for _ in 0..blocks {
            let b = Instant::now();
            e.process(&mut m, &mut p);
            worst = worst.max(b.elapsed().as_secs_f64());
        }
        let avg = t0.elapsed().as_secs_f64() / blocks as f64;
        let budget = 512.0 / f64::from(sr);
        println!(
            "{decks} decks, keylock {keylock:5}: avg {:6.3} ms, worst {:6.3} ms per block, budget {:.2} ms ({:4.1}% load)",
            avg * 1e3,
            worst * 1e3,
            budget * 1e3,
            avg / budget * 100.0
        );
    }
}
