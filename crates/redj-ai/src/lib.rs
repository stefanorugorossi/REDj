//! AI host: Ollama client, embedded MCP client, tool-calling loop, mock
//! LLM.
//!
//! The host never touches the engine directly: it connects to the REDj MCP
//! server in-process, exactly like an external client would.

mod agent;
mod live;
mod mock;
mod model;
mod ollama;

use std::sync::Arc;

pub use agent::{Agent, AgentConfig, AgentEvent, AgentOutcome};
pub use live::{LIVE_DJ_ROLE, LiveDirector, plan_next, spawn_live_director};
pub use mock::MockModel;
pub use model::*;
pub use ollama::{DEFAULT_URL, ModelInfo, OllamaClient};
use redj_control::Session;

/// Role prompt for building an offline set.
pub const OFFLINE_DJ_ROLE: &str = "\
You are the AI DJ of REDj. Build the best possible DJ set from the user's \
library using the tools, then submit it with submit_plan. Think like a club \
DJ: smooth tempo and key progression, energy that builds, transitions in \
the outgoing track's outro over the incoming track's intro, on phrase \
boundaries. Do not invent track ids: use the ids returned by list_tracks. \
Call one tool at a time and keep any text very short.";

/// What to ask for when planning a set.
#[derive(Debug, Clone, Default)]
pub struct SetRequest {
    /// Approximate length in minutes.
    pub minutes: Option<f64>,
    /// Maximum number of tracks.
    pub max_tracks: Option<usize>,
    /// Free-form wishes ("dark and driving", "start with track X").
    pub notes: Option<String>,
}

impl SetRequest {
    pub fn task(&self) -> String {
        let mut t = String::from("Build a DJ set from my library");
        if let Some(m) = self.minutes {
            t.push_str(&format!(", about {m:.0} minutes long"));
        }
        if let Some(n) = self.max_tracks {
            t.push_str(&format!(", with at most {n} tracks"));
        }
        t.push('.');
        if let Some(n) = &self.notes {
            t.push(' ');
            t.push_str(n);
        }
        t.push_str(" Start by calling list_tracks, and finish by calling submit_plan.");
        t
    }
}

/// Lets a chat model plan a set for `session` through the MCP tools. Ends
/// as soon as a plan is accepted.
pub async fn plan_set<M: ChatModel>(
    session: Arc<Session>,
    model: M,
    cfg: AgentConfig,
    request: &SetRequest,
    on_event: impl FnMut(AgentEvent),
) -> Result<AgentOutcome, AiError> {
    let io = redj_mcp::serve_in_process(session.clone());
    let mut agent = Agent::connect(model, io, OFFLINE_DJ_ROLE, cfg).await?;
    // Live-only and rendering tools are not useful here; fewer tools mean a
    // shorter prompt on every request.
    agent.retain_tools(|n| {
        matches!(
            n,
            "list_tracks"
                | "get_track"
                | "compatibility"
                | "suggest_plan"
                | "validate_plan"
                | "submit_plan"
        )
    });
    let s = session.clone();
    let outcome = agent
        .run(&request.task(), on_event, move || {
            s.accepted_plan().is_some()
        })
        .await;
    agent.close().await;
    outcome
}
