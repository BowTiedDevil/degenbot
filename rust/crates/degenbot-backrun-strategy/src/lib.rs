//! The backrun searcher strategy — one example strategy over the
//! `degenbot-simulation` engine.
//!
//! ADR-019 D4/D7 (decision R — Rust-canonical): the strategy stays in Rust
//! (not re-derived in Python — AGENTS.md's "driver shell, not a
//! co-implementation"). This crate owns the backrun bot's strategy logic:
//!
//! - the 7-call pre/post-balance bundle (3 pre-balance → `execute()` →
//!   3-post-balance over WETH9 / Multicall3 / PoolManager ERC6909),
//! - `decode_balance`,
//! - `compute_priority_fee` (TARGET_PROFIT_RATIO / age-decay),
//! - the sim value types (`SimResult`, `SimulateContext`, `SimulatePath`,
//!   `FailBuckets`, the int128 guard),
//! - `dispatch_profitable_results` (the revm-only fan-out) + its thin-margin /
//!   suppression / categorization policy.
//!
//! The engine (revm executor, `BlockSimHandle`, `apply_simulation_overrides`,
//! AL Inspector, `WarmCodeCache`) stays thin + generic in
//! `degenbot-simulation`; this strategy builds the handle via
//! `BlockSimHandle::build` + drives the borrowed `&mut evm` the engine exposes
//! via `BlockSimHandle::evm_mut`. Other searcher strategies (sandwich, JIT-L,
//! liquidation) would be sibling crates over the same engine.
//!
//! # Standalone-Rust consumer
//!
//! `cargo add degenbot-backrun-strategy` reaches the backrun strategy with
//! zero `pyo3` dependency (ADR-005 standalone-core). The Python driver
//! (`examples/eth_backrun_v2_v3_v4_rust.py`) is a thin cockpit over a PyO3
//! wrapper around `dispatch_profitable_results` — it does NOT re-derive the
//! 7-call bundle.

// Solidity/EVM identifiers (balanceOf, WETH9, PoolManager, ERC6909, …) are
// ubiquitous in this crate's docs; allow the pedantic doc-markdown lint.
#![allow(clippy::doc_markdown)]

/// The 7-call calldata builders (balanceOf / getEthBalance / ERC6909 /
/// execute-wrap) — the backrun bundle's read + execute calldata.
pub mod calldata;

/// The dispatch fan-out + categorization policy (`dispatch_profitable_results`)
/// + the candidate/outcome types + the thin-margin pre-filter.
pub mod dispatch;

/// The sim value types + the 7-call orchestration (`simulate_path_on_evm`).
pub mod simulator;

pub use calldata::{
    encode_balance_of_calldata, encode_erc6909_balance_of_calldata,
    encode_get_eth_balance_calldata, wrap_execute_calldata, BALANCE_OF_SELECTOR,
    ERC6909_BALANCE_OF_SELECTOR, GET_ETH_BALANCE_SELECTOR,
};
/// `BlockPriorityFees` is sourced from `degenbot_rpc::fees` (the fee struct is
/// market data, owned by the RPC crate per ADR-019 D5) — re-exported here so
/// the strategy's `SimulateContext` + `compute_priority_fee` consumers reach
/// it through the surface they already use.
pub use degenbot_rpc::BlockPriorityFees;
pub use dispatch::{
    dispatch_profitable_results, filter_thin_margin_results, DispatchCandidate, DispatchOutcome,
    BPS_DENOM, MAX_SIMULATE_CONCURRENT, MIN_PROFIT_NET,
};
pub use simulator::{
    compute_priority_fee, fits_int128, simulate_in_process_with_db, simulate_path_on_evm,
    FailBuckets, SimFailure, SimResult, SimulateContext, SimulatePath, AGE_DECAY_CONSTANT,
    BALANCE_CALL_GAS_LIMIT, EXECUTE_CONFIG, GAS_SAFETY_MARGIN, INITIAL_EXECUTE_GAS, INT128_MAX,
    INT128_MIN, MAX_PRIORITY_FEE_PERCENTILE, MIN_PRIORITY_FEE_PERCENTILE, TARGET_PROFIT_RATIO,
};
