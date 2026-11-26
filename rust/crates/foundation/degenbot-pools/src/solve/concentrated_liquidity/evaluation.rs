use alloy::primitives::{I256, U256};

use super::active_set::{simulate_walk_path_into, PieceView, WalkPathOutcome};
use super::direction::walk_profit_score;
use super::tally::WalkTally;
use super::WalkSolution;
use crate::state::int_v3_hop::SwapWalkCensus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProbeKind {
    Candidate,
    RefineTernary,
    RefineGrid,
}

pub(super) struct WalkEvaluator<'a> {
    hops: &'a [PieceView<'a>],
    input: U256,
    profit: U256,
    hop_outputs: Vec<U256>,
    census: Vec<Option<SwapWalkCensus>>,
    top_score: I256,
    scratch: WalkPathOutcome,
}

impl<'a> WalkEvaluator<'a> {
    pub(super) fn new(hops: &'a [PieceView<'a>]) -> Self {
        Self {
            hops,
            input: U256::ZERO,
            profit: U256::ZERO,
            hop_outputs: Vec::new(),
            census: Vec::new(),
            top_score: I256::MIN,
            scratch: WalkPathOutcome::default(),
        }
    }

    /// Best-so-far profit as a non-negative output−input level, or zero
    /// until the first profitable candidate. The envelope clamp keeps only
    /// inputs whose upper bound can still beat this floor.
    pub(super) fn best_floor(&self) -> U256 {
        if self.top_score <= I256::ZERO {
            return U256::ZERO;
        }
        U256::try_from(self.top_score).unwrap_or(U256::ZERO)
    }

    pub(super) fn top_score(&self) -> I256 {
        self.top_score
    }

    /// Evaluate and record a candidate, reusing the outcome allocation and
    /// cloning hop outputs and census only when a new profit maximum is found.
    pub(super) fn observe(
        &mut self,
        candidate: U256,
        kind: ProbeKind,
        tally: &mut WalkTally,
    ) -> &WalkPathOutcome {
        match kind {
            ProbeKind::Candidate => {}
            ProbeKind::RefineTernary => {
                tally.bump_refine_sims(1);
                tally.bump_ternary_sims(1);
            }
            ProbeKind::RefineGrid => {
                tally.bump_refine_sims(1);
                tally.bump_grid_sims(1);
            }
        }
        let Self {
            input,
            profit,
            hop_outputs,
            census,
            top_score,
            scratch,
            ..
        } = self;
        simulate_walk_path_into(candidate, self.hops, tally, scratch);
        let score = walk_profit_score(scratch.final_output, candidate);
        if score > *top_score {
            *top_score = score;
        }
        if scratch.final_output > candidate {
            let candidate_profit = scratch.final_output - candidate;
            if candidate_profit > *profit {
                *profit = candidate_profit;
                *input = candidate;
                hop_outputs.clone_from(&scratch.hop_outputs);
                *census = self
                    .hops
                    .iter()
                    .zip(scratch.facts.iter())
                    .map(|(hop, &facts)| hop.census(facts))
                    .collect();
            }
        }
        scratch
    }

    pub(super) fn into_solution(self) -> Option<WalkSolution> {
        if self.profit.is_zero() {
            return None;
        }
        Some(WalkSolution {
            optimal_input: self.input,
            profit: self.profit,
            hop_outputs: self.hop_outputs,
            census: self.census,
        })
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use degenbot_math::v2::IntHopState;

    fn v2() -> IntHopState {
        IntHopState::new(
            U256::from(1_000_000u64),
            U256::from(1_000_000u64),
            997_000,
            1_000_000,
        )
    }

    #[test]
    fn thin_coarse_and_corner_candidates_only_add_total_sims() {
        let hop = v2();
        let hops = [PieceView::constant_product(&hop)];
        let mut evaluator = WalkEvaluator::new(&hops);
        let mut tally = WalkTally::default();
        evaluator.observe(U256::from(10u64), ProbeKind::Candidate, &mut tally);
        for _ in 0..33 {
            evaluator.observe(U256::from(10u64), ProbeKind::Candidate, &mut tally);
        }
        evaluator.observe(U256::from(10u64), ProbeKind::Candidate, &mut tally);
        evaluator.observe(U256::from(10u64), ProbeKind::Candidate, &mut tally);
        let stats = tally.walk_stats();
        #[cfg(feature = "telemetry")]
        {
            assert_eq!(stats.sims, 36);
            assert_eq!(stats.refine_sims, 0);
            assert_eq!(stats.ternary_sims, 0);
            assert_eq!(stats.grid_sims, 0);
        }
        #[cfg(not(feature = "telemetry"))]
        assert_eq!(stats, super::super::WalkStats::default());
    }

    #[test]
    fn refine_probe_kinds_preserve_existing_counter_partition() {
        let hop = v2();
        let hops = [PieceView::constant_product(&hop)];
        let mut evaluator = WalkEvaluator::new(&hops);
        let mut tally = WalkTally::default();
        evaluator.observe(U256::from(10u64), ProbeKind::RefineTernary, &mut tally);
        evaluator.observe(U256::from(10u64), ProbeKind::RefineGrid, &mut tally);
        let stats = tally.walk_stats();
        #[cfg(feature = "telemetry")]
        {
            assert_eq!(stats.sims, 2);
            assert_eq!(stats.refine_sims, 2);
            assert_eq!(stats.ternary_sims, 1);
            assert_eq!(stats.grid_sims, 1);
        }
        #[cfg(not(feature = "telemetry"))]
        assert_eq!(stats, super::super::WalkStats::default());
    }

    #[test]
    fn evaluator_reuses_scratch_and_keeps_first_equal_profit_candidate() {
        let first = v2();
        let second = IntHopState::new(
            U256::from(1_000_000u64),
            U256::from(2_000_000u64),
            997_000,
            1_000_000,
        );
        let hops = [
            PieceView::constant_product(&first),
            PieceView::constant_product(&second),
        ];
        let mut evaluator = WalkEvaluator::new(&hops);
        let mut tally = WalkTally::default();
        let first_ptr = evaluator
            .observe(U256::from(10u64), ProbeKind::Candidate, &mut tally)
            .hop_outputs
            .as_ptr();
        let second_ptr = evaluator
            .observe(U256::from(10u64), ProbeKind::Candidate, &mut tally)
            .hop_outputs
            .as_ptr();
        assert_eq!(first_ptr, second_ptr);
        let solution = evaluator.into_solution().unwrap();
        assert_eq!(solution.optimal_input, U256::from(10u64));
        assert_eq!(solution.hop_outputs.len(), 2);
        assert_eq!(solution.census, [None, None]);
        assert!(solution.profit > U256::ZERO);
    }
}
