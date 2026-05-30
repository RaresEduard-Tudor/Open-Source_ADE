//! Integrated terminal backend (PTY).
//!
//! Each terminal is a real pseudo-terminal via `portable-pty` (ConPTY on
//! Windows, openpty elsewhere), so interactive programs, colors, and resizing
//! all work. A reader thread streams output to the webview as `term-output`
//! events; the frontend renders it with xterm.js and sends keystrokes back
//! through [`term_input`].

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use portable_pty::{CommandBuilder, MasterPty, PtySize};
use tauri::{AppHandle, Emitter, Manager};

use crate::AppState;

/// One live terminal: its PTY master (for resize) and a writer (for input).
pub struct Terminal {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
}

/// Holds all open terminals.
#[derive(Default)]
pub struct Terminals {
    map: Mutex<HashMap<u64, Terminal>>,
    next_id: AtomicU64,
}

fn default_shell() -> CommandBuilder {
    if cfg!(windows) {
        CommandBuilder::new("cmd.exe")
    } else {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
        CommandBuilder::new(shell)
    }
}

/// Open a terminal sized `rows`x`cols`, spawned in the project root. Returns its
/// id. Output arrives as `term-output` events `{ id, data }`.
#[tauri::command]
pub fn term_open(app: AppHandle, rows: u16, cols: u16) -> Result<u64, String> {
    let state = app.state::<AppState>();
    let terms = app.state::<Terminals>();

    let pty = portable_pty::native_pty_system();
    let pair = pty
        .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
        .map_err(|e| e.to_string())?;

    let mut cmd = default_shell();
    cmd.cwd(&state.cwd);
    cmd.env("TERM", "xterm-256color");

    let mut child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;

    let id = terms.next_id.fetch_add(1, Ordering::Relaxed) + 1;
    terms
        .map
        .lock()
        .unwrap()
        .insert(id, Terminal { master: pair.master, writer });

    // Stream output to the webview until the shell exits.
    let app2 = app.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let data = String::from_utf8_lossy(&buf[..n]).to_string();
                    let _ = app2.emit("term-output", serde_json::json!({"id": id, "data": data}));
                }
            }
        }
        let _ = child.wait();
        let _ = app2.emit("term-exit", serde_json::json!({"id": id}));
        if let Some(terms) = app2.try_state::<Terminals>() {
            terms.map.lock().unwrap().remove(&id);
        }
    });

    Ok(id)
}

/// Send keystrokes/text to a terminal.
#[tauri::command]
pub fn term_input(app: AppHandle, id: u64, data: String) {
    let terms = app.state::<Terminals>();
    let mut guard = terms.map.lock().unwrap();
    if let Some(t) = guard.get_mut(&id) {
        let _ = t.writer.write_all(data.as_bytes());
        let _ = t.writer.flush();
    }
}

/// Resize a terminal (on container resize).
#[tauri::command]
pub fn term_resize(app: AppHandle, id: u64, rows: u16, cols: u16) {
    let terms = app.state::<Terminals>();
    let guard = terms.map.lock().unwrap();
    if let Some(t) = guard.get(&id) {
        let _ = t.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
    }
}

/// Close a terminal (dropping the master hangs up the PTY).
#[tauri::command]
pub fn term_close(app: AppHandle, id: u64) {
    let terms = app.state::<Terminals>();
    terms.map.lock().unwrap().remove(&id);
}
