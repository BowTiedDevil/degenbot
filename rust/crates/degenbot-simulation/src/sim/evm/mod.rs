//! In-process EVM execution over revm, fed by Rust-owned bot state.
//!
//! This submodule (folded here from the retired `degenbot-evm` crate —
//! ADR-019 D4) is the simulation engine: transaction-envelope execution that
//! runs entirely in the agent's process, reading pool/token/balance state
//! from `degenbot-bot`'s typed state and falling back to `AlloyDB` (RPC) only
//! for contracts the engine does not track. The RPC `eth_simulateV1` +
//! `eth_createAccessList` paths retired (ADR-019 D1) — this is the sole
//! simulation executor.
//!
//! ## Architecture (option B seam — forwarding today)
//!
//! [`bot_state_db::BotStateDb`] is a thin `revm::DatabaseRef` wrapper that
//! currently **forwards every read** to the `WrapDatabaseAsync<AlloyDB>`
//! fallback. The typed-state serving path (V2 reserves / V3 `slot0`/
//! `liquidity`/`ticks` ABI-encoded to EVM slots on demand) was deliberately
//! NOT wired — serving the snapshot's reserves/`slot0` against the on-chain
//! slots the pool's own `swap()` reads (fee growth, tick bitmap,
//! `IERC20.balanceOf`) produced K-invariant / `LOK` reverts from
//! stale-vs-fresh state divergence. The wrapper persists as the option B
//! seam the live `BlockSimHandle` chain references; collapsing it to
//! bare `WrapDatabaseAsync<AlloyDB>` is the Tier 1 refactor's scope (ergo
//! task `V5HCR5`). See [`bot_state_db`] for the historical note on the
//! retired slot encoders.
//!
//! ```text
//! EVM transact → CacheDB (sim-scoped overrides)
//!                 → BotStateDb (forwarding wrapper; option B seam)
//!                 → WrapDatabaseAsync<AlloyDB> (RPC fallback)
//! ```
//!
//! ## Two consumers (ADR-005)
//!
//! 1. **Pure-Rust MEV bot** (`cargo add degenbot`): drives `Bot` (state owner)
//!    → `Bot` *is* the EVM's `Database` → in-process sim with no Python and no
//!    RPC-for-tracked-state.
//! 2. **Python driver shell**: reaches the same in-process sim through a thin
//!    PyO3 wrapper (`degenbot-python/src/simulation/`, the `dispatch`
//!    `#[pyfunction]` over `BlockSimHandle`).
//!
//! ## References
//!
//! - Spike: `docs/spikes/revm-composition-api-and-cold-miss-latency.md`
//!   (version/feature pin, composition API, cold-miss latency, `code_by_hash`
//!   panic safety, access-list emission API).
//! - Feasibility: `docs/spikes/in-process-evm-execution-revm-reth-ethrex-feasibility.md`
//!   (the "Design option B" section).
//! - ADR-003 (Bot as state owner), ADR-005 (three-layer FFI),
//! - ADR-013 (FFI seam private), ADR-014/016 (pool-state deepening, reorg),
//! - ADR-019 (in-process revm sole simulation executor; strategy-vs-engine
//!   separation).

// Solidity/rpc/revm identifiers (PyO3, WETH9, PoolManager, DatabaseRef, AlloyDB,
// CacheDB, ERC6909, RwLock) are ubiquitous here — allow clippy's doc-markdown
// lint for this submodule.
#![allow(clippy::doc_markdown)]

/// The hot-path in-process simulation engine entry point.
///
/// Owns the per-block shared EVM handle (`BlockSimHandle`) + the layered DB
/// types (`BlockEvm` / `ProductionBlockDb`) + the provider newtype. The
/// **strategy** (the 7-call bundle, `SimResult`, `compute_priority_fee`,
/// `dispatch_profitable_results`, `SimulateContext`, the calldata builders)
/// relocated to `degenbot-backrun-strategy` (ADR-019 D4/D7, decision R).
pub mod simulator;

/// Sim-scoped override application on a `CacheDB`.
///
/// Applies the owner-funded-100-ETH + injected-executor+runtime-bytecode +
/// warmup-slots + WETH9 `balanceOf` override into `CacheDB::insert_account_storage` /
/// `insert_account_info` calls, preserving the explicit-balance-wins merge.
pub mod state_override;

/// `BotStateDb` — a thin `revm::DatabaseRef` wrapper that forwards every read
/// to the `WrapDatabaseAsync<AlloyDB>` fallback. Currently a pass-through;
/// the typed-state serving path (option B) is not wired. See the module's
/// historical note on the retired slot encoders.
pub mod bot_state_db;

// (Deleted) V4 PoolManager transient-storage seeder — `v4_transient.rs` was
// built on the false premise that V4 pool swap state (sqrtPriceX96 /
// liquidity / tick) lives in transient storage (EIP-1153). Per the deployed
// V4-core source (`contract_reference/uniswap/V4/PoolManager.sol`),
// `_pools` (mapping `PoolId => Pool.State`) is a PERSISTENT mapping at slot 6
// — see `docs/architecture/v4_poolmanager_storage_layout.md`. Transient
// storage holds the currency-delta accounting + lock flag (NOT pool state).
// The `apply_v4_transient_state` function was a dead no-op stub (never
// called). V4 pool state for the in-process sim is seeded via the PERSISTENT
// CacheDB cold-load path (the `BotStateDb` → `WrapDatabaseAsync<AlloyDB>`
// fallback) — proven for V2/V3 by the `swap_capture_correctness.rs`
// mainnet probe; V4 probe extension is the real V4 captured-amount proof.

// 7-call vector calldata builders live in `degenbot-backrun-strategy::calldata`
// (relocated with the backrun bundle — ADR-019 D4/D7, decision R).

/// EIP-2930 access-list emission from the revm `State` journal — retires
/// `eth_createAccessList`. Reads the touched address + slot set from
/// `transact`'s `ResultAndState.state`.
pub mod access_list;

/// Composable `revm::Inspector` pair for simulation diagnostics
/// (`CallTraceInspector`, `SwapEventCaptureInspector`) + the `SimInspector`
/// composed-tuple alias. Additive + test-only in the prototype (ergo task
/// `2LMT7A`); production wiring gated on the JHPW5W follow-on.
pub mod inspectors;

/// Cross-block warm cache for immutable/long-TTL account data (bytecode +
/// account existence) — the persistent layer underneath the per-block
/// `CacheDB`. Caches `basic_ref` + `code_by_hash_ref` with a per-entry TTL;
/// forwards `storage_ref` + `block_hash_ref` untouched.
pub mod warm_code_cache;

pub use access_list::{emit_access_list_from_state, AccessListCollector};
pub use bot_state_db::BotStateDb;
/// Re-export the diagnostic inspectors + captured structs (engine-generic,
/// ADR-019 D7 — the PyO3 wrapper surfaces them as `#[pyclass]` thin shells).
pub use inspectors::{
    CallFrame, CallTrace, CallTraceHandle, CallTraceInspector, CapturedSwap, FrameOutcome,
    SimInspector, SwapEventCaptureHandle, SwapEventCaptureInspector, SwapFamily,
};
/// Re-export the engine surface so `degenbot-simulation`'s crate root can
/// surface it for the strategy crate (`degenbot-backrun-strategy`) + the
/// PyO3 wrapper. The strategy types (`SimResult`, `SimulateContext`, …) now
/// live in `degenbot-backrun-strategy`.
pub use simulator::{BlockEvm, BlockSimHandle, ProductionBlockDb};
pub use state_override::{apply_simulation_overrides, SimulationOverrideParams};
pub use warm_code_cache::{WarmCodeCache, WarmCodeCacheInner, WARM_CODE_CACHE_TTL_BLOCKS};
