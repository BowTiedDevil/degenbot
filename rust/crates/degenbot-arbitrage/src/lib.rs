//! The settlement-arbitrage searcher strategy — one example strategy over the
//! `degenbot-simulation` engine.
//!
//! ADR-019 D4/D7 (decision R — Rust-canonical): the strategy stays in Rust
//! (not re-derived in Python — AGENTS.md's "driver shell, not a
//! co-implementation"). This crate owns the settlement-arbitrage bot's strategy logic:
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
//! `cargo add degenbot-arbitrage` reaches the settlement-arbitrage strategy with
//! zero `pyo3` dependency (ADR-005 standalone-core). The Python driver
//! (`examples/eth_settlement_arbitrage_v2_v3_v4_rust.py`) is a thin cockpit over a PyO3
//! wrapper around `dispatch_profitable_results` — it does NOT re-derive the
//! 7-call bundle.

// Solidity/EVM identifiers (balanceOf, WETH9, PoolManager, ERC6909, …) are
// ubiquitous in this crate's docs; allow the pedantic doc-markdown lint.
#![expect(clippy::doc_markdown)]

/// The 7-call calldata builders (balanceOf / getEthBalance / ERC6909 /
/// execute-wrap) — the settlement-arbitrage bundle's read + execute calldata.
pub mod calldata;

/// The dispatch fan-out + categorization policy (`dispatch_profitable_results`)
/// + the candidate/outcome types + the thin-margin pre-filter.
pub mod dispatch;

/// Per-pool solver-divergence tracking (`PoolDivergence` +
/// `is_solver_calc_failure`) — a stateful per-pool memo consumed by the
/// dispatch leaf to skip paths through recently-divergent pools (ergo epic
/// GAXXNJ, task GMWYIU). Parallels `PathSuppression` (a stateful per-key
/// counter consumed by the same dispatch leaf).
pub mod pool_divergence;

/// Fee-on-transfer token suspicion attribution (spike `5MP3HQ`) — the
/// `fot_suspected_token` leaf attributes a `SimFailure` to the failing hop's
/// input token when the failure's `reverting_frame.label` is `IIA` (V3) or
/// `CurrencyNotSettled` (V4), or when the captured-swap output mismatches
/// `hop_outputs[i]` (the V2 non-reverting case). Pure lookup off `HopInfo`,
/// no engine accessor required.
pub mod fot_registry;

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
    SolveStep, BPS_DENOM, MAX_SIMULATE_CONCURRENT, MIN_PROFIT_NET,
};
pub use fot_registry::{
    fot_suspected_token, fot_suspected_token_from_reverting_frame,
    fot_suspected_token_from_swap_mismatch, hop_input_token, hop_output_token,
    FeeOnTransferRegistry, FotTokenRecord, FOT_DECAY_BLOCKS, FOT_SUSPICION_THRESHOLD_POOLS,
};
pub use pool_divergence::{
    diverging_pool_keys, hop_pool_key, is_solver_calc_failure, PoolDivergence, PoolDivergenceKey,
    POOL_DIVERGENCE_DECAY_BLOCKS,
};
pub use simulator::{
    compute_priority_fee, execute_gas_limit, fits_int128, simulate_in_process_with_db,
    simulate_path_on_evm, FailBuckets, RevertingFrame, SimFailure, SimResult, SimulateContext,
    SimulatePath, AGE_DECAY_CONSTANT, BALANCE_CALL_GAS_LIMIT, EXECUTE_GAS_ENV, GAS_SAFETY_MARGIN,
    INITIAL_EXECUTE_GAS, INT128_MAX, INT128_MIN, MAX_PRIORITY_FEE_PERCENTILE,
    MIN_PRIORITY_FEE_PERCENTILE, TARGET_PROFIT_RATIO,
};
// The swap-event-capture inspector's decoded per-swap struct (ergo epic
// 63I7WJ). Re-exported here so the PyO3 wrapper (`degenbot-python` outcome)
// can name the success-path `captured_swaps` element type — the same struct
// `SimFailure.captured_swaps` carries (the revert path already surfaces it
// via `failures()`); the success path surfaces it via
// `PyDispatchOutcome.profitable_captured_swaps` (the prerequisite for the
// step-5 classifier re-point at decoded swap amounts instead of the
// `diagnostic.rs` onchain recompute).
pub use degenbot_simulation::sim::evm::inspectors::CapturedSwap;
