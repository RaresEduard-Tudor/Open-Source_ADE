//! Shared MCP host (stdio transport).
//!
//! ADE is one MCP *client*: each configured server is spawned **once** and its
//! tools are folded into the [`ToolRegistry`], namespaced `mcp__<server>__<tool>`.
//! Every agent shares the same servers — no per-model installs.
//!
//! Transport is newline-delimited JSON-RPC 2.0 over the child's stdio. Calls are
//! synchronous request/response (blocking) behind a mutex — fine for the
//! one-task-at-a-time CLI. SSE transport is a later addition.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::config::{McpConfig, McpTransport};
use crate::error::{Error, Result};
use crate::keystore;
use crate::provider::ToolSpec;
use crate::tools::{Tool, ToolContext, ToolRegistry};

const PROTOCOL_VERSION: &str = "2024-11-05";

/// A live connection to one MCP server.
pub struct McpServer {
    name: String,
    io: Mutex<ServerIo>,
}

struct ServerIo {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl McpServer {
    /// Spawn the server and run the MCP handshake. Returns the connection plus
    /// the tools it advertises.
    pub fn spawn(cfg: &McpConfig) -> Result<(Arc<McpServer>, Vec<ToolSpec>)> {
        if cfg.transport != McpTransport::Stdio {
            return Err(Error::Provider(format!(
                "mcp '{}': only stdio transport is supported in this build",
                cfg.name
            )));
        }
        let command = cfg
            .command
            .as_deref()
            .ok_or_else(|| Error::Provider(format!("mcp '{}': missing command", cfg.name)))?;

        let mut cmd = Command::new(command);
        cmd.args(&cfg.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (k, v) in &cfg.env {
            // Values may be key references (env:/keyring:/literal).
            cmd.env(k, keystore::resolve(v)?);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Provider(format!("mcp '{}' spawn: {e}", cfg.name)))?;

        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let server = Arc::new(McpServer {
            name: cfg.name.clone(),
            io: Mutex::new(ServerIo { _child: child, stdin, stdout, next_id: 0 }),
        });

        server.handshake()?;
        let tools = server.list_tools()?;
        Ok((server, tools))
    }

    fn handshake(&self) -> Result<()> {
        let mut io = self.io.lock().unwrap();
        io.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "ade", "version": env!("CARGO_PKG_VERSION")}
            }),
            &self.name,
        )?;
        io.notify("notifications/initialized", json!({}))?;
        Ok(())
    }

    fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        let mut io = self.io.lock().unwrap();
        let result = io.request("tools/list", json!({}), &self.name)?;
        let mut specs = Vec::new();
        if let Some(tools) = result["tools"].as_array() {
            for t in tools {
                let bare = t["name"].as_str().unwrap_or_default();
                specs.push(ToolSpec {
                    name: format!("mcp__{}__{}", self.name, bare),
                    description: t["description"].as_str().unwrap_or_default().to_string(),
                    parameters: t
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object"})),
                });
            }
        }
        Ok(specs)
    }

    /// Invoke a tool by its bare (un-namespaced) name.
    fn call(&self, bare_name: &str, args: &Value) -> Result<String> {
        let mut io = self.io.lock().unwrap();
        let result = io.request(
            "tools/call",
            json!({"name": bare_name, "arguments": args}),
            &self.name,
        )?;
        // Flatten content blocks into text.
        let mut out = String::new();
        if let Some(content) = result["content"].as_array() {
            for block in content {
                if let Some(text) = block["text"].as_str() {
                    out.push_str(text);
                }
            }
        }
        if result["isError"].as_bool().unwrap_or(false) {
            return Err(Error::Tool(format!("mcp tool error: {out}")));
        }
        Ok(out)
    }
}

impl ServerIo {
    /// Send a request and block until the matching response arrives,
    /// skipping any interleaved notifications.
    fn request(&mut self, method: &str, params: Value, server: &str) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.write_msg(&msg)?;

        loop {
            let mut line = String::new();
            let n = self
                .stdout
                .read_line(&mut line)
                .map_err(|e| Error::Provider(format!("mcp '{server}' read: {e}")))?;
            if n == 0 {
                return Err(Error::Provider(format!("mcp '{server}': connection closed")));
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(line)
                .map_err(|e| Error::Provider(format!("mcp '{server}' bad json: {e}")))?;
            if v["id"].as_i64() != Some(id) {
                continue; // notification or unrelated message
            }
            if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
                return Err(Error::Provider(format!("mcp '{server}' {method}: {err}")));
            }
            return Ok(v["result"].clone());
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.write_msg(&msg)
    }

    fn write_msg(&mut self, msg: &Value) -> Result<()> {
        let line = serde_json::to_string(msg)?;
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }
}

/// A registry-facing wrapper around one MCP tool.
struct McpTool {
    server: Arc<McpServer>,
    /// Namespaced name as seen by the model.
    full_name: String,
    /// Bare name sent over the wire.
    bare_name: String,
    spec: ToolSpec,
}

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }
    fn mutating(&self) -> bool {
        // MCP tools are opaque; gate them by default.
        true
    }
    fn summarize(&self, _args: &Value) -> String {
        self.full_name.clone()
    }
    async fn execute(&self, args: &Value, _ctx: &ToolContext) -> Result<String> {
        self.server.call(&self.bare_name, args)
    }
}

/// Owns the spawned servers (keeps them alive) for the session's lifetime.
pub struct McpHost {
    _servers: Vec<Arc<McpServer>>,
}

impl McpHost {
    /// Spawn every configured server and register its tools into `reg`.
    /// A failing server is skipped with a warning rather than aborting startup.
    pub fn start(configs: &[McpConfig], reg: &mut ToolRegistry) -> McpHost {
        let mut servers = Vec::new();
        for cfg in configs {
            match McpServer::spawn(cfg) {
                Ok((server, specs)) => {
                    for spec in specs {
                        let full = spec.name.clone();
                        let bare = full
                            .rsplit("__")
                            .next()
                            .unwrap_or(&full)
                            .to_string();
                        reg.register(Box::new(McpTool {
                            server: server.clone(),
                            full_name: full,
                            bare_name: bare,
                            spec,
                        }));
                    }
                    servers.push(server);
                }
                Err(e) => eprintln!("warning: {e}"),
            }
        }
        McpHost { _servers: servers }
    }
}
