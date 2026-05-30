//! Configuration loading.
//!
//! ADE merges a global config (`~/.config/ade/config.toml`) with an optional
//! per-project config (`./.ade/config.toml`). The project config wins on any
//! overlap; provider/MCP lists are concatenated (project entries appended).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Which wire protocol an adapter speaks. This is the only thing that needs
/// code — every concrete model (Claude, Deepseek, Mimo, Gemini, …) is just a
/// [`ProviderConfig`] entry pointing at one of these kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// OpenAI-compatible `chat/completions`. Covers Deepseek, Mimo, local
    /// servers, and most OSS endpoints.
    Openai,
    /// Anthropic Messages API.
    Anthropic,
    /// Google Gemini `generateContent`.
    Gemini,
}

/// A single configured model/endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Local handle used with `-m/--model` and `/model`.
    pub name: String,
    pub kind: ProviderKind,
    /// API base URL. Optional — adapters supply a sensible default per kind.
    #[serde(default)]
    pub base_url: Option<String>,
    /// The model identifier sent to the provider.
    pub model: String,
    /// Key reference: `env:VAR`, `keyring:service`, or a literal value.
    /// Optional for keyless/local endpoints.
    #[serde(default)]
    pub api_key: Option<String>,
}

/// MCP transport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpTransport {
    Stdio,
    Sse,
}

/// One shared MCP server. Spawned once and shared across every agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    pub name: String,
    pub transport: McpTransport,
    /// stdio: process to launch.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra env for the spawned server. Values may be key references.
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    /// sse: endpoint URL.
    #[serde(default)]
    pub url: Option<String>,
}

/// Permission policy for tool execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionConfig {
    /// Commands/tools that never prompt (e.g. `"cargo build"`).
    #[serde(default)]
    pub allow: Vec<String>,
}

/// The fully merged configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Name of the provider used when `-m` is not given.
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub mcp: Vec<McpConfig>,
    #[serde(default)]
    pub permission: PermissionConfig,
}

impl Config {
    /// Path to the global config file (`~/.config/ade/config.toml`).
    pub fn global_path() -> Result<PathBuf> {
        let dir = dirs::config_dir()
            .ok_or_else(|| Error::Config("cannot locate user config dir".into()))?;
        Ok(dir.join("ade").join("config.toml"))
    }

    /// Path to the project config (`<cwd>/.ade/config.toml`).
    pub fn project_path(cwd: &Path) -> PathBuf {
        cwd.join(".ade").join("config.toml")
    }

    /// Load and merge global + project config. Missing files are treated as
    /// empty, so a fresh install with only env vars still works.
    pub fn load(cwd: &Path) -> Result<Config> {
        let mut cfg = match Self::global_path() {
            Ok(p) => Self::read_file(&p)?,
            Err(_) => Config::default(),
        };
        let proj = Self::read_file(&Self::project_path(cwd))?;
        cfg.merge(proj);
        Ok(cfg)
    }

    fn read_file(path: &Path) -> Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .map_err(|e| Error::Config(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(Error::Io(e)),
        }
    }

    /// Overlay `other` (the project config) onto `self` (global).
    fn merge(&mut self, other: Config) {
        if other.default_model.is_some() {
            self.default_model = other.default_model;
        }
        self.providers.extend(other.providers);
        self.mcp.extend(other.mcp);
        self.permission.allow.extend(other.permission.allow);
    }

    /// Resolve the provider to use given an optional `-m` override.
    pub fn select_provider(&self, requested: Option<&str>) -> Result<&ProviderConfig> {
        let name = requested
            .map(str::to_string)
            .or_else(|| self.default_model.clone())
            .or_else(|| self.providers.first().map(|p| p.name.clone()))
            .ok_or_else(|| Error::Config("no providers configured".into()))?;
        self.providers
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| Error::Config(format!("unknown model '{name}'")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_appends_and_overrides() {
        let mut base: Config = toml::from_str(
            r#"
            default_model = "a"
            [[providers]]
            name = "a"
            kind = "anthropic"
            model = "claude"
        "#,
        )
        .unwrap();
        let proj: Config = toml::from_str(
            r#"
            default_model = "b"
            [[providers]]
            name = "b"
            kind = "openai"
            model = "deepseek-chat"
        "#,
        )
        .unwrap();
        base.merge(proj);
        assert_eq!(base.default_model.as_deref(), Some("b"));
        assert_eq!(base.providers.len(), 2);
    }

    #[test]
    fn select_falls_back_to_first() {
        let cfg: Config = toml::from_str(
            r#"
            [[providers]]
            name = "only"
            kind = "openai"
            model = "x"
        "#,
        )
        .unwrap();
        assert_eq!(cfg.select_provider(None).unwrap().name, "only");
        assert!(cfg.select_provider(Some("nope")).is_err());
    }
}
