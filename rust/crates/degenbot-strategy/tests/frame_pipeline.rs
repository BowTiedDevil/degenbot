//! Frame-pipeline tests (task WKPZQK).
//!
//! 1. The golden frame (offline): a hand-built replay journal (post-target
//!    V2 reserves at slot 8) flows descriptors → `extract_pool_post_states`
//!    → workspace admission → 2-hop discovery/solve → compose, matching the
//!    hand-derived golden reference.
//! 2. The live e2e (`#[ignore]`-gated): a captured frame JSONL replays
//!    end-to-end against a forked node.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::items_after_statements
)]

use alloy::primitives::{address, aliases::U112, Address, B256, U256};
use degenbot_db::connection::DegenbotDb;
use degenbot_pools::slot_layout;
use degenbot_pools::v4_storage_slots::{
    encode_v4_liquidity_slot, encode_v4_slot0, v4_liquidity_slot, v4_pool_state_base_slot,
    v4_slot0_slot, V4Slot0Parts,
};
use degenbot_simulation::sim::evm::frame_replay::{BaseFeeSource, ReplayOutcome, ReplayStatus};
use degenbot_simulation::sim::evm::journal_pools::{
    PoolFamily, PoolPostKind, TypedPoolPost, V4PoolDescriptor,
};
use std::sync::Arc;

use degenbot_bot::bot_core::executor_hop::{V2FeePair, V2Fees};
use degenbot_bot::connector_index::{V2ConnectorIndex, V2Edge};
use degenbot_pathfinding::PoolKind;
use degenbot_strategy::anchored_dfs::{AnchorPool, AnchoredGraph};
use degenbot_strategy::backrun_engine::{BackrunHopRef, BackrunSolver, BackrunV2Pool, LaneFamily};
use degenbot_strategy::backrun_strategy::{
    admit_extracted, backrun_encode_options, cycle_refs, cycle_touched_legs,
    discover_trace_payload, net_bid, solve_dfs_chains, BackrunIntents, CycleHop,
};
use degenbot_strategy::cmd_executor_adapter::{CmdExecutorAdapter, CmdExecutorOutcome};
use degenbot_strategy::execution_context::{
    ExecutionContext, ETHEREUM_V4_POOL_MANAGER, ETHEREUM_WETH as WETH,
};
use degenbot_strategy::frame_pipeline::{
    build_descriptors, empty_frame_observe_reason, state_digest, MarketContext, PipelineConfig,
};
use degenbot_strategy::project_candidate;

fn v2_fee_pair() -> V2FeePair {
    V2FeePair::from_discovered(Some(3), Some(3), Some(1_000))
}

fn v2_fees() -> V2Fees {
    v2_fee_pair().resolve().expect("valid fixture fee")
}

fn test_execution() -> ExecutionContext {
    ExecutionContext::ethereum(address!("00000000000000000000000000000000000000e1"))
}

/// The public adapter and frame-descriptor seams consume one session deployment
/// value; neither reconstructs a manager identity from the touched frame.
#[test]
fn one_execution_context_names_the_authoritative_v4_manager() {
    let executor = address!("00000000000000000000000000000000000000e1");
    let execution = ExecutionContext::new(executor, ETHEREUM_V4_POOL_MANAGER, WETH);
    let adapter = CmdExecutorAdapter::new(execution);

    let descriptors =
        build_descriptors(None, &[(ETHEREUM_V4_POOL_MANAGER, Vec::new())], &execution);

    assert_eq!(adapter.context(), &execution);
    assert!(descriptors.hit_v4);
    assert!(matches!(
        descriptors.by_address.get(&ETHEREUM_V4_POOL_MANAGER),
        Some(PoolFamily::V4PoolManager { pools }) if pools.is_empty()
    ));
}

/// Test stand-in for the Db→head backfill transport. The fixtures stamp no
/// `liquidity_update_block`, so no window is ever backfilled; an unexpected
/// fetch declines loudly rather than staging stale state.
struct NoBackfill;

impl degenbot_bot::bot_core::pool_ingress::LiquidityLogSource for NoBackfill {
    fn fetch_v3_liquidity_events(
        &self,
        _pool: alloy::primitives::Address,
        _from: u64,
        _to: u64,
    ) -> Result<Vec<degenbot_db::LiquidityUpdateEvent>, String> {
        Err("this fixture wires no backfill transport".into())
    }

    fn fetch_v4_liquidity_events(
        &self,
        _manager: alloy::primitives::Address,
        _pool_id: alloy::primitives::B256,
        _from: u64,
        _to: u64,
    ) -> Result<Vec<degenbot_db::LiquidityUpdateEvent>, String> {
        Err("this fixture wires no backfill transport".into())
    }
}

fn market_context(
    registry: Option<std::sync::Arc<degenbot_bot::bot_core::RouteRegistry>>,
    db: Option<std::sync::Arc<degenbot_db::connection::DegenbotDb>>,
) -> MarketContext {
    let db_arm = db.clone().map(|db| {
        degenbot_bot::bot_core::pool_ingress::DbArm::new(db, std::sync::Arc::new(NoBackfill))
    });
    let kit = degenbot_strategy::strategy_kit::StrategyKit::resolve(
        registry,
        db_arm,
        None,
        degenbot_bot::bot_core::pool_ingress::VerifyLevel::default(),
        None,
    );
    MarketContext::new(1, db, kit, 8, 4)
}

use degenbot_strategy::pending_tx::PendingTxReaction;
use revm::state::{Account, AccountStatus, EvmState, EvmStorageSlot};

// ─────────────────── the golden frame (offline, end to end) ────────────────

const TOK: Address = address!("0000000000000000000000000000000000000aa1");
/// P: the affected pool (the target swapped 50 WETH in, TOK -> WETH cycle).
const P: Address = address!("000000000000000000000000000000000000b001");
/// Q: the discovery connector the fan admits (cheaper TOK than staged P).
const Q: Address = address!("000000000000000000000000000000000000b002");
const SEED: u64 = 7;

/// The replayed journal of ONE V2 swap frame: post-target reserves packed at
/// the pair's slot 8 (`476_259` TOK / `1_050` WETH — the golden target's 50-WETH
/// swap settled), touched + status flag set.
fn golden_replay_outcome() -> ReplayOutcome {
    let reserves = slot_layout::pack_v2_reserves_word(U112::from(476_259u64), U112::from(1_050u64));
    let slot8 = U256::from_be_bytes(reserves.0);
    let mut state = EvmState::default();
    let mut account = Account::default();
    account.status = AccountStatus::Touched;
    account.storage.insert(
        U256::from(slot_layout::V2_RESERVES_SLOT),
        EvmStorageSlot {
            original_value: U256::ZERO,
            present_value: slot8,
            transaction_id: revm::state::TransactionId::ZERO,
            is_cold: false,
        },
    );
    state.insert(P, account);
    ReplayOutcome {
        status: ReplayStatus::Success,
        state,
        touched: vec![(P, vec![U256::from(slot_layout::V2_RESERVES_SLOT)])],
        rpc_reads: 0,
        wall: std::time::Duration::from_micros(42),
        base_fee_source: BaseFeeSource::Projected,
    }
}

/// The in-memory runtime fixture: seeded DB (TOK/WETH ids) + index edges for
/// P and Q in canonical (token0 = TOK) order.
fn runtime_fixture() -> (MarketContext, u64, u64) {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    let tok_id = db
        .get_or_create_erc20_token(1, &TOK.to_checksum(None), None, None, None)
        .unwrap();
    let weth_id = db
        .get_or_create_erc20_token(1, &WETH.to_checksum(None), None, None, None)
        .unwrap();
    let mut index = degenbot_bot::connector_index::V2ConnectorIndex::default();
    index.push_edge(degenbot_bot::connector_index::V2Edge {
        pool_id: 101,
        token0_id: u64::try_from(tok_id).unwrap(),
        token1_id: u64::try_from(weth_id).unwrap(),
        address: P,
        fees: v2_fee_pair(),
    });
    index.push_edge(degenbot_bot::connector_index::V2Edge {
        pool_id: 102,
        token0_id: u64::try_from(tok_id).unwrap(),
        token1_id: u64::try_from(weth_id).unwrap(),
        address: Q,
        fees: v2_fee_pair(),
    });
    // The pipeline's sidecars quote chains of 1 (mainnet).
    (
        market_context(
            Some(std::sync::Arc::new(
                degenbot_bot::bot_core::RouteRegistry::new(index),
            )),
            Some(std::sync::Arc::new(db)),
        ),
        u64::try_from(tok_id).unwrap(),
        u64::try_from(weth_id).unwrap(),
    )
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "fixture plumbing reads top-to-bottom: extract, admit, refs by hand, solve, compose, digest"
)]
fn golden_frame_extract_admit_solve_compose_end_to_end() {
    let (rt, _tok_id, _weth_id) = runtime_fixture();
    let outcome = golden_replay_outcome();

    // descriptors: projected from the connector index (the tracked registry).
    let descriptors = build_descriptors(rt.index(), &outcome.touched, &test_execution());
    assert!(
        !descriptors.hit_v4,
        "an all-V2 frame touches no PoolManager"
    );
    assert_eq!(
        descriptors.by_address.len(),
        1,
        "only the tracked pool gets a descriptor"
    );

    // extract: the journalled slot-8 word decodes to the post-target pair.
    let extracted = degenbot_simulation::sim::evm::journal_pools::extract_pool_post_states(
        &outcome,
        &descriptors.by_address,
    );
    assert_eq!(extracted.len(), 1, "one touched tracked pool extracted");
    let degenbot_simulation::sim::evm::journal_pools::PoolPostState {
        address,
        family,
        kind,
    } = &extracted[0];
    assert_eq!(*address, P);
    assert!(matches!(
        family,
        degenbot_simulation::sim::evm::journal_pools::PoolFamily::V2Pair
    ));
    let reserves = match kind {
        degenbot_simulation::sim::evm::journal_pools::PoolPostKind::Typed(
            degenbot_simulation::sim::evm::journal_pools::TypedPoolPost::V2 { reserves },
        ) => reserves,
        other => panic!("expected typed V2 post-state, got {other:?}"),
    };
    assert_eq!(
        reserves.reserve0,
        U112::from(476_259u64),
        "TOK side drained"
    );
    assert_eq!(reserves.reserve1, U112::from(1_050u64), "WETH side grew");

    // admit into THIS frame's fresh workspace scope.
    let mut solver = BackrunSolver::new();
    let affected = admit_extracted(&rt, &mut solver, &extracted, SEED, "0xfixture", None);
    assert_eq!(affected.len(), 1, "P admits and trades WETH");
    assert_eq!(affected[0].address, P);
    assert_eq!(
        affected[0].family.tag(),
        LaneFamily::V2 { fees: v2_fees() }.tag()
    );

    // Admit the connector the walker would have discovered (the admission
    // shape is unchanged; the discovery traversal itself is exercised in the
    // live e2e).
    let q_id = solver
        .admit_v2(&BackrunV2Pool {
            address: Q,
            token0: affected[0].token0,
            token1: affected[0].token1,
            reserve0: 200_000,
            reserve1: 900,
            fees: v2_fee_pair(),
        })
        .expect("connector admits");

    // solve: both drift cycles declared + evaluated; the golden profit. The
    // refs replicate the anchored lane's re-derived traversal exacts (anchor
    // consumes WETH, mid consumes TOK) — the 2-hop walker output verbatim.
    let (t0, t1) = (affected[0].token0, affected[0].token1);
    let tok = if t0 == WETH { t1 } else { t0 };
    let anchor_consuming_weth = BackrunHopRef {
        pool_id: affected[0].workspace_pool_id,
        pool: P,
        token0: t0,
        token1: t1,
        zfo: t0 == WETH,
        family: LaneFamily::V2 { fees: v2_fees() },
    };
    let mid_consuming_tok = BackrunHopRef {
        pool_id: q_id,
        pool: Q,
        token0: t0,
        token1: t1,
        zfo: tok == t0,
        family: LaneFamily::V2 { fees: v2_fees() },
    };
    let mid_consuming_weth = BackrunHopRef {
        pool_id: q_id,
        pool: Q,
        token0: t0,
        token1: t1,
        zfo: t0 == WETH,
        family: LaneFamily::V2 { fees: v2_fees() },
    };
    let anchor_consuming_tok = BackrunHopRef {
        pool_id: affected[0].workspace_pool_id,
        pool: P,
        token0: t0,
        token1: t1,
        zfo: tok == t0,
        family: LaneFamily::V2 { fees: v2_fees() },
    };
    let stats = solve_dfs_chains(
        &mut solver,
        &[
            vec![anchor_consuming_weth, mid_consuming_tok],
            vec![mid_consuming_weth, anchor_consuming_tok],
        ],
        U256::ZERO,
    );
    assert_eq!(stats.dfs_declared, 2, "both drift directions proposed");
    assert!(
        stats.dfs_evaluated >= 1,
        "at least the profitable direction solves (fee-drain direction may not)"
    );
    let best = stats.best.expect("the golden cycle profits");
    assert!(
        U256::from(best.profit) >= U256::from(55u8) && U256::from(best.profit) <= U256::from(56u8),
        "profit {} outside the golden plateau [55,56]",
        best.profit
    );
    assert!(
        U256::from(best.optimal_input) > U256::from(100u8)
            && U256::from(best.optimal_input) < U256::from(150u8),
        "optimal input {} far from the golden 123",
        best.optimal_input
    );

    // compose: the executable artifact builds from the solved hops.
    let (path, result) = project_candidate(&best);
    let CmdExecutorOutcome::Encoded(cd) = CmdExecutorAdapter::new(ExecutionContext::new(
        address!("0x30b28ed8aa581fbc0191c3b532b0697773070e97"),
        address!("000000000004444c5dc75cb358380d2e3de08a90"),
        WETH,
    ))
    .compose(&path, &result, backrun_encode_options(9_800)) else {
        panic!("the 2-hop V2 candidate composes")
    };
    assert!(cd.len() > 4 + 32 * 3 + 64, "execute() calldata shape");

    // digest evidence for the JSONL extract trace.
    let digest = state_digest(&extracted[0]);
    assert!(digest.starts_with("0x"), "digest {digest}");
}

/// The generalized WETH-entry intake reproduces the committed two-hop
/// traversal: `cycle_refs` on the same resolved pools yields the same hop list
/// (per-hop `zfo` + canonical token order) as the committed anchor+mid
/// construction, and the solver's best candidate is identical.
#[test]
fn weth_entry_cycle_refs_reproduce_the_committed_two_hop_traversal() {
    let (rt, _tok_id, _weth_id) = runtime_fixture();
    let outcome = golden_replay_outcome();
    let descriptors = build_descriptors(rt.index(), &outcome.touched, &test_execution());
    let extracted = degenbot_simulation::sim::evm::journal_pools::extract_pool_post_states(
        &outcome,
        &descriptors.by_address,
    );
    let mut solver = BackrunSolver::new();
    let affected = admit_extracted(&rt, &mut solver, &extracted, SEED, "0xparity", None);
    assert_eq!(affected.len(), 1);
    let q_id = solver
        .admit_v2(&BackrunV2Pool {
            address: Q,
            token0: affected[0].token0,
            token1: affected[0].token1,
            reserve0: 200_000,
            reserve1: 900,
            fees: v2_fee_pair(),
        })
        .expect("connector admits");

    let a = &affected[0];
    let hops = [
        CycleHop {
            workspace_pool_id: a.workspace_pool_id,
            pool: P,
            token0: a.token0,
            token1: a.token1,
            family: a.family,
        },
        CycleHop {
            workspace_pool_id: q_id,
            pool: Q,
            token0: a.token0,
            token1: a.token1,
            family: LaneFamily::V2 { fees: v2_fees() },
        },
    ];
    let new_chain = cycle_refs(&hops, WETH).expect("the WETH-entry two-hop cycle closes");

    // The committed anchor/mid refs, verbatim.
    let (t0, t1) = (a.token0, a.token1);
    let tok = if t0 == WETH { t1 } else { t0 };
    let committed = vec![
        BackrunHopRef {
            pool_id: a.workspace_pool_id,
            pool: P,
            token0: t0,
            token1: t1,
            zfo: t0 == WETH,
            family: LaneFamily::V2 { fees: v2_fees() },
        },
        BackrunHopRef {
            pool_id: q_id,
            pool: Q,
            token0: t0,
            token1: t1,
            zfo: tok == t0,
            family: LaneFamily::V2 { fees: v2_fees() },
        },
    ];
    assert_eq!(
        new_chain, committed,
        "the rotated intake must reproduce the committed traversal"
    );

    let new_stats = solve_dfs_chains(&mut solver, std::slice::from_ref(&new_chain), U256::ZERO);
    let committed_stats =
        solve_dfs_chains(&mut solver, std::slice::from_ref(&committed), U256::ZERO);
    let new_best = new_stats.best.expect("the rotated cycle solves");
    let committed_best = committed_stats.best.expect("the committed cycle solves");
    assert_eq!(new_best.hops, committed_best.hops);
    assert_eq!(new_best.optimal_input, committed_best.optimal_input);
    assert_eq!(new_best.hop_outputs, committed_best.hop_outputs);
    assert_eq!(new_best.consumed_inputs, committed_best.consumed_inputs);
    assert_eq!(new_best.profit, committed_best.profit);
    assert_ne!(new_best.path_id, committed_best.path_id);
    let (new_path, new_result) = project_candidate(&new_best);
    let (committed_path, committed_result) = project_candidate(&committed_best);
    assert_eq!(new_path.hops.len(), committed_path.hops.len());
    assert_eq!(new_result.hop_descriptors, committed_result.hop_descriptors);
    assert_eq!(new_result.optimal_input, committed_result.optimal_input);
    assert_eq!(new_result.hop_outputs, committed_result.hop_outputs);
    assert_eq!(new_result.consumed_inputs, committed_result.consumed_inputs);
    assert_eq!(new_result.path_id, new_best.path_id);
    assert_eq!(committed_result.path_id, committed_best.path_id);
}

/// Touched-set discovery: a touched pool that never quotes WETH rides a
/// WETH-entry 4-hop cycle as a mid hop. The frame trace must carry the
/// effective cycle cap, the frame's pin count, each admitted cycle's
/// multi-touched verdict, and each solved chain's touched-leg count. The
/// discover stage needs the production scratch stack, so its payload is
/// asserted at the `discover_trace_payload` boundary; the solve stage runs
/// end-to-end through `evaluate` and its JSONL line is read back.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one frame reads top-to-bottom: admission, payload, solve"
)]
fn touched_set_trace_reports_cap_pins_and_multi_touched() {
    const WETH_ID: u64 = 20;
    const A_ID: u64 = 10;
    const B_ID: u64 = 30;
    const C_ID: u64 = 40;
    const P1: Address = address!("000000000000000000000000000000000000c001");
    const P2: Address = address!("000000000000000000000000000000000000c002");
    const P3: Address = address!("000000000000000000000000000000000000c003");
    const P4: Address = address!("000000000000000000000000000000000000c004");
    const A: Address = address!("0000000000000000000000000000000000000a01");
    const B: Address = address!("0000000000000000000000000000000000000a02");
    const C: Address = address!("0000000000000000000000000000000000000a03");

    let mut index = V2ConnectorIndex::default();
    index.push_edge(V2Edge {
        pool_id: 201,
        token0_id: A_ID,
        token1_id: WETH_ID,
        address: P1,
        fees: v2_fee_pair(),
    });
    index.push_edge(V2Edge {
        pool_id: 202,
        token0_id: A_ID,
        token1_id: B_ID,
        address: P2,
        fees: v2_fee_pair(),
    });
    index.push_edge(V2Edge {
        pool_id: 203,
        token0_id: B_ID,
        token1_id: C_ID,
        address: P3,
        fees: v2_fee_pair(),
    });
    index.push_edge(V2Edge {
        pool_id: 204,
        token0_id: C_ID,
        token1_id: WETH_ID,
        address: P4,
        fees: v2_fee_pair(),
    });
    let graph = AnchoredGraph::from_connector_index(&index);

    // The frame's only touched pool trades A/B and never quotes WETH.
    let touched = vec![AnchorPool {
        pool_id: 202,
        pool_kind: PoolKind::V2,
        token_a_id: A_ID,
        token_b_id: B_ID,
    }];
    let (cycles, refused) = graph.weth_entry_cycles(&touched, WETH_ID, 16, 4);
    assert_eq!(refused, 0, "a WETH-closing rotation exists: {cycles:?}");

    let four_hop = cycles
        .iter()
        .find(|c| c.pools.len() == 4 && c.pools.iter().any(|(id, _)| *id == 202))
        .expect("the 4-hop WETH-entry cycle over the touched mid hop");
    assert_eq!(
        four_hop.entry_token_id, WETH_ID,
        "the cycle is rotated to its WETH stake entry"
    );
    assert_eq!(
        cycle_touched_legs(four_hop, &touched),
        1,
        "only the pinned mid hop is touched"
    );
    assert_eq!(
        cycles
            .iter()
            .filter(|c| cycle_touched_legs(c, &touched) > 1)
            .count(),
        0,
        "no admitted cycle carries two pins"
    );

    let discover = discover_trace_payload(
        "0xtouched",
        cycles.len(),
        cycles.len(),
        1,
        0,
        1,
        refused,
        false,
        4,
        &touched,
        &cycles,
    );
    assert_eq!(
        discover["touched_pools"].as_u64(),
        Some(1),
        "one pin this frame: {discover}"
    );
    assert_eq!(
        discover["cycle_max_hops"].as_u64(),
        Some(4),
        "the effective cap is the configured one: {discover}"
    );
    assert_eq!(discover["cycles_with_multi_touched"].as_u64(), Some(0));
    assert_eq!(discover["non_weth_cycles"].as_u64(), Some(0));
    assert_eq!(discover["cycle_reject"], "non_weth_cycle");

    // The solve stage runs end-to-end; the trace capture lands in a temp file.
    let trace_path = std::env::temp_dir().join("yd5cqx-touched-set-trace.jsonl");
    let _ = std::fs::remove_file(&trace_path);
    let mut boot = degenbot_config::BotConfig::default();
    boot.logging.trace_jsonl = Some(trace_path.clone());
    let _ = degenbot_config::holder::install(Arc::new(boot));

    let mut solver = BackrunSolver::new();
    let p1_id = admitted_pair(&mut solver, P1, A, WETH, 2_000_000, 1_000_000);
    let p2_id = admitted_pair(&mut solver, P2, A, B, 2_000_000, 1_800_000);
    let p3_id = admitted_pair(&mut solver, P3, B, C, 1_800_000, 1_700_000);
    let p4_id = admitted_pair(&mut solver, P4, C, WETH, 1_700_000, 1_000_000);
    let hops = vec![
        CycleHop {
            workspace_pool_id: p1_id,
            pool: P1,
            token0: A,
            token1: WETH,
            family: LaneFamily::V2 { fees: v2_fees() },
        },
        CycleHop {
            workspace_pool_id: p2_id,
            pool: P2,
            token0: A,
            token1: B,
            family: LaneFamily::V2 { fees: v2_fees() },
        },
        CycleHop {
            workspace_pool_id: p3_id,
            pool: P3,
            token0: B,
            token1: C,
            family: LaneFamily::V2 { fees: v2_fees() },
        },
        CycleHop {
            workspace_pool_id: p4_id,
            pool: P4,
            token0: C,
            token1: WETH,
            family: LaneFamily::V2 { fees: v2_fees() },
        },
    ];
    let chain = cycle_refs(&hops, WETH).expect("the WETH-entry 4-hop cycle closes");
    assert_eq!(chain.len(), 4);

    let execution = test_execution();
    let pl = PipelineConfig {
        execution,
        owner: address!("00000000000000000000000000000000000000e2"),
        bribe_bips: 9_800,
        wallet_gas_cost_wei: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        gas_floor_wei: U256::ZERO,
        fixture_mode: false,
    };
    let mut strategy = degenbot_strategy::backrun_strategy::BackrunStrategy::new(execution);
    let intents = BackrunIntents {
        chains: vec![chain],
        touched_legs: vec![1],
        non_base_quote_dropped: false,
        bailed: false,
    };
    let _ = strategy.evaluate(&mut solver, intents, &pl, "0xtouched");

    let traced = std::fs::read_to_string(&trace_path).expect("trace file was written");
    let solve = traced
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("trace line parses"))
        .find(|v| v["kind"] == "solve")
        .expect("the solve event was emitted");
    let chain_trace = &solve["chains"][0];
    assert_eq!(
        chain_trace["pools"].as_array().map(Vec::len),
        Some(4),
        "the solved chain carries all 4 pools: {solve}"
    );
    assert_eq!(
        chain_trace["touched_legs"].as_u64(),
        Some(1),
        "the chain carries one touched leg: {solve}"
    );
}

// ─────────────── per-frame quote selection (the USDC frame) ────────────────

/// Real mainnet USDC — the non-WETH settlement quote this section exercises.
const USDC: Address = address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
/// P2: the USDC frame's affected pool (canonical token0 = TOK: 0x…0aa1).
const P2: Address = address!("000000000000000000000000000000000000b003");

/// The replayed journal of ONE USDC-frame V2 swap: post-target reserves
/// packed at the pair's slot 8 (`900_000` TOK / `1_100` USDC settled).
fn usdc_frame_replay_outcome() -> ReplayOutcome {
    let reserves = slot_layout::pack_v2_reserves_word(U112::from(900_000u64), U112::from(1_100u64));
    let slot8 = U256::from_be_bytes(reserves.0);
    let mut state = EvmState::default();
    let mut account = Account::default();
    account.status = AccountStatus::Touched;
    account.storage.insert(
        U256::from(slot_layout::V2_RESERVES_SLOT),
        EvmStorageSlot {
            original_value: U256::ZERO,
            present_value: slot8,
            transaction_id: revm::state::TransactionId::ZERO,
            is_cold: false,
        },
    );
    state.insert(P2, account);
    ReplayOutcome {
        status: ReplayStatus::Success,
        state,
        touched: vec![(P2, vec![U256::from(slot_layout::V2_RESERVES_SLOT)])],
        rpc_reads: 0,
        wall: std::time::Duration::from_micros(42),
        base_fee_source: BaseFeeSource::Projected,
    }
}

/// USDC-quoted affected pool admits with its quote orientation. The frame
/// moving a USDC-quoted pair previously produced NO admitted pool at all
/// (the WETH-only admission cut it) — the per-frame quote set is {WETH,
/// USDC, USDT}, so this frame's settlement quote is USDC.
#[test]
fn usdc_quoted_pair_admits_with_quote_orientation() {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    let tok_id = db
        .get_or_create_erc20_token(1, &TOK.to_checksum(None), None, None, None)
        .unwrap();
    let weth_id = db
        .get_or_create_erc20_token(1, &WETH.to_checksum(None), None, None, None)
        .unwrap();
    let usdc_id = db
        .get_or_create_erc20_token(1, &USDC.to_checksum(None), None, None, None)
        .unwrap();
    let (tok_id, weth_id, usdc_id) = (
        u64::try_from(tok_id).unwrap(),
        u64::try_from(weth_id).unwrap(),
        u64::try_from(usdc_id).unwrap(),
    );
    let mut index = degenbot_bot::connector_index::V2ConnectorIndex::default();
    // P2 + Q2 trade (TOK, USDC); the normalizer edges trade (USDC, WETH) so
    // the quote actually connects back to the base quote in the index.
    for (pool_id, t0, t1, addr) in [
        (103u64, tok_id, usdc_id, P2),
        (104u64, tok_id, usdc_id, Q),
        (105u64, usdc_id, weth_id, P),
    ] {
        index.push_edge(degenbot_bot::connector_index::V2Edge {
            pool_id,
            token0_id: t0,
            token1_id: t1,
            address: addr,
            fees: v2_fee_pair(),
        });
    }
    let rt = market_context(
        Some(std::sync::Arc::new(
            degenbot_bot::bot_core::RouteRegistry::new(index),
        )),
        Some(std::sync::Arc::new(db)),
    );
    let outcome = usdc_frame_replay_outcome();

    let descriptors = build_descriptors(rt.index(), &outcome.touched, &test_execution());
    let extracted = degenbot_simulation::sim::evm::journal_pools::extract_pool_post_states(
        &outcome,
        &descriptors.by_address,
    );
    assert_eq!(extracted.len(), 1);
    let mut solver = BackrunSolver::new();
    let affected = admit_extracted(&rt, &mut solver, &extracted, SEED, "0xfixture", None);
    // The WETH-only admission cut this frame short: `affected` was empty, so
    // no quote-land discovery could ever start for a USDC-quoted pair.
    assert_eq!(
        affected.len(),
        1,
        "the USDC-quoted pair admits into the frame scope"
    );
    let quotes = &affected[0].quotes;
    assert_eq!(quotes.len(), 1, "one supported quote: USDC");
    assert_eq!(quotes[0].quote, USDC);
    assert_eq!(quotes[0].quote_id, usdc_id);
    assert_eq!(quotes[0].tok_id, tok_id);
    assert!(
        quotes.iter().all(|q| q.quote != WETH),
        "WETH-only selection yields no quote for this frame"
    );
}

/// ADR-059 D8: a touched address present in the DB under a family the backrun
/// arm cannot type observes the closed `family-unsupported` reason (the kind
/// rides the JSONL detail) instead of dropping into an unexplained
/// no-candidate. Positive path: a known V2 address still types as `V2Pair`.
#[test]
fn unsupported_family_observes_loudly_not_silently() {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    let tok_id = db
        .get_or_create_erc20_token(1, &TOK.to_checksum(None), None, None, None)
        .unwrap();
    let weth_id = db
        .get_or_create_erc20_token(1, &WETH.to_checksum(None), None, None, None)
        .unwrap();
    let (tok_id, weth_id) = (
        u64::try_from(tok_id).unwrap(),
        u64::try_from(weth_id).unwrap(),
    );

    const LFJ: Address = address!("000000000000000000000000000000000000c001");
    const LFJ_MANAGER: Address = address!("000000000000000000000000000000000000c002");
    const V4_MANAGER: Address = address!("000000000000000000000000000000000000c003");
    {
        let conn = db.lock();
        conn.execute_batch(&format!(
            "PRAGMA foreign_keys=OFF;
             INSERT INTO exchanges (id, chain_id, name, active, factory) VALUES
               (1, 1, 'test', 1, '0x0000000000000000000000000000000000000001');
             INSERT INTO pools (id, address, chain, kind, token0_id, token1_id, exchange_id) VALUES
               (901, '{lfj}', 1, 'lfj_binned', {t0}, {t1}, 1),
               (902, '{p}', 1, 'uniswap_v2', {t0}, {t1}, 1);
             INSERT INTO pool_managers (id, address, chain, kind, state_view, exchange_id) VALUES
               (901, '{lfj_mgr}', 1, 'lfj_binned', NULL, 1),
               (902, '{v4_mgr}', 1, 'uniswap_v4', NULL, 1);
             INSERT INTO managed_pools (id, kind, manager_id) VALUES
               (901, 'lfj_binned', 901),
               (902, 'uniswap_v4', 902);",
            lfj = LFJ.to_checksum(None),
            p = P.to_checksum(None),
            lfj_mgr = LFJ_MANAGER.to_checksum(None),
            v4_mgr = V4_MANAGER.to_checksum(None),
            t0 = tok_id,
            t1 = weth_id,
        ))
        .unwrap();
    }

    let mut index = degenbot_bot::connector_index::V2ConnectorIndex::default();
    index.push_edge(degenbot_bot::connector_index::V2Edge {
        pool_id: 101,
        token0_id: tok_id,
        token1_id: weth_id,
        address: P,
        fees: v2_fee_pair(),
    });
    index.load_unsupported(&db, 1).unwrap();
    assert_eq!(
        index.unsupported_kind(LFJ),
        Some("lfj_binned"),
        "a `pools`-table unsupported kind is keyed by the pool address"
    );
    assert_eq!(
        index.unsupported_kind(LFJ_MANAGER),
        Some("lfj_binned"),
        "a managed unsupported family is keyed by its MANAGER address"
    );
    assert!(
        index.unsupported_kind(P).is_none(),
        "a supported V2 pool is not an unsupported family"
    );
    assert!(
        index.unsupported_kind(V4_MANAGER).is_none(),
        "uniswap_v4 is supported and must not appear"
    );

    let descriptors = build_descriptors(
        Some(&index),
        &[(P, Vec::new()), (LFJ, Vec::new())],
        &test_execution(),
    );
    assert!(
        matches!(
            descriptors.by_address.get(&P),
            Some(degenbot_simulation::sim::evm::journal_pools::PoolFamily::V2Pair)
        ),
        "positive path: a known V2 address still types as V2Pair"
    );
    assert!(
        !descriptors.by_address.contains_key(&LFJ),
        "an unsupported family never enters the descriptor map (no guessing)"
    );
    assert_eq!(
        descriptors.unsupported,
        vec![(LFJ, "lfj_binned".to_string())],
        "the unsupported touched address surfaces with its kind"
    );
    assert_eq!(
        empty_frame_observe_reason(&descriptors),
        "family-unsupported"
    );

    let only_unsupported = build_descriptors(Some(&index), &[(LFJ, Vec::new())], &test_execution());
    assert!(only_unsupported.by_address.is_empty());
    assert_eq!(
        empty_frame_observe_reason(&only_unsupported),
        "family-unsupported",
        "an unsupported-only frame observes the reason, not no_candidate"
    );
}

/// The live connector-index V4 roster fills the touched manager's
/// `V4PoolSet`, so journal extraction decodes the known pool's typed
/// post-state instead of reporting the family Unsupported.
#[test]
fn known_v4_roster_extracts_the_typed_post_state() {
    const V4_MANAGER: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");
    let pool_hash = B256::new([0x5a; 32]);
    let state_base = v4_pool_state_base_slot(pool_hash);
    let sqrt_price_x96 = U256::from(1u128) << 96;
    let tick_post = 1234i32;
    let liquidity: u128 = 42;
    let slot0 = v4_slot0_slot(state_base);
    let liq_slot = v4_liquidity_slot(state_base);

    let mut index = degenbot_bot::connector_index::V2ConnectorIndex::default();
    index.push_v4_edge(degenbot_bot::connector_index::V4Edge {
        pool_hash,
        manager: V4_MANAGER,
        state_view: None,
        token0: TOK,
        token1: WETH,
        fee: 3000,
        fee_currency1: 3000,
        tick_spacing: 60,
        hooks: Address::ZERO,
        db_pool_id: 902,
    });

    let outcome = manager_journal_outcome(
        V4_MANAGER,
        &[
            (
                slot0,
                encode_v4_slot0(V4Slot0Parts {
                    sqrt_price_x96,
                    tick: tick_post,
                    ..Default::default()
                }),
            ),
            (liq_slot, encode_v4_liquidity_slot(liquidity)),
        ],
    );

    let descriptors = build_descriptors(Some(&index), &outcome.touched, &test_execution());
    let Some(PoolFamily::V4PoolManager { pools }) = descriptors.by_address.get(&V4_MANAGER) else {
        panic!("the touched manager must get a V4 descriptor");
    };
    assert_eq!(
        pools.as_slice(),
        &[V4PoolDescriptor {
            pool_id: pool_hash,
            tick_spacing: 60,
        }],
        "the manager descriptor set owns the index roster"
    );

    let extracted = degenbot_simulation::sim::evm::journal_pools::extract_pool_post_states(
        &outcome,
        &descriptors.by_address,
    );
    let v4 = extracted
        .iter()
        .find(|s| s.address == V4_MANAGER)
        .expect("the manager extracts");
    match &v4.kind {
        PoolPostKind::Typed(TypedPoolPost::V4 {
            pool_id,
            sqrt_price_x96: sqrt,
            tick,
            liquidity: liq,
            ..
        }) => {
            assert_eq!(*pool_id, pool_hash);
            assert_eq!(*sqrt, Some(sqrt_price_x96));
            assert_eq!(*tick, Some(tick_post));
            assert_eq!(*liq, Some(liquidity));
        }
        other => panic!("expected a typed V4 post-state, got {other:?}"),
    }
}

/// A manager with NO roster edges keeps the empty set: the extractor reports
/// the family Unsupported rather than guessing a pool identity.
#[test]
fn manager_without_roster_edges_stays_unsupported() {
    const V4_MANAGER: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");
    let outcome =
        manager_journal_outcome(V4_MANAGER, &[(U256::from(0xdead_u64), U256::from(1u64))]);
    let index = degenbot_bot::connector_index::V2ConnectorIndex::default();

    let descriptors = build_descriptors(Some(&index), &outcome.touched, &test_execution());
    let Some(PoolFamily::V4PoolManager { pools }) = descriptors.by_address.get(&V4_MANAGER) else {
        panic!("the singleton is a known manager even with an empty roster");
    };
    assert!(pools.is_empty());
    assert!(descriptors.hit_v4);

    let extracted = degenbot_simulation::sim::evm::journal_pools::extract_pool_post_states(
        &outcome,
        &descriptors.by_address,
    );
    let v4 = extracted
        .iter()
        .find(|s| s.address == V4_MANAGER)
        .expect("the manager extracts");
    assert!(
        matches!(v4.kind, PoolPostKind::Unsupported),
        "an empty roster never fabricates a decode, got {:?}",
        v4.kind
    );
    assert_eq!(empty_frame_observe_reason(&descriptors), "v4_unsupported");
}

/// One hand-built frame whose only touched account is `manager`, holding the
/// given `(slot, present_value)` writes.
fn manager_journal_outcome(manager: Address, writes: &[(U256, U256)]) -> ReplayOutcome {
    let mut state = EvmState::default();
    let mut account = Account::default();
    account.status = AccountStatus::Touched;
    for (slot, value) in writes {
        account.storage.insert(
            *slot,
            EvmStorageSlot {
                original_value: U256::ZERO,
                present_value: *value,
                transaction_id: revm::state::TransactionId::ZERO,
                is_cold: false,
            },
        );
    }
    state.insert(manager, account);
    ReplayOutcome {
        status: ReplayStatus::Success,
        state,
        touched: vec![(manager, writes.iter().map(|(slot, _)| *slot).collect())],
        rpc_reads: 0,
        wall: std::time::Duration::from_micros(1),
        base_fee_source: BaseFeeSource::Projected,
    }
}

/// Reference constant-product output (independent of the mixed solver's
/// closed form): `out = in*997*r_out / (r_in*1000 + in*997)`.
fn reference_out(r_in: u128, r_out: u128, amount_in: u128) -> u128 {
    let num = U256::from(amount_in) * U256::from(997u64) * U256::from(r_out);
    let den = U256::from(r_in) * U256::from(1000u64) + U256::from(amount_in) * U256::from(997u64);
    u128::try_from(num / den).unwrap()
}

/// Brute-force optimum of a CLP chain with fees — the independent truth the
/// normalized-candidate assertions compare against (the solver's closed form
/// is derived independently of this scan).
fn best_chain_profit(chains: &[(u128, u128)], max_in: u128) -> (u128, u128) {
    let mut best = (0u128, 0u128);
    for w in 1..=max_in {
        let mut amt = w;
        for (r_in, r_out) in chains {
            amt = reference_out(*r_in, *r_out, amt);
        }
        if amt > w {
            let p = amt - w;
            if p > best.1 {
                best = (w, p);
            }
        }
    }
    best
}

/// Admit an abstract-units V2 fixture pool (canonical token order by
/// address, which the fixture constants already are).
fn admitted_pair(
    solver: &mut BackrunSolver,
    addr: Address,
    token0: Address,
    token1: Address,
    r0: u128,
    r1: u128,
) -> u64 {
    solver
        .admit_v2(&BackrunV2Pool {
            address: addr,
            token0,
            token1,
            reserve0: r0,
            reserve1: r1,
            fees: v2_fee_pair(),
        })
        .unwrap()
}

/// The honest drop: a frame whose affected pool trades no WETH quote
/// composes no candidate, and the observe label is `non_base_quote` — never
/// a fake conversion for ranking only. This pins the full label matrix of
/// `honest_observe`: the truthful verdict in every combination.
#[test]
fn non_base_quote_drop_is_truthful() {
    assert_eq!(
        degenbot_strategy::frame_pipeline::honest_observe("no_candidate", true, false),
        "non_base_quote"
    );
    // A WETH-closing candidate that did solve keeps the existing truthful
    // verdict; an unrelated observe reason is never rewritten.
    assert_eq!(
        degenbot_strategy::frame_pipeline::honest_observe("no_candidate", true, true),
        "no_candidate"
    );
    assert_eq!(
        degenbot_strategy::frame_pipeline::honest_observe("budget_exhausted", true, false),
        "budget_exhausted"
    );
}

/// No DB/index is attached (offline-review without the workspace DB), so the
/// discovery lane stays shut and every observe reason must be truthful.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network: needs a chain-1 RPC behind DEGENBOT_RPC_HTTP_CHAINID_1"]
#[expect(
    clippy::too_many_lines,
    reason = "the live dry-run reads top-to-bottom"
)]
async fn dry_run_fixture_frames_replay_end_to_end_without_classifier() {
    use std::path::PathBuf;
    use std::sync::Arc;

    use alloy::primitives::address;
    use alloy::providers::ProviderBuilder;
    use degenbot_bot::bot_core::SimAnchorState;
    use degenbot_strategy::backrun::{Decision, MevblockerBackrun};
    use degenbot_strategy::backrun_strategy::BackrunStrategy;
    use degenbot_strategy::frame_pipeline::{
        build_block_handle, load_fixture_frames, process_frame, PipelineConfig,
    };

    // Arm the guard for the WHOLE run (this target never calls classify).
    std::env::set_var("DEGENBOT_CLASSIFIER_GUARD", "1");

    let rpc_url = std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1").unwrap();
    let provider = degenbot_rpc::provider::AlloyProvider::from_provider(Arc::new(
        ProviderBuilder::default().connect_http(rpc_url.parse().unwrap()),
    ));
    let head = provider.get_block_number().await.unwrap();
    let pin = head.saturating_sub(1);

    // The trace capture lands in a temp file for in-process assertions.
    let trace_path = std::env::temp_dir().join("wkpzqk-e2e-trace.jsonl");
    let _ = std::fs::remove_file(&trace_path);
    let mut boot = degenbot_config::BotConfig::default();
    boot.logging.trace_jsonl = Some(trace_path.clone());
    let _ = degenbot_config::holder::install(std::sync::Arc::new(boot));

    // Live mode needs no feed/signer/dispatcher: process frames directly.
    let mut runtime = market_context(None, None);
    let execution =
        ExecutionContext::ethereum(address!("0x30b28ed8aa581fbc0191c3b532b0697773070e97"));
    let mut strategy = BackrunStrategy::new(execution);
    let anchor_state = SimAnchorState::default();
    let mut handle = Option::from(
        build_block_handle(
            &provider,
            pin,
            &execution,
            &runtime.warm_cache,
            &anchor_state,
        )
        .await
        .expect("live replay handle builds"),
    );

    let mut knobs =
        MevblockerBackrun::from_config(&degenbot_config::BotConfig::default(), String::new())
            .into_config();
    knobs.stop_file = PathBuf::from("/nonexistent-wkpzqk");
    let pl = PipelineConfig {
        execution,
        owner: address!("0x5c603b8a137a40426e0ddfa981ec10c245af080e"),
        bribe_bips: 9_800,
        wallet_gas_cost_wei: Arc::new(std::sync::atomic::AtomicU64::new(1_000_000_000_000)),
        gas_floor_wei: U256::from(50_000_000_000_000u64),
        fixture_mode: false,
    };
    // The bundle-sim client points at the SAME node join (read/sim only, and
    // only reached if a candidate ever composes — offline-review without a
    // DB never gets there: connectors are never guessed).
    let sim_client = alloy::rpc::client::ClientBuilder::default().http(rpc_url.parse().unwrap());

    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/frame_replay_capture.jsonl");
    let frames = load_fixture_frames(&fixture);
    assert!(
        !frames.is_empty(),
        "the committed capture fixture is non-empty"
    );

    for ev in &frames {
        // A capture replayed at a LATER head has a stale nonce (the tx has
        // long since landed). Re-arm the frame's nonce to the pinned view's
        // parent count so the REPLAY path is genuinely exercised end-to-end
        // (the signer reaches the seam as recovered data; the seam validates
        // the nonce faithfully against the layered view).
        let mut frame = ev.clone();
        frame.nonce = provider
            .get_transaction_count(&ev.from, Some(pin.saturating_sub(1)))
            .await
            .unwrap_or(ev.nonce);
        let artifacts = process_frame(
            &mut strategy,
            &mut runtime,
            &provider,
            &sim_client,
            &knobs,
            &pl,
            &mut handle,
            &frame,
            pin,
            U256::ZERO,
        )
        .await;
        // Every decision is observe-only and the reason is TRUTHFUL: the
        // un-composed frame never reads as "the sim rejected our work".
        match &artifacts.decision {
            Decision::Observe { reason } => {
                assert!(
                    matches!(
                        *reason,
                        "no_candidate"
                            | "family-unsupported"
                            | "v4_unsupported"
                            | "reverted"
                            | "replay_failed"
                            | "replay_unavailable"
                    ),
                    "truthful observe reason, got {reason}"
                );
            }
            Decision::Bid { .. } => panic!("dry-run without index/db cannot bid"),
            Decision::Drop { reason } => {
                panic!("a valid dry-run frame must never drop, got {reason}");
            }
        }
    }

    // The JSONL trace got the per-frame replay events (wall/rpc_reads/touched).
    let traced = std::fs::read_to_string(&trace_path).unwrap();
    let mut replay_events = 0;
    for line in traced.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        if v["kind"] == "replay" {
            replay_events += 1;
            // A settled frame carries the seam telemetry (wall/rpc_reads/
            // touched); a rejected frame (stale-nonce capture replayed at a
            // later head) carries the typed error instead. Both are truthful.
            assert!(
                (v["rpc_reads"].is_u64() && v["touched"].is_u64() && v["wall_us"].is_u64())
                    || v["error"].is_string(),
                "replay event shape: {v}"
            );
        }
    }
    assert_eq!(
        replay_events,
        frames.len(),
        "one replay event per fixture frame"
    );
}

// ─────────────── the anchored walker's depth-3 lane ───────────────

/// M: the depth-3 intermediate (canonical token order: TOK < M < WETH).
const M: Address = address!("0000000000000000000000000000000000000ab2");
/// C1/C2: the walker-discovered bridge pools (TOK-M and M-WETH).
const C1: Address = address!("000000000000000000000000000000000000b101");
const C2: Address = address!("000000000000000000000000000000000000b102");

/// The walker's 3-hop chain (WETH entry, P tok->weth anchor first, then
/// C1 tok->M, then C2 M->WETH) runs through the SAME declare/evaluate gate
/// as the fans: declared once, envelope-evaluated, best candidate kept and
/// composable. Profit matches the independent reference CLP-chain scan.
#[test]
fn walker_three_hop_chain_solves_and_composes() {
    let mut solver = BackrunSolver::new();
    // P staged golden (476_259 TOK / 1_050 WETH), C1 (400_000 TOK /
    // 800_000 M), C2 (800_000 M / 2_000 WETH): the bridge pays ~0.005 WETH
    // per TOK against P's ~0.0022, so the WETH-entry chain profits.
    let p_id = admitted_pair(&mut solver, P, TOK, WETH, 476_259, 1_050);
    let c1_id = admitted_pair(&mut solver, C1, TOK, M, 400_000, 800_000);
    let c2_id = admitted_pair(&mut solver, C2, M, WETH, 800_000, 2_000);

    // Canonical `zfo` rule: the INPUT token is the hop's token0.
    let chains = vec![vec![
        BackrunHopRef {
            pool_id: p_id,
            pool: P,
            token0: TOK,
            token1: WETH,
            zfo: false, // entry WETH = token1
            family: LaneFamily::V2 { fees: v2_fees() },
        },
        BackrunHopRef {
            pool_id: c1_id,
            pool: C1,
            token0: TOK,
            token1: M,
            zfo: true, // input TOK = token0
            family: LaneFamily::V2 { fees: v2_fees() },
        },
        BackrunHopRef {
            pool_id: c2_id,
            pool: C2,
            token0: M,
            token1: WETH,
            zfo: true, // input M = token0
            family: LaneFamily::V2 { fees: v2_fees() },
        },
    ]];

    let stats =
        degenbot_strategy::backrun_strategy::solve_dfs_chains(&mut solver, &chains, U256::ZERO);
    assert_eq!(stats.dfs_declared, 1, "the walker chain declares once");
    assert_eq!(stats.dfs_evaluated, 1, "the chain clears the zero floor");
    let best = stats.best.expect("the 3-hop walker chain profits");
    assert_eq!(best.hops.len(), 3);
    assert_eq!(stats.chains.len(), 1, "one chain outcome recorded");
    assert!(stats.chains[0].evaluated);
    assert_eq!(stats.chains[0].pools, vec![P, C1, C2]);
    assert_eq!(stats.chains[0].profit_wei, Some(best.profit));
    assert!(stats.chains[0].reject.is_none());

    // Wei honesty: the recorded profit matches the independent CLP-chain
    // optimum (WETH -> TOK -> M -> WETH).
    let (w_star, p_star) = best_chain_profit(
        &[(1_050, 476_259), (400_000, 800_000), (800_000, 2_000)],
        60_000,
    );
    assert!(p_star > 0, "the scanned 3-hop chain profits");
    assert!(
        (i128::try_from(best.profit).unwrap() - i128::try_from(p_star).unwrap()).abs() <= 3,
        "wei profit {} vs reference {}",
        best.profit,
        p_star
    );
    assert!(
        best.optimal_input.abs_diff(w_star) <= 100,
        "optimal WETH input {} vs reference {}",
        best.optimal_input,
        w_star
    );

    let (path, result) = project_candidate(&best);
    let CmdExecutorOutcome::Encoded(cd) = CmdExecutorAdapter::new(ExecutionContext::new(
        address!("0x30b28ed8aa581fbc0191c3b532b0697773070e97"),
        address!("000000000004444c5dc75cb358380d2e3de08a90"),
        WETH,
    ))
    .compose(&path, &result, backrun_encode_options(9_800)) else {
        panic!("the 3-hop walker candidate composes")
    };
    assert!(cd.len() > 4 + 32 * 3 + 64, "execute() calldata shape");
}

// ───────────────────── the bid ladder (single bribe site) ──────────────────

#[test]
fn bid_ladder_is_floor_of_bribe_share() {
    // The 98% ladder applied to an exact-solve profit: the bid is the
    // TRUNCATED (floor) share, and nothing else gates on a second
    // application of the share. Expected values are worked literals, not
    // recomputed by the code under test.
    assert_eq!(
        net_bid(
            55_000_000_000_000_000_000u128,
            1_000_000_000_000,
            9_800,
            u128::MAX
        )
        .expect("viable")
        .bid_wei,
        53_900_000_000_000_000_000u128
    );
    // Truncation: at a negligible gas cost the ladder floors (123 wei at a
    // 9902-bips floored share = 120), never rounds up.
    assert_eq!(
        net_bid(123, 0, 9_800, u128::MAX).expect("viable").bid_wei,
        U256::from(120u64)
    );
    // A different ceiling moves the bid with it.
    assert_eq!(
        net_bid(10_000, 0, 9_000, u128::MAX)
            .expect("viable")
            .bid_wei,
        U256::from(9_000u64)
    );
    // A gas burn above the gross refuses the bid entirely: 5 wei of gross
    // cannot fund any wallet-positive bundle.
    assert!(net_bid(5, 9_800, 9_800, u128::MAX).is_none());
}

/// The observe-reason histogram split: every typed `ReplayFrameError` maps to
/// exactly one honest label, and the JSONL evidence carries the nonce pair.
#[test]
fn replay_reasons_split() {
    use degenbot_simulation::sim::evm::frame_replay::ReplayFrameError;

    let (r, v) =
        degenbot_strategy::frame_pipeline::replay_observe_reason(&ReplayFrameError::GapPending {
            claimed: 12,
            expected: 10,
        });
    assert_eq!(r, "gap_pending");
    assert_eq!(v["claimed_nonce"], 12);
    assert_eq!(v["expected_nonce"], 10);

    let (r, v) = degenbot_strategy::frame_pipeline::replay_observe_reason(
        &ReplayFrameError::AlreadySettled {
            frame: 30673,
            parent: 30674,
        },
    );
    assert_eq!(r, "already_settled");
    assert_eq!(v["frame_nonce"], 30673);
    assert_eq!(v["parent_nonce"], 30674);

    let (r, v) = degenbot_strategy::frame_pipeline::replay_observe_reason(
        &ReplayFrameError::MalformedTransaction {
            raw: "call gas cost (49784) exceeds the gas limit (0)".into(),
        },
    );
    assert_eq!(r, "malformed_transaction");
    assert_eq!(
        v["detail"],
        "call gas cost (49784) exceeds the gas limit (0)"
    );

    let (r, _) =
        degenbot_strategy::frame_pipeline::replay_observe_reason(&ReplayFrameError::Other {
            raw: "rpc timeout".into(),
        });
    assert_eq!(r, "replay_failed");
}
