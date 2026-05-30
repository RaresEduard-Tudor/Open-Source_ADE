//! Tool registry and the [`Tool`] trait.
//!
//! The agent sees one flat list of [`ToolSpec`]s. A [`ToolRegistry`] merges
//! tools from three sources — built-ins (here), MCP servers, and skills — all
//! behind the same trait so they are indistinguishable to the model.

pub mod builtin;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::provider::ToolSpec;

/// Execution context handed to every tool.
pub struct ToolContext {
    /// Project root; all file paths are resolved under it.
    pub root: PathBuf,
}

/// A callable tool.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Schema advertised to the model.
    fn spec(&self) -> ToolSpec;
    /// Whether running this can change the workspace (gates permission).
    fn mutating(&self) -> bool {
        false
    }
    /// Short human summary of a specific call, for the permission prompt.
    fn summarize(&self, _args: &Value) -> String {
        self.spec().name
    }
    /// Run the tool, returning text to feed back to the model.
    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<String>;
}

/// Holds every available tool, keyed by name.
#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry pre-loaded with the built-in tools.
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        builtin::register_all(&mut r);
        r
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.spec().name, tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|b| b.as_ref())
    }

    /// Specs for every tool, to send to the provider.
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// Join `rel` under `root`, rejecting absolute paths and `..` escapes.
pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf> {
    let candidate = Path::new(rel);
    if candidate.is_absolute() {
        return Err(Error::Tool(format!("absolute paths not allowed: {rel}")));
    }
    for comp in candidate.components() {
        if matches!(comp, std::path::Component::ParentDir) {
            return Err(Error::Tool(format!("path escapes project root: {rel}")));
        }
    }
    Ok(root.join(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_rejects_escape() {
        let root = Path::new("/proj");
        assert!(safe_join(root, "../etc/passwd").is_err());
        assert!(safe_join(root, "/etc/passwd").is_err());
        assert_eq!(safe_join(root, "src/a.rs").unwrap(), Path::new("/proj/src/a.rs"));
    }
}
