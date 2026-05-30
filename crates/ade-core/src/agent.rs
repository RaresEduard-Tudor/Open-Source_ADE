//! The agentic loop.
//!
//! One turn: send history + tool specs to the active provider; if it requests
//! tools, gate each through [`crate::permission`], execute via the
//! [`ToolRegistry`], feed results back, and repeat until the model returns plain
//! text. Every action is surfaced through a [`Reporter`] so nothing is hidden.

use serde_json::Value;

use crate::error::Result;
use crate::permission::{ApprovalRequest, Decision, PermissionGate};
use crate::provider::{ChatRequest, Message, Provider};
use crate::session::Session;
use crate::tools::{ToolContext, ToolRegistry};

/// Observes agent activity for display. The CLI implements this to print
/// assistant text, tool calls, diffs, and denials.
pub trait Reporter: Send + Sync {
    /// A chunk of assistant text as it streams in.
    fn on_assistant_delta(&self, _text: &str) {}
    /// The assistant's text for this step has finished streaming.
    fn on_assistant_end(&self) {}
    fn on_tool_call(&self, _name: &str, _summary: &str) {}
    fn on_tool_result(&self, _name: &str, _result: &str, _ok: bool) {}
    fn on_denied(&self, _name: &str, _summary: &str) {}
}

/// A [`Reporter`] that prints nothing.
pub struct SilentReporter;
impl Reporter for SilentReporter {}

/// Per-turn execution config and dependencies.
pub struct Agent<'a> {
    pub provider: &'a dyn Provider,
    pub registry: &'a ToolRegistry,
    pub gate: &'a PermissionGate,
    pub ctx: &'a ToolContext,
    pub system: Option<String>,
    pub max_iters: usize,
}

impl<'a> Agent<'a> {
    /// Run one user turn to completion, mutating `session` with all messages.
    /// Returns the final assistant text.
    pub async fn run_turn(
        &self,
        session: &mut Session,
        user_input: &str,
        reporter: &dyn Reporter,
    ) -> Result<String> {
        session.push(Message::user(user_input));

        for _ in 0..self.max_iters {
            let mut req = ChatRequest::new(""); // adapter injects its own model
            req.system = self.system.clone();
            req.messages = session.messages.clone();
            req.tools = self.registry.specs();

            let resp = {
                let mut sink = |delta: &str| reporter.on_assistant_delta(delta);
                self.provider.stream(&req, &mut sink).await?
            };
            session.push(resp.to_message());

            if !resp.content.is_empty() {
                reporter.on_assistant_end();
            }
            if resp.tool_calls.is_empty() {
                return Ok(resp.content);
            }

            for call in &resp.tool_calls {
                let result = self.dispatch_tool(&call.name, &call.arguments, reporter).await;
                let payload = match &result {
                    Ok(text) => text.clone(),
                    Err(e) => format!("ERROR: {e}"),
                };
                reporter.on_tool_result(&call.name, &payload, result.is_ok());
                session.push(Message::tool_result(&call.id, payload));
            }
        }

        Ok(format!(
            "(stopped after {} tool iterations)",
            self.max_iters
        ))
    }

    /// Look up, permission-check, and run a single tool call.
    async fn dispatch_tool(
        &self,
        name: &str,
        args: &Value,
        reporter: &dyn Reporter,
    ) -> Result<String> {
        let tool = match self.registry.get(name) {
            Some(t) => t,
            None => return Err(crate::Error::Tool(format!("unknown tool '{name}'"))),
        };

        let summary = tool.summarize(args);
        reporter.on_tool_call(name, &summary);

        let approval = ApprovalRequest {
            tool: name.to_string(),
            mutating: tool.mutating(),
            summary: summary.clone(),
        };
        if self.gate.check(&approval) == Decision::Deny {
            reporter.on_denied(name, &summary);
            return Ok("denied by user".to_string());
        }

        tool.execute(args, self.ctx).await
    }
}
