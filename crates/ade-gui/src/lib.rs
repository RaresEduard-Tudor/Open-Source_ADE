//! ADE GUI backend — a thin Tauri shell over `ade-core`.
//!
//! Reuses the exact same agent loop, providers, tools, and streaming as the
//! CLI. The frontend (vanilla JS in `ui/`) talks to two commands and listens
//! for streamed events.
//!
//! Thin-slice scope: model dropdown + streaming chat with built-in tools and
//! skills, mutating actions auto-approved (an in-GUI approval dialog is the
//! next step — see README). MCP is CLI-only for now.

mod terminal;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use ade_core::agent::{Agent, Reporter};
use ade_core::config::Config;
use ade_core::mcp::McpHost;
use ade_core::permission::{ApprovalRequest, Approver, Decision, PermissionGate};
use ade_core::provider;
use ade_core::session::Session;
use ade_core::skills::SkillRegistry;
use ade_core::tools::{safe_join, ToolContext, ToolRegistry};

/// Process-wide state: loaded config, working dir, the live conversation, and
/// the bookkeeping that lets a synchronous permission check await a click in
/// the webview.
pub(crate) struct AppState {
    cfg: Config,
    pub(crate) cwd: PathBuf,
    session: Mutex<Session>,
    /// Built once: built-in tools + skills + shared MCP tools.
    registry: ToolRegistry,
    /// System prompt (includes skill advertisements), built once.
    system: String,
    /// Keeps the shared MCP servers alive for the process lifetime.
    _mcp: McpHost,
    /// In-flight permission requests: id -> reply channel.
    pending: Mutex<HashMap<u64, mpsc::Sender<u8>>>,
    next_perm_id: AtomicU64,
    /// Tools the user chose "always allow" for this session.
    session_allow: Mutex<HashSet<String>>,
}

#[derive(Serialize)]
struct ModelInfo {
    name: String,
    kind: String,
    model: String,
    default: bool,
}

#[tauri::command]
fn list_models(state: State<AppState>) -> Vec<ModelInfo> {
    let active = state.cfg.select_provider(None).ok().map(|p| p.name.clone());
    state
        .cfg
        .providers
        .iter()
        .map(|p| ModelInfo {
            name: p.name.clone(),
            kind: format!("{:?}", p.kind).to_lowercase(),
            model: p.model.clone(),
            default: active.as_deref() == Some(p.name.as_str()),
        })
        .collect()
}

/// Directory names hidden from the file tree.
const TREE_SKIP: &[&str] = &[".git", "target", "node_modules"];

#[derive(Serialize)]
struct Entry {
    name: String,
    dir: bool,
}

/// List one directory level under the project root (`rel` is "" for the root).
/// Directories first, then files, both alphabetical.
#[tauri::command]
fn list_tree(state: State<AppState>, rel: String) -> Result<Vec<Entry>, String> {
    let dir = safe_join(&state.cwd, if rel.is_empty() { "." } else { &rel })
        .map_err(|e| e.to_string())?;
    let mut entries = Vec::new();
    for e in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let e = e.map_err(|e| e.to_string())?;
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || TREE_SKIP.contains(&name.as_str()) {
            continue;
        }
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        entries.push(Entry { name, dir: is_dir });
    }
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then(a.name.cmp(&b.name)));
    Ok(entries)
}

/// Absolute project root, for the status bar.
#[tauri::command]
fn project_root(state: State<AppState>) -> String {
    state.cwd.display().to_string()
}

/// Read a project file as text.
#[tauri::command]
fn read_file_text(state: State<AppState>, rel: String) -> Result<String, String> {
    let path = safe_join(&state.cwd, &rel).map_err(|e| e.to_string())?;
    std::fs::read_to_string(&path).map_err(|e| e.to_string())
}

/// Save text to a project file (an explicit user action — not gated).
#[tauri::command]
fn save_file_text(state: State<AppState>, rel: String, content: String) -> Result<(), String> {
    let path = safe_join(&state.cwd, &rel).map_err(|e| e.to_string())?;
    std::fs::write(&path, content).map_err(|e| e.to_string())
}

/// Emits agent activity to the webview as Tauri events.
struct GuiReporter {
    app: AppHandle,
}

impl Reporter for GuiReporter {
    fn on_assistant_delta(&self, text: &str) {
        let _ = self.app.emit("assistant-delta", text);
    }
    fn on_assistant_end(&self) {
        let _ = self.app.emit("assistant-end", ());
    }
    fn on_tool_call(&self, name: &str, summary: &str) {
        let _ = self.app.emit("tool-call", serde_json::json!({"name": name, "summary": summary}));
    }
    fn on_tool_result(&self, name: &str, result: &str, ok: bool) {
        let _ = self
            .app
            .emit("tool-result", serde_json::json!({"name": name, "result": result, "ok": ok}));
    }
    fn on_denied(&self, name: &str, _summary: &str) {
        let _ = self.app.emit("tool-result", serde_json::json!({"name": name, "result": "denied", "ok": false}));
    }
}

/// Approver that asks the webview. `approve` runs on a worker thread inside the
/// agent loop; it emits a `permission-request` event and blocks on a channel
/// until [`respond_permission`] delivers the user's choice from another task
/// (this is why the GUI needs the multi-threaded Tokio runtime).
struct GuiApprover {
    app: AppHandle,
}

impl Approver for GuiApprover {
    fn approve(&self, req: &ApprovalRequest) -> Decision {
        let state = self.app.state::<AppState>();
        if state.session_allow.lock().unwrap().contains(&req.tool) {
            return Decision::Allow;
        }

        let id = state.next_perm_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel::<u8>();
        state.pending.lock().unwrap().insert(id, tx);

        let _ = self.app.emit(
            "permission-request",
            serde_json::json!({"id": id, "tool": req.tool, "summary": req.summary}),
        );

        // 0 = deny, 1 = allow once, 2 = always. Channel error (window closed)
        // is treated as deny.
        match rx.recv().unwrap_or(0) {
            1 => Decision::Allow,
            2 => {
                state.session_allow.lock().unwrap().insert(req.tool.clone());
                Decision::Allow
            }
            _ => Decision::Deny,
        }
    }
}

/// Frontend delivers the user's permission choice (0 deny / 1 allow / 2 always).
#[tauri::command]
fn respond_permission(state: State<AppState>, id: u64, choice: u8) {
    if let Some(tx) = state.pending.lock().unwrap().remove(&id) {
        let _ = tx.send(choice);
    }
}

/// Run one user turn. Streams output via events; returns final text or an error.
#[tauri::command]
async fn send_prompt(
    app: AppHandle,
    prompt: String,
    model: Option<String>,
) -> Result<String, String> {
    let state = app.state::<AppState>();

    let gate = PermissionGate::new(
        state.cfg.permission.allow.clone(),
        Box::new(GuiApprover { app: app.clone() }),
    );
    let ctx = ToolContext { root: state.cwd.clone() };

    let providerb =
        provider::build(state.cfg.select_provider(model.as_deref()).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;

    // Clone history out so we don't hold the lock across awaits.
    let mut session = state.session.lock().unwrap().clone();

    let reporter = GuiReporter { app: app.clone() };
    let agent = Agent {
        provider: providerb.as_ref(),
        registry: &state.registry, // shared: builtins + skills + MCP
        gate: &gate,
        ctx: &ctx,
        system: Some(state.system.clone()),
        max_iters: 25,
    };
    let result = agent.run_turn(&mut session, &prompt, &reporter).await;

    // Persist updated history back into shared state.
    *state.session.lock().unwrap() = session;
    result.map_err(|e| e.to_string())
}

/// Tauri entry point.
pub fn run() {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cfg = Config::load(&cwd).unwrap_or_default();

    // Build the shared tool registry once: built-ins + skills + MCP servers
    // (spawned a single time and shared across every turn and model).
    let mut registry = ToolRegistry::with_builtins();
    let mcp = McpHost::start(&cfg.mcp, &mut registry);
    let skills = std::sync::Arc::new(SkillRegistry::discover(&cwd));
    skills.register_tool(&mut registry);

    let mut system = format!(
        "You are an agent running inside ADE in the project at {}. \
         Use the provided tools to read, edit, and run code. Be concise.",
        cwd.display()
    );
    if let Some(sk) = skills.system_prompt() {
        system.push_str("\n\n");
        system.push_str(&sk);
    }

    tauri::Builder::default()
        .manage(AppState {
            cfg,
            cwd,
            session: Mutex::new(Session::new()),
            registry,
            system,
            _mcp: mcp,
            pending: Mutex::new(HashMap::new()),
            next_perm_id: AtomicU64::new(1),
            session_allow: Mutex::new(HashSet::new()),
        })
        .manage(terminal::Terminals::default())
        .invoke_handler(tauri::generate_handler![
            list_models,
            send_prompt,
            respond_permission,
            list_tree,
            read_file_text,
            save_file_text,
            project_root,
            terminal::term_open,
            terminal::term_input,
            terminal::term_resize,
            terminal::term_close
        ])
        .run(tauri::generate_context!())
        .expect("error while running ADE GUI");
}
