//! PyO3 Python bindings for the Möbius transformation optimizer.

#![allow(non_snake_case)]
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

use crate::optimizers::mobius::{
    compute_mobius_coefficients, mobius_solve, simulate_path, HopState, MobiusCoefficients,
    MobiusError,
};
use crate::optimizers::mobius_batch::{
    mobius_batch_solve, mobius_batch_solve_vectorized,
};
use crate::optimizers::mobius_v3::{
    estimate_v3_final_sqrt_price, solve_piecewise, solve_v3_candidates,
    solve_v3_tick_range_sequence,
    TickRangeCrossing, V3TickRangeHop, V3TickRangeSequence,
};
use crate::optimizers::mobius_v3_v3::solve_v3_v3;
use crate::optimizers::mobius_int::{IntHopState, mobius_refine_int, mobius_solve_with_refinement, u256_to_f64};
use crate::alloy_py::{extract_python_u256, PyU256};
use alloy::primitives::U256;

use pyo3::prelude::*;
use pyo3::types::PyList;

impl From<MobiusError> for PyErr {
    fn from(err: MobiusError) -> Self {
        pyo3::exceptions::PyValueError::new_err(format!("Möbius error: {err}"))
    }
}

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

/// Möbius transformation coefficients.
#[pyclass(name = "RustMobiusCoefficients")]
pub struct PyMobiusCoefficients {
    pub inner: MobiusCoefficients,
}

#[pymethods]
impl PyMobiusCoefficients {
    #[getter]
    #[allow(non_snake_case)]
    fn coeff_K(&self) -> f64 {
        self.inner.K
    }

    #[getter]
    #[allow(non_snake_case)]
    fn coeff_M(&self) -> f64 {
        self.inner.M
    }

    #[getter]
    #[allow(non_snake_case)]
    fn coeff_N(&self) -> f64 {
        self.inner.N
    }

    #[getter]
    fn is_profitable(&self) -> bool {
        self.inner.is_profitable
    }

    /// Compute path output for input x.
    #[pyo3(signature = (x))]
    fn path_output(&self, x: f64) -> f64 {
        self.inner.path_output(x)
    }

    /// Compute the exact optimal input.
    fn optimal_input(&self) -> f64 {
        self.inner.optimal_input()
    }

    /// Compute profit for input x.
    #[pyo3(signature = (x))]
    fn profit_at(&self, x: f64) -> f64 {
        self.inner.profit_at(x)
    }

    fn __repr__(&self) -> String {
        format!(
            "RustMobiusCoefficients(K={}, M={}, N={}, is_profitable={})",
            self.inner.K, self.inner.M, self.inner.N, self.inner.is_profitable
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
                "Invalid V3 tick range sequence"
            )),
        }
    }

    /// Compute crossing data to reach range k.
    #[pyo3(signature = (k))]
    fn compute_crossing(&self, k: usize) -> PyResult<PyTickRangeCrossing> {
        match self.inner.compute_crossing(k) {
            Ok(crossing) => Ok(PyTickRangeCrossing { inner: crossing }),
            Err(_) => Err(pyo3::exceptions::PyValueError::new_err(
                "Invalid range index"
            )),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "RustV3TickRangeSequence(ranges={})",
            self.inner.ranges.len()
        )
    }
}

/// Result from a Möbius solve.
#[pyclass(name = "RustMobiusResult")]
pub struct PyMobiusResult {
    pub optimal_input: f64,
    pub profit: f64,
    pub iterations: u32,
    pub success: bool,
}

#[pymethods]
impl PyMobiusResult {
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

    fn __repr__(&self) -> String {
        format!(
            "RustMobiusResult(optimal_input={}, profit={}, iterations={}, success={})",
            self.optimal_input, self.profit, self.iterations, self.success
        )
    }
}

/// High-performance Möbius transformation optimizer implemented in Rust.
///
/// Every constant product swap y = (γ·s·x)/(r + γ·x) is a Möbius
/// transformation. An n-hop path composes into l(x) = K·x / (M + N·x),
/// with closed-form optimal input x_opt = (√(K·M) - M) / N.
///
/// Zero iterations, exact solution, O(n) forward pass.
#[pyclass(name = "RustMobiusOptimizer")]
pub struct PyMobiusOptimizer;

#[pymethods]
impl PyMobiusOptimizer {
    #[new]
    fn new() -> Self {
        Self
    }

    /// Compute Möbius coefficients K, M, N for an n-hop path.
    ///
    /// Parameters
    /// ----------
    /// hops : list of RustHopState
    ///     Pool states along the arbitrage path.
    ///
    /// Returns
    /// -------
    /// RustMobiusCoefficients
    #[pyo3(signature = (hops))]
    fn compute_coefficients(&self, py: Python<'_>, hops: &Bound<'_, PyList>) -> PyResult<PyMobiusCoefficients> {
        let hop_states = extract_hops(hops)?;
        let coeffs = py.detach(|| compute_mobius_coefficients(&hop_states))?;
        Ok(PyMobiusCoefficients { inner: coeffs })
    }

    /// Simulate a swap through all hops.
    ///
    /// Parameters
    /// ----------
    /// x : float
    ///     Input amount.
    /// hops : list of RustHopState
    ///     Pool states along the path.
    ///
    /// Returns
    /// -------
    /// float
    #[pyo3(signature = (x, hops))]
    fn simulate_path(&self, py: Python<'_>, x: f64, hops: &Bound<'_, PyList>) -> PyResult<f64> {
        let hop_states = extract_hops(hops)?;
        Ok(py.detach(|| simulate_path(x, &hop_states)))
    }

    /// Solve for optimal arbitrage input (closed-form, zero iterations).
    ///
    /// Parameters
    /// ----------
    /// hops : list of RustHopState
    ///     Pool states along the arbitrage path.
    /// max_input : float or None
    ///     Optional upper bound on input amount.
    ///
    /// Returns
    /// -------
    /// RustMobiusResult
    #[pyo3(signature = (hops, max_input=None))]
    fn solve(
        &self,
        py: Python<'_>,
        hops: &Bound<'_, PyList>,
        max_input: Option<f64>,
    ) -> PyResult<PyMobiusResult> {
        let hop_states = extract_hops(hops)?;
        let (x_opt, profit, iters) = py.detach(|| mobius_solve(&hop_states, max_input));
        Ok(PyMobiusResult {
            optimal_input: x_opt,
            profit,
            iterations: iters,
            success: x_opt > 0.0 && profit > 0.0,
        })
    }

    /// Solve with multiple candidate V3 tick ranges.
    ///
    /// Parameters
    /// ----------
    /// base_hops : list of RustHopState
    ///     V2 (or other) hops excluding the V3 hop.
    /// v3_hop_index : int
    ///     Index in the full path where the V3 hop sits.
    /// v3_candidates : list of RustV3TickRangeHop
    ///     Candidate V3 tick ranges to check.
    /// max_input : float or None
    ///     Maximum input constraint.
    ///
    /// Returns
    /// -------
    /// RustMobiusResult
    #[pyo3(signature = (base_hops, v3_hop_index, v3_candidates, max_input=None))]
    fn solve_v3_candidates(
        &self,
        py: Python<'_>,
        base_hops: &Bound<'_, PyList>,
        v3_hop_index: usize,
        v3_candidates: &Bound<'_, PyList>,
        max_input: Option<f64>,
    ) -> PyResult<PyMobiusResult> {
        let hop_states = extract_hops(base_hops)?;
        let candidates = extract_v3_candidates(v3_candidates)?;
        let (x_opt, profit, iters) = py.detach(|| solve_v3_candidates(&hop_states, v3_hop_index, &candidates, max_input));
        Ok(PyMobiusResult {
            optimal_input: x_opt,
            profit,
            iterations: iters,
            success: x_opt > 0.0 && profit > 0.0,
        })
    }

    /// Estimate final sqrt price after a V3 swap.
    ///
    /// Parameters
    /// ----------
    /// amount_in : float
    ///     Input amount to the V3 pool.
    /// v3_hop : RustV3TickRangeHop
    ///     V3 tick range hop data.
    ///
    /// Returns
    /// -------
    /// float
    #[pyo3(signature = (amount_in, v3_hop))]
    fn estimate_v3_final_sqrt_price(
        &self,
        py: Python<'_>,
        amount_in: f64,
        v3_hop: &PyV3TickRangeHop,
    ) -> f64 {
        let inner = v3_hop.inner.clone();
        py.detach(|| estimate_v3_final_sqrt_price(amount_in, &inner))
    }

    /// Solve arbitrage with piecewise-Möbius for V3 tick crossings.
    ///
    /// For each candidate ending range (via TickRangeCrossing), the V3 swap
    /// is decomposed into fixed crossing output from crossed ranges plus
    /// variable Möbius output from the ending range.
    ///
    /// Parameters
    /// ----------
    /// hops : list of RustHopState
    ///     Full path hops with V3 hop at v3_hop_index.
    /// v3_hop_index : int
    ///     Index of the V3 hop in the path.
    /// crossings : list of TickRangeCrossing
    ///     Candidate crossing data, ordered by likelihood.
    /// max_input : float or None
    ///     Maximum input constraint.
    ///
    /// Returns
    /// -------
    /// RustMobiusResult
    #[pyo3(signature = (hops, v3_hop_index, crossings, max_input=None))]
    fn solve_piecewise(
        &self,
        py: Python<'_>,
        hops: &Bound<'_, PyList>,
        v3_hop_index: usize,
        crossings: &Bound<'_, PyList>,
        max_input: Option<f64>,
    ) -> PyResult<PyMobiusResult> {
        let hop_states = extract_hops(hops)?;
        let crossing_data = extract_tick_range_crossings(crossings)?;
        let (x_opt, profit, iters) = py.detach(|| solve_piecewise(&hop_states, v3_hop_index, &crossing_data, max_input));
        Ok(PyMobiusResult {
            optimal_input: x_opt,
            profit,
            iterations: iters,
            success: x_opt > 0.0 && profit > 0.0,
        })
    }

    /// Solve arbitrage with full V3 tick range sequence handling.
    ///
    /// This is the high-level entry point for multi-range V3 arbitrage.
    /// It computes crossings for each candidate range and returns the best result.
    ///
    /// Parameters
    /// ----------
    /// hops : list of RustHopState
    ///     Full path hops with V3 hop at v3_hop_index.
    /// v3_hop_index : int
    ///     Index of the V3 hop in the path.
    /// sequence : RustV3TickRangeSequence
    ///     V3 tick range sequence (current + adjacent ranges).
    /// max_candidates : int
    ///     Maximum number of candidate ranges to check.
    /// max_input : float or None
    ///     Maximum input constraint.
    ///
    /// Returns
    /// -------
    /// RustMobiusResult
    #[pyo3(signature = (hops, v3_hop_index, sequence, max_candidates, max_input=None))]
    fn solve_v3_sequence(
        &self,
        py: Python<'_>,
        hops: &Bound<'_, PyList>,
        v3_hop_index: usize,
        sequence: &PyV3TickRangeSequence,
        max_candidates: usize,
        max_input: Option<f64>,
    ) -> PyResult<PyMobiusResult> {
        let hop_states = extract_hops(hops)?;
        let seq = sequence.inner.clone();
        let (x_opt, profit, iters) = py.detach(|| solve_v3_tick_range_sequence(
            &hop_states,
            v3_hop_index,
            &seq,
            max_candidates,
            max_input,
        ));
        Ok(PyMobiusResult {
            optimal_input: x_opt,
            profit,
            iterations: iters,
            success: x_opt > 0.0 && profit > 0.0,
        })
    }

    /// Solve V3-V3 arbitrage (two V3 hops, both potentially crossing ticks).
    ///
    /// Uses enumeration over candidate ending ranges with golden section search.
    ///
    /// Parameters
    /// ----------
    /// sequence1 : RustV3TickRangeSequence
    ///     Tick range sequence for first V3 hop.
    /// sequence2 : RustV3TickRangeSequence
    ///     Tick range sequence for second V3 hop.
    /// max_input : float, optional
    ///     Maximum input constraint.
    /// max_candidates : int, optional
    ///     Maximum number of candidate ranges to check per hop (default: 10).
    ///
    /// Returns
    /// -------
    /// RustMobiusResult
    #[pyo3(signature = (sequence1, sequence2, max_input=None, max_candidates=10))]
    fn solve_v3_v3(
        &self,
        py: Python<'_>,
        sequence1: &PyV3TickRangeSequence,
        sequence2: &PyV3TickRangeSequence,
        max_input: Option<f64>,
        max_candidates: usize,
    ) -> PyResult<PyMobiusResult> {
        let seq1 = sequence1.inner.clone();
        let seq2 = sequence2.inner.clone();
        let (x_opt, profit, iters) = py.detach(|| solve_v3_v3(
            &seq1,
            &seq2,
            max_input,
            max_candidates,
        ));
        Ok(PyMobiusResult {
            optimal_input: x_opt,
            profit,
            iterations: iters,
            success: x_opt > 0.0 && profit > 0.0,
        })
    }

    /// Solve a batch of paths with the same hop count.
    ///
    /// Parameters
    /// ----------
    /// hops_array : list of float
    ///     Flat array [reserve_in, reserve_out, fee] per hop per path.
    /// num_hops : int
    ///     Number of hops per path.
    /// max_inputs : list of float
    ///     Per-path max input constraints (use float('inf') for unconstrained).
    ///
    /// Returns
    /// -------
    /// dict with 'optimal_input', 'profit', 'is_profitable' lists.
    #[pyo3(signature = (hops_array, num_hops, max_inputs))]
    fn solve_batch(
        &self,
        py: Python<'_>,
        hops_array: &Bound<'_, PyList>,
        num_hops: usize,
        max_inputs: &Bound<'_, PyList>,
    ) -> PyResult<Py<PyAny>> {
        let hops_vec: Vec<f64> = hops_array.extract()?;
        let max_vec: Vec<f64> = max_inputs.extract()?;

        let result = py.detach(|| mobius_batch_solve(&hops_vec, num_hops, &max_vec))
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;

        let dict = pyo3::types::PyDict::new(py);
        dict.set_item("optimal_input", result.optimal_input.clone())?;
        dict.set_item("profit", result.profit.clone())?;
        dict.set_item("is_profitable", result.is_profitable)?;
        Ok(dict.into())
    }

    /// Solve a batch using vectorized coefficient computation.
    ///
    /// Parameters
    /// ----------
    /// reserves_in : list of float
    ///     Flat array of input reserves (num_paths * num_hops).
    /// reserves_out : list of float
    ///     Flat array of output reserves (num_paths * num_hops).
    /// fees : list of float
    ///     Flat array of fee fractions (num_paths * num_hops).
    /// num_hops : int
    ///     Number of hops per path.
    /// max_inputs : list of float
    ///     Per-path max input constraints.
    ///
    /// Returns
    /// -------
    /// dict with 'optimal_input', 'profit', 'is_profitable' lists.
    #[pyo3(signature = (reserves_in, reserves_out, fees, num_hops, max_inputs))]
    fn solve_batch_vectorized(
        &self,
        py: Python<'_>,
        reserves_in: &Bound<'_, PyList>,
        reserves_out: &Bound<'_, PyList>,
        fees: &Bound<'_, PyList>,
        num_hops: usize,
        max_inputs: &Bound<'_, PyList>,
    ) -> PyResult<Py<PyAny>> {
        let r_in: Vec<f64> = reserves_in.extract()?;
        let r_out: Vec<f64> = reserves_out.extract()?;
        let fee_vec: Vec<f64> = fees.extract()?;
        let max_vec: Vec<f64> = max_inputs.extract()?;

        let num_paths = max_vec.len();
        let result = py.detach(|| mobius_batch_solve_vectorized(
            num_paths,
            num_hops,
            &r_in,
            &r_out,
            &fee_vec,
            &max_vec,
        ))
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;

        let dict = pyo3::types::PyDict::new(py);
        dict.set_item("optimal_input", result.optimal_input.clone())?;
        dict.set_item("profit", result.profit.clone())?;
        dict.set_item("is_profitable", result.is_profitable)?;
        Ok(dict.into())
    }
}

/// Extract a list of HopState from a Python list.
fn extract_hops(py_list: &Bound<'_, PyList>) -> PyResult<Vec<HopState>> {
    let mut hops = Vec::new();
    for item in py_list.iter() {
        if let Ok(py_hop) = item.extract::<PyRef<PyHopState>>() {
            hops.push(py_hop.inner.clone());
        } else if let Ok((r_in, r_out, fee)) = item.extract::<(f64, f64, f64)>() {
            hops.push(HopState::new(r_in, r_out, fee));
        } else {
            return Err(pyo3::exceptions::PyTypeError::new_err(
                "Each hop must be a RustHopState or a (reserve_in, reserve_out, fee) tuple",
            ));
        }
    }
    Ok(hops)
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
}

/// Extract a list of V3TickRangeHop from a Python list.
fn extract_v3_candidates(py_list: &Bound<'_, PyList>) -> PyResult<Vec<V3TickRangeHop>> {
    let mut candidates = Vec::new();
    for item in py_list.iter() {
        let py_v3 = item.extract::<PyRef<PyV3TickRangeHop>>()?;
        candidates.push(py_v3.inner.clone());
    }
    Ok(candidates)
}

/// Extract a list of TickRangeCrossing from a Python list.
fn extract_tick_range_crossings(py_list: &Bound<'_, PyList>) -> PyResult<Vec<TickRangeCrossing>> {
    let mut crossings = Vec::new();
    for item in py_list.iter() {
        let py_crossing = item.extract::<PyRef<PyTickRangeCrossing>>()?;
        crossings.push(py_crossing.inner.clone());
    }
    Ok(crossings)
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
fn parse_hops(
    hops: &Bound<'_, PyList>,
) -> PyResult<(Vec<HopState>, Vec<IntHopState>, bool, bool)> {
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
            base_hops.push(py_hop.inner.clone());
            all_int = false;
        }
        else {
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
            let (x_opt, profit, iters) = py.detach(|| solve_v3_v3(
                &seq0, &seq1, max_input, max_candidates,
            ));
            return Ok(PyArbResult {
                optimal_input: x_opt, profit, iterations: iters,
                success: x_opt > 0.0 && profit > 0.0, method: SolveMethod::V3V3 as u8,
                supported: true, optimal_input_int: None, profit_int: None,
            });
        } else if v3_seqs.len() == 1 {
            let v3_idx = v3_seqs[0].0;
            let seq = v3_seqs[0].1.clone();
            let (x_opt, profit, iters) = py.detach(|| solve_v3_tick_range_sequence(
                &base_hops, v3_idx, &seq, max_candidates, max_input,
            ));
            return Ok(PyArbResult {
                optimal_input: x_opt, profit, iterations: iters,
                success: x_opt > 0.0 && profit > 0.0, method: SolveMethod::PiecewiseMobius as u8,
                supported: true, optimal_input_int: None, profit_int: None,
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
            return Err(pyo3::exceptions::PyValueError::new_err(
                format!("int_hops_flat length ({n}) must be a multiple of 4"),
            ));
        }
        let num_hops = n / 4;
        if num_hops < 2 {
            return Err(pyo3::exceptions::PyValueError::new_err(
                format!("Need at least 2 hops, got {num_hops}"),
            ));
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
}

// ==========================================================================
// RustPoolCache — direct pool state to Rust solver
// ==========================================================================

use std::collections::HashMap;

/// Cached pool state for fast arbitrage solving by pool ID.
///
/// Pool states are registered once (at pool update time) and then
/// solved by ID reference, eliminating all Python object construction
/// and per-item extraction overhead on the solve path.
///
/// The solve path becomes: `cache.solve([pool_id_0, pool_id_1])` —
/// just two Python integers passed to Rust.
#[pyclass(name = "RustPoolCache")]
pub struct PyPoolCache {
    pools: HashMap<u64, IntHopState>,
}

#[pymethods]
impl PyPoolCache {
    #[new]
    fn new() -> Self {
        Self {
            pools: HashMap::new(),
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
        &mut self,
        pool_id: u64,
        reserve_in: &Bound<'_, PyAny>,
        reserve_out: &Bound<'_, PyAny>,
        gamma_numer: u64,
        fee_denom: u64,
    ) -> PyResult<()> {
        let r_in = extract_python_u256(reserve_in)?;
        let r_out = extract_python_u256(reserve_out)?;

        if gamma_numer >= fee_denom {
            return Err(pyo3::exceptions::PyValueError::new_err(
                format!(
                    "gamma_numer ({gamma_numer}) must be less than fee_denom ({fee_denom})"
                ),
            ));
        }

        self.pools.insert(pool_id, IntHopState::new(r_in, r_out, gamma_numer, fee_denom));
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
    fn remove(&mut self, pool_id: u64) -> bool {
        self.pools.remove(&pool_id).is_some()
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
            return Err(pyo3::exceptions::PyValueError::new_err(
                format!("Need at least 2 pools in path, got {}", pool_ids.len()),
            ));
        }

        let mut int_hops: Vec<IntHopState> = Vec::with_capacity(pool_ids.len());
        let mut base_hops: Vec<HopState> = Vec::with_capacity(pool_ids.len());

        for &pool_id in &pool_ids {
            let hop_state = self.pools.get(&pool_id).ok_or_else(|| {
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

    /// Check if a pool ID is in the cache.
    #[pyo3(signature = (pool_id))]
    fn contains(&self, pool_id: u64) -> bool {
        self.pools.contains_key(&pool_id)
    }

    /// Number of pools in the cache.
    fn __len__(&self) -> usize {
        self.pools.len()
    }

    /// Check if the cache is empty.
    #[must_use]
    fn __bool__(&self) -> bool {
        !self.pools.is_empty()
    }

    fn __repr__(&self) -> String {
        format!("RustPoolCache(pools={})", self.pools.len())
    }
}

pub fn add_mobius_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let submod = pyo3::types::PyModule::new(m.py(), "mobius")?;

    submod.add_class::<PyHopState>()?;
    submod.add_class::<PyMobiusCoefficients>()?;
    submod.add_class::<PyV3TickRangeHop>()?;
    submod.add_class::<PyV3TickRangeSequence>()?;
    submod.add_class::<PyTickRangeCrossing>()?;
    submod.add_class::<PyMobiusResult>()?;
    submod.add_class::<PyMobiusOptimizer>()?;
    submod.add_class::<PyArbResult>()?;
    submod.add_class::<PyArbSolver>()?;
    submod.add_class::<PyPoolCache>()?;

    // Standalone functions
    submod.add_function(wrap_pyfunction!(py_compute_mobius_coefficients, &submod)?)?;
    submod.add_function(wrap_pyfunction!(py_mobius_solve, &submod)?)?;
    submod.add_function(wrap_pyfunction!(py_simulate_path, &submod)?)?;
    submod.add_function(wrap_pyfunction!(py_estimate_v3_final_sqrt_price, &submod)?)?;

    // Integer Möbius solver
    submod.add_class::<PyIntHopState>()?;
    submod.add_class::<PyIntMobiusResult>()?;
    submod.add_function(wrap_pyfunction!(py_int_mobius_solve, &submod)?)?;
    submod.add_function(wrap_pyfunction!(py_int_simulate_path, &submod)?)?;
    submod.add_function(wrap_pyfunction!(py_mobius_refine_int, &submod)?)?;

    m.add_submodule(&submod)?;
    Ok(())
}

/// Compute Möbius coefficients for an n-hop path.
#[pyfunction]
#[pyo3(signature = (hops))]
fn py_compute_mobius_coefficients(py: Python<'_>, hops: &Bound<'_, PyList>) -> PyResult<PyMobiusCoefficients> {
    let hop_states = extract_hops(hops)?;
    let coeffs = py.detach(|| compute_mobius_coefficients(&hop_states))?;
    Ok(PyMobiusCoefficients { inner: coeffs })
}

/// Solve for optimal arbitrage input.
#[pyfunction]
#[pyo3(signature = (hops, max_input=None))]
fn py_mobius_solve(py: Python<'_>, hops: &Bound<'_, PyList>, max_input: Option<f64>) -> PyResult<PyMobiusResult> {
    let hop_states = extract_hops(hops)?;
    let (x_opt, profit, iters) = py.detach(|| mobius_solve(&hop_states, max_input));
    Ok(PyMobiusResult {
        optimal_input: x_opt,
        profit,
        iterations: iters,
        success: x_opt > 0.0 && profit > 0.0,
    })
}

/// Simulate a swap through all hops.
#[pyfunction]
#[pyo3(signature = (x, hops))]
fn py_simulate_path(py: Python<'_>, x: f64, hops: &Bound<'_, PyList>) -> PyResult<f64> {
    let hop_states = extract_hops(hops)?;
    Ok(py.detach(|| simulate_path(x, &hop_states)))
}

/// Estimate final sqrt price after a V3 swap.
#[pyfunction]
#[pyo3(signature = (amount_in, v3_hop))]
fn py_estimate_v3_final_sqrt_price(amount_in: f64, v3_hop: &PyV3TickRangeHop) -> f64 {
    estimate_v3_final_sqrt_price(amount_in, &v3_hop.inner)
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
    fn new(reserve_in: &Bound<'_, PyAny>, reserve_out: &Bound<'_, PyAny>, gamma_numer: u64, fee_denom: u64) -> PyResult<Self> {
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

    #[getter]
    fn fee_denom(&self) -> u64 {
        self.inner.fee_denom
    }

    fn __repr__(&self) -> String {
        format!(
            "RustIntHopState(reserve_in={:?}, reserve_out={:?}, gamma={}/{})",
            self.inner.reserve_in,
            self.inner.reserve_out,
            self.inner.gamma_numer, self.inner.fee_denom
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

    let result = py.detach(|| int_mobius_solve(&int_hops)).map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!("{e}"))
    })?;

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
fn py_int_simulate_path<'py>(py: Python<'py>, x: &Bound<'_, PyAny>, hops: &Bound<'_, PyList>) -> PyResult<Bound<'py, PyAny>> {
    let x_u256 = extract_python_u256(x)?;
    let mut int_hops = Vec::new();
    for item in hops.iter() {
        let py_hop = item.extract::<PyRef<PyIntHopState>>()?;
        int_hops.push(py_hop.inner.clone());
    }

    let output = py.detach(|| int_simulate_path(x_u256, &int_hops));
    PyU256(output).into_pyobject(py)
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
