//! Rust-owned arbitrage route handles for repeated Möbius solves.
//!
//! The route handle stores parsed hop state on the Rust side. Python can build
//! it once and solve repeatedly with different max-input constraints without
//! rebuilding PyO3 hop objects or reparsing Python lists on every hot call.

use alloy::primitives::U256;

use crate::optimizers::mobius::HopState;
use crate::optimizers::mobius_int::{mobius_solve_with_refinement, IntHopState};
use crate::optimizers::mobius_v3::{solve_v3_tick_range_sequence, V3TickRangeSequence};
use crate::optimizers::mobius_v3_v3::solve_v3_v3;

/// Method tags returned by Rust route solving.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteSolveMethod {
    /// Closed-form Möbius solve.
    Mobius = 0,
    /// Piecewise Möbius solve for one multi-range V3 hop.
    PiecewiseMobius = 1,
    /// Piecewise solve for two multi-range V3 hops.
    V3V3 = 2,
    /// Route is outside the Rust solver's supported surface.
    NotSupported = 255,
}

/// Result from a Rust-owned route solve.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct RouteSolveResult {
    /// Float optimal input from the selected solver.
    pub optimal_input: f64,
    /// Float profit from the selected solver.
    pub profit: f64,
    /// Iteration count used by the selected solver.
    pub iterations: u32,
    /// True when the selected solver found a positive-profit solution.
    pub success: bool,
    /// Selected route solve method.
    pub method: RouteSolveMethod,
    /// True when this route is supported by the Rust solver.
    pub supported: bool,
    /// EVM-exact integer optimal input, when integer hops are available.
    pub optimal_input_int: Option<U256>,
    /// EVM-exact integer profit, when integer hops are available.
    pub profit_int: Option<U256>,
}

impl RouteSolveResult {
    /// Build a result for unsupported routes.
    #[must_use]
    pub const fn not_supported() -> Self {
        Self {
            optimal_input: 0.0,
            profit: 0.0,
            iterations: 0,
            success: false,
            method: RouteSolveMethod::NotSupported,
            supported: false,
            optimal_input_int: None,
            profit_int: None,
        }
    }
}

/// Parsed arbitrage route owned by Rust.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ArbRoute {
    base_hops: Vec<HopState>,
    int_hops: Vec<IntHopState>,
    all_int: bool,
    v3_sequences: Vec<(usize, V3TickRangeSequence)>,
}

impl ArbRoute {
    /// Create a Rust-owned route from already parsed hop state.
    #[must_use]
    pub fn new(
        base_hops: Vec<HopState>,
        int_hops: Vec<IntHopState>,
        all_int: bool,
        v3_sequences: Vec<(usize, V3TickRangeSequence)>,
    ) -> Self {
        Self {
            base_hops,
            int_hops,
            all_int,
            v3_sequences,
        }
    }

    /// Number of hops in the route.
    #[must_use]
    pub fn len(&self) -> usize {
        self.base_hops.len()
    }

    /// True when the route has no hops.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.base_hops.is_empty()
    }

    /// Number of multi-range V3 sequences attached to this route.
    #[must_use]
    pub fn v3_sequence_count(&self) -> usize {
        self.v3_sequences.len()
    }

    /// Solve this route without reparsing Python-side hop state.
    #[must_use]
    pub fn solve(&self, max_input: Option<f64>, max_candidates: usize) -> RouteSolveResult {
        if self.base_hops.len() < 2 {
            return RouteSolveResult::not_supported();
        }

        if self.v3_sequences.is_empty() {
            let result = mobius_solve_with_refinement(
                &self.base_hops,
                &self.int_hops,
                self.all_int,
                max_input,
            );
            return RouteSolveResult {
                optimal_input: result.optimal_input,
                profit: result.profit,
                iterations: result.iterations,
                success: result.success,
                method: RouteSolveMethod::Mobius,
                supported: true,
                optimal_input_int: result.optimal_input_int,
                profit_int: result.profit_int,
            };
        }

        if self.v3_sequences.len() == 2 && self.base_hops.len() == 2 {
            let seq0 = &self.v3_sequences[0].1;
            let seq1 = &self.v3_sequences[1].1;
            let (x_opt, profit, iters) = solve_v3_v3(seq0, seq1, max_input, max_candidates);
            return RouteSolveResult {
                optimal_input: x_opt,
                profit,
                iterations: iters,
                success: x_opt > 0.0 && profit > 0.0,
                method: RouteSolveMethod::V3V3,
                supported: true,
                optimal_input_int: None,
                profit_int: None,
            };
        }

        if self.v3_sequences.len() == 1 {
            let (v3_idx, seq) = &self.v3_sequences[0];
            let (x_opt, profit, iters) = solve_v3_tick_range_sequence(
                &self.base_hops,
                *v3_idx,
                seq,
                max_candidates,
                max_input,
            );
            return RouteSolveResult {
                optimal_input: x_opt,
                profit,
                iterations: iters,
                success: x_opt > 0.0 && profit > 0.0,
                method: RouteSolveMethod::PiecewiseMobius,
                supported: true,
                optimal_input_int: None,
                profit_int: None,
            };
        }

        RouteSolveResult::not_supported()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use alloy::primitives::U256;

    use super::{ArbRoute, RouteSolveMethod};
    use crate::optimizers::mobius::HopState;
    use crate::optimizers::mobius_int::IntHopState;

    #[test]
    fn route_solve_matches_profitable_mobius_path() {
        let base_hops = vec![
            HopState::new(1_000_000.0, 5_000_000.0, 0.003),
            HopState::new(1_500_000.0, 3_000_000.0, 0.003),
        ];
        let int_hops = vec![
            IntHopState::new(U256::from(1_000_000), U256::from(5_000_000), 997, 1000),
            IntHopState::new(U256::from(1_500_000), U256::from(3_000_000), 997, 1000),
        ];
        let route = ArbRoute::new(base_hops, int_hops, true, Vec::new());

        let result = route.solve(None, 10);

        assert!(result.supported);
        assert!(result.success);
        assert_eq!(result.method, RouteSolveMethod::Mobius);
        assert!(result.optimal_input_int.is_some());
        assert!(result.profit_int.is_some());
    }

    #[test]
    fn route_rejects_single_hop() {
        let route = ArbRoute::new(
            vec![HopState::new(1_000_000.0, 5_000_000.0, 0.003)],
            Vec::new(),
            false,
            Vec::new(),
        );

        let result = route.solve(None, 10);

        assert!(!result.supported);
        assert!(!result.success);
        assert_eq!(result.method, RouteSolveMethod::NotSupported);
    }
}
