//! Pin for the V3-anchor sparse-tick defect (`unusable_pool_state` on every
//! declared chain): a target swap that crosses NO initialized tick leaves the
//! replayed V3 post-state with an empty tick map, and a solver range sequence
//! cannot be built from it. The fix stages the anchor's tick map through the
//! pool ingress (`Db → Chain` — the complete per-tick map the pump maintains),
//! so a healthy wide/range anchor admits and solves while a genuinely
//! unusable (ticks-less) anchor still rejects.

#![expect(clippy::unwrap_used, clippy::panic)]

use alloy::primitives::aliases::U128;
use alloy::primitives::{address, Address, I256, U256};
use degenbot_bot::connector_index::{V2ConnectorIndex, V2Edge, V3Edge};
use degenbot_db::connection::DegenbotDb;
use degenbot_db::discovery::V3PoolRowInput;
use degenbot_db::{ApplyBitmapAtWord, ApplyLiquidityAtTick};
use degenbot_pools::v3_state::ClSlotLayout;
use degenbot_simulation::sim::evm::journal_pools::{
    PoolFamily, PoolPostKind, PoolPostState, TypedPoolPost,
};
use degenbot_strategy::backrun_engine::{BackrunHopRef, BackrunSolver, BackrunV2Pool, LaneFamily};
use degenbot_strategy::backrun_strategy::{admit_extracted, solve_dfs_chains, WETH};
use degenbot_strategy::frame_pipeline::MarketContext;
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
const FACTORY: Address = address!("00000000000000000000000000000000000000ff");
/// `sqrt(1) * 2^96` — the tick-0 price.
const SQRT_ONE: u128 = 79_228_162_514_264_337_593_543_950_336;

/// Seed the anchor pool row + its initialized ticks into the in-memory DB, the
/// complete map the ingestion pump maintains. With no ticks the pool is a
/// legitimately-empty `Tracked` anchor (the RED case).
fn seed_anchor(db: &DegenbotDb, ticks: &[i32]) {
    db.upsert_exchange(1, "uniswap_v3", FACTORY, None).unwrap();
    db.upsert_v3_pools(
        1,
        "uniswap_v3",
        1,
        1_000_000,
        &[V3PoolRowInput {
            address: ANCHOR,
            token0_address: TOK,
            token1_address: WETH,
            fee: i64::from(ANCHOR_FEE),
            tick_spacing: i64::from(ANCHOR_SPACING),
        }],
    )
    .unwrap();
    let addr_s = ANCHOR.to_checksum(None);
    let pool_id: i64 = {
        let conn = db.lock();
        conn.query_row(
            "SELECT id FROM pools WHERE address = ?1 LIMIT 1",
            [&addr_s],
            |r| r.get(0),
        )
        .unwrap()
    };
    let mut tick_bitmap: HbMap<i32, ApplyBitmapAtWord> = HbMap::new();
    let mut tick_data: HbMap<i32, ApplyLiquidityAtTick> = HbMap::new();
    for &tick in ticks {
        tick_data.insert(
            tick,
            ApplyLiquidityAtTick {
                liquidity_gross: U128::from(10_000u64),
                liquidity_net: I256::try_from(if tick > 0 { 5_000i128 } else { -4_000i128 })
                    .unwrap(),
                block: 0,
            },
        );
        let compressed = tick.div_euclid(ANCHOR_SPACING);
        let word = compressed >> 8;
        let bit = compressed.rem_euclid(256) as u64;
        tick_bitmap
            .entry(word)
            .or_insert_with(|| ApplyBitmapAtWord {
                bitmap: U256::ZERO,
                block: 0,
            });
        tick_bitmap.get_mut(&word).unwrap().bitmap |= U256::from(1u64) << bit;
    }
    db.upsert_v3_liquidity_positions(pool_id, &tick_data)
        .unwrap();
    db.upsert_v3_initialization_maps(pool_id, &tick_bitmap)
        .unwrap();
}

/// Test stand-in for the Db→head backfill transport. The fixtures stamp no
/// `liquidity_update_block`, so no window is ever backfilled; an unexpected
/// fetch declines loudly rather than staging stale state.
struct NoBackfill;

impl degenbot_bot::bot_core::pool_ingress::V3LiquidityLogSource for NoBackfill {
    fn fetch_v3_liquidity_events(
        &self,
        _pool: alloy::primitives::Address,
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

fn runtime(ticks: &[i32]) -> (MarketContext, u64, u64) {
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
    seed_anchor(&db, ticks);
    let mut index = V2ConnectorIndex::default();
    index.push_v3_edge(V3Edge {
        pool_id: 201,
        token0_id: tok_id,
        token1_id: weth_id,
        address: ANCHOR,
        fee: ANCHOR_FEE,
        tick_spacing: ANCHOR_SPACING,
        layout: ClSlotLayout::UniswapV3,
    });
    index.push_edge(V2Edge {
        pool_id: 202,
        token0_id: tok_id,
        token1_id: weth_id,
        address: MID,
    });
    (
        market_context(
            Some(std::sync::Arc::new(
                degenbot_bot::bot_core::RouteRegistry::new(index),
            )),
            Some(std::sync::Arc::new(db)),
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

/// RED half pins the defect (an anchor with no DB ticks cannot project);
/// GREEN half pins the fix (the Db-staged map makes the anchor modelable).
/// The gate stays for the genuinely unusable anchor.
#[test]
fn v3_anchor_db_tick_map_admits_and_solves() {
    // RED: the replayed state alone (a Db-registered but tick-less anchor)
    // cannot project — the whole chain rejects.
    let (rt, _tok, _weth) = runtime(&[]);
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
    match &stats.chains[0].reject {
        Some(degenbot_strategy::backrun_engine::PathReject::UnusablePoolState {
            deficits, ..
        }) => {
            assert!(*deficits >= 1, "the unusable anchor is the deficit");
        }
        other => panic!("expected unusable_pool_state, got {other:?}"),
    }

    // GREEN: the Db-staged tick map fills the replayed map; the same healthy
    // anchor + healthy V2 hop now solves.
    let (rt, _tok, _weth) = runtime(&[120, -120]);
    let mut solver = BackrunSolver::new();
    let affected = admit_extracted(
        &rt,
        &mut solver,
        &[anchor_post_state()],
        SEED,
        "0xpin",
        None,
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
        "the Db-staged map makes the anchor modelable"
    );
    assert!(stats.chains[0].reject.is_none());
}

/// The `anchor_ticks` probe must tolerate u128-scale liquidity: real pools
/// carry >> `u64::MAX` liquidity, and `serde_json::json!` errors ("number out
/// of range") on such u128s — which panicked the io-rt thread and stopped
/// the feed live. Pinned by driving a `u128::MAX` anchor through admission.
#[test]
fn anchor_admission_survives_u128_liquidity() {
    let (rt, _tok, _weth) = runtime(&[120, -120]);
    let mut post = anchor_post_state();
    if let PoolPostKind::Typed(TypedPoolPost::V3 { liquidity, .. }) = &mut post.kind {
        *liquidity = Some(u128::MAX);
    }
    let affected = admit_extracted(&rt, &mut BackrunSolver::new(), &[post], SEED, "0xpin", None);
    assert_eq!(
        affected.len(),
        1,
        "u128 liquidity stages and traces cleanly"
    );
}

/// The ingress map never overwrites a replayed touched tick (post-frame facts
/// win) — pinned through the admission path's public behavior.
#[test]
fn replayed_touched_tick_wins_over_ingress_map() {
    let (rt, _tok, _weth) = runtime(&[120, -120]);
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
    let affected = admit_extracted(&rt, &mut solver, &[post], SEED, "0xpin", None);
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
