//! Resolve API-key references into actual secrets.
//!
//! A reference is one of:
//! - `env:VAR_NAME`   — read from an environment variable
//! - `keyring:SERVICE` — read from the OS keyring (requires the `keyring`
//!   feature; otherwise a clear error is returned)
//! - anything else     — treated as a literal secret
//!
//! Resolved secrets are never persisted to session history or logs.

use crate::error::{Error, Result};

/// Resolve a key reference to its secret value.
pub fn resolve(reference: &str) -> Result<String> {
    if let Some(var) = reference.strip_prefix("env:") {
        return std::env::var(var)
            .map_err(|_| Error::Key(format!("env var '{var}' not set")));
    }
    if let Some(service) = reference.strip_prefix("keyring:") {
        return resolve_keyring(service);
    }
    Ok(reference.to_string())
}

#[cfg(feature = "keyring")]
fn resolve_keyring(service: &str) -> Result<String> {
    let user = std::env::var("USER").unwrap_or_else(|_| "ade".into());
    let entry = keyring::Entry::new(service, &user)
        .map_err(|e| Error::Key(format!("keyring open '{service}': {e}")))?;
    entry
        .get_password()
        .map_err(|e| Error::Key(format!("keyring read '{service}': {e}")))
}

#[cfg(not(feature = "keyring"))]
fn resolve_keyring(service: &str) -> Result<String> {
    Err(Error::Key(format!(
        "keyring reference '{service}' requires building ade with --features keyring"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_passthrough() {
        assert_eq!(resolve("sk-abc").unwrap(), "sk-abc");
    }

    #[test]
    fn env_resolution() {
        std::env::set_var("ADE_TEST_KEY", "secret123");
        assert_eq!(resolve("env:ADE_TEST_KEY").unwrap(), "secret123");
        assert!(resolve("env:ADE_TEST_MISSING").is_err());
    }
}
