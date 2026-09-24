//! Replay-driven e2e for the frame pipeline: a pending-tx frame is staged
//! by REPLAY (the replay seam's touched set + journalled words — no
//! classifier, no analytic post-target estimate), processed end to end
//! (replay → extract → workspace admission → discovery → solve → compose →
//! the `eth_callMany` bundle gate → decision), and asserted on the decision
//! surface: `Decision::Bid` with composed `execute()` calldata and the bid
//! equal to the floor of the 98% bribe share of the exact-solve profit; a
//! reverted target observes with the truthful `reverted` reason.
//!
//! Live (network + local DB dependent); run:
//! ```text
//! DEGENBOT_RPC_HTTP_CHAINID_1=... DEGENBOT_DB_PATH=~/.local/state/degenbot/db/degenbot.db \
//!   cargo test -p degenbot-bot --test backrun_lane_e2e -- --ignored --nocapture
//! ```

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    clippy::print_stdout,
    reason = "diagnostic e2e probe: narrates the whole-pipeline fixture"
)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use alloy::primitives::{address, keccak256, Bytes, U256};
use degenbot_bot::bot_core::SimAnchorState;
use degenbot_rpc::backrun_feed::BackrunFeedEvent;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_strategy::backrun::{BackrunConfig, Decision, MevblockerBackrun};
use degenbot_strategy::backrun_strategy::BackrunStrategy;
use degenbot_strategy::frame_pipeline::{
    build_block_handle, process_frame, MarketContext, PipelineConfig,
};

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

const WETH: alloy::primitives::Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
const USDC: alloy::primitives::Address = address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
const UNISWAP_V2_ROUTER: alloy::primitives::Address =
    address!("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D");
/// The canonical USDC/WETH V2 pair: the fixture's affected pool.
const USDC_WETH_V2: alloy::primitives::Address =
    address!("b4e16d0168e52d35cacd2c6185b44281ec28c9dc");
/// A deep-balance stand-in sender: the replay relaxes balances, but the
/// bundle sim must MOVE real tokens — the frame's ETH value routes through
/// the router's wrap, so the sim's `from` needs the dislocation's size.
const FRAME_SENDER: alloy::primitives::Address =
    address!("F977814e90dA44bFA03b6295A0616a897441aceC");
const EXECUTOR: alloy::primitives::Address = address!("30b28ed8aa581fbc0191c3b532b0697773070e97");
const OPERATOR: alloy::primitives::Address = address!("5c603b8a137A40426E0dDFA981EC10c245AF080e");
/// `execute(bytes,uint256)`
const EXECUTE_SELECTOR: [u8; 4] = [0xab, 0x58, 0x98, 0xe8];
const BRIBE_BIPS: u16 = 9_800;
const GAS_FLOOR_WEI: u64 = 50_000_000_000_000;

fn live_provider() -> Arc<AlloyProvider> {
    let rpc_url = std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1").unwrap();
    let client = alloy::rpc::client::ClientBuilder::default().http(rpc_url.parse().unwrap());
    let inner = alloy::providers::ProviderBuilder::default().connect_client(client);
    Arc::new(AlloyProvider::from_provider(Arc::new(inner)))
}

fn sim_client() -> alloy::rpc::client::RpcClient {
    let rpc_url = std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1").unwrap();
    alloy::rpc::client::ClientBuilder::default().http(rpc_url.parse().unwrap())
}

fn live_db() -> degenbot_db::connection::DegenbotDb {
    let db_path = std::env::var("DEGENBOT_DB_PATH").unwrap();
    degenbot_db::connection::DegenbotDb::open(std::path::Path::new(&db_path))
        .unwrap()
        .0
}

/// The trace sink the pipeline writes its composed-profit events to. Both
/// fixtures append; each filters by its own frame hash.
fn trace_sink() -> &'static PathBuf {
    static SINK: OnceLock<PathBuf> = OnceLock::new();
    SINK.get_or_init(|| {
        let p = std::env::temp_dir().join(format!("frame_e2e_trace_{}.jsonl", std::process::id()));
        std::fs::remove_file(&p).ok();
        let mut cfg = degenbot_config::BotConfig::default();
        cfg.logging.trace_jsonl = Some(p.clone());
        let _ = degenbot_config::holder::install(std::sync::Arc::new(cfg));
        p
    })
}

fn bid_config() -> (BackrunConfig, PipelineConfig) {
    let mut cfg =
        MevblockerBackrun::from_config(&degenbot_config::BotConfig::default(), String::new())
            .into_config();
    cfg.bid_mode = true;
    cfg.budget_wei = U256::from(10_000_000u128) * U256::from(10u64).pow(U256::from(18u8));
    cfg.max_bundle_wei = U256::from(1_000_000u128) * U256::from(10u64).pow(U256::from(18u8));
    cfg.stop_file = PathBuf::from("/nonexistent-frame-e2e-stop");
    let pl = PipelineConfig {
        exec: EXECUTOR,
        owner: OPERATOR,
        bribe_bips: BRIBE_BIPS,
        wallet_gas_cost_wei: Arc::new(std::sync::atomic::AtomicU64::new(
            // Live-scale wallet gas burn so the e2e path exercises the
            // net-of-gas gate exactly as the driver prices it.
            600_000_000_000,
        )),
        gas_floor_wei: U256::from(GAS_FLOOR_WEI),
        fixture_mode: false,
    };
    (cfg, pl)
}

/// `swapExactETHForTokens(0, [WETH, USDC], sender, max)`: the class of
/// pending frame the feed carries, sized to genuinely dislocate the
/// canonical pair, so a backrun cycle clears the envelope floor on the
/// replayed post-state.
fn dislocating_frame(value_eth: u128, nonce: u64) -> BackrunFeedEvent {
    let data = hex_str_to_bytes(concat!(
        "0x7ff36ab5",
        // amountOutMin = 0
        "0000000000000000000000000000000000000000000000000000000000000000",
        // path offset
        "0000000000000000000000000000000000000000000000000000000000000080",
        // to = the frame sender
        "0000000000000000000000001111111111111111111111111111111111111111",
        // deadline = max
        "00000000000000000000000000000000000000000000000000000000ffffffff",
        // path length = 2
        "0000000000000000000000000000000000000000000000000000000000000002",
        // path = [WETH, USDC]
        "000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
        "000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
    ));
    let value = U256::from(value_eth) * U256::from(10u64).pow(U256::from(18u8));
    frame_event(value, data, nonce)
}

/// `USDC.transfer(sender, 1e12)`: the token's own stake (a transitory
/// pool, not the fixture sender) has nowhere near this balance, so the
/// transfer reverts, the frame's replay reports `Reverted`, and the
/// pipeline must observe with the truthful reason — never compose a bid.
fn reverting_frame(nonce: u64) -> BackrunFeedEvent {
    let mut data = vec![0xa9, 0x05, 0x9c, 0xbb];
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(FRAME_SENDER.as_slice());
    // 1e12 raw = 1,000,000 USDC (6 decimals).
    data.extend_from_slice(&[
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xE8, 0xD4, 0xA5, 0x10,
        0, 0, 0, 0, 0, 0,
    ]);
    frame_event(U256::ZERO, Bytes::from(data), nonce)
}

fn frame_event(value: U256, data: Bytes, nonce: u64) -> BackrunFeedEvent {
    // Derived from the payload so the trace filter key is stable and
    // unique per fixture invocation.
    let hash = keccak256(data.clone());
    BackrunFeedEvent {
        chain_id: 1,
        from: FRAME_SENDER,
        to: Some(UNISWAP_V2_ROUTER),
        value,
        data,
        gas: 500_000,
        max_fee_per_gas: 50_000_000_000,
        max_priority_fee_per_gas: 1_000_000_000,
        nonce,
        hash,
        access_list: serde_json::Value::Null,
        tx_type: 2,
        received_unix_ms: 0,
    }
}

fn hex_str_to_bytes(s: &'static str) -> Bytes {
    let s = s.strip_prefix("0x").unwrap();
    Bytes::from(
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect::<Vec<u8>>(),
    )
}

async fn live_nonce(provider: &AlloyProvider) -> u64 {
    provider
        .get_transaction_count(&FRAME_SENDER, None)
        .await
        .unwrap_or(0)
}

/// Read the pipeline's `composed` trace event for `tx`: the exact-solve
/// profit the bid ladder applies to (the number the bid formula consumes).
fn composed_profit(tx: alloy::primitives::B256) -> Option<u128> {
    let text = std::fs::read_to_string(trace_sink()).ok()?;
    text.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["kind"] == "composed" && v["tx"] == format!("0x{}", alloy::hex::encode(tx)))
        .and_then(|v| {
            v["profit"]
                .as_str()
                .and_then(|p| p.parse::<u128>().ok())
                .or_else(|| v["profit"].as_u64().map(u128::from))
        })
}

/// Decode the composed `execute(bytes,uint256)` config word (head word 1):
/// the packed tuple of check mode, bribe share, recipient index, and the
/// profit-expectation ceiling.
fn composed_config(cd: &Bytes) -> (u8, u16, u8, U256) {
    assert_eq!(
        cd[0..4],
        EXECUTE_SELECTOR,
        "the composed artifact is an execute() call"
    );
    let raw: [u8; 32] = cd[4 + 32..4 + 64].try_into().unwrap();
    degenbot_submission::bundle::decode_config_word(U256::from_be_bytes(raw))
}

async fn runtime(
    provider: &Arc<AlloyProvider>,
) -> (MarketContext, u64, HashMap<alloy::primitives::Address, u64>) {
    let db = live_db();
    let ids: HashMap<alloy::primitives::Address, u64> = db
        .fetch_token_ids_by_address(1, &[WETH, USDC])
        .unwrap()
        .into_iter()
        .collect();
    assert!(
        ids.contains_key(&WETH) && ids.contains_key(&USDC),
        "WETH/USDC joined from the DB"
    );
    let mut index = degenbot_bot::connector_index::V2ConnectorIndex::load(&db, 1).unwrap();
    index.load_v3(&db, 1).unwrap();
    index.set_ranker(Arc::new(
        degenbot_bot::connector_index::OnChainLiquidityRanker::new(Arc::clone(provider)),
    ));
    assert!(
        index.edge_by_address(USDC_WETH_V2).is_some(),
        "the canonical USDC/WETH pair must sit in the DB index"
    );
    let head = provider.get_block_number().await.unwrap();
    let rt = market_context(
        Some(Arc::new(degenbot_bot::bot_core::RouteRegistry::new(index))),
        Some(Arc::new(live_db())),
    );
    (rt, head, ids)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network + local DB: replay-staged Bid-branch e2e"]
async fn frame_pipeline_replay_staging_bids_with_composed_calldata() {
    let _ = trace_sink(); // arm the pipeline's trace sink before any frame
    let provider = live_provider();
    let (mut rt, head, _ids) = runtime(&provider).await;
    let anchor = SimAnchorState::default();
    let mut handle = Some(
        build_block_handle(&provider, head, &rt.warm_cache, &anchor)
            .await
            .expect("the per-block replay handle builds against the live chain"),
    );
    let (cfg, pl) = bid_config();
    let nonce = live_nonce(&provider).await;
    let ev = dislocating_frame(300, nonce);
    println!("frame hash 0x{}", alloy::hex::encode(ev.hash));

    let mut strategy = BackrunStrategy::new();
    let artifacts = process_frame(
        &mut strategy,
        &mut rt,
        &provider,
        &sim_client(),
        &cfg,
        &pl,
        &mut handle,
        &ev,
        head,
        U256::ZERO,
    )
    .await;

    println!("decision: {:?}", artifacts.decision);
    println!("requested_bid: {}", artifacts.requested_bid);
    println!("stages: {}", artifacts.stages.to_json());

    // ── the Bid branch (acceptance: frame in → gate → Decision::Bid) ──
    let bid_wei = match &artifacts.decision {
        Decision::Bid { bid_wei } => *bid_wei,
        other => panic!("expected a replay-staged Bid, got {other:?}"),
    };
    let cd = artifacts
        .submit_calldata
        .clone()
        .expect("a Bid carries the composed calldata");
    assert!(
        artifacts.requested_bid >= U256::from(1),
        "bids are positive"
    );

    // The decoded exact-solve profit is the ONLY bid input: the formula
    // b = floor(profit × bribe_bips / 10000) — floor via truncation, the
    // composed 1-wei floor, capped by the (here unbinding) bundle ceiling.
    let profit = composed_profit(ev.hash).expect("the pipeline recorded the composed profit");
    let expected = U256::from(profit) * U256::from(u64::from(BRIBE_BIPS)) / U256::from(10_000u16);
    let expected = expected.max(U256::from(1)).min(cfg.max_bundle_wei);
    assert_eq!(
        bid_wei, expected,
        "bid {bid_wei} must be the floor 98% share of the exact-solve profit {profit}"
    );

    // The bribe share rides the composed config exactly once (check mode 1
    // = WETH+ETH true-delta seatbelt, recipient 0 = coinbase).
    let (check_mode, bips, recipient, expected_value) = composed_config(&cd);
    assert_eq!(check_mode, 1);
    assert_eq!(
        bips, BRIBE_BIPS,
        "the on-chain bribe share matches the ladder"
    );
    assert_eq!(recipient, 0, "coinbase bribe");
    assert!(expected_value.is_zero());
    println!("bid ladder: profit {profit} wei -> bid {bid_wei} wei");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network + local DB: reverted-target Observe-branch e2e"]
async fn frame_pipeline_reverted_target_observes_truthfully() {
    let _ = trace_sink(); // arm the pipeline's trace sink before any frame
    let provider = live_provider();
    let (mut rt, head, _ids) = runtime(&provider).await;
    let anchor = SimAnchorState::default();
    let mut handle = Some(
        build_block_handle(&provider, head, &rt.warm_cache, &anchor)
            .await
            .expect("the per-block replay handle builds against the live chain"),
    );
    let (cfg, pl) = bid_config();
    let nonce = live_nonce(&provider).await;
    let ev = reverting_frame(nonce);

    let mut strategy = BackrunStrategy::new();
    let artifacts = process_frame(
        &mut strategy,
        &mut rt,
        &provider,
        &sim_client(),
        &cfg,
        &pl,
        &mut handle,
        &ev,
        head,
        U256::ZERO,
    )
    .await;

    assert_eq!(
        artifacts.decision,
        Decision::Observe { reason: "reverted" },
        "a reverted target observes with the truthful reason"
    );
    assert!(artifacts.submit_calldata.is_none());
    assert!(artifacts.requested_bid.is_zero());
    assert!(
        artifacts.stages.replay_us > 0 || artifacts.stages.extract_us > 0,
        "the frame traversed the pipeline stages"
    );
}
