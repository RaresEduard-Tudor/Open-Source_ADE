//! Built-in tools scoped to the project root.
//!
//! Six tools: `read_file`, `list_dir`, `search` (read-only) and `write_file`,
//! `edit_file`, `run_shell` (mutating — gated by [`crate::permission`]).

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{safe_join, Tool, ToolContext, ToolRegistry};
use crate::error::{Error, Result};
use crate::provider::ToolSpec;

/// Directory names skipped by `search` to avoid huge/irrelevant trees.
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".ade"];

pub fn register_all(reg: &mut ToolRegistry) {
    reg.register(Box::new(ReadFile));
    reg.register(Box::new(ListDir));
    reg.register(Box::new(Search));
    reg.register(Box::new(WriteFile));
    reg.register(Box::new(EditFile));
    reg.register(Box::new(RunShell));
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .ok_or_else(|| Error::Tool(format!("missing string arg '{key}'")))
}

// ---- read_file -------------------------------------------------------------

struct ReadFile;
#[async_trait]
impl Tool for ReadFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "Read a UTF-8 text file relative to the project root.".into(),
            parameters: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        }
    }
    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<String> {
        let path = safe_join(&ctx.root, arg_str(args, "path")?)?;
        Ok(std::fs::read_to_string(&path)?)
    }
}

// ---- list_dir --------------------------------------------------------------

struct ListDir;
#[async_trait]
impl Tool for ListDir {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_dir".into(),
            description: "List entries of a directory relative to the project root.".into(),
            parameters: json!({
                "type": "object",
                "properties": {"path": {"type": "string", "default": "."}},
            }),
        }
    }
    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<String> {
        let rel = args["path"].as_str().unwrap_or(".");
        let dir = safe_join(&ctx.root, rel)?;
        let mut out = String::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let suffix = if entry.file_type()?.is_dir() { "/" } else { "" };
            out.push_str(&entry.file_name().to_string_lossy());
            out.push_str(suffix);
            out.push('\n');
        }
        Ok(out)
    }
}

// ---- search ----------------------------------------------------------------

struct Search;
#[async_trait]
impl Tool for Search {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search".into(),
            description: "Substring search over project text files. Returns path:line: text."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "path": {"type": "string", "default": "."}
                },
                "required": ["query"]
            }),
        }
    }
    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<String> {
        let query = arg_str(args, "query")?;
        let start = safe_join(&ctx.root, args["path"].as_str().unwrap_or("."))?;
        let mut out = String::new();
        let mut hits = 0usize;
        walk_search(&ctx.root, &start, query, &mut out, &mut hits);
        if out.is_empty() {
            out.push_str("(no matches)");
        }
        Ok(out)
    }
}

fn walk_search(root: &std::path::Path, dir: &std::path::Path, query: &str, out: &mut String, hits: &mut usize) {
    const MAX_HITS: usize = 200;
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if *hits >= MAX_HITS {
            return;
        }
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
            continue;
        }
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => walk_search(root, &path, query, out, hits),
            Ok(ft) if ft.is_file() => {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    let rel = path.strip_prefix(root).unwrap_or(&path);
                    for (i, line) in text.lines().enumerate() {
                        if line.contains(query) {
                            out.push_str(&format!("{}:{}: {}\n", rel.display(), i + 1, line.trim()));
                            *hits += 1;
                            if *hits >= MAX_HITS {
                                return;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

// ---- write_file ------------------------------------------------------------

struct WriteFile;
#[async_trait]
impl Tool for WriteFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file".into(),
            description: "Create or overwrite a file with the given content.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            }),
        }
    }
    fn mutating(&self) -> bool {
        true
    }
    fn summarize(&self, args: &Value) -> String {
        format!("write_file {}", args["path"].as_str().unwrap_or("?"))
    }
    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<String> {
        let path = safe_join(&ctx.root, arg_str(args, "path")?)?;
        let content = arg_str(args, "content")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, content)?;
        Ok(format!("wrote {} bytes to {}", content.len(), arg_str(args, "path")?))
    }
}

// ---- edit_file -------------------------------------------------------------

struct EditFile;
#[async_trait]
impl Tool for EditFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit_file".into(),
            description: "Replace an exact, unique string in a file with new text.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old": {"type": "string", "description": "exact text to replace (must be unique)"},
                    "new": {"type": "string"}
                },
                "required": ["path", "old", "new"]
            }),
        }
    }
    fn mutating(&self) -> bool {
        true
    }
    fn summarize(&self, args: &Value) -> String {
        format!("edit_file {}", args["path"].as_str().unwrap_or("?"))
    }
    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<String> {
        let rel = arg_str(args, "path")?;
        let path = safe_join(&ctx.root, rel)?;
        let old = arg_str(args, "old")?;
        let new = arg_str(args, "new")?;
        let text = std::fs::read_to_string(&path)?;
        let count = text.matches(old).count();
        if count == 0 {
            return Err(Error::Tool(format!("'old' not found in {rel}")));
        }
        if count > 1 {
            return Err(Error::Tool(format!(
                "'old' matches {count} times in {rel}; make it unique"
            )));
        }
        let updated = text.replacen(old, new, 1);
        std::fs::write(&path, &updated)?;
        Ok(format!("edited {rel}"))
    }
}

// ---- run_shell -------------------------------------------------------------

struct RunShell;
#[async_trait]
impl Tool for RunShell {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "run_shell".into(),
            description: "Run a shell command (cmd.exe on Windows, /bin/sh otherwise) in the project root. Returns stdout+stderr."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {"cmd": {"type": "string"}},
                "required": ["cmd"]
            }),
        }
    }
    fn mutating(&self) -> bool {
        true
    }
    fn summarize(&self, args: &Value) -> String {
        args["cmd"].as_str().unwrap_or("?").to_string()
    }
    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<String> {
        let cmd = arg_str(args, "cmd")?;
        // Use the platform's shell: cmd.exe on Windows, /bin/sh elsewhere.
        let mut command = if cfg!(windows) {
            let mut c = std::process::Command::new("cmd");
            c.arg("/C");
            c
        } else {
            let mut c = std::process::Command::new("/bin/sh");
            c.arg("-c");
            c
        };
        let output = command
            .arg(cmd)
            .current_dir(&ctx.root)
            .output()
            .map_err(|e| Error::Tool(format!("spawn failed: {e}")))?;
        let mut out = String::new();
        out.push_str(&String::from_utf8_lossy(&output.stdout));
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.is_empty() {
            out.push_str("\n[stderr]\n");
            out.push_str(&stderr);
        }
        out.push_str(&format!("\n[exit {}]", output.status.code().unwrap_or(-1)));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> (tempdir::Holder, ToolContext) {
        let dir = tempdir::Holder::new();
        let ctx = ToolContext { root: dir.path().to_path_buf() };
        (dir, ctx)
    }

    #[tokio::test]
    async fn write_then_read() {
        let (_d, c) = ctx();
        WriteFile
            .execute(&json!({"path": "a.txt", "content": "hello"}), &c)
            .await
            .unwrap();
        let got = ReadFile.execute(&json!({"path": "a.txt"}), &c).await.unwrap();
        assert_eq!(got, "hello");
    }

    #[tokio::test]
    async fn edit_requires_unique_match() {
        let (_d, c) = ctx();
        WriteFile
            .execute(&json!({"path": "b.txt", "content": "x x"}), &c)
            .await
            .unwrap();
        let err = EditFile
            .execute(&json!({"path": "b.txt", "old": "x", "new": "y"}), &c)
            .await;
        assert!(err.is_err());
        EditFile
            .execute(&json!({"path": "b.txt", "old": "x x", "new": "y"}), &c)
            .await
            .unwrap();
        let got = ReadFile.execute(&json!({"path": "b.txt"}), &c).await.unwrap();
        assert_eq!(got, "y");
    }

    #[tokio::test]
    async fn escape_is_blocked() {
        let (_d, c) = ctx();
        assert!(ReadFile.execute(&json!({"path": "../x"}), &c).await.is_err());
    }
}

/// Tiny self-contained temp-dir helper for tests (avoids a dev-dependency).
#[cfg(test)]
mod tempdir {
    use std::path::{Path, PathBuf};

    pub struct Holder(PathBuf);

    impl Holder {
        pub fn new() -> Self {
            let mut p = std::env::temp_dir();
            let unique = format!(
                "ade-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            p.push(unique);
            std::fs::create_dir_all(&p).unwrap();
            Holder(p)
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Holder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
