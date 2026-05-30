//! ade-core — shared core for the Open Source ADE.
//!
//! Layering: [`config`] loads provider/MCP/skill/permission settings;
//! [`keystore`] resolves API-key references; later phases add the provider
//! adapters, tool registry, MCP host, skills, and the agent loop.

pub mod agent;
pub mod config;
pub mod error;
pub mod keystore;
pub mod mcp;
pub mod permission;
pub mod provider;
pub mod session;
pub mod skills;
pub mod tools;

pub use error::{Error, Result};
