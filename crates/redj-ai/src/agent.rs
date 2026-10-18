//! Tool-calling loop between a chat model and the REDj MCP server.

use std::time::{Duration, Instant};

use rmcp::RoleClient;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ContentBlock};
use rmcp::service::RunningService;

use crate::model::{
    AiError, ChatModel, ChatOptions, ChatRequest, FunctionSpec, Message, Role, ToolSpec,
};

/// Agent settings.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub model: String,
    /// Model requests per run before giving up.
    pub max_turns: usize,
    /// Thinking mode, for models that support it (`None` = leave the
    /// model's default).
    pub think: Option<bool>,
    /// Ollama context window. The default (4096) is far too small for
    /// tool schemas plus a library listing.
    pub num_ctx: u32,
    pub temperature: f32,
    /// How long Ollama keeps the model loaded between requests.
    pub keep_alive: String,
    /// Retries of a failed model request.
    pub retries: u32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: "qwen3.6:35b-a3b-q4_K_M".into(),
            max_turns: 16,
            think: Some(false),
            num_ctx: 32_768,
            temperature: 0.2,
            keep_alive: "30m".into(),
            retries: 1,
        }
    }
}

/// Progress of an agent run.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// A model request starts.
    Request {
        turn: usize,
    },
    /// The model answered (timing and token counts).
    Response {
        turn: usize,
        secs: f64,
        prompt_tokens: u64,
        output_tokens: u64,
    },
    Thinking(String),
    Text(String),
    ToolCall {
        name: String,
        arguments: String,
    },
    ToolResult {
        name: String,
        ok: bool,
        preview: String,
    },
    Retry {
        turn: usize,
        error: String,
    },
}

/// Result of an agent run.
#[derive(Debug, Clone)]
pub struct AgentOutcome {
    /// The model's final message.
    pub text: String,
    pub turns: usize,
    pub tool_calls: usize,
    /// True if the run ended because the stop condition was met.
    pub stopped: bool,
}

/// A chat model connected to an MCP server.
pub struct Agent<M: ChatModel> {
    model: M,
    client: RunningService<RoleClient, ()>,
    tools: Vec<ToolSpec>,
    system: String,
    cfg: AgentConfig,
    history: Vec<Message>,
}

impl<M: ChatModel> std::fmt::Debug for Agent<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("tools", &self.tools.len())
            .field("cfg", &self.cfg)
            .finish()
    }
}

fn preview(s: &str) -> String {
    const MAX: usize = 300;
    if s.len() <= MAX {
        s.to_owned()
    } else {
        let mut end = MAX;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

impl<M: ChatModel> Agent<M> {
    /// Connects to an MCP server over `transport` and prepares the system
    /// prompt (`role` followed by the server's instructions).
    pub async fn connect(
        model: M,
        transport: tokio::io::DuplexStream,
        role: &str,
        cfg: AgentConfig,
    ) -> Result<Self, AiError> {
        let client = ().serve(transport).await.map_err(|e| AiError::Mcp(e.to_string()))?;
        let tools = client
            .list_all_tools()
            .await
            .map_err(|e| AiError::Mcp(e.to_string()))?
            .into_iter()
            .map(|t| ToolSpec {
                kind: "function".into(),
                function: FunctionSpec {
                    name: t.name.to_string(),
                    description: t.description.map(|d| d.to_string()).unwrap_or_default(),
                    parameters: serde_json::Value::Object(t.input_schema.as_ref().clone()),
                },
            })
            .collect();
        let instructions = client
            .peer_info()
            .and_then(|i| i.instructions.clone())
            .unwrap_or_default();
        let system = format!("{role}\n\n{instructions}");
        Ok(Self {
            model,
            client,
            tools,
            system,
            cfg,
            history: Vec::new(),
        })
    }

    pub fn tools(&self) -> &[ToolSpec] {
        &self.tools
    }

    /// Keeps only the tools whose names pass `keep` (smaller prompts for
    /// slow models).
    pub fn retain_tools(&mut self, keep: impl Fn(&str) -> bool) {
        self.tools.retain(|t| keep(&t.function.name));
    }

    pub fn history(&self) -> &[Message] {
        &self.history
    }

    async fn call_tool(
        &self,
        name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> (bool, String) {
        let params = CallToolRequestParams::new(name.to_owned()).with_arguments(args);
        match self.client.call_tool(params).await {
            Ok(r) => {
                let text = r
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        ContentBlock::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                (r.is_error != Some(true), text)
            }
            // Bad arguments etc.: tell the model so it can correct itself.
            Err(e) => (false, format!("tool call failed: {e}")),
        }
    }

    async fn request(
        &self,
        turn: usize,
        on_event: &mut impl FnMut(AgentEvent),
    ) -> Result<Message, AiError> {
        let mut messages = Vec::with_capacity(self.history.len() + 1);
        messages.push(Message::new(Role::System, self.system.clone()));
        messages.extend(self.history.iter().cloned());
        let req = ChatRequest {
            model: self.cfg.model.clone(),
            messages,
            tools: self.tools.clone(),
            stream: false,
            think: self.cfg.think,
            options: ChatOptions {
                num_ctx: self.cfg.num_ctx,
                temperature: self.cfg.temperature,
                seed: Some(7),
            },
            keep_alive: self.cfg.keep_alive.clone(),
        };
        let mut attempt = 0;
        loop {
            on_event(AgentEvent::Request { turn });
            let t0 = Instant::now();
            match self.model.chat(req.clone()).await {
                Ok(r) => {
                    on_event(AgentEvent::Response {
                        turn,
                        secs: t0.elapsed().as_secs_f64(),
                        prompt_tokens: r.prompt_eval_count,
                        output_tokens: r.eval_count,
                    });
                    return Ok(r.message);
                }
                Err(e @ AiError::Unreachable { .. }) => return Err(e),
                Err(e) if attempt < self.cfg.retries => {
                    attempt += 1;
                    on_event(AgentEvent::Retry {
                        turn,
                        error: e.to_string(),
                    });
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Runs the conversation for `task` until the model stops calling
    /// tools, `stop()` becomes true after a tool call, or `max_turns` is
    /// reached.
    pub async fn run(
        &mut self,
        task: &str,
        mut on_event: impl FnMut(AgentEvent),
        stop: impl Fn() -> bool,
    ) -> Result<AgentOutcome, AiError> {
        self.history.push(Message::new(Role::User, task));
        let mut tool_calls = 0;
        for turn in 1..=self.cfg.max_turns {
            let msg = self.request(turn, &mut on_event).await?;
            if let Some(t) = msg.thinking.as_ref().filter(|t| !t.trim().is_empty()) {
                on_event(AgentEvent::Thinking(t.clone()));
            }
            if !msg.content.trim().is_empty() {
                on_event(AgentEvent::Text(msg.content.clone()));
            }
            let calls = msg.tool_calls.clone();
            // Thinking is not sent back: it only costs context.
            self.history.push(Message {
                thinking: None,
                ..msg.clone()
            });
            if calls.is_empty() {
                return Ok(AgentOutcome {
                    text: msg.content,
                    turns: turn,
                    tool_calls,
                    stopped: false,
                });
            }
            for call in calls {
                tool_calls += 1;
                let args = call.function.arguments_object();
                on_event(AgentEvent::ToolCall {
                    name: call.function.name.clone(),
                    arguments: preview(&serde_json::Value::Object(args.clone()).to_string()),
                });
                let (ok, text) = self.call_tool(&call.function.name, args).await;
                on_event(AgentEvent::ToolResult {
                    name: call.function.name.clone(),
                    ok,
                    preview: preview(&text),
                });
                self.history
                    .push(Message::tool_result(call.function.name.clone(), text));
            }
            if stop() {
                return Ok(AgentOutcome {
                    text: String::new(),
                    turns: turn,
                    tool_calls,
                    stopped: true,
                });
            }
        }
        Err(AiError::TooManyTurns(self.cfg.max_turns))
    }

    /// Closes the MCP connection.
    pub async fn close(self) {
        let _ = self.client.cancel().await;
    }
}
