//! MCP server exposing the console to AI clients over stdio and Streamable
//! HTTP.
//!
//! This is the only AI-facing interface: the built-in Ollama host talks to
//! it in-process through an MCP client, so local models and external
//! clients (Claude Code, Claude Desktop) see exactly the same tools.

use std::sync::Arc;

use redj_control::live::TransitionRequest;
use redj_control::{AutoPlanOptions, Session};
use redj_core::TrackId;
use redj_plan::{MixPlan, plan_schema};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

/// Instructions sent to clients at initialization.
pub const INSTRUCTIONS: &str = "\
REDj is a DJ console. You plan DJ sets; the engine plays and renders them.

Positions are always in BARS of the track they refer to (bar 0 = first \
downbeat; 4 beats per bar). Tracks are 4/4 club music; phrases are 8 or 16 \
bars, so transitions should start on multiples of 8.

Workflow: list_tracks -> (get_track for details) -> compatibility to choose \
an order -> optionally suggest_plan as a starting point -> submit_plan. If \
submit_plan returns errors, fix exactly what each error's path and hint say \
and submit again.

A plan lists entries in playing order. Every entry after the first has a \
transition: at_bar is a bar of the PREVIOUS track (usually where its mix_out \
range starts), length_bars is typically 16 (8..32), and the incoming track \
starts at its start_bar (usually its mix_in start, 0). Styles: bass_swap \
(default choice), eq_swap, blend, filter (hides key clashes), echo_out \
(for tempo changes over 6%: use length_bars 1 and tempo \"incoming\"), cut. \
Prefer tempo changes under 6% and Camelot key distance 0-1. Add a short \
rationale to each transition.

LIVE mode (a console is playing): call get_live_state, pick the next track \
(never one listed in `played`) and call schedule_transition with at_bar, a \
bar of the PLAYING track, >= earliest_at_bar, before deadline_bar is reached. \
suggested_at_bar and suggested_length_bars (the playing track's outro) are \
the safe default.";

/// Accepts the plan as a JSON object or as a string containing JSON (some
/// models send the latter).
fn plan_or_string<'de, D: Deserializer<'de>>(d: D) -> Result<MixPlan, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    let v = match v {
        serde_json::Value::String(s) => {
            serde_json::from_str(&s).map_err(serde::de::Error::custom)?
        }
        other => other,
    };
    serde_json::from_value(v).map_err(serde::de::Error::custom)
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TrackArgs {
    /// Track id (or a unique prefix of at least 4 characters).
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CompatArgs {
    /// Track ids in the order to evaluate.
    pub ids: Vec<String>,
    /// Compare every ordered pair instead of consecutive ids only.
    #[serde(default)]
    pub all_pairs: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlanArgs {
    /// The complete mix plan.
    #[serde(deserialize_with = "plan_or_string")]
    #[schemars(with = "MixPlan")]
    pub plan: MixPlan,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SuggestArgs {
    /// Use exactly these tracks in this order (otherwise the planner
    /// chooses the order).
    #[serde(default)]
    pub ids: Option<Vec<String>>,
    /// Maximum number of tracks.
    #[serde(default)]
    pub max_tracks: Option<usize>,
    /// Approximate length of the set in minutes.
    #[serde(default)]
    pub target_minutes: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RenderArgs {
    /// Output file name (written to the session's output folder).
    pub file_name: String,
    /// Also write an MP3.
    #[serde(default)]
    pub mp3: bool,
}

#[derive(Serialize)]
struct RenderSummary {
    wav: String,
    mp3: Option<String>,
    minutes: f64,
    integrated_lufs: f64,
    true_peak_dbtp: f64,
}

fn json<T: Serialize>(v: &T) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![ContentBlock::json(v)?]))
}

fn tool_error(msg: impl Into<String>) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::error(vec![ContentBlock::text(msg.into())]))
}

/// The REDj MCP server.
#[derive(Debug, Clone)]
pub struct RedjServer {
    session: Arc<Session>,
    /// Who is calling: "ai" for the built-in director, "external" otherwise.
    label: &'static str,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl RedjServer {
    pub fn new(session: Arc<Session>) -> Self {
        Self {
            session,
            label: "external",
            tool_router: Self::tool_router(),
        }
    }

    /// Marks requests as coming from the built-in AI director.
    pub fn for_builtin_ai(mut self) -> Self {
        self.label = "ai";
        self
    }

    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }

    #[tool(
        description = "List all analyzed tracks: id, name, bpm, Camelot key, length in bars and minutes, mean energy (0..1), and the bar ranges suited to mix in (intro) and mix out (outro)."
    )]
    async fn list_tracks(&self) -> Result<CallToolResult, McpError> {
        json(&self.session.summaries())
    }

    #[tool(
        description = "Full analysis of one track: sections with bar ranges and energy, energy per 8-bar phrase, loudness, key confidence."
    )]
    async fn get_track(
        &self,
        Parameters(a): Parameters<TrackArgs>,
    ) -> Result<CallToolResult, McpError> {
        match self.session.detail(&a.id) {
            Some(d) => json(&d),
            None => tool_error(format!(
                "unknown track id {:?}; use ids from list_tracks",
                a.id
            )),
        }
    }

    #[tool(
        description = "How well tracks follow each other: tempo change in percent, Camelot key distance, energy change and a 0..1 score, for consecutive ids (or all ordered pairs)."
    )]
    async fn compatibility(
        &self,
        Parameters(a): Parameters<CompatArgs>,
    ) -> Result<CallToolResult, McpError> {
        match self.session.compatibility(&a.ids, a.all_pairs) {
            Ok(c) => json(&c),
            Err(e) => tool_error(e),
        }
    }

    #[tool(
        description = "A valid plan from REDj's built-in planner, as a starting point you may modify before submitting."
    )]
    async fn suggest_plan(
        &self,
        Parameters(a): Parameters<SuggestArgs>,
    ) -> Result<CallToolResult, McpError> {
        let opts = AutoPlanOptions {
            max_tracks: a.max_tracks,
            target_minutes: a.target_minutes,
            ..AutoPlanOptions::default()
        };
        let plan = match a.ids {
            Some(ids) => {
                let lib = self.session.library();
                let mut order = Vec::new();
                for id in &ids {
                    match lib.resolve(id) {
                        Some(t) => order.push(t.id.clone()),
                        None => return tool_error(format!("unknown track id {id:?}")),
                    }
                }
                redj_control::autoplan::plan_for_order(&lib, &order, &opts)
            }
            None => self.session.suggest(&opts),
        };
        json(&plan)
    }

    #[tool(
        description = "Check a plan without accepting it. Returns ok, errors and warnings (each with a JSON path and a hint), the set length and the tracklist."
    )]
    async fn validate_plan(
        &self,
        Parameters(a): Parameters<PlanArgs>,
    ) -> Result<CallToolResult, McpError> {
        json(&self.session.check(&a.plan))
    }

    #[tool(
        description = "Submit the final plan. If it has no errors it becomes the session's plan (ok: true). Otherwise fix the reported errors and submit again."
    )]
    async fn submit_plan(
        &self,
        Parameters(a): Parameters<PlanArgs>,
    ) -> Result<CallToolResult, McpError> {
        json(&self.session.submit(a.plan))
    }

    #[tool(description = "The currently accepted plan, if any.")]
    async fn get_plan(&self) -> Result<CallToolResult, McpError> {
        match self.session.accepted_plan() {
            Some(p) => json(&p),
            None => tool_error("no plan has been accepted yet"),
        }
    }

    #[tool(description = "JSON Schema of mix plans.")]
    async fn plan_schema(&self) -> Result<CallToolResult, McpError> {
        json(&plan_schema())
    }

    #[tool(
        description = "Live console state: decks with the playing track, current bar, tempo, end and outro bars; tracks already played; the next scheduled transition; the earliest bar a transition may start (frozen horizon) and the deadline after which the autopilot mixes on its own."
    )]
    async fn get_live_state(&self) -> Result<CallToolResult, McpError> {
        match self.session.live() {
            Some(s) => json(&s),
            None => tool_error("no live console is running (offline session)"),
        }
    }

    #[tool(
        description = "Schedule the next transition from the playing track into another track. Positions are bars of the PLAYING track. Replaces a scheduled transition that has not started yet. Returns ok, or errors with fixes."
    )]
    async fn schedule_transition(
        &self,
        Parameters(req): Parameters<TransitionRequest>,
    ) -> Result<CallToolResult, McpError> {
        let rx = self.session.propose(req, self.label);
        let reply =
            tokio::task::spawn_blocking(move || rx.recv_timeout(std::time::Duration::from_secs(5)))
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        match reply {
            Ok(r) => json(&r),
            Err(_) => tool_error("the console did not answer; is the live console running?"),
        }
    }

    #[tool(description = "Cancel the next scheduled (or running) transition.")]
    async fn cancel_transition(&self) -> Result<CallToolResult, McpError> {
        self.session.request_cancel();
        json(&serde_json::json!({ "ok": true }))
    }

    #[tool(
        description = "Render the accepted plan to a WAV file (and optionally MP3) in the session's output folder."
    )]
    async fn render_mix(
        &self,
        Parameters(a): Parameters<RenderArgs>,
    ) -> Result<CallToolResult, McpError> {
        let session = self.session.clone();
        let res = tokio::task::spawn_blocking(move || session.render_accepted(&a.file_name, a.mp3))
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        match res {
            Ok(r) => json(&RenderSummary {
                wav: r.wav.display().to_string(),
                mp3: r.mp3.map(|p| p.display().to_string()),
                minutes: (r.duration_sec / 6.0).round() / 10.0,
                integrated_lufs: (r.integrated_lufs * 10.0).round() / 10.0,
                true_peak_dbtp: (r.true_peak_dbtp * 10.0).round() / 10.0,
            }),
            Err(e) => tool_error(e),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for RedjServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("redj", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

/// Serves the session over stdin/stdout until the client disconnects.
pub async fn serve_stdio(
    session: Arc<Session>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let service = RedjServer::new(session)
        .serve(rmcp::transport::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}

/// Starts a server on an in-memory pipe and returns the client end, for
/// the in-process AI host.
pub fn serve_in_process(session: Arc<Session>) -> tokio::io::DuplexStream {
    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    tokio::spawn(async move {
        match RedjServer::new(session)
            .for_builtin_ai()
            .serve(server_io)
            .await
        {
            Ok(s) => {
                let _ = s.waiting().await;
            }
            Err(e) => tracing::error!("in-process MCP server failed: {e}"),
        }
    });
    client_io
}

/// Whether a track id exists (helper for hosts).
pub fn has_track(session: &Session, id: &str) -> bool {
    session.has_track(&TrackId(id.to_owned()))
}

/// Serves the session over MCP Streamable HTTP at
/// `http://127.0.0.1:<port>/mcp` on a background thread, so external AI
/// clients can join a running console. Loopback only.
pub fn serve_http(
    session: Arc<Session>,
    port: u16,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService,
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    // Bind synchronously so errors (port in use) reach the caller.
    let std_listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
    std_listener.set_nonblocking(true)?;
    std::thread::Builder::new()
        .name("redj-mcp-http".into())
        .spawn(move || {
            rt.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(std_listener) {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::error!("MCP HTTP server: {e}");
                        return;
                    }
                };
                let service: StreamableHttpService<RedjServer, LocalSessionManager> =
                    StreamableHttpService::new(
                        move || Ok(RedjServer::new(session.clone())),
                        Default::default(),
                        StreamableHttpServerConfig::default(),
                    );
                let router = axum::Router::new().nest_service("/mcp", service);
                if let Err(e) = axum::serve(listener, router).await {
                    tracing::error!("MCP HTTP server stopped: {e}");
                }
            });
        })
}
