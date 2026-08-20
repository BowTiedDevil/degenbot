/// The whole `degenbot-core` crate (errors, hex, EIP-55, runtime, EIP-1559).
pub use degenbot_core as core;
/// Foundational utilities — errors, hex, EIP-55 addresses, shared runtime.
pub use degenbot_core::{address_utils, errors, hex_utils, runtime};

/// `examples/path*_solver_fixture.rs` investigate-runner dialect.
pub mod investigation;

/// state, plus the Möbius solvers + the unified `ArbitrageEngine`.
pub use degenbot_bot as bot;
/// `degenbot::solvers` — ADR-015 relocation).
pub use degenbot_bot::bot_core;
/// traits (ADR-005 standalone-by-design pool value/trait layer).
pub use degenbot_pools as pools;

/// Pathfinding graph (`PathGraph`) + edge graph.
pub use degenbot_pathfinding as pathfinding;

/// re-exports only `arb_engine`; the relocated solver math lives here.
pub use degenbot_solvers as solvers;

/// The whole `degenbot-uniswap` crate (dex identity + V2 encoding + registry).
pub use degenbot_uniswap as uniswap;
/// value objects + `pub const` per-DEX presets, and the V2 swap-call encoder.
pub use degenbot_uniswap::{dex_identity, v2_encoding};

/// Uniswap V2/V3/V4 event-log decoders (alloy-only leaf).
pub use degenbot_decoders as decoders;

/// family modules, byte-verified against canonical sources by the tier-3 oracles.
pub use degenbot_math as math;

/// EIP-1559 base-fee math.
pub use degenbot_core::eip_1559;

/// `degenbot-rpc` `eth_call` (pyo3-free leaf).
pub use degenbot_price as price;

/// cmd-executor domain — simulation warmup-slot storage math (pure-Rust leaf).
pub use degenbot_executor as cmd_executor;

/// here; `degenbot-arbitrage` implements it as the default adapter.
pub use degenbot_execution as execution;
/// pending-tx receipt monitor (pure-Rust leaf).
pub use degenbot_submission as submission;

/// test/diagnostic Rust surface — deliberately no FFI exposure.
pub use degenbot_simulation as simulation;

/// co-implementation).
pub use degenbot_arbitrage as arbitrage;

/// consumer by this crate (B5L2XA).
pub use degenbot_order_index as order_index;

/// ABI encode/decode (`decoder`, `encoder`).
pub use degenbot_abi as abi;

/// optional feature the umbrella never enables).
pub use degenbot_rpc as rpc;

/// feature).
pub use degenbot_db as db;

/// `cargo add degenbot` consumer runs `run_aave_update` with no Python.
pub use degenbot_aave as aave;

/// in `$PATH` at runtime).
pub use degenbot_fork as fork;

/// Standalone-Rust core mirroring the Aave updater shape.
pub use degenbot_pool_updater as pool_updater;

// ---------------------------------------------------------------------------
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
