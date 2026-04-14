//! `PyO3` wrappers for Uniswap V3 tick math functions.
//!
//! Thin binding layer that extracts Python arguments, releases the GIL,
//! calls the pure Rust core, and converts results back to Python types.

use alloy::primitives::aliases::U160;
use pyo3::{exceptions::PyTypeError, exceptions::PyValueError, prelude::*, types::PyAny};

use crate::alloy_py;
use crate::tick_math::{get_sqrt_ratio_at_tick_internal, get_tick_at_sqrt_ratio_internal};

/// Extract a U160 from a Python object (accepts int or bytes).
#[inline]
fn extract_u160(obj: &Bound<'_, PyAny>) -> PyResult<U160> {
    /// Number of bytes in a 160-bit word (U160).
    const BYTES_PER_WORD: usize = 20;

    if let Ok(bytes) = obj.extract::<&[u8]>() {
        if bytes.len() > BYTES_PER_WORD {
            return Err(PyErr::new::<PyValueError, _>(
                "Sqrt price X96 is too large (exceeds 20 bytes)",
            ));
        }
        return U160::try_from_be_slice(bytes).ok_or_else(|| {
            PyErr::new::<PyValueError, _>("Failed to parse sqrt_price_x96 from bytes")
        });
    }

    // Try to extract as i128 first (common case)
    if let Ok(int_val) = obj.extract::<i128>() {
        if int_val < 0 {
            return Err(PyErr::new::<PyValueError, _>(
                "Sqrt price X96 cannot be negative",
            ));
        }
        return Ok(U160::from(int_val.cast_unsigned()));
    }

    // For larger integers, convert via bytes
    let py = obj.py();
    let int_type = py.import("builtins")?.getattr("int")?;
    if obj.is_instance(&int_type)? {
        let kwargs = pyo3::types::PyDict::new(py);
        kwargs.set_item("signed", false)?;
        let bytes = obj.call_method("to_bytes", (BYTES_PER_WORD, "big"), Some(&kwargs))?;
        let bytes: &[u8] = bytes.extract()?;
        return U160::try_from_be_slice(bytes).ok_or_else(|| {
            PyErr::new::<PyValueError, _>("Failed to parse sqrt_price_x96 from bytes")
        });
    }

    Err(PyErr::new::<PyTypeError, _>(
        "sqrt_price_x96 must be int or bytes",
    ))
}

/// Converts a tick value to its corresponding sqrt price (X96 format).
///
/// This function calculates the sqrt price for a given tick value using the
/// Uniswap V3 tick math formula. The result is returned as a native Python int.
///
/// # Arguments
///
/// * `tick` - The tick value in range [-887272, 887272]
///
/// # Returns
///
/// A Python int representing the sqrt price X96 value
///
/// # Errors
///
/// Returns `PyValueError` if the tick value is invalid
///
/// # Example
///
/// ```python
/// from degenbot_rs import get_sqrt_ratio_at_tick
/// ratio = get_sqrt_ratio_at_tick(0)
/// ```
#[pyfunction(signature = (tick))]
pub fn get_sqrt_ratio_at_tick(py: Python<'_>, tick: i32) -> PyResult<Bound<'_, PyAny>> {
    let result = py.detach(|| get_sqrt_ratio_at_tick_internal(tick))?;
    // U160 is at most 160 bits, so it fits in U256
    let u256 = alloy::primitives::U256::from(result);
    alloy_py::u256_to_py(py, &u256)
}

/// Converts a sqrt price (X96 format) to its corresponding tick value.
///
/// This function calculates the tick for a given sqrt price using the
/// Uniswap V3 tick math formula. The result is returned as a Python `i32`.
///
/// # Arguments
///
/// * `sqrt_price_x96` - The sqrt price X96 value as a Python `int` or `bytes`
///
/// # Returns
///
/// The tick value corresponding to the given sqrt price
///
/// # Errors
///
/// Returns `PyValueError` if:
/// - The input is too large (exceeds 20 bytes)
/// - The sqrt price is outside the valid [`MIN_SQRT_RATIO`, `MAX_SQRT_RATIO`) range
///
/// Returns `PyTypeError` if the input is not an int or bytes
///
/// # Example
///
/// ```python
/// from degenbot_rs import get_tick_at_sqrt_ratio
/// tick = get_tick_at_sqrt_ratio(79228162514264337593543950336)
/// ```
#[pyfunction(signature = (sqrt_price_x96))]
pub fn get_tick_at_sqrt_ratio(py: Python<'_>, sqrt_price_x96: &Bound<'_, PyAny>) -> PyResult<i32> {
    let sqrt_price = extract_u160(sqrt_price_x96)?;
    let tick = py.detach(|| get_tick_at_sqrt_ratio_internal(sqrt_price))?;
    Ok(tick.as_i32())
}
