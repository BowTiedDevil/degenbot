//! Live verification (`--ignored`) that the real frame scratch view fills a
//! V3 anchor's empty replayed tick map and the production chain solves.
//!
//! Run:
//! ```text
//! DEGENBOT_RPC_HTTP_CHAINID_1=... DEGENBOT_DB_PATH=~/.local/state/degenbot/db/degenbot.db \
//!   cargo test -p degenbot-submission --test admit_probe -- --ignored --nocapture
//! ```
#![expect(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::sync::Arc;

use alloy::primitives::{address, Address, U256};
use degenbot_bot::bot_core::SimAnchorState;
use degenbot_bot::sidecar_engine::{LaneFamily, SidecarHopRef, SidecarSolver, SidecarV2Pool};
use degenbot_db::connection::DegenbotDb;
use degenbot_pools::v3_state::ClSlotLayout;
use degenbot_simulation::sim::evm::journal_pools::{
    PoolFamily, PoolPostKind, PoolPostState, TypedPoolPost,
};
use degenbot_submission::backrun_strategy::{admit_extracted, solve_dfs_chains, WETH};
use degenbot_submission::frame_pipeline::{build_block_handle, MarketContext};

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
#[ignore = "live network + DB: V3 anchor scratch tick-window verification"]
#[expect(
    clippy::too_many_lines,
    reason = "the live diagnostic reads top-to-bottom"
)]
async fn live_v3_anchor_scratch_window_solves_production_chain() {
    let provider = live_provider();
    let db_path = std::env::var("DEGENBOT_DB_PATH").unwrap();
    let (db, _state) = DegenbotDb::open(std::path::Path::new(&db_path)).unwrap();
    let mut index = degenbot_bot::sidecar_paths::V2ConnectorIndex::load(&db, 1).unwrap();
    index.load_v3(&db, 1).unwrap();
    index.set_ranker(Arc::new(
        degenbot_bot::sidecar_paths::OnChainLiquidityRanker::new(Arc::clone(&provider)),
    ));
    let rt = MarketContext::new(
        1,
        Some(std::sync::Arc::new(
            degenbot_bot::bot_core::RouteRegistry::new(index),
        )),
        Some(db),
        8,
    );
    let head = provider.get_block_number().await.unwrap();

    let anchor = *rt.index().unwrap().v3_edge_by_address(ANCHOR).unwrap();
    let (sqrt, tick, liq) = degenbot_rpc::abi::fetch_v3_slot0_liquidity(&provider, &ANCHOR, None)
        .await
        .unwrap();
    let tick = i32::try_from(tick).unwrap();

    // The anchor's frame-replay scratch: the same chain view the frames use.
    let anchor_state: &'static SimAnchorState = Box::leak(Box::new(SimAnchorState::default()));
    let mut handle = build_block_handle(&provider, head, &rt.warm_cache, anchor_state)
        .await
        .expect("replay handle builds");
    let scratch = handle.scratch_evm().expect("scratch EVM");

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

    let mut solver = SidecarSolver::new();
    let affected = admit_extracted(
        &rt,
        &mut solver,
        &[post],
        head,
        "0xlive",
        Some(scratch.ext()),
    );
    assert_eq!(affected.len(), 1, "the production V3 anchor admits");
    let a = &affected[0];

    // The two production V2 mids, admitted from live reserves.
    let mut chain = vec![SidecarHopRef {
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
            .admit_v2(&SidecarV2Pool {
                address: mid,
                token0: m0,
                token1: m1,
                reserve0: u128::try_from(r0).unwrap(),
                reserve1: u128::try_from(r1).unwrap(),
            })
            .unwrap();
        let (zfo, out_id) = if edge.token0_id == in_id {
            (true, edge.token1_id)
        } else {
            (false, edge.token0_id)
        };
        chain.push(SidecarHopRef {
            pool_id: id,
            pool: mid,
            token0: m0,
            token1: m1,
            zfo,
            family: LaneFamily::V2,
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
