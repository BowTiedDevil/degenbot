//! The simulation engine — the in-process revm EVM handle + its DB stack.
//!
//! ADR-019 D4/D7 (decision R — strategy/engine separation): this crate owns
//! the **engine** (`BlockSimHandle`, the layered DB, `apply_simulation_overrides`,
//! `AccessListCollector`, `WarmCodeCache`). The **strategy** — the 7-call
//! pre/post-balance bundle, `compute_priority_fee`, `decode_balance`,
//! `SimResult`, `dispatch_profitable_results`, `SimulateContext`,
//! `SimulatePath`, `FailBuckets`, the calldata builders — relocated to the
//! `degenbot-backrun-strategy` crate, which drives the borrowed `&mut evm`
//! the engine exposes via [`BlockSimHandle::evm_mut`].
//!
//! # Standalone-Rust consumer
//!
//! `cargo add degenbot-simulation` reaches the in-process engine surface with
//! zero `pyo3` dependency (ADR-005 standalone-core).

// Solidity/EVM identifiers (balanceOf, stateDiff, ERC6909, WETH9, PoolManager,
// …) are ubiquitous in this crate's docs; allow the pedantic doc-markdown lint
// to match the peer core crates.
#![allow(clippy::doc_markdown)]

/// The in-process revm engine + its DB stack (the EVM handle, overrides,
/// AL collector, warm cache). The backrun strategy relocated to
/// `degenbot-backrun-strategy` (ADR-019 D4/D7, decision R).
pub mod sim;

// Re-export the engine surface at the crate root for ergonomic access + for
// the strategy crate (`degenbot-backrun-strategy`) + the PyO3 wrapper. The
// strategy value types (`SimResult`, `SimulateContext`, `FailBuckets`, …)
// now live in `degenbot-backrun-strategy`.
pub use sim::evm::{
    apply_simulation_overrides, divergence_probe, emit_access_list_from_state, AccessListCollector,
    BlockEvm, BlockSimHandle, BotStateDb, CallFrame, CallTrace, CallTraceHandle,
    CallTraceInspector, CapturedSwap, FrameOutcome, ProductionBlockDb, SimInspector,
    SimulationOverrideParams, SwapEventCaptureHandle, SwapEventCaptureInspector, SwapFamily,
    WarmCodeCache, WarmCodeCacheInner, WARM_CODE_CACHE_TTL_BLOCKS,
};
