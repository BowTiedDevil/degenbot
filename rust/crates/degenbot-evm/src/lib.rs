//! In-process EVM execution over revm, fed by Rust-owned bot state.
//!
//! This crate absorbs `eth_simulateV1` + `eth_createAccessList` into the
//! degenbot Rust core: transaction-envelope simulation that runs entirely in
//! the agent's process, reading pool/token/balance state from `degenbot-bot`'s
//! typed state and falling back to `AlloyDB` (RPC) only for contracts the
//! engine does not track.
//!
//! ## Architecture (option B, chosen by the operator post-spike QGJGWI)
//!
//! The engine state *is* the EVM's `Database`. A hand-written `DatabaseRef`
//! impl ([`bot_state_db::BotStateDb`]) reads `Bot`'s typed pool state
//! (`V2PoolState` reserves, `V3PoolState`/`V4PoolState` `slot0`/`liquidity`/
//! tick-data) and ABI-encodes it to EVM slots on demand — no long-lived
//! encoded copy (the typed fields remain the single source of truth). Composes
//! under [`revm::database::CacheDB`] for sim-scoped overrides:
//!
//! ```text
//! EVM transact → CacheDB (sim-scoped overrides)
//!                 → BotStateDb (engine typed state, encode-on-demand)
//!                 → WrapDatabaseAsync<AlloyDB> (RPC fallback for untracked)
//! ```
//!
//! ## Two consumers (ADR-005)
//!
//! 1. **Pure-Rust MEV bot** (`cargo add degenbot`): drives `Bot` (state owner)
//!    → `Bot` *is* the EVM's `Database` → in-process sim with no Python and no
//!    RPC-for-tracked-state.
//! 2. **Python driver shell**: reaches the same in-process sim through a thin
//!    PyO3 wrapper (`degenbot-python/src/simulation/evm.rs`).
//!
//! ## References
//!
//! - Spike: `docs/spikes/revm-composition-api-and-cold-miss-latency.md`
//!   (version/feature pin, composition API, cold-miss latency, `code_by_hash`
//!   panic safety, access-list emission API).
//! - Feasibility: `docs/spikes/in-process-evm-execution-revm-reth-ethrex-feasibility.md`
//!   (the "Design option B" section).
//! - ADR-003 (Bot as state owner), ADR-005 (three-layer FFI),
//!   ADR-013 (FFI seam private), ADR-014/016 (pool-state deepening, reorg).

// Solidity/rpc/revm identifiers (PyO3, WETH9, PoolManager, DatabaseRef, AlloyDB,
// CacheDB, ERC6909, RwLock) are ubiquitous here — match the degenbot-simulation
// convention and allow clippy's doc-markdown lint crate-wide.
#![allow(clippy::doc_markdown)]

/// The hot-path in-process simulation entry point.
///
/// Executes the 7-call vector (pre-balances → `execute()` → post-balances) via
/// revm `transact_one`, returning the same `SimResult` shape
/// `degenbot-simulation::simulate_one` produces, so the dispatch leaf can swap
/// `dispatch::simulate_v1` for this behind a single call-site change.
pub mod simulator;

/// Sim-scoped override application on a `CacheDB`.
///
/// Ports `degenbot-simulation::build_simulation_state_overrides` (owner funded
/// 100 ETH, injected executor + runtime bytecode, warmup slots, WETH9
/// `balanceOf` override) into `CacheDB::insert_account_storage` /
/// `insert_account_info` calls, preserving the explicit-balance-wins merge.
pub mod state_override;

/// `BotStateDb` — a `revm::DatabaseRef` impl over `Bot`'s typed pool state,
/// with `WrapDatabaseAsync<AlloyDB>` as the cold-miss fallback for untracked
/// contracts. The move that makes the engine state *be* the EVM's `Database`.
pub mod bot_state_db;

/// V4 PoolManager transient-storage seeder (EIP-1153) — seeds the built EVM's
/// `journaled_state.inner.transient_storage` from `V4PoolState` before
/// `transact`, since V4 pools have no persistent on-chain storage at fixed
/// slots. The V4 slot-layout mapping is a follow-up sub-step (revm's
/// transient-storage capability is verified).
pub mod v4_transient;

/// EIP-2930 access-list emission from the revm `State` journal — retires
/// `eth_createAccessList`. Reads the touched address + slot set from
/// `transact`'s `ResultAndState.state`.
pub mod access_list;

pub use access_list::emit_access_list_from_state;
pub use bot_state_db::BotStateDb;
pub use simulator::simulate_in_process;
pub use state_override::apply_simulation_overrides;
