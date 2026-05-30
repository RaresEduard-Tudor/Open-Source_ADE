//! Shared MCP host.
//!
//! ADE is one MCP *client*: each configured server is spawned/connected **once**
//! and its tools are folded into the [`ToolRegistry`], namespaced
//! `mcp__<server>__<tool>`. Every agent shares the same servers.
//!
//! Two transports:
//! - **stdio**: newline-delimited JSON-RPC 2.0 over a child process (blocking).
//! - **sse**: the HTTP+SSE transport. A dedicated worker thread runs its own
//!   single-threaded Tokio runtime (so it never collides with the app's
//!   runtime), opens the SSE stream, learns the POST endpoint from the
//!   `endpoint` event, and correlates JSON-RPC responses by id. The public API
//!   stays synchronous via channels.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use crate::config::{McpConfig, McpTransport};
use crate::error::{Error, Result};
use crate::keystore;
use crate::provider::ToolSpec;
use crate::tools::{Tool, ToolContext, ToolRegistry};

const PROTOCOL_VERSION: &str = "2024-11-05";

/// A JSON-RPC transport to one MCP server.
trait Transport: Send + Sync {
    fn request(&self, method: &str, params: Value, server: &str) -> Result<Value>;
    fn notify(&self, method: &str, params: Value) -> Result<()>;
}

/// A live connection to one MCP server (transport-agnostic).
pub struct McpServer {
    name: String,
    transport: Box<dyn Transport>,
}

impl McpServer {
    /// Connect and run the MCP handshake. Returns the connection plus the tools
    /// it advertises.
    pub fn spawn(cfg: &McpConfig) -> Result<(Arc<McpServer>, Vec<ToolSpec>)> {
        let transport: Box<dyn Transport> = match cfg.transport {
            McpTransport::Stdio => Box::new(StdioTransport::connect(cfg)?),
            McpTransport::Sse => Box::new(SseTransport::connect(cfg)?),
        };
        let server = Arc::new(McpServer { name: cfg.name.clone(), transport });
        server.handshake()?;
        let tools = server.list_tools()?;
        Ok((server, tools))
    }

    fn handshake(&self) -> Result<()> {
        self.transport.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "ade", "version": env!("CARGO_PKG_VERSION")}
            }),
            &self.name,
        )?;
        self.transport.notify("notifications/initialized", json!({}))?;
        Ok(())
    }

    fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        let result = self.transport.request("tools/list", json!({}), &self.name)?;
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
        let result = self.transport.request(
            "tools/call",
            json!({"name": bare_name, "arguments": args}),
            &self.name,
        )?;
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

// ---- stdio transport -------------------------------------------------------

struct StdioTransport {
    io: Mutex<ServerIo>,
}

struct ServerIo {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl StdioTransport {
    fn connect(cfg: &McpConfig) -> Result<StdioTransport> {
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
            cmd.env(k, keystore::resolve(v)?);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Provider(format!("mcp '{}' spawn: {e}", cfg.name)))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        Ok(StdioTransport { io: Mutex::new(ServerIo { _child: child, stdin, stdout, next_id: 0 }) })
    }
}

impl Transport for StdioTransport {
    fn request(&self, method: &str, params: Value, server: &str) -> Result<Value> {
        self.io.lock().unwrap().request(method, params, server)
    }
    fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.io.lock().unwrap().notify(method, params)
    }
}

impl ServerIo {
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
                continue;
            }
            if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
                return Err(Error::Provider(format!("mcp '{server}' {method}: {err}")));
            }
            return Ok(v["result"].clone());
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.write_msg(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    fn write_msg(&mut self, msg: &Value) -> Result<()> {
        let line = serde_json::to_string(msg)?;
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }
}

// ---- SSE transport ---------------------------------------------------------

/// A message to send to the SSE worker.
struct Cmd {
    msg: Value,
    /// Present for requests (awaiting a response); None for notifications.
    reply: Option<mpsc::Sender<Result<Value>>>,
    id: Option<i64>,
}

struct SseTransport {
    tx: UnboundedSender<Cmd>,
    next_id: AtomicI64,
}

impl SseTransport {
    fn connect(cfg: &McpConfig) -> Result<SseTransport> {
        let url = cfg
            .url
            .clone()
            .ok_or_else(|| Error::Provider(format!("mcp '{}': sse transport needs a url", cfg.name)))?;
        // `env` entries become HTTP headers (e.g. Authorization), key-resolved.
        let mut headers = Vec::new();
        for (k, v) in &cfg.env {
            headers.push((k.clone(), keystore::resolve(v)?));
        }
        let tx = spawn_sse_worker(url, headers)?;
        Ok(SseTransport { tx, next_id: AtomicI64::new(0) })
    }
}

impl Transport for SseTransport {
    fn request(&self, method: &str, params: Value, server: &str) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let (rtx, rrx) = mpsc::channel();
        self.tx
            .send(Cmd { msg, reply: Some(rtx), id: Some(id) })
            .map_err(|_| Error::Provider(format!("mcp '{server}': worker stopped")))?;
        match rrx.recv_timeout(Duration::from_secs(60)) {
            Ok(r) => r,
            Err(_) => Err(Error::Provider(format!("mcp '{server}': {method} timed out"))),
        }
    }
    fn notify(&self, method: &str, params: Value) -> Result<()> {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let _ = self.tx.send(Cmd { msg, reply: None, id: None });
        Ok(())
    }
}

/// Origin (`scheme://host[:port]`) of a URL, for resolving relative endpoints.
fn origin(url: &str) -> String {
    if let Some(i) = url.find("://") {
        let after = &url[i + 3..];
        let end = after.find('/').map(|p| i + 3 + p).unwrap_or(url.len());
        url[..end].to_string()
    } else {
        url.to_string()
    }
}

fn resolve_endpoint(base: &str, data: &str) -> String {
    if data.starts_with("http://") || data.starts_with("https://") {
        data.to_string()
    } else if data.starts_with('/') {
        format!("{base}{data}")
    } else {
        format!("{base}/{data}")
    }
}

async fn http_post(client: &reqwest::Client, url: &str, msg: &Value) -> Result<()> {
    let resp = client
        .post(url)
        .json(msg)
        .send()
        .await
        .map_err(|e| Error::Provider(format!("mcp post: {e}")))?;
    if !resp.status().is_success() {
        return Err(Error::Provider(format!("mcp post status {}", resp.status())));
    }
    Ok(())
}

async fn dispatch_cmd(
    client: &reqwest::Client,
    post_url: &str,
    cmd: Cmd,
    pending: &mut HashMap<i64, mpsc::Sender<Result<Value>>>,
) {
    match (cmd.id, cmd.reply) {
        (Some(id), Some(reply)) => {
            pending.insert(id, reply);
            if let Err(e) = http_post(client, post_url, &cmd.msg).await {
                if let Some(r) = pending.remove(&id) {
                    let _ = r.send(Err(e));
                }
            }
        }
        _ => {
            let _ = http_post(client, post_url, &cmd.msg).await;
        }
    }
}

/// Start the SSE worker thread. Blocks (up to 10s) until the POST endpoint is
/// learned, so a dead server fails fast rather than hanging the handshake.
fn spawn_sse_worker(url: String, headers: Vec<(String, String)>) -> Result<UnboundedSender<Cmd>> {
    let (tx, mut rx) = unbounded_channel::<Cmd>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(r) => r,
            Err(e) => {
                let _ = ready_tx.send(Err(Error::Provider(format!("mcp sse runtime: {e}"))));
                return;
            }
        };
        rt.block_on(async move {
            let client = reqwest::Client::new();
            let mut req = client.get(&url).header("accept", "text/event-stream");
            for (k, v) in &headers {
                req = req.header(k.as_str(), v.as_str());
            }
            let mut resp = match req.send().await {
                Ok(r) if r.status().is_success() => r,
                Ok(r) => {
                    let _ = ready_tx.send(Err(Error::Provider(format!("mcp sse status {}", r.status()))));
                    return;
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(Error::Provider(format!("mcp sse connect: {e}"))));
                    return;
                }
            };

            let base = origin(&url);
            let mut post_url: Option<String> = None;
            let mut pending: HashMap<i64, mpsc::Sender<Result<Value>>> = HashMap::new();
            let mut queued: Vec<Cmd> = Vec::new();
            let mut ready_sent = false;
            let mut buf: Vec<u8> = Vec::new();
            let mut data = String::new();
            let mut event = String::new();

            loop {
                tokio::select! {
                    biased;
                    maybe = rx.recv() => match maybe {
                        Some(cmd) => match &post_url {
                            Some(p) => dispatch_cmd(&client, p, cmd, &mut pending).await,
                            None => queued.push(cmd),
                        },
                        None => break, // senders dropped -> shut down
                    },
                    chunk = resp.chunk() => match chunk {
                        Ok(Some(bytes)) => {
                            buf.extend_from_slice(&bytes);
                            while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                                let raw: Vec<u8> = buf.drain(..=nl).collect();
                                let line = String::from_utf8_lossy(&raw);
                                let line = line.trim_end_matches(['\r', '\n']);
                                if line.is_empty() {
                                    let ev = if event.is_empty() { "message" } else { event.as_str() };
                                    if ev == "endpoint" {
                                        post_url = Some(resolve_endpoint(&base, data.trim()));
                                        if !ready_sent {
                                            let _ = ready_tx.send(Ok(()));
                                            ready_sent = true;
                                        }
                                        if let Some(p) = &post_url {
                                            for c in std::mem::take(&mut queued) {
                                                dispatch_cmd(&client, p, c, &mut pending).await;
                                            }
                                        }
                                    } else if !data.is_empty() {
                                        if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                                            if let Some(id) = v["id"].as_i64() {
                                                if let Some(reply) = pending.remove(&id) {
                                                    let r = match v.get("error").filter(|e| !e.is_null()) {
                                                        Some(err) => Err(Error::Provider(format!("mcp: {err}"))),
                                                        None => Ok(v["result"].clone()),
                                                    };
                                                    let _ = reply.send(r);
                                                }
                                            }
                                        }
                                    }
                                    event.clear();
                                    data.clear();
                                } else if let Some(rest) = line.strip_prefix("data:") {
                                    if !data.is_empty() {
                                        data.push('\n');
                                    }
                                    data.push_str(rest.trim_start());
                                } else if let Some(rest) = line.strip_prefix("event:") {
                                    event = rest.trim().to_string();
                                }
                            }
                        }
                        Ok(None) | Err(_) => break,
                    },
                }
            }

            if !ready_sent {
                let _ = ready_tx.send(Err(Error::Provider("mcp sse: closed before endpoint".into())));
            }
            for (_, reply) in pending.drain() {
                let _ = reply.send(Err(Error::Provider("mcp sse: connection closed".into())));
            }
        });
    });

    match ready_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => Ok(tx),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(Error::Provider("mcp sse: timed out waiting for endpoint".into())),
    }
}

// ---- registry glue ---------------------------------------------------------

/// A registry-facing wrapper around one MCP tool.
struct McpTool {
    server: Arc<McpServer>,
    full_name: String,
    bare_name: String,
    spec: ToolSpec,
}

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }
    fn mutating(&self) -> bool {
        true // MCP tools are opaque; gate them by default.
    }
    fn summarize(&self, _args: &Value) -> String {
        self.full_name.clone()
    }
    async fn execute(&self, args: &Value, _ctx: &ToolContext) -> Result<String> {
        self.server.call(&self.bare_name, args)
    }
}

/// Owns the connected servers (keeps them alive) for the session's lifetime.
pub struct McpHost {
    _servers: Vec<Arc<McpServer>>,
}

impl McpHost {
    /// Connect every configured server and register its tools into `reg`.
    /// A failing server is skipped with a warning rather than aborting startup.
    pub fn start(configs: &[McpConfig], reg: &mut ToolRegistry) -> McpHost {
        let mut servers = Vec::new();
        for cfg in configs {
            match McpServer::spawn(cfg) {
                Ok((server, specs)) => {
                    for spec in specs {
                        let full = spec.name.clone();
                        let bare = full.rsplit("__").next().unwrap_or(&full).to_string();
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
