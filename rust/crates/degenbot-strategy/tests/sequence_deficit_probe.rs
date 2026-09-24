#![expect(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]
//! Live capture+inject probe for the `sequence_unavailable` deficit on two
//! HIGH-liquidity roster pools (2026-09-24) — both for the same unlisted
//! token `0x8E870D67…89E1`, both on the canonical Uniswap V3 factory:
//!
//! - `0x08680f…D445` — token/USDT, spacing 10, fee 500, `liquidity()` ≈ 1.57e22
//! - `0x0BDDd19c…5Ba6` — token/WETH, spacing 1,  fee 100, `liquidity()` ≈ 1.86e17
//!
//! Unlike the dormant (liquidity = 0) pools whose rejection is provably
//! correct, these are healthy pools, so the reject is suspect.
//!
//! Hypothesis A (wide positions): the production cold-admit ladder stages
//! only the CURRENT bitmap word ± 1; wide/full-range positions sit far
//! outside. Hypothesis B (directional gap): the pool's in-range liquidity
//! lies only on ONE side of the current tick, so the cycle's demanded
//! direction cannot be modeled while the opposite direction could.
//!
//! Capture the real neighborhood, then inject at two levels:
//!
//! 1. **solver-level** (`RegisterV3PoolParams` → `build_int_v3_sequence`);
//! 2. **production-path** (`admit_extracted` → `solve_dfs_chains` over the
//!    real WETH-quoted V2 connector 0x3016A43B).
//!
//! ```text
//! DEGENBOT_RPC_HTTP_CHAINID_1=http://host.containers.internal:8545/ \
//!   cargo test -p degenbot-strategy \
//!     --test sequence_deficit_probe -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;

use alloy::primitives::aliases::U128;
use alloy::primitives::{Address, U256};
use degenbot_bot::bot_core::executor_hop::{V2FeePair, V2Fees};
use degenbot_bot::connector_index::{V2ConnectorIndex, V2Edge, V3Edge};
use degenbot_db::connection::DegenbotDb;
use degenbot_db::discovery::V3PoolRowInput;
use degenbot_db::{ApplyBitmapAtWord, ApplyLiquidityAtTick};
use degenbot_pools::v3_state::ClSlotLayout;
use degenbot_pools::v3_state::RegisterV3PoolParams;
use degenbot_pools::TickInfo;
use degenbot_rpc::abi::{fetch_tick_bitmap, fetch_tick_data, fetch_v3_slot0_liquidity};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm::journal_pools::{
    PoolFamily, PoolPostKind, PoolPostState, TypedPoolPost,
};
use degenbot_strategy::backrun_engine::{
    BackrunHopRef, BackrunSolver, BackrunV2Pool, LaneFamily, PathReject,
};
use degenbot_strategy::backrun_strategy::{admit_extracted, solve_dfs_chains, WETH};
use degenbot_strategy::frame_pipeline::MarketContext;
use hashbrown::HashMap as HbMap;

fn v2_fee_pair() -> V2FeePair {
    V2FeePair::from_discovered(Some(3), Some(3), Some(1_000))
}

fn v2_fees() -> V2Fees {
    v2_fee_pair().resolve().expect("valid fixture fee")
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

/// Pool 1: the production-path target (token/WETH spacing 1) + its real
/// WETH-quoted V2 connector — a natural WETH-entry cycle through the roster.
const POOL: &str = "0x0BDDd19CEc6b6E614B7B8BfF380cE830D9b85Ba6";
const POOL_TOKEN: &str = "0x8E870D67F660D95d5be530380D0eC0bd388289E1";
const CONNECTOR_V2: &str = "0x3016A43B482d0480460f6625115bd372FE90c6bf";
const POOL_SPACING: i32 = 1;
const POOL_FEE: u32 = 100;

/// Pool 2 (token/USDT spacing 10): capture-only evidence — its live deficit
/// surfaced as a HOP inside larger chains.
const POOL2: &str = "0x08680f0135E41314F3D7c1Ac16343cb3ec69D445";
const POOL2_SPACING: i32 = 10;

/// One pool's captured neighborhood.
struct Captured {
    sqrt: U256,
    tick: i32,
    liquidity: u128,
    /// The production ladder's staged map: current word ± 1.
    ladder: BTreeMap<i32, (u128, i128)>,
    /// The wide capture: every initialized tick to the MIN/MAX words.
    wide: BTreeMap<i32, (u128, i128)>,
}

async fn capture(provider: &Arc<AlloyProvider>, pool: Address, spacing: i32) -> Captured {
    let (sqrt, tick_wide, liq_wide) = fetch_v3_slot0_liquidity(provider, &pool, None)
        .await
        .expect("slot0");
    let tick: i32 = tick_wide.try_into().expect("tick fits i32");
    let liquidity: u128 = liq_wide.try_into().expect("liquidity fits u128");
    let word = i64::from(tick.div_euclid(spacing) >> 8);
    let max_w = 887_272i64.div_euclid(i64::from(spacing)) >> 8;
    let mut ladder = BTreeMap::new();
    let mut wide = BTreeMap::new();
    for dw in -(max_w + 1)..=(max_w + 1) {
        let w = word.checked_add(dw).expect("word in range");
        if w.abs() > max_w {
            continue;
        }
        let w = i16::try_from(w).expect("word fits i16");
        let bitmap = fetch_tick_bitmap(provider, &pool, w, None)
            .await
            .expect("bitmap");
        for bit in 0..256u32 {
            if (bitmap >> U256::from(bit)) & U256::from(1u8) != U256::from(1u8) {
                continue;
            }
            let t: i32 = ((i64::from(w) * 256 + i64::from(bit)) * i64::from(spacing))
                .try_into()
                .expect("tick fits i32");
            let (gross, net) = fetch_tick_data(provider, &pool, t, None)
                .await
                .expect("tick fetch");
            wide.insert(t, (gross.to::<u128>(), net));
            if (-1..=1).contains(&dw) {
                ladder.insert(t, (gross.to::<u128>(), net));
            }
        }
    }
    Captured {
        sqrt,
        tick,
        liquidity,
        ladder,
        wide,
    }
}

fn to_tickinfo(map: &BTreeMap<i32, (u128, i128)>, head: u64) -> HbMap<i32, TickInfo> {
    map.iter()
        .map(|(t, (gross, net))| {
            (
                *t,
                TickInfo {
                    liquidity_gross: U128::from(*gross),
                    liquidity_net: *net,
                    block: head,
                },
            )
        })
        .collect()
}

/// Seed a captured map into the in-memory DB as the pool's maintained tick
/// ledger: the exact content production now stages (`Db` arm).
fn seed_db_map(
    db: &DegenbotDb,
    pool: Address,
    token: Address,
    spacing: i32,
    map: &BTreeMap<i32, (u128, i128)>,
) {
    let factory: Address = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
        .parse()
        .unwrap();
    db.upsert_exchange(1, "uniswap_v3", factory, None).unwrap();
    db.upsert_v3_pools(
        1,
        "uniswap_v3",
        1,
        1_000_000,
        &[V3PoolRowInput {
            address: pool,
            token0_address: token,
            token1_address: WETH,
            fee: 100,
            tick_spacing: i64::from(spacing),
        }],
    )
    .unwrap();
    let pool_s = pool.to_checksum(None);
    let pool_id: i64 = {
        let conn = db.lock();
        conn.query_row(
            "SELECT id FROM pools WHERE address = ?1 LIMIT 1",
            [&pool_s],
            |r| r.get(0),
        )
        .unwrap()
    };
    let mut tick_bitmap: HbMap<i32, ApplyBitmapAtWord> = HbMap::new();
    let mut tick_data: HbMap<i32, ApplyLiquidityAtTick> = HbMap::new();
    for (&tick, &(gross, net)) in map {
        tick_data.insert(
            tick,
            ApplyLiquidityAtTick {
                liquidity_gross: U128::from(gross),
                liquidity_net: alloy::primitives::I256::try_from(net).unwrap(),
                block: 0,
            },
        );
        let compressed = tick.div_euclid(spacing);
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

/// Solver-level injection: build the range sequences from a captured map.
fn build_sequence(
    pool: Address,
    sqrt: U256,
    tick: i32,
    liquidity: u128,
    map: &BTreeMap<i32, (u128, i128)>,
    spacing: i32,
    fee: u32,
) -> (Option<usize>, Option<usize>) {
    let params = RegisterV3PoolParams {
        address: pool,
        token0: Address::ZERO,
        token1: Address::ZERO,
        fee,
        tick_spacing: spacing,
        factory: Address::ZERO,
        sqrt_price_x96: sqrt,
        liquidity,
        tick,
        tick_data: to_tickinfo(map, 0),
        update_block: 0,
        tick_data_block: None,
        coverage: degenbot_pools::v3_state::PoolTickCoverage::Sparse,
        fetcher: None,
        deployer: Address::ZERO,
        init_hash: alloy::primitives::B256::ZERO,
        slot_layout: ClSlotLayout::UniswapV3,
    };
    let (_identity, state) = degenbot_pools::v3_state::V3PoolState::from_params(params, 8);
    (
        state
            .build_int_v3_sequence(spacing, fee, true)
            .map(|s| s.ranges.len()),
        state
            .build_int_v3_sequence(spacing, fee, false)
            .map(|s| s.ranges.len()),
    )
}

/// The production-path fixture: the probe pool (V3) + its real WETH-quoted
/// V2 connector, over an in-memory roster.
fn runtime_for(
    pool: Address,
    token: Address,
    spacing: i32,
    fee: u32,
    v2: Address,
    map: &BTreeMap<i32, (u128, i128)>,
) -> MarketContext {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    let tok_id = db
        .get_or_create_erc20_token(1, &token.to_checksum(None), None, None, None)
        .unwrap();
    let weth_id = db
        .get_or_create_erc20_token(1, &WETH.to_checksum(None), None, None, None)
        .unwrap();
    let (tok_id, weth_id) = (
        u64::try_from(tok_id).unwrap(),
        u64::try_from(weth_id).unwrap(),
    );
    seed_db_map(&db, pool, token, spacing, map);
    let mut index = V2ConnectorIndex::default();
    index.push_v3_edge(V3Edge {
        pool_id: 301,
        token0_id: tok_id,
        token1_id: weth_id,
        address: pool,
        fee,
        tick_spacing: spacing,
        layout: ClSlotLayout::UniswapV3,
    });
    index.push_edge(V2Edge {
        pool_id: 302,
        token0_id: tok_id,
        token1_id: weth_id,
        address: v2,
        fees: v2_fee_pair(),
    });
    market_context(
        Some(Arc::new(degenbot_bot::bot_core::RouteRegistry::new(index))),
        Some(Arc::new(db)),
    )
}

fn post_state(
    pool: Address,
    sqrt: U256,
    tick: i32,
    liquidity: u128,
    spacing: i32,
) -> PoolPostState {
    PoolPostState {
        address: pool,
        family: PoolFamily::V3 {
            layout: ClSlotLayout::UniswapV3,
            tick_spacing: spacing,
            current_tick_hint: Some(tick),
        },
        kind: PoolPostKind::Typed(TypedPoolPost::V3 {
            sqrt_price_x96: Some(sqrt),
            tick: Some(tick),
            liquidity: Some(liquidity),
            touched_ticks: Vec::new(),
        }),
    }
}

fn probe_chain(
    pool: Address,
    token: Address,
    v2: Address,
    v3_id: u64,
    v2_id: u64,
    fee: u32,
) -> Vec<BackrunHopRef> {
    vec![
        BackrunHopRef {
            pool_id: v3_id,
            pool,
            token0: token,
            token1: WETH,
            zfo: false,
            family: LaneFamily::V3 { fee },
        },
        BackrunHopRef {
            pool_id: v2_id,
            pool: v2,
            token0: token,
            token1: WETH,
            zfo: true,
            family: LaneFamily::V2 { fees: v2_fees() },
        },
    ]
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network: capture + inject the two high-liquidity deficit pools"]
#[expect(clippy::too_many_lines)]
async fn sequence_deficit_pools_capture_inject_reproduce() {
    let provider = Arc::new(
        AlloyProvider::new(
            &std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1")
                .unwrap_or_else(|_| String::from("http://host.containers.internal:8545")),
            3,
        )
        .await
        .expect("provider"),
    );
    let pool: Address = POOL.parse().unwrap();
    let token: Address = POOL_TOKEN.parse().unwrap();
    let v2: Address = CONNECTOR_V2.parse().unwrap();
    let head = provider.get_block_number().await.unwrap();

    let cap = capture(&provider, pool, POOL_SPACING).await;
    println!(
        "captured: tick={} liquidity={} ladder-initialized={} wide-initialized={}",
        cap.tick,
        cap.liquidity,
        cap.ladder.len(),
        cap.wide.len()
    );
    let nearest = cap
        .wide
        .keys()
        .map(|t| (*t - cap.tick).abs())
        .min()
        .unwrap_or(i32::MAX);
    println!("nearest initialized tick: {nearest} ticks from the current tick");
    if nearest == 0 {
        println!("NOTE: the CURRENT TICK ITSELF is initialized");
    }

    // INJECT 1 - solver-level evidence (soft verdicts, per-pool cause).
    let (zfo_ladder, ofz_ladder) = build_sequence(
        pool,
        cap.sqrt,
        cap.tick,
        cap.liquidity,
        &cap.ladder,
        POOL_SPACING,
        POOL_FEE,
    );
    let (zfo_wide, ofz_wide) = build_sequence(
        pool,
        cap.sqrt,
        cap.tick,
        cap.liquidity,
        &cap.wide,
        POOL_SPACING,
        POOL_FEE,
    );
    println!(
        "solver-level sequence lens: ladder zfo={zfo_ladder:?} ofz={ofz_ladder:?} | wide zfo={zfo_wide:?} ofz={ofz_wide:?}"
    );
    if zfo_ladder.is_none() && ofz_ladder.is_none() {
        println!("SOLVER: ladder map reproduces sequence_unavailable (both directions)");
    } else if zfo_ladder.is_none() {
        println!("SOLVER: zfo-only deficit (directional modeling gap)");
    } else if ofz_ladder.is_none() {
        println!("SOLVER: ofz-only deficit (directional modeling gap)");
    } else {
        println!("SOLVER: ladder map BUILDS - the deficit came from elsewhere");
    }
    if zfo_wide.is_some() || ofz_wide.is_some() {
        println!("SOLVER: wide capture builds a sequence (fix direction proven)");
    } else {
        println!(
            "SOLVER: even the FULL-CHAIN wide capture cannot build - the node's tick data for this pool is empty/anomalous"
        );
    }

    // INJECT 2 - production path through the policy-enforcing ingress.
    let rt = runtime_for(pool, token, POOL_SPACING, POOL_FEE, v2, &cap.ladder);
    let mut solver = BackrunSolver::new();
    let post = post_state(pool, cap.sqrt, cap.tick, cap.liquidity, POOL_SPACING);
    let affected = admit_extracted(&rt, &mut solver, &[post], head, "0xprobe-red", None);
    assert_eq!(affected.len(), 1, "the anchor admits under the ladder map");
    let v2_id = solver
        .admit_v2(&BackrunV2Pool {
            address: v2,
            token0: token,
            token1: WETH,
            reserve0: 100_000_000_000u128,
            reserve1: 100_000_000_000u128,
            fees: v2_fee_pair(),
        })
        .expect("the WETH-quoted V2 connector admits");
    let chain = probe_chain(
        pool,
        token,
        v2,
        affected[0].workspace_pool_id,
        v2_id,
        POOL_FEE,
    );
    let stats = solve_dfs_chains(&mut solver, &[chain], U256::ZERO);
    println!(
        "production (DB ladder map): declared={} evaluated={} rejects={:?}",
        stats.dfs_declared,
        stats.dfs_evaluated,
        stats
            .chains
            .iter()
            .map(|c| c.reject.as_ref().map(PathReject::label))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        stats.dfs_evaluated, 1,
        "production evaluates the Db-staged map (no clamped ladder seam remains)"
    );

    // GREEN: the complete wide map through the same verified ingress path.
    let rt = runtime_for(pool, token, POOL_SPACING, POOL_FEE, v2, &cap.wide);
    let mut solver = BackrunSolver::new();
    let post = post_state(pool, cap.sqrt, cap.tick, cap.liquidity, POOL_SPACING);
    let affected = admit_extracted(&rt, &mut solver, &[post], head, "0xprobe-green", None);
    assert_eq!(affected.len(), 1, "the anchor admits under the wide map");
    let v2_id = solver
        .admit_v2(&BackrunV2Pool {
            address: v2,
            token0: token,
            token1: WETH,
            reserve0: 100_000_000_000u128,
            reserve1: 100_000_000_000u128,
            fees: v2_fee_pair(),
        })
        .expect("the WETH-quoted V2 connector admits");
    let chain = probe_chain(
        pool,
        token,
        v2,
        affected[0].workspace_pool_id,
        v2_id,
        POOL_FEE,
    );
    let stats = solve_dfs_chains(&mut solver, &[chain], U256::ZERO);
    println!(
        "production (DB wide map): declared={} evaluated={} best={:?} rejects={:?}",
        stats.dfs_declared,
        stats.dfs_evaluated,
        stats.best.as_ref().map(|b| b.profit.to_string()),
        stats
            .chains
            .iter()
            .map(|c| c.reject.as_ref().map(PathReject::label))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        stats.dfs_evaluated, 1,
        "production evaluates the complete Db map"
    );
    assert!(
        stats.best.is_some(),
        "a real candidate is produced from the complete map"
    );
}

/// Pool 2's capture-only evidence (its live deficit appeared as a HOP inside
/// larger chains; the production-path injection is pool 1's job).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network: capture the second high-liquidity deficit pool"]
async fn sequence_deficit_pool2_capture() {
    let provider = Arc::new(
        AlloyProvider::new(
            &std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1")
                .unwrap_or_else(|_| String::from("http://host.containers.internal:8545")),
            3,
        )
        .await
        .expect("provider"),
    );
    let pool: Address = POOL2.parse().unwrap();
    let cap = capture(&provider, pool, POOL2_SPACING).await;
    println!(
        "captured pool2: tick={} liquidity={} ladder={} wide={}",
        cap.tick,
        cap.liquidity,
        cap.ladder.len(),
        cap.wide.len()
    );
    let nearest = cap
        .wide
        .keys()
        .map(|t| (*t - cap.tick).abs())
        .min()
        .unwrap_or(i32::MAX);
    println!("pool2 nearest initialized tick: {nearest} ticks from current");
    let (zfo_ladder, ofz_ladder) = build_sequence(
        pool,
        cap.sqrt,
        cap.tick,
        cap.liquidity,
        &cap.ladder,
        POOL2_SPACING,
        500,
    );
    let (zfo_wide, ofz_wide) = build_sequence(
        pool,
        cap.sqrt,
        cap.tick,
        cap.liquidity,
        &cap.wide,
        POOL2_SPACING,
        500,
    );
    println!(
        "pool2 sequence lens: ladder zfo={zfo_ladder:?} ofz={ofz_ladder:?} | wide zfo={zfo_wide:?} ofz={ofz_wide:?}"
    );
    // The fix's direction: the complete map models a sequence where the
    // narrow ladder window does not — production now stages the DB's map.
    assert!(
        zfo_wide.is_some() || ofz_wide.is_some(),
        "the complete wide map models a sequence (the production Db arm stages it)"
    );
}
