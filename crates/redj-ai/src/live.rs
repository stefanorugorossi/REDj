//! The live AI director: whenever the console needs the next transition,
//! asks the model to plan it through the MCP tools, well ahead of time.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use redj_control::Session;

use crate::agent::{Agent, AgentConfig, AgentEvent};
use crate::model::ChatModel;

/// Role prompt for live planning.
pub const LIVE_DJ_ROLE: &str = "\
You are the AI DJ of REDj, playing a live set. The console is playing a \
track; your job is to choose the next track and schedule the transition \
into it with schedule_transition, well before the deadline. Smooth tempo \
and key moves, energy that builds, transitions starting at the playing \
track's outro (mix_out) on phrase boundaries. Never pick a track listed in \
`played`. Use ids from list_tracks. Call one tool at a time; keep any text \
very short.";

/// Tools the live director offers the model.
const LIVE_TOOLS: [&str; 5] = [
    "get_live_state",
    "list_tracks",
    "get_track",
    "compatibility",
    "schedule_transition",
];

/// A running live director; stops when dropped.
#[derive(Debug)]
pub struct LiveDirector {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for LiveDirector {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            // The thread may be waiting on a slow model; do not block the UI.
            drop(t);
        }
    }
}

fn short(e: &AgentEvent) -> Option<String> {
    match e {
        AgentEvent::ToolCall { name, arguments } if name == "schedule_transition" => {
            Some(format!("AI proposes {arguments}"))
        }
        AgentEvent::ToolResult {
            name,
            ok: false,
            preview,
        } => Some(format!("AI: {name} failed: {preview}")),
        AgentEvent::Retry { error, .. } => Some(format!("AI retry: {error}")),
        _ => None,
    }
}

/// Starts the director thread. `model` is cloned per planning round.
pub fn spawn_live_director<M>(session: Arc<Session>, model: M, cfg: AgentConfig) -> LiveDirector
where
    M: ChatModel + Clone + Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    let thread = std::thread::Builder::new()
        .name("redj-ai-director".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    session.log(format!("AI director cannot start: {e}"), None);
                    return;
                }
            };
            rt.block_on(run(session, model, cfg, s));
        })
        .ok();
    LiveDirector { stop, thread }
}

async fn run<M: ChatModel + Clone>(
    session: Arc<Session>,
    model: M,
    cfg: AgentConfig,
    stop: Arc<AtomicBool>,
) {
    let mut backoff = Duration::from_secs(1);
    while !stop.load(Ordering::Relaxed) {
        tokio::time::sleep(backoff).await;
        backoff = Duration::from_secs(1);
        let Some(snap) = session.live() else { continue };
        if !snap.needs_plan {
            continue;
        }
        session.set_ai_busy(true);
        let res = plan_next(&session, model.clone(), cfg.clone()).await;
        session.set_ai_busy(false);
        match res {
            Ok(true) => {}
            Ok(false) => {
                session.log("AI ended without scheduling a transition", None);
                backoff = Duration::from_secs(5);
            }
            Err(e) => {
                session.log(format!("AI error: {e}"), None);
                backoff = Duration::from_secs(10);
            }
        }
    }
}

/// One planning round. Returns whether a transition got scheduled.
pub async fn plan_next<M: ChatModel>(
    session: &Arc<Session>,
    model: M,
    cfg: AgentConfig,
) -> Result<bool, crate::AiError> {
    let io = redj_mcp::serve_in_process(session.clone());
    let mut agent = Agent::connect(model, io, LIVE_DJ_ROLE, cfg).await?;
    agent.retain_tools(|n| LIVE_TOOLS.contains(&n));
    let s = session.clone();
    let log = session.clone();
    let task = "Plan the next transition now: call get_live_state, choose the next track, then schedule_transition.";
    let out = agent
        .run(
            task,
            move |e| {
                if let Some(t) = short(&e) {
                    log.log(t, None);
                }
            },
            move || s.live().is_some_and(|l| l.next.is_some()),
        )
        .await;
    agent.close().await;
    Ok(out?.stopped)
}
