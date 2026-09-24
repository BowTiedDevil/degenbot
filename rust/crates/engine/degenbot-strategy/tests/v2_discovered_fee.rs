//! M4LIT6 interface coverage: a discovered non-default V2 species fee must
//! survive connector loading, solver admission, lane composition, and the
//! shared settlement/backrun executable-hop projection.

#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use alloy::primitives::{address, aliases::U112, Address, Bytes, U256};
use degenbot_bot::arb_engine::path_info::build_path_info;
use degenbot_bot::bot_core::{
    pool_ingress::VerifyLevel, BotState, RegisterV2PoolParams, RouteRegistry,
};
use degenbot_bot::connector_index::V2ConnectorIndex;
use degenbot_db::{DegenbotDb, V2PoolRowInput};
use degenbot_execution::{solve_result::HopDescriptor, SolveResult};
use degenbot_pools::slot_layout::V2ReservesParts;
use degenbot_simulation::sim::evm::journal_pools::{
    PoolFamily, PoolPostKind, PoolPostState, TypedPoolPost,
};
use degenbot_solvers::mixed::{HopType, MixedPoolRef};
use degenbot_strategy::backrun_engine::{
    BackrunHopRef, BackrunSolver, BackrunV2Pool, LaneCandidate, LaneFamily,
};
use degenbot_strategy::backrun_strategy::{admit_extracted, backrun_encode_options};
use degenbot_strategy::cmd_executor_adapter::{CmdExecutorAdapter, CmdExecutorOutcome};
use degenbot_strategy::execution_context::{ExecutionContext, ETHEREUM_V4_POOL_MANAGER};
use degenbot_strategy::frame_pipeline::MarketContext;
use degenbot_strategy::project_candidate;
use degenbot_strategy::strategy_kit::StrategyKit;

const TOKEN0: Address = address!("0000000000000000000000000000000000000aa1");
const TOKEN1: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
const POOL_P: Address = address!("000000000000000000000000000000000000b001");
const POOL_Q: Address = address!("000000000000000000000000000000000000b002");
const FACTORY: Address = address!("420dd381b31aef6683db6b902084cb0ffece40da");
const CHAIN_ID: i64 = 8453;

fn post(address: Address, reserve0: u64, reserve1: u64) -> PoolPostState {
    PoolPostState {
        address,
        family: PoolFamily::V2Pair,
        kind: PoolPostKind::Typed(TypedPoolPost::V2 {
            reserves: V2ReservesParts {
                reserve0: U112::from(reserve0),
                reserve1: U112::from(reserve1),
                block_timestamp_last: 0,
            },
        }),
    }
}

#[test]
fn production_v2_lane_has_no_hard_coded_fee_default() {
    let engine = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/backrun_engine.rs"
    ));
    let engine = engine
        .split_once("#[cfg(test)]")
        .expect("engine test boundary")
        .0;
    let strategy = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/backrun_strategy.rs"
    ));
    for source in [engine, strategy] {
        for forbidden in [
            "LaneFamily::V2,",
            "v2_fee_bips(997",
            "fee_token0: (997, 1000)",
            "fee_token0: (997, 1_000)",
        ] {
            assert!(
                !source.contains(forbidden),
                "production retains hard-coded V2 fee default {forbidden:?}"
            );
        }
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the interface test keeps discovery, admission, both encoders, and golden bytes visible"
)]
fn discovered_non_default_v2_fee_reaches_executor_bytes_with_settlement_parity() {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    db.upsert_exchange(CHAIN_ID, "aerodrome_v2", FACTORY, None)
        .unwrap();
    db.upsert_v2_pools(
        CHAIN_ID,
        "aerodrome_v2",
        1,
        10_000,
        &[
            V2PoolRowInput {
                address: POOL_P,
                token0_address: TOKEN0,
                token1_address: TOKEN1,
                fee_token0: 5,
                fee_token1: 7,
                stable: Some(false),
            },
            V2PoolRowInput {
                address: POOL_Q,
                token0_address: TOKEN0,
                token1_address: TOKEN1,
                fee_token0: 5,
                fee_token1: 7,
                stable: Some(false),
            },
        ],
    )
    .unwrap();

    let index = V2ConnectorIndex::load(&db, CHAIN_ID).unwrap();
    let registry = Arc::new(RouteRegistry::new(index));
    let kit = StrategyKit::resolve(Some(registry), None, None, VerifyLevel::default(), None);
    let runtime = MarketContext::new(CHAIN_ID, Some(Arc::new(db)), kit, 8, 4);
    let mut solver = BackrunSolver::new();
    let affected = admit_extracted(
        &runtime,
        &mut solver,
        &[post(POOL_P, 500_000, 1_000)],
        1,
        "0xdiscovered-fee",
        None,
    );
    assert_eq!(affected.len(), 1, "the discovered P edge admits");
    let p_fees = match affected[0].family {
        LaneFamily::V2 { fees } => fees,
        other => panic!("expected V2 lane, got {other:?}"),
    };
    assert_eq!(p_fees.token0.executor_bips(), 5);
    assert_eq!(p_fees.token1.executor_bips(), 7);

    let q = runtime
        .index()
        .expect("connector registry")
        .edge_by_address(POOL_Q)
        .expect("discovered Q edge");
    let q_fees = q.fees.resolve().expect("valid discovered Q fee");
    let q_id = solver
        .admit_v2(&BackrunV2Pool {
            address: POOL_Q,
            token0: TOKEN0,
            token1: TOKEN1,
            reserve0: 200_000,
            reserve1: 900,
            fees: q.fees,
        })
        .expect("discovered Q fee admits");
    let p_id = affected[0].workspace_pool_id;

    let amounts = (123, vec![5_893_000, 1_235], vec![123, 5_892_315]);
    let candidate = LaneCandidate {
        path_id: 17,
        hops: vec![
            BackrunHopRef {
                pool_id: p_id,
                pool: POOL_P,
                token0: TOKEN0,
                token1: TOKEN1,
                zfo: false,
                family: LaneFamily::V2 { fees: p_fees },
            },
            BackrunHopRef {
                pool_id: q_id,
                pool: POOL_Q,
                token0: TOKEN0,
                token1: TOKEN1,
                zfo: true,
                family: LaneFamily::V2 { fees: q_fees },
            },
        ],
        optimal_input: amounts.0,
        hop_outputs: amounts.1.clone(),
        consumed_inputs: amounts.2.clone(),
        profit: 55,
    };
    let (backrun_path, backrun_result) = project_candidate(&candidate);
    let adapter = CmdExecutorAdapter::new(ExecutionContext::new(
        POOL_P,
        ETHEREUM_V4_POOL_MANAGER,
        TOKEN1,
    ));
    let CmdExecutorOutcome::Encoded(backrun) = adapter.compose(
        &backrun_path,
        &backrun_result,
        backrun_encode_options(1_000),
    ) else {
        panic!("backrun composes through the shared hop projection")
    };

    let mut core = BotState::new();
    let p = core
        .register_v2_pool(&RegisterV2PoolParams {
            address: POOL_P,
            token0: TOKEN0,
            token1: TOKEN1,
            reserve0: U112::from(500_000),
            reserve1: U112::from(1_000),
            fee_token0: (9_995, 10_000),
            fee_token1: (9_993, 10_000),
            ..Default::default()
        })
        .unwrap();
    let q = core
        .register_v2_pool(&RegisterV2PoolParams {
            address: POOL_Q,
            token0: TOKEN0,
            token1: TOKEN1,
            reserve0: U112::from(200_000),
            reserve1: U112::from(900),
            fee_token0: (9_995, 10_000),
            fee_token1: (9_993, 10_000),
            ..Default::default()
        })
        .unwrap();
    let settlement_path = build_path_info(
        &core,
        &[
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: p,
                zero_for_one: false,
            },
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: q,
                zero_for_one: true,
            },
        ],
    )
    .unwrap();
    let settlement_result = SolveResult {
        path_id: 0,
        hop_count: settlement_path.hops.len(),
        optimal_input: U256::from(amounts.0),
        hop_outputs: amounts.1.iter().copied().map(U256::from).collect(),
        consumed_inputs: amounts.2.iter().copied().map(U256::from).collect(),
        net_profit: U256::from(55),
        hop_descriptors: settlement_path
            .hops
            .iter()
            .map(HopDescriptor::from_hop_info)
            .collect(),
    };
    let CmdExecutorOutcome::Encoded(settlement) = adapter.compose(
        &settlement_path,
        &settlement_result,
        backrun_encode_options(1_000),
    ) else {
        panic!("settlement composes through the shared adapter")
    };

    assert_eq!(settlement, backrun, "shared V2 hop parity");
    let expected = Bytes::from(
        alloy::hex::decode(
            "ab5898e80000000000000000000000000000000000000000000000000000000000000040000000000000000000000000000000000000000000000000000000000003e801000000000000000000000000000000000000000000000000000000000000006200000000000000000000000000000000000000b002000000000000000000000000000000000000000aa1ff20fd0000000000000000000059eb88fd00072410010000000000000000000059eb88210001fd000510fefd00000000000000000000007b000000000000000000000000000000000000000000000000000000000000",
        )
        .expect("static expected hex"),
    );
    assert_eq!(settlement, expected, "non-default fee bytes");
}
