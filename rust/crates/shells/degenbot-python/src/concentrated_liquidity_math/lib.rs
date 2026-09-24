//! `PyO3` wrappers for the concentrated-liquidity math library.
//!
//! Thin binding layer that extracts Python arguments, calls the pure Rust
//! core, and converts results back to Python types.
//!
//! The GIL is held (not released) during computation because CL math
//! operations take ~20ns — far less than the ~200ns GIL release/reacquire
//! overhead.

use crate::prelude::*;
use alloy::primitives::{aliases::I256, U256};
use pyo3::{exceptions::PyValueError, types::PyAny, PyTypeInfo};

// PyObject alias for pyo3 0.28+
type PyObject = pyo3::Py<pyo3::PyAny>;

use crate::conversion::alloy as alloy_py;
use degenbot_math::cl::bit_math;
use degenbot_math::cl::full_math;
use degenbot_math::cl::liquidity_mapping;
use degenbot_math::cl::swap_math;

/// Convert a Python int/bytes to U256.
fn extract_u256(obj: &Bound<'_, PyAny>) -> PyResult<U256> {
    if let Ok(v) = obj.extract::<u64>() {
        return Ok(U256::from(v));
    }
    if let Ok(v) = obj.extract::<u128>() {
        return Ok(U256::from(v));
    }
    // For arbitrary Python ints, convert via bytes
    let py = obj.py();
    if obj.is_instance(&pyo3::types::PyInt::type_object(py))? {
        let kwargs = pyo3::types::PyDict::new(py);
        kwargs.set_item("signed", false)?;
        let bytes_obj = obj.call_method("to_bytes", (32, "big"), Some(&kwargs))?;
        let bytes: &[u8] = bytes_obj.extract()?;
        Ok(U256::try_from_be_slice(bytes).unwrap_or(U256::ZERO))
    } else {
        Err(PyErr::new::<PyValueError, _>("Expected int"))
    }
}

/// Convert a U256 to a Python int (owned `PyObject`).
fn u256_to_py_obj(py: Python<'_>, v: U256) -> PyResult<PyObject> {
    let bound = alloy_py::u256_to_py(py, &v)?;
    Ok(bound.unbind())
}

/// Convert a Python int to I256 (signed 256-bit).
fn extract_i256(obj: &Bound<'_, PyAny>) -> PyResult<I256> {
    let py = obj.py();
    if obj.is_instance(&pyo3::types::PyInt::type_object(py))? {
        // Get 32-byte signed representation directly
        let kwargs = pyo3::types::PyDict::new(py);
        kwargs.set_item("signed", true)?;
        let bytes_obj = obj.call_method("to_bytes", (32, "big"), Some(&kwargs))?;
        let bytes: &[u8] = bytes_obj.extract()?;
        if bytes.len() != 32 {
            return Err(PyErr::new::<PyValueError, _>(
                "to_bytes returned unexpected length",
            ));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(bytes);
        Ok(I256::from_be_bytes::<32>(arr))
    } else {
        Err(PyErr::new::<PyValueError, _>("Expected int"))
    }
}

// extract_round_up not needed — we use Option<bool> directly in the signature
// ─── BitMath ───────────────────────────────────────────────────────────

/// Find the index of the most significant bit set in `x`.
///
/// # Errors
///
/// Returns `PyValueError` if `x` is zero.
#[pyfunction(signature = (x))]
pub fn most_significant_bit(x: &Bound<'_, PyAny>) -> PyResult<u8> {
    let v = extract_u256(x)?;
    bit_math::most_significant_bit(v).map_err(|e| PyValueError::new_err(e.to_string()))
}

/// Find the index of the least significant bit set in `x`.
///
/// # Errors
///
/// Returns `PyValueError` if `x` is zero.
#[pyfunction(signature = (x))]
pub fn least_significant_bit(x: &Bound<'_, PyAny>) -> PyResult<u8> {
    let v = extract_u256(x)?;
    bit_math::least_significant_bit(v).map_err(|e| PyValueError::new_err(e.to_string()))
}

// ─── FullMath ──────────────────────────────────────────────────────────

/// Compute `floor(a * b / denominator)` with full 512-bit precision.
///
/// # Errors
///
/// Returns `PyValueError` on division by zero or overflow.
#[pyfunction(signature = (a, b, denominator))]
pub fn muldiv(
    a: &Bound<'_, PyAny>,
    b: &Bound<'_, PyAny>,
    denominator: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = a.py();
    let result = full_math::muldiv(
        extract_u256(a)?,
        extract_u256(b)?,
        extract_u256(denominator)?,
    )?;
    u256_to_py_obj(py, result)
}

/// Compute `ceil(a * b / denominator)` with full 512-bit precision.
///
/// # Errors
///
/// Returns `PyValueError` on division by zero or overflow.
#[pyfunction(signature = (a, b, denominator))]
pub fn muldiv_rounding_up(
    a: &Bound<'_, PyAny>,
    b: &Bound<'_, PyAny>,
    denominator: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = a.py();
    let result = full_math::muldiv_rounding_up(
        extract_u256(a)?,
        extract_u256(b)?,
        extract_u256(denominator)?,
    )?;
    u256_to_py_obj(py, result)
}

// ─── SwapMath ──────────────────────────────────────────────────────────

/// Compute a V3-style swap step.
///
/// # Errors
///
/// Returns `PyValueError` on invalid input, overflow, or if liquidity exceeds int128.
#[expect(clippy::similar_names)]
#[pyfunction(signature = (sqrt_price_current, sqrt_price_target, liquidity, amount_remaining, fee_pips))]
pub fn compute_swap_step_v3(
    sqrt_price_current: &Bound<'_, PyAny>,
    sqrt_price_target: &Bound<'_, PyAny>,
    liquidity: &Bound<'_, PyAny>,
    amount_remaining: &Bound<'_, PyAny>,
    fee_pips: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = sqrt_price_current.py();
    let amount_i256 = extract_i256(amount_remaining)?;
    let liquidity_u256 = extract_u256(liquidity)?;
    let liquidity_u128: u128 = match liquidity_u256.try_into() {
        Ok(v) => v,
        Err(_) => return Err(PyValueError::new_err("liquidity exceeds u128")),
    };
    // Validate before cast to avoid using a wrapped value.
    if liquidity_u128 > i128::MAX as u128 {
        return Err(PyValueError::new_err("liquidity exceeds int128 range"));
    }
    // Safe: validated above that liquidity_u128 <= i128::MAX.
    let liquidity_i128 = liquidity_u128.cast_signed();

    let result = swap_math::compute_swap_step_v3(
        extract_u256(sqrt_price_current)?,
        extract_u256(sqrt_price_target)?,
        liquidity_i128,
        amount_i256,
        extract_u256(fee_pips)?,
    )?;

    let tuple = pyo3::types::PyTuple::new(
        py,
        [
            u256_to_py_obj(py, result.sqrt_price_next)?,
            u256_to_py_obj(py, result.amount_in)?,
            u256_to_py_obj(py, result.amount_out)?,
            u256_to_py_obj(py, result.fee_amount)?,
        ],
    )?;
    Ok(tuple.into_any().unbind())
}

/// Compute a V4-style swap step.
///
/// # Errors
///
/// Returns `PyValueError` on invalid input, overflow, or if liquidity exceeds int128.
#[expect(clippy::similar_names)]
#[pyfunction(signature = (sqrt_price_current, sqrt_price_target, liquidity, amount_remaining, fee_pips))]
pub fn compute_swap_step_v4(
    sqrt_price_current: &Bound<'_, PyAny>,
    sqrt_price_target: &Bound<'_, PyAny>,
    liquidity: &Bound<'_, PyAny>,
    amount_remaining: &Bound<'_, PyAny>,
    fee_pips: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = sqrt_price_current.py();
    let amount_i256 = extract_i256(amount_remaining)?;
    let liquidity_u256 = extract_u256(liquidity)?;
    let liquidity_u128: u128 = match liquidity_u256.try_into() {
        Ok(v) => v,
        Err(_) => return Err(PyValueError::new_err("liquidity exceeds u128")),
    };
    // V4's compute_swap_step takes i128 for liquidity, but V4 Solidity uses uint128.
    // Positive i128 values cover the full uint128 range (0..2^127-1). Values >= 2^127
    // are not expected in practice since max uint128 = 2^128-1 cannot fit in i128.
    // Validate before cast to avoid using a wrapped value.
    if liquidity_u128 > i128::MAX as u128 {
        return Err(PyValueError::new_err("liquidity exceeds int128 range"));
    }
    // Safe: validated above that liquidity_u128 <= i128::MAX.
    let liquidity_i128 = liquidity_u128.cast_signed();

    let result = swap_math::compute_swap_step_v4(
        extract_u256(sqrt_price_current)?,
        extract_u256(sqrt_price_target)?,
        liquidity_i128,
        amount_i256,
        extract_u256(fee_pips)?,
    )?;

    let tuple = pyo3::types::PyTuple::new(
        py,
        [
            u256_to_py_obj(py, result.sqrt_price_next)?,
            u256_to_py_obj(py, result.amount_in)?,
            u256_to_py_obj(py, result.amount_out)?,
            u256_to_py_obj(py, result.fee_amount)?,
        ],
    )?;
    Ok(tuple.into_any().unbind())
}

// ─── TickMath (additional helpers) ─────────────────────────────────────

// ─── LiquidityMapping (tick-bitmap + apply_liquidity_mapping_update) ────

/// Compute the tick word and bit position for a compressed tick.
///
/// Returns `(word, bit)` where `word` is the mapping key (`i32`) and `bit`
/// is in `0..=255` (`u8`). Mirrors `degenbot_math::cl::get_tick_word_and_bit_position`.
#[pyfunction(signature = (tick, tick_spacing))]
#[must_use]
pub fn get_tick_word_and_bit_position(tick: i32, tick_spacing: i32) -> (i32, u8) {
    liquidity_mapping::get_tick_word_and_bit_position(tick, tick_spacing)
}

// ─── Register all CL math functions ────────────────────────────────────

/// Register the math functions on the concentrated-liquidity submodule.
///
/// Called by `crate::concentrated_liquidity_math::add_concentrated_liquidity_math_module` (the single entry point that
/// also registers the `tick_math.rs` entry points + boundary constants and
/// wires up `sys.modules`). This helper registers only the `lib.rs` fns
/// (`BitMath` / `FullMath` / `UnsafeMath` / `LiquidityMath` / `SqrtPriceMath` /
/// `SwapMath` / `TickMath` helpers / `LiquidityMapping`), un-prefixed.
///
/// # Errors
///
/// Returns `PyErr` if any function fails to register.
pub fn add_lib_functions(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // BitMath
    m.add_function(wrap_pyfunction!(most_significant_bit, m)?)?;
    m.add_function(wrap_pyfunction!(least_significant_bit, m)?)?;

    // FullMath
    m.add_function(wrap_pyfunction!(muldiv, m)?)?;
    m.add_function(wrap_pyfunction!(muldiv_rounding_up, m)?)?;

    // UnsafeMath

    // LiquidityMath

    // SqrtPriceMath

    // SwapMath
    m.add_function(wrap_pyfunction!(compute_swap_step_v3, m)?)?;
    m.add_function(wrap_pyfunction!(compute_swap_step_v4, m)?)?;

    // TickMath (additional helpers beyond the existing get_sqrt_ratio/tick functions)

    // LiquidityMapping
    m.add_function(wrap_pyfunction!(get_tick_word_and_bit_position, m)?)?;

    Ok(())
}
