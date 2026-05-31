//! Git source-control integration for the GUI.
//!
//! Shells out to the system `git` in the project root. This keeps the binary
//! lean (no libgit2 dependency) and shows the user exactly what they'd see on
//! the command line — fitting ADE's "not a black box" stance.

use std::path::Path;
use std::process::Command;

use serde::Serialize;
use tauri::State;

use crate::AppState;

/// Run `git` in `cwd`, returning stdout on success or stderr as the error.
fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// One changed file in a status listing.
#[derive(Serialize)]
pub struct GitFile {
    path: String,
    /// Single-letter status code: M/A/D/R/C/U/? .
    code: String,
    untracked: bool,
}

/// Working-tree status, split into staged (index) and unstaged changes.
#[derive(Serialize)]
pub struct GitStatus {
    repo: bool,
    branch: String,
    ahead: u32,
    behind: u32,
    staged: Vec<GitFile>,
    unstaged: Vec<GitFile>,
}

impl GitStatus {
    fn not_a_repo() -> Self {
        GitStatus {
            repo: false,
            branch: String::new(),
            ahead: 0,
            behind: 0,
            staged: Vec::new(),
            unstaged: Vec::new(),
        }
    }
}

/// Take the post-rename path from a porcelain entry (`old -> new` → `new`).
fn rename_target(p: &str) -> String {
    match p.split_once(" -> ") {
        Some((_, new)) => new.to_string(),
        None => p.to_string(),
    }
}

/// `git status` parsed into staged / unstaged lists plus branch tracking info.
#[tauri::command]
pub fn git_status(state: State<AppState>) -> GitStatus {
    // Cheap repo check first so non-git projects just get an empty panel.
    if git(&state.cwd, &["rev-parse", "--is-inside-work-tree"]).is_err() {
        return GitStatus::not_a_repo();
    }
    let raw = match git(&state.cwd, &["status", "--porcelain=v1", "--branch"]) {
        Ok(s) => s,
        Err(_) => return GitStatus::not_a_repo(),
    };

    let mut st = GitStatus::not_a_repo();
    st.repo = true;

    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            // e.g. "main...origin/main [ahead 1, behind 2]" or "No commits yet on main"
            parse_branch_line(rest, &mut st);
            continue;
        }
        if line.len() < 3 {
            continue;
        }
        let bytes = line.as_bytes();
        let x = bytes[0] as char; // index (staged) status
        let y = bytes[1] as char; // worktree (unstaged) status
        let path = line[3..].to_string();

        if x == '?' && y == '?' {
            st.unstaged.push(GitFile { path, code: "?".into(), untracked: true });
            continue;
        }
        if x != ' ' && x != '?' {
            st.staged.push(GitFile {
                path: rename_target(&path),
                code: x.to_string(),
                untracked: false,
            });
        }
        if y != ' ' && y != '?' {
            st.unstaged.push(GitFile {
                path: rename_target(&path),
                code: y.to_string(),
                untracked: false,
            });
        }
    }
    st
}

fn parse_branch_line(rest: &str, st: &mut GitStatus) {
    // Branch name is everything up to "..." (tracking) or the first space.
    let head = rest.split("...").next().unwrap_or(rest);
    let name = head.split(['[', ' ']).next().unwrap_or(head).trim();
    if let Some(stripped) = rest.strip_prefix("No commits yet on ") {
        st.branch = stripped.split_whitespace().next().unwrap_or("").to_string();
    } else {
        st.branch = name.to_string();
    }
    if let Some(open) = rest.find('[') {
        let track = &rest[open + 1..rest.find(']').unwrap_or(rest.len())];
        for part in track.split(',') {
            let part = part.trim();
            if let Some(n) = part.strip_prefix("ahead ") {
                st.ahead = n.trim().parse().unwrap_or(0);
            } else if let Some(n) = part.strip_prefix("behind ") {
                st.behind = n.trim().parse().unwrap_or(0);
            }
        }
    }
}

/// Unified diff for one file (staged or working-tree). Untracked files are
/// rendered as an all-added diff so the panel can still show their contents.
#[tauri::command]
pub fn git_diff(state: State<AppState>, path: String, staged: bool) -> Result<String, String> {
    if staged {
        return git(&state.cwd, &["diff", "--cached", "--", &path]);
    }
    let d = git(&state.cwd, &["diff", "--", &path])?;
    if !d.trim().is_empty() {
        return Ok(d);
    }
    // Likely untracked: synthesize an added-lines diff from file contents.
    let abs = state.cwd.join(&path);
    match std::fs::read_to_string(&abs) {
        Ok(content) => {
            let mut out = format!("--- /dev/null\n+++ b/{path}\n");
            for l in content.lines() {
                out.push('+');
                out.push_str(l);
                out.push('\n');
            }
            Ok(out)
        }
        Err(_) => Ok(String::new()),
    }
}

/// Stage a path (`git add`).
#[tauri::command]
pub fn git_stage(state: State<AppState>, path: String) -> Result<(), String> {
    git(&state.cwd, &["add", "--", &path]).map(|_| ())
}

/// Unstage a path (`git reset HEAD`).
#[tauri::command]
pub fn git_unstage(state: State<AppState>, path: String) -> Result<(), String> {
    git(&state.cwd, &["reset", "-q", "HEAD", "--", &path]).map(|_| ())
}

/// Stage everything (`git add -A`).
#[tauri::command]
pub fn git_stage_all(state: State<AppState>) -> Result<(), String> {
    git(&state.cwd, &["add", "-A"]).map(|_| ())
}

/// Discard working-tree changes to a path (`git checkout --`). Destructive —
/// the frontend confirms before calling. Untracked files are removed.
#[tauri::command]
pub fn git_discard(state: State<AppState>, path: String, untracked: bool) -> Result<(), String> {
    if untracked {
        let abs = state.cwd.join(&path);
        return std::fs::remove_file(&abs).map_err(|e| e.to_string());
    }
    git(&state.cwd, &["checkout", "--", &path]).map(|_| ())
}

/// Commit staged changes. Errors (e.g. nothing staged) propagate to the UI.
#[tauri::command]
pub fn git_commit(state: State<AppState>, message: String) -> Result<String, String> {
    let msg = message.trim();
    if msg.is_empty() {
        return Err("empty commit message".into());
    }
    git(&state.cwd, &["commit", "-m", msg])
}
