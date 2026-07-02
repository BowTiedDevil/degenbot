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
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Tick math functions (feature = "cl-math")
    #[cfg(feature = "cl-math")]
    m.add_function(wrap_pyfunction!(
        crate::cl_math::tick_math::get_sqrt_ratio_at_tick,
        m
    )?)?;
    #[cfg(feature = "cl-math")]
    m.add_function(wrap_pyfunction!(
        crate::cl_math::tick_math::get_tick_at_sqrt_ratio,
        m
    )?)?;

    #[cfg(feature = "cl-math")]
    register_tick_math_constants(m)?;

    // Address utilities (feature = "uniswap")
    #[cfg(feature = "uniswap")]
    m.add_function(wrap_pyfunction!(
        crate::uniswap::address::to_checksum_address,
        m
    )?)?;

    // Solady LibZip (FastLZ) compress/decompress — lives in `degenbot-core`
    // (always a dependency), so no feature gate.
    m.add_function(wrap_pyfunction!(crate::solady::libzip::flz_compress, m)?)?;
    m.add_function(wrap_pyfunction!(crate::solady::libzip::flz_decompress, m)?)?;

    // Pathfinding graph + DFS (feature = "pathfinding")
    #[cfg(feature = "pathfinding")]
    m.add_function(wrap_pyfunction!(crate::pathfinding::find_paths_rust, m)?)?;
    #[cfg(feature = "pathfinding")]
    m.add_class::<crate::pathfinding::PathIterator>()?;

    // CL math library submodule (feature = "cl-math")
    #[cfg(feature = "cl-math")]
    crate::cl_math::cl_lib::add_cl_lib_module(m)?;

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

    // ABI decoder/encoder functions (feature = "abi")
    #[cfg(feature = "abi")]
    m.add_function(wrap_pyfunction!(crate::abi::decoder::decode, m)?)?;
    #[cfg(feature = "abi")]
    m.add_function(wrap_pyfunction!(crate::abi::decoder::decode_single, m)?)?;
    #[cfg(feature = "abi")]
    m.add_function(wrap_pyfunction!(crate::abi::encoder::encode, m)?)?;
    #[cfg(feature = "abi")]
    m.add_function(wrap_pyfunction!(crate::abi::encoder::encode_single, m)?)?;

    // Provider + contract + subscription modules (feature = "rpc")
    #[cfg(feature = "rpc")]
    crate::rpc::provider::add_provider_module(m)?;
    #[cfg(feature = "rpc")]
    crate::rpc::contract::add_contract_module(m)?;

    // Uniswap mixed V2/V3/V4 engine (feature = "bot")
    #[cfg(feature = "bot")]
    m.add_class::<crate::bot::engine::PyUniswapArbEngine>()?;
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

    // Typed V4 pool-admission exceptions (Plan 102, slice 2): distinct
    // `ValueError` subclasses so `build_paths` can classify pool rejections
    // by type instead of fragile string matching. (feature = "bot")
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

    // Bot — Rust-owned state (feature = "bot")
    #[cfg(feature = "bot")]
    m.add_class::<crate::bot::PyBot>()?;
    #[cfg(feature = "bot")]
    m.add_class::<crate::bot::pool::PyLiquidityPool>()?;
    #[cfg(feature = "bot")]
    m.add_class::<crate::bot::token::PyErc20Token>()?;
    #[cfg(feature = "bot")]
    m.add_class::<crate::bot::py_bot_io::PyBotIo>()?;

    // DEX identity presets (ADR-005 slice 6) (feature = "bot")
    #[cfg(feature = "bot")]
    crate::bot::dex_identity::add_dex_identity(m)?;

    // Deployment-identity lookup over the embedded deployments.json
    // (Fork A, 7FA5EZ) (feature = "bot")
    #[cfg(feature = "bot")]
    crate::bot::deployments::add_deployments(m)?;

    // Async modules (feature = "async")
    #[cfg(feature = "async")]
    m.add_class::<crate::rpc::async_provider::PyAsyncAlloyProvider>()?;
    #[cfg(feature = "async")]
    m.add_class::<crate::rpc::async_contract::PyAsyncContract>()?;

    // Subscription module (feature = "rpc")
    #[cfg(feature = "rpc")]
    m.add_class::<crate::rpc::subscription::PyAlloySubscription>()?;

    Ok(())
}

/// Register the four `TickMath` boundary constants on the Python module.
///
/// ADR-005 single-source-of-truth: the canonical home is the
/// `degenbot-cl-math` core; the `PyO3` seam surfaces them so Python
/// companions and a standalone Rust consumer share one source. The
/// `v3_libraries/__init__` package re-exports these names; the now-retired
/// pure-Python `tick_math.py` constant definitions are gone (C8 task CM2YQ4).
#[cfg(feature = "cl-math")]
fn register_tick_math_constants(m: &Bound<'_, PyModule>) -> PyResult<()> {
    use crate::cl_lib::tick_math::{MAX_SQRT_RATIO, MAX_TICK, MIN_SQRT_RATIO, MIN_TICK};
    let py = m.py();
    m.add("MIN_TICK", MIN_TICK)?;
    m.add("MAX_TICK", MAX_TICK)?;
    // U160 widens to U256 for the alloy → Python-int conversion helper.
    m.add(
        "MIN_SQRT_RATIO",
        crate::conversion::alloy::u256_to_py(py, &alloy::primitives::U256::from(MIN_SQRT_RATIO))?,
    )?;
    m.add(
        "MAX_SQRT_RATIO",
        crate::conversion::alloy::u256_to_py(py, &alloy::primitives::U256::from(MAX_SQRT_RATIO))?,
    )?;
    Ok(())
}
