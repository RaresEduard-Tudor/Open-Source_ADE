//! Anthropic Messages API adapter.
//!
//! Anthropic has only user/assistant roles; tool calls are `tool_use` blocks on
//! assistant turns and results are `tool_result` blocks inside a *user* turn.
//! Consecutive tool results coalesce into one user message.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::types::*;
use super::Provider;
use crate::config::ProviderConfig;
use crate::error::{Error, Result};

const DEFAULT_BASE: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

pub struct AnthropicAdapter {
    name: String,
    base_url: String,
    model: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl AnthropicAdapter {
    pub fn new(cfg: &ProviderConfig, api_key: Option<String>, client: reqwest::Client) -> Self {
        Self {
            name: cfg.name.clone(),
            base_url: cfg.base_url.clone().unwrap_or_else(|| DEFAULT_BASE.to_string()),
            model: cfg.model.clone(),
            api_key,
            client,
        }
    }
}

pub(crate) fn build_body(req: &ChatRequest) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    // Buffer of pending tool_result blocks to flush as one user message.
    let mut pending_results: Vec<Value> = Vec::new();

    let flush_results = |messages: &mut Vec<Value>, pending: &mut Vec<Value>| {
        if !pending.is_empty() {
            messages.push(json!({"role": "user", "content": std::mem::take(pending)}));
        }
    };

    for m in &req.messages {
        match m.role {
            Role::Tool => {
                pending_results.push(json!({
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id,
                    "content": m.content,
                }));
            }
            Role::User => {
                flush_results(&mut messages, &mut pending_results);
                messages.push(json!({
                    "role": "user",
                    "content": [{"type": "text", "text": m.content}],
                }));
            }
            Role::Assistant => {
                flush_results(&mut messages, &mut pending_results);
                let mut blocks: Vec<Value> = Vec::new();
                if !m.content.is_empty() {
                    blocks.push(json!({"type": "text", "text": m.content}));
                }
                for c in &m.tool_calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": c.id,
                        "name": c.name,
                        "input": c.arguments,
                    }));
                }
                messages.push(json!({"role": "assistant", "content": blocks}));
            }
            // A System message in the list is folded into the system field below;
            // ignore here (system is normally passed via req.system).
            Role::System => {}
        }
    }
    flush_results(&mut messages, &mut pending_results);

    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "messages": messages,
    });
    if let Some(sys) = &req.system {
        body["system"] = json!(sys);
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect();
        body["tools"] = json!(tools);
    }
    body
}

pub(crate) fn parse_response(v: &Value) -> Result<ChatResponse> {
    let blocks = v["content"]
        .as_array()
        .ok_or_else(|| Error::Provider("anthropic: missing content".into()))?;

    let mut content = String::new();
    let mut tool_calls = Vec::new();
    for b in blocks {
        match b["type"].as_str() {
            Some("text") => content.push_str(b["text"].as_str().unwrap_or_default()),
            Some("tool_use") => tool_calls.push(ToolCall {
                id: b["id"].as_str().unwrap_or_default().to_string(),
                name: b["name"].as_str().unwrap_or_default().to_string(),
                arguments: b["input"].clone(),
            }),
            _ => {}
        }
    }

    let finish = match v["stop_reason"].as_str() {
        Some("end_turn") | Some("stop_sequence") => FinishReason::Stop,
        Some("tool_use") => FinishReason::ToolCalls,
        Some("max_tokens") => FinishReason::Length,
        _ if !tool_calls.is_empty() => FinishReason::ToolCalls,
        _ => FinishReason::Stop,
    };

    Ok(ChatResponse {
        content,
        tool_calls,
        finish,
        usage: Usage {
            input_tokens: v["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32,
            output_tokens: v["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32,
        },
    })
}

#[async_trait]
impl Provider for AnthropicAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let mut body = build_body(req);
        body["model"] = serde_json::json!(self.model);
        let mut rb = self
            .client
            .post(&url)
            .header("anthropic-version", API_VERSION)
            .json(&body);
        if let Some(key) = &self.api_key {
            rb = rb.header("x-api-key", key);
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| Error::Provider(format!("anthropic request: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| Error::Provider(format!("anthropic body: {e}")))?;
        if !status.is_success() {
            return Err(Error::Provider(format!("anthropic {status}: {body}")));
        }
        let v: Value = serde_json::from_str(&body)?;
        parse_response(&v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_results_coalesce_into_user_turn() {
        let mut req = ChatRequest::new("claude");
        req.messages = vec![
            Message::user("hi"),
            Message {
                role: Role::Assistant,
                content: "let me check".into(),
                tool_calls: vec![ToolCall {
                    id: "t1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "a"}),
                }],
                tool_call_id: None,
            },
            Message::tool_result("t1", "data"),
        ];
        let body = build_body(&req);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[1]["content"][0]["type"], "text");
        assert_eq!(msgs[1]["content"][1]["type"], "tool_use");
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
    }

    #[test]
    fn parse_mixed_text_and_tool_use() {
        let v = json!({
            "content": [
                {"type": "text", "text": "ok"},
                {"type": "tool_use", "id": "t2", "name": "edit_file", "input": {"path": "x"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 7, "output_tokens": 3}
        });
        let r = parse_response(&v).unwrap();
        assert_eq!(r.content, "ok");
        assert_eq!(r.finish, FinishReason::ToolCalls);
        assert_eq!(r.tool_calls[0].arguments["path"], "x");
    }
}
