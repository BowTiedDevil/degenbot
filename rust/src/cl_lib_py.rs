//! `PyO3` wrappers for the concentrated-liquidity math library.
//!
//! Thin binding layer that extracts Python arguments, calls the pure Rust
//! core, and converts results back to Python types.
//!
//! The GIL is held (not released) during computation because CL math
//! operations take ~20ns — far less than the ~200ns GIL release/reacquire
//! overhead.

use alloy::primitives::{aliases::I256, U256};
use pyo3::{exceptions::PyValueError, prelude::*, types::PyAny, PyTypeInfo};

// PyObject alias for pyo3 0.28+
type PyObject = pyo3::Py<pyo3::PyAny>;

use crate::alloy_py;
use crate::cl_lib::bit_math;
use crate::cl_lib::full_math;
use crate::cl_lib::liquidity_math;
use crate::cl_lib::sqrt_price_math;
use crate::cl_lib::swap_math;
use crate::cl_lib::tick_math;
use crate::cl_lib::unsafe_math;

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
            return Err(PyErr::new::<PyValueError, _>("to_bytes returned unexpected length"));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(bytes);
        Ok(I256::from_be_bytes::<32>(arr))
    } else {
        Err(PyErr::new::<PyValueError, _>("Expected int"))
    }
}

// extract_round_up not needed — we use Option<bool> directly in the signature


/// Extract a Python int as i128 (for liquidity values).
fn extract_i128(obj: &Bound<'_, PyAny>) -> PyResult<i128> {
    let i256 = extract_i256(obj)?;
    let bytes = i256.to_be_bytes::<32>();
    Ok(i128::from_be_bytes(bytes[16..32].try_into().unwrap_or([0u8; 16])))
}
// ─── BitMath ───────────────────────────────────────────────────────────

#[pyfunction(signature = (x))]
pub fn cl_most_significant_bit(x: &Bound<'_, PyAny>) -> PyResult<u8> {
    let v = extract_u256(x)?;
    bit_math::most_significant_bit(v).map_err(|e| PyValueError::new_err(e.to_string()))
}

#[pyfunction(signature = (x))]
pub fn cl_least_significant_bit(x: &Bound<'_, PyAny>) -> PyResult<u8> {
    let v = extract_u256(x)?;
    bit_math::least_significant_bit(v).map_err(|e| PyValueError::new_err(e.to_string()))
}

// ─── FullMath ──────────────────────────────────────────────────────────

#[pyfunction(signature = (a, b, denominator))]
pub fn cl_muldiv(a: &Bound<'_, PyAny>, b: &Bound<'_, PyAny>, denominator: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    let py = a.py();
    let result = full_math::muldiv(extract_u256(a)?, extract_u256(b)?, extract_u256(denominator)?)?;
    u256_to_py_obj(py, result)
}

#[pyfunction(signature = (a, b, denominator))]
pub fn cl_muldiv_rounding_up(a: &Bound<'_, PyAny>, b: &Bound<'_, PyAny>, denominator: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    let py = a.py();
    let result = full_math::muldiv_rounding_up(extract_u256(a)?, extract_u256(b)?, extract_u256(denominator)?)?;
    u256_to_py_obj(py, result)
}

// ─── UnsafeMath ────────────────────────────────────────────────────────

#[pyfunction(signature = (x, y))]
pub fn cl_div_rounding_up(x: &Bound<'_, PyAny>, y: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    let py = x.py();
    let result = unsafe_math::div_rounding_up(extract_u256(x)?, extract_u256(y)?);
    u256_to_py_obj(py, result)
}

#[pyfunction(signature = (a, b, denominator))]
pub fn cl_simple_mul_div(a: &Bound<'_, PyAny>, b: &Bound<'_, PyAny>, denominator: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    let py = a.py();
    let result = unsafe_math::simple_mul_div(extract_u256(a)?, extract_u256(b)?, extract_u256(denominator)?);
    u256_to_py_obj(py, result)
}

// ─── LiquidityMath ─────────────────────────────────────────────────────

#[pyfunction(signature = (x, y))]
pub fn cl_add_delta(x: &Bound<'_, PyAny>, y: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    let py = x.py();
    // Extract x as u128 via U256 — validate range first
    let x_u256 = extract_u256(x)?;
    let x_u128: u128 = match x_u256.try_into() {
        Ok(v) => v,
        Err(_) => return Err(PyValueError::new_err("x exceeds u128")),
    };
    // Extract y as i128 via I256 — validate range first
    let y_i256 = extract_i256(y)?;
    let y_bytes = y_i256.to_be_bytes::<32>();
    let y_i128 = i128::from_be_bytes(y_bytes[16..32].try_into().unwrap_or([0u8; 16]));
    let result = liquidity_math::add_delta(x_u128, y_i128).map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(pyo3::types::PyInt::new(py, result).into_any().unbind())
}

// ─── SqrtPriceMath ─────────────────────────────────────────────────────

#[pyfunction(signature = (sqrt_price_a, sqrt_price_b, liquidity, round_up=None))]
pub fn cl_get_amount0_delta(
    sqrt_price_a: &Bound<'_, PyAny>,
    sqrt_price_b: &Bound<'_, PyAny>,
    liquidity: &Bound<'_, PyAny>,
    round_up: Option<bool>,
) -> PyResult<PyObject> {
    let py = sqrt_price_a.py();
    let result = sqrt_price_math::get_amount0_delta(
        extract_u256(sqrt_price_a)?,
        extract_u256(sqrt_price_b)?,
        extract_i128(liquidity)?,
        round_up,
    )?;
    u256_to_py_obj(py, result)
}

#[pyfunction(signature = (sqrt_price_a, sqrt_price_b, liquidity, round_up=None))]
pub fn cl_get_amount1_delta(
    sqrt_price_a: &Bound<'_, PyAny>,
    sqrt_price_b: &Bound<'_, PyAny>,
    liquidity: &Bound<'_, PyAny>,
    round_up: Option<bool>,
) -> PyResult<PyObject> {
    let py = sqrt_price_a.py();
    let result = sqrt_price_math::get_amount1_delta(
        extract_u256(sqrt_price_a)?,
        extract_u256(sqrt_price_b)?,
        extract_i128(liquidity)?,
        round_up,
    )?;
    u256_to_py_obj(py, result)
}

#[pyfunction(signature = (sqrt_price_x96, liquidity, amount, add))]
pub fn cl_get_next_sqrt_price_from_amount0_rounding_up(
    sqrt_price_x96: &Bound<'_, PyAny>,
    liquidity: &Bound<'_, PyAny>,
    amount: &Bound<'_, PyAny>,
    add: bool,
) -> PyResult<PyObject> {
    let py = sqrt_price_x96.py();
    let result = sqrt_price_math::get_next_sqrt_price_from_amount0_rounding_up(
        extract_u256(sqrt_price_x96)?,
        extract_i128(liquidity)?,
        extract_u256(amount)?,
        add,
    )?;
    u256_to_py_obj(py, result)
}

#[pyfunction(signature = (sqrt_price_x96, liquidity, amount, add))]
pub fn cl_get_next_sqrt_price_from_amount1_rounding_down(
    sqrt_price_x96: &Bound<'_, PyAny>,
    liquidity: &Bound<'_, PyAny>,
    amount: &Bound<'_, PyAny>,
    add: bool,
) -> PyResult<PyObject> {
    let py = sqrt_price_x96.py();
    let result = sqrt_price_math::get_next_sqrt_price_from_amount1_rounding_down(
        extract_u256(sqrt_price_x96)?,
        extract_i128(liquidity)?,
        extract_u256(amount)?,
        add,
    )?;
    u256_to_py_obj(py, result)
}

#[pyfunction(signature = (sqrt_price_x96, liquidity, amount_in, zero_for_one))]
pub fn cl_get_next_sqrt_price_from_input(
    sqrt_price_x96: &Bound<'_, PyAny>,
    liquidity: &Bound<'_, PyAny>,
    amount_in: &Bound<'_, PyAny>,
    zero_for_one: bool,
) -> PyResult<PyObject> {
    let py = sqrt_price_x96.py();
    let result = sqrt_price_math::get_next_sqrt_price_from_input(
        extract_u256(sqrt_price_x96)?,
        extract_i128(liquidity)?,
        extract_u256(amount_in)?,
        zero_for_one,
    )?;
    u256_to_py_obj(py, result)
}

#[pyfunction(signature = (sqrt_price_x96, liquidity, amount_out, zero_for_one))]
pub fn cl_get_next_sqrt_price_from_output(
    sqrt_price_x96: &Bound<'_, PyAny>,
    liquidity: &Bound<'_, PyAny>,
    amount_out: &Bound<'_, PyAny>,
    zero_for_one: bool,
) -> PyResult<PyObject> {
    let py = sqrt_price_x96.py();
    let result = sqrt_price_math::get_next_sqrt_price_from_output(
        extract_u256(sqrt_price_x96)?,
        extract_i128(liquidity)?,
        extract_u256(amount_out)?,
        zero_for_one,
    )?;
    u256_to_py_obj(py, result)
}

// ─── SwapMath ──────────────────────────────────────────────────────────

#[pyfunction(signature = (sqrt_price_current, sqrt_price_target, liquidity, amount_remaining, fee_pips))]
pub fn cl_compute_swap_step_v3(
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
    let liquidity_i128 = liquidity_u128 as i128; // wraps for values >= 2^127
    if liquidity_u128 > i128::MAX as u128 {
        return Err(PyValueError::new_err("liquidity exceeds int128 range"));
    }

    let result = swap_math::compute_swap_step_v3(
        extract_u256(sqrt_price_current)?,
        extract_u256(sqrt_price_target)?,
        liquidity_i128,
        amount_i256,
        extract_u256(fee_pips)?,
    )?;

    let tuple = pyo3::types::PyTuple::new(py, [
        u256_to_py_obj(py, result.sqrt_price_next)?,
        u256_to_py_obj(py, result.amount_in)?,
        u256_to_py_obj(py, result.amount_out)?,
        u256_to_py_obj(py, result.fee_amount)?,
    ])?;
    Ok(tuple.into_any().unbind())
}

#[pyfunction(signature = (sqrt_price_current, sqrt_price_target, liquidity, amount_remaining, fee_pips))]
pub fn cl_compute_swap_step_v4(
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
    // The Rust core validates that liquidity >= 0.
    let liquidity_i128 = liquidity_u128 as i128; // wraps for values >= 2^127
    if liquidity_u128 > i128::MAX as u128 {
        return Err(PyValueError::new_err("liquidity exceeds int128 range"));
    }

    let result = swap_math::compute_swap_step_v4(
        extract_u256(sqrt_price_current)?,
        extract_u256(sqrt_price_target)?,
        liquidity_i128,
        amount_i256,
        extract_u256(fee_pips)?,
    )?;

    let tuple = pyo3::types::PyTuple::new(py, [
        u256_to_py_obj(py, result.sqrt_price_next)?,
        u256_to_py_obj(py, result.amount_in)?,
        u256_to_py_obj(py, result.amount_out)?,
        u256_to_py_obj(py, result.fee_amount)?,
    ])?;
    Ok(tuple.into_any().unbind())
}

// ─── TickMath (additional helpers) ─────────────────────────────────────

#[pyfunction(signature = (tick_spacing))]
#[must_use]
pub fn cl_max_usable_tick(tick_spacing: i32) -> i32 {
    tick_math::max_usable_tick(tick_spacing)
}

#[pyfunction(signature = (tick_spacing))]
#[must_use]
pub fn cl_min_usable_tick(tick_spacing: i32) -> i32 {
    tick_math::min_usable_tick(tick_spacing)
}

// ─── Register all CL math functions ────────────────────────────────────

/// Add all concentrated-liquidity math functions to the Python module.
pub fn add_cl_lib_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // BitMath
    m.add_function(wrap_pyfunction!(cl_most_significant_bit, m)?)?;
    m.add_function(wrap_pyfunction!(cl_least_significant_bit, m)?)?;

    // FullMath
    m.add_function(wrap_pyfunction!(cl_muldiv, m)?)?;
    m.add_function(wrap_pyfunction!(cl_muldiv_rounding_up, m)?)?;

    // UnsafeMath
    m.add_function(wrap_pyfunction!(cl_div_rounding_up, m)?)?;
    m.add_function(wrap_pyfunction!(cl_simple_mul_div, m)?)?;

    // LiquidityMath
    m.add_function(wrap_pyfunction!(cl_add_delta, m)?)?;

    // SqrtPriceMath
    m.add_function(wrap_pyfunction!(cl_get_amount0_delta, m)?)?;
    m.add_function(wrap_pyfunction!(cl_get_amount1_delta, m)?)?;
    m.add_function(wrap_pyfunction!(cl_get_next_sqrt_price_from_amount0_rounding_up, m)?)?;
    m.add_function(wrap_pyfunction!(cl_get_next_sqrt_price_from_amount1_rounding_down, m)?)?;
    m.add_function(wrap_pyfunction!(cl_get_next_sqrt_price_from_input, m)?)?;
    m.add_function(wrap_pyfunction!(cl_get_next_sqrt_price_from_output, m)?)?;

    // SwapMath
    m.add_function(wrap_pyfunction!(cl_compute_swap_step_v3, m)?)?;
    m.add_function(wrap_pyfunction!(cl_compute_swap_step_v4, m)?)?;

    // TickMath (additional helpers beyond the existing get_sqrt_ratio/tick functions)
    m.add_function(wrap_pyfunction!(cl_max_usable_tick, m)?)?;
    m.add_function(wrap_pyfunction!(cl_min_usable_tick, m)?)?;

    Ok(())
}
