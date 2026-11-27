//! The test-side adapter onto `scripts/env_surface.py`.
//!
//! The environment surface has ONE implementation, in stdlib Python: the
//! companion's environment reads as a syntax tree, and the Rust tree's
//! `DEGENBOT_*` key inventory. The `degenbot-config` gates below call it as a
//! subprocess, so `rust-test-affected` keeps running both halves, and the
//! default pytest suite calls the same module directly
//! (`tests/test_env_surface.py`).

use std::path::Path;
use std::process::{Command, Output};

/// Run the shared scanner, or refuse by name when it cannot run.
///
/// `python3` is a declared test-tool dependency: CI's `rust-test` job asserts it
/// on PATH and `scripts/hooks/ci-recipe-tools.txt` registers the requirement. An
/// absent interpreter PANICS here rather than skipping: a gate that did not run
/// must never read as a gate that passed, and there is no snapshot to fall back
/// to.
#[expect(
    clippy::panic,
    reason = "test helper: an absent interpreter is a named refusal that fails the gate, never a skip"
)]
pub fn run(program: &str, repo: &Path, args: &[&str]) -> Output {
    Command::new(program)
        .arg(repo.join("scripts").join("env_surface.py"))
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "{program}: python3 is a declared test-tool dependency of the env-surface scan \
                 (scripts/env_surface.py); provision it on PATH (CI's rust-test job asserts it) \
                 -- the gate fails closed rather than skipping: {error}"
            )
        })
}
