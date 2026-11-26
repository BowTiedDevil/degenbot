use alloy::primitives::{I256, U256};

use super::direction::walk_profit_score;
use super::evaluation::{ProbeKind, WalkEvaluator};
use super::tally::WalkTally;
use super::window::{clamp_window_to_bound, RefineBound};
use crate::solve::runtime::SolveRuntimeConfig;

/// Maximum width the refine ternary settles to before the final probe grid.
/// The caller pins `hi` to the exact right edge for bounded pieces, so the
/// grid captures corner maxima while the coarse bracket bounds interior error.
pub(super) const REFINE_BRACKET_WEI: u64 = 1_000_000;
/// Final-bracket width at or below which refinement sweeps every wei instead
/// of using the coarse grid, preserving sharp narrow-range peaks exactly.
pub(super) const REFINE_DENSE_SPAN: u64 = 1024;
/// Points probed across a wide final bracket, including both endpoints.
const REFINE_GRID_POINTS: u64 = 33;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RefineWindow {
    pub(super) lo: U256,
    pub(super) hi: U256,
}

pub(super) struct Refiner<'a, 'b> {
    evaluator: &'b mut WalkEvaluator<'a>,
    argmax_x: U256,
    best_score: I256,
}

impl<'a, 'b> Refiner<'a, 'b> {
    pub(super) fn new(evaluator: &'b mut WalkEvaluator<'a>) -> Self {
        Self {
            evaluator,
            argmax_x: U256::ZERO,
            best_score: I256::MIN,
        }
    }

    fn probe(&mut self, x: U256, kind: ProbeKind, tally: &mut WalkTally) -> I256 {
        let outcome = self.evaluator.observe(x, kind, tally);
        let score = walk_profit_score(outcome.final_output, x);
        if score > self.best_score {
            self.best_score = score;
            self.argmax_x = x;
        }
        score
    }

    /// Maximize signed profit over one window using ternary narrowing followed
    /// by a grid or wei-precise sweep. Concavity within each piece makes the
    /// ternary valid; the final probes cover the integer staircase and corners.
    /// This is a profit-epsilon search, not an exact-wei argmax guarantee.
    ///
    /// Returns the best input and signed score observed in this window; the
    /// shared evaluator owns the global best across all candidates.
    pub(super) fn refine_window(
        &mut self,
        window: RefineWindow,
        piece_anchor: Option<U256>,
        cfg: &SolveRuntimeConfig,
        env: Option<&[RefineBound]>,
        tally: &mut WalkTally,
    ) -> (U256, I256) {
        let RefineWindow { mut lo, mut hi } = window;
        self.argmax_x = lo;
        self.best_score = I256::MIN;
        // The composed bound dominates output pointwise, so an input whose
        // bound minus input cannot beat the incumbent needs no simulation.
        if let Some(env) = env {
            match clamp_window_to_bound(env, lo, hi, self.evaluator.best_floor()) {
                Some((clamped_lo, clamped_hi)) => {
                    lo = clamped_lo;
                    hi = clamped_hi;
                }
                None => {
                    return (lo, I256::MIN);
                }
            }
            if lo > hi {
                return (lo, I256::MIN);
            }
        }
        // An in-window smooth Möbius anchor brackets the discrete top at wei
        // scale. Neighbor refinements pass None because a different piece's
        // anchor is not evidence for their window and keeps the full search.
        if let Some(anchor) =
            piece_anchor.filter(|a| cfg.refine_model_anchor && *a >= lo && *a <= hi)
        {
            lo = anchor
                .saturating_sub(U256::from(REFINE_BRACKET_WEI))
                .max(lo);
            hi = anchor
                .saturating_add(U256::from(REFINE_BRACKET_WEI))
                .min(hi);
        }
        let mut l = lo;
        let mut r = hi;
        while r.saturating_sub(l) > U256::from(REFINE_BRACKET_WEI) {
            let third = ((r - l) / U256::from(3u64)).max(U256::ONE);
            let m1 = l + third;
            let m2 = r - third;
            let s1 = self.probe(m1, ProbeKind::RefineTernary, tally);
            let s2 = self.probe(m2, ProbeKind::RefineTernary, tally);
            if s1 < s2 {
                l = m1 + U256::from(1u64);
            } else {
                r = m2.saturating_sub(U256::from(1u64));
            }
        }
        let span = r.saturating_sub(l);
        if span <= U256::from(REFINE_DENSE_SPAN) {
            // A narrow bracket is cheap to search exactly and may contain a
            // sharp integer-scale peak.
            let mut x = l;
            loop {
                self.probe(x, ProbeKind::RefineGrid, tally);
                if x >= r {
                    break;
                }
                x += U256::from(1u64);
            }
        } else {
            // Endpoints are included so flat tops and range-saturation corners
            // receive the same profit-epsilon coverage as interior candidates.
            for i in 0..REFINE_GRID_POINTS {
                let x = l + (span * U256::from(i)) / U256::from(REFINE_GRID_POINTS - 1);
                self.probe(x, ProbeKind::RefineGrid, tally);
            }
        }
        (self.argmax_x, self.best_score)
    }
}
