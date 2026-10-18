//! Chat types (Ollama wire format) and the model abstraction.

use std::future::Future;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A function call requested by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// Arguments object (some models send a JSON string; see
    /// [`FunctionCall::arguments_object`]).
    #[serde(default)]
    pub arguments: serde_json::Value,
}

impl FunctionCall {
    /// Arguments as a JSON object, parsing string-encoded arguments.
    pub fn arguments_object(&self) -> serde_json::Map<String, serde_json::Value> {
        let v = match &self.arguments {
            serde_json::Value::String(s) => serde_json::from_str(s).unwrap_or_default(),
            other => other.clone(),
        };
        match v {
            serde_json::Value::Object(m) => m,
            _ => serde_json::Map::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub function: FunctionCall,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// For `Role::Tool`: the tool that produced this result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            thinking: None,
            tool_calls: Vec::new(),
            tool_name: None,
        }
    }

    pub fn tool_result(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_name: Some(name.into()),
            ..Self::new(Role::Tool, content)
        }
    }
}

/// A tool offered to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionSpec,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Sampling and context options.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatOptions {
    pub num_ctx: u32,
    pub temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u32>,
}

/// A chat request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSpec>,
    pub stream: bool,
    /// Thinking on/off; omitted for models without the capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub think: Option<bool>,
    pub options: ChatOptions,
    pub keep_alive: String,
}

/// A chat response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    pub message: Message,
    #[serde(default)]
    pub done: bool,
    /// Prompt tokens evaluated (not cached).
    #[serde(default)]
    pub prompt_eval_count: u64,
    /// Tokens generated.
    #[serde(default)]
    pub eval_count: u64,
    /// Total time in nanoseconds.
    #[serde(default)]
    pub total_duration: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("cannot reach Ollama at {url}: {msg}")]
    Unreachable { url: String, msg: String },
    #[error("Ollama error: {0}")]
    Ollama(String),
    #[error("model request timed out after {0} s")]
    Timeout(u64),
    #[error("MCP error: {0}")]
    Mcp(String),
    #[error("the model did not finish within {0} turns")]
    TooManyTurns(usize),
    #[error("{0}")]
    Other(String),
}

/// Something that answers chat requests: Ollama, or a mock in tests.
pub trait ChatModel: Send + Sync {
    fn chat(&self, req: ChatRequest) -> impl Future<Output = Result<ChatResponse, AiError>> + Send;
}
