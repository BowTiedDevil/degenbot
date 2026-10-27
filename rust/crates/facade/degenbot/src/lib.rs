//! The `cargo add degenbot` umbrella — the pure-Rust capability map.
//!
//! One alias per capability crate, grouped by ADR-084's capability layers
//! (`foundation` ⊂ `engine` ⊂ `integrations`; `shells`/`facade` compose).
//! The authoritative capability → home → door-item → consumer mapping is
//! [`docs/capabilities.md`](../../../../docs/capabilities.md), pinned by
//! `tests/architecture_gates.rs::capability_catalog_covers_every_public_crate`.

// ---------------------------------------------------------------------------
// Configuration & session foundation
// ---------------------------------------------------------------------------

/// Typed configuration, provenance-aware loading, and shared driver-domain resolvers.
pub use degenbot_config as config;

/// The whole `degenbot-core` crate (errors, hex, EIP-55, runtime, EIP-1559).
pub use degenbot_core as core;

/// Foundational utilities — errors, hex, EIP-55 addresses, shared runtime.
pub use degenbot_core::{address_utils, errors, hex_utils, runtime};

/// The observability facade (ADR-043): closed `degenbot::<domain>` targets,
/// the `diag!`/`op_info!`/`op_warn!`/`op_error!`/`op_span!` macros, and the
/// panic hook. Re-exported so a `cargo add degenbot` consumer emits on the
/// same standard as the `PyO3` binding.
pub use degenbot_core::telemetry;

pub use degenbot_core::{diag, op_error, op_info, op_span, op_warn, telemetry_target};

/// EIP-1559 base-fee math.
pub use degenbot_core::eip_1559;

/// `examples/path*_solver_fixture.rs` investigate-runner dialect.
pub mod investigation;

// ---------------------------------------------------------------------------
// State, substrate & the engine
// ---------------------------------------------------------------------------

/// The per-chain state owner + the unified multi-DEX arb engine (`bot_core`,
/// `arb_engine`); the stateless solver math lives in [`crate::solvers`].
pub use degenbot_bot as bot;

/// Engine-session end DETECTION facts: the typed cause vocabulary, the heartbeat
/// stall watchdog, and the once-only delivery channel both drivers share.
pub use degenbot_bot::arb_engine::session_end;

/// ADR-050 / Gap G1: the public Rust driver seam over the
/// crate-private engine. `cargo add degenbot` reaches the full
/// subscribe→resume(+auto-backfill)→stop ritual (also available as
/// `degenbot::bot::arb_engine::EngineDriver`).
pub use degenbot_bot::arb_engine::{DriverError, EngineDriver, PhaseError};

/// `BotState` state-owner surface, re-exported alongside [`crate::bot`].
pub use degenbot_bot::bot_core;

/// The strategy substrate (ADR-067): the state owner, planning workspace,
/// pool ingress, connector index, and executor hop views both consumers
/// compose as a peer.
pub use degenbot_substrate as substrate;

// ---------------------------------------------------------------------------
// Pools, solve & path search
// ---------------------------------------------------------------------------

/// traits (ADR-005 standalone-by-design pool value/trait layer).
pub use degenbot_pools as pools;

/// Stateless solver math (Möbius composition, V3/V4 tick solving, the
/// `QuantAMM` Balancer basket solver) — the solve door (ADR-084 W2).
pub use degenbot_pools::solve as solvers;

/// Pathfinding graph (`PathGraph`) + edge graph.
pub use degenbot_pathfinding as pathfinding;

/// family modules, byte-verified against canonical sources by the tier-3 oracles.
pub use degenbot_math as math;

// ---------------------------------------------------------------------------
// Strategy & execution planes
// ---------------------------------------------------------------------------

/// The strategy plane: the six-slot strategy vocabulary and the concrete
/// executable strategies composed over the capability crates.
pub use degenbot_strategy as strategy;

/// The production `cmd_executor` adapter and its session deployment context,
/// also available under `degenbot::strategy`.
pub use degenbot_strategy::{CmdExecutorAdapter, ExecutionContext};

/// `ExecutionAdapter` seam — the contract side every foreign-executor adapter
/// implements; `degenbot-arbitrage` implements it as the default adapter.
pub use degenbot_execution as execution;

/// co-implementation.
pub use degenbot_arbitrage as arbitrage;

/// The core batch executor (the per-batch dispatch choreography: assembly,
/// payload merge, ordered sim-submit lane) — the value-configured module both
/// the pure-Rust bot and the Python companion drive for Batch outcome records.
pub use degenbot_batch_executor as batch_executor;

/// pending-tx receipt monitor (pure-Rust leaf).
pub use degenbot_submission as submission;

/// test/diagnostic Rust surface — deliberately no FFI exposure.
pub use degenbot_simulation as simulation;

/// cmd-executor domain — simulation warmup-slot storage math (pure-Rust leaf).
pub use degenbot_executor as cmd_executor;

/// consumer by this crate.
pub use degenbot_order_index as order_index;

// ---------------------------------------------------------------------------
// Transport, storage & ingestion
// ---------------------------------------------------------------------------

/// `degenbot-rpc` `eth_call` (pyo3-free leaf; the optional feature the
/// umbrella never enables).
pub use degenbot_rpc as rpc;

/// WebSocket log/block ingestion — the standalone-Rust consumer subscribes to
/// its event stream without Python.
pub use degenbot_ingestion as ingestion;

/// Hub — per-process intake fan-out with declared overflow discipline
/// (the strategy-subscription surface; strategies never own intake).
pub use degenbot_eventhub as eventhub;

/// Rust-owned SQLite schema, migrations, file operations, and persistence substrate.
pub use degenbot_db as db;

/// Per-session run-artifact directories (stdout log + trace JSONL): a
/// std-only, synchronous facility over the typed `logging.runs_dir` root.
pub use degenbot_runs as runs;

/// On-chain Chainlink and Aave-oracle price readers.
pub use degenbot_price as price;

// ---------------------------------------------------------------------------
// Family leaves & updaters
// ---------------------------------------------------------------------------

/// Uniswap V2/V3/V4 event-log decoders (alloy-only leaf).
pub use degenbot_decoders as decoders;

/// The whole `degenbot-uniswap` crate (dex identity + V2 encoding + registry).
pub use degenbot_uniswap as uniswap;

/// value objects + `pub const` per-DEX presets, and the V2 swap-call encoder.
pub use degenbot_uniswap::{dex_identity, v2_encoding};

/// ABI encode/decode (`decoder`, `encoder`).
pub use degenbot_abi as abi;

/// `cargo add degenbot` consumer runs `run_aave_update` with no Python.
pub use degenbot_aave as aave;

/// Anvil fork lifecycle and development RPC support (isolated `anvil` binary
/// in `$PATH` at runtime).
pub use degenbot_fork as fork;

/// Standalone-Rust core mirroring the Aave updater shape.
pub use degenbot_pool_updater as pool_updater;

/// The role-switching worker fleet (ADR-042): the `WorkerRole` FSM, the
/// `FleetBudget` quota authority, bounded priority dispatch, and the
/// `Nominal ⇄ Cordoned` posture — one bounded host for every execution
/// resource, usable by a pure-Rust consumer with zero Python.
pub use degenbot_workers as workers;

// ---------------------------------------------------------------------------
// Registration & construction door items (ADR-078)
// ---------------------------------------------------------------------------

/// The construction door (ADR-078 D2): ONE request ([`PoolConstruction`]), ONE
/// verdict ([`ConstructionVerdict`]), family as data. The builder verbs and the
/// construction interior stay engine-side under
/// `degenbot::bot_core::pool_builder`.
pub use degenbot_bot::bot_core::pool_builder::construction_door::{
    ConstructionVerdict, PoolConstruction,
};

/// [`PoolConstruction::V4`]'s caller-supplied identity overrides — the door's
/// own argument vocabulary.
pub use degenbot_bot::bot_core::pool_builder::builder::V4PoolBuildOverrides;

/// The verdict's payloads: the constructed, registered pool (with the fresh
/// build's identity), or the typed refusal.
pub use degenbot_bot::bot_core::pool_builder::route::{
    BuiltIdentity, ConstructedPool, ConstructionRefusal,
};

pub use degenbot_bot::bot_core::registration_ledger::{PreBuildIdentity, RefusalKind};

pub use degenbot_bot::bot_core::registration_lifecycle::{
    run_v3_registration_lifecycle, run_v4_registration_lifecycle, run_verify_lifecycle,
};

/// The reason a build refused a pool as out of spec — the builder's
/// `PoolBuilderError::Spec` payload, so a consumer can name the refusal without
/// reaching into the pools crate.
pub use degenbot_pools::SpecRefusal;

/// The session object registry — the per-session owner of pool/token
/// canonical identity, the path-identity and position seams (also available
/// as [`crate::substrate::session_registry`]).
pub use degenbot_substrate::session_registry::{
    Freshness, HealthFactor, ObjectRefusal, PoolIdentity, PoolObject, PositionIdentity,
    PositionObserver, PositionReading, PositionRefusal, SessionObjectRegistry, TokenIdentity,
    TokenObject,
};

/// The registration intake types (`PoolEntry` and the per-family
/// `Register*PoolParams` payloads + resolved states).
pub use degenbot_bot::bot_core::RegisterCurvePoolParams;
pub use degenbot_pools::PoolEntry;
pub use degenbot_pools::RegisterAerodromeV2PoolParams;
pub use degenbot_pools::RegisterV3PoolParams;
pub use degenbot_pools::{RegisterV2PoolParams, V2PoolState};
pub use degenbot_pools::{RegisterV4PoolParams, V4PoolKey};

pub use degenbot_substrate::BotState;

pub use degenbot_substrate::PoolLookup;

pub use degenbot_uniswap::dex_identity::{
    preset_for_variant, DexIdentity, DexVariant, ReservesAbi, UNISWAP_V2,
};
