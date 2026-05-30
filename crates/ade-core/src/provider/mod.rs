//! Provider abstraction: one trait, three wire adapters.
//!
//! Concrete models are config entries ([`crate::config::ProviderConfig`]); the
//! adapter chosen depends only on [`crate::config::ProviderKind`]. This is the
//! "any agent, any API" core — a new endpoint is a config block, not code.

pub mod types;

mod anthropic;
mod gemini;
mod openai;

use async_trait::async_trait;

use crate::config::{ProviderConfig, ProviderKind};
use crate::error::{Error, Result};
use crate::keystore;

pub use types::*;

/// A chat-completions backend with tool-calling, normalised to ADE types.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Human-readable name (the config handle).
    fn name(&self) -> &str;
    /// Single non-streaming completion.
    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse>;
}

/// Build a live provider from its config, resolving the API key.
pub fn build(cfg: &ProviderConfig) -> Result<Box<dyn Provider>> {
    let api_key = match &cfg.api_key {
        Some(reference) => Some(keystore::resolve(reference)?),
        None => None,
    };
    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| Error::Provider(format!("http client: {e}")))?;

    Ok(match cfg.kind {
        ProviderKind::Openai => Box::new(openai::OpenAiAdapter::new(cfg, api_key, client)),
        ProviderKind::Anthropic => Box::new(anthropic::AnthropicAdapter::new(cfg, api_key, client)),
        ProviderKind::Gemini => Box::new(gemini::GeminiAdapter::new(cfg, api_key, client)),
    })
}
