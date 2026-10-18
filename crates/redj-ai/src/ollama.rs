//! Ollama HTTP client.

use std::time::Duration;

use serde::Deserialize;

use crate::model::{AiError, ChatModel, ChatRequest, ChatResponse};

pub const DEFAULT_URL: &str = "http://127.0.0.1:11434";

/// An installed model.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    pub name: String,
    pub size_bytes: u64,
    /// From `/api/show`: e.g. "completion", "tools", "thinking", "vision".
    pub capabilities: Vec<String>,
}

impl ModelInfo {
    pub fn supports_tools(&self) -> bool {
        self.capabilities.iter().any(|c| c == "tools")
    }

    pub fn supports_thinking(&self) -> bool {
        self.capabilities.iter().any(|c| c == "thinking")
    }
}

#[derive(Deserialize)]
struct Tags {
    models: Vec<TagModel>,
}

#[derive(Deserialize)]
struct TagModel {
    name: String,
    #[serde(default)]
    size: u64,
}

#[derive(Deserialize)]
struct Show {
    #[serde(default)]
    capabilities: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct OllamaClient {
    base: String,
    http: reqwest::Client,
    timeout: Duration,
}

impl OllamaClient {
    /// `timeout` bounds each chat request (slow CPU models need minutes).
    pub fn new(base_url: impl Into<String>, timeout: Duration) -> Self {
        Self {
            base: base_url.into().trim_end_matches('/').to_owned(),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .build()
                .expect("http client"),
            timeout,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn unreachable(&self, e: impl std::fmt::Display) -> AiError {
        AiError::Unreachable {
            url: self.base.clone(),
            msg: e.to_string(),
        }
    }

    /// Installed models with their capabilities.
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        let tags: Tags = self
            .http
            .get(format!("{}/api/tags", self.base))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| self.unreachable(e))?
            .json()
            .await
            .map_err(|e| AiError::Ollama(e.to_string()))?;
        let mut out = Vec::with_capacity(tags.models.len());
        for m in tags.models {
            let capabilities = self.capabilities(&m.name).await.unwrap_or_default();
            out.push(ModelInfo {
                name: m.name,
                size_bytes: m.size,
                capabilities,
            });
        }
        Ok(out)
    }

    /// Capabilities of one model (`/api/show`).
    pub async fn capabilities(&self, model: &str) -> Result<Vec<String>, AiError> {
        let resp = self
            .http
            .post(format!("{}/api/show", self.base))
            .timeout(Duration::from_secs(30))
            .json(&serde_json::json!({ "model": model }))
            .send()
            .await
            .map_err(|e| self.unreachable(e))?;
        if !resp.status().is_success() {
            return Err(AiError::Ollama(format!("model {model:?} not found")));
        }
        let show: Show = resp
            .json()
            .await
            .map_err(|e| AiError::Ollama(e.to_string()))?;
        Ok(show.capabilities)
    }
}

impl ChatModel for OllamaClient {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, AiError> {
        let resp = self
            .http
            .post(format!("{}/api/chat", self.base))
            .timeout(self.timeout)
            .json(&req)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    AiError::Timeout(self.timeout.as_secs())
                } else {
                    self.unreachable(e)
                }
            })?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            if e.is_timeout() {
                AiError::Timeout(self.timeout.as_secs())
            } else {
                AiError::Ollama(e.to_string())
            }
        })?;
        if !status.is_success() {
            let msg = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
                .unwrap_or(text);
            return Err(AiError::Ollama(format!("{status}: {msg}")));
        }
        serde_json::from_str(&text).map_err(|e| AiError::Ollama(format!("bad response: {e}")))
    }
}
