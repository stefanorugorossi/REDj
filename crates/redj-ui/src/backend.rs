//! Audio backends: a cpal device, or a paced "null" output when no device
//! is available (the console still runs, silently).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use redj_engine::driver::{self, AudioOutput, Routing};
use redj_engine::{EngineHandle, LiveConfig, LiveEngine};

pub enum Backend {
    Device(AudioOutput),
    Null { stop: Arc<AtomicBool> },
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

impl Backend {
    pub fn describe(&self) -> String {
        match self {
            Backend::Device(o) => format!(
                "{} ({} ch, {} Hz)",
                o.device.name, o.device.channels, o.device.sample_rate
            ),
            Backend::Null { .. } => "no audio device".into(),
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        if let Backend::Null { stop } = self {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

/// Starts the engine on the given device, falling back to the null output.
pub fn start(
    device: Option<&str>,
    routing: Routing,
    decks: usize,
) -> (Backend, EngineHandle, u32, Option<String>) {
    let mut handle = None;
    let mut rate = 44_100;
    let res = driver::start(device, routing, |sr| {
        rate = sr;
        let (e, h) = LiveEngine::new(LiveConfig {
            sample_rate: sr,
            decks,
            ceiling_db: -1.0,
        });
        handle = Some(h);
        e
    });
    match (res, handle) {
        (Ok(out), Some(h)) => (Backend::Device(out), h, rate, None),
        (res, _) => {
            let err = res.err().map(|e| e.to_string());
            let (mut engine, h) = LiveEngine::new(LiveConfig {
                sample_rate: 44_100,
                decks,
                ceiling_db: -1.0,
            });
            let stop = Arc::new(AtomicBool::new(false));
            let s = stop.clone();
            std::thread::spawn(move || {
                let mut m = vec![0f32; 1024];
                let mut p = vec![0f32; 1024];
                let block = Duration::from_secs_f64(512.0 / 44_100.0);
                let mut next = Instant::now();
                while !s.load(Ordering::Relaxed) {
                    engine.process(&mut m, &mut p);
                    next += block;
                    if let Some(d) = next.checked_duration_since(Instant::now()) {
                        std::thread::sleep(d);
                    }
                }
            });
            (Backend::Null { stop }, h, 44_100, err)
        }
    }
}
