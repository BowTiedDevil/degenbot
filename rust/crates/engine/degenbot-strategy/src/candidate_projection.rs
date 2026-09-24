//! Strategy-owned projection of executable candidates into the production
//! adapter's generic solve-result vocabulary.

use crate::backrun_engine::{LaneCandidate, LaneFamily};
use alloy::primitives::U256;
use degenbot_bot::bot_core::executor_hop::{v2_hop, v3_hop, v4_hop};
use degenbot_execution::{solve_result::HopDescriptor, SolveResult};
use degenbot_executor::composers::PathInfo;

/// Project one solved strategy candidate into `PathInfo` and `SolveResult`.
///
/// Family and fee data stay in the candidate because they are strategy
/// concerns; the returned values contain only the vocabulary consumed by the
/// production adapter. The candidate path id is the workspace declaration id,
/// so a valid zero remains a real first-path identity rather than a sentinel.
#[must_use]
pub fn project_candidate(candidate: &LaneCandidate) -> (PathInfo, SolveResult) {
    let path = PathInfo::new(
        candidate
            .hops
            .iter()
            .map(|hop| match hop.family {
                LaneFamily::V2 { fees } => v2_hop(
                    hop.pool,
                    hop.token0,
                    hop.token1,
                    fees.direction(hop.zfo),
                    hop.zfo,
                ),
                LaneFamily::V3 { fee } => v3_hop(hop.pool, hop.token0, hop.token1, fee, hop.zfo),
                LaneFamily::V4 {
                    fee,
                    pool_id,
                    tick_spacing,
                    hooks,
                } => v4_hop(
                    hop.pool,
                    pool_id,
                    hop.token0,
                    hop.token1,
                    fee,
                    tick_spacing,
                    hooks,
                    hop.zfo,
                ),
            })
            .collect(),
    );
    let result = SolveResult {
        path_id: candidate.path_id,
        hop_count: candidate.hops.len(),
        optimal_input: U256::from(candidate.optimal_input),
        hop_outputs: candidate
            .hop_outputs
            .iter()
            .copied()
            .map(U256::from)
            .collect(),
        consumed_inputs: candidate
            .consumed_inputs
            .iter()
            .copied()
            .map(U256::from)
            .collect(),
        net_profit: U256::from(candidate.profit),
        hop_descriptors: path.hops.iter().map(HopDescriptor::from_hop_info).collect(),
    };
    (path, result)
}
