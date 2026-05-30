//! OpenAI-compatible adapter (`/chat/completions`).
//!
//! Covers OpenAI, Deepseek, Mimo, local servers, and most OSS endpoints.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::types::*;
use super::{sse, Provider, TextSink};
use crate::config::ProviderConfig;
use crate::error::{Error, Result};

const DEFAULT_BASE: &str = "https://api.openai.com/v1";

pub struct OpenAiAdapter {
    name: String,
    base_url: String,
    model: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl OpenAiAdapter {
    pub fn new(cfg: &ProviderConfig, api_key: Option<String>, client: reqwest::Client) -> Self {
        Self {
            name: cfg.name.clone(),
            base_url: cfg
                .base_url
                .clone()
                .unwrap_or_else(|| DEFAULT_BASE.to_string()),
            model: cfg.model.clone(),
            api_key,
            client,
        }
    }
}

/// Translate a unified request into an OpenAI chat body. Pure — unit tested.
pub(crate) fn build_body(req: &ChatRequest) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    if let Some(sys) = &req.system {
        messages.push(json!({"role": "system", "content": sys}));
    }
    for m in &req.messages {
        match m.role {
            Role::Tool => messages.push(json!({
                "role": "tool",
                "tool_call_id": m.tool_call_id,
                "content": m.content,
            })),
            Role::Assistant if !m.tool_calls.is_empty() => {
                let calls: Vec<Value> = m
                    .tool_calls
                    .iter()
                    .map(|c| {
                        json!({
                            "id": c.id,
                            "type": "function",
                            "function": {
                                "name": c.name,
                                "arguments": c.arguments.to_string(),
                            }
                        })
                    })
                    .collect();
                messages.push(json!({
                    "role": "assistant",
                    "content": if m.content.is_empty() { Value::Null } else { json!(m.content) },
                    "tool_calls": calls,
                }));
            }
            role => {
                let role = match role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => unreachable!(),
                };
                messages.push(json!({"role": role, "content": m.content}));
            }
        }
    }

    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "max_tokens": req.max_tokens,
    });
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect();
        body["tools"] = json!(tools);
    }
    body
}

/// Parse an OpenAI chat response. Pure — unit tested.
pub(crate) fn parse_response(v: &Value) -> Result<ChatResponse> {
    let choice = v["choices"]
        .get(0)
        .ok_or_else(|| Error::Provider("openai: no choices".into()))?;
    let msg = &choice["message"];
    let content = msg["content"].as_str().unwrap_or_default().to_string();

    let mut tool_calls = Vec::new();
    if let Some(calls) = msg["tool_calls"].as_array() {
        for c in calls {
            let args_str = c["function"]["arguments"].as_str().unwrap_or("{}");
            let arguments: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
            tool_calls.push(ToolCall {
                id: c["id"].as_str().unwrap_or_default().to_string(),
                name: c["function"]["name"].as_str().unwrap_or_default().to_string(),
                arguments,
            });
        }
    }

    let finish = match choice["finish_reason"].as_str() {
        Some("stop") => FinishReason::Stop,
        Some("tool_calls") => FinishReason::ToolCalls,
        Some("length") => FinishReason::Length,
        _ if !tool_calls.is_empty() => FinishReason::ToolCalls,
        _ => FinishReason::Other,
    };

    Ok(ChatResponse {
        content,
        tool_calls,
        finish,
        usage: Usage {
            input_tokens: v["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32,
            output_tokens: v["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32,
        },
    })
}

#[async_trait]
impl Provider for OpenAiAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut body = build_body(req);
        body["model"] = serde_json::json!(self.model);
        let mut rb = self.client.post(&url).json(&body);
        if let Some(key) = &self.api_key {
            rb = rb.bearer_auth(key);
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| Error::Provider(format!("openai request: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| Error::Provider(format!("openai body: {e}")))?;
        if !status.is_success() {
            return Err(Error::Provider(format!("openai {status}: {body}")));
        }
        let v: Value = serde_json::from_str(&body)?;
        parse_response(&v)
    }

    async fn stream(&self, req: &ChatRequest, sink: &mut TextSink<'_>) -> Result<ChatResponse> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut body = build_body(req);
        body["model"] = json!(self.model);
        body["stream"] = json!(true);
        body["stream_options"] = json!({"include_usage": true});

        let mut rb = self.client.post(&url).json(&body);
        if let Some(key) = &self.api_key {
            rb = rb.bearer_auth(key);
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| Error::Provider(format!("openai stream request: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Provider(format!("openai {status}: {body}")));
        }

        let mut content = String::new();
        // index -> (id, name, accumulated argument string)
        let mut calls: BTreeMap<u64, (String, String, String)> = BTreeMap::new();
        let mut finish = FinishReason::Stop;
        let mut usage = Usage::default();

        sse::for_each_data(resp, |data| {
            let v: Value = serde_json::from_str(data)
                .map_err(|e| Error::Provider(format!("openai stream json: {e}")))?;
            if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                usage.input_tokens = u["prompt_tokens"].as_u64().unwrap_or(0) as u32;
                usage.output_tokens = u["completion_tokens"].as_u64().unwrap_or(0) as u32;
            }
            let Some(choice) = v["choices"].get(0) else { return Ok(()) };
            let delta = &choice["delta"];
            if let Some(c) = delta["content"].as_str() {
                if !c.is_empty() {
                    content.push_str(c);
                    sink(c);
                }
            }
            if let Some(tcs) = delta["tool_calls"].as_array() {
                for tc in tcs {
                    let idx = tc["index"].as_u64().unwrap_or(0);
                    let entry = calls.entry(idx).or_default();
                    if let Some(id) = tc["id"].as_str() {
                        entry.0 = id.to_string();
                    }
                    if let Some(name) = tc["function"]["name"].as_str() {
                        entry.1.push_str(name);
                    }
                    if let Some(args) = tc["function"]["arguments"].as_str() {
                        entry.2.push_str(args);
                    }
                }
            }
            match choice["finish_reason"].as_str() {
                Some("tool_calls") => finish = FinishReason::ToolCalls,
                Some("length") => finish = FinishReason::Length,
                Some("stop") => finish = FinishReason::Stop,
                _ => {}
            }
            Ok(())
        })
        .await?;

        let tool_calls: Vec<ToolCall> = calls
            .into_values()
            .map(|(id, name, args)| ToolCall {
                id,
                name,
                arguments: serde_json::from_str(&args).unwrap_or(json!({})),
            })
            .collect();
        if !tool_calls.is_empty() {
            finish = FinishReason::ToolCalls;
        }

        Ok(ChatResponse { content, tool_calls, finish, usage })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_includes_tools_and_tool_results() {
        let mut req = ChatRequest::new("gpt");
        req.system = Some("be terse".into());
        req.messages = vec![
            Message::user("hi"),
            Message {
                role: Role::Assistant,
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "a.txt"}),
                }],
                tool_call_id: None,
            },
            Message::tool_result("call_1", "hello"),
        ];
        req.tools = vec![ToolSpec {
            name: "read_file".into(),
            description: "read".into(),
            parameters: json!({"type": "object"}),
        }];
        let body = build_body(&req);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][2]["tool_calls"][0]["id"], "call_1");
        assert_eq!(body["messages"][3]["role"], "tool");
        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
    }

    #[test]
    fn parse_tool_call_response() {
        let v = json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_9",
                        "function": {"name": "run_shell", "arguments": "{\"cmd\":\"ls\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        });
        let r = parse_response(&v).unwrap();
        assert_eq!(r.finish, FinishReason::ToolCalls);
        assert_eq!(r.tool_calls[0].name, "run_shell");
        assert_eq!(r.tool_calls[0].arguments["cmd"], "ls");
        assert_eq!(r.usage.input_tokens, 10);
    }
}
