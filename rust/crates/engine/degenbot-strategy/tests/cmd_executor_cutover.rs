#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "cutover tests assert valid fixture composition"
)]

use std::sync::{atomic::AtomicU64, Arc};

use alloy::primitives::{address, Address, B256};

use degenbot_bot::bot_core::executor_hop::{V2FeePair, V2Fees};
use degenbot_strategy::backrun::{Decision, MevblockerBackrun};
use degenbot_strategy::backrun_engine::{BackrunHopRef, LaneCandidate, LaneFamily};
use degenbot_strategy::backrun_strategy::{BackrunEvaluated, BackrunStrategy};
use degenbot_strategy::cmd_executor_adapter::{CmdExecutorAdapter, CmdExecutorOutcome};
use degenbot_strategy::execution_context::{ExecutionContext, ETHEREUM_WETH as WETH};
use degenbot_strategy::frame_pipeline::{PipelineConfig, StageTrace};
use degenbot_strategy::pending_tx::{GateInput, PendingTxReaction};
use degenbot_strategy::project_candidate;

const TOK: Address = address!("0000000000000000000000000000000000000aa1");
const EXECUTOR: Address = address!("00000000000000000000000000000000000000e1");

fn fees() -> V2Fees {
    V2FeePair::from_discovered(Some(3), Some(3), Some(1_000))
        .resolve()
        .expect("valid fee")
}

fn candidate() -> LaneCandidate {
    LaneCandidate {
        path_id: 17,
        hops: vec![
            BackrunHopRef {
                pool_id: 1,
                pool: address!("000000000000000000000000000000000000b001"),
                token0: TOK,
                token1: WETH,
                zfo: false,
                family: LaneFamily::V2 { fees: fees() },
            },
            BackrunHopRef {
                pool_id: 2,
                pool: address!("000000000000000000000000000000000000b002"),
                token0: TOK,
                token1: WETH,
                zfo: true,
                family: LaneFamily::V2 { fees: fees() },
            },
        ],
        optimal_input: 123,
        hop_outputs: vec![5_893_000, 1_235],
        consumed_inputs: vec![123, 5_892_315],
        profit: 1_000_000_000,
    }
}

#[test]
fn candidate_projects_to_the_adapter_interface() {
    let candidate = candidate();
    let (path, result) = project_candidate(&candidate);

    assert_eq!(path.hops.len(), 2);
    assert_eq!(result.hop_count, 2);
    assert_eq!(result.hop_descriptors.len(), 2);
    assert_eq!(
        result.net_profit,
        alloy::primitives::U256::from(candidate.profit)
    );
}

#[test]
fn strategy_declines_v4_candidate_from_a_different_session_manager() {
    let execution = ExecutionContext::ethereum(EXECUTOR);
    let mut strategy = BackrunStrategy::new(execution);
    let evaluated = BackrunEvaluated {
        stats: degenbot_strategy::backrun_strategy::SolveStats {
            best: Some(LaneCandidate {
                path_id: 17,
                hops: vec![
                    BackrunHopRef {
                        pool_id: 1,
                        pool: address!("000000000000000000000000000000000000c0fe"),
                        token0: TOK,
                        token1: WETH,
                        zfo: false,
                        family: LaneFamily::V4 {
                            fee: 3_000,
                            pool_id: B256::ZERO,
                            tick_spacing: 60,
                            hooks: Address::ZERO,
                        },
                    },
                    BackrunHopRef {
                        pool_id: 2,
                        pool: address!("000000000000000000000000000000000000b002"),
                        token0: TOK,
                        token1: WETH,
                        zfo: true,
                        family: LaneFamily::V2 { fees: fees() },
                    },
                ],
                optimal_input: 123,
                hop_outputs: vec![5_893_000, 1_235],
                consumed_inputs: vec![123, 5_892_315],
                profit: 1_000_000_000,
            }),
            ..Default::default()
        },
    };
    let pipeline = PipelineConfig {
        execution,
        owner: Address::ZERO,
        bribe_bips: 9_800,
        wallet_gas_cost_wei: Arc::new(AtomicU64::new(100_000_000)),
        gas_floor_wei: alloy::primitives::U256::ZERO,
        fixture_mode: false,
    };

    let knobs =
        MevblockerBackrun::from_config(&degenbot_config::BotConfig::default(), String::new())
            .into_config();
    let mut stage_trace = StageTrace::default();
    let mut cx = frame_context(&pipeline, &knobs, &mut stage_trace, "0xwrong-manager");
    assert!(strategy.compose(&mut cx, &evaluated).is_none());
}

/// The stages' production context over one test's config + evidence buffer.
fn frame_context<'a>(
    pl: &'a PipelineConfig,
    knobs: &'a degenbot_strategy::BackrunConfig,
    trace: &'a mut StageTrace,
    trace_tx: &'a str,
) -> degenbot_strategy::pending_tx::FrameContext<'a> {
    degenbot_strategy::pending_tx::FrameContext {
        pl,
        knobs,
        head: 0,
        trace_tx,
        trace,
    }
}

#[test]
fn wallet_true_net_bid_recomposes_through_the_session_adapter() {
    let candidate = candidate();
    let execution = ExecutionContext::ethereum(EXECUTOR);
    let mut strategy = BackrunStrategy::new(execution);
    let evaluated = BackrunEvaluated {
        stats: degenbot_strategy::backrun_strategy::SolveStats {
            best: Some(candidate),
            ..Default::default()
        },
    };
    let pipeline = PipelineConfig {
        execution,
        owner: Address::ZERO,
        bribe_bips: 9_800,
        wallet_gas_cost_wei: Arc::new(AtomicU64::new(100_000_000)),
        gas_floor_wei: alloy::primitives::U256::ZERO,
        fixture_mode: false,
    };

    let mut config =
        MevblockerBackrun::from_config(&degenbot_config::BotConfig::default(), String::new())
            .into_config();
    config.bid_mode = true;
    config.budget_wei = alloy::primitives::U256::from(1_000_000_000u64);
    config.max_bundle_wei = alloy::primitives::U256::from(1_000_000_000u64);
    let mut stage_trace = StageTrace::default();
    let mut cx = frame_context(&pipeline, &config, &mut stage_trace, "0xcutover");
    let ceiling = strategy
        .compose(&mut cx, &evaluated)
        .expect("ceiling composition encodes");
    let decided = strategy.decide(
        &mut cx,
        &GateInput {
            evaluated: &evaluated,
            composed: Some(&ceiling),
            sim_ok: true,
            spent: alloy::primitives::U256::ZERO,
        },
    );

    assert!(matches!(decided.decision, Decision::Bid { .. }));
    let economics = decided.economics.expect("wallet economics are recorded");
    assert_eq!(economics.bribe_bips, 8_950);
    assert_eq!(
        economics.bid_wei,
        alloy::primitives::U256::from(895_000_000u64)
    );
    let submit = decided
        .submit_calldata
        .expect("recomposed calldata submits");
    assert_ne!(
        submit, ceiling.sim_calldata,
        "wallet-true bips replace the ceiling config"
    );

    let (path, result) = project_candidate(evaluated.stats.best.as_ref().unwrap());
    let expected = CmdExecutorAdapter::new(execution).compose(
        &path,
        &result,
        degenbot_strategy::backrun_strategy::backrun_encode_options(economics.bribe_bips),
    );
    assert!(matches!(expected, CmdExecutorOutcome::Encoded(bytes) if bytes == submit));
}
