//! The simulation domain — the in-process revm executor + the dispatch fan-out.
//!
//! ADR-019 retired the RPC simulation path (`eth_simulateV1` /
//! `eth_createAccessList` / the `stateOverrides` JSON builder / the RPC-path
//! `simulate_one`) — the in-process revm path is the sole simulation executor,
//! and the `AccessListCollector` is the sole access-list source. This crate
//! now owns both halves of the engine, folded together by ADR-019 D4:
//!
//! - **`sim::evm`** — the in-process EVM execution core (revm over the
//!   `CacheDB<WarmCodeCache<BotStateDb<WrapDatabaseAsync<AlloyDB>>>` stack),
//!   folded here from the retired `degenbot-evm` crate. Owns `BlockSimHandle`,
//!   `SimResult`, `SimulateContext`, `SimulatePath`, `compute_priority_fee`,
//!   `WarmCodeCache`, `AccessListCollector`, the calldata builders, etc.
//! - **`dispatch_profitable`** — the `dispatch_profitable_results` fan-out +
//!   categorization (the `Some(bot_state)` revm arm; the legacy `None` RPC arm
//!   is retired — kept as `Option` but `None` is `unreachable!` until step 5
//!   / step 6 collapse the FFI seam).
//!
//! What remains is engine code only (the strategy —
//! `dispatch_profitable_results` itself — relocates to `examples/` in step 5,
//! JB22F5).
//!
//! # Standalone-Rust consumer
//!
//! `cargo add degenbot-simulation` reaches the in-process sim surface with
//! zero `pyo3` dependency (ADR-005 standalone-core).

// Solidity/EVM identifiers (balanceOf, stateDiff, ERC6909, WETH9, PoolManager,
// …) are ubiquitous in this crate's docs; allow the pedantic doc-markdown lint
// to match the peer core crates.
#![allow(clippy::doc_markdown)]

/// The in-process revm executor + its DB stack (folded from the retired
/// `degenbot-evm` crate — ADR-019 D4).
pub mod sim;

/// Balance-call calldata builders (B1): WETH9 `balanceOf`, Multicall3
/// `getEthBalance`, PoolManager ERC6909 `balanceOf`, + `wrap_execute_calldata`.
/// Re-exported from `sim::evm` so existing `use degenbot_simulation::calldata`
/// call sites stay unchanged.
pub use sim::evm::calldata;

/// The `dispatch_profitable_results` fan-out + categorization (D-row). Fans
/// the in-process revm `BlockSimHandle::simulate_path` out SERIALLY over a
/// shared per-block `&mut evm` (Tier 1, `V5HCR5`), gathers with exception
/// tolerance, categorizes into gas-profitable / gas-unprofitable / exception,
/// and integrates the thin-margin pre-filter (SYI3PG) + `PathSuppression`
/// (M756BN, consumed from `degenbot-submission`).
pub mod dispatch_profitable;

// Re-export the most-used types at the crate root for ergonomic access
// (mirrors how `degenbot_executor` surfaces `WarmupSlots` / `mapping_slot`).
// Sourced from `sim::evm` (the folded engine home); `BlockPriorityFees` is
// sourced from `degenbot_rpc` (the fee struct is market data, owned by the
// RPC crate per ADR-019 D5).
pub use degenbot_rpc::BlockPriorityFees;
pub use sim::evm::{
    compute_priority_fee, fits_int128, BlockSimHandle, FailBuckets, SimFailure, SimResult,
    SimulateContext, SimulatePath, WarmCodeCache, WarmCodeCacheInner, AGE_DECAY_CONSTANT,
    EXECUTE_CONFIG, GAS_SAFETY_MARGIN, INITIAL_EXECUTE_GAS, INT128_MAX, INT128_MIN,
    MAX_PRIORITY_FEE_PERCENTILE, MIN_PRIORITY_FEE_PERCENTILE, TARGET_PROFIT_RATIO,
    WARM_CODE_CACHE_TTL_BLOCKS,
};
