//! Google Gemini `generateContent` adapter.
//!
//! Roles are `user`/`model`; tool calls/results are `functionCall` /
//! `functionResponse` parts. Gemini assigns no call ids, so we synthesise the
//! id from the function name (matched back by name on the response turn).

use async_trait::async_trait;
use serde_json::{json, Value};

use super::types::*;
use super::Provider;
use crate::config::ProviderConfig;
use crate::error::{Error, Result};

const DEFAULT_BASE: &str = "https://generativelanguage.googleapis.com";

pub struct GeminiAdapter {
    name: String,
    base_url: String,
    model: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl GeminiAdapter {
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
    let mut contents: Vec<Value> = Vec::new();
    for m in &req.messages {
        match m.role {
            Role::User => contents.push(json!({
                "role": "user",
                "parts": [{"text": m.content}],
            })),
            Role::Assistant => {
                let mut parts: Vec<Value> = Vec::new();
                if !m.content.is_empty() {
                    parts.push(json!({"text": m.content}));
                }
                for c in &m.tool_calls {
                    parts.push(json!({
                        "functionCall": {"name": c.name, "args": c.arguments}
                    }));
                }
                contents.push(json!({"role": "model", "parts": parts}));
            }
            Role::Tool => contents.push(json!({
                "role": "user",
                "parts": [{
                    "functionResponse": {
                        "name": m.tool_call_id,
                        "response": {"result": m.content},
                    }
                }],
            })),
            Role::System => {}
        }
    }

    let mut body = json!({"contents": contents});
    if let Some(sys) = &req.system {
        body["systemInstruction"] = json!({"parts": [{"text": sys}]});
    }
    if !req.tools.is_empty() {
        let decls: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                })
            })
            .collect();
        body["tools"] = json!([{"functionDeclarations": decls}]);
    }
    let mut gen = json!({"maxOutputTokens": req.max_tokens});
    if let Some(t) = req.temperature {
        gen["temperature"] = json!(t);
    }
    body["generationConfig"] = gen;
    body
}

pub(crate) fn parse_response(v: &Value) -> Result<ChatResponse> {
    let cand = v["candidates"]
        .get(0)
        .ok_or_else(|| Error::Provider("gemini: no candidates".into()))?;
    let parts = cand["content"]["parts"].as_array();

    let mut content = String::new();
    let mut tool_calls = Vec::new();
    if let Some(parts) = parts {
        for p in parts {
            if let Some(t) = p["text"].as_str() {
                content.push_str(t);
            }
            if let Some(fc) = p.get("functionCall").filter(|fc| !fc.is_null()) {
                let name = fc["name"].as_str().unwrap_or_default().to_string();
                tool_calls.push(ToolCall {
                    id: name.clone(),
                    name,
                    arguments: fc["args"].clone(),
                });
            }
        }
    }

    let finish = match cand["finishReason"].as_str() {
        Some("MAX_TOKENS") => FinishReason::Length,
        _ if !tool_calls.is_empty() => FinishReason::ToolCalls,
        Some("STOP") => FinishReason::Stop,
        _ => FinishReason::Stop,
    };

    Ok(ChatResponse {
        content,
        tool_calls,
        finish,
        usage: Usage {
            input_tokens: v["usageMetadata"]["promptTokenCount"].as_u64().unwrap_or(0) as u32,
            output_tokens: v["usageMetadata"]["candidatesTokenCount"].as_u64().unwrap_or(0) as u32,
        },
    })
}

#[async_trait]
impl Provider for GeminiAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let key = self
            .api_key
            .as_deref()
            .ok_or_else(|| Error::Provider("gemini: api_key required".into()))?;
        let url = format!(
            "{}/v1beta/models/{}:generateContent?key={}",
            self.base_url.trim_end_matches('/'),
            self.model,
            key
        );
        let resp = self
            .client
            .post(&url)
            .json(&build_body(req))
            .send()
            .await
            .map_err(|e| Error::Provider(format!("gemini request: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| Error::Provider(format!("gemini body: {e}")))?;
        if !status.is_success() {
            return Err(Error::Provider(format!("gemini {status}: {body}")));
        }
        let v: Value = serde_json::from_str(&body)?;
        parse_response(&v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_call_roundtrip() {
        let mut req = ChatRequest::new("gemini-2.0");
        req.system = Some("sys".into());
        req.messages = vec![
            Message::user("hi"),
            Message {
                role: Role::Assistant,
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "search".into(),
                    name: "search".into(),
                    arguments: json!({"q": "x"}),
                }],
                tool_call_id: None,
            },
            Message::tool_result("search", "found"),
        ];
        let body = build_body(&req);
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "sys");
        assert_eq!(body["contents"][1]["role"], "model");
        assert_eq!(body["contents"][1]["parts"][0]["functionCall"]["name"], "search");
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["name"],
            "search"
        );
    }

    #[test]
    fn parse_function_call() {
        let v = json!({
            "candidates": [{
                "content": {"parts": [
                    {"text": "thinking"},
                    {"functionCall": {"name": "list_dir", "args": {"path": "."}}}
                ]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 4, "candidatesTokenCount": 2}
        });
        let r = parse_response(&v).unwrap();
        assert_eq!(r.content, "thinking");
        assert_eq!(r.finish, FinishReason::ToolCalls);
        assert_eq!(r.tool_calls[0].name, "list_dir");
    }
}
