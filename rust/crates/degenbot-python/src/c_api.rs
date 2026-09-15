//! `#[pymodule]` registration — the single place `#[pyfunction]`/`#[pyclass]`
//! symbols are bound onto the Python `degenbot._ffi` module tree.
//!
//! Mirrors `polars-python/src/c_api/mod.rs`: the binding crate's `lib.rs` is a
//! thin module-tree + re-export file, and every `m.add_function(...)` /
//! `m.add_class::<...>()` call lives here. Domain `#[pyfunction]`/`#[pyclass]`
//! DEFINITIONS live in the domain modules under `src/` (e.g. `solvers_basket.rs`,
//! `db/mod.rs`) — only their registration calls come here.
//!
//! Documented exception to the single place: the tracing subscriber init and
//! `python_log_layer::PythonLogLayer::register_pyfunction` (which registers
//! `shutdown_log_drainer`) run in `lib.rs`'s `#[pymodule]` init — that is
//! module-lifecycle setup, not symbol registration.

use pyo3::prelude::*;
use pyo3::wrap_pyfunction;

/// Register every Rust-wrapped symbol on the Python module `m`.
///
/// Order and set of registered symbols must stay byte-equivalent to the
/// pre-extraction `#[pymodule]` body . The tracing
/// subscriber init (`python_log_layer::init_logging_subscriber()`) stays in
/// `lib.rs`'s `#[pymodule]` because it is module-lifecycle setup, not symbol
/// registration.
///
/// # Errors
///
/// Returns a [`PyErr`] if any individual `add_class`/`add_function`/`add` call
/// fails (e.g. a name collision). Errors are propagated unchanged — the
/// `#[pymodule]` caller in `lib.rs` converts them into the module-init failure.
#[expect(clippy::too_many_lines)]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Concentrated-liquidity math (feature = "concentrated-liquidity-math") — registered on a real
    // Python submodule `degenbot._ffi.concentrated_liquidity_math` (21 fns + 4 tick-boundary
    // constants, un-prefixed). See `crate::concentrated_liquidity_math::add_concentrated_liquidity_math_module`.
    #[cfg(feature = "concentrated-liquidity-math")]
    crate::concentrated_liquidity_math::add_concentrated_liquidity_math_module(m)?;

    // Build identity (stale-.so detector, AGENTS.md): the monotonic build
    // counter `build.rs` bakes into every compile of this cdylib.
    // Unconditional — the freshness check must work in every configuration.
    m.add_function(wrap_pyfunction!(crate::build_info::build_number, m)?)?;
    m.add_function(wrap_pyfunction!(crate::build_info::build_fingerprint, m)?)?;

    // ADR-051 D3: the Python console entry (degenbot._cli:main) forwards argv
    // verbatim into the Rust console. Registered unconditionally - the console
    // passthrough is core, not a feature.
    m.add_function(wrap_pyfunction!(crate::cli::cli_main, m)?)?;

    // FF-T5 (NT7HJC): the runtime fleet status — budget, plan,
    // census ("degenbot.runtime_status()").
    m.add_function(wrap_pyfunction!(crate::runtime_status::runtime_status, m)?)?;

    // 4IOEVT: the typed discovery-delivery batch size
    // (`pathfinding.discovery_batch_size` / `DEGENBOT_DISCOVERY_BATCH_SIZE`).
    // The Python discovery pipeline reads it and forwards it to
    // `find_paths_async`.
    m.add_function(wrap_pyfunction!(crate::config::discovery_batch_size, m)?)?;

    // 6LC4JB: the shared core verification-retry policy defaults, so the
    // Python driver shell reads them from the one Rust-owned declaration site.
    // The pyclass (`RetryPolicyDefaults`) is self-describing: a Rust-side
    // field reorder cannot silently mis-assign a positional tuple read.
    m.add_class::<crate::config::RetryPolicyDefaults>()?;
    m.add_function(wrap_pyfunction!(
        crate::config::verification_retry_policy_defaults,
        m
    )?)?;

    // Ambient-runtime driver seam (VJGZJ2): lets a Python driver satisfy the
    // ambient-runtime-only policy on the verify seams. Unconditional —
    // degenbot-core (the runtime singleton) is.
    m.add_function(wrap_pyfunction!(
        crate::ambient_runtime::call_on_ambient_runtime,
        m
    )?)?;
    // FYZMAF: the async sibling — offload a blocking Python prep step to the
    // shared runtime's blocking pool so the asyncio loop keeps turning.
    m.add_function(wrap_pyfunction!(
        crate::ambient_runtime::call_blocking_on_ambient_runtime,
        m
    )?)?;

    // Keccak256 + event topic (always a dependency;)
    m.add_function(wrap_pyfunction!(crate::crypto::keccak256, m)?)?;
    m.add_function(wrap_pyfunction!(crate::crypto::event_topic, m)?)?;

    // Address utilities (feature = "uniswap")
    #[cfg(feature = "uniswap")]
    {
        m.add_function(wrap_pyfunction!(
            crate::uniswap::address::to_checksum_address,
            m
        )?)?;
        m.add_function(wrap_pyfunction!(
            crate::uniswap::address::compute_aerodrome_v2_pool_address,
            m
        )?)?;
        m.add_function(wrap_pyfunction!(
            crate::uniswap::address::compute_aerodrome_v3_pool_address,
            m
        )?)?;
        // The Uniswap V2/V3 pool-address derivations + the generic
        // EIP-1014 primitive — FFI exposure of `degenbot-uniswap`'s pure-Rust
        // `create2` family; the Python counterparts in
        // `src/degenbot/uniswap/v{2,3}_functions.py` and
        // `src/degenbot/contract/addresses.py` delegate here.
        m.add_function(wrap_pyfunction!(
            crate::uniswap::address::create2_address,
            m
        )?)?;
        m.add_function(wrap_pyfunction!(
            crate::uniswap::address::generate_v2_pool_address,
            m
        )?)?;
        m.add_function(wrap_pyfunction!(
            crate::uniswap::address::generate_v3_pool_address,
            m
        )?)?;
    }

    // Solady LibZip (FastLZ) compress/decompress — lives in `degenbot-core`
    // (always a dependency), so no feature gate. Registered on a real
    // Python submodule `degenbot._ffi.solady`.
    crate::solady::add_solady_module(m)?;

    // Pathfinding graph + DFS (feature = "pathfinding")
    #[cfg(feature = "pathfinding")]
    m.add_function(wrap_pyfunction!(crate::pathfinding::find_paths_rust, m)?)?;
    // 4IOEVT: the batched async DFS iterator Python's find_paths_async drives.
    #[cfg(feature = "pathfinding")]
    m.add_function(wrap_pyfunction!(
        crate::pathfinding::find_paths_async_rust,
        m
    )?)?;
    // The build_path_graph seam choreographs a degenbot-db read + a
    // degenbot-pathfinding graph build, so it needs BOTH features.
    #[cfg(all(feature = "pathfinding", feature = "db"))]
    m.add_function(wrap_pyfunction!(crate::pathfinding::build_path_graph, m)?)?;
    #[cfg(feature = "pathfinding")]
    m.add_class::<crate::pathfinding::PathIterator>()?;
    #[cfg(feature = "pathfinding")]
    m.add_class::<crate::pathfinding::PathBatchIterator>()?;

    // Balancer V2 math library functions (feature = "balancer-math")
    #[cfg(feature = "balancer-math")]
    crate::balancer_math::lib::add_balancer_math_module(m)?;

    // Curve StableSwap math library functions (feature = "curve-math")
    #[cfg(feature = "curve-math")]
    crate::curve_math::lib::add_curve_math_module(m)?;

    // Curve get_dy calculator seam (feature = "curve-math")
    #[cfg(feature = "curve-math")]
    crate::curve_dy::lib::add_curve_dy_module(m)?;

    // Solidly / Aerodrome / Camelot stable-math library functions
    // (feature = "solidly-math")
    #[cfg(feature = "solidly-math")]
    crate::solidly_math::lib::add_solidly_math_module(m)?;

    // V2 constant-product (x*y=k) swap math (feature = "v2-math")
    #[cfg(feature = "v2-math")]
    crate::v2_math::lib::add_v2_math_module(m)?;

    // SQLite file operations (feature = "db")
    #[cfg(feature = "db")]
    crate::db::add_db_module(m)?;

    // EIP-1559 base fee (next_base_fee) — always on (degenbot-core is a non-optional,
    // no-extra-feature dep).
    crate::eip_1559::add_eip_1559_module(m)?;

    // `CancelHandle` — the cooperative cancel flag for the updater loops
    // (`run_pool_update`, `run_aave_update`). Gated on either updater feature
    // (whichever needs it); registered once, top-level. Lives in `cancel.rs`.
    #[cfg(any(feature = "pool", feature = "aave-updater"))]
    crate::cancel::register_cancel(m)?;

    // Pool-updater chunk-loop seam (feature = "pool") — `run_pool_update`.
    // Gates `db` + `rpc` (the chunk loop reads the DB + RPCs log fetches).
    // Task QZHNZQ; epic 2SFL6I. `CancelHandle` is registered above (shared).
    #[cfg(feature = "pool")]
    crate::pool::add_pool_module(m)?;

    // Aave-updater chunk-loop seam (feature = "aave-updater") —
    // `run_aave_update`. Gates `db` + `rpc` (mirrors the pool seam). Epic
    // AZGJUN, task 5XNTC5. `CancelHandle` is registered above (shared).
    #[cfg(feature = "aave-updater")]
    crate::aave_updater::add_aave_updater_module(m)?;

    // Command-stream encoding seam (feature = "executor")
    #[cfg(feature = "executor")]
    crate::executor::add_executor_module(m)?;

    // ExecutionStrategy seam lift (feature = "execution") — `PySolveResult`,
    // `PyPayloadComposer`, `abi_encode_call` (ADR-025). Foreign-contract path;
    // never threaded into the canonical dispatch fan-out (D3).
    #[cfg(feature = "execution")]
    crate::execution::add_execution_module(m)?;

    // Anvil-fork seam (feature = "fork") — `PyAnvilFork` over the
    // `degenbot-fork` core crate (epic NXYVYU). Lifecycle + dev-RPC.
    #[cfg(feature = "fork")]
    crate::fork::add_fork_module(m)?;

    // ABI decoder/encoder functions (feature = "abi") — registered on a real
    // Python submodule `degenbot._ffi.abi` (decode/decode_single/encode/
    // encode_single). See `crate::abi::add_abi_module`.
    #[cfg(feature = "abi")]
    crate::abi::add_abi_module(m)?;

    // Provider + contract + subscription modules (feature = "rpc")
    #[cfg(feature = "rpc")]
    crate::rpc::provider::add_provider_module(m)?;
    #[cfg(feature = "rpc")]
    crate::rpc::contract::add_contract_module(m)?;

    // Uniswap mixed V2/V3/V4 engine (feature = "bot")
    #[cfg(feature = "bot")]
    m.add_class::<crate::bot::engine::PyArbEngine>()?;
    // Block-stream async iterator  — the authoritative
    // `newHeads`-derived block clock, consumed by Python in parallel with
    // the result-batch iterator. See `engine::result_channel::BlockStream`.
    #[cfg(feature = "bot")]
    m.add_class::<crate::bot::engine::BlockStream>()?;

    // Typed verification exceptions (TODO-53b7453b): distinct `RuntimeError`
    // subclasses so `build_paths` can classify verification failures by type
    // instead of fragile string matching. (feature = "bot")
    #[cfg(feature = "bot")]
    m.add(
        "VerificationMismatchError",
        m.py()
            .get_type::<crate::bot::engine::VerificationMismatchError>(),
    )?;
    #[cfg(feature = "bot")]
    m.add(
        "VerificationRpcError",
        m.py()
            .get_type::<crate::bot::engine::VerificationRpcError>(),
    )?;

    // FF-T1 (BPHR6F): the fleet boot refusal surfaces as the typed
    // BootRefused exception — the library never aborts the host process
    // on the boot-refusal arm; the degenbot binary maps it to its loud
    // named fail-fast exit.
    #[cfg(feature = "bot")]
    m.add(
        "BootRefused",
        m.py().get_type::<crate::bot::engine::BootRefused>(),
    )?;

    // TB4QGX T6: the Faulted intake drain (spike S2) surfaces as a typed
    // receipt exception.
    #[cfg(feature = "bot")]
    m.add(
        "FleetIntakeFaultedError",
        m.py()
            .get_type::<crate::bot::engine::FleetIntakeFaultedError>(),
    )?;

    // Typed pool-admission exceptions (Plan 102, F2EVV6): a unified
    // `PoolRegistrationError` hierarchy so `build_paths` can classify
    // V2/V3/V4 admission refusals by type instead of fragile string
    // matching. The V4-specific `HookedPoolRejectedError` /
    // `DynamicFeePoolRejectedError` reparent under `PoolRegistrationError`;
    // `PoolAlreadyRegisteredError` + `SpecViolationError` are the unified
    // admission categories shared by V2/V3/V4. (feature = "bot")
    #[cfg(feature = "bot")]
    m.add(
        "PoolRegistrationError",
        m.py()
            .get_type::<crate::bot::engine::PoolRegistrationError>(),
    )?;
    #[cfg(feature = "bot")]
    m.add(
        "HookedPoolRejectedError",
        m.py()
            .get_type::<crate::bot::engine::HookedPoolRejectedError>(),
    )?;
    #[cfg(feature = "bot")]
    m.add(
        "DynamicFeePoolRejectedError",
        m.py()
            .get_type::<crate::bot::engine::DynamicFeePoolRejectedError>(),
    )?;
    #[cfg(feature = "bot")]
    m.add(
        "PossibleInaccurateResult",
        m.py()
            .get_type::<crate::bot::engine::PossibleInaccurateResult>(),
    )?;
    #[cfg(feature = "bot")]
    m.add(
        "HighFeePoolRejectedError",
        m.py()
            .get_type::<crate::bot::engine::HighFeePoolRejectedError>(),
    )?;
    #[cfg(feature = "bot")]
    m.add(
        "PoolAlreadyRegisteredError",
        m.py()
            .get_type::<crate::bot::engine::PoolAlreadyRegisteredError>(),
    )?;
    #[cfg(feature = "bot")]
    m.add(
        "SpecViolationError",
        m.py().get_type::<crate::bot::engine::SpecViolationError>(),
    )?;
    // PRG-4 / IRUMXD: the registered-path cap refusal — a BENIGN stop signal
    // the crawl catches instead of a Python counter unwind.
    #[cfg(feature = "bot")]
    m.add(
        "PathRegistryFullError",
        m.py()
            .get_type::<crate::bot::engine::PathRegistryFullError>(),
    )?;

    // Bot — Rust-owned state (feature = "bot")
    #[cfg(feature = "bot")]
    m.add_class::<crate::bot::PyBot>()?;
    m.add_class::<crate::bot::intake::PyIntakeReceipt>()?;
    #[cfg(feature = "bot")]
    m.add_class::<crate::bot::pool::PyLiquidityPool>()?;
    #[cfg(feature = "bot")]
    m.add_class::<crate::bot::pool::PyPool>()?;
    #[cfg(feature = "bot")]
    m.add_class::<crate::bot::pool::PyReservePairView>()?;
    #[cfg(feature = "bot")]
    m.add_class::<crate::bot::pool::PyConcentratedLiquidityView>()?;
    #[cfg(feature = "bot")]
    m.add_class::<crate::bot::pool::PyBalanceVectorView>()?;
    #[cfg(feature = "bot")]
    m.add_class::<crate::bot::token::PyErc20Token>()?;
    #[cfg(feature = "bot")]
    m.add_class::<crate::bot::py_bot_io::PyBotIo>()?;
    #[cfg(all(feature = "bot", feature = "db"))]
    m.add_class::<crate::bot::py_bot_io::PyErc20TokenRow>()?;

    // DEX identity presets (ADR-005 slice 6) (feature = "bot")
    #[cfg(feature = "bot")]
    crate::bot::dex_identity::add_dex_identity(m)?;

    // Deployment-identity lookup over the embedded deployments.json
    // (Fork A, 7FA5EZ) (feature = "bot")
    #[cfg(feature = "bot")]
    crate::bot::deployments::add_deployments(m)?;

    // Price-reader seam (feature = "price")
    #[cfg(feature = "price")]
    crate::price::add_price_module(m)?;

    // Submission seam (feature = "submission")
    #[cfg(feature = "submission")]
    crate::submission::add_submission_module(m)?;

    // Diagnostics instrumentation : GIL-acquire-latency probe
    // + main-loop stuck-watchdog. Unconditional (no feature gate) so the
    // probe is available in every build; the example opts in at startup.
    crate::diagnostics::add_diagnostics_module(m)?;

    // Simulation seam (feature = "simulation") — the PyO3 binding over
    // `degenbot-arbitrage` (per-block profitability pipeline:
    // `dispatch_profitable_results` + the 7-call `simulate_path_on_evm`,
    // driven over the `degenbot-simulation` engine's `BlockSimHandle`).
    #[cfg(feature = "simulation")]
    crate::simulation::add_simulation_module(m)?;

    // Fleet operator seam (feature = "simulation") — the JCI2FW Part B
    // runtime re-tune channel over the process posture owner. Registered on
    // a real Python submodule `degenbot._ffi.fleet` (the mirror home is
    // `degenbot.fleet`; the operator op is `set_fleet_posture`).
    #[cfg(feature = "simulation")]
    crate::fleet::add_fleet_module(m)?;

    // `QuantAMM` closed-form N-token Balancer weighted basket solver
    // (feature = "bot") — `solve_balancer_weighted_basket`.
    #[cfg(feature = "bot")]
    m.add_function(wrap_pyfunction!(
        crate::solvers_basket::solve_balancer_weighted_basket,
        m
    )?)?;

    Ok(())
}
