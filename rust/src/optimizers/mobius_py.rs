//! PyO3 Python bindings for the integer-exact (U512) Möbius solver.
//!
//! The f64 Möbius surface that used to live here (`RustArbSolver`,
//! `RustHopState`, `RustV3TickRange*`, `RustTickRangeCrossing`,
//! `py_mobius_refine_int`, `py_int_mobius_solve`, `py_int_simulate_path`) has
//! been removed — see `rust/CONTEXT.md` ruling "f64 vs U512 Möbius solver
//! stack". What remains is the integer-exact seam consumed by the Python
//! orchestrator-era solver classes (`ArbSolver` registered-path solving via
//! `RustPoolCache`, and `RustIntHopState` for hop construction).
//!
//! All solving flows through [`exact_mobius_solve`] (the single U512-native
//! Möbius solver).

#![allow(clippy::must_use_candidate)]
#![allow(clippy::use_self)]
#![allow(clippy::let_and_return)]
#![allow(clippy::redundant_field_names)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::float_cmp)]
#![allow(clippy::suboptimal_flops)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::unused_self)]
#![allow(clippy::option_if_let_else)]
#![allow(clippy::unnecessary_wraps)]

use crate::alloy_py::{extract_python_u256, PyU256};
use crate::optimizers::mobius_int::{int_simulate_path, IntHopState};
use crate::optimizers::mobius_int_exact::exact_mobius_solve;
use alloy::primitives::U256;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyList};

// ===========================================================================
// Solve method discriminator (mirrors the value the Python side maps back to
// `SolverMethod` via `ArbSolver._RUST_METHOD_MAP`).
// ===========================================================================

/// Method tag carried on [`PyArbResult`].
enum SolveMethod {
    Mobius = 0,
    NotSupported = 255,
}

// ===========================================================================
// PyArbResult — integer-exact solve result
// ===========================================================================

/// EVM-exact integer solve result.
///
/// Replaces the former f64-carrying result. Every field is integer-exact;
/// there is no float `optimal_input`/`profit` (the f64 fields are gone with
/// the f64 solver). `optimal_input_int`/`profit_int` are always `Some` for
/// supported Möbius results.
#[pyclass(name = "RustArbResult", skip_from_py_object)]
pub struct PyArbResult {
    pub iterations: u32,
    pub success: bool,
    pub method: u8,
    pub supported: bool,
    pub optimal_input_int: Option<U256>,
    pub profit_int: Option<U256>,
}

#[pymethods]
impl PyArbResult {
    #[getter]
    fn iterations(&self) -> u32 {
        self.iterations
    }

    #[getter]
    fn success(&self) -> bool {
        self.success
    }

    #[getter]
    fn method(&self) -> u8 {
        self.method
    }

    #[getter]
    fn supported(&self) -> bool {
        self.supported
    }

    /// EVM-exact integer optimal input.
    #[getter]
    fn optimal_input_int<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        match self.optimal_input_int {
            Some(v) => Ok(Some(PyU256(v).into_pyobject(py)?)),
            None => Ok(None),
        }
    }

    /// EVM-exact integer profit.
    #[getter]
    fn profit_int<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        match self.profit_int {
            Some(v) => Ok(Some(PyU256(v).into_pyobject(py)?)),
            None => Ok(None),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "RustArbResult(iterations={}, success={}, method={}, supported={}, optimal_input_int={:?}, profit_int={:?})",
            self.iterations,
            self.success,
            self.method,
            self.supported,
            self.optimal_input_int.unwrap_or_default(),
            self.profit_int.unwrap_or_default(),
        )
    }
}

/// Build a not-supported `PyArbResult`.
fn not_supported_result() -> PyArbResult {
    PyArbResult {
        iterations: 0,
        success: false,
        method: SolveMethod::NotSupported as u8,
        supported: false,
        optimal_input_int: None,
        profit_int: None,
    }
}

/// Solve a pure Möbius (constant/bounded product) integer path.
///
/// Runs the U512-native closed-form solver ([`exact_mobius_solve`]). When
/// `max_input` is set and the unconstrained optimum exceeds it, the optimal
/// input is clamped to the cap and profit is recomputed via EVM-exact
/// simulation — the constrained optimum for a unimodal Möbius profit curve
/// is the boundary.
fn solve_mobius(int_hops: &[IntHopState], max_input: Option<U256>) -> PyArbResult {
    let Ok(result) = exact_mobius_solve(int_hops) else {
        return not_supported_result();
    };

    if !result.is_profitable || result.optimal_input.is_zero() || result.profit.is_zero() {
        return PyArbResult {
            iterations: 0,
            success: false,
            method: SolveMethod::Mobius as u8,
            supported: true,
            optimal_input_int: Some(U256::ZERO),
            profit_int: Some(U256::ZERO),
        };
    }

    let (optimal_input, profit) = match max_input {
        Some(cap) if result.optimal_input > cap => {
            // Constrained optimum: clamp to the cap and recompute profit exactly.
            if cap.is_zero() {
                return not_supported_result();
            }
            let output = int_simulate_path(cap, int_hops).final_output;
            if output > cap {
                (cap, output - cap)
            } else {
                return not_supported_result();
            }
        }
        _ => (result.optimal_input, result.profit),
    };

    if profit.is_zero() {
        return not_supported_result();
    }

    PyArbResult {
        iterations: 0,
        success: true,
        method: SolveMethod::Mobius as u8,
        supported: true,
        optimal_input_int: Some(optimal_input),
        profit_int: Some(profit),
    }
}

// ===========================================================================
// PyIntHopState — integer hop state exposed to Python
// ===========================================================================

/// Integer (U256) hop state for the integer-exact solver.
#[pyclass(name = "RustIntHopState", skip_from_py_object)]
#[derive(Clone)]
pub struct PyIntHopState {
    pub inner: IntHopState,
}

#[pymethods]
impl PyIntHopState {
    #[new]
    #[pyo3(signature = (reserve_in, reserve_out, gamma_numer, fee_denom))]
    fn new(
        reserve_in: &Bound<'_, PyAny>,
        reserve_out: &Bound<'_, PyAny>,
        gamma_numer: u64,
        fee_denom: u64,
    ) -> PyResult<Self> {
        let r_in = extract_python_u256(reserve_in)?;
        let r_out = extract_python_u256(reserve_out)?;
        Ok(Self {
            inner: IntHopState::new(r_in, r_out, gamma_numer, fee_denom),
        })
    }

    #[getter]
    fn reserve_in<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        PyU256(self.inner.reserve_in).into_pyobject(py)
    }

    #[getter]
    fn reserve_out<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        PyU256(self.inner.reserve_out).into_pyobject(py)
    }
}

// ===========================================================================
// PyPoolCache — registered-path solving over the integer-exact solver
// ===========================================================================

use lru::LruCache;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};

/// A pre-resolved registered path: ordered `IntHopState` list + source pool IDs.
struct RegisteredPath {
    hops: Vec<IntHopState>,
    pool_ids: Vec<u64>,
}

static NEXT_PATH_ID: AtomicU64 = AtomicU64::new(1);

/// Convert a U256 to a Python int with a fast path for u64-fit values.
fn u256_to_py_fast(py: Python<'_>, val: U256) -> PyResult<Bound<'_, PyAny>> {
    let bytes = val.to_be_bytes::<32>();
    // Fast path: value fits in u64 (top 24 bytes are zero).
    if bytes[..24].iter().all(|b| *b == 0) {
        let mut lo = [0u8; 8];
        lo.copy_from_slice(&bytes[24..32]);
        let v = u64::from_be_bytes(lo);
        return Ok(v.into_pyobject(py)?.into_any());
    }
    // Slow path: large values via from_be_bytes.
    Ok(PyU256(val).into_pyobject(py)?.into_any())
}

/// `RustPoolCache` — integer-exact registered-path solving.
///
/// Pool states are stored as [`IntHopState`]. Registered paths hold pre-resolved
/// `IntHopState` lists; solving flows through the U512-native
/// [`exact_mobius_solve`]. The f64 refinement path is gone.
#[pyclass(name = "RustPoolCache", skip_from_py_object)]
#[allow(clippy::missing_errors_doc)]
pub struct PyPoolCache {
    pools: Mutex<LruCache<u64, IntHopState>>,
    /// Pre-registered paths: path_id → resolved `IntHopState` list.
    paths: Mutex<HashMap<u64, RegisteredPath>>,
}

#[pymethods]
impl PyPoolCache {
    #[new]
    fn new() -> Self {
        const CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(10_000).unwrap(); // infallible for non-zero literal
        Self {
            pools: Mutex::new(LruCache::new(CACHE_CAPACITY)),
            paths: Mutex::new(HashMap::new()),
        }
    }

    /// Insert or update a pool's state in the cache.
    #[pyo3(signature = (pool_id, reserve_in, reserve_out, gamma_numer, fee_denom))]
    fn insert(
        &self,
        pool_id: u64,
        reserve_in: &Bound<'_, PyAny>,
        reserve_out: &Bound<'_, PyAny>,
        gamma_numer: u64,
        fee_denom: u64,
    ) -> PyResult<()> {
        let r_in = extract_python_u256(reserve_in)?;
        let r_out = extract_python_u256(reserve_out)?;

        if gamma_numer >= fee_denom {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "gamma_numer ({gamma_numer}) must be less than fee_denom ({fee_denom})"
            )));
        }

        self.pools
            .lock()
            .put(pool_id, IntHopState::new(r_in, r_out, gamma_numer, fee_denom));
        Ok(())
    }

    /// Remove a pool from the cache. Returns True if found and removed.
    #[pyo3(signature = (pool_id))]
    fn remove(&self, pool_id: u64) -> bool {
        self.pools.lock().pop(&pool_id).is_some()
    }

    /// Solve an arbitrage path using cached pool states.
    #[pyo3(signature = (path, max_input=None))]
    fn solve(
        &self,
        py: Python<'_>,
        path: &Bound<'_, PyList>,
        max_input: Option<f64>,
    ) -> PyResult<PyArbResult> {
        let pool_ids: Vec<u64> = path.extract()?;

        if pool_ids.len() < 2 {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "Need at least 2 pools in path, got {}",
                pool_ids.len()
            )));
        }

        let int_hops: Vec<IntHopState> = {
            let mut cache = self.pools.lock();
            let mut hops = Vec::with_capacity(pool_ids.len());
            for &pool_id in &pool_ids {
                let Some(hop_state) = cache.get(&pool_id).cloned() else {
                    return Ok(not_supported_result());
                };
                hops.push(hop_state);
            }
            hops
        };

        let max_input_u256 = max_input.map(f64_to_u256_cap);
        Ok(py.detach(|| solve_mobius(&int_hops, max_input_u256)))
    }

    /// Solve multiple arbitrage paths in a single Python → Rust round-trip.
    #[pyo3(signature = (paths, max_input=None))]
    fn solve_batch(
        &self,
        py: Python<'_>,
        paths: &Bound<'_, PyList>,
        max_input: Option<f64>,
    ) -> PyResult<Py<PyList>> {
        let num_paths = paths.len();
        if num_paths == 0 {
            return Ok(PyList::empty(py).unbind());
        }

        let pool_ids_list: Vec<Vec<u64>> = paths.extract()?;
        let max_input_u256 = max_input.map(f64_to_u256_cap);

        let assembled: Vec<Option<Vec<IntHopState>>> = {
            let mut cache = self.pools.lock();
            pool_ids_list
                .iter()
                .map(|pool_ids| {
                    if pool_ids.len() < 2 {
                        return None;
                    }
                    let mut hops = Vec::with_capacity(pool_ids.len());
                    for &pool_id in pool_ids {
                        let hop_state = cache.get(&pool_id).cloned()?;
                        hops.push(hop_state);
                    }
                    Some(hops)
                })
                .collect()
        };

        let results: Vec<PyArbResult> = py.detach(|| {
            assembled
                .iter()
                .map(|opt| match opt {
                    Some(hops) => solve_mobius(hops, max_input_u256),
                    None => not_supported_result(),
                })
                .collect()
        });

        let py_list = PyList::empty(py);
        for result in results {
            py_list.append(result)?;
        }
        Ok(py_list.unbind())
    }

    /// Register an arbitrage path by its pool IDs.
    ///
    /// If a pool ID is not in the cache, the path is registered with empty
    /// hops and returns not_supported when solved — call `update_path()`
    /// after inserting the missing pool.
    #[pyo3(signature = (pool_ids))]
    fn register_path(&self, pool_ids: Vec<u64>) -> PyResult<u64> {
        if pool_ids.len() < 2 {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "Need at least 2 pools in path, got {}",
                pool_ids.len()
            )));
        }

        let path_id = NEXT_PATH_ID.fetch_add(1, Ordering::Relaxed);

        let hops: Vec<IntHopState> = {
            let mut cache = self.pools.lock();
            let mut hops = Vec::with_capacity(pool_ids.len());
            for &pool_id in &pool_ids {
                let Some(hop_state) = cache.get(&pool_id).cloned() else {
                    // Pool missing — register empty; caller must update_path().
                    drop(cache);
                    {
                        let mut paths = self.paths.lock();
                        paths.insert(path_id, RegisteredPath {
                            hops: Vec::new(),
                            pool_ids,
                        });
                    }
                    return Ok(path_id);
                };
                hops.push(hop_state);
            }
            hops
        };

        {
            let mut paths = self.paths.lock();
            paths.insert(path_id, RegisteredPath { hops, pool_ids });
        }

        Ok(path_id)
    }

    /// Re-resolve a previously registered path's pool states.
    /// Returns True if the path was found, False if not found.
    #[pyo3(signature = (path_id))]
    fn update_path(&self, path_id: u64) -> bool {
        let pool_ids = {
            let paths = self.paths.lock();
            let Some(registered) = paths.get(&path_id) else {
                return false;
            };
            registered.pool_ids.clone()
        };

        let hops: Vec<IntHopState> = {
            let mut cache = self.pools.lock();
            let mut hops = Vec::with_capacity(pool_ids.len());
            for &pool_id in &pool_ids {
                let Some(hop_state) = cache.get(&pool_id).cloned() else {
                    // Pool missing — clear hops so solve returns not_supported.
                    drop(cache);
                    {
                        let mut paths = self.paths.lock();
                        if let Some(rp) = paths.get_mut(&path_id) {
                            rp.hops.clear();
                        }
                    }
                    return true;
                };
                hops.push(hop_state);
            }
            hops
        };

        {
            let mut paths = self.paths.lock();
            if let Some(rp) = paths.get_mut(&path_id) {
                rp.hops = hops;
            }
        }

        true
    }

    /// Re-resolve all registered paths after a batch pool state update.
    /// Returns the number of paths updated.
    fn update_all_paths(&self) -> usize {
        let all_pool_ids: Vec<(u64, Vec<u64>)> = {
            let paths = self.paths.lock();
            paths
                .iter()
                .map(|(&id, rp)| (id, rp.pool_ids.clone()))
                .collect()
        };

        let resolved: Vec<(u64, bool, Vec<IntHopState>)> = {
            let mut cache = self.pools.lock();
            all_pool_ids
                .iter()
                .map(|(path_id, pool_ids)| {
                    let mut hops = Vec::with_capacity(pool_ids.len());
                    let mut path_valid = true;
                    for &pool_id in pool_ids {
                        let Some(hop_state) = cache.get(&pool_id).cloned() else {
                            path_valid = false;
                            break;
                        };
                        hops.push(hop_state);
                    }
                    (*path_id, path_valid, hops)
                })
                .collect()
        };

        let mut paths = self.paths.lock();
        let mut updated = 0;
        for (path_id, path_valid, hops) in resolved {
            if let Some(rp) = paths.get_mut(&path_id) {
                if path_valid {
                    rp.hops = hops;
                } else {
                    rp.hops.clear();
                }
                updated += 1;
            }
        }

        updated
    }

    /// Remove a registered path. Returns True if found and removed.
    #[pyo3(signature = (path_id))]
    fn remove_path(&self, path_id: u64) -> bool {
        self.paths.lock().remove(&path_id).is_some()
    }

    /// Solve multiple pre-registered paths by path ID.
    #[pyo3(signature = (path_ids, max_input=None))]
    fn solve_registered(
        &self,
        py: Python<'_>,
        path_ids: Vec<u64>,
        max_input: Option<f64>,
    ) -> PyResult<Py<PyList>> {
        if path_ids.is_empty() {
            return Ok(PyList::empty(py).unbind());
        }

        let resolved: Vec<Option<Vec<IntHopState>>> = {
            let paths_lock = self.paths.lock();
            path_ids
                .iter()
                .map(|id| paths_lock.get(id).map(|rp| rp.hops.clone()))
                .collect()
        };
        let max_input_u256 = max_input.map(f64_to_u256_cap);

        let results: Vec<PyArbResult> = py.detach(|| {
            resolved
                .iter()
                .map(|opt| match opt {
                    None => not_supported_result(),
                    Some(hops) if hops.len() < 2 => not_supported_result(),
                    Some(hops) => solve_mobius(hops, max_input_u256),
                })
                .collect()
        });

        let py_list = PyList::empty(py);
        for result in results {
            py_list.append(result)?;
        }
        Ok(py_list.unbind())
    }

    /// Solve multiple pre-registered paths, returning only integer results
    /// as a flat list: `[input0, profit0, input1, profit1, ...]`.
    ///
    /// Minimum-overhead solve path. Profitable paths return their integer
    /// optimal input + profit; unsupported/unprofitable paths return `0, 0`.
    #[pyo3(signature = (path_ids, max_input=None))]
    fn solve_registered_ints(
        &self,
        py: Python<'_>,
        path_ids: Vec<u64>,
        max_input: Option<f64>,
    ) -> PyResult<Py<PyList>> {
        if path_ids.is_empty() {
            return Ok(PyList::empty(py).unbind());
        }

        let resolved: Vec<Option<Vec<IntHopState>>> = {
            let paths_lock = self.paths.lock();
            path_ids
                .iter()
                .map(|id| paths_lock.get(id).map(|rp| rp.hops.clone()))
                .collect()
        };
        let max_input_u256 = max_input.map(f64_to_u256_cap);

        let int_results: Vec<Option<(U256, U256)>> = py.detach(|| {
            resolved
                .iter()
                .map(|opt| match opt {
                    None => None,
                    Some(hops) if hops.len() < 2 => None,
                    Some(hops) => {
                        let result = solve_mobius(hops, max_input_u256);
                        if result.success {
                            result
                                .optimal_input_int
                                .zip(result.profit_int)
                                .filter(|(_, profit)| !profit.is_zero())
                        } else {
                            None
                        }
                    }
                })
                .collect()
        });

        let py_list = PyList::empty(py);
        for opt_result in &int_results {
            if let Some((optimal_input, profit)) = opt_result {
                let input_py = u256_to_py_fast(py, *optimal_input)?;
                let profit_py = u256_to_py_fast(py, *profit)?;
                py_list.append(input_py)?;
                py_list.append(profit_py)?;
            } else {
                py_list.append(0)?;
                py_list.append(0)?;
            }
        }

        Ok(py_list.unbind())
    }

    /// Check if a pool ID is in the cache.
    #[pyo3(signature = (pool_id))]
    fn contains(&self, pool_id: u64) -> bool {
        self.pools.lock().contains(&pool_id)
    }

    /// Number of pools in the cache.
    fn __len__(&self) -> usize {
        self.pools.lock().len()
    }

    /// Check if the cache is empty.
    #[must_use]
    fn __bool__(&self) -> bool {
        !self.pools.lock().is_empty()
    }

    fn __repr__(&self) -> String {
        format!("RustPoolCache(pools={})", self.pools.lock().len())
    }

    /// Clear all pools from the cache.
    fn clear(&self) {
        self.pools.lock().clear();
    }
}

/// Convert an f64 max-input cap to a U256, clamped to U256::MAX and floored
/// toward zero (matches the f64→U256 truncation the prior gen-2 path used).
fn f64_to_u256_cap(v: f64) -> U256 {
    const U256_MAX_AS_F64: f64 = 1.157_920_892_373_162e77;
    const U64_MAX_AS_F64: f64 = u64::MAX as f64;

    if v <= 0.0 || !v.is_finite() {
        return U256::ZERO;
    }
    if v >= U256_MAX_AS_F64 {
        return U256::MAX;
    }
    if v < U64_MAX_AS_F64 {
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        return U256::from(v as u64);
    }

    // 4-limb iterative decomposition for the general case.
    let mut remaining = v;
    let mut limbs = [0u64; 4];
    for limb in &mut limbs {
        let upper = remaining / 2f64.powi(64);
        let lower_f64 = remaining - upper * 2f64.powi(64);
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let limb_val = lower_f64 as u64;
        *limb = limb_val;
        remaining = upper;
        if upper == 0.0 {
            break;
        }
    }
    U256::from_limbs(limbs)
}

// ===========================================================================
// Python module registration
// ===========================================================================

/// Register the integer-exact Möbius PyO3 classes (`RustArbResult`,
/// `RustPoolCache`, `RustIntHopState`) on `m`.
///
/// # Errors
///
/// Returns a [`PyResult`] error if a class cannot be added to the module.
pub fn add_mobius_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyArbResult>()?;
    m.add_class::<PyPoolCache>()?;
    m.add_class::<PyIntHopState>()?;
    Ok(())
}
