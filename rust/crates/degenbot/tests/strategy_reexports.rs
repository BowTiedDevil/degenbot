//! Compile-time contract for the pure-Rust umbrella's production adapter path.
//!
//! This checks public re-exports only; adapter behavior is covered at its
//! `degenbot-strategy` seam.

use degenbot::{strategy, CmdExecutorAdapter};

#[test]
fn production_cmd_executor_adapter_is_available_at_stable_umbrella_paths() {
    let direct_path = std::any::type_name::<CmdExecutorAdapter>();
    let strategy_path = std::any::type_name::<strategy::CmdExecutorAdapter>();

    assert_eq!(direct_path, strategy_path);
}
