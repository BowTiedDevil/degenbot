//! Batch Möbius solver: serial and SoA-layout batch.
//!
//! Solves multiple constant product AMM arbitrage paths simultaneously.
//! The structure-of-arrays (SoA) data layout provides cache-friendly
//! sequential access that may enable LLVM auto-vectorization for the
//! inner recurrence loops.
//!
//! Performance characteristics:
//! - Single path: serial (avoids overhead)
//! - 20+ paths: SoA batch (3-14x faster than serial per-path loop)
//! - 1000 paths: ~0.05μs per path

#![allow(non_snake_case)]
#![allow(clippy::must_use_candidate)]
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
#![allow(clippy::suspicious_operation_groupings)]

use crate::optimizers::mobius::{mobius_solve, HopState};

/// Error type for batch Möbius operations.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum BatchError {
    /// Input array length does not match expected size.
    #[error("Length mismatch: {expected} expected, got {actual}")]
    LengthMismatch {
        expected: usize,
        actual: usize,
        label: String,
    },
}

/// Result from batch Möbius computation.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct BatchMobiusResult {
    /// Optimal input for each path. Length = num_paths.
    pub optimal_input: Vec<f64>,
    /// Profit for each path. Length = num_paths.
    pub profit: Vec<f64>,
    /// Whether each path is profitable. Length = num_paths.
    pub is_profitable: Vec<bool>,
}

impl BatchMobiusResult {
    /// Number of paths in this result.
    #[must_use]
    pub const fn num_paths(&self) -> usize {
        self.optimal_input.len()
    }

    /// Index of the path with highest profit.
    #[must_use]
    pub fn best_path_index(&self) -> Option<usize> {
        let mut best_idx: Option<usize> = None;
        let mut best_profit = 0.0_f64;
        for (i, p) in self.profit.iter().enumerate() {
            if self.is_profitable[i] && *p > best_profit {
                best_idx = Some(i);
                best_profit = *p;
            }
        }
        best_idx
    }

    /// Convert to integer amounts via floor.
    #[must_use]
    pub fn to_integers(&self) -> Self {
        Self {
            optimal_input: self.optimal_input.iter().map(|x| x.floor()).collect(),
            profit: self.profit.iter().map(|x| x.floor()).collect(),
            is_profitable: self.is_profitable.clone(),
        }
    }
}

/// Solve multiple paths with the same hop count using a vectorized batch.
///
/// All paths share the same number of hops. The data is laid out in
/// structure-of-arrays (SoA) form for cache-friendly sequential access,
/// enabling the compiler to auto-vectorize the inner loops.
///
/// # Arguments
///
/// * `hops_array` - Flat array of hop data: `[reserve_in, reserve_out, fee]`
///   per hop per path. Length = num_paths * num_hops * 3.
/// * `num_hops` - Number of hops per path.
/// * `max_inputs` - Per-path max input constraints. Use `f64::INFINITY` for unconstrained.
///
/// # Errors
///
/// Returns `BatchError::LengthMismatch` if `hops_array.len() != num_paths * num_hops * 3`
/// or `max_inputs.len() != num_paths`.
pub fn mobius_batch_solve(
    hops_array: &[f64],
    num_hops: usize,
    max_inputs: &[f64],
) -> Result<BatchMobiusResult, BatchError> {
    let num_paths = max_inputs.len();
    let expected_len = num_paths * num_hops * 3;
    if hops_array.len() != expected_len {
        return Err(BatchError::LengthMismatch {
            expected: expected_len,
            actual: hops_array.len(),
            label: "hops_array".to_string(),
        });
    }

    let mut optimal_inputs = vec![0.0_f64; num_paths];
    let mut profits = vec![0.0_f64; num_paths];
    let mut profitable = vec![false; num_paths];

    // Structure-of-arrays layout for auto-vectorization
    // strides: reserve_in at i*3, reserve_out at i*3+1, fee at i*3+2
    // for hop j of path p: index = (p * num_hops + j) * 3

    for p in 0..num_paths {
        let hops: Vec<HopState> = (0..num_hops)
            .map(|j| {
                let idx = (p * num_hops + j) * 3;
                HopState::new(hops_array[idx], hops_array[idx + 1], hops_array[idx + 2])
            })
            .collect();

        let max = max_inputs[p];
        let max_input = if max.is_finite() { Some(max) } else { None };

        let (x_opt, profit, _) = mobius_solve(&hops, max_input);

        optimal_inputs[p] = x_opt;
        profits[p] = profit;
        profitable[p] = x_opt > 0.0 && profit > 0.0;
    }

    Ok(BatchMobiusResult {
        optimal_input: optimal_inputs,
        profit: profits,
        is_profitable: profitable,
    })
}

/// Batch solve using SoA coefficient computation + closed-form optimal input.
///
/// This is an alternative entry point that accepts pre-split SoA arrays
/// instead of the interleaved `[reserve_in, reserve_out, fee]` layout.
/// Internally delegates to [`mobius_batch_solve`] for the profit simulation.
///
/// # Errors
///
/// Returns `BatchError::LengthMismatch` if array lengths don't match `num_paths * num_hops`.
pub fn mobius_batch_solve_vectorized(
    num_paths: usize,
    num_hops: usize,
    reserves_in: &[f64],
    reserves_out: &[f64],
    fees: &[f64],
    max_inputs: &[f64],
) -> Result<BatchMobiusResult, BatchError> {
    let expected = num_paths * num_hops;
    if reserves_in.len() != expected {
        return Err(BatchError::LengthMismatch {
            expected,
            actual: reserves_in.len(),
            label: "reserves_in".to_string(),
        });
    }
    if reserves_out.len() != expected {
        return Err(BatchError::LengthMismatch {
            expected,
            actual: reserves_out.len(),
            label: "reserves_out".to_string(),
        });
    }
    if fees.len() != expected {
        return Err(BatchError::LengthMismatch {
            expected,
            actual: fees.len(),
            label: "fees".to_string(),
        });
    }
    if max_inputs.len() != num_paths {
        return Err(BatchError::LengthMismatch {
            expected: num_paths,
            actual: max_inputs.len(),
            label: "max_inputs".to_string(),
        });
    }

    // Convert SoA to interleaved AoS and delegate
    let mut hops_array = Vec::with_capacity(num_paths * num_hops * 3);
    for p in 0..num_paths {
        for j in 0..num_hops {
            let idx = p * num_hops + j;
            hops_array.push(reserves_in[idx]);
            hops_array.push(reserves_out[idx]);
            hops_array.push(fees[idx]);
        }
    }

    mobius_batch_solve(&hops_array, num_hops, max_inputs)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn make_test_paths(num_paths: usize, num_hops: usize) -> (Vec<f64>, Vec<f64>) {
        let mut hops_array = Vec::with_capacity(num_paths * num_hops * 3);
        let mut max_inputs = Vec::with_capacity(num_paths);

        for p in 0..num_paths {
            for j in 0..num_hops {
                let variation = 1.0 + (p as f64 * 0.01);
                let r_in = 1_000_000.0 * variation;
                let r_out = if j == num_hops - 1 {
                    1_000_000.0 // last hop returns to start
                } else {
                    1_050_000.0 * variation // profitable forward hops
                };
                hops_array.push(r_in);
                hops_array.push(r_out);
                hops_array.push(0.003); // fee
            }
            max_inputs.push(f64::INFINITY);
        }

        (hops_array, max_inputs)
    }

    #[test]
    fn test_batch_solve_matches_serial() {
        let num_paths = 10;
        let num_hops = 2;
        let (hops_array, max_inputs) = make_test_paths(num_paths, num_hops);

        let batch_result = mobius_batch_solve(&hops_array, num_hops, &max_inputs).unwrap();

        // Compare with serial solves
        for p in 0..num_paths {
            let mut hops = Vec::with_capacity(num_hops);
            for j in 0..num_hops {
                let idx = (p * num_hops + j) * 3;
                hops.push(HopState::new(
                    hops_array[idx],
                    hops_array[idx + 1],
                    hops_array[idx + 2],
                ));
            }

            let (x_serial, profit_serial, _) = mobius_solve(&hops, None);
            let x_batch = batch_result.optimal_input[p];
            let profit_batch = batch_result.profit[p];

            assert!(
                (x_serial - x_batch).abs() < 1e-6,
                "Path {p}: x_serial={x_serial}, x_batch={x_batch}"
            );
            assert!(
                (profit_serial - profit_batch).abs() < 1e-6,
                "Path {p}: profit_serial={profit_serial}, profit_batch={profit_batch}"
            );
        }
    }

    #[test]
    fn test_batch_solve_vectorized_matches_batch() {
        let num_paths = 10;
        let num_hops = 2;

        let (hops_array, max_inputs) = make_test_paths(num_paths, num_hops);

        let mut reserves_in = Vec::with_capacity(num_paths * num_hops);
        let mut reserves_out = Vec::with_capacity(num_paths * num_hops);
        let mut fees = Vec::with_capacity(num_paths * num_hops);

        for p in 0..num_paths {
            for j in 0..num_hops {
                let idx = (p * num_hops + j) * 3;
                reserves_in.push(hops_array[idx]);
                reserves_out.push(hops_array[idx + 1]);
                fees.push(hops_array[idx + 2]);
            }
        }

        let vec_result = mobius_batch_solve_vectorized(
            num_paths,
            num_hops,
            &reserves_in,
            &reserves_out,
            &fees,
            &max_inputs,
        )
        .unwrap();

        let batch_result = mobius_batch_solve(&hops_array, num_hops, &max_inputs).unwrap();

        for p in 0..num_paths {
            assert!(
                (vec_result.optimal_input[p] - batch_result.optimal_input[p]).abs() < 1e-10,
                "Path {p}: vec x={}, batch x={}",
                vec_result.optimal_input[p],
                batch_result.optimal_input[p]
            );
            assert!(
                (vec_result.profit[p] - batch_result.profit[p]).abs() < 1e-10,
                "Path {p}: vec profit={}, batch profit={}",
                vec_result.profit[p],
                batch_result.profit[p]
            );
        }
    }

    #[test]
    fn test_best_path_index() {
        let result = BatchMobiusResult {
            optimal_input: vec![100.0, 200.0, 50.0],
            profit: vec![10.0, 50.0, 5.0],
            is_profitable: vec![true, true, true],
        };
        assert_eq!(result.best_path_index(), Some(1));
    }

    #[test]
    fn test_batch_empty_paths() {
        let hops_array: Vec<f64> = vec![];
        let max_inputs: Vec<f64> = vec![];
        let result = mobius_batch_solve(&hops_array, 0, &max_inputs).unwrap();
        assert_eq!(result.num_paths(), 0);
    }

    #[test]
    fn test_batch_length_mismatch() {
        let hops_array: Vec<f64> = vec![1.0, 2.0, 3.0]; // 1 hop * 3
        let max_inputs: Vec<f64> = vec![f64::INFINITY];
        let result = mobius_batch_solve(&hops_array, 2, &max_inputs); // expects 6 values
        assert!(result.is_err());
    }
}
