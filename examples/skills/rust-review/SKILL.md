---
name: rust-review
description: Review Rust code for idioms, error handling, and clippy issues
---

When reviewing Rust code:

1. Run `cargo clippy --all-targets` and address every warning.
2. Prefer `?` over `unwrap()`/`expect()` outside tests and `main`.
3. Check that public items have doc comments.
4. Flag `clone()` in hot paths; suggest borrows where possible.
5. Verify error types are propagated, not swallowed.

Report findings as `file:line — issue — fix`.
