//! `PyO3` wrappers over the `degenbot-concentrated-liquidity-math` pure core.
//! Mirrors `crates/degenbot-concentrated-liquidity-math/`. (ergo UG6FKN task WXHGOH.)

pub mod lib;
pub mod tick_math;

use pyo3::prelude::*;
use pyo3::wrap_pyfunction;

/// Register the full concentrated-liquidity math surface on a real Python
/// submodule `degenbot._ffi.concentrated_liquidity_math`.
///
/// Combines the 19 functions in `lib.rs` (BitMath/FullMath/UnsafeMath/
/// LiquidityMath/SqrtPriceMath/SwapMath/TickMath helpers/LiquidityMapping),
/// the 2 `tick_math.rs` entry points (`get_sqrt_ratio_at_tick` /
/// `get_tick_at_sqrt_ratio`), and the 4 `TickMath` boundary constants
/// (`MIN_TICK`/`MAX_TICK`/`MIN_SQRT_RATIO`/`MAX_SQRT_RATIO`) — previously all
/// flat on the root `_ffi` namespace — onto one real submodule with
/// un-prefixed names (`muldiv`, not `cl_muldiv`).
///
/// The companion `src/degenbot/uniswap/math.py` re-exports these as the stable
/// import path, decoupling Python consumers from `degenbot._ffi`.
///
/// # Errors
///
/// Returns `PyErr` if any function/class/constant fails to register.
pub fn add_concentrated_liquidity_math_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.concentrated_liquidity_math")?;

    // lib.rs — the 19 swap-path math functions.
    lib::add_lib_functions(&submod)?;

    // tick_math.rs — the 2 high-level entry points (previously flat on root
    // via c_api.rs) + the 4 TickMath boundary constants.
    submod.add_function(wrap_pyfunction!(
        tick_math::get_sqrt_ratio_at_tick,
        &submod
    )?)?;
    submod.add_function(wrap_pyfunction!(
        tick_math::get_tick_at_sqrt_ratio,
        &submod
    )?)?;
    register_tick_math_constants(&submod)?;

    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.concentrated_liquidity_math", &submod)?;

    Ok(())
}

/// Register the four `TickMath` boundary constants on the concentrated liquidity math submodule.
///
/// ADR-005 single-source-of-truth: the canonical home is the
/// `degenbot-concentrated-liquidity-math` core; the `PyO3` seam surfaces them so Python
/// companions and a standalone Rust consumer share one source. The
/// `uniswap/v3_libraries/__init__` package re-exports these names; the
/// now-retired pure-Python `tick_math.py` constant definitions are gone
/// (C8 task CM2YQ4).
fn register_tick_math_constants(m: &Bound<'_, PyModule>) -> PyResult<()> {
    use degenbot_math::cl::tick_math::{MAX_SQRT_RATIO, MAX_TICK, MIN_SQRT_RATIO, MIN_TICK};
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
