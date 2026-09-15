//! Rust extension for degenbot.
//!
//! This crate provides high-performance Rust implementations of common operations
//! used by the degenbot Python package.
//!
//! # Modules
//!
//! - [`abi_types`] - Unified ABI type/value representation (`AbiType`, `AbiValue`, `CachedAbiTypes`)
//! - [`decoder`] - High-performance ABI decoding
//! - [`encoder`] - High-performance ABI encoding
//! - [`conversion`] - Shared PyO3-dependent converters (U256/I256 ↔ Python `int`, cached refs, JSON/RPC type → Python)
//! - [`address_utils`] - Ethereum address utilities (EIP-55 checksumming)
//! - [`errors`] - Centralized error types with `thiserror`
//! - [`provider`] - Ethereum RPC provider with Alloy (HTTP, WS, IPC)
//! - [`rpc::provider`] - `PyO3` bindings for sync provider
//! - [`rpc::async_provider`] - Async Ethereum provider wrapper
//! - [`contract`] - Smart contract interface with ABI encoding/decoding
//! - [`rpc::contract`] - `PyO3` bindings for contract
//! - [`rpc::async_contract`] - Async contract wrapper with batch calls
//! - [`signature_parser`] - Robust function signature parsing
//! - [`runtime`] - Shared Tokio runtime singleton
//! - [`hex_utils`] - Pure-Rust hex encoding/decoding (no `PyO3` dependency)
//!
//! See individual module documentation for usage examples.

use degenbot_core::diag;
use degenbot_core::op_info;
// Opt-in allocator swap for churn-heavy workloads (missed-WS-pong follow-up,
// RSS-growth investigation). The measured pathology was glibc free-page
// retention across per-thread arenas (system vs in-use spread of gigabytes,
// reclaimed on demand by malloc_trim). mimalloc returns freed segments to the
// OS aggressively instead of pooling them, at the cost of the system
// allocator's free-list caching. TUNING (epic AZZDBI T4): the purge cadence
// is runtime-controlled by degenbot-bot/src/allocator_ctrl.rs — default
// MADV_FREE (lazy) + purge_delay discovered from observed block cadence
// (2 x mean interval, 10 percent hysteresis); measured on mainnet:
// 89 percent lower per-block refault churn at equal/better solve p95.
// Dev builds opt in via pyproject
// [tool.maturin] features; release wheels keep the system allocator until the
// swap is judged (same policy as hotpath/otel). NOTE: this switches only the
// Rust global allocator — CPython's pymalloc and third-party C libs (sqlite,
// etc.) keep glibc, so the blast radius is the Rust heap only.
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// PyO3 seam for the `degenbot-aave` chunk loop (`run_aave_update`).
#[cfg(feature = "aave-updater")]
pub mod aave_updater;
#[cfg(feature = "abi")]
pub mod abi;
/// Ambient-runtime driver seam (VJGZJ2): enter the shared degenbot-core
/// runtime around a Python callable. Unconditional — degenbot-core is.
pub mod ambient_runtime;
#[cfg(feature = "balancer-math")]
pub mod balancer_math;
#[cfg(feature = "bot")]
pub mod bot;
/// Build identity: monotonic build counter baked by `build.rs` (stale-
/// `.so` detector, AGENTS.md). Unconditional — see `build_info.rs`.
pub mod build_info;
pub mod c_api;
/// `CancelHandle` — the cooperative cancel flag for the updater loops.
/// Gated on `any(pool, aave-updater)` (whichever seam needs it).
#[cfg(any(feature = "pool", feature = "aave-updater"))]
pub mod cancel;
/// The Python-side console seam (ADR-051 D3): argv passthrough into the
/// Rust-owned console. Unconditional - the console is core.
pub mod cli;
#[cfg(feature = "concentrated-liquidity-math")]
pub mod concentrated_liquidity_math;
/// Typed `BotConfig` accessors for the Python driver shell (4IOEVT). The
/// loader (the ONLY env reader) installs the process-wide config; these
/// getters expose its typed fields to Python without a second declaration
/// site. Unconditional — degenbot-config is always a dependency.
pub mod config;
pub mod conversion;
pub mod crypto;
#[cfg(feature = "curve-math")]
pub mod curve_dy;
#[cfg(feature = "curve-math")]
pub mod curve_math;
#[cfg(feature = "db")]
pub mod db;
pub mod diagnostics;
pub mod eip_1559;
#[cfg(feature = "execution")]
pub mod execution;
#[cfg(feature = "executor")]
pub mod executor;
/// The fleet operator seam (JCI2FW Part B): the runtime re-tune channel
/// over the process posture owner. Gated on `simulation` — the feature
/// that carries the `degenbot-workers` dependency.
#[cfg(feature = "simulation")]
pub mod fleet;
#[cfg(feature = "fork")]
pub mod fork;
#[cfg(feature = "pathfinding")]
pub mod pathfinding;
#[cfg(feature = "pool")]
pub mod pool;
pub mod prelude;
#[cfg(feature = "price")]
pub mod price;
pub mod python_log_layer;
#[cfg(feature = "rpc")]
pub mod rpc;
/// FF-T5 (NT7HJC): the runtime fleet status — budget, plan, census
/// ("degenbot.runtime_status()"). Unconditional — the plan
/// function and the census are.
pub mod runtime_status;
#[cfg(feature = "simulation")]
pub mod simulation;
pub mod solady;
#[cfg(feature = "solidly-math")]
pub mod solidly_math;
#[cfg(feature = "bot")]
pub mod solvers_basket;
#[cfg(feature = "submission")]
pub mod submission;
#[cfg(feature = "uniswap")]
pub mod uniswap;
#[cfg(feature = "v2-math")]
pub mod v2_math;

// The foundational core modules live in the `degenbot-core` workspace member.
// Re-exported here as `crate::errors` / `crate::hex_utils` / etc. so every
// existing `crate::errors::` call site in the binding layer keeps resolving
// through the re-export, with zero edits to call sites. Pure-Rust consumers
// depend on `degenbot-core` directly (default features, no pyo3).
pub use degenbot_core::{address_utils, errors, hex_utils, runtime};

// The ABI type/decode/encode + signature-parsing core lives in the
// `degenbot-abi` workspace member. Re-exported as `crate::abi_types` /
// `crate::decoder` / `crate::encoder` / `crate::signature_parser` so
// every existing call site in the binding layer (`contract`, `rpc::contract`,
// `conversion::alloy`, `degenbot-uniswap::v2_encoding`) keeps resolving. The `#[pyfunction]`
// wrappers (`decode`/`encode`) live in `abi::decoder` / `abi::encoder`.
#[cfg(feature = "abi")]
pub use degenbot_abi::{abi_types, decoder, encoder, signature_parser};

// The RPC provider / contract / subscription core lives in the
// `degenbot-rpc` workspace member. Re-exported as `crate::provider` /
// `crate::contract` / `crate::subscription` so every existing call site in the
// binding layer (`rpc::provider`, `rpc::contract`, `rpc::subscription`,
// `rpc::async_provider`, `rpc::async_contract`) keeps resolving. The `#[pyfunction]`
// wrappers + the GIL-bound `drain_buffer`/`DrainResult` stay in the root
// `*_py` modules (they need `conversion::cache` / `conversion::rpc_types`).
#[cfg(feature = "rpc")]
pub use degenbot_rpc::{contract, provider, subscription};

// The bot state (`BotState`, reorg journal, verifier, pump,
// V2/V3/V4 state) + Möbius solvers + the unified arbitrage engine live in the
// `degenbot-bot` workspace member — one crate by ADR-003 (the state/solver seam
// is genuine domain coupling, not over-abstracted). Re-exported as
// `crate::bot_core` / `crate::arb_engine` so every existing call site in the
// binding layer keeps resolving. The `#[pyclass]`/`#[pyfunction]` wrappers
// (`PyBot`, `PyLiquidityPool`, `PyErc20Token`, `PyDexIdentity`,
// `PyArbEngine`, the `Verification*Error`/`*RejectedError` exception
// types) live in the `bot` / `bot::pool` / `bot::token` /
// `bot::dex_identity` modules and the `bot::engine` subdir (they need `conversion::alloy` / `conversion::cache`).
#[cfg(feature = "bot")]
pub use degenbot_bot::bot_core;

// The pure Uniswap V2/V3/V4 event-log decoders live in the `degenbot-decoders`
// workspace member (Plan 104) — an alloy-only leaf (no pyo3/tokio/degenbot-core).
// No `pub use` re-export: the binding layer reaches the lone type it needs
// (`degenbot_decoders::v4_swap_decoder::V4PoolId`) via the direct path dependency
// in `bot::engine`. The state-coupled dispatch layer (`LogDecoder`,
// `DecodedPoolEvent`, `LogDispatcher`) stays in `degenbot-bot`'s
// `bot_core::log_dispatcher`.

// The Uniswap-protocol domain crate `degenbot-uniswap` (Plan 105) holds the
// DEX identity presets (`DexIdentity`/`DexVariant`/`ReservesAbi`) and the V2
// swap callldata encoder (`encode_v2_swap`/`EncodedCall`). No `pub use`
// re-export: the binding layer reaches these via the direct path dependency in
// `bot::dex_identity` (and `bot_core` reaches `v2_encoding` directly).

// Re-export commonly used items at the crate root
pub use address_utils::{parse_address, to_checksum_address_bytes, to_checksum_address_str};
pub use hex_utils::{decode_hex, encode_hex, HexError};
#[cfg(feature = "uniswap")]
pub use uniswap::address::to_checksum_address;

#[cfg(feature = "concentrated-liquidity-math")]
pub use concentrated_liquidity_math::tick_math::{get_sqrt_ratio_at_tick, get_tick_at_sqrt_ratio};
#[cfg(feature = "concentrated-liquidity-math")]
pub use degenbot_math::cl::tick_math::{
    get_sqrt_ratio_at_tick_internal, get_tick_at_sqrt_ratio_internal, MAX_SQRT_RATIO,
    MIN_SQRT_RATIO,
};
#[cfg(feature = "concentrated-liquidity-math")]
pub use degenbot_math::cl::{
    bit_math, full_math, functions, liquidity_math, sqrt_price_math, swap_math, unsafe_math,
};
pub use errors::{AbiDecodeError, AddressError, ClMathError, ProviderError, TickMathError};

/// Ensure Python is initialized before the test harness spawns threads.
///
/// Without this, multiple test threads racing to call `Python::attach()` can
/// trigger `Py_InitializeEx()` concurrently. CPython's `Py_InitializeEx()` sets
/// `Py_IsInitialized()` before completing all setup (e.g. importing site.py),
/// so a second thread that sees the flag and proceeds can hit
/// `_PyImport_Init: global import state already initialized`.
///
/// The `ctor` attribute runs this before `main()`, guaranteeing single-threaded
/// initialization regardless of how many threads the test harness later spawns.
///
/// # Safety
///
/// This is safe because `Python::initialize()` uses a `std::sync::Once` guard
/// internally and only calls `Py_InitializeEx()` when the interpreter is not
/// yet running, making multiple calls harmless.
#[cfg(test)]
#[ctor::ctor(unsafe)]
fn init_python_before_test_threads() {
    pyo3::Python::initialize();
}

use pyo3::prelude::*;

#[pymodule]
fn _ffi(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Initialize tracing subscriber stack with batched Python-forwarding layer.
    // Replaces the previous `pyo3_log::init()` (per-record GIL round-trip)
    // with a `tracing_subscriber` registry + a custom `PythonLogLayer` that
    // batches events and flushes to Python `logging` via one `Python::attach`
    // per batch. Stays in the module init (not `c_api::register`) because it
    // is module-lifecycle setup, not symbol registration.
    // KAHU5W boot wiring: install the typed BotConfig BEFORE any
    // subscriber/failure-policy/engine code reads the holder. The loader is
    // the ONLY env-reading site; without this install every production run
    // observed schema defaults (metrics bound 127.0.0.1, default debounce),
    // silently ignoring DEGENBOT_* env.
    // JLFE2F (Option B hard cutover): the standard file layer is LIVE —
    // DEGENBOT_CONFIG (or ~/.config/degenbot/config.toml) feeds the typed
    // BotConfig; the retired pre-0.6 vocabulary ([rpc]/[ws]/[database]/
    // [otel]/default_chain_id) fails the load with pointed migration
    // errors (docs/config-migration.md). [failure_policy] is a sanctioned
    // free-form table the loader skips; the reader below consumes it from
    // the SAME file via the single standard_file_path() contract.
    // ADR-040 D3: per-bucket failure-policy overrides, boot-validated. An
    // invalid bucket/action is a boot ERROR (process exits) — the operator
    // asked for a specific containment stance; silently ignoring it would
    // trade on a policy the process does not actually have.
    let config_file = ::degenbot_config::standard_file_path();
    match ::degenbot_config::BotConfigLoader::new()
        .with_standard_file_paths()
        .load()
    {
        Ok(loaded) => {
            // First-wins: a test harness or an embedding that installed
            // earlier keeps ITS config; this is the production boot path.
            let _ = degenbot_bot::bot_core::stance::install(std::sync::Arc::new(loaded.config));
        }
        Err(e) => {
            #[expect(clippy::print_stderr)]
            {
                eprintln!("invalid configuration - boot refused: {e}");
            }
            #[expect(clippy::exit)]
            std::process::exit(2);
        }
    }
    match python_log_layer::read_failure_policy_overrides(config_file.as_deref()) {
        Ok(overrides) if !overrides.is_empty() => {
            let refs: Vec<(&str, &str)> = overrides
                .iter()
                .map(|(k, a)| (k.as_str(), a.as_str()))
                .collect();
            if let Err(e) = degenbot_bot::failure_policy::install_overrides(refs) {
                // DELIBERATE fail-loud import seam: module init has no
                // subscriber yet and no trading surface is up, so a refused
                // policy exits the process before any boot can proceed on a
                // half-read containment stance. Both lints are suppressed for
                // this one seam (the same predicates block_pump.rs grants its
                // pre-abort stderr marker).
                #[expect(clippy::print_stderr)]
                {
                    eprintln!("invalid override - boot refused: {e}");
                }
                #[expect(clippy::exit)]
                std::process::exit(2);
            }
            // ADR-040 D3 loudness: a softened tainted bucket must be visible
            // as an operator DECISION on the boot surface, not a silent count.
            // (Softening = any kind whose default is quarantine/exit set to a
            // weaker action; the pair list makes it greppable.)
            let pairs = overrides
                .iter()
                .map(|(k, a)| format!("{k}={a}"))
                .collect::<Vec<_>>()
                .join(", ");
            op_info!(domain = pump, count = overrides.len(),
                overrides = %pairs,
                "failure_policy overrides installed"
            );
        }
        Ok(_) => {}
        // A malformed [failure_policy] VALUES table is a boot error (ADR-040
        // D3): the operator asked for a containment stance; refuse loudly.
        Err(e) => {
            // DELIBERATE fail-loud import seam (see the Ok arm above).
            #[expect(clippy::print_stderr)]
            {
                eprintln!("invalid override table - boot refused: {e}");
            }
            #[expect(clippy::exit)]
            std::process::exit(2);
        }
    }

    python_log_layer::init_logging_subscriber();

    // ADR-043 §5 migration safety net: loudly name any retired verbosity env
    // name still present in the environment (detection, not compatibility).
    degenbot_core::telemetry::warn_retired_env_names();

    // GOQWCL (incident 2026-08-21): bind pyo3-async-runtimes to the shared
    // bot runtime instead of letting the first `future_into_py` call lazily
    // spawn a SECOND nproc-worker multi-thread runtime mid-run (observed as
    // 24 surprise worker threads appearing at the wedge timestamp). The
    // singleton is created on first use either way — this just makes it the
    // ONE runtime, created deterministically at import.
    if pyo3_async_runtimes::tokio::init_with_runtime(degenbot_core::runtime::get_runtime()).is_err()
    {
        diag!(
            domain = pump,
            "pyo3_async_runtimes already bound to a runtime"
        );
    }

    // Soak-2026-08-22 forensics + ADR-043 §2: the panic hook lives in the
    // observability facade (degenbot_core::telemetry::install_panic_hook) so
    // the pure-Rust core and this binding share ONE contract — exactly one
    // ERROR carrying the panic payload + thread name, with the active span
    // marked ERROR so the OTel layer exports it as a span exception. The hook
    // chains the previous hook, so the default backtrace still prints, and
    // fires for ANY panic anywhere (PythonLogLayer forwards it to Python).
    degenbot_core::telemetry::install_panic_hook();

    // PE4FPM: dump the ONE structured worker-census boot line with the full
    // table (see degenbot_core::worker_census). Resources that boot lazily
    // (solve executor, drainers, sim slots) register later and emit their
    // own census line on first use — the metric gauge picks every row up
    // through the export hook either way.
    degenbot_core::worker_census::emit_boot_table();

    // Register the shutdown pyfunction on the module.
    python_log_layer::PythonLogLayer::register_pyfunction(m)?;

    // Register every `#[pyfunction]`/`#[pyclass]` surface on the module.
    // See `c_api.rs` (ergo UG6FKN task KFVI5F) — mirrors polars-python's
    // `c_api/mod.rs` registration site.
    c_api::register(m)
}
