//! The standalone backrun sidecar binary (epic 6ZOGIT, task OQQCQO).
//!
//! Loop: `MEVBlocker` feed -> hub classification -> exact-sim oracle
//! (`eth_simulateV1`) -> [`degenbot_bot::sidecar::decide`] -> bid through the
//! submission leaf ([`dispatch_and_submit`], bid mode exclusively via the
//! private-RPC extra broadcast). Observe-only default; all state is local; zero
//! touches to the live engine block pump (FORK-1).
//!
//! Run (observe-only): `SIDECAR_RPC_URL=$RPC cargo run --bin backrun_sidecar`.
//! Bid mode adds `SIDECAR_BID_MODE=1`, `SIDECAR_BUDGET_WEI=<wei>` and
//! `SIDECAR_KEY_FILE=<hex path>`. Bids are MEVBlocker-specific per
//! docs.mevblocker.io/how-to/searchers/bid: `eth_sendBundle` on the
//! searcher WS with `txs = [targetHash, signed backrun]`, block-pinned.
//! The signed backrun NEVER touches the public mempool or another relay.
//! Kill switch: `touch /tmp/degenbot-sidecar-STOP`.

#![expect(
    clippy::expect_used,
    reason = "bin: fatal config failures exit the process loudly"
)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::eips::BlockId;
use alloy::primitives::{Address, Bytes, U256};
use degenbot_bot::sidecar::{decide, Decision, SidecarConfig};
use degenbot_decoders::target_classifier::{classify, PoolProtocol, RouterRegistry, TargetClass};
use degenbot_rpc::backrun_feed::{BackrunFeed, BackrunFeedConfig};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_submission::bundle::{
    backrun_sim_call, eth_call_many_bundle_sim_params, MEVBLOCKER_STREAM_URL,
};
use degenbot_submission::dispatcher::Dispatcher;
use degenbot_submission::monitor::ReceiptProbe;
use degenbot_submission::signer::TxSigner;
use degenbot_submission::submit::{dispatch_and_submit, BundleTarget, SubmitCandidate};

// Console subscriber so observe-mode frames are visible; structured (OTel)
// export stays the operator's layering choice via the bot crate.
fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();
}

/// Provider-backed receipt probe (the sidecar's own node join; the `PyReceiptProbe`
/// twin is the python driver's -- this keeps the sidecar standalone).
struct SidecarProbe {
    provider: Arc<AlloyProvider>,
}

impl ReceiptProbe for SidecarProbe {
    fn receipt_found(
        &self,
        tx_hash: alloy::primitives::B256,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = degenbot_submission::SubmissionResult<bool>>
                + Send
                + '_,
        >,
    > {
        let provider = Arc::clone(&self.provider);
        Box::pin(async move {
            let rec = provider
                .get_transaction_receipt(&tx_hash.to_string())
                .await
                .map_err(|e| degenbot_submission::SubmissionError::MonitorProbe(format!("{e}")))?;
            Ok(rec.is_some())
        })
    }
}

/// Resolved (pool, fee, `tick_spacing`) per (`tokenIn`, `tokenOut`) pair.
type V3PoolCache =
    std::sync::Mutex<std::collections::HashMap<(Address, Address), (Address, u32, i32)>>;

/// The Uniswap V3 factory (the pool CREATE2 deployer on mainnet).
const V3_FACTORY: alloy::primitives::Address =
    alloy::primitives::address!("1F98431c8aD98523631AE4a59f267346ea31F984");

fn pool_v3_cache_new() -> V3PoolCache {
    std::sync::Mutex::new(std::collections::HashMap::new())
}

/// Resolve the V3 pool for a router decoded (tokenIn, tokenOut) by probing
/// the four standard fee tiers' CREATE2 addresses with a slot0 call (the one
/// that returns state is the pool). Cached per token pair per process.
async fn resolve_v3_pool(
    provider: &degenbot_rpc::provider::AlloyProvider,
    cache: &V3PoolCache,
    token_in: Address,
    token_out: Address,
) -> Option<(Address, u32, i32)> {
    {
        #[expect(clippy::expect_used, reason = "poisoned std sync-guard = process bug")]
        let hit = cache
            .lock()
            .expect("v3 pool cache")
            .get(&(token_in, token_out))
            .copied();
        if hit.is_some() {
            return hit;
        }
    }
    let init_hash = degenbot_uniswap::deployments::resolve_v3_init_hash(1, V3_FACTORY);
    for fee in [100u32, 500, 3_000, 10_000] {
        let pool = degenbot_uniswap::create2::compute_v3_address(
            V3_FACTORY,
            token_in.min(token_out),
            token_in.max(token_out),
            fee,
            init_hash,
        );
        if degenbot_rpc::abi::fetch_v3_slot0_liquidity(provider, &pool, None)
            .await
            .is_err()
        {
            continue;
        }
        let tick_spacing = fetch_v3_tick_spacing(provider, &pool).await?;
        #[expect(clippy::expect_used)]
        let mut guard = cache.lock().expect("v3 pool cache");
        guard.insert((token_in, token_out), (pool, fee, tick_spacing));
        return Some((pool, fee, tick_spacing));
    }
    None
}

/// `tickSpacing()` (selector 0xd0c93a7c) -> int24 sovereign per pool.
async fn fetch_v3_tick_spacing(
    provider: &degenbot_rpc::provider::AlloyProvider,
    pool: &Address,
) -> Option<i32> {
    let ret = provider
        .eth_call(
            pool,
            alloy::primitives::Bytes::from([0xd0, 0xc9, 0x3a, 0x7c, 0, 0, 0, 0][..4].to_vec()),
            None,
        )
        .await
        .ok()?;
    let word = ret.get(0..32)?;
    // int24: last byte holds the sign in the ABI word's top extension —
    // the fee tiers use POSITIVE spacings; read the low byte.
    Some(i32::from(word[31]))
}

/// The bundle sim for a V3-target candidate: [target, backrun] via
/// `eth_callMany` on the /fast READ endpoint. Same question the V2 gate
/// asks: does the composed artifact survive + profit after the target lands?
async fn simulate_candidate(
    sim_client: &alloy::rpc::client::RpcClient,
    ev: &degenbot_rpc::backrun_feed::BackrunFeedEvent,
    exec: Address,
    owner: Address,
    cd: &alloy::primitives::Bytes,
) -> bool {
    let target_call = serde_json::json!({
        "from": format!("0x{}", alloy::hex::encode(ev.from)),
        "to": format!("0x{}", alloy::hex::encode(ev.to.unwrap_or_default())),
        "data": format!("0x{}", alloy::hex::encode(&ev.data)),
        "value": format!("0x{:x}", ev.value),
        "gas": format!("0x{:x}", ev.gas.max(120_000)),
        "gasPrice": format!("0x{:x}", ev.max_fee_per_gas.max(1)),
    });
    let backrun_call =
        degenbot_submission::bundle::backrun_sim_call(owner, exec, cd, 900_000, 30_000_000_000);
    let params = degenbot_submission::bundle::eth_call_many_bundle_sim_params(
        &[target_call, backrun_call],
        "latest",
    );
    match sim_client
        .request::<serde_json::Value, serde_json::Value>("eth_callMany", params)
        .await
    {
        Ok(resp) => {
            let txt = serde_json::to_string(&resp).unwrap_or_default();
            !txt.contains("error")
        }
        Err(_) => false,
    }
}

/// The bribe share of TRUE profit paid to the builder (98% default; the
/// env override steers the competitiveness ladder without a rebuild).
fn bribe_bips() -> u16 {
    std::env::var("SIDECAR_BRIBE_BIPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9_800)
        .min(10_000)
}

#[tokio::main]
#[expect(
    clippy::too_many_lines,
    reason = "bin orchestration loop reads top-to-bottom"
)]
async fn main() {
    const MEVBLOCKER_SIM_URL: &str = "https://rpc.mevblocker.io/fast";

    // Offline-review capture (env-gated): SIDECAR_TRACE_JSONL=<path> appends
    // one JSON line per feed frame / bundle wire / sim round-trip so MEVBlocker
    // payloads and our bids can be replayed and reviewed away from the hot loop.
    /// Append one JSON line to the offline-review capture (best-effort:
    /// capture failures never disturb the hot loop).
    fn trace_jsonl(kind: &str, mut v: serde_json::Value) {
        use std::io::Write;
        let Ok(path) = std::env::var("SIDECAR_TRACE_JSONL") else {
            return;
        };
        let mut line = serde_json::json!({
            "ts_unix_ms": u64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0_u128, |d| d.as_millis()),
            )
            .unwrap_or_default(),
            "kind": kind,
        });
        if let (Some(dst), Some(src)) = (line.as_object_mut(), v.as_object_mut()) {
            for (k, val) in std::mem::take(src) {
                dst.insert(k, val);
            }
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(f, "{line}");
        }
    }

    let pool_v3_cache = pool_v3_cache_new();

    init_tracing();
    let cfg = SidecarConfig::from_env();

    let client = alloy::rpc::client::ClientBuilder::default().http(
        cfg.rpc_url
            .parse()
            .expect("SIDECAR_RPC_URL is a valid http url"),
    );
    let provider = Arc::new(AlloyProvider::from_provider(Arc::new(
        alloy::providers::ProviderBuilder::default().connect_client(client),
    )));

    // The bundle-sim client: MEVBlocker's /fast read endpoint (doc
    // how-to/searchers/bid step 2). READ/SIM ONLY -- `eth_callMany` never
    // broadcasts, and this client is passed nothing else.
    let sim_client = alloy::rpc::client::ClientBuilder::default()
        .http(MEVBLOCKER_SIM_URL.parse().expect("sim url"));
    // Bid mode legality was already decided in `decide`; the signer only
    // loads when the key material exists so observe-only runs need none.
    let signer: Option<TxSigner> = cfg.key_file.as_ref().map(|p| {
        let hex = std::fs::read_to_string(p)
            .expect("SIDECAR_KEY_FILE readable")
            .trim()
            .to_string();
        TxSigner::from_key_hex(&hex, 1).expect("SIDECAR_KEY_FILE parses as a secp256k1 key")
    });

    let feed = BackrunFeed::spawn(BackrunFeedConfig {
        url: if cfg.stream_url.is_empty() {
            BackrunFeedConfig::for_mainnet().url
        } else {
            cfg.stream_url.clone()
        },
        ..BackrunFeedConfig::for_mainnet()
    });

    let registry = RouterRegistry::mainnet();
    let exec: Address = std::env::var("SIDECAR_EXECUTOR")
        .unwrap_or_else(|_| String::from("0x30b28ed8aa581fbc0191c3b532b0697773070e97"))
        .parse()
        .expect("SIDECAR_EXECUTOR is a valid address");
    // The sim oracle's caller identity: the executor is OWNER-gated
    // (`execute()` asserts msg.sender == OWNER_ADDR), so the simulated
    // call must come from the OPERATOR address -- never the target tx's
    // original sender (that reverts Unauthorized on every frame, which is
    // exactly the sim_gate_failed storm this replaced). The bid tx
    // itself is signed by the operator key, so sim-from == tx-from.
    let owner: Address = std::env::var("SIDECAR_OPERATOR")
        .or_else(|_| std::env::var("EXECUTOR_OWNER_ADDRESS"))
        .unwrap_or_else(|_| String::from("0x5c603b8a137A40426E0dDFA981EC10c245AF080e"))
        .parse()
        .expect("executor owner address parses");

    // DFYDYI B3: the DB-backed V2 connector index -- ONE startup scan,
    // never a per-frame query. Optional (SIDECAR_DB_PATH): without it the
    // connector lane stays disabled and the bids ride the lean eval alone.
    let (connector_index, connector_db): (
        Option<degenbot_bot::sidecar_paths::V2ConnectorIndex>,
        Option<degenbot_db::connection::DegenbotDb>,
    ) = match std::env::var("SIDECAR_DB_PATH") {
        Ok(path) => match degenbot_db::connection::DegenbotDb::open(std::path::Path::new(&path)) {
            Ok((db, _)) => match degenbot_bot::sidecar_paths::V2ConnectorIndex::load(&db, 1)
                .and_then(|mut ix| ix.load_v3(&db, 1).map(|()| ix))
            {
                Ok(ix) => {
                    tracing::info!(edges = ix.len(), "connector index loaded");
                    (Some(ix), Some(db))
                }
                Err(e) => {
                    tracing::warn!(error = %e, "connector index load failed - lane disabled");
                    (None, None)
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "SIDECAR_DB_PATH unopenable - lane disabled");
                (None, None)
            }
        },
        Err(_) => (None, None),
    };
    let connector_cap: usize = std::env::var("SIDECAR_CONNECTORS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    let mut token_ids: std::collections::HashMap<alloy::primitives::Address, u64> =
        std::collections::HashMap::new();

    // The gate-2 sweep leg (executor sweep = coinbase bid). The overlay solver
    // (S7KG7E) will prepend the backrun target tx; the bid leg is standard.
    // The sweep probe calldata was removed from the bid path: it is an
    // exact-sim/cast-debug artifact (see the sim-gate note above), not a
    // bid-able strategy.

    let dispatcher = Arc::new(Mutex::new(Dispatcher::for_block(
        provider.get_block_number().await.expect("head block fetch"),
    )));
    let operator_nonce = provider
        .get_transaction_count(
            &signer.as_ref().map(TxSigner::address).unwrap_or_default(),
            None,
        )
        .await
        .unwrap_or_default();

    let effective_stream = if cfg.stream_url.is_empty() {
        degenbot_rpc::backrun_feed::DEFAULT_STREAM_URL.to_string()
    } else {
        cfg.stream_url.clone()
    };
    tracing::info!(
        stream = %effective_stream,
        bid_mode = cfg.bid_mode_legal(),
        budget = %cfg.budget_wei,
        stop_file = %cfg.stop_file.display(),
        "sidecar starting"
    );

    let mut spent = U256::ZERO;
    let mut current_block = dispatcher
        .lock()
        .expect("dispatcher mutex poisoned")
        .current_block();
    loop {
        if cfg.stop_file.exists() {
            tracing::info!("kill switch present - halting");
            feed.stop();
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;

        if let Ok(head) = provider.get_block_number().await {
            if head > current_block {
                dispatcher
                    .lock()
                    .expect("dispatcher mutex poisoned")
                    .advance_block(head);
                current_block = head;
            }
        }

        for ev in feed.drain() {
            // Offline-review capture: the feed wire shape, verbatim in all its
            // fields (doc how-to/searchers/listen) keyed by frame hash.
            trace_jsonl(
                "frame",
                serde_json::json!({
                    "hash": format!("0x{}", ev.hash),
                    "chain_id": ev.chain_id,
                    "from": format!("0x{}", alloy::hex::encode(ev.from)),
                    "to": ev.to.map(|a| format!("0x{}", alloy::hex::encode(a))),
                    "value": format!("0x{:x}", ev.value),
                    "data": format!("0x{}", alloy::hex::encode(&ev.data)),
                    "gas": ev.gas,
                    "max_fee_per_gas": ev.max_fee_per_gas,
                    "max_priority_fee_per_gas": ev.max_priority_fee_per_gas,
                    "nonce": ev.nonce,
                    "tx_type": ev.tx_type,
                    "received_unix_ms": ev.received_unix_ms,
                }),
            );
            tracing::debug!(
                tx = %ev.hash, from = %ev.from, to = ?ev.to, value = %ev.value,
                gas = ev.gas, max_fee = ev.max_fee_per_gas, prio = ev.max_priority_fee_per_gas,
                data_len = ev.data.len(), type_ = ev.tx_type,
                "feed frame"
            );
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0));
            let age_ms = now_ms.saturating_sub(ev.received_unix_ms);

            let class = ev
                .to
                .map_or(TargetClass::Inert, |to| classify(to, &ev.data, &registry));

            // Overlay-solve: decoded V2 WETH legs stage through the lean
            // post-target view; the eval output sizes the requested bid.
            // Reserves are fetched async BEFORE the (non-async) decision, so
            // the loop stays sequential and without nested executors.
            let mut requested_bid = U256::from(1);
            // DFYDYI B4: when the connector lane finds a profitable 2-hop
            // family, this becomes the composed execute(calldata) of the
            // full candidate (coinbase bribe via the executor config word);
            // submission calldata only ever holds a sim-verified artifact.
            let mut candidate_calldata: Option<(alloy::primitives::Bytes, U256)> = None;
            // Fresh per-frame engine: staged state (post-target overlays,
            // staged V3 prices) is frame-scoped BY DESIGN.
            let mut solver = degenbot_bot::sidecar_engine::SidecarSolver::new();
            if let TargetClass::Swap(legs) = &class {
                // V3 router targets (EXACT_INPUT[_SINGLE]) do not name the
                // pool in the wire: resolve it CREATE2-style from the decoded
                // (tokenIn, tokenOut, fee tier probe), stage the post-target
                // sqrtP (the B1 follow-up), and run the same mixed fan.
                let Some(index) = connector_index.as_ref() else {
                    continue;
                };
                for l in legs.iter().filter(|l| l.protocol == PoolProtocol::V3) {
                    let (Some(token_in), Some(token_out), Some(amount_in)) =
                        (l.token_in, l.token_out, l.amount_in)
                    else {
                        trace_jsonl(
                            "v3_skip",
                            serde_json::json!({
                                "tx": format!("0x{}", ev.hash),
                                "stage": "leg-fields"
                            }),
                        );
                        continue;
                    };
                    let token0 = token_in.min(token_out);
                    let token1 = token_in.max(token_out);
                    let tok_is_t0 = token_in == token0;
                    let Some((pool, fee, tick_spacing)) =
                        resolve_v3_pool(&provider, &pool_v3_cache, token_in, token_out).await
                    else {
                        trace_jsonl(
                            "v3_skip",
                            serde_json::json!({
                                "tx": format!("0x{}", ev.hash),
                                "stage": "pool-resolve"
                            }),
                        );
                        continue;
                    };
                    let Ok((sqrt_p, _tick, liquidity)) =
                        degenbot_rpc::abi::fetch_v3_slot0_liquidity(&provider, &pool, None).await
                    else {
                        trace_jsonl(
                            "v3_skip",
                            serde_json::json!({
                                "tx": format!("0x{}", ev.hash),
                                "stage": "slot0"
                            }),
                        );
                        continue;
                    };
                    let Some(staged) = degenbot_bot::bot_core::post_target::v3_exact_in_post_target(
                        sqrt_p,
                        u128::try_from(liquidity).unwrap_or(0),
                        fee,
                        amount_in,
                        tok_is_t0,
                    ) else {
                        continue;
                    };
                    // DB ids for both pool tokens (the fan's connector index
                    // lookup is id-keyed).
                    let mut need_vec = vec![token0, token1];
                    need_vec.retain(|a| !token_ids.contains_key(a));
                    if !need_vec.is_empty() {
                        if let Some(Ok(m)) = connector_db
                            .as_ref()
                            .map(|db| db.fetch_token_ids_by_address(1, &need_vec))
                        {
                            token_ids.extend(m);
                        }
                    }
                    let (Some(tok_id), Some(other_id)) = (
                        token_ids.get(&token_in).copied(),
                        token_ids.get(&token_out).copied(),
                    ) else {
                        trace_jsonl(
                            "v3_skip",
                            serde_json::json!({
                                "tx": format!("0x{}", ev.hash),
                                "stage": "db-ids"
                            }),
                        );
                        continue;
                    };
                    let Some(p_id) = solver
                        .admit_v3_full(
                            &provider,
                            pool,
                            token0,
                            token1,
                            fee,
                            tick_spacing,
                            Some(staged.sqrt_p_x96),
                            current_block,
                        )
                        .await
                    else {
                        trace_jsonl(
                            "v3_skip",
                            serde_json::json!({
                                "tx": format!("0x{}", ev.hash),
                                "stage": "admit-staged"
                            }),
                        );
                        continue;
                    };
                    let p_pool_id = u64::from_be_bytes(pool.0[0..8].try_into().expect("8 bytes"));
                    let grade = solver
                        .run_v3_target_lane(
                            &provider,
                            &degenbot_bot::sidecar_engine::V3LaneCtx {
                                index,
                                tok_id,
                                other_id,
                                p_pool_id,
                                p_id,
                                pool,
                                token0,
                                token1,
                                tok_is_t0,
                                fee,
                                head: current_block,
                                cap: connector_cap,
                                gas_floor_wei: U256::from(50_000_000_000_000u64),
                            },
                        )
                        .await;
                    trace_jsonl(
                        "v3_grade",
                        serde_json::json!({
                            "tx": format!("0x{}", ev.hash),
                            "pool": format!("0x{}", alloy::hex::encode(pool)),
                            "connectors": grade.connectors,
                            "declared": grade.paths_declared,
                            "evaluated": grade.paths_evaluated,
                            "admit_failures": grade.admit_failures,
                            "best_profit": format!("0x{:x}", grade.best_profit),
                        }),
                    );
                    if let Some(cand) = grade.best.as_ref() {
                        // Compose + gate the composed artifact (same share as
                        // the on-chain config word)...
                        let composed = degenbot_bot::sidecar_engine::build_candidate_calldata(
                            cand,
                            exec,
                            degenbot_bot::sidecar_solve::weth(),
                            bribe_bips(),
                        );
                        if let Some(cd) = composed {
                            if simulate_candidate(&sim_client, &ev, exec, owner, &cd).await {
                                let bid = U256::from(cand.profit) * U256::from(bribe_bips())
                                    / U256::from(10_000u16);
                                requested_bid = requested_bid.max(bid.max(U256::from(1)));
                                candidate_calldata = Some((cd, bid.max(U256::from(1))));
                                tracing::info!(
                                    pool = ?pool,
                                    connectors = grade.connectors,
                                    profit = %cand.profit,
                                    input = %cand.optimal_input,
                                    "[connectors] v3 target candidate composed + bundle sim PASSED"
                                );
                            }
                        }
                    }
                }
                for l in legs.iter().filter(|l| l.protocol == PoolProtocol::V2) {
                    let Some(pool) = l.pool else { continue };
                    let Some((r0, r1)) =
                        degenbot_bot::sidecar_solve::fetch_v2_reserves(&provider, pool).await
                    else {
                        continue;
                    };
                    if let Ok(o) = degenbot_bot::sidecar_solve::stage_and_eval_v2(
                        l,
                        (r0, r1),
                        degenbot_bot::bot_core::post_target::V2FeeParams {
                            gamma_numer: 997,
                            fee_denom: 1000,
                        },
                        U256::from(50_000_000_000_000u64),
                    ) {
                        requested_bid = requested_bid.max(o.net_wei);
                    } else {
                        // Unprofitable/unstageable: fall back to the sweep-min bid.
                    }

                    // DFYDYI B3/B4 observe lane: the engine-backed connector
                    // solve grades the frame's 2-hop family against the DB
                    // index. Bids stay LEAN-sized until B4 wires multi-hop
                    // executor calldata -- never bid beyond wired calldata.
                    if let (Some(index), Some(db)) =
                        (connector_index.as_ref(), connector_db.as_ref())
                    {
                        let mut need: Vec<alloy::primitives::Address> =
                            vec![degenbot_bot::sidecar_solve::weth()];
                        if let Some(ti) = l.token_in {
                            need.push(ti);
                        }
                        if let Some(to) = l.token_out {
                            need.push(to);
                        }
                        need.retain(|a| !token_ids.contains_key(a));
                        if !need.is_empty() {
                            match db.fetch_token_ids_by_address(1, &need) {
                                Ok(m) => token_ids.extend(m),
                                Err(e) => tracing::debug!(error = %e, "token id fetch failed"),
                            }
                        }
                        let grade = solver
                            .run_connector_lane(
                                &provider,
                                &degenbot_bot::sidecar_engine::ConnectorLaneCtx {
                                    index,
                                    ids: &token_ids,
                                    leg: l,
                                    p_family: degenbot_bot::sidecar_engine::LaneFamily::V2,
                                    staged_p: None,
                                    pair_reserves: (r0, r1),
                                    cap: connector_cap,
                                    head: current_block,
                                    gas_floor_wei: U256::from(50_000_000_000_000u64),
                                },
                            )
                            .await;
                        if let Some(cand) = grade.best.as_ref() {
                            // B4: compose + exact-sim the full candidate. The
                            // bid rides the executor's native coinbase bribe
                            // (bips on the TRUE profit delta), so the wire
                            // bid = bips share of the solver profit.
                            //
                            // Backrun auction stance: the TARGET's searcher
                            // normally dictates the clearing price -- we
                            // compete for inclusion behind their tx, so the
                            // builder's payoff is the lever we control.
                            // Default 98% (9800 bips) of TRUE profit goes to
                            // the builder; the 2% floor retains dominated-
                            // cycle filtering and makes the bid strictly
                            // cheaper-for-us than every 90%-or-less rival on
                            // the same dislocation.
                            let bribe_bips: u16 = bribe_bips();
                            match degenbot_bot::sidecar_engine::build_candidate_calldata(
                                cand,
                                exec,
                                degenbot_bot::sidecar_solve::weth(),
                                bribe_bips,
                            ) {
                                Some(cd) => {
                                    let sim_ok = simulate_sweep(&provider, exec, owner, cd.clone())
                                        .await
                                        .is_some_and(|blocks| {
                                            blocks.first().is_some_and(|b| {
                                                b.calls.first().is_some_and(|c| c.status)
                                            })
                                        });
                                    if sim_ok {
                                        let bid = U256::from(cand.profit) * U256::from(bribe_bips)
                                            / U256::from(10_000u16);
                                        candidate_calldata = Some((cd, bid.max(U256::from(1))));
                                        tracing::info!(
                                            pool = ?l.pool,
                                            connectors = grade.connectors,
                                            evaluated = grade.paths_evaluated,
                                            profit = %cand.profit,
                                            input = %cand.optimal_input,
                                            "[connectors] candidate composed + sim PASSED"
                                        );
                                    } else {
                                        tracing::info!(
                                            pool = ?l.pool,
                                            profit = %cand.profit,
                                            "[connectors] candidate sim FAILED - observe"
                                        );
                                    }
                                }
                                None => {
                                    tracing::debug!(
                                        "[connectors] candidate shape rejected by composer"
                                    );
                                }
                            }
                        } else if grade.best_profit > U256::ZERO {
                            tracing::info!(
                                pool = ?l.pool,
                                connectors = grade.connectors,
                                evaluated = grade.paths_evaluated,
                                best_profit = %grade.best_profit,
                                best_input = %grade.best_input,
                                "[connectors] profitable family, no executable candidate"
                            );
                        } else if grade.connectors > 0 {
                            tracing::debug!(
                                connectors = grade.connectors,
                                evaluated = grade.paths_evaluated,
                                "[connectors] no profitable 2-hop family"
                            );
                        }
                    }
                }
            }

            // Exact-sim gate: the submitted artifact (composed candidate or
            // the bare sweep) must succeed on the current head state or
            // nothing downstream may bid.
            let composed_any = candidate_calldata.is_some();
            let submit_calldata = match candidate_calldata {
                Some((cd, bid)) => {
                    requested_bid = requested_bid.max(bid);
                    Some(cd)
                }
                None => None,
            };
            // Sim gate: ONLY the composed candidate, and ONLY as [target,
            // backrun] via `eth_callMany` on MEVBlocker's /fast read
            // endpoint (doc how-to/searchers/bid, step 2 -- read/sim only,
            // never a broadcast). A head-only sim of the backrun alone
            // answers the wrong question: the profit exists ONLY after the
            // target lands, so pre-target execution structurally rejects
            // perfect backruns while admitting weakest already-live ones.
            // (The bare sweep probe stays retired: check_mode 3 + 100%
            // bribe = a balance transfer to the builder, never an artifact.)
            let sim_ok = match &submit_calldata {
                Some(cd) => {
                    let target_call = serde_json::json!({
                        "from": format!("0x{}", alloy::hex::encode(ev.from)),
                        "to": format!("0x{}", alloy::hex::encode(ev.to.unwrap_or_default())),
                        "data": format!("0x{}", alloy::hex::encode(&ev.data)),
                        "value": format!("0x{:x}", ev.value),
                        "gas": format!("0x{:x}", ev.gas.max(120_000)),
                        "gasPrice": format!("0x{:x}", ev.max_fee_per_gas.max(1)),
                    });
                    let backrun_call =
                        backrun_sim_call(owner, exec, cd, 900_000, u128::from(30_000_000_000u64));
                    let req =
                        eth_call_many_bundle_sim_params(&[target_call, backrun_call], "latest");
                    match sim_client
                        .request::<serde_json::Value, serde_json::Value>("eth_callMany", req)
                        .await
                    {
                        Ok(resp) => {
                            let txt = serde_json::to_string(&resp).unwrap_or_default();
                            let ok = !txt.contains("error");
                            if !ok {
                                tracing::debug!(
                                    tx = %ev.hash,
                                    resp = %serde_json::to_string(&resp).unwrap_or_default(),
                                    "bundle sim: backrun does not survive the target"
                                );
                            }
                            ok
                        }
                        Err(e) => {
                            tracing::debug!(tx = %ev.hash, error = %e, "bundle sim rpc error");
                            false
                        }
                    }
                }
                _ => false,
            };

            let decision = decide(
                &cfg,
                cfg.stop_file.exists(),
                &class,
                sim_ok,
                requested_bid,
                age_ms,
                spent,
            );

            // The observe label tells the truth: a frame that never composed
            // a candidate must not read as "the sim rejected our work".
            let decision = if composed_any {
                decision
            } else {
                match decision {
                    Decision::Observe {
                        reason: "sim_gate_failed",
                    } => Decision::Observe {
                        reason: "no_candidate",
                    },
                    d => d,
                }
            };
            match decision {
                Decision::Bid { bid_wei } => {
                    let Some(s) = signer.as_ref() else {
                        tracing::warn!("bid decided without a signer loaded - skipping");
                        continue;
                    };
                    // Defense in depth: decide() already refuses zero bids;
                    // this refusal keeps the sweep probe out of the auction
                    // even if a future refactor reintroduces the fallback.
                    let Some(cd) = submit_calldata.clone() else {
                        tracing::warn!(
                            tx = %ev.hash,
                            "bid decided without a composed candidate - refusing sweep-only bid"
                        );
                        continue;
                    };
                    let head = provider.get_block_number().await.unwrap_or(current_block);
                    let base_fee_next = provider
                        .get_block(head)
                        .await
                        .ok()
                        .flatten()
                        .and_then(|b| b.header.base_fee_per_gas)
                        .map_or(30_000_000_000u128, |x| u128::from(x) * 12 / 10);
                    let priority_fee: u128 = std::env::var("SIDECAR_PRIORITY_FEE_GWEI")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(2)
                        * 1_000_000_000;

                    let candidate = SubmitCandidate {
                        path_id: u64::from_be_bytes(ev.hash.0[0..8].try_into().expect("8 bytes")),
                        gross_profit: bid_wei,
                        net_profit: bid_wei,
                        gas_used: 300_000,
                        priority_fee,
                        base_fee_next,
                        execute_calldata: cd,
                        executor_address: exec,
                        access_list: None,
                        path_pools: HashSet::new(),
                    };

                    // The bid bundle: this frame's target hash (txs[0]), pinned
                    // to the next block, MEVBlocker searcher WS only.
                    let bundle_target = BundleTarget {
                        stream_url: if cfg.stream_url.is_empty() {
                            String::from(MEVBLOCKER_STREAM_URL)
                        } else {
                            cfg.stream_url.clone()
                        },
                        target_tx_hash: ev.hash,
                        block_number: head + 1,
                    };

                    match dispatch_and_submit(
                        vec![candidate],
                        &dispatcher,
                        &provider,
                        s,
                        Arc::new(SidecarProbe {
                            provider: Arc::clone(&provider),
                        }),
                        operator_nonce,
                        head,
                        std::env::var("SIDECAR_DRY_RUN").is_ok_and(|v| v == "1"),
                        false,
                        &[],
                        Some(&bundle_target),
                    )
                    .await
                    {
                        Ok(outcome) => {
                            if outcome.submitted_count() > 0 {
                                spent += bid_wei;
                            }
                            tracing::info!(
                                tx = %ev.hash,
                                target = %ev.hash,
                                submitted = outcome.submitted_count(),
                                skipped = outcome.skipped_count(),
                                "bid dispatched"
                            );
                        }
                        Err(e) => tracing::warn!(tx = %ev.hash, error = %e, "bid dispatch failed"),
                    }
                }
                Decision::Observe { reason } => {
                    tracing::info!(tx = %ev.hash, reason, "observe");
                }
                Decision::Drop { reason } => {
                    tracing::debug!(tx = %ev.hash, reason, "drop");
                }
            }
        }
    }
}

/// One-block exact sim of the sweep leg on the live head state.
async fn simulate_sweep(
    provider: &AlloyProvider,
    exec: Address,
    from: Address,
    sweep: Bytes,
) -> Option<Vec<alloy::rpc::types::eth::simulate::SimulatedBlock>> {
    provider
        .eth_simulate_v1(
            &alloy::rpc::types::eth::simulate::SimulatePayload {
                block_state_calls: vec![alloy::rpc::types::eth::simulate::SimBlock {
                    calls: vec![alloy::rpc::types::TransactionRequest {
                        from: Some(from),
                        to: Some(exec.into()),
                        input: alloy::rpc::types::TransactionInput::new(sweep),
                        gas: Some(300_000),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            },
            BlockId::latest(),
        )
        .await
        .ok()
}
