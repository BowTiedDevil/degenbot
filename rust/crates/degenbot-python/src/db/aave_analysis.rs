//! `PyO3` seam for the Aave V3 position **analysis math** — a thin
//! `#[pyfunction]` over [`degenbot_aave::analysis::analyze_user_position`].
//!
//! Takes plain `dict` records (the exact shape [`PyDatabasePositionQuery`]
//! returns) + a `dict[int, bool]` config map + an optional `dict[str, int]`
//! price map, releases the GIL across the pure `U256` math, and returns a
//! `#[pyclass]` `PyUserPositionSummary` exposing the same attribute names the
//! Python `UserPositionSummary` dataclass defined (so the CLI's
//! `_display_user_risk` + the parity test read attributes unchanged).
//!
//! The position collections (`collateral_positions` / `debt_positions`) are
//! exposed as `#[pyclass]` wrappers (`PyCollateralPositionData` /
//! `PyDebtPositionData`) — NOT plain dicts — because the CLI reads attributes
//! (`collateral_pos.actual_balance`, `collateral_pos.asset_symbol`, …) on each
//! position. Wrapping preserves that attribute-access shape so the Step C
//! cutover (`delete analysis/core.py`) needs no CLI rewrite.
//!
//! No business logic (ADR-005 three-layer architecture). The Python
//! `analyze_user_position` (`src/degenbot/aave/analysis/core.py`) becomes a
//! delegating shell over this seam in Step C.
//!
//! [`PyDatabasePositionQuery`]: crate::db::aave::PyDatabasePositionQuery

use std::collections::HashMap;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};
use pyo3::wrap_pyfunction;

use alloy::primitives::U256;

use crate::conversion::alloy::{extract_python_u256, u256_to_py};
use degenbot_aave::analysis::{
    analyze_user_position, CollateralPositionData, DebtPositionData, UserPositionSummary,
};
use degenbot_db::aave::{AaveCollateralPositionRecord, AaveDebtPositionRecord, AaveUserRecord};

// =========================================================================
// #[pyclass] wrappers — attribute access mirrors the Python dataclass fields
// =========================================================================

/// A user's Aave V3 position summary (health factor, LTV, position lists).
///
/// Rust-backed mirror of the Python `UserPositionSummary` dataclass. The
/// Step C cutover swaps the Python dataclass for this `#[pyclass]` — the CLI
/// + the parity test read the same attribute names.
#[pyclass(frozen, module = "degenbot._ffi.db")]
pub struct PyUserPositionSummary {
    inner: UserPositionSummary,
}

#[pymethods]
impl PyUserPositionSummary {
    /// The user's checksummed address.
    #[getter]
    fn user_address(&self) -> String {
        self.inner.user_address.clone()
    }

    /// The market id.
    #[getter]
    fn market_id(&self) -> i64 {
        self.inner.market_id
    }

    /// The eMode category id (`None` when the user is not in eMode).
    #[getter]
    fn emode_category_id(&self) -> Option<i64> {
        self.inner.emode_category_id
    }

    /// Whether the user is in isolation mode.
    #[getter]
    fn is_isolation_mode(&self) -> bool {
        self.inner.is_isolation_mode
    }

    /// The collateral positions (as `PyCollateralPositionData` wrappers).
    ///
    /// Returns a fresh `list` each call (the inner field is a `Vec`, not a
    /// Python object; wrapping each element on demand).
    #[getter]
    fn collateral_positions<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let out = PyList::empty(py);
        for pos in &self.inner.collateral_positions {
            out.append(Py::new(
                py,
                PyCollateralPositionData { inner: pos.clone() },
            )?)?;
        }
        Ok(out)
    }

    /// The debt positions (as `PyDebtPositionData` wrappers).
    #[getter]
    fn debt_positions<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let out = PyList::empty(py);
        for pos in &self.inner.debt_positions {
            out.append(Py::new(py, PyDebtPositionData { inner: pos.clone() })?)?;
        }
        Ok(out)
    }

    /// Total collateral value (price-adjusted, oracle base currency). A
    /// Python `int` (the `U256` sum) — matches the Python runtime where the
    /// `float`-typed field holds an `int`.
    #[getter]
    fn total_collateral_value<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        u256_to_py(py, &self.inner.total_collateral_value)
    }

    /// Total debt value (price-adjusted). A Python `int`.
    #[getter]
    fn total_debt_value<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        u256_to_py(py, &self.inner.total_debt_value)
    }

    /// The health factor (`None` when the user has no debt).
    #[getter]
    fn health_factor(&self) -> Option<f64> {
        self.inner.health_factor
    }

    /// The max-LTV ratio (`None` when no debt or no collateral capacity).
    #[getter]
    fn max_ltv_ratio(&self) -> Option<f64> {
        self.inner.max_ltv_ratio
    }

    /// `True` if the position is near liquidation (HF < at-risk threshold).
    #[getter]
    fn is_at_risk(&self) -> bool {
        self.inner.is_at_risk()
    }

    /// `True` if the position can be liquidated (HF < 1.0).
    #[getter]
    fn is_liquidatable(&self) -> bool {
        self.inner.is_liquidatable()
    }

    /// `True` if the user has any debt positions.
    #[getter]
    fn has_debt(&self) -> bool {
        self.inner.has_debt()
    }
}

/// Collateral position with calculated values (Rust-backed).
#[pyclass(frozen, module = "degenbot._ffi.db")]
pub struct PyCollateralPositionData {
    inner: CollateralPositionData,
}

#[pymethods]
impl PyCollateralPositionData {
    #[getter]
    fn asset_address(&self) -> String {
        self.inner.asset_address.clone()
    }

    #[getter]
    fn asset_symbol(&self) -> Option<String> {
        self.inner.asset_symbol.clone()
    }

    #[getter]
    fn scaled_balance<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        u256_to_py(py, &self.inner.scaled_balance)
    }

    #[getter]
    fn actual_balance<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        u256_to_py(py, &self.inner.actual_balance)
    }

    #[getter]
    fn liquidation_threshold(&self) -> i64 {
        self.inner.liquidation_threshold
    }

    #[getter]
    fn ltv(&self) -> i64 {
        self.inner.ltv
    }

    #[getter]
    fn is_enabled_as_collateral(&self) -> bool {
        self.inner.is_enabled_as_collateral
    }

    #[getter]
    fn in_emode(&self) -> bool {
        self.inner.in_emode
    }

    #[getter]
    fn emode_category_id(&self) -> Option<i64> {
        self.inner.emode_category_id
    }

    /// The oracle price (`None` when unavailable — the Rust core treats
    /// `None` as price = 1).
    #[getter]
    fn price<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match self.inner.price {
            Some(ref p) => u256_to_py(py, p),
            None => Ok(py.None().into_bound(py)),
        }
    }
}

/// Debt position with calculated values (Rust-backed).
#[pyclass(frozen, module = "degenbot._ffi.db")]
pub struct PyDebtPositionData {
    inner: DebtPositionData,
}

#[pymethods]
impl PyDebtPositionData {
    #[getter]
    fn asset_address(&self) -> String {
        self.inner.asset_address.clone()
    }

    #[getter]
    fn asset_symbol(&self) -> Option<String> {
        self.inner.asset_symbol.clone()
    }

    #[getter]
    fn scaled_balance<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        u256_to_py(py, &self.inner.scaled_balance)
    }

    #[getter]
    fn actual_balance<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        u256_to_py(py, &self.inner.actual_balance)
    }

    #[getter]
    fn stable_debt(&self) -> bool {
        self.inner.stable_debt
    }

    #[getter]
    fn in_emode(&self) -> bool {
        self.inner.in_emode
    }

    #[getter]
    fn emode_category_id(&self) -> Option<i64> {
        self.inner.emode_category_id
    }

    #[getter]
    fn price<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match self.inner.price {
            Some(ref p) => u256_to_py(py, p),
            None => Ok(py.None().into_bound(py)),
        }
    }
}

// =========================================================================
// dict → Rust record extraction (under the GIL — touches Python)
// =========================================================================

/// Extract an [`AaveUserRecord`] from a Python `dict` (the
/// `PyDatabasePositionQuery.get_users_with_debt` row shape).
fn extract_user_record(obj: &Bound<'_, PyAny>) -> PyResult<AaveUserRecord> {
    let d = obj.cast::<PyDict>()?;
    let id: i64 = d
        .get_item("id")?
        .ok_or_else(|| missing_key("id"))?
        .extract()?;
    let address: String = d
        .get_item("address")?
        .ok_or_else(|| missing_key("address"))?
        .extract()?;
    let market_id: i64 = d
        .get_item("market_id")?
        .ok_or_else(|| missing_key("market_id"))?
        .extract()?;
    let e_mode: i64 = d
        .get_item("e_mode")?
        .ok_or_else(|| missing_key("e_mode"))?
        .extract()?;
    let is_isolation_mode: bool = d
        .get_item("is_isolation_mode")?
        .ok_or_else(|| missing_key("is_isolation_mode"))?
        .extract()?;
    let isolation_mode_debt = extract_python_u256(
        &d.get_item("isolation_mode_debt")?
            .ok_or_else(|| missing_key("isolation_mode_debt"))?,
    )?;
    let isolation_debt_ceiling = match d.get_item("isolation_debt_ceiling")? {
        Some(v) if v.is_none() => None,
        Some(v) => Some(extract_python_u256(&v)?),
        None => None,
    };
    Ok(AaveUserRecord {
        id,
        address,
        market_id,
        e_mode,
        is_isolation_mode,
        isolation_mode_debt,
        isolation_debt_ceiling,
    })
}

/// Extract a `Vec<AaveCollateralPositionRecord>` from a Python `list[dict]`.
fn extract_collateral_records(
    list: &Bound<'_, PyList>,
) -> PyResult<Vec<AaveCollateralPositionRecord>> {
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        let d = item.cast::<PyDict>()?;
        let asset_id: i64 = d
            .get_item("asset_id")?
            .ok_or_else(|| missing_key("asset_id"))?
            .extract()?;
        let balance = extract_python_u256(
            &d.get_item("balance")?
                .ok_or_else(|| missing_key("balance"))?,
        )?;
        let underlying_address: String = d
            .get_item("underlying_address")?
            .ok_or_else(|| missing_key("underlying_address"))?
            .extract()?;
        let underlying_symbol = match d.get_item("underlying_symbol")? {
            Some(v) if v.is_none() => None,
            Some(v) => Some(v.extract()?),
            None => None,
        };
        let liquidity_index = extract_python_u256(
            &d.get_item("liquidity_index")?
                .ok_or_else(|| missing_key("liquidity_index"))?,
        )?;
        let e_mode_category_id = match d.get_item("e_mode_category_id")? {
            Some(v) if v.is_none() => None,
            Some(v) => Some(v.extract()?),
            None => None,
        };
        let asset_lt: i64 = d
            .get_item("asset_lt")?
            .ok_or_else(|| missing_key("asset_lt"))?
            .extract()?;
        let asset_ltv: i64 = d
            .get_item("asset_ltv")?
            .ok_or_else(|| missing_key("asset_ltv"))?
            .extract()?;
        let emode_lt = match d.get_item("emode_lt")? {
            Some(v) if v.is_none() => None,
            Some(v) => Some(v.extract()?),
            None => None,
        };
        let emode_ltv = match d.get_item("emode_ltv")? {
            Some(v) if v.is_none() => None,
            Some(v) => Some(v.extract()?),
            None => None,
        };
        out.push(AaveCollateralPositionRecord {
            asset_id,
            balance,
            underlying_address,
            underlying_symbol,
            liquidity_index,
            e_mode_category_id,
            asset_lt,
            asset_ltv,
            emode_lt,
            emode_ltv,
        });
    }
    Ok(out)
}

/// Extract a `Vec<AaveDebtPositionRecord>` from a Python `list[dict]`.
fn extract_debt_records(list: &Bound<'_, PyList>) -> PyResult<Vec<AaveDebtPositionRecord>> {
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        let d = item.cast::<PyDict>()?;
        let asset_id: i64 = d
            .get_item("asset_id")?
            .ok_or_else(|| missing_key("asset_id"))?
            .extract()?;
        let balance = extract_python_u256(
            &d.get_item("balance")?
                .ok_or_else(|| missing_key("balance"))?,
        )?;
        let underlying_address: String = d
            .get_item("underlying_address")?
            .ok_or_else(|| missing_key("underlying_address"))?
            .extract()?;
        let underlying_symbol = match d.get_item("underlying_symbol")? {
            Some(v) if v.is_none() => None,
            Some(v) => Some(v.extract()?),
            None => None,
        };
        let borrow_index = extract_python_u256(
            &d.get_item("borrow_index")?
                .ok_or_else(|| missing_key("borrow_index"))?,
        )?;
        let e_mode_category_id = match d.get_item("e_mode_category_id")? {
            Some(v) if v.is_none() => None,
            Some(v) => Some(v.extract()?),
            None => None,
        };
        out.push(AaveDebtPositionRecord {
            asset_id,
            balance,
            underlying_address,
            underlying_symbol,
            borrow_index,
            e_mode_category_id,
        });
    }
    Ok(out)
}

/// Extract an `asset_id -> enabled` map from a Python `dict[int, bool]`.
fn extract_config_map(dict: &Bound<'_, PyDict>) -> PyResult<HashMap<i64, bool>> {
    let mut out = HashMap::with_capacity(dict.len());
    for (k, v) in dict {
        let asset_id: i64 = k.extract()?;
        let enabled: bool = v.extract()?;
        out.insert(asset_id, enabled);
    }
    Ok(out)
}

/// Extract an optional `address -> price` map from a Python
/// `dict[str, int] | None`.
fn extract_price_map(dict: Option<&Bound<'_, PyDict>>) -> PyResult<Option<HashMap<String, U256>>> {
    match dict {
        None => Ok(None),
        Some(d) => {
            let mut out = HashMap::with_capacity(d.len());
            for (k, v) in d {
                let addr: String = k.extract()?;
                let price = extract_python_u256(&v)?;
                out.insert(addr, price);
            }
            Ok(Some(out))
        }
    }
}

fn missing_key(key: &str) -> PyErr {
    PyValueError::new_err(format!("missing required key '{key}' in position dict"))
}

// =========================================================================
// the #[pyfunction]
// =========================================================================

/// `degenbot._ffi.db.analyze_aave_user_position(user, collateral_positions,
/// debt_positions, collateral_config_map, price_map=None) ->
/// PyUserPositionSummary`
///
/// Analyze a single user's Aave V3 position for liquidation risk. Pure math
/// (no I/O) — delegates to [`degenbot_aave::analysis::analyze_user_position`].
///
/// Args:
///     user: A `dict` with keys `id`, `address`, `market_id`, `e_mode`,
///         `is_isolation_mode`, `isolation_mode_debt`,
///         `isolation_debt_ceiling` (the shape
///         `PyDatabasePositionQuery.get_users_with_debt` returns).
///     `collateral_positions`: A `list[dict]` (the
///         `get_collateral_positions` row shape).
///     `debt_positions`: A `list[dict]` (the `get_debt_positions` row shape).
///     `collateral_config_map`: A `dict[int, bool]` (`asset_id` -> enabled).
///     `price_map`: An optional `dict[str, int]` (address -> oracle price in 8
///         decimals). When `None`, prices are treated as 1.
///
/// Returns:
///     A `PyUserPositionSummary` with attribute access matching the Python
///     `UserPositionSummary` dataclass (`health_factor`, `max_ltv_ratio`,
///     `collateral_positions`, …).
///
/// Raises:
///     `ValueError`: On a missing dict key, a malformed value, or a scaled-
///         balance computation overflow.
#[pyfunction]
#[pyo3(signature = (user, collateral_positions, debt_positions, collateral_config_map, price_map=None))]
pub(crate) fn analyze_aave_user_position(
    py: Python<'_>,
    user: &Bound<'_, PyAny>,
    collateral_positions: &Bound<'_, PyList>,
    debt_positions: &Bound<'_, PyList>,
    collateral_config_map: &Bound<'_, PyDict>,
    price_map: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyUserPositionSummary>> {
    // Extract all args under the GIL (touches Python), then release the GIL
    // for the pure U256 math.
    let user_record = extract_user_record(user)?;
    let collateral = extract_collateral_records(collateral_positions)?;
    let debt = extract_debt_records(debt_positions)?;
    let cfg = extract_config_map(collateral_config_map)?;
    let prices = extract_price_map(price_map)?;

    let summary = py
        .detach(|| analyze_user_position(&user_record, &collateral, &debt, &cfg, prices.as_ref()))
        .map_err(|e| PyValueError::new_err(e.to_string()))?;

    Py::new(py, PyUserPositionSummary { inner: summary })
}

// =========================================================================
// registration
// =========================================================================

/// Register the Aave analysis seam on the `db` submodule.
///
/// Adds the `analyze_aave_user_position` pyfunction + the three `#[pyclass]`
/// wrappers. Mirrors the `aave::PyDatabasePositionQuery` registration
/// (gated on `aave-updater`, which brings in the `degenbot-aave` dep).
///
/// # Errors
///
/// Returns a [`PyErr`] if any `add_function`/`add_class` call fails.
pub fn register_aave_analysis(submod: &Bound<'_, PyModule>) -> PyResult<()> {
    submod.add_function(wrap_pyfunction!(analyze_aave_user_position, submod)?)?;
    submod.add_class::<PyUserPositionSummary>()?;
    submod.add_class::<PyCollateralPositionData>()?;
    submod.add_class::<PyDebtPositionData>()?;
    Ok(())
}

#[cfg(all(test, feature = "auto-initialize"))]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_simple_position() {
        Python::attach(|py| {
            let user = PyDict::new(py);
            user.set_item("id", 1i64).unwrap();
            user.set_item("address", "0xabc").unwrap();
            user.set_item("market_id", 7i64).unwrap();
            user.set_item("e_mode", 0i64).unwrap();
            user.set_item("is_isolation_mode", false).unwrap();
            user.set_item("isolation_mode_debt", 0i64).unwrap();
            user.set_item("isolation_debt_ceiling", py.None()).unwrap();

            let collateral = PyList::empty(py);
            let col = PyDict::new(py);
            col.set_item("asset_id", 1i64).unwrap();
            col.set_item("balance", 1000i64).unwrap();
            col.set_item("underlying_address", "0xweth").unwrap();
            col.set_item("underlying_symbol", "WETH").unwrap();
            col.set_item("liquidity_index", 10u128.pow(27)).unwrap();
            col.set_item("e_mode_category_id", py.None()).unwrap();
            col.set_item("asset_lt", 8000i64).unwrap();
            col.set_item("asset_ltv", 7500i64).unwrap();
            col.set_item("emode_lt", py.None()).unwrap();
            col.set_item("emode_ltv", py.None()).unwrap();
            collateral.append(col).unwrap();

            let debt = PyList::empty(py);
            let d = PyDict::new(py);
            d.set_item("asset_id", 2i64).unwrap();
            d.set_item("balance", 500i64).unwrap();
            d.set_item("underlying_address", "0xusdc").unwrap();
            d.set_item("underlying_symbol", "USDC").unwrap();
            d.set_item("borrow_index", 10u128.pow(27)).unwrap();
            d.set_item("e_mode_category_id", py.None()).unwrap();
            debt.append(d).unwrap();

            let cfg = PyDict::new(py);
            cfg.set_item(1i64, true).unwrap();

            let summary =
                analyze_aave_user_position(py, &user, &collateral, &debt, &cfg, None).unwrap();

            let bound = summary.bind(py);
            assert_eq!(
                bound
                    .getattr("market_id")
                    .unwrap()
                    .extract::<i64>()
                    .unwrap(),
                7
            );
            let hf = bound
                .getattr("health_factor")
                .unwrap()
                .extract::<Option<f64>>()
                .unwrap();
            assert!(hf.is_some_and(|f| f > 1.0));
        });
    }
}
