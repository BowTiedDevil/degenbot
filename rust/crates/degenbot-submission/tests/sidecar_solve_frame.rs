//! Live-colored leg -> overlay eval -> staged candidate -> sim gate -> bid
//! decision (the S7KG7E end-to-end frame, against the provided node with the
//! pre-funded gate-2 executor). Ignored by default.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use alloy::primitives::{address, U256};
use degenbot_bot::bot_core::post_target::V2FeeParams;
use degenbot_bot::sidecar::{decide, Decision, SidecarConfig};
use degenbot_bot::sidecar_solve::{fetch_v2_reserves, stage_and_eval_v2};
use degenbot_decoders::target_classifier::{classify, PoolProtocol, RouterRegistry, SwapLeg};
use degenbot_rpc::provider::AlloyProvider;

#[tokio::test]
#[ignore = "live network"]
async fn live_weth_pair_leg_stages_through_the_full_frame() {
    let rpc_url = std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1").unwrap();
    let client = alloy::rpc::client::ClientBuilder::default().http(rpc_url.parse().unwrap());
    let provider = AlloyProvider::from_provider(Arc::new(
        alloy::providers::ProviderBuilder::default().connect_client(client),
    ));

    // USDC/WETH pair (0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc) - a live V2
    // color the resolver can pull without any registry priming.
    let pair = address!("b4e16d0168e52d35cacd2c6185b44281ec28c9dc");
    let reserves = fetch_v2_reserves(&provider, pair).await.unwrap();
    assert!(reserves.0 > 0 && reserves.1 > 0, "live pair has reserves");

    // Color a target leg: WETH -> USDC through the pair, one basis of input.
    let leg = SwapLeg {
        protocol: PoolProtocol::V2,
        pool: Some(pair),
        token_in: Some(address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2")),
        token_out: Some(address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")),
        value: U256::ZERO,
        amount_in: Some(U256::from(1_000_000_000_000u64)),
        amount_out_min: None,
        amount_out: None,
        amount_in_max: None,
        hops: 1,
    };
    // Deep-pool backrun eval: unprofitable against the gas floor is the
    // expected live outcome - the fixture asserts the STAGING path ran.
    let eval = stage_and_eval_v2(
        &leg,
        (reserves.0, reserves.1),
        V2FeeParams {
            gamma_numer: 997,
            fee_denom: 1000,
        },
        U256::from(1_000_000_000_000_000u64),
    );
    // Either staged with net > floor (rare) or unprofitable; NOT a NotV2/NonWeth skip.
    assert!(!matches!(
        eval,
        Err(degenbot_bot::sidecar_solve::SolveSkip::NotV2)
    ));
    assert!(!matches!(
        eval,
        Err(degenbot_bot::sidecar_solve::SolveSkip::NonWethPair)
    ));

    // The decision layer trusts the eval: any staged candidate still needs a
    // sim pass + legal bid mode to become a Bid.
    let cfg = SidecarConfig {
        stream_url: String::new(),
        rpc_url: String::new(),
        key_file: None,
        bid_mode: false,
        budget_wei: U256::ZERO,
        max_bundle_wei: U256::from(500_000_000_000_000u64),
        stop_file: "/nonexistent".into(),
        stale_ms: 1500,
    };
    let class = classify(pair, &[0u8; 4], &RouterRegistry::mainnet());
    let d = decide(
        &cfg,
        false,
        &class,
        eval.is_ok(),
        U256::from(1),
        10,
        U256::ZERO,
    );
    assert!(!matches!(d, Decision::Bid { .. }), "observe-only default");
}
