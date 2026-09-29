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

// Opt-in allocator swap for churn-heavy workloads (missed-WS-pong follow-up,
// RSS-growth investigation). The measured pathology was glibc free-page
// retention across per-thread arenas (system vs in-use spread of gigabytes,
// reclaimed on demand by malloc_trim). mimalloc returns freed segments to the
// OS aggressively instead of pooling them, at the cost of the system
// allocator's free-list caching. TUNING: the purge cadence
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
/// Ambient-runtime driver seam: enter the shared degenbot-core
/// runtime around a Python callable. Unconditional — degenbot-core is.
pub mod ambient_runtime;
#[cfg(feature = "balancer-math")]
pub mod balancer_math;
#[cfg(feature = "bot")]
pub mod bot;
/// Build identity: monotonic build counter baked by `build.rs` (stale-
/// `.so` detector, AGENTS.md). Unconditional — see `build_info.rs`.
pub mod build_info;
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
/// The `PyO3` projection of the core registration outcome ledger (S12).
pub mod registration;
#[cfg(feature = "rpc")]
pub mod rpc;
/// FF-T5: the runtime fleet status — budget, plan, census
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
pub use hex_utils::{decode_hex, HexError};
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

/// Install the long-running-driver stack: the ONE shared runtime bound to
/// pyo3-async, the tracing subscriber + Rust→Python log drainer, the metrics
/// scrape thread, the panic hook, and the worker-census boot dump.
///
/// Idempotent — each piece guards its own once-registration, so a second call
/// reallocates nothing. The async seams additionally call
/// [`ambient_runtime::ensure_async_runtime_bound`] themselves, so a driver
/// that forgets this call degrades to a deterministic first-use boot rather
/// than a missing subscriber.
static PANIC_HOOK: std::sync::OnceLock<()> = std::sync::OnceLock::new();

#[pyfunction]
fn driver_boot(_py: Python<'_>) {
    ambient_runtime::ensure_async_runtime_bound();

    // Soak-2026-08-22 forensics + ADR-043 §2: the panic hook lives in the
    // observability facade (degenbot_core::telemetry::install_panic_hook) so
    // the pure-Rust core and this binding share ONE contract — exactly one
    // ERROR carrying the panic payload + thread name, with the active span
    // marked ERROR so the OTel layer exports it as a span exception. The hook
    // chains the previous hook, so the default backtrace still prints, and
    // fires for ANY panic anywhere (PythonLogLayer forwards it to Python).
    PANIC_HOOK.get_or_init(degenbot_core::telemetry::install_panic_hook);

    python_log_layer::init_logging_subscriber();

    // dump the ONE structured worker-census boot line with the full
    // table (see degenbot_core::worker_census). Resources that boot lazily
    // (solve executor, drainers, sim slots) register later and emit their
    // own census line on first use — the metric gauge picks every row up
    // through the export hook either way.
    degenbot_core::worker_census::emit_boot_table();
}

// The declarative root of the `degenbot._ffi` module tree.
//
// Declarative `#[pymodule] mod` form (ADR-066 prerequisite, task TGFHFG):
// the fn-based `#[pymodule]` expansion passed empty member lists and the
// incomplete flag to `experimental-inspect` introspection, so the generated
// stubs could not see any member. Every root function/class below is a
// `#[pymodule_export] use`, every submodule a declarative `#[pymodule]`
// living beside its definitions (e.g. `crate::abi::abi`), so the macro
// expansion records the full member tree. Registration order and names
// mirror the pre-conversion `c_api::register` body; the one imperative
// island left is the `create_exception!` types (they have no `_PYO3_DEF`
// and so cannot be exported declaratively) plus the `sys.modules`
// submodule entries — both in `init`.
#[pymodule]
mod _ffi {
    use degenbot_core::op_info;
    use pyo3::prelude::*;

    // Initialize tracing subscriber stack with batched Python-forwarding layer.
    // Replaces the previous `pyo3_log::init()` (per-record GIL round-trip)
    // with a `tracing_subscriber` registry + a custom `PythonLogLayer` that
    // batches events and flushes to Python `logging` via one `Python::attach`
    // per batch. Stays in the module init (not a registration site) because
    // it is module-lifecycle setup, not symbol registration.
    //
    // Everything the long-running Python driver needs (runtimes, subscriber +
    // drainer, metrics scrape, panic hook, census dump) lives behind the
    // explicit `driver_boot()` — a one-shot consumer (the console passthrough,
    // a plain library import) pays none of it at import.
    #[pymodule_export]
    use crate::driver_boot;

    // The shutdown pyfunctions on the module (pre-conversion:
    // `PythonLogLayer::register_pyfunction`).
    #[pymodule_export]
    use crate::python_log_layer::{flush_telemetry, shutdown_log_drainer};

    // Build identity (stale-.so detector, AGENTS.md): the monotonic build
    // counter `build.rs` bakes into every compile of this cdylib.
    // Unconditional — the freshness check must work in every configuration.
    #[pymodule_export]
    use crate::build_info::{build_fingerprint, build_number};

    // ADR-051 D3: the Python console entry (degenbot._cli:main) forwards argv
    // verbatim into the Rust console. Registered unconditionally - the console
    // passthrough is core, not a feature.
    #[pymodule_export]
    use crate::cli::cli_main;

    // FF-T5: the runtime fleet status — budget, plan,
    // census ("degenbot.runtime_status()").
    #[pymodule_export]
    use crate::runtime_status::runtime_status;

    // ADR-062 D7/D10: the resolved config verdict. ONE frozen object carries
    // every declared key, the layer each came from, and the resolutions that
    // need a capability or an override, so the seam stops growing one
    // function per config key. Registered unconditionally: the loader that
    // produced the layers is unconditional.
    #[pymodule_export]
    use crate::config::{
        resolve_hypothetical, resolve_hypothetical_chain_id, resolve_hypothetical_database_path,
        resolve_hypothetical_node_uri, resolved_config, verification_retry_policy_defaults,
        HypotheticalConfig, ResolvedChainId, ResolvedConfig, ResolvedDatabasePath, ResolvedNodeUri,
        RetryPolicy, RetryPolicyDefaults, StrategyReadinessView,
    };

    // Ambient-runtime driver seam: lets a Python driver satisfy the
    // ambient-runtime-only policy on the verify seams. Unconditional —
    // degenbot-core (the runtime singleton) is.
    #[pymodule_export]
    use crate::ambient_runtime::{call_blocking_on_ambient_runtime, call_on_ambient_runtime};

    // Keccak256 + event topic (always a dependency;)
    #[pymodule_export]
    use crate::crypto::{event_topic, keccak256};

    // Address utilities (feature = "uniswap"): the Uniswap V2/V3
    // pool-address derivations + the generic EIP-1014 primitive — FFI
    // exposure of `degenbot-uniswap`'s pure-Rust `create2` family; the
    // Python counterparts in `src/degenbot/uniswap/v{2,3}_functions.py` and
    // `src/degenbot/contract/addresses.py` delegate here.
    #[cfg(feature = "uniswap")]
    #[pymodule_export]
    use crate::uniswap::address::{
        compute_aerodrome_v2_pool_address, compute_aerodrome_v3_pool_address, create2_address,
        generate_v2_pool_address, generate_v3_pool_address, to_checksum_address,
    };

    // Pathfinding graph + DFS (feature = "pathfinding").
    #[cfg(feature = "pathfinding")]
    #[pymodule_export]
    use crate::pathfinding::{
        classify_pool_kind, classify_pool_kinds, convert_pool_type_filter, find_paths_async_rust,
        find_paths_rust, prepare_traversal_plan, PathBatchIterator, PathIterator, PathStepBuilder,
        PoolKind,
    };
    // The build_path_graph seam choreographs a degenbot-db read + a
    // degenbot-pathfinding graph build, so it needs BOTH features.
    #[cfg(all(feature = "pathfinding", feature = "db"))]
    #[pymodule_export]
    use crate::pathfinding::build_path_graph;

    // S12: the core registration outcome ledger + its bounded tag vocabulary
    // (`degenbot_bot::bot_core::registration_ledger`), projected for the
    // Python registration pipeline. The tags are exported so Python builds its
    // label enum FROM the core vocabulary rather than re-declaring it.
    #[pymodule_export]
    use crate::registration::{
        classify_build_refusal, registration_outcome_tags, registration_pool_memo_key,
        PyBuildRefusal, PyRegistrationLedger, PyUnregistrablePoolRecord,
    };

    // Uniswap mixed V2/V3/V4 engine (feature = "bot") + the block-stream
    // async iterator (the authoritative `newHeads`-derived block clock) +
    // the cockpit session-phase table (`strategy_host::SessionPhase`),
    // exposed so the Python `_Phase` translates the host's verdict instead
    // of authoring the legal-state matrix.
    #[cfg(feature = "bot")]
    #[pymodule_export]
    use crate::bot::engine::{session_phase_next, BlockStream, PyArbEngine};

    // Bot — Rust-owned state (feature = "bot"). `PyIntakeReceipt` and
    // `PyConcentratedLiquidityView` keep the (missing) gate they had in the
    // pre-conversion registration body — mirrored, not fixed, so the surface
    // stays byte-equivalent; `bot` is a default feature so every real build
    // compiles them.
    #[pymodule_export]
    use crate::bot::intake::PyIntakeReceipt;
    #[pymodule_export]
    use crate::bot::pool::PyConcentratedLiquidityView;
    #[cfg(feature = "bot")]
    #[pymodule_export]
    use crate::bot::pool::{PyBalanceVectorView, PyPoolTickCoverage, PyReservePairView};
    #[cfg(feature = "bot")]
    #[pymodule_export]
    use crate::bot::token::PyErc20Token;
    #[cfg(feature = "bot")]
    #[pymodule_export]
    use crate::bot::{PyBot, PyLiquidityPool};
    // Session object identity — the canonical name the session's pool/token
    // registries resolve through (thin projection of the Rust
    // `SessionObjectRegistry`; the registry stays the identity authority).
    #[cfg(feature = "bot")]
    #[pymodule_export]
    use crate::bot::py_bot_io::PyBotIo;
    #[cfg(all(feature = "bot", feature = "db"))]
    #[pymodule_export]
    use crate::bot::py_bot_io::PyErc20TokenRow;
    #[cfg(feature = "bot")]
    #[pymodule_export]
    use crate::bot::session_registry::PySessionObject;

    // The core fee-history percentile pair the settlement driver polls. A
    // module function like `verification_retry_policy_defaults`: a core
    // default, not a member of the resolved config verdict.
    #[cfg(feature = "simulation")]
    #[pymodule_export]
    use crate::simulation::dispatch::fee_percentiles;

    // `QuantAMM` closed-form N-token Balancer weighted basket solver
    // (feature = "bot") — `solve_balancer_weighted_basket`.
    #[cfg(feature = "bot")]
    #[pymodule_export]
    use crate::solvers_basket::solve_balancer_weighted_basket;

    // ── Submodules (declarative `#[pymodule]`s beside their definitions;
    //    registration order mirrors the pre-conversion body) ──────────────

    // Concentrated-liquidity math (feature = "concentrated-liquidity-math") —
    // 21 fns + 4 tick-boundary constants, un-prefixed. See
    // `crate::concentrated_liquidity_math::concentrated_liquidity_math`.
    #[cfg(feature = "concentrated-liquidity-math")]
    #[pymodule_export]
    use crate::concentrated_liquidity_math::concentrated_liquidity_math;

    // Solady LibZip (FastLZ) compress/decompress — lives in `degenbot-core`
    // (always a dependency), so no feature gate.
    #[pymodule_export]
    use crate::solady::solady;

    #[cfg(feature = "balancer-math")]
    #[pymodule_export]
    use crate::balancer_math::lib::balancer_math;

    #[cfg(feature = "curve-math")]
    #[pymodule_export]
    use crate::curve_math::lib::curve_math;

    #[cfg(feature = "curve-math")]
    #[pymodule_export]
    use crate::curve_dy::lib::curve_dy;

    #[cfg(feature = "solidly-math")]
    #[pymodule_export]
    use crate::solidly_math::lib::solidly_math;

    #[cfg(feature = "v2-math")]
    #[pymodule_export]
    use crate::v2_math::lib::v2_math;

    #[cfg(feature = "db")]
    #[pymodule_export]
    use crate::db::db;

    // EIP-1559 base fee (next_base_fee) — always on (degenbot-core is a
    // non-optional, no-extra-feature dep).
    #[pymodule_export]
    use crate::eip_1559::eip_1559;

    // `CancelHandle` — the cooperative cancel flag for the updater loops
    // (`run_pool_update`, `run_aave_update`). Gated on either updater feature
    // (whichever needs it); registered once, as its own submodule.
    #[cfg(any(feature = "pool", feature = "aave-updater"))]
    #[pymodule_export]
    use crate::cancel::cancel;

    // Pool-updater chunk-loop seam (feature = "pool") — `run_pool_update`.
    #[cfg(feature = "pool")]
    #[pymodule_export]
    use crate::pool::pool;

    // Aave-updater chunk-loop seam (feature = "aave-updater") —
    // `run_aave_update` (Python name `aave`, mirroring the pre-conversion
    // `degenbot._ffi.aave` submodule name).
    #[cfg(feature = "aave-updater")]
    #[pymodule_export]
    use crate::aave_updater::aave;

    // Command-stream encoding seam (feature = "executor").
    #[cfg(feature = "executor")]
    #[pymodule_export]
    use crate::executor::executor;

    // ExecutionAdapter seam lift (feature = "execution") — `PySolveResult`,
    // `PyPayloadComposer`, `abi_encode_call` (ADR-025). Foreign-contract path;
    // never threaded into the canonical dispatch fan-out (D3).
    #[cfg(feature = "execution")]
    #[pymodule_export]
    use crate::execution::execution;

    // Anvil-fork seam (feature = "fork") — `PyAnvilFork` over the
    // `degenbot-fork` core crate. Lifecycle + dev-RPC.
    #[cfg(feature = "fork")]
    #[pymodule_export]
    use crate::fork::fork;

    // ABI decoder/encoder functions (feature = "abi").
    #[cfg(feature = "abi")]
    #[pymodule_export]
    use crate::abi::abi;

    // Provider + contract + subscription + backrun modules (feature = "rpc").
    #[cfg(feature = "rpc")]
    #[pymodule_export]
    use crate::rpc::provider::provider;

    #[cfg(feature = "rpc")]
    #[pymodule_export]
    use crate::rpc::backrun_py::backrun;

    #[cfg(feature = "rpc")]
    #[pymodule_export]
    use crate::rpc::contract::contract;

    #[cfg(feature = "bot")]
    #[pymodule_export]
    use crate::bot::dex_identity::dex_identity_pymodule;

    // Deployment-identity lookup over the embedded deployments.json
    // (Fork A, 7FA5EZ) (feature = "bot").
    #[cfg(feature = "bot")]
    #[pymodule_export]
    use crate::bot::deployments::deployments;

    // Price-reader seam (feature = "price").
    #[cfg(feature = "price")]
    #[pymodule_export]
    use crate::price::price;

    // Submission seam (feature = "submission").
    #[cfg(feature = "submission")]
    #[pymodule_export]
    use crate::submission::submission;

    // Diagnostics instrumentation: GIL-acquire-latency probe
    // + main-loop stuck-watchdog. Unconditional (no feature gate) so the
    // probe is available in every build; the example opts in at startup.
    #[pymodule_export]
    use crate::diagnostics::gil_probe::diagnostics;

    // Simulation seam (feature = "simulation") — the PyO3 binding over
    // `degenbot-arbitrage` (per-block profitability pipeline).
    #[cfg(feature = "simulation")]
    #[pymodule_export]
    use crate::simulation::simulation;

    // Fleet operator seam (feature = "simulation") — the JCI2FW Part B
    // runtime re-tune channel over the process posture owner (the mirror
    // home is `degenbot.fleet`; the operator op is `set_fleet_posture`).
    #[cfg(feature = "simulation")]
    #[pymodule_export]
    use crate::fleet::fleet;

    /// Module-lifecycle setup: the typed-config install, the failure-policy
    /// overrides, the retired-env warnings, the `create_exception!` island,
    /// and the `sys.modules` entries for the declarative submodules.
    ///
    /// Runs AFTER the declarative member registration (the expansion calls it
    /// last) — a reordering with no observable effect: the only failure exit
    /// is `std::process::exit(2)`, which never returns to Python either way.
    #[pymodule_init]
    fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
        // KAHU5W boot wiring: install the typed BotConfig BEFORE any
        // subscriber/failure-policy/engine code reads the holder. The loader is
        // the ONLY env-reading site; without this install every production run
        // observed schema defaults (metrics bound 127.0.0.1, default debounce),
        // silently ignoring DEGENBOT_* env.
        // Option B hard cutover: the standard file layer is LIVE —
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
        match ::degenbot_config::load_process_config() {
            Ok(loaded) => {
                // First-wins: a test harness or an embedding that installed
                // earlier keeps ITS config; this is the production boot path.
                let installed = degenbot_bot::bot_core::stance::install(std::sync::Arc::new(
                    loaded.config.clone(),
                ));
                // The driver-domain resolvers read the LAYERS, not just the typed
                // value, so they need the provenance this same load produced. One
                // load, published once: a resolver cannot see a different file or
                // environment than the holder received.
                crate::config::publish_loaded(loaded, installed);
                // The verdict is the seam's one object, so it is built from the
                // layers this same load published rather than on first read.
                crate::config::install_verdict();
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
        match crate::python_log_layer::read_failure_policy_overrides(config_file.as_deref()) {
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

        // ADR-043 §5 migration safety net: loudly name any retired verbosity env
        // name still present in the environment (detection, not compatibility).
        degenbot_core::telemetry::warn_retired_env_names();

        register_exception_types(m)?;
        register_submodules_in_sys(m)
    }

    /// The `create_exception!` exception types. They carry no `_PYO3_DEF`
    /// (only `#[pyclass]`/`#[pyfunction]`/`#[pymodule]` items do), so they
    /// cannot take `#[pymodule_export]` — this is the one imperative
    /// registration island left on the root module.
    fn register_exception_types(m: &Bound<'_, PyModule>) -> PyResult<()> {
        let py = m.py();
        // The chain-identity refusal (ADR-062 D8): a distinct type so a driver
        // can read the expected and actual chain ids off the exception instead
        // of matching a message. (feature = "rpc")
        #[cfg(feature = "rpc")]
        m.add(
            "ChainMismatchError",
            py.get_type::<crate::rpc::errors::ChainMismatchError>(),
        )?;

        // Typed verification exceptions (TODO-53b7453b): distinct `RuntimeError`
        // subclasses so `build_paths` can classify verification failures by type
        // instead of fragile string matching. (feature = "bot")
        #[cfg(feature = "bot")]
        {
            m.add(
                "VerificationMismatchError",
                py.get_type::<crate::bot::engine::VerificationMismatchError>(),
            )?;
            m.add(
                "VerificationRpcError",
                py.get_type::<crate::bot::engine::VerificationRpcError>(),
            )?;
            // FF-T1: the fleet boot refusal surfaces as the typed
            // BootRefused exception — the library never aborts the host process
            // on the boot-refusal arm; the degenbot binary maps it to its loud
            // named fail-fast exit.
            m.add(
                "BootRefused",
                py.get_type::<crate::bot::engine::BootRefused>(),
            )?;
            // the Faulted intake drain (spike S2) surfaces as a typed
            // receipt exception.
            m.add(
                "FleetIntakeFaultedError",
                py.get_type::<crate::bot::engine::FleetIntakeFaultedError>(),
            )?;
            // The construction route's loud refusal (ADR-055 D4).
            m.add(
                "UnsupportedPoolFamilyError",
                py.get_type::<crate::bot::engine::UnsupportedPoolFamilyError>(),
            )?;
            // Typed pool-admission exceptions (Plan 102, F2EVV6): a unified
            // `PoolRegistrationError` hierarchy so `build_paths` can classify
            // V2/V3/V4 admission refusals by type instead of fragile string
            // matching. The V4-specific `HookedPoolRejectedError` /
            // `DynamicFeePoolRejectedError` reparent under
            // `PoolRegistrationError`; `PoolAlreadyRegisteredError` +
            // `SpecViolationError` are the unified admission categories shared
            // by V2/V3/V4.
            m.add(
                "PoolRegistrationError",
                py.get_type::<crate::bot::engine::PoolRegistrationError>(),
            )?;
            m.add(
                "HookedPoolRejectedError",
                py.get_type::<crate::bot::engine::HookedPoolRejectedError>(),
            )?;
            m.add(
                "DynamicFeePoolRejectedError",
                py.get_type::<crate::bot::engine::DynamicFeePoolRejectedError>(),
            )?;
            m.add(
                "PossibleInaccurateResult",
                py.get_type::<crate::bot::engine::PossibleInaccurateResult>(),
            )?;
            m.add(
                "HighFeePoolRejectedError",
                py.get_type::<crate::bot::engine::HighFeePoolRejectedError>(),
            )?;
            m.add(
                "PoolAlreadyRegisteredError",
                py.get_type::<crate::bot::engine::PoolAlreadyRegisteredError>(),
            )?;
            m.add(
                "SpecViolationError",
                py.get_type::<crate::bot::engine::SpecViolationError>(),
            )?;
            // PRG-4: the registered-path cap refusal — a BENIGN stop signal
            // the crawl catches instead of a Python counter unwind.
            m.add(
                "PathRegistryFullError",
                py.get_type::<crate::bot::engine::PathRegistryFullError>(),
            )?;
            // Strategy-host operator refusals: typed so an unknown or
            // unconfigured strategy is classifiable by type, not message.
            m.add(
                "StrategyHostError",
                py.get_type::<crate::bot::engine::StrategyHostError>(),
            )?;
            m.add(
                "UnknownStrategyError",
                py.get_type::<crate::bot::engine::UnknownStrategyError>(),
            )?;
            m.add(
                "UnconfiguredStrategyError",
                py.get_type::<crate::bot::engine::UnconfiguredStrategyError>(),
            )?;
        }
        Ok(())
    }

    /// Populate `sys.modules["degenbot._ffi.<name>"]` for every registered
    /// submodule.
    ///
    /// Declarative submodule registration only sets the parent attribute
    /// (`add_submodule`) — it does NOT insert the `sys.modules` entry, and
    /// Python's import system needs that entry to traverse the
    /// `degenbot._ffi.<name>` dotted path (the extension module is a single
    /// file, not a package, so without it the import fails with
    /// `ModuleNotFoundError: 'degenbot._ffi' is not a package`). The
    /// pre-conversion imperative builders each inserted their own entry; this
    /// shared helper replaces all of them, gated to the same feature set.
    fn register_submodules_in_sys(m: &Bound<'_, PyModule>) -> PyResult<()> {
        let py = m.py();
        let sys_modules = py.import("sys")?.getattr("modules")?;
        // The unconditional entries lead the initializer; the feature-gated
        // remainder appends push-by-push so each name carries its own gate
        // mirroring the registration site above (order carries no meaning —
        // the sys.modules entries are independent set_items).
        let mut names: Vec<&'static str> = vec!["solady", "eip_1559", "diagnostics"];
        #[cfg(feature = "concentrated-liquidity-math")]
        names.push("concentrated_liquidity_math");
        #[cfg(feature = "balancer-math")]
        names.push("balancer_math");
        #[cfg(feature = "curve-math")]
        names.push("curve_math");
        #[cfg(feature = "curve-math")]
        names.push("curve_dy");
        #[cfg(feature = "solidly-math")]
        names.push("solidly_math");
        #[cfg(feature = "v2-math")]
        names.push("v2_math");
        #[cfg(feature = "db")]
        names.push("db");
        #[cfg(any(feature = "pool", feature = "aave-updater"))]
        names.push("cancel");
        #[cfg(feature = "pool")]
        names.push("pool");
        #[cfg(feature = "aave-updater")]
        names.push("aave");
        #[cfg(feature = "executor")]
        names.push("executor");
        #[cfg(feature = "execution")]
        names.push("execution");
        #[cfg(feature = "fork")]
        names.push("fork");
        #[cfg(feature = "abi")]
        names.push("abi");
        #[cfg(feature = "rpc")]
        names.push("provider");
        #[cfg(feature = "rpc")]
        names.push("backrun");
        #[cfg(feature = "rpc")]
        names.push("contract");
        #[cfg(feature = "bot")]
        names.push("dex_identity");
        #[cfg(feature = "bot")]
        names.push("deployments");
        #[cfg(feature = "price")]
        names.push("price");
        #[cfg(feature = "submission")]
        names.push("submission");
        #[cfg(feature = "simulation")]
        names.push("simulation");
        #[cfg(feature = "simulation")]
        names.push("fleet");

        for name in names {
            let dotted = format!("degenbot._ffi.{name}");
            sys_modules.set_item(dotted, m.getattr(name)?)?;
        }
        Ok(())
    }
}
