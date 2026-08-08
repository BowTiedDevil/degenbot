//! `PyO3` binding for the Curve `get_dy` calculator layer (task `CNEP47`,
//! epic `TV72EG`).
//!
//! The Python companion (`CurveStableswapPool`) keeps its I/O orchestration
//! (`_resolve_calculation_inputs_via_io`, amp/rates/xp, provider fetches) but
//! delegates the **pure swap calculation** here. `DyCalculationInputs` is a
//! mutable builder python-class the companion fills; `calculate_dy` /
//! `calculate_dy_underlying` then call the `degenbot-curve-math` core.

use crate::prelude::*;
use alloy::primitives::{Address, U256};
use degenbot_curve_math::curve_dy_calculator::{
    calculate_dy as core_calculate_dy, calculate_dy_underlying as core_calculate_dy_underlying,
    CurveBasePoolPort as CoreBasePort, CurveSwapError, DyCalculationInputs as CoreInputs,
};
use pyo3::{
    exceptions::PyValueError,
    types::{PyDict, PyList, PyModule},
    wrap_pyfunction, PyTypeInfo,
};

type PyObject = pyo3::Py<pyo3::PyAny>;

use crate::conversion::alloy as alloy_py;

/// Extract a Python `int` (or `bytes`) into a `U256`.
fn extract_u256(obj: &Bound<'_, PyAny>) -> PyResult<U256> {
    if let Ok(v) = obj.extract::<u64>() {
        return Ok(U256::from(v));
    }
    if let Ok(v) = obj.extract::<u128>() {
        return Ok(U256::from(v));
    }
    let py = obj.py();
    if obj.is_instance(&pyo3::types::PyInt::type_object(py))? {
        let kwargs = PyDict::new(py);
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

/// Extract a Python `list[int]` into `Vec<U256>`.
fn extract_u256_vec(obj: &Bound<'_, PyAny>) -> PyResult<Vec<U256>> {
    let list = obj.cast::<PyList>()?;
    let mut out = Vec::with_capacity(list.len());
    for item in list.iter() {
        out.push(extract_u256(&item)?);
    }
    Ok(out)
}

/// Convert a `U256` into a Python `int`.
fn u256_to_py_obj(py: Python<'_>, v: U256) -> PyResult<PyObject> {
    Ok(alloy_py::u256_to_py(py, &v)?.unbind())
}

/// Translate a `CurveSwapError` into a Python exception. Invariant failures
/// surface as `ValueError` (the Python `DyCalculator` re-wraps those as
/// `EVMRevertError`); wiring failures surface as `TypeError`/`ValueError`.
fn curve_swap_err(e: CurveSwapError) -> PyErr {
    match e {
        CurveSwapError::Invariant(inner) => PyValueError::new_err(format!("{inner:?}")),
        CurveSwapError::NotMetapool => {
            PyValueError::new_err("underlying calc requires a base pool")
        }
        other => PyValueError::new_err(other.to_string()),
    }
}

/// Extension helper: default (empty) core inputs the builder fills in.
fn empty_inputs() -> CoreInputs {
    CoreInputs {
        precision: U256::ZERO,
        fee_denominator: U256::ZERO,
        fee: U256::ZERO,
        n_coins: 0,
        balances: Vec::new(),
        rate_multipliers: Vec::new(),
        precision_multipliers: Vec::new(),
        offpeg_fee_multiplier: U256::ZERO,
        fee_gamma: U256::ZERO,
        mid_fee: U256::ZERO,
        out_fee: U256::ZERO,
        address: Address::ZERO,
        resolved_rates: Vec::new(),
        xp: Vec::new(),
        block_number: 0,
        block_timestamp: 0,
        amp: U256::ZERO,
        d_variant: degenbot_curve_math::DVariant::Standard,
        y_variant: degenbot_curve_math::YVariant::Standard,
        a_precision: U256::ZERO,
        swap_style: 1,
        metapool: false,
        metapool_rate_style: 1,
        metapool_underlying_style: 1,
        d: None,
        gamma: None,
        price_scale: None,
        live_balances: None,
        admin_balances: None,
        effective_balances: None,
        virtual_price: None,
        scaled_redemption_price: None,
    }
}

/// A mutable builder for the pure [`CoreInputs`] snapshot the calculator reads.
///
/// The Python companion fills each field via setters (named after the
/// `DyCalculationInputs` dataclass), then passes it to `calculate_dy` /
/// `calculate_dy_underlying`.
#[pyclass(module = "degenbot._ffi.curve_dy")]
pub struct DyCalculationInputs {
    inner: CoreInputs,
}

#[pymethods]
impl DyCalculationInputs {
    /// Create an empty snapshot; the companion sets every field it needs.
    #[new]
    fn py_new() -> Self {
        Self {
            inner: empty_inputs(),
        }
    }

    #[setter]
    fn set_precision(&mut self, v: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner.precision = extract_u256(v)?;
        Ok(())
    }
    #[setter]
    fn set_fee_denominator(&mut self, v: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner.fee_denominator = extract_u256(v)?;
        Ok(())
    }
    #[setter]
    fn set_fee(&mut self, v: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner.fee = extract_u256(v)?;
        Ok(())
    }
    #[setter]
    fn set_n_coins(&mut self, v: usize) {
        self.inner.n_coins = v;
    }
    #[setter]
    fn set_balances(&mut self, v: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner.balances = extract_u256_vec(v)?;
        Ok(())
    }
    #[setter]
    fn set_rate_multipliers(&mut self, v: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner.rate_multipliers = extract_u256_vec(v)?;
        Ok(())
    }
    #[setter]
    fn set_precision_multipliers(&mut self, v: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner.precision_multipliers = extract_u256_vec(v)?;
        Ok(())
    }
    #[setter]
    fn set_offpeg_fee_multiplier(&mut self, v: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner.offpeg_fee_multiplier = extract_u256(v)?;
        Ok(())
    }
    #[setter]
    fn set_fee_gamma(&mut self, v: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner.fee_gamma = extract_u256(v)?;
        Ok(())
    }
    #[setter]
    fn set_mid_fee(&mut self, v: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner.mid_fee = extract_u256(v)?;
        Ok(())
    }
    #[setter]
    fn set_out_fee(&mut self, v: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner.out_fee = extract_u256(v)?;
        Ok(())
    }
    #[setter]
    fn set_address(&mut self, v: &str) -> PyResult<()> {
        self.inner.address = v
            .parse()
            .map_err(|_| PyValueError::new_err("invalid address"))?;
        Ok(())
    }
    #[setter]
    fn set_resolved_rates(&mut self, v: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner.resolved_rates = extract_u256_vec(v)?;
        Ok(())
    }
    #[setter]
    fn set_xp(&mut self, v: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner.xp = extract_u256_vec(v)?;
        Ok(())
    }
    #[setter]
    fn set_block_number(&mut self, v: u64) {
        self.inner.block_number = v;
    }
    #[setter]
    fn set_block_timestamp(&mut self, v: u64) {
        self.inner.block_timestamp = v;
    }
    #[setter]
    fn set_amp(&mut self, v: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner.amp = extract_u256(v)?;
        Ok(())
    }
    #[setter]
    fn set_d_variant(&mut self, v: u8) -> PyResult<()> {
        self.inner.d_variant = degenbot_curve_math::DVariant::try_from_u8(v)
            .ok_or_else(|| PyValueError::new_err(format!("Unknown d_variant: {v}")))?;
        Ok(())
    }
    #[setter]
    fn set_y_variant(&mut self, v: u8) -> PyResult<()> {
        self.inner.y_variant = degenbot_curve_math::YVariant::try_from_u8(v)
            .ok_or_else(|| PyValueError::new_err(format!("Unknown y_variant: {v}")))?;
        Ok(())
    }
    #[setter]
    fn set_a_precision(&mut self, v: &Bound<'_, PyAny>) -> PyResult<()> {
        self.inner.a_precision = extract_u256(v)?;
        Ok(())
    }
    #[setter]
    fn set_swap_style(&mut self, v: u8) {
        self.inner.swap_style = v;
    }
    #[setter]
    fn set_metapool(&mut self, v: bool) {
        self.inner.metapool = v;
    }
    #[setter]
    fn set_metapool_rate_style(&mut self, v: u8) {
        self.inner.metapool_rate_style = v;
    }
    #[setter]
    fn set_metapool_underlying_style(&mut self, v: u8) {
        self.inner.metapool_underlying_style = v;
    }
    #[setter]
    fn set_d(&mut self, v: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        self.inner.d = v.map(extract_u256).transpose()?;
        Ok(())
    }
    #[setter]
    fn set_gamma(&mut self, v: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        self.inner.gamma = v.map(extract_u256).transpose()?;
        Ok(())
    }
    #[setter]
    fn set_price_scale(&mut self, v: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        self.inner.price_scale = v.map(extract_u256_vec).transpose()?;
        Ok(())
    }
    #[setter]
    fn set_effective_balances(&mut self, v: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        self.inner.effective_balances = v.map(extract_u256_vec).transpose()?;
        Ok(())
    }
    #[setter]
    fn set_virtual_price(&mut self, v: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        self.inner.virtual_price = v.map(extract_u256).transpose()?;
        Ok(())
    }
    #[setter]
    fn set_scaled_redemption_price(&mut self, v: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        self.inner.scaled_redemption_price = v.map(extract_u256).transpose()?;
        Ok(())
    }
}

/// A Rust [`CoreBasePort`] adapter that delegates to a Python base-pool object.
///
/// The Python `_LazyBasePool` / `CurveStableswapPool` base pool satisfies the
/// Python `BasePoolPort` Protocol surface; this adapter calls those same
/// methods (via `Python::attach`) so `calculate_dy_underlying` can recurse
/// through it.
struct PyBasePoolPort {
    obj: Py<PyAny>,
}

/// Map any Python-side failure into a recoverable base-pool error.
fn base_err() -> CurveSwapError {
    CurveSwapError::BasePool(Box::new(CurveSwapError::NotMetapool))
}

impl CoreBasePort for PyBasePoolPort {
    fn token_count(&self) -> usize {
        Python::attach(|py| {
            self.obj.getattr(py, "tokens").ok().map_or(0, |t| {
                if let Ok(l) = t.extract::<Bound<'_, PyList>>(py) {
                    l.len()
                } else {
                    t.extract::<Bound<'_, pyo3::types::PyTuple>>(py)
                        .map_or(0, |tp| tp.len())
                }
            })
        })
    }

    fn fee(&self) -> U256 {
        Python::attach(|py| {
            self.obj
                .getattr(py, "fee")
                .ok()
                .and_then(|v| v.extract::<u128>(py).ok())
                .map_or(U256::ZERO, U256::from)
        })
    }

    fn calc_token_amount(&self, amounts: &[U256], block: u64) -> Result<U256, CurveSwapError> {
        Python::attach(|py| {
            let amount_pys: PyResult<Vec<PyObject>> =
                amounts.iter().map(|v| u256_to_py_obj(py, *v)).collect();
            let amount_pys = amount_pys.map_err(|_| base_err())?;
            let amounts_list = PyList::new(py, amount_pys).map_err(|_| base_err())?;
            let kwargs = PyDict::new(py);
            kwargs
                .set_item("amounts", &amounts_list)
                .map_err(|_| base_err())?;
            kwargs.set_item("deposit", true).map_err(|_| base_err())?;
            kwargs
                .set_item("block_identifier", block)
                .map_err(|_| base_err())?;
            let res = self
                .obj
                .call_method(py, "calc_token_amount", (), Some(&kwargs))
                .map_err(|_| base_err())?;
            let bound = res
                .extract::<Bound<'_, PyAny>>(py)
                .map_err(|_| base_err())?;
            extract_u256(&bound).map_err(|_| base_err())
        })
    }

    fn get_dy(&self, i: usize, j: usize, dx: U256, block: u64) -> Result<U256, CurveSwapError> {
        Python::attach(|py| {
            let kwargs = PyDict::new(py);
            kwargs.set_item("i", i).map_err(|_| base_err())?;
            kwargs.set_item("j", j).map_err(|_| base_err())?;
            let dx_py = alloy_py::u256_to_py(py, &dx).map_err(|_| base_err())?;
            kwargs.set_item("dx", &dx_py).map_err(|_| base_err())?;
            kwargs
                .set_item("block_identifier", block)
                .map_err(|_| base_err())?;
            let res = self
                .obj
                .call_method(py, "get_dy", (), Some(&kwargs))
                .map_err(|_| base_err())?;
            let bound = res
                .extract::<Bound<'_, PyAny>>(py)
                .map_err(|_| base_err())?;
            extract_u256(&bound).map_err(|_| base_err())
        })
    }

    fn calc_withdraw_one_coin(
        &self,
        token_amount: U256,
        i: usize,
        block: u64,
    ) -> Result<U256, CurveSwapError> {
        Python::attach(|py| {
            let kwargs = PyDict::new(py);
            let amt_py = alloy_py::u256_to_py(py, &token_amount).map_err(|_| base_err())?;
            kwargs
                .set_item("_token_amount", &amt_py)
                .map_err(|_| base_err())?;
            kwargs.set_item("i", i).map_err(|_| base_err())?;
            kwargs
                .set_item("block_identifier", block)
                .map_err(|_| base_err())?;
            let res = self
                .obj
                .call_method(py, "calc_withdraw_one_coin", (), Some(&kwargs))
                .map_err(|_| base_err())?;
            // The Python method returns a tuple; take element 0.
            let tuple = res
                .extract::<Bound<'_, pyo3::types::PyTuple>>(py)
                .map_err(|_| base_err())?;
            let bound = tuple.get_item(0).map_err(|_| base_err())?;
            extract_u256(&bound).map_err(|_| base_err())
        })
    }
}

/// `calculate_dy(i, j, dx, inputs)` — pure `get_dy` across all swap styles +
/// metapool fast-path.
///
/// # Errors
///
/// Returns `ValueError` on an invariant failure / bad index / unknown style.
#[pyfunction(signature = (i, j, dx, inputs))]
fn calculate_dy(
    i: usize,
    j: usize,
    dx: &Bound<'_, PyAny>,
    inputs: &Bound<'_, DyCalculationInputs>,
) -> PyResult<PyObject> {
    let py = dx.py();
    let inner = &inputs.borrow().inner;
    let result = core_calculate_dy(i, j, extract_u256(dx)?, inner).map_err(curve_swap_err)?;
    u256_to_py_obj(py, result)
}

/// `calculate_dy_underlying(i, j, dx, inputs, base)` — metapool swap into/out
/// of an underlying base-pool coin. `base` is a Python object exposing the
/// `BasePoolPort` surface (`tokens`, `fee`, `calc_token_amount`, `get_dy`,
/// `calc_withdraw_one_coin`).
///
/// # Errors
///
/// Returns `ValueError` on an invariant failure / base-pool delegation failure.
#[pyfunction(signature = (i, j, dx, inputs, base))]
fn calculate_dy_underlying(
    i: usize,
    j: usize,
    dx: &Bound<'_, PyAny>,
    inputs: &Bound<'_, DyCalculationInputs>,
    base: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = dx.py();
    let inner = &inputs.borrow().inner;
    let port = PyBasePoolPort {
        obj: base.clone().unbind(),
    };
    let result = core_calculate_dy_underlying(i, j, extract_u256(dx)?, inner, &port)
        .map_err(curve_swap_err)?;
    u256_to_py_obj(py, result)
}

/// Register the Curve `get_dy` calculator seam on `degenbot._ffi.curve_dy`.
///
/// # Errors
///
/// Returns `PyErr` if any symbol fails to register.
pub fn add_curve_dy_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.curve_dy")?;

    submod.add_class::<DyCalculationInputs>()?;
    submod.add_function(wrap_pyfunction!(calculate_dy, &submod)?)?;
    submod.add_function(wrap_pyfunction!(calculate_dy_underlying, &submod)?)?;

    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.curve_dy", &submod)?;

    Ok(())
}
