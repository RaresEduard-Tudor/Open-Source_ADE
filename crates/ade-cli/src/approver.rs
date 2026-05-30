//! Interactive permission approver for the CLI.
//!
//! Prompts on stdin for mutating actions. `y` allows once, `a` allows that tool
//! for the rest of the session, anything else denies. With `--yes` it allows
//! everything. When stdin is not interactive (EOF), it denies — safe default.

use std::collections::HashSet;
use std::io::{self, BufRead, Write};
use std::sync::Mutex;

use ade_core::permission::{ApprovalRequest, Approver, Decision};

pub struct InteractiveApprover {
    auto: bool,
    session_allow: Mutex<HashSet<String>>,
}

impl InteractiveApprover {
    pub fn new(auto: bool) -> Self {
        Self { auto, session_allow: Mutex::new(HashSet::new()) }
    }
}

impl Approver for InteractiveApprover {
    fn approve(&self, req: &ApprovalRequest) -> Decision {
        if self.auto {
            return Decision::Allow;
        }
        if self.session_allow.lock().unwrap().contains(&req.tool) {
            return Decision::Allow;
        }

        eprint!(
            "\n  permission: {} → {}\n  allow? [y]es / [a]lways / [N]o: ",
            req.tool, req.summary
        );
        let _ = io::stderr().flush();

        let mut line = String::new();
        let n = io::stdin().lock().read_line(&mut line).unwrap_or(0);
        if n == 0 {
            return Decision::Deny; // EOF / non-interactive
        }
        match line.trim() {
            "y" | "Y" => Decision::Allow,
            "a" | "A" => {
                self.session_allow.lock().unwrap().insert(req.tool.clone());
                Decision::Allow
            }
            _ => Decision::Deny,
        }
    }
}
