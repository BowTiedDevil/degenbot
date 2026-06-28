//! `PyO3` wrappers for the Solidly / Aerodrome / Camelot stable-pool math.
//!
//! Thin binding layer that extracts Python arguments, calls the pure Rust core
//! (`degenbot_solidly_math`), and converts results back to Python `int`s.
//! Mirrors `balancer_math::lib` and `curve_math::lib` — `extract_u256` /
//! `u256_to_py_obj` round-trip helpers, an `solidly_err` error translator, and
//! one `#[pyfunction]` per wrapped entrypoint.

use crate::prelude::*;
use alloy::primitives::U256;
use degenbot_solidly_math::SolidlyMathError;
use pyo3::{
    exceptions::{PyValueError, PyZeroDivisionError},
    types::PyModule,
    wrap_pyfunction, PyTypeInfo,
};

type PyObject = pyo3::Py<pyo3::PyAny>;

use crate::conversion::alloy as alloy_py;

/// Extract a Python `int` (or `bytes`) into a `U256`.
///
/// Mirrors `balancer_math::lib::extract_u256` /
/// `curve_math::lib::extract_u256` — small ints fast-path through
/// `u64`/`u128`; arbitrary-precision Python ints round-trip via `to_bytes`.
fn extract_u256(obj: &Bound<'_, PyAny>) -> PyResult<U256> {
    if let Ok(v) = obj.extract::<u64>() {
        return Ok(U256::from(v));
    }
    if let Ok(v) = obj.extract::<u128>() {
        return Ok(U256::from(v));
    }
    let py = obj.py();
    if obj.is_instance(&pyo3::types::PyInt::type_object(py))? {
        let kwargs = pyo3::types::PyDict::new(py);
        kwargs.set_item("signed", false)?;
        let bytes_obj = obj.call_method("to_bytes", (32, "big"), Some(&kwargs))?;
        let bytes: &[u8] = bytes_obj.extract()?;
        if bytes.len() != 32 {
            return Err(PyValueError::new_err("to_bytes returned unexpected length"));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(bytes);
        Ok(U256::from_be_bytes(arr))
    } else {
        Err(PyValueError::new_err("Expected int"))
    }
}

/// Convert a `U256` into a Python `int`.
fn u256_to_py_obj(py: Python<'_>, v: U256) -> PyResult<PyObject> {
    Ok(alloy_py::u256_to_py(py, &v)?.unbind())
}

/// Translate a [`SolidlyMathError`] into the matching Python exception.
///
/// Mirrors the Python oracle's exception shapes:
/// - `Overflow` (the Python oracle's `InvalidUint256` / `EVMRevertError("Not a
///   valid uint256")`) surfaces as `ValueError` carrying the Solidity revert
///   string — this matches how the Python oracle raises in
///   `calculations/evm_math.py:raise_if_invalid_uint256` and how the Python
///   `solidly_stable.py:calc_exact_in_stable` re-wraps `ZeroDivisionError`
///   into `EVMRevertError("Division by zero")` (caught by Aerodrome callers
///   and surfaced to user code as `EVMRevertError`).
/// - `InvalidTokenIn` (the oracle's `DegenbotValueError(message=...)`) surfaces
///   as `ValueError` (the `DegenbotValueError` parent class).
/// - `DidNotConverge` (the oracle's `EVMRevertError("Failed to converge on a
///   value for y")`) surfaces as `ValueError` (the binding matches the
///   curve/balancer pattern of converting revert-tagged errors to
///   `ValueError`).
#[allow(clippy::needless_pass_by_value)]
fn solidly_err(e: SolidlyMathError) -> PyErr {
    match e {
        SolidlyMathError::Overflow => {
            PyZeroDivisionError::new_err("EVM Revert: Not a valid uint256")
        }
        SolidlyMathError::InvalidTokenIn => PyValueError::new_err("Invalid token_in identifier"),
        SolidlyMathError::DidNotConverge => {
            PyValueError::new_err("EVM Revert: Failed to converge on a value for y")
        }
    }
}

// ─── Step functions ─────────────────────────────────────────────────────

/// `solidly_calc_d` — the Solidly stable `D = 3*x0*y^2 + x0^3*y` analytic.
///
/// # Errors
///
/// Only surfaces `PyErr` if the result-`int` conversion fails (vanishingly
/// rare — the value is always a valid `uint256`).
#[pyfunction]
pub fn solidly_calc_d(x0: &Bound<'_, PyAny>, y: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    let py = x0.py();
    let result = degenbot_solidly_math::calc_d(extract_u256(x0)?, extract_u256(y)?);
    u256_to_py_obj(py, result)
}

/// `solidly_calc_k` — the Solidly/Aerodrome invariant `k = a*b` over
/// decimal-scaled reserves. Surfaces the deployed-contract's
/// `InvalidUint256` revert as `ZeroDivisionError`-tagged `ValueError`.
///
/// # Errors
///
/// Returns `ValueError` ("EVM Revert: Not a valid uint256") on overflow.
#[pyfunction]
pub fn solidly_calc_k(
    balance_0: &Bound<'_, PyAny>,
    balance_1: &Bound<'_, PyAny>,
    decimals_0: &Bound<'_, PyAny>,
    decimals_1: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = balance_0.py();
    let result = degenbot_solidly_math::calc_k(
        extract_u256(balance_0)?,
        extract_u256(balance_1)?,
        extract_u256(decimals_0)?,
        extract_u256(decimals_1)?,
    )
    .map_err(solidly_err)?;
    u256_to_py_obj(py, result)
}

/// `solidly_calc_f` — the Solidly/Aerodrome invariant `f(x0, y) = x0*y^3 + x0^3*y`.
///
/// # Errors
///
/// Only surfaces `PyErr` if the result-`int` conversion fails.
#[pyfunction]
pub fn solidly_calc_f(x0: &Bound<'_, PyAny>, y: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    let py = x0.py();
    let result = degenbot_solidly_math::calc_f(extract_u256(x0)?, extract_u256(y)?);
    u256_to_py_obj(py, result)
}

/// `camelot_f` — the Camelot-variant invariant `f(x0, y)`.
///
/// # Errors
///
/// Only surfaces `PyErr` if the result-`int` conversion fails.
#[pyfunction]
pub fn camelot_f(x0: &Bound<'_, PyAny>, y: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    let py = x0.py();
    let result = degenbot_solidly_math::f_camelot(extract_u256(x0)?, extract_u256(y)?);
    u256_to_py_obj(py, result)
}

/// `camelot_k` — the Camelot-variant `k = a*b` invariant.
///
/// # Errors
///
/// Returns `ValueError` on overflow.
#[pyfunction]
pub fn camelot_k(
    balance_0: &Bound<'_, PyAny>,
    balance_1: &Bound<'_, PyAny>,
    decimals_0: &Bound<'_, PyAny>,
    decimals_1: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = balance_0.py();
    let result = degenbot_solidly_math::k_camelot(
        extract_u256(balance_0)?,
        extract_u256(balance_1)?,
        extract_u256(decimals_0)?,
        extract_u256(decimals_1)?,
    )
    .map_err(solidly_err)?;
    u256_to_py_obj(py, result)
}

// ─── Iterative solvers ──────────────────────────────────────────────────

/// `solidly_get_y_solidly` — Newton's-method `y` solve for the Solidly/Aerodrome
/// invariant. Direct port of `solidly_stable.get_y_solidly`.
///
/// # Errors
///
/// Returns `ValueError` on non-convergence (after 255 iterations) or overflow.
#[pyfunction(signature = (x0, xy, y, decimals_0, decimals_1))]
#[allow(clippy::too_many_arguments)] // mirrors the deployed-contract 5-arg signature + decimals pair
pub fn solidly_get_y_solidly(
    x0: &Bound<'_, PyAny>,
    xy: &Bound<'_, PyAny>,
    y: &Bound<'_, PyAny>,
    decimals_0: &Bound<'_, PyAny>,
    decimals_1: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = x0.py();
    let result = degenbot_solidly_math::get_y_solidly(
        extract_u256(x0)?,
        extract_u256(xy)?,
        extract_u256(y)?,
        extract_u256(decimals_0)?,
        extract_u256(decimals_1)?,
    )
    .map_err(solidly_err)?;
    u256_to_py_obj(py, result)
}

/// `camelot_get_y_camelot` — Newton's-method `y` solve for the Camelot
/// invariant. Direct port of `camelot.get_y_camelot`. Takes `x_0, xy, y_seed`
/// (3-arg form — the Python oracle's 5-arg shape ignores the decimals pair).
///
/// # Errors
///
/// Never reverts from the loop; surfaces `ValueError` only if the inner
/// `calc_d` would divide by zero (the Python oracle propagates
/// `ZeroDivisionError` from `solidly_calc_d`).
#[pyfunction(signature = (x_0, xy, y))]
pub fn camelot_get_y_camelot(
    x_0: &Bound<'_, PyAny>,
    xy: &Bound<'_, PyAny>,
    y: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = x_0.py();
    let result = degenbot_solidly_math::get_y_camelot(
        extract_u256(x_0)?,
        extract_u256(xy)?,
        extract_u256(y)?,
    );
    u256_to_py_obj(py, result)
}

// ─── Pre-baked orchestration entrypoints ────────────────────────────────

/// `solidly_calc_exact_in_volatile` — Solidly volatile (`x*y >= k`) amount-out.
/// Direct port of `solidly_stable.calc_exact_in_volatile`.
///
/// # Errors
///
/// Returns `ValueError` ("Invalid `token_in` identifier") if `token_in` is not
/// 0 or 1.
#[pyfunction(signature = (amount_in, token_in, reserves_0, reserves_1, fee_numer, fee_denom))]
#[allow(clippy::too_many_arguments)]
pub fn solidly_calc_exact_in_volatile(
    amount_in: &Bound<'_, PyAny>,
    token_in: u8,
    reserves_0: &Bound<'_, PyAny>,
    reserves_1: &Bound<'_, PyAny>,
    fee_numer: &Bound<'_, PyAny>,
    fee_denom: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = amount_in.py();
    let result = degenbot_solidly_math::calc_exact_in_volatile(
        extract_u256(amount_in)?,
        token_in,
        extract_u256(reserves_0)?,
        extract_u256(reserves_1)?,
        extract_u256(fee_numer)?,
        extract_u256(fee_denom)?,
    )
    .map_err(solidly_err)?;
    u256_to_py_obj(py, result)
}

/// `solidly_calc_exact_in_stable_solidly` — Solidly / Aerodrome stable
/// amount-out. Pre-baked variant using `calc_k` + `get_y_solidly`.
///
/// # Errors
///
/// Returns `ValueError` on invalid `token_in`, overflow, or non-convergence.
#[pyfunction(signature = (amount_in, token_in, reserves_0, reserves_1, decimals_0, decimals_1, fee_numer, fee_denom))]
#[allow(clippy::too_many_arguments)]
pub fn solidly_calc_exact_in_stable_solidly(
    amount_in: &Bound<'_, PyAny>,
    token_in: u8,
    reserves_0: &Bound<'_, PyAny>,
    reserves_1: &Bound<'_, PyAny>,
    decimals_0: &Bound<'_, PyAny>,
    decimals_1: &Bound<'_, PyAny>,
    fee_numer: &Bound<'_, PyAny>,
    fee_denom: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = amount_in.py();
    let result = degenbot_solidly_math::calc_exact_in_stable_solidly(
        extract_u256(amount_in)?,
        token_in,
        extract_u256(reserves_0)?,
        extract_u256(reserves_1)?,
        extract_u256(decimals_0)?,
        extract_u256(decimals_1)?,
        extract_u256(fee_numer)?,
        extract_u256(fee_denom)?,
    )
    .map_err(solidly_err)?;
    u256_to_py_obj(py, result)
}

/// `solidly_calc_exact_in_stable_camelot` — Camelot stable amount-out.
/// Pre-baked variant using `k_camelot` + `get_y_camelot`.
///
/// # Errors
///
/// Returns `ValueError` on invalid `token_in`, overflow, or non-convergence.
#[pyfunction(signature = (amount_in, token_in, reserves_0, reserves_1, decimals_0, decimals_1, fee_numer, fee_denom))]
#[allow(clippy::too_many_arguments)]
pub fn solidly_calc_exact_in_stable_camelot(
    amount_in: &Bound<'_, PyAny>,
    token_in: u8,
    reserves_0: &Bound<'_, PyAny>,
    reserves_1: &Bound<'_, PyAny>,
    decimals_0: &Bound<'_, PyAny>,
    decimals_1: &Bound<'_, PyAny>,
    fee_numer: &Bound<'_, PyAny>,
    fee_denom: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = amount_in.py();
    let result = degenbot_solidly_math::calc_exact_in_stable_camelot(
        extract_u256(amount_in)?,
        token_in,
        extract_u256(reserves_0)?,
        extract_u256(reserves_1)?,
        extract_u256(decimals_0)?,
        extract_u256(decimals_1)?,
        extract_u256(fee_numer)?,
        extract_u256(fee_denom)?,
    )
    .map_err(solidly_err)?;
    u256_to_py_obj(py, result)
}

/// Register all Solidly-math functions on the umbrella module.
///
/// # Errors
///
/// Returns `PyErr` if any function fails to register.
pub fn add_solidly_math_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(solidly_calc_d, m)?)?;
    m.add_function(wrap_pyfunction!(solidly_calc_k, m)?)?;
    m.add_function(wrap_pyfunction!(solidly_calc_f, m)?)?;
    m.add_function(wrap_pyfunction!(camelot_f, m)?)?;
    m.add_function(wrap_pyfunction!(camelot_k, m)?)?;
    m.add_function(wrap_pyfunction!(solidly_get_y_solidly, m)?)?;
    m.add_function(wrap_pyfunction!(camelot_get_y_camelot, m)?)?;
    m.add_function(wrap_pyfunction!(solidly_calc_exact_in_volatile, m)?)?;
    m.add_function(wrap_pyfunction!(solidly_calc_exact_in_stable_solidly, m)?)?;
    m.add_function(wrap_pyfunction!(solidly_calc_exact_in_stable_camelot, m)?)?;
    Ok(())
}
