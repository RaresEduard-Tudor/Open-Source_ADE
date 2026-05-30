//! Conversation state and JSONL persistence.
//!
//! History is provider-agnostic [`Message`]s, so the active model can change
//! between turns and the next adapter just re-serialises the same history.
//! Sessions persist as one JSON message per line under `.ade/sessions/`.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::provider::Message;

#[derive(Debug, Default, Clone)]
pub struct Session {
    pub messages: Vec<Message>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, m: Message) {
        self.messages.push(m);
    }

    /// Default session directory for a project.
    pub fn dir(project_root: &Path) -> PathBuf {
        project_root.join(".ade").join("sessions")
    }

    /// Append-friendly full rewrite (sessions are small). One JSON line each.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::File::create(path)?;
        for m in &self.messages {
            writeln!(f, "{}", serde_json::to_string(m)?)?;
        }
        Ok(())
    }

    /// Load a session from a JSONL file. Missing file yields an empty session.
    pub fn load(path: &Path) -> Result<Session> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Session::new()),
            Err(e) => return Err(e.into()),
        };
        let mut messages = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            messages.push(serde_json::from_str(line)?);
        }
        Ok(Session { messages })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Message;

    #[test]
    fn roundtrip_jsonl() {
        let mut s = Session::new();
        s.push(Message::user("hi"));
        s.push(Message::assistant("hello"));
        let mut path = std::env::temp_dir();
        path.push(format!("ade-sess-{}.jsonl", std::process::id()));
        s.save(&path).unwrap();
        let loaded = Session::load(&path).unwrap();
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[1].content, "hello");
        let _ = std::fs::remove_file(&path);
    }
}
