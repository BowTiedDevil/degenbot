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
fn installed_config_supplies_and_expands_the_runs_root() {
    let mut cfg = BotConfig::default();
    cfg.logging.runs_dir = PathBuf::from("~/degenbot-runs-test");
    assert!(
        degenbot_config::holder::install(Arc::new(cfg)),
        "first install wins in this test process"
    );

    let root = degenbot_runs::resolve_runs_root().expect("resolve");
    assert!(
        !root.to_string_lossy().starts_with('~'),
        "leading tilde expanded against HOME: {}",
        root.display()
    );
    assert!(
        root.ends_with("degenbot-runs-test"),
        "configured leaf preserved: {}",
        root.display()
    );
}
