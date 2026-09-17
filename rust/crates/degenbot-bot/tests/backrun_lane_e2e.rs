//! Live whole-pipeline e2e for the sidecar connector lane (epic DFYDYI):
//! DB index -> staged affected pair -> connector admission -> envelope-
//! gated mixed solve -> candidate compose -> the exact-sim oracle round
//! trip against the provided reth node.
//!
//! Ignored by default (network + local DB dependent); run:
//! ```text
//! DEGENBOT_RPC_HTTP_CHAINID_1=... DEGENBOT_DB_PATH=~/.config/degenbot/degenbot.db \
//!   cargo test -p degenbot-bot --test backrun_lane_e2e -- --ignored --nocapture
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::too_many_lines,
    reason = "diagnostic e2e probe: narrates the whole-lane pipeline"
)]

use std::collections::HashMap;
use std::sync::Arc;

use alloy::primitives::{address, U256};
use alloy::rpc::types::eth::simulate::{SimBlock, SimulatePayload};
use degenbot_bot::sidecar_engine::{build_candidate_calldata, ConnectorLaneCtx, SidecarSolver};
use degenbot_bot::sidecar_paths::V2ConnectorIndex;
use degenbot_rpc::provider::AlloyProvider;

fn live_provider() -> Arc<AlloyProvider> {
    let rpc_url = std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1").unwrap();
    let client = alloy::rpc::client::ClientBuilder::default().http(rpc_url.parse().unwrap());
    let inner = alloy::providers::ProviderBuilder::default().connect_client(client);
    Arc::new(AlloyProvider::from_provider(Arc::new(inner)))
}

const WETH: alloy::primitives::Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
/// The canonical USDC/WETH V2 pair (affected pool under a synthetic target).
const USDC_WETH_V2: alloy::primitives::Address =
    address!("b4e16d0168e52d35cacd2c6185b44281ec28c9dc");
const USDC: alloy::primitives::Address = address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");

/// Synthetic target leg: WETH -> USDC 10 WETH (the class of frame the lane
/// replays); reserves come live from the pair.
#[tokio::test]
#[ignore = "live network + local DB: the whole-lane pipeline probe"]
async fn lane_pipeline_reaches_exact_sim() {
    let provider = live_provider();
    let db_path = std::env::var("DEGENBOT_DB_PATH").unwrap();
    let (db, _) =
        degenbot_db::connection::DegenbotDb::open(std::path::Path::new(&db_path)).unwrap();
    let mut index = V2ConnectorIndex::load(&db, 1).unwrap();
    index.load_v3(&db, 1).unwrap();
    println!("index edges: {} v3: {}", index.len(), index.v3_len());

    let p_edge = index
        .edge_by_address(USDC_WETH_V2)
        .expect("canonical USDC/WETH pair in the DB index");

    let mut ids = HashMap::new();
    let m = db.fetch_token_ids_by_address(1, &[WETH, USDC]).unwrap();
    ids.extend(m);

    let (r0, r1) = degenbot_bot::sidecar_solve::fetch_v2_reserves(&provider, USDC_WETH_V2)
        .await
        .unwrap();
    println!("live reserves (token0, token1): {r0} {r1}");

    let leg = degenbot_decoders::target_classifier::SwapLeg {
        protocol: degenbot_decoders::target_classifier::PoolProtocol::V2,
        pool: Some(USDC_WETH_V2),
        token_in: Some(WETH),
        token_out: Some(USDC),
        value: U256::ZERO,
        // Large enough to genuinely dislocate the pool's marginal price so
        // the lane (and, on false prices, the oracle) exercises end to end.
        amount_in: Some(U256::from(3_000u128) * U256::from(10u128).pow(U256::from(18u8))), // 3000 WETH
        amount_out_min: None,
        amount_out: None,
        amount_in_max: None,
        hops: 0,
    };

    let mut solver = SidecarSolver::new();
    // Diagnostics: the lane's token-id joins.
    let weth_id = ids[&WETH];
    let usdc_id = ids[&USDC];
    eprintln!(
        "joined: weth_id={weth_id} usdc_id={usdc_id} edge=({},{}) edge_pool={}",
        p_edge.token0_id, p_edge.token1_id, p_edge.pool_id
    );
    let direct = index.connectors(usdc_id, weth_id, p_edge.pool_id, 8).await;
    eprintln!("direct connectors: {}", direct.len());
    let grade = solver
        .run_connector_lane(
            &provider,
            &ConnectorLaneCtx {
                index: &index,
                ids: &ids,
                leg: &leg,
                p_family: degenbot_bot::sidecar_engine::LaneFamily::V2,
                staged_p: None,
                pair_reserves: (r0, r1),
                cap: 8,
                head: provider.get_block_number().await.unwrap_or(1),
                gas_floor_wei: U256::from(50_000_000_000_000u64),
            },
        )
        .await;
    println!(
        "grade: connectors={} declared={} evaluated={} admit_failures={} best_profit={}",
        grade.connectors,
        grade.paths_declared,
        grade.paths_evaluated,
        grade.admit_failures,
        grade.best_profit
    );
    assert!(
        grade.connectors > 0,
        "the canonical pair must surface connectors"
    );
    assert!(
        grade.paths_declared >= 2,
        "both drift directions must be declared"
    );
    if grade.paths_evaluated == 0 {
        let last = solver.path_count();
        for idx in last.saturating_sub(grade.paths_declared)..last {
            eprintln!("path {idx}: {}", solver.resolve_debug(idx));
        }
    }

    // The composed artifact + the oracle round trip (either a passing sim on
    // a genuinely profitable shape, or an honest revert -- both pin the
    // pipeline: compose -> RPC -> block parse).
    if let Some(cand) = grade.best {
        println!(
            "candidate: profit={} input={} hops={}",
            cand.profit,
            cand.optimal_input,
            cand.hops.len()
        );
        let cd = build_candidate_calldata(
            &cand,
            address!("30b28ed8aa581fbc0191c3b532b0697773070e97"),
            WETH,
            1_000,
        )
        .expect("all-V2 2-hop candidate composes");
        let blocks = provider
            .eth_simulate_v1(
                &SimulatePayload {
                    block_state_calls: vec![SimBlock {
                        calls: vec![alloy::rpc::types::TransactionRequest {
                            from: Some(alloy::primitives::address!(
                                "5c603b8a137a40426e0ddfa981ec10c245af080e"
                            )),
                            to: Some(
                                alloy::primitives::address!(
                                    "30b28ed8aa581fbc0191c3b532b0697773070e97"
                                )
                                .into(),
                            ),
                            input: alloy::rpc::types::TransactionInput::new(cd),
                            gas: Some(800_000),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                alloy::eips::BlockId::latest(),
            )
            .await
            .expect("oracle round trip succeeds");
        let status = blocks
            .first()
            .and_then(|b| b.calls.first())
            .map(|c| c.status)
            .unwrap();
        println!("exact-sim status: {status}");
    } else {
        println!(
            "no executable candidate at head state (unprofitable shape) -- grade-only verified"
        );
    }
    let _ = p_edge; // resolved for the adjacency sanity above
}
