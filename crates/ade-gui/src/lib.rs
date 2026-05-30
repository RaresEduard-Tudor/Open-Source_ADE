//! ADE GUI backend — a thin Tauri shell over `ade-core`.
//!
//! Reuses the exact same agent loop, providers, tools, and streaming as the
//! CLI. The frontend (vanilla JS in `ui/`) talks to two commands and listens
//! for streamed events.
//!
//! Thin-slice scope: model dropdown + streaming chat with built-in tools and
//! skills, mutating actions auto-approved (an in-GUI approval dialog is the
//! next step — see README). MCP is CLI-only for now.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use ade_core::agent::{Agent, Reporter};
use ade_core::config::Config;
use ade_core::permission::{AlwaysAllow, PermissionGate};
use ade_core::provider;
use ade_core::session::Session;
use ade_core::skills::SkillRegistry;
use ade_core::tools::{ToolContext, ToolRegistry};

/// Process-wide state: loaded config, working dir, and the live conversation.
struct AppState {
    cfg: Config,
    cwd: PathBuf,
    session: Mutex<Session>,
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

/// Run one user turn. Streams output via events; returns final text or an error.
#[tauri::command]
async fn send_prompt(
    app: AppHandle,
    prompt: String,
    model: Option<String>,
) -> Result<String, String> {
    let state = app.state::<AppState>();

    // Build a fresh runtime per turn (cheap): builtins + skills, auto-approve.
    let mut registry = ToolRegistry::with_builtins();
    let skills = std::sync::Arc::new(SkillRegistry::discover(&state.cwd));
    skills.register_tool(&mut registry);

    let gate = PermissionGate::new(state.cfg.permission.allow.clone(), Box::new(AlwaysAllow));
    let ctx = ToolContext { root: state.cwd.clone() };

    let mut system = format!(
        "You are an agent running inside ADE in the project at {}. \
         Use the provided tools to read, edit, and run code. Be concise.",
        state.cwd.display()
    );
    if let Some(sk) = skills.system_prompt() {
        system.push_str("\n\n");
        system.push_str(&sk);
    }

    let providerb =
        provider::build(state.cfg.select_provider(model.as_deref()).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;

    // Clone history out so we don't hold the lock across awaits.
    let mut session = state.session.lock().unwrap().clone();

    let reporter = GuiReporter { app: app.clone() };
    let agent = Agent {
        provider: providerb.as_ref(),
        registry: &registry,
        gate: &gate,
        ctx: &ctx,
        system: Some(system),
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

    tauri::Builder::default()
        .manage(AppState { cfg, cwd, session: Mutex::new(Session::new()) })
        .invoke_handler(tauri::generate_handler![list_models, send_prompt])
        .run(tauri::generate_context!())
        .expect("error while running ADE GUI");
}
