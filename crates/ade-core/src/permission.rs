//! Permission gate for tool execution.
//!
//! Read-only tools run freely. Mutating tools (write/shell) are checked against
//! the config allowlist, then delegated to an [`Approver`] — the CLI supplies an
//! interactive one; `--yes` supplies [`AlwaysAllow`]. This is the "no black box"
//! guarantee: nothing mutates the workspace without an explicit yes.

/// What the agent wants to do.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub tool: String,
    pub mutating: bool,
    /// One-line human summary (e.g. the shell command or target path).
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

/// Asked to approve a mutating action not covered by the allowlist.
pub trait Approver: Send + Sync {
    fn approve(&self, req: &ApprovalRequest) -> Decision;
}

/// Approves everything (used by `--yes`).
pub struct AlwaysAllow;
impl Approver for AlwaysAllow {
    fn approve(&self, _req: &ApprovalRequest) -> Decision {
        Decision::Allow
    }
}

/// Denies everything (safe default when no interactive terminal).
pub struct AlwaysDeny;
impl Approver for AlwaysDeny {
    fn approve(&self, _req: &ApprovalRequest) -> Decision {
        Decision::Deny
    }
}

/// Combines the config allowlist with an interactive approver.
pub struct PermissionGate {
    allow: Vec<String>,
    approver: Box<dyn Approver>,
}

impl PermissionGate {
    pub fn new(allow: Vec<String>, approver: Box<dyn Approver>) -> Self {
        Self { allow, approver }
    }

    /// Decide whether `req` may proceed.
    pub fn check(&self, req: &ApprovalRequest) -> Decision {
        if !req.mutating {
            return Decision::Allow;
        }
        if self
            .allow
            .iter()
            .any(|a| a == &req.tool || req.summary.starts_with(a.as_str()))
        {
            return Decision::Allow;
        }
        self.approver.approve(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(tool: &str, mutating: bool, summary: &str) -> ApprovalRequest {
        ApprovalRequest { tool: tool.into(), mutating, summary: summary.into() }
    }

    #[test]
    fn read_always_allowed() {
        let gate = PermissionGate::new(vec![], Box::new(AlwaysDeny));
        assert_eq!(gate.check(&req("read_file", false, "a.txt")), Decision::Allow);
    }

    #[test]
    fn allowlist_matches_prefix() {
        let gate = PermissionGate::new(vec!["cargo build".into()], Box::new(AlwaysDeny));
        assert_eq!(
            gate.check(&req("run_shell", true, "cargo build --release")),
            Decision::Allow
        );
        assert_eq!(gate.check(&req("run_shell", true, "rm -rf /")), Decision::Deny);
    }

    #[test]
    fn delegates_to_approver() {
        let gate = PermissionGate::new(vec![], Box::new(AlwaysAllow));
        assert_eq!(gate.check(&req("write_file", true, "x")), Decision::Allow);
    }
}
