//! Compile-time contract for the pure-Rust umbrella's production adapter and session context paths.
//!
//! This checks public re-exports only; adapter behavior is covered at its
//! `degenbot-strategy` seam.

use degenbot::{strategy, CmdExecutorAdapter, ExecutionContext};

#[test]
fn production_cmd_executor_adapter_is_available_at_stable_umbrella_paths() {
    let direct_path = std::any::type_name::<CmdExecutorAdapter>();
    let strategy_path = std::any::type_name::<strategy::CmdExecutorAdapter>();
    let context_path = std::any::type_name::<ExecutionContext>();
    let strategy_context_path = std::any::type_name::<strategy::ExecutionContext>();

    assert_eq!(direct_path, strategy_path);
    assert_eq!(context_path, strategy_context_path);
}
