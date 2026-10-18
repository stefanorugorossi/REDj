//! Scripted chat model for tests and demos without Ollama.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::model::{
    AiError, ChatModel, ChatRequest, ChatResponse, FunctionCall, Message, Role, ToolCall,
};

/// Returns scripted responses in order and records every request.
#[derive(Debug, Default)]
pub struct MockModel {
    script: Mutex<VecDeque<ChatResponse>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl MockModel {
    pub fn new(script: impl IntoIterator<Item = ChatResponse>) -> Self {
        Self {
            script: Mutex::new(script.into_iter().collect()),
            requests: Mutex::default(),
        }
    }

    /// A response that calls one tool.
    pub fn tool_call(name: &str, arguments: serde_json::Value) -> ChatResponse {
        ChatResponse {
            message: Message {
                tool_calls: vec![ToolCall {
                    function: FunctionCall {
                        name: name.into(),
                        arguments,
                    },
                }],
                ..Message::new(Role::Assistant, "")
            },
            done: true,
            prompt_eval_count: 0,
            eval_count: 0,
            total_duration: 0,
        }
    }

    /// A plain text response.
    pub fn text(content: &str) -> ChatResponse {
        ChatResponse {
            message: Message::new(Role::Assistant, content),
            done: true,
            prompt_eval_count: 0,
            eval_count: 0,
            total_duration: 0,
        }
    }

    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl ChatModel for MockModel {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, AiError> {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(req);
        self.script
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
            .ok_or_else(|| AiError::Other("mock script exhausted".into()))
    }
}

impl<T: ChatModel> ChatModel for std::sync::Arc<T> {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, AiError> {
        (**self).chat(req).await
    }
}
