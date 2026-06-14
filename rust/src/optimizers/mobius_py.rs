//! PyO3 Python bindings for the Möbius transformation optimizer.

#![allow(clippy::must_use_candidate)]
#![allow(clippy::use_self)]
#![allow(clippy::let_and_return)]
#![allow(clippy::redundant_field_names)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::float_cmp)]
#![allow(clippy::suboptimal_flops)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::unused_self)]
#![allow(clippy::option_if_let_else)]
#![allow(clippy::unnecessary_wraps)]

use crate::alloy_py::{extract_python_u256, PyU256};
use crate::optimizers::mobius::HopState;

use crate::optimizers::mobius_int::{
    mobius_refine_int, mobius_solve_with_refinement, u256_to_f64, IntHopState,
};
use crate::optimizers::mobius_v3::{
    solve_v3_tick_range_sequence, TickRangeCrossing, V3TickRangeHop,
    V3TickRangeSequence,
};
use crate::optimizers::mobius_v3_v3::solve_v3_v3;
use alloy::primitives::U256;

use pyo3::prelude::*;
use pyo3::types::PyList;

/// Reserve and fee state for a single pool hop.
#[pyclass(name = "RustHopState", skip_from_py_object)]
#[derive(Clone)]
pub struct PyHopState {
    pub inner: HopState,
}

#[pymethods]
impl PyHopState {
    #[new]
    #[pyo3(signature = (reserve_in, reserve_out, fee))]
    fn new(reserve_in: f64, reserve_out: f64, fee: f64) -> Self {
        Self {
            inner: HopState::new(reserve_in, reserve_out, fee),
        }
    }

    #[getter]
    fn reserve_in(&self) -> f64 {
        self.inner.reserve_in
    }

    #[getter]
    fn reserve_out(&self) -> f64 {
        self.inner.reserve_out
    }

    #[getter]
    fn fee(&self) -> f64 {
        self.inner.fee
    }

    fn __repr__(&self) -> String {
        format!(
            "RustHopState(reserve_in={}, reserve_out={}, fee={})",
            self.inner.reserve_in, self.inner.reserve_out, self.inner.fee
        )
    }
}

/// V3 tick range hop data.
#[pyclass(name = "RustV3TickRangeHop", skip_from_py_object)]
#[derive(Clone)]
pub struct PyV3TickRangeHop {
    pub inner: V3TickRangeHop,
}

#[pymethods]
impl PyV3TickRangeHop {
    #[new]
    #[pyo3(signature = (liquidity, sqrt_price_current, sqrt_price_lower, sqrt_price_upper, fee, zero_for_one))]
    fn new(
        liquidity: f64,
        sqrt_price_current: f64,
        sqrt_price_lower: f64,
        sqrt_price_upper: f64,
        fee: f64,
        zero_for_one: bool,
    ) -> Self {
        Self {
            inner: V3TickRangeHop {
                liquidity,
                sqrt_price_current,
                sqrt_price_lower,
                sqrt_price_upper,
                fee,
                zero_for_one,
            },
        }
    }

    #[getter]
    fn liquidity(&self) -> f64 {
        self.inner.liquidity
    }

    #[getter]
    fn sqrt_price_current(&self) -> f64 {
        self.inner.sqrt_price_current
    }

    #[getter]
    fn sqrt_price_lower(&self) -> f64 {
        self.inner.sqrt_price_lower
    }

    #[getter]
    fn sqrt_price_upper(&self) -> f64 {
        self.inner.sqrt_price_upper
    }

    #[getter]
    fn fee(&self) -> f64 {
        self.inner.fee
    }

    #[getter]
    fn zero_for_one(&self) -> bool {
        self.inner.zero_for_one
    }

    /// Lower bound on R0: L / √P_upper.
    fn alpha(&self) -> f64 {
        self.inner.alpha()
    }

    /// Lower bound on R1: L · √P_lower.
    fn beta(&self) -> f64 {
        self.inner.beta()
    }

    /// Convert to a RustHopState with effective reserves.
    fn to_hop_state(&self) -> PyHopState {
        PyHopState {
            inner: self.inner.to_hop_state(),
        }
    }

    /// Check if a sqrt price is within this tick range.
    #[pyo3(signature = (sqrt_price))]
    fn contains_sqrt_price(&self, sqrt_price: f64) -> bool {
        self.inner.contains_sqrt_price(sqrt_price)
    }

    /// Maximum gross input (including fees) this range can absorb without
    /// pushing the price past the range boundary.
    #[pyo3(signature = ())]
    fn max_gross_input_in_range(&self) -> f64 {
        self.inner.max_gross_input_in_range()
    }

    fn __repr__(&self) -> String {
        format!(
            "RustV3TickRangeHop(L={}, sqrt_p={}, range=[{}, {}], fee={}, zfo={})",
            self.inner.liquidity,
            self.inner.sqrt_price_current,
            self.inner.sqrt_price_lower,
            self.inner.sqrt_price_upper,
            self.inner.fee,
            self.inner.zero_for_one
        )
    }
}

/// Python wrapper for V3TickRangeSequence.
#[pyclass(name = "RustV3TickRangeSequence")]
pub struct PyV3TickRangeSequence {
    pub inner: V3TickRangeSequence,
}

#[pymethods]
impl PyV3TickRangeSequence {
    #[new]
    #[pyo3(signature = (ranges))]
    fn new(ranges: &Bound<'_, PyList>) -> PyResult<Self> {
        let mut rust_ranges = Vec::new();
        for item in ranges.iter() {
            let py_v3 = item.extract::<PyRef<PyV3TickRangeHop>>()?;
            rust_ranges.push(py_v3.inner.clone());
        }

        match V3TickRangeSequence::new(rust_ranges) {
            Ok(seq) => Ok(Self { inner: seq }),
            Err(_) => Err(pyo3::exceptions::PyValueError::new_err(
                "Invalid V3 tick range sequence",
            )),
        }
    }

    /// Compute crossing data to reach range k.
    #[pyo3(signature = (k))]
    fn compute_crossing(&self, k: usize) -> PyResult<PyTickRangeCrossing> {
        match self.inner.compute_crossing(k) {
            Ok(crossing) => Ok(PyTickRangeCrossing { inner: crossing }),
            Err(_) => Err(pyo3::exceptions::PyValueError::new_err(
                "Invalid range index",
            )),
        }
    }

    /// Number of ranges in the sequence.
    fn __len__(&self) -> usize {
        self.inner.ranges.len()
    }

    /// Get the i-th range as a RustV3TickRangeHop.
    fn __getitem__(&self, idx: usize) -> PyResult<PyV3TickRangeHop> {
        if idx < self.inner.ranges.len() {
            Ok(PyV3TickRangeHop {
                inner: self.inner.ranges[idx].clone(),
            })
        } else {
            Err(pyo3::exceptions::PyIndexError::new_err("Index out of range"))
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "RustV3TickRangeSequence(ranges={})",
            self.inner.ranges.len()
        )
    }
}

/// Python wrapper for TickRangeCrossing.
#[pyclass(name = "RustTickRangeCrossing")]
pub struct PyTickRangeCrossing {
    pub inner: TickRangeCrossing,
}

#[pymethods]
impl PyTickRangeCrossing {
    #[new]
    #[pyo3(signature = (crossing_gross_input, crossing_output, ending_range))]
    fn new(
        crossing_gross_input: f64,
        crossing_output: f64,
        ending_range: &PyV3TickRangeHop,
    ) -> Self {
        Self {
            inner: TickRangeCrossing {
                crossing_gross_input,
                crossing_output,
                ending_range: ending_range.inner.clone(),
            },
        }
    }

    #[getter]
    fn crossing_gross_input(&self) -> f64 {
        self.inner.crossing_gross_input
    }

    #[getter]
    fn crossing_output(&self) -> f64 {
        self.inner.crossing_output
    }

    #[getter]
    fn ending_range(&self) -> PyV3TickRangeHop {
        PyV3TickRangeHop {
            inner: self.inner.ending_range.clone(),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "RustTickRangeCrossing(crossing_gross_input={}, crossing_output={})",
            self.inner.crossing_gross_input, self.inner.crossing_output
        )
    }
}

// ==========================================================================
// Unified ArbSolver — Rust dispatch
// ==========================================================================

/// Method tags returned by the unified solver.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SolveMethod {
    Mobius = 0,
    PiecewiseMobius = 1,
    V3V3 = 2,
    NotSupported = 255,
}

/// Result from the unified arb solver.
#[pyclass(name = "RustArbResult")]
pub struct PyArbResult {
    pub optimal_input: f64,
    pub profit: f64,
    pub iterations: u32,
    pub success: bool,
    pub method: u8,
    pub supported: bool,
    /// EVM-exact integer optimal input. Set when integer hops are provided
    /// and method is Möbius.
    pub optimal_input_int: Option<U256>,
    /// EVM-exact integer profit. Set when integer hops are provided
    /// and method is Möbius.
    pub profit_int: Option<U256>,
}

#[pymethods]
impl PyArbResult {
    #[getter]
    fn optimal_input(&self) -> f64 {
        self.optimal_input
    }

    #[getter]
    fn profit(&self) -> f64 {
        self.profit
    }

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

    /// EVM-exact integer optimal input (set when int hops provided, Möbius method).
    #[getter]
    fn optimal_input_int<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        match self.optimal_input_int {
            Some(v) => Ok(Some(PyU256(v).into_pyobject(py)?)),
            None => Ok(None),
        }
    }

    /// EVM-exact integer profit (set when int hops provided, Möbius method).
    #[getter]
    fn profit_int<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        match self.profit_int {
            Some(v) => Ok(Some(PyU256(v).into_pyobject(py)?)),
            None => Ok(None),
        }
    }

    fn __repr__(&self) -> String {
        match (self.optimal_input_int, self.profit_int) {
            (Some(_), Some(_)) => format!(
                "RustArbResult(optimal_input={}, profit={}, iterations={}, success={}, method={}, supported={}, optimal_input_int={:?}, profit_int={:?})",
                self.optimal_input, self.profit, self.iterations, self.success, self.method, self.supported,
                self.optimal_input_int.unwrap_or_default(),
                self.profit_int.unwrap_or_default(),
            ),
            _ => format!(
                "RustArbResult(optimal_input={}, profit={}, iterations={}, success={}, method={}, supported={})",
                self.optimal_input, self.profit, self.iterations, self.success, self.method, self.supported
            ),
        }
    }
}

/// Parse a Python list of hops into float HopState and optional IntHopState lists.
///
/// Returns `(base_hops, int_hops, all_int, unsupported)`.
/// When all hops are RustIntHopState, `all_int=true` and `int_hops` is populated.
fn parse_hops(hops: &Bound<'_, PyList>) -> PyResult<(Vec<HopState>, Vec<IntHopState>, bool, bool)> {
    let mut base_hops: Vec<HopState> = Vec::new();
    let mut int_hops: Vec<IntHopState> = Vec::new();
    let mut all_int = true;
    let mut unsupported = false;

    for item in hops.iter() {
        // Try as (reserve_in, reserve_out, fee) tuple
        if let Ok(tuple) = item.extract::<(f64, f64, f64)>() {
            base_hops.push(HopState::new(tuple.0, tuple.1, tuple.2));
            all_int = false;
        }
        // Try as RustIntHopState
        else if let Ok(py_hop) = item.extract::<PyRef<PyIntHopState>>() {
            let int_hop = py_hop.inner.clone();
            let r_in_f64 = u256_to_f64(int_hop.reserve_in);
            let r_out_f64 = u256_to_f64(int_hop.reserve_out);
            let fee_f64 = 1.0 - (int_hop.gamma_numer as f64 / int_hop.fee_denom as f64);
            base_hops.push(HopState::new(r_in_f64, r_out_f64, fee_f64));
            int_hops.push(int_hop);
        }
        // Try as RustHopState
        else if let Ok(py_hop) = item.extract::<PyRef<PyHopState>>() {
            base_hops.push(py_hop.inner);
            all_int = false;
        } else {
            unsupported = true;
        }
    }

    // Integer refinement only works for pure int hops (no mixing)
    if !int_hops.is_empty() && !all_int {
        all_int = false;
        int_hops.clear();
    }

    Ok((base_hops, int_hops, all_int, unsupported))
}

/// Parse V3 sequence data from a Python list.
///
/// Returns `(v3_seqs, unsupported)`.
fn parse_v3_sequences(
    v3_list: &Bound<'_, PyList>,
) -> PyResult<(Vec<(usize, V3TickRangeSequence)>, bool)> {
    let mut v3_seqs: Vec<(usize, V3TickRangeSequence)> = Vec::new();
    let mut unsupported = false;

    for item in v3_list.iter() {
        if let Ok(py_tuple) = item.cast::<pyo3::types::PyTuple>() {
            if py_tuple.len() == 2 {
                let idx: usize = py_tuple.get_item(0)?.extract()?;
                let seq: PyRef<PyV3TickRangeSequence> = py_tuple.get_item(1)?.extract()?;
                v3_seqs.push((idx, seq.inner.clone()));
            } else {
                unsupported = true;
            }
        } else {
            unsupported = true;
        }
    }

    Ok((v3_seqs, unsupported))
}

/// Build a not-supported PyArbResult.
fn not_supported_result() -> PyArbResult {
    PyArbResult {
        optimal_input: 0.0,
        profit: 0.0,
        iterations: 0,
        success: false,
        method: SolveMethod::NotSupported as u8,
        supported: false,
        optimal_input_int: None,
        profit_int: None,
    }
}

/// Solve a pure Möbius (constant/bounded product) path.
///
/// When `all_int` is true and `int_hops` is populated, does merged
/// integer refinement and returns EVM-exact integer results.
fn solve_mobius(
    base_hops: &[HopState],
    int_hops: &[IntHopState],
    all_int: bool,
    max_input: Option<f64>,
) -> PyArbResult {
    let result = mobius_solve_with_refinement(base_hops, int_hops, all_int, max_input);
    PyArbResult {
        optimal_input: result.optimal_input,
        profit: result.profit,
        iterations: result.iterations,
        success: result.success,
        method: SolveMethod::Mobius as u8,
        supported: true,
        optimal_input_int: result.optimal_input_int,
        profit_int: result.profit_int,
    }
}

/// Unified arbitrage solver with Rust dispatch.
///
/// Accepts mixed hop types and automatically selects the best solver.
/// Returns `supported=False` for hop types not handled by Rust
/// (Solidly, Balancer, Curve), so Python can fall back.
#[pyclass(name = "RustArbSolver")]
pub struct PyArbSolver;

#[pymethods]
impl PyArbSolver {
    #[new]
    fn new() -> Self {
        Self
    }

    /// Unified solve entry point with automatic method selection.
    ///
    /// `hops` is a flat list of one of:
    /// - `(reserve_in, reserve_out, fee)` float tuples
    /// - `RustHopState` objects
    /// - `RustIntHopState` objects (EVM-exact integer reserves)
    ///
    /// When all hops are `RustIntHopState`, the solver does float Möbius solve
    /// + U256 integer refinement in a single call, returning EVM-exact integer
    /// results via `optimal_input_int` and `profit_int` fields.
    ///
    /// `v3_sequences` is an optional list of `(hop_index, RustV3TickRangeSequence)`
    /// for V3 hops that have multi-range tick crossing data. Not compatible
    /// with `RustIntHopState` hops (integer refinement only applies to Möbius paths).
    ///
    /// Returns a `RustArbResult` with `supported=False` if Rust cannot handle
    /// the path (e.g. Solidly, Balancer, Curve hops).
    #[pyo3(signature = (hops, v3_sequences=None, max_input=None, max_candidates=10))]
    #[allow(clippy::too_many_lines)]
    fn solve(
        &self,
        py: Python<'_>,
        hops: &Bound<'_, PyList>,
        v3_sequences: Option<&Bound<'_, PyList>>,
        max_input: Option<f64>,
        max_candidates: usize,
    ) -> PyResult<PyArbResult> {
        let (base_hops, mut int_hops, mut all_int, mut unsupported) = parse_hops(hops)?;

        let v3_seqs = if let Some(v3_list) = v3_sequences {
            all_int = false;
            int_hops.clear();
            let (seqs, v3_unsupported) = parse_v3_sequences(v3_list)?;
            unsupported = unsupported || v3_unsupported;
            seqs
        } else {
            Vec::new()
        };

        if unsupported || base_hops.len() < 2 {
            return Ok(not_supported_result());
        }

        if v3_seqs.is_empty() {
            return Ok(py.detach(|| solve_mobius(&base_hops, &int_hops, all_int, max_input)));
        } else if v3_seqs.len() == 2 && base_hops.len() == 2 {
            let seq0 = v3_seqs[0].1.clone();
            let seq1 = v3_seqs[1].1.clone();
            let (x_opt, profit, iters) =
                py.detach(|| solve_v3_v3(&seq0, &seq1, max_input, max_candidates));
            return Ok(PyArbResult {
                optimal_input: x_opt,
                profit,
                iterations: iters,
                success: x_opt > 0.0 && profit > 0.0,
                method: SolveMethod::V3V3 as u8,
                supported: true,
                optimal_input_int: None,
                profit_int: None,
            });
        } else if v3_seqs.len() == 1 {
            let v3_idx = v3_seqs[0].0;
            let seq = v3_seqs[0].1.clone();
            let (x_opt, profit, iters) = py.detach(|| {
                solve_v3_tick_range_sequence(&base_hops, v3_idx, &seq, max_candidates, max_input)
            });
            return Ok(PyArbResult {
                optimal_input: x_opt,
                profit,
                iterations: iters,
                success: x_opt > 0.0 && profit > 0.0,
                method: SolveMethod::PiecewiseMobius as u8,
                supported: true,
                optimal_input_int: None,
                profit_int: None,
            });
        }

        Ok(not_supported_result())
    }

    /// Solve with raw flat integer arrays, avoiding Python object construction.
    ///
    /// This is the fast path for V2/V3-single-range paths where all hops
    /// have integer reserves. Instead of creating `RustIntHopState` Python
    /// objects (each costing ~1μs of PyO3 extraction), the caller passes a
    /// flat list of Python ints and the Rust side parses them directly.
    ///
    /// Parameters
    /// ----------
    /// int_hops_flat : list of int
    ///     Flat array with 4 elements per hop:
    ///     [reserve_in, reserve_out, gamma_numer, fee_denom] per hop.
    ///     gamma_numer = fee_denom - fee.numerator (e.g. 997 for 0.3% fee).
    ///     reserve_in and reserve_out are Python ints (up to 2^256-1).
    ///     gamma_numer and fee_denom are Python ints (must fit in u64).
    /// max_input : float or None
    ///     Optional upper bound on input amount.
    ///
    /// Returns
    /// -------
    /// RustArbResult
    ///     Same as solve(), with integer fields populated for Möbius results.
    #[pyo3(signature = (int_hops_flat, max_input=None))]
    fn solve_raw(
        &self,
        py: Python<'_>,
        int_hops_flat: &Bound<'_, PyList>,
        max_input: Option<f64>,
    ) -> PyResult<PyArbResult> {
        // Validate array length: 4 elements per hop
        let n = int_hops_flat.len();
        if n % 4 != 0 {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "int_hops_flat length ({n}) must be a multiple of 4"
            )));
        }
        let num_hops = n / 4;
        if num_hops < 2 {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "Need at least 2 hops, got {num_hops}"
            )));
        }

        let mut base_hops: Vec<HopState> = Vec::with_capacity(num_hops);
        let mut int_hops: Vec<IntHopState> = Vec::with_capacity(num_hops);

        for i in 0..num_hops {
            let r_in_obj = int_hops_flat.get_item(i * 4)?;
            let r_out_obj = int_hops_flat.get_item(i * 4 + 1)?;
            let gamma_numer_obj = int_hops_flat.get_item(i * 4 + 2)?;
            let fee_denom_obj = int_hops_flat.get_item(i * 4 + 3)?;

            let r_in = extract_python_u256(&r_in_obj)?;
            let r_out = extract_python_u256(&r_out_obj)?;
            let gamma_numer: u64 = gamma_numer_obj.extract()?;
            let fee_denom: u64 = fee_denom_obj.extract()?;

            if gamma_numer >= fee_denom {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    format!(
                        "gamma_numer ({gamma_numer}) must be less than fee_denom ({fee_denom}) for hop {i}"
                    ),
                ));
            }

            int_hops.push(IntHopState::new(r_in, r_out, gamma_numer, fee_denom));

            // Derive float HopState from integer reserves for the float solve
            let r_in_f64 = u256_to_f64(r_in);
            let r_out_f64 = u256_to_f64(r_out);
            let fee_f64 = 1.0 - (gamma_numer as f64 / fee_denom as f64);
            base_hops.push(HopState::new(r_in_f64, r_out_f64, fee_f64));
        }

        Ok(py.detach(|| solve_mobius(&base_hops, &int_hops, true, max_input)))
    }

    /// Solve multiple arbitrage paths in a single Python → Rust round-trip.
    ///
    /// All paths are parsed under the GIL, then solved inside a single
    /// `py.detach()` call. This amortizes the ~1,160ns PyO3 bridge overhead
    /// across all paths instead of paying it per-path.
    ///
    /// Parameters
    /// ----------
    /// paths : list of list of int
    ///     Each inner list is an ordered list of pool IDs along an arbitrage
    ///     path. Must have at least 2 pool IDs.
    /// max_input : float or None
    ///     Optional maximum input constraint (applied to all paths).
    ///
    /// Returns
    /// -------
    /// list of RustArbResult
    ///     One result per path. Paths with missing pool IDs or < 2 hops
    ///     are returned as not_supported.
    #[pyo3(signature = (paths, max_input=None))]
    fn solve_raw_batch(
        &self,
        py: Python<'_>,
        paths: &Bound<'_, PyList>,
        max_input: Option<f64>,
    ) -> PyResult<Py<PyList>> {
        let num_paths = paths.len();
        if num_paths == 0 {
            return Ok(PyList::empty(py).unbind());
        }

        // Phase 1 (GIL-held): Parse all paths into (base_hops, int_hops) pairs
        let mut assembled: Vec<(
            bool,   // supported
            Vec<HopState>,
            Vec<IntHopState>,
        )> = Vec::with_capacity(num_paths);

        for path_item in paths.iter() {
            let int_flat: &Bound<'_, PyList> = path_item.cast()?;
            let n = int_flat.len();

            if n % 4 != 0 || n / 4 < 2 {
                assembled.push((false, Vec::new(), Vec::new()));
                continue;
            }

            let num_hops = n / 4;
            let mut base_hops: Vec<HopState> = Vec::with_capacity(num_hops);
            let mut int_hops: Vec<IntHopState> = Vec::with_capacity(num_hops);
            let mut path_valid = true;

            for i in 0..num_hops {
                let r_in_obj = int_flat.get_item(i * 4)?;
                let r_out_obj = int_flat.get_item(i * 4 + 1)?;
                let gamma_numer_obj = int_flat.get_item(i * 4 + 2)?;
                let fee_denom_obj = int_flat.get_item(i * 4 + 3)?;

                let Ok(r_in) = extract_python_u256(&r_in_obj) else {
                    path_valid = false;
                    break;
                };
                let Ok(r_out) = extract_python_u256(&r_out_obj) else {
                    path_valid = false;
                    break;
                };
                let Ok(gamma_numer) = gamma_numer_obj.extract::<u64>() else {
                    path_valid = false;
                    break;
                };
                let Ok(fee_denom) = fee_denom_obj.extract::<u64>() else {
                    path_valid = false;
                    break;
                };

                if gamma_numer >= fee_denom {
                    path_valid = false;
                    break;
                }

                int_hops.push(IntHopState::new(r_in, r_out, gamma_numer, fee_denom));
                let r_in_f64 = u256_to_f64(r_in);
                let r_out_f64 = u256_to_f64(r_out);
                let fee_f64 = 1.0 - (gamma_numer as f64 / fee_denom as f64);
                base_hops.push(HopState::new(r_in_f64, r_out_f64, fee_f64));
            }

            if path_valid {
                assembled.push((true, base_hops, int_hops));
            } else {
                assembled.push((false, Vec::new(), Vec::new()));
            }
        }

        // Phase 2 (GIL-released): Solve all valid paths in one batch
        let results: Vec<PyArbResult> = py.detach(|| {
            assembled
                .iter()
                .map(|(supported, base_hops, int_hops)| {
                    if !supported {
                        return not_supported_result();
                    }
                    solve_mobius(base_hops, int_hops, true, max_input)
                })
                .collect()
        });

        // Phase 3 (GIL-held): Build Python list of results
        let py_list = PyList::empty(py);
        for result in results {
            py_list.append(result)?;
        }

        Ok(py_list.unbind())
    }

    /// Verify that `py.detach()` releases the GIL by spawning an OS thread
    /// that re-acquires it via `Python::attach()`.
    ///
    /// Returns `True` if the spawned thread successfully acquired the GIL
    /// while the current thread had released it inside `py.detach()`,
    /// proving the GIL was actually released. Returns `False` if the spawned
    /// thread could not acquire the GIL (meaning `py.detach()` did not release it).
    ///
    /// This is a deterministic, timing-free test — no sleep or scheduling
    /// assumptions required.
    #[pyo3(signature = ())]
    fn verify_gil_release(&self, py: Python<'_>) -> bool {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let gil_acquired_by_other = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&gil_acquired_by_other);

        py.detach(|| {
            // GIL is released here. Spawn an OS thread that tries Python::attach().
            // If py.detach() actually released the GIL, Python::attach() will succeed
            // (it re-acquires the GIL). If not, Python::attach() would deadlock
            // waiting for the GIL — but detach is documented to release it.
            let handle = std::thread::spawn(move || {
                Python::attach(|_py| {
                    flag.store(true, Ordering::SeqCst);
                });
            });
            handle.join().expect("GIL re-acquisition thread panicked");
        });

        gil_acquired_by_other.load(Ordering::SeqCst)
    }
}

// ==========================================================================
// RustPoolCache — direct pool state to Rust solver
// ==========================================================================

use lru::LruCache;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};

/// Pre-resolved hop states for a registered path.
///
/// When a path is registered, its pool IDs are resolved to concrete
/// `IntHopState` values once and stored. On solve, no pool lookup
/// or float conversion is needed — just iterate and solve.
struct RegisteredPath {
    hops: ResolvedHops,
    pool_ids: Vec<u64>,
}

/// Type alias for pre-resolved hop state pairs.
type ResolvedHops = Vec<(HopState, IntHopState)>;

/// Global counter for auto-assigned path IDs.
static NEXT_PATH_ID: AtomicU64 = AtomicU64::new(1);

/// Convert a U256 to a Python int with a fast path for u64-fit values.
///
/// For values that fit in a single u64 (high 3 limbs are zero), uses
/// `PyInt::new(py, val)` which is a single C API call (~20ns).
/// For larger values, falls back to `int.from_bytes()` (~160ns).
///
/// This is the key optimization for `solve_registered_ints`: most arbitrage
/// results (optimal_input, profit) fit in u64 for common pools, so the
/// fast path is hit almost always.
fn u256_to_py_fast(py: Python<'_>, val: U256) -> PyResult<Bound<'_, PyAny>> {
    let limbs = val.as_limbs(); // &[u64; 4]
    if limbs[1] == 0 && limbs[2] == 0 && limbs[3] == 0 {
        // Fits in u64
        let low = limbs[0];
        if let Ok(signed) = i64::try_from(low) {
            // Fits in i64 — use fast C API path
            Ok(pyo3::types::PyInt::new(py, signed).into_any())
        } else {
            // u64 value > i64::MAX: must use from_bytes to avoid cast wrapping
            PyU256(val).into_pyobject(py)
        }
    } else {
        // Needs big-int conversion
        PyU256(val).into_pyobject(py)
    }
}

/// Cached pool state for fast arbitrage solving by pool ID.
///
/// Pool states are registered once (at pool update time) and then
/// solved by ID reference, eliminating all Python object construction
/// and per-item extraction overhead on the solve path.
///
/// Two solve modes are available:
/// - `solve([pool_ids])` / `solve_batch([[pool_ids], ...])` — resolve pool IDs
///   on every call. Simple but incurs per-call lock + lookup overhead.
/// - `register_path([pool_ids])` → `solve_registered([path_ids])` — resolve
///   pool IDs once at registration time, then solve by path ID reference.
///   This eliminates all per-solve pool lookups, float conversions, and
///   lock acquisitions. The solve hot path becomes: look up pre-built
///   `(HopState, IntHopState)` vectors by path ID, then call `py.detach()`
///   once for the entire batch.
///
/// Uses LRU eviction (capacity 10,000) for pool state to prevent unbounded
/// memory growth in long-running processes. `Mutex` is required because
/// `LruCache::get()` takes `&mut self` (it updates LRU ordering on
/// access), but `PyPoolCache::solve(&self, ...)` must remain `&self`
/// (pyo3 convention). `Mutex` is safe under both GIL-enabled and
/// free-threaded Python builds: the lock is uncontended in normal use
/// (GIL-enabled: only one thread runs Python code at a time), and the
/// solve path calls no Python code while holding the lock.
#[pyclass(name = "RustPoolCache")]
pub struct PyPoolCache {
    pools: Mutex<LruCache<u64, IntHopState>>,
    /// Pre-registered paths: path_id → resolved (HopState, IntHopState) pairs.
    paths: Mutex<HashMap<u64, RegisteredPath>>,
}
#[pymethods]
impl PyPoolCache {
    #[new]
    fn new() -> Self {
        /// LRU capacity for the pool cache.
        const CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(10_000).unwrap(); // infallible for non-zero literal
        Self {
            pools: Mutex::new(LruCache::new(CACHE_CAPACITY)),
            paths: Mutex::new(HashMap::new()),
        }
    }

    /// Insert or update a pool's state in the cache.
    ///
    /// Parameters
    /// ----------
    /// pool_id : int
    ///     Unique pool identifier (e.g. hash of pool address).
    /// reserve_in : int
    ///     Input reserve (up to 2^256-1).
    /// reserve_out : int
    ///     Output reserve (up to 2^256-1).
    /// gamma_numer : int
    ///     Gamma numerator = fee_denom - fee.numerator (e.g. 997 for 0.3% fee).
    ///     Must fit in u64.
    /// fee_denom : int
    ///     Fee denominator (e.g. 1000 for 0.3% fee). Must fit in u64.
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

    /// Remove a pool from the cache.
    ///
    /// Parameters
    /// ----------
    /// pool_id : int
    ///     Pool identifier to remove.
    ///
    /// Returns True if the pool was found and removed, False otherwise.
    #[pyo3(signature = (pool_id))]
    fn remove(&self, pool_id: u64) -> bool {
        self.pools
            .lock()
            .pop(&pool_id)
            .is_some()
    }

    /// Solve an arbitrage path using cached pool states.
    ///
    /// Looks up each pool by ID, assembles the IntHopState list,
    /// and calls the same Möbius + U256 integer refinement pipeline.
    ///
    /// Parameters
    /// ----------
    /// path : list of int
    ///     Ordered list of pool IDs along the arbitrage path.
    /// max_input : float or None
    ///     Optional maximum input constraint.
    ///
    /// Returns
    /// -------
    /// RustArbResult
    ///     Same result format as RustArbSolver.solve()/solve_raw().
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

        let mut int_hops: Vec<IntHopState> = Vec::with_capacity(pool_ids.len());
        let mut base_hops: Vec<HopState> = Vec::with_capacity(pool_ids.len());

        for &pool_id in &pool_ids {
            let hop_state = self
                .pools
                .lock()
                .get(&pool_id)
                .cloned()
                .ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err(format!(
                        "Pool ID {pool_id} not found in cache"
                    ))
                })?;

            int_hops.push(hop_state.clone());
            let r_in_f64 = u256_to_f64(hop_state.reserve_in);
            let r_out_f64 = u256_to_f64(hop_state.reserve_out);
            let fee_f64 = 1.0 - (hop_state.gamma_numer as f64 / hop_state.fee_denom as f64);
            base_hops.push(HopState::new(r_in_f64, r_out_f64, fee_f64));
        }

        Ok(py.detach(|| solve_mobius(&base_hops, &int_hops, true, max_input)))
    }

    /// Solve multiple arbitrage paths in a single Python → Rust round-trip.
    ///
    /// All paths are looked up under the GIL (one lock acquisition), then
    /// solved inside a single `py.detach()` call. This amortizes the
    /// ~1,160ns PyO3 bridge overhead across all paths.
    ///
    /// Parameters
    /// ----------
    /// paths : list of list of int
    ///     Each inner list is an ordered list of pool IDs along an arbitrage
    ///     path. Must have at least 2 pool IDs.
    /// max_input : float or None
    ///     Optional maximum input constraint (applied to all paths).
    ///
    /// Returns
    /// -------
    /// list of RustArbResult
    ///     One result per path. Paths with missing pool IDs or < 2 hops
    ///     are returned as not_supported.
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

        // Phase 1 (GIL-held): Look up all pool states for all paths
        // One lock acquisition for the entire batch
        let pool_ids_list: Vec<Vec<u64>> = paths.extract()?;

        let mut cache = self.pools.lock();

        let mut assembled: Vec<(
            bool,   // supported
            Vec<HopState>,
            Vec<IntHopState>,
        )> = Vec::with_capacity(num_paths);

        for pool_ids in &pool_ids_list {
            if pool_ids.len() < 2 {
                assembled.push((false, Vec::new(), Vec::new()));
                continue;
            }

            let mut base_hops: Vec<HopState> = Vec::with_capacity(pool_ids.len());
            let mut int_hops: Vec<IntHopState> = Vec::with_capacity(pool_ids.len());
            let mut path_valid = true;

            for &pool_id in pool_ids {
                let Some(hop_state) = cache.get(&pool_id).cloned() else {
                    path_valid = false;
                    break;
                };

                int_hops.push(hop_state.clone());
                let r_in_f64 = u256_to_f64(hop_state.reserve_in);
                let r_out_f64 = u256_to_f64(hop_state.reserve_out);
                let fee_f64 = 1.0 - (hop_state.gamma_numer as f64 / hop_state.fee_denom as f64);
                base_hops.push(HopState::new(r_in_f64, r_out_f64, fee_f64));
            }

            if path_valid {
                assembled.push((true, base_hops, int_hops));
            } else {
                assembled.push((false, Vec::new(), Vec::new()));
            }
        }

        // Release cache lock before GIL release — no longer needed
        drop(cache);

        // Phase 2 (GIL-released): Solve all valid paths in one batch
        let results: Vec<PyArbResult> = py.detach(|| {
            assembled
                .iter()
                .map(|(supported, base_hops, int_hops)| {
                    if !supported {
                        return not_supported_result();
                    }
                    solve_mobius(base_hops, int_hops, true, max_input)
                })
                .collect()
        });

        // Phase 3 (GIL-held): Build Python list of results
        let py_list = PyList::empty(py);
        for result in results {
            py_list.append(result)?;
        }

        Ok(py_list.unbind())
    }

    /// Register an arbitrage path by its pool IDs.
    ///
    /// Resolves the pool IDs to concrete `IntHopState` values once and
    /// stores them under an auto-assigned path ID. Subsequent calls to
    /// `solve_registered()` use this path ID, eliminating all per-solve
    /// pool lookups, lock acquisitions, and float conversions.
    ///
    /// If a pool ID is not found in the cache, the path is still registered
    /// but will return not_supported when solved. Call `update_path()` after
    /// registering missing pools.
    ///
    /// Parameters
    /// ----------
    /// pool_ids : list of int
    ///     Ordered list of pool IDs along the arbitrage path.
    ///     Must have at least 2 pool IDs.
    ///
    /// Returns
    /// -------
    /// int
    ///     The auto-assigned path ID for use with `solve_registered()`.
    #[pyo3(signature = (pool_ids))]
    fn register_path(&self, pool_ids: Vec<u64>) -> PyResult<u64> {
        if pool_ids.len() < 2 {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "Need at least 2 pools in path, got {}",
                pool_ids.len()
            )));
        }

        let path_id = NEXT_PATH_ID.fetch_add(1, Ordering::Relaxed);

        // Resolve pool IDs to hop states
        let mut cache = self.pools.lock();
        let mut hops = Vec::with_capacity(pool_ids.len());

        for &pool_id in &pool_ids {
            let Some(hop_state) = cache.get(&pool_id).cloned() else {
                // Pool not found — register with empty hops, caller must
                // call update_path() after inserting the pool
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

            let r_in_f64 = u256_to_f64(hop_state.reserve_in);
            let r_out_f64 = u256_to_f64(hop_state.reserve_out);
            let fee_f64 = 1.0 - (hop_state.gamma_numer as f64 / hop_state.fee_denom as f64);
            let base_hop = HopState::new(r_in_f64, r_out_f64, fee_f64);
            hops.push((base_hop, hop_state));
        }
        drop(cache);

        {
            let mut paths = self.paths.lock();
            paths.insert(path_id, RegisteredPath {
                hops,
                pool_ids,
            });
        }

        Ok(path_id)
    }

    /// Re-resolve a previously registered path's pool states.
    ///
    /// Call this after updating pool states (e.g., at block boundaries)
    /// to refresh the pre-resolved hop states from the pool cache.
    ///
    /// Parameters
    /// ----------
    /// path_id : int
    ///     The path ID returned by `register_path()`.
    ///
    /// Returns True if the path was found and updated, False if not found.
    #[pyo3(signature = (path_id))]
    fn update_path(&self, path_id: u64) -> bool {
        let pool_ids = {
            let paths = self.paths.lock();
            let Some(registered) = paths.get(&path_id) else {
                return false;
            };
            let pool_ids = registered.pool_ids.clone();
            drop(paths);
            pool_ids
        };

        // Re-resolve pool IDs
        let mut cache = self.pools.lock();
        let mut hops = Vec::with_capacity(pool_ids.len());

        for &pool_id in &pool_ids {
            let Some(hop_state) = cache.get(&pool_id).cloned() else {
                // Pool missing — clear hops so solve returns not_supported
                drop(cache);
                {
                    let mut paths = self.paths.lock();
                    if let Some(rp) = paths.get_mut(&path_id) {
                        rp.hops.clear();
                    }
                }
                return true;
            };

            let r_in_f64 = u256_to_f64(hop_state.reserve_in);
            let r_out_f64 = u256_to_f64(hop_state.reserve_out);
            let fee_f64 = 1.0 - (hop_state.gamma_numer as f64 / hop_state.fee_denom as f64);
            let base_hop = HopState::new(r_in_f64, r_out_f64, fee_f64);
            hops.push((base_hop, hop_state));
        }
        drop(cache);

        {
            let mut paths = self.paths.lock();
            if let Some(rp) = paths.get_mut(&path_id) {
                rp.hops = hops;
            }
        }

        true
    }

    /// Re-resolve all registered paths after a batch pool state update.
    ///
    /// More efficient than calling `update_path()` individually because
    /// it acquires the pool cache lock once for all paths.
    ///
    /// Returns the number of paths updated.
    fn update_all_paths(&self) -> usize {
        let all_pool_ids: Vec<(u64, Vec<u64>)> = {
            let paths = self.paths.lock();
            paths
                .iter()
                .map(|(&id, rp)| (id, rp.pool_ids.clone()))
                .collect()
        };

        // Resolve all pool IDs under a single cache lock
        let mut cache = self.pools.lock();
        let mut resolved: Vec<(u64, bool, ResolvedHops)> = Vec::with_capacity(all_pool_ids.len());
        for (path_id, pool_ids) in &all_pool_ids {
            let mut hops = Vec::with_capacity(pool_ids.len());
            let mut path_valid = true;

            for &pool_id in pool_ids {
                let Some(hop_state) = cache.get(&pool_id).cloned() else {
                    path_valid = false;
                    break;
                };

                let r_in_f64 = u256_to_f64(hop_state.reserve_in);
                let r_out_f64 = u256_to_f64(hop_state.reserve_out);
                let fee_f64 = 1.0 - (hop_state.gamma_numer as f64 / hop_state.fee_denom as f64);
                let base_hop = HopState::new(r_in_f64, r_out_f64, fee_f64);
                hops.push((base_hop, hop_state));
            }

            resolved.push((*path_id, path_valid, hops));
        }
        drop(cache);

        // Update path storage under a single paths lock
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

    /// Remove a registered path.
    ///
    /// Returns True if the path was found and removed, False otherwise.
    #[pyo3(signature = (path_id))]
    fn remove_path(&self, path_id: u64) -> bool {
        self.paths.lock().remove(&path_id).is_some()
    }

    /// Solve multiple pre-registered paths by their path IDs in a single
    /// Python → Rust round-trip.
    ///
    /// This is the fastest solve path: paths were pre-resolved at
    /// registration time, so no pool lookups, float conversions, or
    /// lock acquisitions are needed on the solve hot path. The GIL is
    /// released once for the entire batch.
    ///
    /// Parameters
    /// ----------
    /// path_ids : list of int
    ///     Path IDs returned by `register_path()`.
    /// max_input : float or None
    ///     Optional maximum input constraint (applied to all paths).
    ///
    /// Returns
    /// -------
    /// list of RustArbResult
    ///     One result per path_id. Paths that are not registered or
    ///     have incomplete hops are returned as not_supported.
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

        // Phase 1 (GIL-held): Look up pre-resolved hop states by path ID
        // Clone the data so we can release the lock before py.detach()
        let paths_lock = self.paths.lock();

        let resolved: Vec<Option<ResolvedHops>> = path_ids
            .iter()
            .map(|id| {
                paths_lock
                    .get(id)
                    .map(|rp| rp.hops.clone())
            })
            .collect();

        drop(paths_lock);

        // Phase 2 (GIL-released): Solve all paths in one batch
        // No pool lookups, no float conversions, no lock acquisitions needed
        let results: Vec<PyArbResult> = py.detach(|| {
            resolved
                .iter()
                .map(|opt_hops| match opt_hops {
                    None => not_supported_result(),
                    Some(hops) if hops.len() < 2 => not_supported_result(),
                    Some(hops) => {
                        let base_hops: Vec<HopState> = hops.iter().map(|(b, _)| *b).collect();
                        let int_hops: Vec<IntHopState> = hops.iter().map(|(_, i)| i.clone()).collect();
                        solve_mobius(&base_hops, &int_hops, true, max_input)
                    }
                })
                .collect()
        });

        // Phase 3 (GIL-held): Build Python list of results
        let py_list = PyList::empty(py);
        for result in results {
            py_list.append(result)?;
        }

        Ok(py_list.unbind())
    }

    /// Solve multiple pre-registered paths, returning only integer results.
    ///
    /// This is the **minimum-overhead** solve path. Returns a flat list of
    /// Python integers: `[input0, profit0, input1, profit1, ...]`.
    ///
    /// For paths that are not registered, not supported, or not profitable,
    /// both values are 0.
    ///
    /// Optimizations over `solve_registered`:
    /// - Calls `mobius_solve_with_refinement` directly, skipping PyArbResult
    /// - For u64-fit values, returns native Python ints (~20ns) instead of
    ///   calling `int.from_bytes()` (~160ns)
    /// - Flat int list instead of tuple list (avoids PyTuple allocation)
    ///
    /// Parameters
    /// ----------
    /// path_ids : list of int
    ///     Path IDs returned by `register_path()`.
    /// max_input : float or None
    ///     Optional maximum input constraint (applied to all paths).
    ///
    /// Returns
    /// -------
    /// list of int
    ///     Flat list: `[optimal_input_0, profit_0, optimal_input_1, profit_1, ...]`
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

        // Phase 1 (GIL-held): Look up pre-resolved hop states
        let paths_lock = self.paths.lock();
        let resolved: Vec<Option<ResolvedHops>> = path_ids
            .iter()
            .map(|id| paths_lock.get(id).map(|rp| rp.hops.clone()))
            .collect();
        drop(paths_lock);

        // Phase 2 (GIL-released): Solve all paths, calling solver directly
        let int_results: Vec<Option<(U256, U256)>> = py.detach(|| {
            resolved
                .iter()
                .map(|opt_hops| match opt_hops {
                    None => None,
                    Some(hops) if hops.len() < 2 => None,
                    Some(hops) => {
                        let base_hops: Vec<HopState> = hops.iter().map(|(b, _)| *b).collect();
                        let int_hops: Vec<IntHopState> = hops.iter().map(|(_, i)| i.clone()).collect();
                        let result = mobius_solve_with_refinement(&base_hops, &int_hops, true, max_input);
                        if !result.success {
                            None
                        } else if let (Some(opt_input), Some(profit)) =
                            (result.optimal_input_int, result.profit_int)
                        {
                            Some((opt_input, profit))
                        } else {
                            None
                        }
                    }
                })
                .collect()
        });

        // Phase 3 (GIL-held): Convert to Python ints with fast path for u64 values
        // For values that fit in u64, use PyLong::new (C call, ~20ns)
        // For larger values, fall back to int.from_bytes (~160ns)
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

#[allow(clippy::missing_errors_doc)]
pub fn add_mobius_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyHopState>()?;
    m.add_class::<PyV3TickRangeHop>()?;
    m.add_class::<PyV3TickRangeSequence>()?;
    m.add_class::<PyTickRangeCrossing>()?;
    m.add_class::<PyArbResult>()?;
    m.add_class::<PyArbSolver>()?;
    m.add_class::<PyPoolCache>()?;

    // Integer Möbius solver
    m.add_class::<PyIntHopState>()?;
    m.add_class::<PyIntMobiusResult>()?;
    m.add_function(wrap_pyfunction!(py_int_mobius_solve, m)?)?;
    m.add_function(wrap_pyfunction!(py_int_simulate_path, m)?)?;
    m.add_function(wrap_pyfunction!(py_mobius_refine_int, m)?)?;

    Ok(())
}

// ==========================================================================
// Integer Möbius solver (EVM-exact)
// ==========================================================================

use crate::optimizers::mobius_int::{int_mobius_solve, int_simulate_path};
use pyo3::types::PyAny;

/// Integer hop state for EVM-exact arbitrage optimization.
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

    #[getter]
    fn gamma_numer(&self) -> u64 {
        self.inner.gamma_numer
    }

    /// Fee numerator (the actual fee taken, not the retained fraction).
    /// Computed as fee_denom - gamma_numer.
    #[getter]
    fn fee_numer(&self) -> u64 {
        self.inner.fee_denom - self.inner.gamma_numer
    }

    #[getter]
    fn fee_denom(&self) -> u64 {
        self.inner.fee_denom
    }

    fn __repr__(&self) -> String {
        format!(
            "RustIntHopState(reserve_in={:?}, reserve_out={:?}, gamma={}/{})",
            self.inner.reserve_in,
            self.inner.reserve_out,
            self.inner.gamma_numer,
            self.inner.fee_denom
        )
    }
}

/// Result from integer Möbius solver (EVM-exact).
#[pyclass(name = "RustIntMobiusResult")]
pub struct PyIntMobiusResult {
    pub optimal_input: U256,
    pub profit: U256,
    pub success: bool,
    pub iterations: u32,
}

#[pymethods]
impl PyIntMobiusResult {
    #[getter]
    fn optimal_input<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        PyU256(self.optimal_input).into_pyobject(py)
    }

    #[getter]
    fn profit<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        PyU256(self.profit).into_pyobject(py)
    }

    #[getter]
    fn success(&self) -> bool {
        self.success
    }

    #[getter]
    fn iterations(&self) -> u32 {
        self.iterations
    }

    fn __repr__(&self) -> String {
        format!(
            "RustIntMobiusResult(optimal_input={:?}, profit={:?}, success={}, iterations={})",
            self.optimal_input, self.profit, self.success, self.iterations
        )
    }
}

/// Solve for EVM-exact optimal arbitrage input using integer Möbius coefficients.
///
/// Parameters
/// ----------
/// hops : list of RustIntHopState
///     Pool states with integer reserves and fee parameters.
///
/// Returns
/// -------
/// RustIntMobiusResult
#[pyfunction]
#[pyo3(signature = (hops))]
fn py_int_mobius_solve(py: Python<'_>, hops: &Bound<'_, PyList>) -> PyResult<PyIntMobiusResult> {
    let mut int_hops = Vec::new();
    for item in hops.iter() {
        let py_hop = item.extract::<PyRef<PyIntHopState>>()?;
        int_hops.push(py_hop.inner.clone());
    }

    let result = py
        .detach(|| int_mobius_solve(&int_hops))
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("{e}")))?;

    Ok(PyIntMobiusResult {
        optimal_input: result.optimal_input,
        profit: result.profit,
        success: result.success,
        iterations: result.iterations,
    })
}

/// Simulate a swap through all hops using EVM-exact integer arithmetic.
///
/// Parameters
/// ----------
/// x : int
///     Input amount.
/// hops : list of RustIntHopState
///     Pool states.
///
/// Returns
/// -------
/// int
#[pyfunction]
#[pyo3(signature = (x, hops))]
fn py_int_simulate_path<'py>(
    py: Python<'py>,
    x: &Bound<'_, PyAny>,
    hops: &Bound<'_, PyList>,
) -> PyResult<Bound<'py, PyAny>> {
    let x_u256 = extract_python_u256(x)?;
    let mut int_hops = Vec::new();
    for item in hops.iter() {
        let py_hop = item.extract::<PyRef<PyIntHopState>>()?;
        int_hops.push(py_hop.inner.clone());
    }

    let result = py.detach(|| int_simulate_path(x_u256, &int_hops));
    PyU256(result.final_output).into_pyobject(py)
}

/// Integer refinement around a float optimum using EVM-exact U256 arithmetic.
///
/// This is the core of the "move integer refinement to Rust" optimization.
/// Instead of returning a float result to Python and doing 3-5 Python
/// `_simulate_path` calls, we do the ±N search entirely in Rust with
/// U256 integer arithmetic.
///
/// Parameters
/// ----------
/// x_approx : float
///     Approximate optimal input from the float Möbius solver.
/// hops : list of RustIntHopState
///     Pool states with integer reserves and fee parameters.
/// max_input : float or None
///     Maximum input constraint (None = unconstrained).
///
/// Returns
/// -------
/// RustIntMobiusResult
#[pyfunction]
#[pyo3(signature = (x_approx, hops, max_input=None))]
fn py_mobius_refine_int(
    py: Python<'_>,
    x_approx: f64,
    hops: &Bound<'_, PyList>,
    max_input: Option<f64>,
) -> PyResult<PyIntMobiusResult> {
    let mut int_hops = Vec::new();
    for item in hops.iter() {
        let py_hop = item.extract::<PyRef<PyIntHopState>>()?;
        int_hops.push(py_hop.inner.clone());
    }

    let result = py.detach(|| mobius_refine_int(x_approx, &int_hops, max_input));

    Ok(PyIntMobiusResult {
        optimal_input: result.optimal_input,
        profit: result.profit,
        success: result.success,
        iterations: result.iterations,
    })
}
