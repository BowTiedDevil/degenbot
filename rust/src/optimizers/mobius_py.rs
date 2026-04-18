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

use pyo3::prelude::*;
use pyo3::types::{PyInt, PyList};

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
    fn compute_coefficients(&self, hops: &Bound<'_, PyList>) -> PyResult<PyMobiusCoefficients> {
        let hop_states = extract_hops(hops)?;
        let coeffs = compute_mobius_coefficients(&hop_states)?;
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
    fn simulate_path(&self, x: f64, hops: &Bound<'_, PyList>) -> PyResult<f64> {
        let hop_states = extract_hops(hops)?;
        Ok(simulate_path(x, &hop_states))
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
        hops: &Bound<'_, PyList>,
        max_input: Option<f64>,
    ) -> PyResult<PyMobiusResult> {
        let hop_states = extract_hops(hops)?;
        let (x_opt, profit, iters) = mobius_solve(&hop_states, max_input);
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
        base_hops: &Bound<'_, PyList>,
        v3_hop_index: usize,
        v3_candidates: &Bound<'_, PyList>,
        max_input: Option<f64>,
    ) -> PyResult<PyMobiusResult> {
        let hop_states = extract_hops(base_hops)?;
        let candidates = extract_v3_candidates(v3_candidates)?;
        let (x_opt, profit, iters) = solve_v3_candidates(&hop_states, v3_hop_index, &candidates, max_input);
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
        amount_in: f64,
        v3_hop: &PyV3TickRangeHop,
    ) -> f64 {
        estimate_v3_final_sqrt_price(amount_in, &v3_hop.inner)
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
        hops: &Bound<'_, PyList>,
        v3_hop_index: usize,
        crossings: &Bound<'_, PyList>,
        max_input: Option<f64>,
    ) -> PyResult<PyMobiusResult> {
        let hop_states = extract_hops(hops)?;
        let crossing_data = extract_tick_range_crossings(crossings)?;
        let (x_opt, profit, iters) = solve_piecewise(&hop_states, v3_hop_index, &crossing_data, max_input);
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
        hops: &Bound<'_, PyList>,
        v3_hop_index: usize,
        sequence: &PyV3TickRangeSequence,
        max_candidates: usize,
        max_input: Option<f64>,
    ) -> PyResult<PyMobiusResult> {
        let hop_states = extract_hops(hops)?;
        let (x_opt, profit, iters) = solve_v3_tick_range_sequence(
            &hop_states,
            v3_hop_index,
            &sequence.inner,
            max_candidates,
            max_input,
        );
        Ok(PyMobiusResult {
            optimal_input: x_opt,
            profit,
            iterations: iters,
            success: x_opt > 0.0 && profit > 0.0,
        })
    }

    /// Solve V3-V3 arbitrage (two V3 hops, both potentially crossing ticks).
    ///
    /// Uses coordinate descent to optimize inputs for both hops.
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
        sequence1: &PyV3TickRangeSequence,
        sequence2: &PyV3TickRangeSequence,
        max_input: Option<f64>,
        max_candidates: usize,
    ) -> PyResult<PyMobiusResult> {
        let (x_opt, profit, iters) = solve_v3_v3(
            &sequence1.inner,
            &sequence2.inner,
            max_input,
            max_candidates,
        );
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
        hops_array: &Bound<'_, PyList>,
        num_hops: usize,
        max_inputs: &Bound<'_, PyList>,
    ) -> PyResult<Py<PyAny>> {
        let hops_vec: Vec<f64> = hops_array.extract()?;
        let max_vec: Vec<f64> = max_inputs.extract()?;

        let result = mobius_batch_solve(&hops_vec, num_hops, &max_vec);

        {
            let py = hops_array.py();
            let dict = pyo3::types::PyDict::new(py);
            dict.set_item("optimal_input", result.optimal_input.clone())?;
            dict.set_item("profit", result.profit.clone())?;
            dict.set_item("is_profitable", result.is_profitable)?;
            Ok(dict.into())
        }
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
        let result = mobius_batch_solve_vectorized(
            num_paths,
            num_hops,
            &r_in,
            &r_out,
            &fee_vec,
            &max_vec,
        );

        {
            let py = reserves_in.py();
            let dict = pyo3::types::PyDict::new(py);
            dict.set_item("optimal_input", result.optimal_input.clone())?;
            dict.set_item("profit", result.profit.clone())?;
            dict.set_item("is_profitable", result.is_profitable)?;
            Ok(dict.into())
        }
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

/// Add the Möbius optimizer module to the parent PyModule.
pub fn add_mobius_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let submod = pyo3::types::PyModule::new(m.py(), "mobius")?;

    submod.add_class::<PyHopState>()?;
    submod.add_class::<PyMobiusCoefficients>()?;
    submod.add_class::<PyV3TickRangeHop>()?;
    submod.add_class::<PyV3TickRangeSequence>()?;
    submod.add_class::<PyTickRangeCrossing>()?;
    submod.add_class::<PyMobiusResult>()?;
    submod.add_class::<PyMobiusOptimizer>()?;

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

    m.add_submodule(&submod)?;
    Ok(())
}

/// Compute Möbius coefficients for an n-hop path.
#[pyfunction]
#[pyo3(signature = (hops))]
fn py_compute_mobius_coefficients(hops: &Bound<'_, PyList>) -> PyResult<PyMobiusCoefficients> {
    let hop_states = extract_hops(hops)?;
    let coeffs = compute_mobius_coefficients(&hop_states)?;
    Ok(PyMobiusCoefficients { inner: coeffs })
}

/// Solve for optimal arbitrage input.
#[pyfunction]
#[pyo3(signature = (hops, max_input=None))]
fn py_mobius_solve(hops: &Bound<'_, PyList>, max_input: Option<f64>) -> PyResult<PyMobiusResult> {
    let hop_states = extract_hops(hops)?;
    let (x_opt, profit, iters) = mobius_solve(&hop_states, max_input);
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
fn py_simulate_path(x: f64, hops: &Bound<'_, PyList>) -> PyResult<f64> {
    let hop_states = extract_hops(hops)?;
    Ok(simulate_path(x, &hop_states))
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

use crate::optimizers::mobius_int::{IntHopState, int_mobius_solve, int_simulate_path};
use alloy::primitives::U256;
use num_bigint::BigUint;

/// Integer hop state for EVM-exact arbitrage optimization.
#[pyclass(name = "RustIntHopState", skip_from_py_object)]
#[derive(Clone)]
pub struct PyIntHopState {
    pub inner: IntHopState,
}

#[pymethods]
impl PyIntHopState {
    #[new]
    #[pyo3(signature = (reserve_in, reserve_out, fee_numer, fee_denom))]
    fn new(reserve_in: &Bound<'_, PyAny>, reserve_out: &Bound<'_, PyAny>, fee_numer: u64, fee_denom: u64) -> PyResult<Self> {
        let r_in = extract_u256(reserve_in)?;
        let r_out = extract_u256(reserve_out)?;
        Ok(Self {
            inner: IntHopState::new(r_in, r_out, fee_numer, fee_denom),
        })
    }

    #[getter]
    fn reserve_in(&self) -> BigUint {
        u256_to_biguint(self.inner.reserve_in)
    }

    #[getter]
    fn reserve_out(&self) -> BigUint {
        u256_to_biguint(self.inner.reserve_out)
    }

    #[getter]
    fn fee_numer(&self) -> u64 {
        self.inner.fee_numer
    }

    #[getter]
    fn fee_denom(&self) -> u64 {
        self.inner.fee_denom
    }

    fn __repr__(&self) -> String {
        format!(
            "RustIntHopState(reserve_in={}, reserve_out={}, fee={}/{})",
            u256_to_biguint(self.inner.reserve_in),
            u256_to_biguint(self.inner.reserve_out),
            self.inner.fee_numer, self.inner.fee_denom
        )
    }
}

/// Result from integer Möbius solver (EVM-exact).
#[pyclass(name = "RustIntMobiusResult")]
pub struct PyIntMobiusResult {
    pub optimal_input: BigUint,
    pub profit: BigUint,
    pub success: bool,
    pub iterations: u32,
}

#[pymethods]
impl PyIntMobiusResult {
    #[getter]
    fn optimal_input(&self) -> BigUint {
        self.optimal_input.clone()
    }

    #[getter]
    fn profit(&self) -> BigUint {
        self.profit.clone()
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
            "RustIntMobiusResult(optimal_input={}, profit={}, success={}, iterations={})",
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
fn py_int_mobius_solve(hops: &Bound<'_, PyList>) -> PyResult<PyIntMobiusResult> {
    let mut int_hops = Vec::new();
    for item in hops.iter() {
        let py_hop = item.extract::<PyRef<PyIntHopState>>()?;
        int_hops.push(py_hop.inner.clone());
    }

    let result = int_mobius_solve(&int_hops).map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!("{e}"))
    })?;

    // Convert U256 to BigUint for Python
    let opt_input_biguint = u256_to_biguint(result.optimal_input);
    let profit_biguint = u256_to_biguint(result.profit);

    Ok(PyIntMobiusResult {
        optimal_input: opt_input_biguint,
        profit: profit_biguint,
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
fn py_int_simulate_path<'py>(py: Python<'py>, x: &Bound<'_, PyAny>, hops: &Bound<'_, PyList>) -> Result<Bound<'py, PyInt>, PyErr> {
    let x_u256 = extract_u256(x)?;
    let mut int_hops = Vec::new();
    for item in hops.iter() {
        let py_hop = item.extract::<PyRef<PyIntHopState>>()?;
        int_hops.push(py_hop.inner.clone());
    }

    let output = int_simulate_path(x_u256, &int_hops);
    u256_to_biguint(output).into_pyobject(py)
}

/// Convert U256 to BigUint for Python interop.
fn u256_to_biguint(v: U256) -> BigUint {
    BigUint::from_bytes_be(&v.to_be_bytes::<32>())
}

/// Extract U256 from a Python int or bytes.
fn extract_u256(obj: &Bound<'_, PyAny>) -> PyResult<U256> {
    // Try int first (most common)
    if let Ok(bigint) = obj.extract::<BigUint>() {
        if bigint.bits() > 256 {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "Value exceeds 256 bits"
            ));
        }
        let bytes = bigint.to_bytes_be();
        // Pad to 32 bytes
        let mut padded = [0u8; 32];
        padded[32 - bytes.len()..].copy_from_slice(&bytes);
        return Ok(U256::from_be_bytes(padded));
    }
    // Try bytes
    if let Ok(bytes) = obj.extract::<&[u8]>() {
        if bytes.len() > 32 {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "Bytes exceed 32 bytes"
            ));
        }
        let mut padded = [0u8; 32];
        padded[32 - bytes.len()..].copy_from_slice(bytes);
        return Ok(U256::from_be_bytes(padded));
    }

    Err(pyo3::exceptions::PyTypeError::new_err(
        "Expected int or bytes"
    ))
}
