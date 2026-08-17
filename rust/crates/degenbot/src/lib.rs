//! # `degenbot` — umbrella Rust crate (ADR-005 standalone surface)
//!
//! Re-exports the **pyo3-free** degenbot core surface so a standalone Rust
//! consumer can `cargo add degenbot` and reach `BotState`, the `DexIdentity`
//! presets, the V2/V3/V4 pool-state structs, the swap calc math, and the V2
//! swap-call encoder — **with zero Python in the build graph**.
//!
//! This mirrors the `polars` umbrella Rust crate, which `pub use`s
//! `polars_core::{DataFrame, Series, ...}` with no `pyo3`; all `Py*` wrappers
//! live exclusively in `polars-python`, which Rust consumers never touch.
//! Here the binding layer (`degenbot_rs` cdylib, built from
//! `crates/degenbot-python/`) is a separate workspace member that DOES pull
//! `pyo3`; the umbrella never depends on it.
//!
//! # Standalone-Rust consumer
//!
//! See [`examples/standalone_consumer.rs`](../examples/standalone_consumer.rs):
//! it constructs a `BotState`, registers a V2 pool via the `UNISWAP_V2`
//! `DexIdentity` preset, and runs a swap calc — no Python interpreter, no
//! `pyo3` feature, no maturin.
//!
//! # Verifying pyo3-freeness
//!
//! `rg 'use pyo3' rust/crates/degenbot` is empty by construction (the
//! umbrella has no `pyo3` dependency; the gates `just check-no-pyo3-in-cores`
//! covers the core crates — this umbrella is the same class).

/// Foundational utilities — errors, hex, EIP-55 addresses, shared runtime.
pub use degenbot_core::{address_utils, errors, hex_utils, runtime};

/// Path **investigation toolkit** — shared fixture load/reconstruct/per-hop
/// oracle helpers for reproducing captured failing backrun paths in the
/// `examples/path*_solver_fixture.rs` investigate-runner dialect.
pub mod investigation;

/// Per-chain Rust-owned bot state (`BotState`), reorg journal, decoders,
/// liquidity verifier, block pump, log/solve/reorg coordinators, V2/V3/V4
/// state, plus the Möbius solvers + the unified `ArbitrageEngine`.
pub use degenbot_bot::{bot_core, solvers};

/// Value-only pool identity/state structs + stateless swap simulation
/// (`PoolEntry`, `*PoolIdentity`/`*PoolState`, `v3_simulate_swap`/`v4_simulate_swap`,
/// the V2 constant-product dispatch, the spec-bound validators) + the
/// `TickWordFetcher`/`CurveDataProvider`/`BalancerRateProvider` *interface*
/// traits (ADR-005 standalone-by-design pool value/trait layer).
pub use degenbot_pools;

/// Pathfinding graph (`PathGraph`) + edge graph.
pub use degenbot_pathfinding;

/// Möbius solver math (`basket` / `mixed`) relocated out of
/// `degenbot_bot::solvers` into this standalone crate. `degenbot_bot::solvers`
/// re-exports only `arb_engine`; the relocated solver math lives here.
pub use degenbot_solvers;

/// Uniswap-protocol domain — `DexIdentity` / `DexVariant` / `ReservesAbi`
/// value objects + `pub const` per-DEX presets, and the V2 swap-call encoder.
pub use degenbot_uniswap::{dex_identity, v2_encoding};

/// Uniswap V2/V3/V4 event-log decoders (alloy-only leaf).
pub use degenbot_decoders;

/// Curve `StableSwap` invariant math (Vyper D/Y solvers — pure-Rust leaf).
pub use degenbot_curve_math;

/// Balancer V2 math (`FixedPoint` / `LogExpMath` / `WeightedMath` / `StableMath` — pure-Rust leaf).
pub use degenbot_balancer_math;

/// Solidly / Aerodrome / Camelot stable-pool invariant math (pure-Rust leaf).
pub use degenbot_solidly_math;

/// EVM arithmetic — EIP-1559 base-fee math (pure-Rust; now in `degenbot-core`).
pub use degenbot_core::evm_math;

/// On-chain price readers — Chainlink aggregator + Aave oracle over
/// `degenbot-rpc` `eth_call` (pyo3-free leaf).
pub use degenbot_price;

/// cmd-executor domain — simulation warmup-slot storage math (pure-Rust leaf).
pub use degenbot_executor;

/// The `ExecutionStrategy` seam (ADR-025) — the `PayloadComposer` Encode part +
/// the gate protocol + solve-result view value types, consumed alike by the
/// standalone Rust path and the `PyO3` driver shell. No default strategy ships
/// here; `degenbot-backrun-strategy` implements it as the default adapter.
pub use degenbot_execution;
/// Transaction submission pipeline — fee sizing, signer, dispatcher, and the
/// pending-tx receipt monitor (pure-Rust leaf).
pub use degenbot_submission;

/// `eth_simulateV1` `stateOverrides` construction — code injection + ETH/WETH
/// funding + warmup-slot integration + WETH9 `balanceOf` override
/// (pure-Rust leaf). The in-process revm engine (the per-block shared-EVM
/// handle, DB stack, overrides, AL collector, warm cache).
pub use degenbot_simulation;

/// The backrun searcher strategy — the 7-call pre/post-balance bundle,
/// `compute_priority_fee`, `decode_balance`, `SimResult`, + the
/// `dispatch_profitable_results` fan-out/categorization policy over the
/// `degenbot-simulation` engine (ADR-019 D4/D7, decision R — Rust-canonical:
/// the strategy stays in Rust; the Python driver is a thin cockpit, not a
/// co-implementation).
pub use degenbot_backrun_strategy;

/// Concentrated-liquidity math (`cl_lib`, `tick_math`).
pub use degenbot_concentrated_liquidity_math;
/// Uniswap V2 constant-product (`x·y=k`) single-hop swap math (`IntHopState`,
/// `int_simulate_path`) — the shared primitive for the V2 pool family + the
/// Solidly/Camelot/Aerodrome volatile V2-equivalent hop.
pub use degenbot_v2_math;

/// ABI encode/decode (`abi_decoder`, `abi_encoder`).
pub use degenbot_abi;

/// RPC provider/contract/subscription seams (the pure core; `pyo3` is an
/// optional feature the umbrella never enables).
pub use degenbot_rpc;

/// `SQLite` persistence substrate — read handle + Alembic-aware schema gate +
/// typed rows + read fns (pyo3-free leaf; `rusqlite` with the `bundled`
/// feature).
pub use degenbot_db;

/// Aave V3 domain — the updater chunk-loop + the position-analysis math
/// (health-factor / LTV / eMode / isolation). Standalone-Rust core: a
/// `cargo add degenbot` consumer runs `run_aave_update` with no Python.
pub use degenbot_aave;

/// Anvil fork lifecycle + dev-RPC core (`AnvilFork`, `evm_mine`,
/// `anvil_reset`, `anvil_snapshot`, ...). Standalone-Rust core: a Rust-only
/// consumer spins an anvil fork with no Python (the `anvil` binary must be
/// in `$PATH` at runtime).
pub use degenbot_fork;

/// Pool-updater chunk-loop RPC + decode bridge (`run_pool_update`,
/// `apply_chunk_writes_on_conn`, on-chain `verify_v3/v4_liquidity_map`).
/// Standalone-Rust core mirroring the Aave updater shape.
pub use degenbot_pool_updater;

// ---------------------------------------------------------------------------
// Convenience top-level re-exports of the most-used types (mirrors how the
// `polars` umbrella re-exports `DataFrame`/`Series` at the crate root).
// ---------------------------------------------------------------------------

pub use degenbot_bot::bot_core::pool_builder::builder::{
    build_aerodrome_v2, build_balancer_stable, build_balancer_weighted, build_curve_pool,
    build_erc20_metadata, build_v2, build_v3, build_v4, probe_pool_type, resolve_v4_identity,
    PoolBuilderError, PoolFamily, V4BuildResult, V4PoolBuildIdentity, V4PoolBuildOverrides,
};
pub use degenbot_bot::bot_core::registration_lifecycle::{
    run_cl_v3_lifecycle, run_cl_v4_lifecycle, run_v3_registration_lifecycle,
    run_v4_registration_lifecycle, RegistrationLifecycleError,
};
pub use degenbot_bot::bot_core::{
    BotState, PoolEntry, RegisterAerodromeV2PoolParams, RegisterCurvePoolParams,
    RegisterV2PoolParams, RegisterV3PoolParams, RegisterV4PoolParams, V2PoolState, V4PoolKey,
};
pub use degenbot_uniswap::dex_identity::{
    preset_for_variant, DexIdentity, DexVariant, ReservesAbi, UNISWAP_V2,
};
