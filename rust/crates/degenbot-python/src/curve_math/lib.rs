//! `PyO3` wrappers for the Curve `StableSwap` math library.
//!
//! Thin binding layer that extracts Python arguments, calls the pure Rust core
//! (`degenbot_curve_math`), and converts results back to Python `int`s. Wraps
//! the five iterative solvers the `DyCalculator` strategy seam invokes; the
//! step functions (`calc_d`/`calc_dp*`) are internal to the solvers and not
//! re-exported. The variant enums (`DVariant`/`YVariant`/`YDVariant`) cross
//! the seam as `u8` (1-based, matching the Python `auto()` enum `.value` +
//! the Rust `try_from_u8`).

use crate::prelude::*;
use alloy::primitives::U256;
use degenbot_curve_math::{CurveMathError, CurveMathError as CErr, DVariant, YDVariant, YVariant};
use pyo3::{
    exceptions::PyValueError,
    types::{PyList, PyModule},
    wrap_pyfunction, PyTypeInfo,
};

type PyObject = pyo3::Py<pyo3::PyAny>;

use crate::conversion::alloy as alloy_py;

/// Extract a Python `int` (or `bytes`) into a `U256`.
///
/// Mirrors `balancer_math::lib::extract_u256` / `cl_math::cl_lib::extract_u256`
/// — small ints fast-path through `u64`/`u128`; arbitrary-precision Python ints
/// round-trip via `to_bytes`.
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

/// Extract a Python `list[int]` into `Vec<U256>`.
fn extract_u256_vec(obj: &Bound<'_, PyAny>) -> PyResult<Vec<U256>> {
    let list = obj.cast::<PyList>()?;
    let mut out = Vec::with_capacity(list.len());
    for item in list.iter() {
        out.push(extract_u256(&item)?);
    }
    Ok(out)
}

/// Map the Python `DVariant.value` (1-based `auto()`) → `DVariant`.
fn d_variant_from_u8(d: u8) -> PyResult<DVariant> {
    DVariant::try_from_u8(d)
        .ok_or_else(|| PyValueError::new_err(format!("Unknown d_variant discriminant: {d}")))
}

/// Map the Python `YVariant.value` → `YVariant`.
fn y_variant_from_u8(d: u8) -> PyResult<YVariant> {
    YVariant::try_from_u8(d)
        .ok_or_else(|| PyValueError::new_err(format!("Unknown y_variant discriminant: {d}")))
}

/// Map the Python `YDVariant.value` → `YDVariant`.
fn yd_variant_from_u8(d: u8) -> PyResult<YDVariant> {
    YDVariant::try_from_u8(d)
        .ok_or_else(|| PyValueError::new_err(format!("Unknown yd_variant discriminant: {d}")))
}

/// Translate a `CurveMathError` into the matching Python exception.
///
/// All variants surface as `ValueError` carrying the Vyper revert shape the
/// Python `DyCalculator` catches and re-wraps as `EVMRevertError(error=str(e))`
/// — including overflow (Vyper uses checked uint256 arithmetic, so overflow is
/// a revert, not a Python `OverflowError`). The Python oracle
/// (`calculations/stableswap.py`) raises `ValueError` for the same conditions
/// (Python big-ints can't overflow, so the only oracle raises are
/// non-convergence / index / unsafe-value).
fn curve_err(e: CurveMathError) -> PyErr {
    match e {
        CErr::Overflow => PyValueError::new_err("OVERFLOW"),
        CErr::NotConverged => PyValueError::new_err("Not converged"),
        CErr::IndexOutOfBounds => PyValueError::new_err("Index out of bounds"),
        CErr::UnsafeValue => PyValueError::new_err("Unsafe value"),
    }
}

// ─── Iterative solvers ─────────────────────────────────────────────────

/// `stableswap_get_d` — compute the `StableSwap` invariant D from XP + amp.
///
/// # Errors
///
/// Returns `OverflowError` on uint256 overflow; `ValueError` ("Not converged") after 255 iterations.
#[pyfunction(signature = (xp, amp, n_coins, a_precision, d_variant))]
pub fn stableswap_get_d(
    xp: &Bound<'_, PyAny>,
    amp: &Bound<'_, PyAny>,
    n_coins: &Bound<'_, PyAny>,
    a_precision: &Bound<'_, PyAny>,
    d_variant: u8,
) -> PyResult<PyObject> {
    let py = xp.py();
    let result = degenbot_curve_math::stableswap_get_d(
        &extract_u256_vec(xp)?,
        extract_u256(amp)?,
        extract_u256(n_coins)?,
        extract_u256(a_precision)?,
        d_variant_from_u8(d_variant)?,
    )
    .map_err(curve_err)?;
    u256_to_py_obj(py, result)
}

/// `stableswap_get_y` — compute x[j] given x[i] = x (the swap-solve entry).
///
/// # Errors
///
/// Returns `ValueError` ("Index out of bounds") if `i == j` or out of range; "Not converged" after 255 iterations; `OverflowError` on overflow.
#[pyfunction(signature = (i, j, x, xp, amp, n_coins, a_precision, y_variant, d_variant))]
#[expect(clippy::too_many_arguments)] // mirrors the Vyper contract's 9-arg signature
pub fn stableswap_get_y(
    i: usize,
    j: usize,
    x: &Bound<'_, PyAny>,
    xp: &Bound<'_, PyAny>,
    amp: &Bound<'_, PyAny>,
    n_coins: &Bound<'_, PyAny>,
    a_precision: &Bound<'_, PyAny>,
    y_variant: u8,
    d_variant: u8,
) -> PyResult<PyObject> {
    let py = x.py();
    let result = degenbot_curve_math::stableswap_get_y(
        i,
        j,
        extract_u256(x)?,
        &extract_u256_vec(xp)?,
        extract_u256(amp)?,
        extract_u256(n_coins)?,
        extract_u256(a_precision)?,
        y_variant_from_u8(y_variant)?,
        d_variant_from_u8(d_variant)?,
    )
    .map_err(curve_err)?;
    u256_to_py_obj(py, result)
}

/// `stableswap_get_y_d` — compute x[i] given the invariant D.
///
/// # Errors
///
/// Returns `ValueError` ("Index out of bounds") if `i >= n_coins`; "Not converged" after 255 iterations; `OverflowError` on overflow.
#[pyfunction(signature = (amp, i, xp, d, n_coins, a_precision, yd_variant))]
pub fn stableswap_get_y_d(
    amp: &Bound<'_, PyAny>,
    i: usize,
    xp: &Bound<'_, PyAny>,
    d: &Bound<'_, PyAny>,
    n_coins: &Bound<'_, PyAny>,
    a_precision: &Bound<'_, PyAny>,
    yd_variant: u8,
) -> PyResult<PyObject> {
    let py = amp.py();
    let result = degenbot_curve_math::stableswap_get_y_d(
        extract_u256(amp)?,
        i,
        &extract_u256_vec(xp)?,
        extract_u256(d)?,
        extract_u256(n_coins)?,
        extract_u256(a_precision)?,
        yd_variant_from_u8(yd_variant)?,
    )
    .map_err(curve_err)?;
    u256_to_py_obj(py, result)
}

/// `stableswap_newton_y` — the crypto-variant Newton solve (A/gamma/D/frac bounds).
///
/// # Errors
///
/// Returns `ValueError` ("Unsafe value") on an A/gamma/D/frac safety-assertion failure; "Not converged" after 255 iterations; `OverflowError` on overflow.
#[pyfunction(signature = (ann, gamma, xp, d, token_index, n_coins, a_multiplier))]
pub fn stableswap_newton_y(
    ann: &Bound<'_, PyAny>,
    gamma: &Bound<'_, PyAny>,
    xp: &Bound<'_, PyAny>,
    d: &Bound<'_, PyAny>,
    token_index: usize,
    n_coins: &Bound<'_, PyAny>,
    a_multiplier: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = ann.py();
    let result = degenbot_curve_math::stableswap_newton_y(
        extract_u256(ann)?,
        extract_u256(gamma)?,
        &extract_u256_vec(xp)?,
        extract_u256(d)?,
        token_index,
        extract_u256(n_coins)?,
        extract_u256(a_multiplier)?,
    )
    .map_err(curve_err)?;
    u256_to_py_obj(py, result)
}

/// `stableswap_reduction_coefficient` — the dynamic-fee reduction multiplier.
///
/// # Errors
///
/// Returns `OverflowError` on uint256 overflow.
#[pyfunction(signature = (x, fee_gamma, n_coins))]
pub fn stableswap_reduction_coefficient(
    x: &Bound<'_, PyAny>,
    fee_gamma: &Bound<'_, PyAny>,
    n_coins: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = x.py();
    let result = degenbot_curve_math::stableswap_reduction_coefficient(
        &extract_u256_vec(x)?,
        extract_u256(fee_gamma)?,
        extract_u256(n_coins)?,
    )
    .map_err(curve_err)?;
    u256_to_py_obj(py, result)
}

/// Register all Curve-math functions on a real Python submodule.
///
/// Creates `degenbot._ffi.curve_math` (a `PyModule`, not a flat
/// root-level prefix) and registers the 5 `StableSwap` solver functions on it
/// with un-prefixed names — `stableswap_get_d`, not `curve_stableswap_get_d`.
/// The `curve_` prefix was an artifact of the flat root registration; on the
/// submodule it is redundant.
///
/// The companion `src/degenbot/curve/math.py` re-exports these as the stable
/// import path (`from degenbot.curve.math import stableswap_get_y`),
/// decoupling Python consumers from the `degenbot._ffi` internal module.
///
/// # Errors
///
/// Returns `PyErr` if any function fails to register.
pub fn add_curve_math_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.curve_math")?;

    submod.add_function(wrap_pyfunction!(stableswap_get_d, &submod)?)?;
    submod.add_function(wrap_pyfunction!(stableswap_get_y, &submod)?)?;
    submod.add_function(wrap_pyfunction!(stableswap_get_y_d, &submod)?)?;
    submod.add_function(wrap_pyfunction!(stableswap_newton_y, &submod)?)?;
    submod.add_function(wrap_pyfunction!(stableswap_reduction_coefficient, &submod)?)?;

    // Register as parent attribute AND in `sys.modules` so
    // `from degenbot._ffi.curve_math import X` resolves (see
    // `add_balancer_math_module` for the `sys.modules` rationale —
    // the extension module is a single file, not a package).
    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.curve_math", &submod)?;

    Ok(())
}
