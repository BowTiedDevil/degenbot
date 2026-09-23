//! Pin for the V3-anchor sparse-tick defect (`unusable_pool_state` on every
//! declared chain): a target swap that crosses NO initialized tick leaves the
//! replayed V3 post-state with an empty tick map, and a solver range sequence
//! cannot be built from it. The fix sources the in-range initialized-tick
//! window from the same chain view the frame replayed over, so a healthy
//! anchor admits and solves while a genuinely unusable (ticks-less) anchor
//! still rejects.

#![expect(clippy::unwrap_used, clippy::panic)]

use alloy::primitives::{address, Address, U128, U256};
use degenbot_bot::connector_index::{V2ConnectorIndex, V2Edge, V3Edge};
use degenbot_db::connection::DegenbotDb;
use degenbot_pools::v3_state::ClSlotLayout;
use degenbot_pools::TickInfo;
use degenbot_simulation::sim::evm::journal_pools::{
    PoolFamily, PoolPostKind, PoolPostState, TypedPoolPost,
};
use degenbot_strategy::backrun_engine::{BackrunHopRef, BackrunSolver, BackrunV2Pool, LaneFamily};
use degenbot_strategy::backrun_strategy::{admit_extracted, solve_dfs_chains, WETH};
use degenbot_strategy::frame_pipeline::MarketContext;
use degenbot_strategy::pending_tx::V3TickWindow;
use hashbrown::HashMap as HbMap;

/// The V3 anchor's tokens (canonical order: TOK0 < WETH).
const TOK: Address = address!("0000000000000000000000000000000000000aa1");
/// The V3 anchor pool (the frame's touched pool).
const ANCHOR: Address = address!("000000000000000000000000000000000000b001");
/// The V2 connector the walker discovers (cheaper TOK than the anchor).
const MID: Address = address!("000000000000000000000000000000000000b002");
const SEED: u64 = 7;
const ANCHOR_FEE: u32 = 3_000;
const ANCHOR_SPACING: i32 = 60;
/// `sqrt(1) * 2^96` — the tick-0 price.
const SQRT_ONE: u128 = 79_228_162_514_264_337_593_543_950_336;

/// The chain-view tick window the fix merges: two initialized ticks
/// straddling tick 0 (the same fixture the CL projection tests use).
struct TwoTickWindow;
impl V3TickWindow for TwoTickWindow {
    fn tick_window(
        &self,
        _pool: Address,
        _layout: ClSlotLayout,
        _tick_spacing: i32,
        _current_tick: i32,
        head: u64,
    ) -> HbMap<i32, TickInfo> {
        let mut m = HbMap::default();
        m.insert(
            120,
            TickInfo {
                liquidity_gross: U128::from(10_000),
                liquidity_net: 5_000,
                block: head,
            },
        );
        m.insert(
            -120,
            TickInfo {
                liquidity_gross: U128::from(8_000),
                liquidity_net: -4_000,
                block: head,
            },
        );
        m
    }
}

fn runtime() -> (MarketContext, u64, u64) {
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
    let mut index = V2ConnectorIndex::default();
    index.push_v3_edge(V3Edge {
        pool_id: 201,
        token0_id: tok_id,
        token1_id: weth_id,
        address: ANCHOR,
        fee: ANCHOR_FEE,
        tick_spacing: ANCHOR_SPACING,
    });
    index.push_edge(V2Edge {
        pool_id: 202,
        token0_id: tok_id,
        token1_id: weth_id,
        address: MID,
    });
    (
        MarketContext::new(
            1,
            Some(std::sync::Arc::new(
                degenbot_bot::bot_core::RouteRegistry::new(index),
            )),
            Some(db),
            8,
            4,
        ),
        tok_id,
        weth_id,
    )
}

/// The frame's replayed V3 post-state: price/liquidity staged, NO touched
/// ticks (the target swap crossed no initialized tick).
fn anchor_post_state() -> PoolPostState {
    PoolPostState {
        address: ANCHOR,
        family: PoolFamily::V3 {
            layout: ClSlotLayout::UniswapV3,
            tick_spacing: ANCHOR_SPACING,
            current_tick_hint: Some(0),
        },
        kind: PoolPostKind::Typed(TypedPoolPost::V3 {
            sqrt_price_x96: Some(U256::from(SQRT_ONE)),
            tick: Some(0),
            liquidity: Some(10_000_000_000_000),
            touched_ticks: Vec::new(),
        }),
    }
}

fn chain_refs(anchor_id: u64, mid_id: u64) -> Vec<BackrunHopRef> {
    vec![
        BackrunHopRef {
            pool_id: anchor_id,
            pool: ANCHOR,
            token0: TOK,
            token1: WETH,
            // WETH entry: the anchor consumes WETH = token1.
            zfo: false,
            family: LaneFamily::V3 { fee: ANCHOR_FEE },
        },
        BackrunHopRef {
            pool_id: mid_id,
            pool: MID,
            token0: TOK,
            token1: WETH,
            // Now holding TOK: the mid consumes TOK = token0.
            zfo: true,
            family: LaneFamily::V2,
        },
    ]
}

fn admit_mid(solver: &mut BackrunSolver) -> u64 {
    solver
        .admit_v2(&BackrunV2Pool {
            address: MID,
            token0: TOK,
            token1: WETH,
            reserve0: 2_000,
            reserve1: 400_000,
        })
        .unwrap()
}

/// RED half reproduces the defect (no chain-view window), GREEN half pins the
/// fix (window present). The gate stays for the genuinely unusable anchor.
#[test]
fn v3_anchor_sparse_tick_window_admits_and_solves() {
    // RED: the replayed state alone cannot project — the whole chain rejects.
    let (rt, _tok, _weth) = runtime();
    let mut solver = BackrunSolver::new();
    let affected = admit_extracted(
        &rt,
        &mut solver,
        &[anchor_post_state()],
        SEED,
        "0xpin",
        None,
    );
    assert_eq!(affected.len(), 1, "the V3 anchor admits into the scope");
    let mid_id = admit_mid(&mut solver);
    let stats = solve_dfs_chains(
        &mut solver,
        &[chain_refs(affected[0].workspace_pool_id, mid_id)],
        U256::ZERO,
    );
    assert_eq!(stats.dfs_declared, 1);
    assert_eq!(
        stats.dfs_evaluated, 0,
        "an anchor with no modelable tick ranges must not evaluate"
    );
    match stats.chains[0].reject {
        Some(degenbot_strategy::backrun_engine::PathReject::UnusablePoolState { deficits }) => {
            assert!(deficits >= 1, "the unusable anchor is the deficit");
        }
        other => panic!("expected unusable_pool_state, got {other:?}"),
    }

    // GREEN: the chain-view tick window fills the replayed map; the same
    // healthy anchor + healthy V2 hop now solves.
    let (rt, _tok, _weth) = runtime();
    let mut solver = BackrunSolver::new();
    let affected = admit_extracted(
        &rt,
        &mut solver,
        &[anchor_post_state()],
        SEED,
        "0xpin",
        Some(&TwoTickWindow),
    );
    assert_eq!(affected.len(), 1, "the V3 anchor admits");
    let mid_id = admit_mid(&mut solver);
    let stats = solve_dfs_chains(
        &mut solver,
        &[chain_refs(affected[0].workspace_pool_id, mid_id)],
        U256::ZERO,
    );
    assert_eq!(stats.dfs_declared, 1);
    assert_eq!(
        stats.dfs_evaluated, 1,
        "the chain-view window makes the anchor modelable"
    );
    assert!(stats.chains[0].reject.is_none());
}

/// The merged window never overwrites a replayed touched tick (post-frame
/// facts win) — pinned through the admission path's public behavior.
#[test]
fn replayed_touched_tick_wins_over_window() {
    let (rt, _tok, _weth) = runtime();
    let mut post = anchor_post_state();
    if let PoolPostKind::Typed(TypedPoolPost::V3 { touched_ticks, .. }) = &mut post.kind {
        touched_ticks.push(
            degenbot_simulation::sim::evm::journal_pools::TouchedTickWord {
                tick: -120,
                liquidity_gross: 1,
                liquidity_net: -1,
            },
        );
    }
    let mut solver = BackrunSolver::new();
    let affected = admit_extracted(
        &rt,
        &mut solver,
        &[post],
        SEED,
        "0xpin",
        Some(&TwoTickWindow),
    );
    assert_eq!(affected.len(), 1);
    // The pool's state is readable and the chain solves (the replayed tick
    // did not corrupt the merged map).
    let mid_id = admit_mid(&mut solver);
    let stats = solve_dfs_chains(
        &mut solver,
        &[chain_refs(affected[0].workspace_pool_id, mid_id)],
        U256::ZERO,
    );
    assert!(stats.chains[0].evaluated);
}
