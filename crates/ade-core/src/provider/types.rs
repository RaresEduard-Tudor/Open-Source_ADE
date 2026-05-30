//! Provider-agnostic message and tool types.
//!
//! Every adapter translates to/from these. The shape is deliberately close to
//! the OpenAI chat model (flat messages + tool calls) because it round-trips
//! cleanly to Anthropic blocks and Gemini parts.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    /// A tool result being fed back to the model.
    Tool,
}

/// A model-requested tool invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-assigned id, echoed back with the result. Synthesised when the
    /// provider does not supply one (Gemini).
    pub id: String,
    pub name: String,
    /// Parsed JSON arguments.
    pub arguments: Value,
}

/// One conversation message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default)]
    pub content: String,
    /// Present on assistant turns that call tools.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Set on `Role::Tool` messages — which call this result answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: Role::User, content: content.into(), tool_calls: vec![], tool_call_id: None }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: content.into(), tool_calls: vec![], tool_call_id: None }
    }
    pub fn tool_result(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: vec![],
            tool_call_id: Some(id.into()),
        }
    }
}

/// A tool advertised to the model (JSON-Schema parameters).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema object describing the arguments.
    pub parameters: Value,
}

/// A request to a provider.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system: None,
            messages: vec![],
            tools: vec![],
            max_tokens: 4096,
            temperature: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    Other,
}

/// A provider's reply.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish: FinishReason,
    pub usage: Usage,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

impl ChatResponse {
    /// Convert this reply into the assistant message to append to history.
    pub fn to_message(&self) -> Message {
        Message {
            role: Role::Assistant,
            content: self.content.clone(),
            tool_calls: self.tool_calls.clone(),
            tool_call_id: None,
        }
    }
}
