//! The `dispatch_profitable_results` fan-out + categorization (Rust core).
//!
//! ADR-019 retired the RPC simulation path (`eth_simulateV1` /
//! `eth_createAccessList` / the `stateOverrides` JSON builder / the RPC-path
//! `simulate_one`) — the in-process revm path (`degenbot-evm`'s
//! `BlockSimHandle::simulate_path`) is the sole simulation executor, and the
//! `AccessListCollector` is the sole access-list source. What remains in this
//! crate is the fan-out + categorization orchestration (the `Some(bot_state)`
//! revm arm of `dispatch_profitable_results`); the legacy `None` RPC arm is
//! retired (transitional: kept as `Option` but `None` is `unreachable!` until
//! step 5 / step 6 collapse the FFI seam).
//!
//! The surviving sim primitives (`SimResult`, `SimulateContext`, `SimulatePath`,
//! `FailBuckets`, `compute_priority_fee`, `BlockSimHandle`, the priority-fee
//! constants) live in `degenbot-evm` and are re-exported here so existing
//! `use degenbot_simulation::SimResult` call sites + the PyO3 wrappers stay
//! unchanged. Step 4 (ADR-019 NIL7ZU) folds `degenbot-evm` into this crate
//! under the `simulation` name; step 5 (JB22F5) extracts the strategy
//! (including `dispatch_profitable_results`) to `examples/`.
//!
//! # Standalone-Rust consumer
//!
//! `cargo add degenbot-simulation` reaches the in-process sim surface with
//! zero `pyo3` dependency (ADR-005 standalone-core).

// Solidity/EVM identifiers (balanceOf, stateDiff, ERC6909, WETH9, PoolManager,
// …) are ubiquitous in this crate's docs; allow the pedantic doc-markdown lint
// to match the peer core crates.
#![allow(clippy::doc_markdown)]

/// Balance-call calldata builders (B1): WETH9 `balanceOf`, Multicall3
/// `getEthBalance`, PoolManager ERC6909 `balanceOf`, + `wrap_execute_calldata`.
/// Moved to `degenbot-evm` (shared with `BlockSimHandle`); re-exported
/// here so existing `use degenbot_simulation::calldata::...` call sites stay
/// unchanged.
pub use degenbot_evm::calldata;

/// The `dispatch_profitable_results` fan-out + categorization (D-row). Fans
/// the in-process revm `BlockSimHandle::simulate_path` out SERIALLY over a
/// shared per-block `&mut evm` (Tier 1, `V5HCR5`), gathers with exception
/// tolerance, categorizes into gas-profitable / gas-unprofitable / exception,
/// and integrates the thin-margin pre-filter (SYI3PG) + `PathSuppression`
/// (M756BN, consumed from `degenbot-submission`).
pub mod dispatch_profitable;

// Re-export the most-used types at the crate root for ergonomic access
// (mirrors how `degenbot_executor` surfaces `WarmupSlots` / `mapping_slot`).
// `BlockPriorityFees` is sourced from `degenbot_rpc` (the fee struct is market
// data, owned by the RPC crate per ADR-019 D5), not `degenbot_evm`.
pub use degenbot_evm::{
    compute_priority_fee, fits_int128, BlockSimHandle, FailBuckets, SimFailure, SimResult,
    SimulateContext, SimulatePath, WarmCodeCacheInner, AGE_DECAY_CONSTANT, EXECUTE_CONFIG,
    GAS_SAFETY_MARGIN, INITIAL_EXECUTE_GAS, INT128_MAX, INT128_MIN, MAX_PRIORITY_FEE_PERCENTILE,
    MIN_PRIORITY_FEE_PERCENTILE, TARGET_PROFIT_RATIO,
};
pub use degenbot_rpc::BlockPriorityFees;
