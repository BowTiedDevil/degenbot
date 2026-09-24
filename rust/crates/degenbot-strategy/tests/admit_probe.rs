//! Live verification (`--ignored`) that `PoolIngress` fills a V3 anchor's
//! replayed tick map and the production chain solves.
//!
//! Run:
//! ```text
//! DEGENBOT_RPC_HTTP_CHAINID_1=... DEGENBOT_DB_PATH=~/.local/state/degenbot/db/degenbot.db \
//!   cargo test -p degenbot-submission --test admit_probe -- --ignored --nocapture
//! ```
#![expect(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::sync::Arc;

use alloy::primitives::{address, Address, U256};
use degenbot_bot::bot_core::executor_hop::{V2FeePair, V2Fees};
use degenbot_db::connection::DegenbotDb;
use degenbot_pools::v3_state::ClSlotLayout;
use degenbot_simulation::sim::evm::journal_pools::{
    PoolFamily, PoolPostKind, PoolPostState, TypedPoolPost,
};
use degenbot_strategy::backrun_engine::{BackrunHopRef, BackrunSolver, BackrunV2Pool, LaneFamily};
use degenbot_strategy::backrun_strategy::{admit_extracted, solve_dfs_chains, WETH};
use degenbot_strategy::frame_pipeline::MarketContext;

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

const ANCHOR: Address = address!("11b815efb8f581194ae79006d24e0d814b7697f6");
const MID1: Address = address!("f641eafb5bce9568c4ff1079c58f36a7e8a6cd8d");
const MID2: Address = address!("c5a788f63e5d9cf2c324621eed51a98f85ae373b");

fn live_provider() -> Arc<degenbot_rpc::provider::AlloyProvider> {
    let rpc_url = std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1").unwrap();
    let client = alloy::rpc::client::ClientBuilder::default().http(rpc_url.parse().unwrap());
    let inner = alloy::providers::ProviderBuilder::default().connect_client(client);
    Arc::new(degenbot_rpc::provider::AlloyProvider::from_provider(
        Arc::new(inner),
    ))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network + DB: V3 anchor PoolIngress verification"]
async fn live_v3_anchor_ingress_solves_production_chain() {
    let provider = live_provider();
    let db_path = std::env::var("DEGENBOT_DB_PATH").unwrap();
    let (db, _state) = DegenbotDb::open(std::path::Path::new(&db_path)).unwrap();
    let mut index = degenbot_bot::connector_index::V2ConnectorIndex::load(&db, 1).unwrap();
    index.load_v3(&db, 1).unwrap();
    index.set_ranker(Arc::new(
        degenbot_bot::connector_index::OnChainLiquidityRanker::new(Arc::clone(&provider)),
    ));
    let rt = market_context(
        Some(std::sync::Arc::new(
            degenbot_bot::bot_core::RouteRegistry::new(index),
        )),
        Some(std::sync::Arc::new(db)),
    );
    let head = provider.get_block_number().await.unwrap();

    let anchor = *rt.index().unwrap().v3_edge_by_address(ANCHOR).unwrap();
    let (sqrt, tick, liq) = degenbot_rpc::abi::fetch_v3_slot0_liquidity(&provider, &ANCHOR, None)
        .await
        .unwrap();
    let tick = i32::try_from(tick).unwrap();

    // Replayed post-state: price/liquidity staged, NO touched ticks.
    let post = PoolPostState {
        address: ANCHOR,
        family: PoolFamily::V3 {
            layout: ClSlotLayout::UniswapV3,
            tick_spacing: anchor.tick_spacing,
            current_tick_hint: Some(tick),
        },
        kind: PoolPostKind::Typed(TypedPoolPost::V3 {
            sqrt_price_x96: Some(sqrt),
            tick: Some(tick),
            liquidity: Some(u128::try_from(liq).unwrap()),
            touched_ticks: Vec::new(),
        }),
    };

    let mut solver = BackrunSolver::new();
    let affected = admit_extracted(&rt, &mut solver, &[post], head, "0xlive", None);
    assert_eq!(affected.len(), 1, "the production V3 anchor admits");
    let a = &affected[0];

    // The two production V2 mids, admitted from live reserves.
    let mut chain = vec![BackrunHopRef {
        pool_id: a.workspace_pool_id,
        pool: ANCHOR,
        token0: a.token0,
        token1: a.token1,
        zfo: a.token0 == WETH,
        family: LaneFamily::V3 { fee: anchor.fee },
    }];
    let mut in_id = if a.token0 == WETH {
        rt.token_id(a.token1).unwrap()
    } else {
        rt.token_id(a.token0).unwrap()
    };
    for mid in [MID1, MID2] {
        let edge = *rt.index().unwrap().edge_by_address(mid).unwrap();
        let m0 = rt.token_addr(edge.token0_id).unwrap();
        let m1 = rt.token_addr(edge.token1_id).unwrap();
        let (r0, r1) = degenbot_rpc::abi::fetch_v2_reserves(&provider, &mid, None)
            .await
            .unwrap();
        let id = solver
            .admit_v2(&BackrunV2Pool {
                address: mid,
                token0: m0,
                token1: m1,
                reserve0: u128::try_from(r0).unwrap(),
                reserve1: u128::try_from(r1).unwrap(),
                fees: v2_fee_pair(),
            })
            .unwrap();
        let (zfo, out_id) = if edge.token0_id == in_id {
            (true, edge.token1_id)
        } else {
            (false, edge.token0_id)
        };
        chain.push(BackrunHopRef {
            pool_id: id,
            pool: mid,
            token0: m0,
            token1: m1,
            zfo,
            family: LaneFamily::V2 { fees: v2_fees() },
        });
        in_id = out_id;
    }
    println!("chain: {chain:?}");
    let stats = solve_dfs_chains(&mut solver, std::slice::from_ref(&chain), U256::ZERO);
    println!("stats: {stats:?}");
    let idx = solver.declare_hops(&chain);
    println!("resolve_debug: {}", solver.resolve_debug(idx));
    assert_eq!(stats.dfs_evaluated, 1, "the production chain evaluates");
    assert!(stats.best.is_some(), "the production chain profits");
}
