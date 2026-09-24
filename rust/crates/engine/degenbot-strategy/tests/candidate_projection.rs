#![expect(
    clippy::expect_used,
    reason = "projection fixtures are valid by construction"
)]

use alloy::primitives::{address, Address, B256, U256};
use degenbot_bot::bot_core::executor_hop::{V2FeePair, V2Fees};
use degenbot_execution::solve_result::HopDescriptor;
use degenbot_executor::composers::HopInfo;
use degenbot_strategy::backrun_engine::{BackrunHopRef, LaneCandidate, LaneFamily};
use degenbot_strategy::backrun_strategy::backrun_encode_options;
use degenbot_strategy::cmd_executor_adapter::{
    CmdExecutorAdapter, CmdExecutorDecline, CmdExecutorOutcome,
};
use degenbot_strategy::execution_context::ExecutionContext;
use degenbot_strategy::project_candidate;

const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
const TOK: Address = address!("0000000000000000000000000000000000000aa1");
const PM: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");
const EXECUTOR: Address = address!("00000000000000000000000000000000000000e1");
const OTHER_PM: Address = address!("000000000000000000000000000000000000c0fe");

fn v2_fees() -> V2Fees {
    V2FeePair::from_discovered(Some(3), Some(3), Some(1_000))
        .resolve()
        .expect("valid fixture fee")
}

fn hop(pool_id: u64, pool: Address, family: LaneFamily, zfo: bool) -> BackrunHopRef {
    BackrunHopRef {
        pool_id,
        pool,
        token0: TOK,
        token1: WETH,
        zfo,
        family,
    }
}

fn candidate(hops: Vec<BackrunHopRef>) -> LaneCandidate {
    LaneCandidate {
        path_id: 47,
        hops,
        optimal_input: 123,
        hop_outputs: vec![5_893_000, 6_000, 7_000],
        consumed_inputs: vec![123, 5_892_315, 5_999_000],
        profit: 55,
    }
}

#[test]
fn projection_preserves_path_identity_family_descriptors_and_amounts() {
    let candidate = candidate(vec![
        hop(
            1,
            address!("000000000000000000000000000000000000b001"),
            LaneFamily::V2 { fees: v2_fees() },
            false,
        ),
        hop(
            2,
            address!("000000000000000000000000000000000000b002"),
            LaneFamily::V3 { fee: 3_000 },
            true,
        ),
        hop(
            3,
            PM,
            LaneFamily::V4 {
                fee: 500,
                pool_id: B256::new([0x11; 32]),
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
            false,
        ),
    ]);
    let (path, result) = project_candidate(&candidate);

    assert_eq!(result.path_id, candidate.path_id);
    assert_eq!(result.hop_count, 3);
    assert_eq!(
        result.hop_descriptors,
        path.hops
            .iter()
            .map(HopDescriptor::from_hop_info)
            .collect::<Vec<_>>()
    );
    assert_eq!(result.optimal_input, U256::from(candidate.optimal_input));
    assert_eq!(
        result.hop_outputs,
        candidate
            .hop_outputs
            .iter()
            .copied()
            .map(U256::from)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        result.consumed_inputs,
        candidate
            .consumed_inputs
            .iter()
            .copied()
            .map(U256::from)
            .collect::<Vec<_>>()
    );
    assert!(matches!(path.hops[0], HopInfo::V2(_)));
    assert!(matches!(path.hops[1], HopInfo::V3(_)));
    assert!(matches!(path.hops[2], HopInfo::V4(_)));
}

#[test]
fn projection_preserves_v4_manager_for_adapter_validation() {
    let candidate = candidate(vec![
        hop(
            1,
            OTHER_PM,
            LaneFamily::V4 {
                fee: 500,
                pool_id: B256::new([0x22; 32]),
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
            false,
        ),
        hop(
            2,
            address!("000000000000000000000000000000000000b002"),
            LaneFamily::V2 { fees: v2_fees() },
            true,
        ),
    ]);
    let (path, result) = project_candidate(&candidate);
    assert!(matches!(path.hops[0], HopInfo::V4(ref hop) if hop.pool_manager_address == OTHER_PM));
    assert_eq!(
        CmdExecutorAdapter::new(ExecutionContext::new(EXECUTOR, PM, WETH)).compose(
            &path,
            &result,
            backrun_encode_options(0)
        ),
        CmdExecutorOutcome::Declined(CmdExecutorDecline::MixedPoolManagers)
    );
}
