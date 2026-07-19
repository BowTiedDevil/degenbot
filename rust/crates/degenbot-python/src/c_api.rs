//! `#[pymodule]` registration — the single place `#[pyfunction]`/`#[pyclass]`
//! symbols are bound to the Python `degenbot.degenbot_rs` module.
//!
//! Mirrors `polars-python/src/c_api/mod.rs`: the binding crate's `lib.rs` is a
//! thin module-tree + re-export file, and every `m.add_function(...)` /
//! `m.add_class::<...>()` call lives here. Future `#[pyfunction]`/`#[pyclass]`
//! surface lands in this file (gated by cargo features in step 7 of the
//! binding-layer reorg — UG6FKN).

use pyo3::prelude::*;
use pyo3::wrap_pyfunction;

/// `QuantAMM` closed-form N-token Balancer weighted basket arbitrage solver.
///
/// Thin `PyO3` wrapper over [`degenbot_bot::solvers::balancer_weighted_basket::solve_balancer_weighted`].
/// Port of `BalancerMultiTokenSolver` / `solve_balancer_weighted` (Willetts &
/// Harrington, `QuantAMM` Equation 9).
///
/// # Arguments
///
/// - `reserves`: list of token reserves in wei.
/// - `weights`: list of normalized weights as 18-decimal fixed point (sum = 1e18).
/// - `fee_numer`, `fee_denom`: swap fee as the fraction `fee_numer / fee_denom`.
/// - `decimals`: list of decimal places per token; empty list = no scaling.
/// - `market_prices`: list of market prices per token (in numeraire).
/// - `max_input`: optional max total deposit value in numeraire units.
///
/// # Returns
///
/// `(trades, profit, success, signature, iterations)` — `trades` is a list of
/// native-token-integer amounts (positive = deposit, negative = withdraw).
///
/// # Errors
///
/// Returns `ValueError` if reserves and `market_prices` lengths don't match,
/// or if reserves/weights can't be converted to u128/u64.
#[cfg(feature = "bot")]
#[allow(
    clippy::too_many_arguments,
    clippy::needless_pass_by_value,
    clippy::type_complexity
)]
#[pyfunction]
#[pyo3(signature = (
    reserves, weights, fee_numer, fee_denom, decimals, market_prices, max_input=None
))]
pub fn solve_balancer_weighted_basket(
    reserves: Vec<u128>,
    weights: Vec<u64>,
    fee_numer: u64,
    fee_denom: u64,
    decimals: Vec<u8>,
    market_prices: Vec<f64>,
    max_input: Option<f64>,
) -> PyResult<(Vec<i128>, f64, bool, Vec<i8>, usize)> {
    let pool = degenbot_bot::solvers::balancer_weighted_basket::BalancerMultiTokenState {
        reserves,
        weights,
        fee_numer,
        fee_denom,
        decimals,
    };
    let result = degenbot_bot::solvers::balancer_weighted_basket::solve_balancer_weighted(
        &pool,
        &market_prices,
        max_input,
    );
    Ok((
        result.trades,
        result.profit,
        result.success,
        result.signature,
        result.iterations,
    ))
}

/// Register every Rust-wrapped symbol on the Python module `m`.
///
/// Order and set of registered symbols must stay byte-equivalent to the
/// pre-extraction `#[pymodule]` body (ergo UG6FKN task KFVI5F). The logging
/// bridge init (`pyo3_log::init()`) stays in `lib.rs`'s `#[pymodule]` because it
/// is module-lifecycle setup, not symbol registration.
///
/// # Errors
///
/// Returns a [`PyErr`] if any individual `add_class`/`add_function`/`add` call
/// fails (e.g. a name collision). Errors are propagated unchanged — the
/// `#[pymodule]` caller in `lib.rs` converts them into the module-init failure.
#[allow(clippy::too_many_lines)]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Concentrated-liquidity math (feature = "cl-math") — registered on a real
    // Python submodule `degenbot._ffi.cl_math` (21 fns + 4 tick-boundary
    // constants, un-prefixed). See `crate::cl_math::add_cl_math_module`.
    #[cfg(feature = "cl-math")]
    crate::cl_math::add_cl_math_module(m)?;

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
    }

    // Solady LibZip (FastLZ) compress/decompress — lives in `degenbot-core`
    // (always a dependency), so no feature gate. Registered on a real
    // Python submodule `degenbot._ffi.solady`.
    crate::solady::add_solady_module(m)?;

    // Pathfinding graph + DFS (feature = "pathfinding")
    #[cfg(feature = "pathfinding")]
    m.add_function(wrap_pyfunction!(crate::pathfinding::find_paths_rust, m)?)?;
    // The build_path_graph seam choreographs a degenbot-db read + a
    // degenbot-pathfinding graph build, so it needs BOTH features.
    #[cfg(all(feature = "pathfinding", feature = "db"))]
    m.add_function(wrap_pyfunction!(crate::pathfinding::build_path_graph, m)?)?;
    #[cfg(feature = "pathfinding")]
    m.add_class::<crate::pathfinding::PathIterator>()?;

    // Balancer V2 math library functions (feature = "balancer-math")
    #[cfg(feature = "balancer-math")]
    crate::balancer_math::lib::add_balancer_math_module(m)?;

    // Curve StableSwap math library functions (feature = "curve-math")
    #[cfg(feature = "curve-math")]
    crate::curve_math::lib::add_curve_math_module(m)?;

    // Solidly / Aerodrome / Camelot stable-math library functions
    // (feature = "solidly-math")
    #[cfg(feature = "solidly-math")]
    crate::solidly_math::lib::add_solidly_math_module(m)?;

    // SQLite file operations (feature = "db")
    #[cfg(feature = "db")]
    crate::db::add_db_module(m)?;

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
    m.add_class::<crate::bot::engine::PyArbitrageEngine>()?;
    // Block-stream async iterator (epic 6W35AI) — the authoritative
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
        "PoolAlreadyRegisteredError",
        m.py()
            .get_type::<crate::bot::engine::PoolAlreadyRegisteredError>(),
    )?;
    #[cfg(feature = "bot")]
    m.add(
        "SpecViolationError",
        m.py().get_type::<crate::bot::engine::SpecViolationError>(),
    )?;

    // Bot — Rust-owned state (feature = "bot")
    #[cfg(feature = "bot")]
    m.add_class::<crate::bot::PyBot>()?;
    #[cfg(feature = "bot")]
    m.add_class::<crate::bot::pool::PyLiquidityPool>()?;
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

    // Simulation seam (feature = "simulation") — the PyO3 binding over
    // `degenbot-simulation` (per-block profitability pipeline: simulate_one
    // + dispatch_profitable_results). Skeleton for now; the pyclasses +
    // `dispatch_profitable_py` pyfunction land in A2/A4.
    #[cfg(feature = "simulation")]
    crate::simulation::add_simulation_module(m)?;

    // Pub/sub seam: register a Python callback as a `PoolStateSubscriber`
    // against the Rust `LogDispatcher` fan-out (ZBD4MS) (feature = "bot")
    #[cfg(feature = "bot")]
    crate::bot::subscriber::add_subscriber_module(m)?;

    // `QuantAMM` closed-form N-token Balancer weighted basket solver
    // (feature = "bot") — `solve_balancer_weighted_basket`.
    #[cfg(feature = "bot")]
    m.add_function(wrap_pyfunction!(
        crate::c_api::solve_balancer_weighted_basket,
        m
    )?)?;

    Ok(())
}
