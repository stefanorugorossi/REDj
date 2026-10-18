//! Live AI director end to end: mock model -> MCP -> console -> engine.

use std::sync::Arc;
use std::time::{Duration, Instant};

use redj_ai::{AgentConfig, MockModel, plan_next};
use redj_analysis::{AnalysisOptions, analyze_file};
use redj_control::live::TransitionRequest;
use redj_control::{Console, Session};
use redj_core::{Key, Library, Mode, SectionKind, TrackId};
use redj_engine::{Command, LiveConfig, LiveEngine, TransitionState};
use redj_plan::TransitionStyle;
use redj_testkit::{SynthSpec, render};
use serde_json::json;

struct Rig {
    engine: LiveEngine,
    console: Console,
    session: Arc<Session>,
    a: TrackId,
    b: TrackId,
    _dir: std::path::PathBuf,
}

fn rig(name: &str) -> Rig {
    let dir = std::env::temp_dir().join(format!("redj-live-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut ids = Vec::new();
    for (i, (bpm, key)) in [
        (126.0, Key::new(9, Mode::Minor)),
        (127.0, Key::new(4, Mode::Minor)),
    ]
    .into_iter()
    .enumerate()
    {
        let mut spec = SynthSpec::short(bpm, key).with_seed(i as u64 + 1);
        spec.sections = vec![
            (SectionKind::Intro, 8),
            (SectionKind::Drop, 16),
            (SectionKind::Outro, 8),
        ];
        let path = dir.join(format!("t{i}.wav"));
        render(&spec).write_wav(&path).unwrap();
        ids.push(analyze_file(&path, &AnalysisOptions::default()).unwrap());
    }
    let (a, b) = (ids[0].id.clone(), ids[1].id.clone());
    let lib: Library = ids.into_iter().collect();
    let session = Arc::new(Session::new(lib));
    let (engine, handle) = LiveEngine::new(LiveConfig {
        sample_rate: 44_100,
        decks: 2,
        ceiling_db: -1.0,
    });
    let mut console = Console::new(session.clone(), handle, 44_100, 2);
    console.ai_on = true;
    Rig {
        engine,
        console,
        session,
        a,
        b,
        _dir: dir,
    }
}

impl Rig {
    fn step(&mut self, blocks: usize) {
        let mut m = vec![0f32; 1024];
        let mut p = vec![0f32; 1024];
        self.console.poll();
        for _ in 0..blocks {
            self.engine.process(&mut m, &mut p);
        }
        self.console.poll();
    }

    fn start_a(&mut self) {
        let a = self.a.clone();
        self.console.load(0, &a);
        let t0 = Instant::now();
        while !self.console.status().decks[0].loaded {
            self.step(1);
            assert!(t0.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(2));
        }
        self.console.send(Command::Play { deck: 0, on: true });
        self.step(4);
    }

    /// Runs until the transition completes (or panics after `max_sec`).
    fn run_until_done(&mut self, max_sec: f64) {
        let blocks = (max_sec * 44_100.0 / 512.0) as usize;
        for i in 0..blocks {
            self.step(8);
            if self.console.status().transition.completed > 0 {
                return;
            }
            if i % 64 == 0 {
                // Let loader threads deliver.
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        panic!(
            "transition did not complete: {:?}",
            self.console.status().transition
        );
    }
}

#[test]
fn ai_schedules_and_the_engine_executes() {
    let mut r = rig("ai");
    r.start_a();
    assert!(r.session.live().unwrap().needs_plan);
    let model = Arc::new(MockModel::new([
        MockModel::tool_call("get_live_state", json!({})),
        MockModel::tool_call(
            "schedule_transition",
            json!({"track": r.b.0, "at_bar": 24, "length_bars": 8, "style": "bass_swap",
                   "rationale": "outro over intro"}),
        ),
    ]));
    let session = r.session.clone();
    let m2 = model.clone();
    let planner = std::thread::spawn(move || {
        tokio::runtime::Runtime::new().unwrap().block_on(plan_next(
            &session,
            m2,
            AgentConfig::default(),
        ))
    });
    // Keep the console answering while the model "thinks", without moving
    // the playhead: on a slow machine the track would otherwise pass the
    // scripted bar 24 (or the autopilot step in) before the plan arrives.
    while !planner.is_finished() {
        r.console.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(planner.join().unwrap().unwrap(), "plan scheduled");
    let live_state = &model.requests()[1].messages;
    assert!(
        live_state
            .last()
            .unwrap()
            .content
            .contains("earliest_at_bar")
    );
    let s = r.console.director().scheduled().unwrap();
    assert_eq!(
        (s.by.as_str(), s.at_bar, s.style),
        ("ai", 24.0, TransitionStyle::BassSwap)
    );
    r.run_until_done(90.0);
    let st = *r.console.status();
    assert!(st.decks[1].playing && !st.decks[0].playing);
    assert_eq!(r.console.director().main_deck(), Some(1));
    assert!(
        r.console
            .director()
            .log
            .iter()
            .any(|l| l.text.contains("done"))
    );
    let live = r.session.live().unwrap();
    assert_eq!(live.played, vec![r.a.0.clone(), r.b.0.clone()]);
}

#[test]
fn autopilot_steps_in_when_the_ai_is_late() {
    let mut r = rig("autopilot");
    r.start_a();
    r.run_until_done(90.0);
    let log: Vec<String> = r
        .console
        .director()
        .log
        .iter()
        .map(|l| l.text.clone())
        .collect();
    assert!(log.iter().any(|l| l.starts_with("autopilot")), "{log:?}");
    assert!(r.console.status().decks[1].playing);
}

#[test]
fn frozen_horizon_and_rejection() {
    let mut r = rig("frozen");
    r.start_a();
    r.step(800); // about 9 s: bar ~4.7
    let rx = r.session.propose(
        TransitionRequest {
            track: r.b.0.clone(),
            at_bar: 6.0,
            length_bars: 8.0,
            style: TransitionStyle::Blend,
            start_bar: None,
            rationale: None,
        },
        "external",
    );
    r.step(1);
    let reply = rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(!reply.ok);
    assert!(reply.errors[0].contains("too soon"), "{:?}", reply.errors);

    let rx = r.session.propose(
        TransitionRequest {
            track: r.b.0.clone(),
            at_bar: 24.0,
            length_bars: 8.0,
            style: TransitionStyle::Blend,
            start_bar: None,
            rationale: Some("test".into()),
        },
        "external",
    );
    r.step(1);
    assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap().ok);
    let t0 = Instant::now();
    while r.console.status().transition.state != TransitionState::Armed {
        r.step(2);
        std::thread::sleep(Duration::from_millis(2));
        assert!(t0.elapsed() < Duration::from_secs(20));
    }
    r.console.reject_transition();
    r.step(2);
    assert_eq!(r.console.status().transition.state, TransitionState::Idle);
    assert!(
        r.console
            .director()
            .log
            .last()
            .unwrap()
            .text
            .starts_with("rejected")
    );
}

/// Needs a local Ollama with the default model:
/// `cargo test -p redj-ai --test live -- --ignored --nocapture`
#[test]
#[ignore]
fn real_ollama_plans_live() {
    let mut r = rig("ollama");
    r.start_a();
    let session = r.session.clone();
    let planner = std::thread::spawn(move || {
        tokio::runtime::Runtime::new().unwrap().block_on(plan_next(
            &session,
            redj_ai::OllamaClient::new(redj_ai::DEFAULT_URL, Duration::from_secs(900)),
            AgentConfig::default(),
        ))
    });
    let t0 = Instant::now();
    // The console keeps answering; audio time is frozen while the model
    // thinks, so the deadline cannot pass in this test.
    while !planner.is_finished() {
        r.console.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
    let res = planner.join().unwrap();
    println!("planning took {:.0} s: {res:?}", t0.elapsed().as_secs_f64());
    for e in r.session.events() {
        println!("event: {}", e.what);
    }
    let s = r
        .console
        .director()
        .scheduled()
        .expect("a transition was scheduled")
        .clone();
    println!(
        "scheduled {} at bar {} ({:?}) by {}: {:?}",
        s.name, s.at_bar, s.style, s.by, s.rationale
    );
    assert_eq!(s.by, "ai");
    r.run_until_done(90.0);
}
