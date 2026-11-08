//! The settlement-arbitrage searcher strategy — one example strategy over the
//! `degenbot-simulation` engine.
//!
//! ADR-019 D4/D7 (decision R — Rust-canonical): the strategy stays in Rust
//! (not re-derived in Python — AGENTS.md's "driver shell, not a
//! co-implementation"). This crate owns the settlement-arbitrage bot's strategy logic:
//!
//! - the built-in `cmd_executor` deployment binding ([`cmd_executor`]): its
//!   Encode (command stream + packed config) and its receipt assessment (the
//!   contract reports `ExecutionReceipt { gross_profit, bribe_paid }`) — the
//!   built-in side of the ADR-098 seam,
//! - `compute_priority_fee` (TARGET_PROFIT_RATIO / age-decay) — the operator's
//!   fee sizing, applied once above the seam for a deployment that opts into
//!   [`FeeSizing::MarketAware`] (an adapter-owned deployment keeps its own Fee),
//! - the sim value types (`SimResult`, `SimulatePath`, `FailBuckets`, the
//!   int128 guard) + the composition boundary ([`deployment::SimulateContext`]:
//!   block facts beside the injected [`deployment::SimulationDeployment`]),
//! - `dispatch_profitable_results` (the in-process fan-out) + its thin-margin /
//!   suppression / categorization policy, and `dispatch_across_deployments`
//!   (the explicit all-target fan-out over an ordered
//!   [`deployment::DeploymentRegistry`]).
//!
//! The engine (evm2 executor, `Evm2BlockSimHandle`, `apply_simulation_overrides`,
//! AL Inspector, `WarmCodeCache`) stays thin + generic in
//! `degenbot-simulation`; this strategy builds the handle via
//! `Evm2BlockSimHandle::build` and hands a session's `run_sequence` verb its calls.
//! Other searcher strategies (sandwich, JIT-L, liquidation)
//! would be sibling crates over the same engine.
//!
//! # Standalone-Rust consumer
//!
//! `cargo add degenbot-arbitrage` reaches the settlement-arbitrage strategy with
//! zero `pyo3` dependency (ADR-005 standalone-core). The Python driver
//! (`examples/eth_settlement_arbitrage_v2_v3_v4_rust.py`) is a thin cockpit over a PyO3
//! wrapper around `dispatch_profitable_results` — it does NOT re-derive the
//! settlement's profit measurement.

// Solidity/EVM identifiers (balanceOf, WETH9, PoolManager, ERC6909, …) are
// ubiquitous in this crate's docs; allow the pedantic doc-markdown lint.
#![expect(clippy::doc_markdown)]

/// The built-in `cmd_executor` simulation binding: Encode / Probe / Assess plus
/// the deployment's warmup math, runtime-artifact selection, and cmd config.
pub mod cmd_executor;

/// The simulation composition boundary: block-wide facts beside the injected
/// execution deployment, and the session's ordered deployment registry.
pub mod deployment;

/// Validating built-in target binding (ADR-098 D5): read a deployment's declared
/// release, resolve it in the bounded release registry, validate it against that
/// release's manifest, and bind the deployment's complete call encoding to it.
pub mod release_binding;

/// The registry fan-out: one simulation lane per registered deployment, under
/// either target-selection policy.
pub mod fanout;

/// The target-selection policy above the registry: which deployments a solved
/// path is offered to, and the ordered first-compatible walk.
pub mod selection;

/// The executor-runtime bytecode artifact: the engine's resolution precedence
/// + the validated `0x`-hex read. Every consumer that injects executor code
/// takes its bytes from here, so the pure-Rust path needs no driver shell.
pub mod executor_runtime;

/// The dispatch fan-out + categorization policy (`dispatch_profitable_results`)
/// + the candidate/outcome types + the thin-margin pre-filter.
pub mod dispatch;

/// Per-pool solver-divergence tracking (`PoolDivergence` +
/// `is_solver_calc_failure`) — a stateful per-pool memo consumed by the
/// dispatch leaf to skip paths through recently-divergent pools. Parallels `PathSuppression` (a stateful per-key
/// counter consumed by the same dispatch leaf).
pub mod pool_divergence;

/// Fee-on-transfer token suspicion attribution — the
/// `fot_suspected_token` leaf attributes a `SimFailure` to the failing hop's
/// input token when the failure's `reverting_frame.label` is `IIA` (V3) or
/// `CurrencyNotSettled` (V4), or when the captured-swap output mismatches
/// `hop_outputs[i]` (the V2 non-reverting case). Pure lookup off `HopInfo`,
/// no engine accessor required.
pub mod fot_registry;

/// The sim value types + the production simulation entry
/// (`simulate_path_on_evm`).
pub mod simulator;

pub use cmd_executor::{
    CmdExecutorBinding, CmdExecutorSimAdapter, ExecutionReceipt, InjectionAddressMismatch,
    EXECUTION_RECEIPT_LEN,
};
/// `BlockPriorityFees` is sourced from `degenbot_rpc::fees` (the fee struct is
/// market data, owned by the RPC crate per ADR-019 D5) — re-exported here so
/// the strategy's `SimulateContext` + `compute_priority_fee` consumers reach
/// it through the surface they already use.
pub use degenbot_rpc::BlockPriorityFees;
pub use deployment::{
    DeploymentRegistry, DeploymentRegistryError, FeeSizing, SimulateContext, SimulationBlock,
    SimulationDeployment,
};
pub use dispatch::{
    candidate_is_stale, dispatch_profitable_results, filter_thin_margin_results, is_gas_profitable,
    DispatchCandidate, DispatchOutcome, SolveStep, BPS_DENOM, MAX_SIMULATE_CONCURRENT,
    MIN_PROFIT_NET,
};
/// The executor-runtime artifact loader — the engine's one resolution + read
/// path for the bytecode the sim injects.
pub use executor_runtime::{
    load_executor_runtime, resolve_executor_runtime_path, ExecutorRuntime, ExecutorRuntimeError,
    EXECUTOR_RUNTIME_FILE,
};
pub use fanout::{
    dispatch_across_deployments, DeploymentFanOutError, DeploymentFanOutOutcome, DeploymentLane,
};

pub use release_binding::{
    read_metadata, BindingError, BoundCmdExecutor, BoundRuntimeError, ReleaseSource,
};

/// The release model a bound deployment resolves against: the bounded registry,
/// the per-release complete encodings, and the on-chain discovery classifiers.
pub use degenbot_executor::{discovery, receipt, release, release_registry};
pub use fot_registry::{
    fot_suspected_token, fot_suspected_token_from_reverting_frame,
    fot_suspected_token_from_swap_mismatch, hop_input_token, hop_output_token,
    FeeOnTransferRegistry, FotTokenRecord, FOT_DECAY_BLOCKS, FOT_SUSPICION_THRESHOLD_POOLS,
};
pub use pool_divergence::{
    diverging_pool_keys, hop_pool_key, is_solver_calc_failure, PoolDivergence, PoolDivergenceKey,
    POOL_DIVERGENCE_DECAY_BLOCKS,
};
pub use selection::{PreparedEncoding, SelectedCandidates, TargetSelection};
pub use simulator::{
    compose_transaction, compute_priority_fee, execute_gas_limit, fits_int128,
    simulate_in_process_with_db, simulate_path_on_evm, simulate_path_on_evm_in_span,
    simulate_path_on_evm_prepared, FailBuckets, NoProfitShape, RevertingFrame, SimFailure,
    SimResult, SimulatePath, AGE_DECAY_CONSTANT, EXECUTE_GAS_ENV, GAS_SAFETY_MARGIN,
    INITIAL_EXECUTE_GAS, INT128_MAX, INT128_MIN, MAX_PRIORITY_FEE_PERCENTILE,
    MIN_PRIORITY_FEE_PERCENTILE, TARGET_PROFIT_RATIO,
};
// The swap-event-capture inspector's decoded per-swap struct. Re-exported
// here so a consumer of the sim surface can name the captured-swap element
// type: `SimFailure.captured_swaps` (the revert path) and the survivor's
// capture both carry it, so the step-5 classifier reads decoded swap amounts
// instead of the `diagnostic.rs` onchain recompute.
pub use degenbot_simulation::sim::capture::CapturedSwap;
