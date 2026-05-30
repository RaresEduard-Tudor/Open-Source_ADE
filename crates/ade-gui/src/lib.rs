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
use std::sync::{Arc, Mutex};

use notify::{RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

use ade_core::agent::{Agent, Reporter};
use ade_core::config::Config;
use ade_core::mcp::McpHost;
use ade_core::permission::{ApprovalRequest, Approver, Decision, PermissionGate};
use ade_core::provider;
use ade_core::session::Session;
use ade_core::skills::SkillRegistry;
use ade_core::tools::{safe_join, ToolContext, ToolRegistry};

/// Process-wide, read-only state shared by every window: config, project dir,
/// the merged tool registry, the system prompt, and the live MCP servers.
pub(crate) struct AppState {
    cfg: Config,
    pub(crate) cwd: PathBuf,
    /// Built once: built-in tools + skills + shared MCP tools.
    registry: ToolRegistry,
    /// System prompt (includes skill advertisements), built once.
    system: String,
    /// Keeps the shared MCP servers alive for the process lifetime.
    _mcp: McpHost,
}

/// Per-window state: each window has its own conversation, permission
/// bookkeeping, and cancel signal, so windows don't mirror one another.
struct WinCtx {
    session: Mutex<Session>,
    /// JSONL file this window's conversation persists to.
    session_path: PathBuf,
    /// In-flight permission requests: id -> reply channel.
    pending: Mutex<HashMap<u64, mpsc::Sender<u8>>>,
    next_perm_id: AtomicU64,
    /// Tools the user chose "always allow" for this window's session.
    session_allow: Mutex<HashSet<String>>,
    /// Notified to cancel this window's in-flight agent turn.
    cancel: tokio::sync::Notify,
}

/// Registry of per-window contexts, keyed by Tauri window label.
#[derive(Default)]
struct Windows {
    map: Mutex<HashMap<String, Arc<WinCtx>>>,
}

impl Windows {
    /// Get (or lazily create) the context for `label`, restoring its saved
    /// conversation the first time it's seen.
    fn ctx(&self, label: &str, cwd: &std::path::Path) -> Arc<WinCtx> {
        let mut map = self.map.lock().unwrap();
        if let Some(c) = map.get(label) {
            return c.clone();
        }
        let session_path = Session::dir(cwd).join(format!("win-{label}.jsonl"));
        let session = Session::load(&session_path).unwrap_or_default();
        let ctx = Arc::new(WinCtx {
            session: Mutex::new(session),
            session_path,
            pending: Mutex::new(HashMap::new()),
            next_perm_id: AtomicU64::new(1),
            session_allow: Mutex::new(HashSet::new()),
            cancel: tokio::sync::Notify::new(),
        });
        map.insert(label.to_string(), ctx.clone());
        ctx
    }

    fn get(&self, label: &str) -> Option<Arc<WinCtx>> {
        self.map.lock().unwrap().get(label).cloned()
    }
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

/// Flat, recursive list of project files (capped) for the command palette.
#[tauri::command]
fn list_files(state: State<AppState>) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<String>) {
        const CAP: usize = 4000;
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            if out.len() >= CAP {
                return;
            }
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || TREE_SKIP.contains(&name.as_str()) {
                continue;
            }
            let path = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => walk(root, &path, out),
                Ok(t) if t.is_file() => {
                    if let Ok(rel) = path.strip_prefix(root) {
                        out.push(rel.to_string_lossy().replace('\\', "/"));
                    }
                }
                _ => {}
            }
        }
    }
    walk(&state.cwd, &state.cwd, &mut out);
    out.sort();
    out
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

/// Emits agent activity to one window as Tauri events.
struct GuiReporter {
    app: AppHandle,
    label: String,
}

impl GuiReporter {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        let _ = self.app.emit_to(self.label.as_str(), event, payload);
    }
}

impl Reporter for GuiReporter {
    fn on_assistant_delta(&self, text: &str) {
        self.emit("assistant-delta", serde_json::json!(text));
    }
    fn on_assistant_end(&self) {
        self.emit("assistant-end", serde_json::json!(null));
    }
    fn on_tool_call(&self, name: &str, summary: &str) {
        self.emit("tool-call", serde_json::json!({"name": name, "summary": summary}));
    }
    fn on_tool_result(&self, name: &str, result: &str, ok: bool) {
        self.emit("tool-result", serde_json::json!({"name": name, "result": result, "ok": ok}));
    }
    fn on_denied(&self, name: &str, _summary: &str) {
        self.emit("tool-result", serde_json::json!({"name": name, "result": "denied", "ok": false}));
    }
}

/// Approver that asks the webview. `approve` runs on a worker thread inside the
/// agent loop; it emits a `permission-request` event and blocks on a channel
/// until [`respond_permission`] delivers the user's choice from another task
/// (this is why the GUI needs the multi-threaded Tokio runtime).
struct GuiApprover {
    app: AppHandle,
    label: String,
    ctx: Arc<WinCtx>,
}

impl Approver for GuiApprover {
    fn approve(&self, req: &ApprovalRequest) -> Decision {
        if self.ctx.session_allow.lock().unwrap().contains(&req.tool) {
            return Decision::Allow;
        }

        let id = self.ctx.next_perm_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel::<u8>();
        self.ctx.pending.lock().unwrap().insert(id, tx);

        let _ = self.app.emit_to(
            self.label.as_str(),
            "permission-request",
            serde_json::json!({"id": id, "tool": req.tool, "summary": req.summary}),
        );

        // 0 = deny, 1 = allow once, 2 = always. Channel error (window closed)
        // is treated as deny.
        match rx.recv().unwrap_or(0) {
            1 => Decision::Allow,
            2 => {
                self.ctx.session_allow.lock().unwrap().insert(req.tool.clone());
                Decision::Allow
            }
            _ => Decision::Deny,
        }
    }
}

/// Frontend delivers the user's permission choice (0 deny / 1 allow / 2 always).
#[tauri::command]
fn respond_permission(window: tauri::Window, windows: State<Windows>, id: u64, choice: u8) {
    if let Some(ctx) = windows.get(window.label()) {
        if let Some(tx) = ctx.pending.lock().unwrap().remove(&id) {
            let _ = tx.send(choice);
        }
    }
}

/// Run one user turn. Streams output via events; returns final text or an error.
#[tauri::command]
async fn send_prompt(
    app: AppHandle,
    window: tauri::Window,
    prompt: String,
    model: Option<String>,
) -> Result<String, String> {
    let state = app.state::<AppState>();
    let label = window.label().to_string();
    let winctx = app.state::<Windows>().ctx(&label, &state.cwd);

    let gate = PermissionGate::new(
        state.cfg.permission.allow.clone(),
        Box::new(GuiApprover { app: app.clone(), label: label.clone(), ctx: winctx.clone() }),
    );
    let ctx = ToolContext { root: state.cwd.clone() };

    let providerb =
        provider::build(state.cfg.select_provider(model.as_deref()).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;

    // Clone history out so we don't hold the lock across awaits.
    let mut session = winctx.session.lock().unwrap().clone();

    let reporter = GuiReporter { app: app.clone(), label };
    let agent = Agent {
        provider: providerb.as_ref(),
        registry: &state.registry, // shared: builtins + skills + MCP
        gate: &gate,
        ctx: &ctx,
        system: Some(state.system.clone()),
        max_iters: 25,
    };
    // Race the turn against a cancel signal so Stop can interrupt mid-stream.
    // Scope the future so its borrow of `session` ends before we move it.
    let outcome = {
        let run = agent.run_turn(&mut session, &prompt, &reporter);
        tokio::pin!(run);
        tokio::select! {
            r = &mut run => Some(r.map_err(|e| e.to_string())),
            _ = winctx.cancel.notified() => None,
        }
    };

    match outcome {
        Some(r) => {
            // Persist updated history into this window's state and to disk so
            // the conversation survives a restart.
            let _ = session.save(&winctx.session_path);
            *winctx.session.lock().unwrap() = session;
            r
        }
        None => Ok("(cancelled)".to_string()), // partial turn discarded
    }
}

/// Cancel this window's in-flight agent turn.
#[tauri::command]
fn stop(window: tauri::Window, windows: State<Windows>) {
    if let Some(ctx) = windows.get(window.label()) {
        ctx.cancel.notify_waiters();
    }
}

#[derive(Serialize)]
struct ChatTurn {
    role: String,
    content: String,
}

/// User/assistant text from the restored conversation, for repainting the chat
/// on startup. Tool calls and results are omitted (history, not live activity).
#[tauri::command]
fn session_history(
    window: tauri::Window,
    state: State<AppState>,
    windows: State<Windows>,
) -> Vec<ChatTurn> {
    use ade_core::provider::Role;
    let ctx = windows.ctx(window.label(), &state.cwd);
    let msgs = ctx.session.lock().unwrap();
    msgs.messages
        .iter()
        .filter_map(|m| match m.role {
            Role::User if !m.content.is_empty() => {
                Some(ChatTurn { role: "user".into(), content: m.content.clone() })
            }
            Role::Assistant if !m.content.is_empty() => {
                Some(ChatTurn { role: "assistant".into(), content: m.content.clone() })
            }
            _ => None,
        })
        .collect()
}

/// Wipe this window's conversation (in memory and on disk). Frontend clears its
/// own transcript log separately.
#[tauri::command]
fn clear_session(window: tauri::Window, windows: State<Windows>) {
    if let Some(ctx) = windows.get(window.label()) {
        *ctx.session.lock().unwrap() = Session::new();
        let _ = std::fs::remove_file(&ctx.session_path);
    }
}

/// Open another window on the same project (side-by-side editing).
#[tauri::command]
fn new_window(app: AppHandle) -> Result<(), String> {
    let label = format!("w{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0));
    WebviewWindowBuilder::new(&app, label, WebviewUrl::App("index.html".into()))
        .title("ADE")
        .inner_size(1200.0, 800.0)
        .build()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Watch the project tree and tell the webview to refresh when files change
/// underneath it (agent edits, terminal commands, external editors). Build and
/// VCS dirs are filtered out; the frontend debounces and reloads.
fn spawn_fs_watcher(app: AppHandle, root: PathBuf) {
    let mut watcher = match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        let relevant = ev.paths.iter().any(|p| {
            !p.components().any(|c| {
                matches!(
                    c.as_os_str().to_str(),
                    Some(".git") | Some("target") | Some("node_modules") | Some(".ade")
                )
            })
        });
        if relevant {
            let _ = app.emit("fs-changed", ());
        }
    }) {
        Ok(w) => w,
        Err(_) => return,
    };
    if watcher.watch(&root, RecursiveMode::Recursive).is_ok() {
        // Keep the watcher alive for the lifetime of the process.
        std::mem::forget(watcher);
    }
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

    let watch_root = cwd.clone();

    tauri::Builder::default()
        .manage(AppState { cfg, cwd, registry, system, _mcp: mcp })
        .manage(Windows::default())
        .manage(terminal::Terminals::default())
        .setup(move |app| {
            spawn_fs_watcher(app.handle().clone(), watch_root);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_models,
            send_prompt,
            respond_permission,
            list_tree,
            read_file_text,
            save_file_text,
            project_root,
            list_files,
            stop,
            session_history,
            clear_session,
            new_window,
            terminal::term_open,
            terminal::term_input,
            terminal::term_resize,
            terminal::term_close
        ])
        .run(tauri::generate_context!())
        .expect("error while running ADE GUI");
}
