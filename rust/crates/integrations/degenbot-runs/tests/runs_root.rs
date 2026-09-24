//! Root resolution: an installed typed config wins, and a `~`-relative root
//! expands against HOME. The holder is process-global, so this lives in its
//! own integration binary.

#![expect(
    clippy::expect_used,
    reason = "test fixtures fail loudly on an unconstructible prerequisite"
)]

use std::path::PathBuf;
use std::sync::Arc;

use degenbot_config::BotConfig;

#[test]
fn installed_config_supplies_and_expands_both_artifact_roots() {
    let mut cfg = BotConfig::default();
    cfg.logging.runs_dir = PathBuf::from("~/degenbot-runs-test");
    cfg.persistence.state_dir = PathBuf::from("~/degenbot-state-test");
    assert!(
        degenbot_config::holder::install(Arc::new(cfg)),
        "first install wins in this test process"
    );

    let runs = degenbot_runs::resolve_runs_root().expect("resolve runs");
    assert!(
        !runs.to_string_lossy().starts_with('~'),
        "leading tilde expanded against HOME: {}",
        runs.display()
    );
    assert!(
        runs.ends_with("degenbot-runs-test"),
        "configured leaf preserved: {}",
        runs.display()
    );

    let state = degenbot_runs::resolve_state_root().expect("resolve state");
    assert!(
        !state.to_string_lossy().starts_with('~'),
        "leading tilde expanded against HOME: {}",
        state.display()
    );
    assert!(
        state.ends_with("degenbot-state-test"),
        "configured leaf preserved: {}",
        state.display()
    );
}
