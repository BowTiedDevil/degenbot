//! `PyO3` wrappers for the Balancer V2 math library.
//!
//! Thin binding layer that extracts Python arguments, calls the pure Rust
//! core (`degenbot_balancer_math`), and converts results back to Python
//! `int`s. Only the swap-path math surfaces the Balancer companions invoke
//! (`weighted_math`/`stable_math` `calc_out_given_in`/`calc_in_given_out`/
//! `calculate_invariant` + the fee helpers) are wrapped here; the BPT /
//! join / exit math (`stable_math._calc_bpt_*`) is out of scope for the
//! swap seam and lives only in the Python port.
//!
//! The `PowVersion` discriminant (1 = V1 / 2 = V2) is threaded across the
//! seam as a `u8` from the Python-side bytecode detection
//! (`BalancerV2Pool.pow_version`). `round_up` (the stable-invariant V1/V2
//! axis) is threaded as a `bool`.

use crate::prelude::*;
use alloy::primitives::U256;
use degenbot_balancer_math::{BalancerMathError, PowVersion};
use pyo3::{
    exceptions::{PyOverflowError, PyValueError},
    types::{PyList, PyModule},
    wrap_pyfunction, PyTypeInfo,
};

// PyObject alias for pyo3 0.29.
type PyObject = pyo3::Py<pyo3::PyAny>;

/// Extract a Python `int` (or `bytes`) into a `U256`.
///
/// Mirrors `cl_math::cl_lib::extract_u256` — small ints fast-path through
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

/// Extract a Python `list[int]` into `Vec<U256>`.
fn extract_u256_vec(obj: &Bound<'_, PyAny>) -> PyResult<Vec<U256>> {
    let list = obj.cast::<PyList>()?;
    let mut out = Vec::with_capacity(list.len());
    for item in list.iter() {
        out.push(extract_u256(&item)?);
    }
    Ok(out)
}

/// Map the Python `pow_version` discriminant (1/2) to `PowVersion`.
fn pow_version(d: u8) -> PyResult<PowVersion> {
    PowVersion::from_u8(d).ok_or_else(|| {
        PyValueError::new_err(format!(
            "Unknown pow_version discriminant: {d} (expected 1 or 2)"
        ))
    })
}

/// Translate a `BalancerMathError` into the matching Python exception.
///
/// Overflow / out-of-bounds conditions surface as `OverflowError` so probes
/// can distinguish them from the contract-revert-shaped `ValueError`s the
/// deployed contract raises; everything else maps to `ValueError` with the
/// Solidity revert tag (parity with the Python `EVMRevertError`).
fn bal_err(e: BalancerMathError) -> PyErr {
    let tag = match e {
        BalancerMathError::AddOverflow => "ADD_OVERFLOW",
        BalancerMathError::SubOverflow => "SUB_OVERFLOW",
        BalancerMathError::MulOverflow => "MUL_OVERFLOW",
        BalancerMathError::DivInternal => "DIV_INTERNAL",
        BalancerMathError::ZeroDivision => "ZERO_DIVISION",
        BalancerMathError::XOutOfBounds => "X_OUT_OF_BOUNDS",
        BalancerMathError::YOutOfBounds => "Y_OUT_OF_BOUNDS",
        BalancerMathError::ProductOutOfBounds => "PRODUCT_OUT_OF_BOUNDS",
        BalancerMathError::OutOfBounds => "OUT_OF_BOUNDS",
        BalancerMathError::ZeroInvariant => "ZERO_INVARIANT",
        BalancerMathError::MaxInRatio => "MAX_IN_RATIO",
        BalancerMathError::MaxOutRatio => "MAX_OUT_RATIO",
        BalancerMathError::StableInvariantDidntConverge => "STABLE_INVARIANT_DIDNT_CONVERGE",
        BalancerMathError::StableGetBalanceDidntConverge => "STABLE_GET_BALANCE_DIDNT_CONVERGE",
    };
    // Overflow-class errors (would exceed MAX_UINT256) surface as OverflowError;
    // the rest mirror the deployed-contract revert tags the Python oracle raises.
    match e {
        BalancerMathError::AddOverflow
        | BalancerMathError::SubOverflow
        | BalancerMathError::MulOverflow
        | BalancerMathError::DivInternal => PyOverflowError::new_err(tag),
        _ => PyValueError::new_err(tag),
    }
}

// ─── Weighted math ─────────────────────────────────────────────────────

/// Weighted-product invariant (`WeightedMath._calculateInvariant`).
///
/// # Errors
///
/// Returns `OverflowError`/`ValueError` (Solidity revert tag) on overflow or a zero invariant.
#[pyfunction(signature = (normalized_weights, balances, version))]
pub fn weighted_calculate_invariant(
    normalized_weights: &Bound<'_, PyAny>,
    balances: &Bound<'_, PyAny>,
    version: u8,
) -> PyResult<PyObject> {
    let py = normalized_weights.py();
    let weights = extract_u256_vec(normalized_weights)?;
    let balances = extract_u256_vec(balances)?;
    let result = degenbot_balancer_math::weighted_calculate_invariant(
        &weights,
        &balances,
        pow_version(version)?,
    )
    .map_err(bal_err)?;
    u256_to_py_obj(py, result)
}

/// Weighted `calc_out_given_in` (`WeightedMath._calc_out_given_in`).
///
/// # Errors
///
/// Returns `ValueError` (`MAX_IN_RATIO`) if `amount_in` exceeds 30% of `balance_in`, plus overflow reverts.
#[pyfunction(signature = (balance_in, weight_in, balance_out, weight_out, amount_in, version))]
pub fn weighted_calc_out_given_in(
    balance_in: &Bound<'_, PyAny>,
    weight_in: &Bound<'_, PyAny>,
    balance_out: &Bound<'_, PyAny>,
    weight_out: &Bound<'_, PyAny>,
    amount_in: &Bound<'_, PyAny>,
    version: u8,
) -> PyResult<PyObject> {
    let py = balance_in.py();
    let result = degenbot_balancer_math::weighted_calc_out_given_in(
        extract_u256(balance_in)?,
        extract_u256(weight_in)?,
        extract_u256(balance_out)?,
        extract_u256(weight_out)?,
        extract_u256(amount_in)?,
        pow_version(version)?,
    )
    .map_err(bal_err)?;
    u256_to_py_obj(py, result)
}

/// Weighted `calc_in_given_out` (`WeightedMath._calc_in_given_out`).
///
/// # Errors
///
/// Returns `ValueError` (`MAX_OUT_RATIO`) if `amount_out` exceeds 30% of `balance_out`, plus overflow reverts.
#[pyfunction(signature = (balance_in, weight_in, balance_out, weight_out, amount_out, version))]
pub fn weighted_calc_in_given_out(
    balance_in: &Bound<'_, PyAny>,
    weight_in: &Bound<'_, PyAny>,
    balance_out: &Bound<'_, PyAny>,
    weight_out: &Bound<'_, PyAny>,
    amount_out: &Bound<'_, PyAny>,
    version: u8,
) -> PyResult<PyObject> {
    let py = balance_in.py();
    let result = degenbot_balancer_math::weighted_calc_in_given_out(
        extract_u256(balance_in)?,
        extract_u256(weight_in)?,
        extract_u256(balance_out)?,
        extract_u256(weight_out)?,
        extract_u256(amount_out)?,
        pow_version(version)?,
    )
    .map_err(bal_err)?;
    u256_to_py_obj(py, result)
}

/// Subtract the swap fee (`WeightedMath._subtract_swap_fee_amount`): `amount - mul_up(amount, fee)`.
///
/// # Errors
///
/// Returns `OverflowError` (`MUL_OVERFLOW`/`SUB_OVERFLOW`) on overflow.
#[pyfunction(signature = (amount, fee_percentage))]
pub fn weighted_subtract_swap_fee_amount(
    amount: &Bound<'_, PyAny>,
    fee_percentage: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = amount.py();
    let result = degenbot_balancer_math::subtract_swap_fee_amount(
        extract_u256(amount)?,
        extract_u256(fee_percentage)?,
    )
    .map_err(bal_err)?;
    u256_to_py_obj(py, result)
}

/// Add the swap fee (`WeightedMath._add_swap_fee_amount`): `amount + mul_up(amount, fee)`.
///
/// # Errors
///
/// Returns `OverflowError` (`MUL_OVERFLOW`/`ADD_OVERFLOW`) on overflow.
#[pyfunction(signature = (amount, fee_percentage))]
pub fn weighted_add_swap_fee_amount(
    amount: &Bound<'_, PyAny>,
    fee_percentage: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = amount.py();
    let result = degenbot_balancer_math::add_swap_fee_amount(
        extract_u256(amount)?,
        extract_u256(fee_percentage)?,
    )
    .map_err(bal_err)?;
    u256_to_py_obj(py, result)
}

// ─── Fixed-point (shared by weighted + stable math) ─────────────────

/// `FixedPoint._mul_down(a, b) = a * b / ONE`, rounding down.
///
/// Uses `widening_mul` + truncation (mirrors Solidity's `uint256` return for
/// over-large-but-unreachable inputs); the deployed contract reverts on
/// `a * b > MAX_UINT256` — observable only for adversarial inputs the bot
/// never feeds in normal swap math. C8d: routes `scaling_helpers` off the
/// pure-Python `fixed_point.py` port.
///
/// # Errors
///
/// Never reverts (differs from the Python port's `MUL_OVERFLOW` check,
/// which is unreachable for valid scaling factors).
#[pyfunction(signature = (a, b))]
pub fn fixed_point_mul_down(a: &Bound<'_, PyAny>, b: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    let py = a.py();
    let result = degenbot_balancer_math::fixed_point::mul_down(extract_u256(a)?, extract_u256(b)?)
        .map_err(bal_err)?;
    u256_to_py_obj(py, result)
}

/// `FixedPoint._div_down(a, b) = a * ONE / b`, rounding down.
///
/// # Errors
///
/// Returns `ValueError` (`ZERO_DIVISION`) on `b == 0`; `OverflowError`
/// (`DIV_INTERNAL`) when `a * ONE` exceeds `MAX_UINT256`.
#[pyfunction(signature = (a, b))]
pub fn fixed_point_div_down(a: &Bound<'_, PyAny>, b: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    let py = a.py();
    let result = degenbot_balancer_math::fixed_point::div_down(extract_u256(a)?, extract_u256(b)?)
        .map_err(bal_err)?;
    u256_to_py_obj(py, result)
}

/// `FixedPoint._div_up(a, b) = (a * ONE - 1) / b + 1`, rounding up.
///
/// # Errors
///
/// Returns `ValueError` (`ZERO_DIVISION`) on `b == 0`; `OverflowError`
/// (`DIV_INTERNAL`) when `a * ONE` exceeds `MAX_UINT256`.
#[pyfunction(signature = (a, b))]
pub fn fixed_point_div_up(a: &Bound<'_, PyAny>, b: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    let py = a.py();
    let result = degenbot_balancer_math::fixed_point::div_up(extract_u256(a)?, extract_u256(b)?)
        .map_err(bal_err)?;
    u256_to_py_obj(py, result)
}

// ─── Stable math ───────────────────────────────────────────────────────

/// `StableSwap` invariant (`StableMath._calculate_invariant`, V1 always-roundDown `D_P`).
///
/// # Errors
///
/// Returns `ValueError` (`STABLE_INVARIANT_DIDNT_CONVERGE`) if Newton iteration fails to converge.
#[pyfunction(signature = (amp, balances))]
pub fn stable_calculate_invariant(
    amp: &Bound<'_, PyAny>,
    balances: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = amp.py();
    let balances = extract_u256_vec(balances)?;
    let result = degenbot_balancer_math::stable_calculate_invariant(extract_u256(amp)?, &balances)
        .map_err(bal_err)?;
    u256_to_py_obj(py, result)
}

/// `StableSwap` deployed invariant (`StableMath._calculate_invariant_deployed`, V2 round-up-param `P_D`).
///
/// # Errors
///
/// Returns `ValueError` (`STABLE_INVARIANT_DIDNT_CONVERGE`) if Newton iteration fails to converge.
#[pyfunction(signature = (amp, balances, round_up))]
pub fn stable_calculate_invariant_deployed(
    amp: &Bound<'_, PyAny>,
    balances: &Bound<'_, PyAny>,
    round_up: bool,
) -> PyResult<PyObject> {
    let py = amp.py();
    let balances = extract_u256_vec(balances)?;
    let result = degenbot_balancer_math::stable_calculate_invariant_deployed(
        extract_u256(amp)?,
        &balances,
        round_up,
    )
    .map_err(bal_err)?;
    u256_to_py_obj(py, result)
}

/// Stable `calc_out_given_in` (`StableMath._calc_out_given_in`).
///
/// # Errors
///
/// Returns `ValueError` (`STABLE_GET_BALANCE_DIDNT_CONVERGE`/`SUB_OVERFLOW`) on non-convergence or underflow.
#[pyfunction(signature = (amp, balances, token_index_in, token_index_out, token_amount_in, invariant))]
pub fn stable_calc_out_given_in(
    amp: &Bound<'_, PyAny>,
    balances: &Bound<'_, PyAny>,
    token_index_in: usize,
    token_index_out: usize,
    token_amount_in: &Bound<'_, PyAny>,
    invariant: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = amp.py();
    let balances = extract_u256_vec(balances)?;
    let result = degenbot_balancer_math::stable_calc_out_given_in(
        extract_u256(amp)?,
        &balances,
        token_index_in,
        token_index_out,
        extract_u256(token_amount_in)?,
        extract_u256(invariant)?,
    )
    .map_err(bal_err)?;
    u256_to_py_obj(py, result)
}

/// Stable `calc_in_given_out` (`StableMath._calc_in_given_out`).
///
/// # Errors
///
/// Returns `ValueError` (`STABLE_GET_BALANCE_DIDNT_CONVERGE`/`SUB_OVERFLOW`) on non-convergence or underflow.
#[pyfunction(signature = (amp, balances, token_index_in, token_index_out, token_amount_out, invariant))]
pub fn stable_calc_in_given_out(
    amp: &Bound<'_, PyAny>,
    balances: &Bound<'_, PyAny>,
    token_index_in: usize,
    token_index_out: usize,
    token_amount_out: &Bound<'_, PyAny>,
    invariant: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = amp.py();
    let balances = extract_u256_vec(balances)?;
    let result = degenbot_balancer_math::stable_calc_in_given_out(
        extract_u256(amp)?,
        &balances,
        token_index_in,
        token_index_out,
        extract_u256(token_amount_out)?,
        extract_u256(invariant)?,
    )
    .map_err(bal_err)?;
    u256_to_py_obj(py, result)
}

/// Register the Balancer-math functions as a real Python submodule.
///
/// Creates `degenbot._ffi.balancer_math` (a `PyModule`, not a flat
/// root-level prefix) and registers the 12 swap-path math functions on it
/// with un-prefixed names — `fixed_point_mul_down`, not
/// `balancer_fixed_point_mul_down`. The `balancer_` prefix was an artifact
/// of the flat root registration; on the submodule it is redundant.
///
/// The companion `src/degenbot/balancer/math.py` re-exports these as the
/// stable import path (`from degenbot.balancer.math import fixed_point_div_down`),
/// decoupling Python consumers from the `degenbot._ffi` internal module and
/// letting the Rust crate structure show through to Python.
///
/// # Errors
///
/// Returns `PyErr` if any function fails to register.
pub fn add_balancer_math_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.balancer_math")?;

    // Weighted math
    submod.add_function(wrap_pyfunction!(weighted_calculate_invariant, &submod)?)?;
    submod.add_function(wrap_pyfunction!(weighted_calc_out_given_in, &submod)?)?;
    submod.add_function(wrap_pyfunction!(weighted_calc_in_given_out, &submod)?)?;
    submod.add_function(wrap_pyfunction!(
        weighted_subtract_swap_fee_amount,
        &submod
    )?)?;
    submod.add_function(wrap_pyfunction!(weighted_add_swap_fee_amount, &submod)?)?;

    // Fixed-point (shared by weighted + stable)
    submod.add_function(wrap_pyfunction!(fixed_point_mul_down, &submod)?)?;
    submod.add_function(wrap_pyfunction!(fixed_point_div_down, &submod)?)?;
    submod.add_function(wrap_pyfunction!(fixed_point_div_up, &submod)?)?;

    // Stable math
    submod.add_function(wrap_pyfunction!(stable_calculate_invariant, &submod)?)?;
    submod.add_function(wrap_pyfunction!(
        stable_calculate_invariant_deployed,
        &submod
    )?)?;
    submod.add_function(wrap_pyfunction!(stable_calc_out_given_in, &submod)?)?;
    submod.add_function(wrap_pyfunction!(stable_calc_in_given_out, &submod)?)?;

    // Register as an attribute of the parent AND in `sys.modules` so
    // `from degenbot._ffi.balancer_math import X` resolves. `add_submodule`
    // alone only sets the attribute — Python's import system also requires
    // a `sys.modules` entry to traverse the `degenbot._ffi.balancer_math`
    // dotted path (the extension module `_ffi.abi3.so` is a single file,
    // not a package, so without this entry the dotted-path import fails
    // with `ModuleNotFoundError: 'degenbot._ffi' is not a package`).
    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.balancer_math", &submod)?;

    Ok(())
}
