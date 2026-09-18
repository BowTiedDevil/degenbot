//! Integration check: with `$HOME` sandboxed to a temp dir and
//! `$XDG_STATE_HOME` unset, the run-artifacts helper resolves and creates the
//! XDG state-home default `~/.local/state/degenbot/logs`.
//!
//! The config holder is process-global and uninstalled in this binary, so the
//! helper loads schema defaults and expands the state home through `HOME`.
//! This test owns the process environment for the whole binary (single test).

#![expect(
    clippy::expect_used,
    reason = "test fixtures fail loudly on an unconstructible prerequisite"
)]

#[test]
fn run_artifacts_default_lands_under_home_local_state() {
    let home = std::env::temp_dir().join(format!("degenbot-runs-xdg-{}-home", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("scratch home");

    // Sandbox every layer that could steer the resolution elsewhere.
    let saved = [
        ("HOME", std::env::var("HOME").ok()),
        ("XDG_STATE_HOME", std::env::var("XDG_STATE_HOME").ok()),
        ("XDG_CONFIG_HOME", std::env::var("XDG_CONFIG_HOME").ok()),
        ("DEGENBOT_CONFIG", std::env::var("DEGENBOT_CONFIG").ok()),
        ("DEGENBOT_RUNS_DIR", std::env::var("DEGENBOT_RUNS_DIR").ok()),
        (
            "DEGENBOT_STATE_DIR",
            std::env::var("DEGENBOT_STATE_DIR").ok(),
        ),
    ];
    std::env::set_var("HOME", &home);
    for name in [
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
        "DEGENBOT_CONFIG",
        "DEGENBOT_RUNS_DIR",
        "DEGENBOT_STATE_DIR",
    ] {
        std::env::remove_var(name);
    }

    let expected = home.join(".local/state/degenbot/logs");
    let resolved = degenbot_runs::resolve_runs_root().expect("resolve runs root");
    assert_eq!(
        resolved, expected,
        "unset XDG_STATE_HOME -> $HOME/.local/state/degenbot/logs"
    );

    let run = degenbot_runs::RunDirectory::create("backrun-sidecar").expect("create run dir");
    assert!(
        run.session_dir()
            .starts_with(home.join(".local/state/degenbot/logs")),
        "session nested under the state-home default: {}",
        run.session_dir().display()
    );

    for (name, value) in saved {
        match value {
            Some(v) => std::env::set_var(name, v),
            None => std::env::remove_var(name),
        }
    }
    let _ = std::fs::remove_dir_all(&home);
}
