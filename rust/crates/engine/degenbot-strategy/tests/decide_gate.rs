//! The decide gate, tested through the real [`PendingTxReaction::decide`] on
//! production arguments: a composed [`ComposedIntent`] from the session
//! adapter, the driver's simulation verdict, and the live knobs. The
//! actionable-arm semantics are named here — every frame reaching the
//! decision stage is actionable, so the gate speaks in bid economics and
//! observe labels, never in target classification.

#![expect(clippy::unwrap_used, reason = "tests resolve valid fee fixtures")]

use std::sync::{atomic::AtomicU64, Arc};

use alloy::primitives::{address, Address, U256};

use degenbot_strategy::backrun::{Decision, MevblockerBackrun};
use degenbot_strategy::backrun_engine::{BackrunHopRef, LaneCandidate, LaneFamily};
use degenbot_strategy::backrun_strategy::{BackrunEvaluated, BackrunStrategy};
use degenbot_strategy::execution_context::{ExecutionContext, ETHEREUM_WETH as WETH};
use degenbot_strategy::frame_pipeline::{PipelineConfig, StageTrace};
use degenbot_strategy::pending_tx::{Decided, GateInput, PendingTxReaction};

const TOK: Address = address!("0000000000000000000000000000000000000aa1");
const EXECUTOR: Address = address!("00000000000000000000000000000000000000e1");

/// The production-shaped evaluated frame: a solved WETH-closing V2 cycle
/// with 1 ETH gross profit.
fn evaluated_with_profit(profit: u128) -> BackrunEvaluated {
    let hops = vec![
        BackrunHopRef {
            pool_id: 1,
            pool: address!("000000000000000000000000000000000000b001"),
            token0: TOK,
            token1: WETH,
            zfo: false,
            family: LaneFamily::V2 {
                fees: degenbot_bot::bot_core::executor_hop::V2FeePair::from_discovered(
                    Some(3),
                    Some(3),
                    Some(1_000),
                )
                .resolve()
                .unwrap(),
            },
        },
        BackrunHopRef {
            pool_id: 2,
            pool: address!("000000000000000000000000000000000000b002"),
            token0: TOK,
            token1: WETH,
            zfo: true,
            family: LaneFamily::V2 {
                fees: degenbot_bot::bot_core::executor_hop::V2FeePair::from_discovered(
                    Some(3),
                    Some(3),
                    Some(1_000),
                )
                .resolve()
                .unwrap(),
            },
        },
    ];
    BackrunEvaluated {
        stats: degenbot_strategy::backrun_strategy::SolveStats {
            best: Some(LaneCandidate {
                path_id: 17,
                hops,
                optimal_input: 123,
                hop_outputs: vec![5_893_000, 1_235],
                consumed_inputs: vec![123, 5_892_315],
                profit,
            }),
            ..Default::default()
        },
    }
}

struct Harness {
    strategy: BackrunStrategy,
    pipeline: PipelineConfig,
    knobs: degenbot_strategy::BackrunConfig,
}

impl Harness {
    fn new(fixture_mode: bool, wallet_gas_cost_wei: u64) -> Self {
        let execution = ExecutionContext::ethereum(EXECUTOR);
        let pipeline = PipelineConfig {
            execution,
            owner: Address::ZERO,
            bribe_bips: 9_800,
            wallet_gas_cost_wei: Arc::new(AtomicU64::new(wallet_gas_cost_wei)),
            gas_floor_wei: U256::ZERO,
            fixture_mode,
        };
        let mut knobs =
            MevblockerBackrun::from_config(&degenbot_config::BotConfig::default(), String::new())
                .into_config();
        knobs.bid_mode = true;
        knobs.budget_wei = U256::from(1_000_000_000u64);
        knobs.max_bundle_wei = U256::from(1_000_000_000u64);
        knobs.stop_file = std::path::PathBuf::from("/nonexistent");
        Self {
            strategy: BackrunStrategy::new(pipeline.execution),
            pipeline,
            knobs,
        }
    }

    /// The full production path into the gate: compose the ceiling artifact,
    /// then hand the gate the composed intent, the simulation verdict, and
    /// the spent budget.
    fn decide(&mut self, evaluated: &BackrunEvaluated, sim_ok: bool, spent: U256) -> Decided {
        let mut stage_trace = StageTrace::default();
        let mut cx = degenbot_strategy::pending_tx::FrameContext {
            pl: &self.pipeline,
            knobs: &self.knobs,
            head: 0,
            trace_tx: "0xdecide-gate",
            trace: &mut stage_trace,
        };
        let composed = self.strategy.compose(&mut cx, evaluated);
        self.strategy.decide(
            &mut cx,
            &GateInput {
                evaluated,
                composed: composed.as_ref(),
                sim_ok,
                spent,
            },
        )
    }
}

/// A bundle-sim rejection of a composed candidate must read as exactly that:
/// the sim gate failed. Masking it as "no candidate" hid real rejections.
#[test]
fn failed_sim_on_a_composed_candidate_observes_sim_gate_failed() {
    let mut h = Harness::new(false, 100_000_000);
    let evaluated = evaluated_with_profit(1_000_000_000);
    let decided = h.decide(&evaluated, false, U256::ZERO);
    assert_eq!(
        decided.decision,
        Decision::Observe {
            reason: "sim_gate_failed"
        }
    );
    assert_eq!(decided.requested_bid, U256::ZERO);
    assert!(decided.submit_calldata.is_none());
}

#[test]
fn observe_only_when_bid_mode_is_off() {
    let mut h = Harness::new(false, 100_000_000);
    h.knobs.bid_mode = false;
    let evaluated = evaluated_with_profit(1_000_000_000);
    let decided = h.decide(&evaluated, true, U256::ZERO);
    assert_eq!(
        decided.decision,
        Decision::Observe {
            reason: "observe_only"
        }
    );
}

/// Bid-mode legality is flag AND budget: a zero budget observes, never bids.
#[test]
fn observe_only_when_budget_is_zero() {
    let mut h = Harness::new(false, 100_000_000);
    h.knobs.budget_wei = U256::ZERO;
    let evaluated = evaluated_with_profit(1_000_000_000);
    let decided = h.decide(&evaluated, true, U256::ZERO);
    assert_eq!(
        decided.decision,
        Decision::Observe {
            reason: "observe_only"
        }
    );
}

#[test]
fn exhausted_budget_observes_budget_exhausted() {
    let mut h = Harness::new(false, 100_000_000);
    let evaluated = evaluated_with_profit(1_000_000_000);
    // The wallet-true bid for 1 ETH gross is 895 mwei; 600 mwei already spent
    // breaches the 1 ETH budget.
    let decided = h.decide(&evaluated, true, U256::from(600_000_000u64));
    assert_eq!(
        decided.decision,
        Decision::Observe {
            reason: "budget_exhausted"
        }
    );
}

#[test]
fn all_gates_pass_bid_is_capped_at_max_bundle() {
    let mut h = Harness::new(false, 100_000_000);
    h.knobs.max_bundle_wei = U256::from(500_000_000u64);
    let evaluated = evaluated_with_profit(1_000_000_000);
    let decided = h.decide(&evaluated, true, U256::ZERO);
    assert_eq!(
        decided.decision,
        Decision::Bid {
            bid_wei: U256::from(500_000_000u64)
        }
    );
    assert!(decided.submit_calldata.is_some());
    assert!(decided.economics.is_some());
}

/// The kill switch outranks every economics arm: a present stop file drops
/// the frame before any bid is sized.
#[test]
fn kill_switch_drops_the_frame_before_any_bid() {
    let stop = std::env::temp_dir().join("degenbot-decide-gate-kill-switch");
    std::fs::write(&stop, b"halt").unwrap();
    let mut h = Harness::new(false, 100_000_000);
    h.knobs.stop_file = stop.clone();
    let evaluated = evaluated_with_profit(1_000_000_000);
    let decided = h.decide(&evaluated, true, U256::ZERO);
    assert_eq!(
        decided.decision,
        Decision::Drop {
            reason: "kill_switch"
        }
    );
    std::fs::remove_file(&stop).unwrap();
}

/// A gross that cannot cover the wallet's gas plus margin never bids: the
/// frame observes as net-unprofitable, not as a sim or budget refusal.
#[test]
fn unprofitable_net_observes_net_after_gas() {
    let mut h = Harness::new(false, 2_000_000_000);
    let evaluated = evaluated_with_profit(1_000_000_000);
    let decided = h.decide(&evaluated, true, U256::ZERO);
    assert_eq!(
        decided.decision,
        Decision::Observe {
            reason: "net_after_gas_unprofitable"
        }
    );
}

/// Fixture mode skips the live bundle sim by design; the label says so
/// instead of inventing a sim verdict.
#[test]
fn fixture_mode_observes_sim_skipped() {
    let mut h = Harness::new(true, 100_000_000);
    let evaluated = evaluated_with_profit(1_000_000_000);
    let decided = h.decide(&evaluated, false, U256::ZERO);
    assert_eq!(
        decided.decision,
        Decision::Observe {
            reason: "sim_skipped_fixture_mode"
        }
    );
}

/// A frame with no solved candidate observes honestly as no-candidate —
/// distinct from a sim rejection or a budget refusal.
#[test]
fn no_solved_candidate_observes_no_candidate() {
    let mut h = Harness::new(false, 100_000_000);
    let evaluated = BackrunEvaluated {
        stats: degenbot_strategy::backrun_strategy::SolveStats::default(),
    };
    let decided = h.decide(&evaluated, false, U256::ZERO);
    assert_eq!(
        decided.decision,
        Decision::Observe {
            reason: "no_candidate"
        }
    );
}
