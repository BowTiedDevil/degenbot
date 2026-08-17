//! `PyO3` wrappers for the V2 constant-product (x*y=k) swap math.
//!
//! Thin binding layer over `degenbot_v2_math` (`v2_swap_exact_in` /
//! `v2_swap_exact_out`): `extract_u256` argument extraction, an error
//! translator, and one `#[pyfunction]` per wrapped entrypoint (RH3L24).

use crate::prelude::*;
use alloy::primitives::U256;
use pyo3::{exceptions::PyValueError, types::PyModule, wrap_pyfunction, PyTypeInfo};

type PyObject = pyo3::Py<pyo3::PyAny>;

use crate::conversion::alloy as alloy_py;

/// Extract a Python `int` into a `U256` (local copy of the math-binding
/// helper pattern, mirroring `solidly_math::extract_u256`).
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

fn v2_math_err(e: degenbot_v2_math::HopSwapError) -> PyErr {
    let msg = match e {
        degenbot_v2_math::HopSwapError::Overflow => {
            "uint256 overflow in V2 swap math (on-chain would revert)"
        }
        degenbot_v2_math::HopSwapError::InsufficientReserves => {
            "requested output >= pool reserves (or degenerate fee parameters)"
        }
        degenbot_v2_math::HopSwapError::InvalidFee => "fee numerator must be < fee denominator",
    };
    PyValueError::new_err(msg)
}

/// `calc_exact_in_v2` — V2 constant-product amount-OUT for an exact input
/// (the on-chain getAmountOut):
/// `y = (fee_denom - fee_numer) * r_out * amount_in / (fee_denom * r_in + (fee_denom - fee_numer) * amount_in)` (floor).
///
/// # Errors
///
/// Returns `ValueError` on invalid fee parameters or uint256 overflow.
#[pyfunction(signature = (reserves_in, reserves_out, amount_in, fee_numer, fee_denom))]
pub fn calc_exact_in_v2(
    reserves_in: &Bound<'_, PyAny>,
    reserves_out: &Bound<'_, PyAny>,
    amount_in: &Bound<'_, PyAny>,
    fee_numer: &Bound<'_, PyAny>,
    fee_denom: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = reserves_in.py();
    let result = degenbot_v2_math::v2_swap_exact_in(
        extract_u256(reserves_in)?,
        extract_u256(reserves_out)?,
        extract_u256(amount_in)?,
        u64::try_from(extract_u256(fee_numer)?)
            .map_err(|_| PyValueError::new_err("fee_numer too large"))?,
        u64::try_from(extract_u256(fee_denom)?)
            .map_err(|_| PyValueError::new_err("fee_denom too large"))?,
    )
    .map_err(v2_math_err)?;
    u256_to_py_obj(py, result)
}

/// `calc_exact_out_v2` — V2 constant-product amount-IN for an exact output
/// (getAmountOut inverse): `x = 1 + r_in * amount_out * fee_denom /
/// ((r_out - amount_out) * (fee_denom - fee_numer))` (floor).
///
/// # Errors
///
/// Returns `ValueError` on overdraw (amount_out >= reserves_out), invalid
/// fee parameters, or uint256 overflow.
#[pyfunction(signature = (reserves_in, reserves_out, amount_out, fee_numer, fee_denom))]
pub fn calc_exact_out_v2(
    reserves_in: &Bound<'_, PyAny>,
    reserves_out: &Bound<'_, PyAny>,
    amount_out: &Bound<'_, PyAny>,
    fee_numer: &Bound<'_, PyAny>,
    fee_denom: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = reserves_in.py();
    let result = degenbot_v2_math::v2_swap_exact_out(
        extract_u256(reserves_in)?,
        extract_u256(reserves_out)?,
        extract_u256(amount_out)?,
        u64::try_from(extract_u256(fee_numer)?)
            .map_err(|_| PyValueError::new_err("fee_numer too large"))?,
        u64::try_from(extract_u256(fee_denom)?)
            .map_err(|_| PyValueError::new_err("fee_denom too large"))?,
    )
    .map_err(v2_math_err)?;
    u256_to_py_obj(py, result)
}

pub fn add_v2_math_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.v2_math")?;

    submod.add_function(wrap_pyfunction!(calc_exact_in_v2, &submod)?)?;
    submod.add_function(wrap_pyfunction!(calc_exact_out_v2, &submod)?)?;

    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.v2_math", &submod)?;

    Ok(())
}
