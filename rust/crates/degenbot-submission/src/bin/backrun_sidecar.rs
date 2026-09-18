//! The standalone backrun sidecar binary (epic 6ZOGIT, task OQQCQO; frame
//! pipeline rewired by WKPZQK).
//!
//! Loop: `MEVBlocker` feed → `frame_pipeline::process_frame` (frame-replay
//! seam → journal-pool extraction → workspace admission → discovery fan →
//! solve → compose → `eth_callMany` gate → [`degenbot_bot::sidecar::decide`])
//! → bid through the submission leaf ([`dispatch_and_submit`], bid mode
//! exclusively via the private-RPC extra broadcast). Observe-only default;
//! all state is local; zero touches to the live engine block pump (FORK-1).
//!
//! Frames are shaped by the replay (touched set + journalled words), never
//! by calldata decoding.
//!
//! Run (observe-only): `SIDECAR_RPC_URL=$RPC cargo run --bin backrun_sidecar`.
//! Bid mode adds `SIDECAR_BID_MODE=1`, `SIDECAR_BUDGET_WEI=<wei>` and
//! `SIDECAR_KEY_FILE=<hex path>`. Bids are MEVBlocker-specific per
//! docs.mevblocker.io/how-to/searchers/bid: `eth_sendBundle` on the
//! searcher WS with `txs = [targetHash, signed backrun]`, block-pinned.
//! The signed backrun NEVER touches the public mempool or another relay.
//! Dry-run (offline-review over a captured frame JSONL):
//! `SIDECAR_DRY_RUN_JSONL=/tmp/mb_trace.jsonl` replaces the live feed with
//! the capture's frames, processed once, in order.
//! Kill switch: `touch /tmp/degenbot-sidecar-STOP`.

#![expect(
    clippy::expect_used,
    reason = "bin: fatal config failures exit the process loudly"
)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::primitives::{Address, U256};
use degenbot_bot::bot_core::SimAnchorState;
use degenbot_bot::sidecar::{Decision, SidecarConfig};
use degenbot_rpc::backrun_feed::{BackrunFeed, BackrunFeedConfig};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::BlockSimHandle;
use degenbot_submission::bundle::MEVBLOCKER_STREAM_URL;
use degenbot_submission::dispatcher::Dispatcher;
use degenbot_submission::frame_pipeline::{
    build_block_handle, load_fixture_frames, process_frame, trace_jsonl, PipelineConfig,
    StrategyRuntime,
};
use degenbot_submission::gap_quarantine::{ParkedFrame, Quarantine, QuarantineDecision};
use degenbot_submission::monitor::ReceiptProbe;
use degenbot_submission::signer::TxSigner;
use degenbot_submission::submit::{dispatch_and_submit, BundleTarget, SubmitCandidate};

/// The gas floor the envelope gate evaluates at (wei) — the composed lane's
/// standing economics (the env override did not exist upstream either).
const GAS_FLOOR_WEI: u64 = 50_000_000_000_000;

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

/// The bribe share of TRUE profit paid to the builder (98% default; the
/// env override steers the competitiveness ladder without a rebuild).
fn bribe_bips() -> u16 {
    std::env::var("SIDECAR_BRIBE_BIPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9_800)
        .min(10_000)
}

#[expect(
    clippy::too_many_arguments,
    reason = "the frame handler takes the runtime surfaces it needs"
)]
#[expect(
    clippy::too_many_lines,
    reason = "capture -> pipeline -> dispatch reads top-to-bottom"
)]
async fn run_frame(
    ev: &degenbot_rpc::backrun_feed::BackrunFeedEvent,
    rt: &mut StrategyRuntime,
    provider: &Arc<AlloyProvider>,
    sim_client: &alloy::rpc::client::RpcClient,
    cfg: &SidecarConfig,
    pl: &PipelineConfig,
    handle: &mut Option<BlockSimHandle<'static>>,
    head: u64,
    age_ms: u64,
    spent: &mut U256,
    dispatcher: &Arc<Mutex<Dispatcher>>,
    operator_nonce: u64,
    signer: Option<&TxSigner>,
    gap_probe: &degenbot_submission::gap_probe::GapProbe,
    quarantine: &mut Quarantine,
) {
    // Decode-stage reject: a frame whose gas field reads zero can never
    // pass the EVM's pre-checks (`CallGasCostMoreThanGasLimit` fires
    // structurally) — dropping here keeps the pipeline histogram
    // truthful about WHICH class the frame died in.
    if ev.gas == 0 {
        tracing::info!(
            "observe tx=0x{:x} reason=\"envelope_artifact\" (zero-gas envelope)",
            ev.hash
        );
        trace_jsonl(
            "envelope_artifact",
            serde_json::json!({"tx": format!("0x{:x}", ev.hash), "zero_gas": true}),
        );
        return;
    }

    // Offline-review capture: the feed wire shape, verbatim in all its
    // fields (doc how-to/searchers/listen) keyed by frame hash.
    trace_jsonl(
        "frame",
        serde_json::json!({
            "hash": ev.hash.to_string(),
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
    let artifacts = process_frame(
        rt, provider, sim_client, cfg, pl, handle, ev, head, age_ms, *spent,
    )
    .await;
    trace_jsonl(
        "stages",
        serde_json::json!({
            "tx": ev.hash.to_string(),
            "stages": artifacts.stages.to_json(),
            "head": head,
        }),
    );
    // The decision + its inputs, offline-review readable: the tracing line
    // alone leaves the funnel's last stage out of the JSONL.
    let (decision_kind, decision_reason, decision_bid) = match &artifacts.decision {
        Decision::Bid { bid_wei } => ("bid", None, Some(bid_wei.to_string())),
        Decision::Observe { reason } => ("observe", Some((*reason).to_string()), None),
        Decision::Drop { reason } => ("drop", Some((*reason).to_string()), None),
    };
    trace_jsonl(
        "decide",
        serde_json::json!({
            "tx": ev.hash.to_string(),
            "decision": decision_kind,
            "reason": decision_reason,
            "bid_wei": decision_bid,
            "composed_any": artifacts.submit_calldata.is_some(),
            "requested_bid": artifacts.requested_bid.to_string(),
            "age_ms": age_ms,
            "spent": spent.to_string(),
            "stop_file": cfg.stop_file.exists(),
        }),
    );
    if let Some(degenbot_simulation::sim::evm::frame_replay::ReplayFrameError::GapPending {
        claimed,
        expected,
    }) = &artifacts.replay_frame_error
    {
        let parked = ParkedFrame {
            hash: ev.hash,
            from: ev.from,
            to: ev.to,
            value: ev.value,
            data: ev.data.clone(),
            gas: ev.gas,
            max_fee_per_gas: ev.max_fee_per_gas,
            max_priority_fee_per_gas: ev.max_priority_fee_per_gas,
            claimed_nonce: ev.nonce,
            arrived_at: std::time::Instant::now(),
            expected_at_capture: *expected,
        };
        let parked_count = quarantine.push(parked);
        trace_jsonl(
            "quarantine",
            serde_json::json!({
                "action": "park",
                "tx": ev.hash.to_string(),
                "claimed_nonce": claimed,
                "expected_nonce": expected,
                "parked": parked_count,
                "gap": claimed - expected,
            }),
        );
    }

    if let Decision::Observe {
        reason: "gap_pending",
    } = &artifacts.decision
    {
        let probe_out = gap_probe.probe_gap(ev.from, ev.nonce).await;
        trace_jsonl(
            "gap_probe",
            serde_json::json!({
                "tx": ev.hash.to_string(),
                "sender": format!("0x{:x}", ev.from),
                "claimed_nonce": ev.nonce,
                "probe": probe_out,
            }),
        );
    }
    match artifacts.decision {
        Decision::Bid { bid_wei } => {
            let Some(s) = signer else {
                tracing::warn!("bid decided without a signer loaded - skipping");
                return;
            };
            // Defense in depth: decide() already refuses zero bids; this
            // refusal keeps a bare-sweep bid out of the auction even if a
            // future refactor reintroduces a fallback.
            let Some(cd) = artifacts.submit_calldata.clone() else {
                tracing::warn!(
                    tx = %ev.hash,
                    "bid decided without a composed candidate - refusing"
                );
                return;
            };
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
                executor_address: pl.exec,
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
                dispatcher,
                provider,
                s,
                Arc::new(SidecarProbe {
                    provider: Arc::clone(provider),
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
                        *spent += bid_wei;
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

#[tokio::main]
#[expect(
    clippy::too_many_lines,
    reason = "bin orchestration loop reads top-to-bottom"
)]
async fn main() {
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

    // The bundle-sim client (`SIDECAR_SIM_URL`, default: the chain node
    // the frames replay against). READ/SIM ONLY -- `eth_callMany` never
    // broadcasts, and this client is passed nothing else. The sim MUST run
    // on an endpoint that actually serves `eth_callMany`; MEVBlocker's
    // /fast http tier answers method-missing for it, so the node is the
    // fallback and /fast stays available as an explicit override.
    let sim_client = alloy::rpc::client::ClientBuilder::default().http(
        std::env::var("SIDECAR_SIM_URL")
            .unwrap_or_else(|_| cfg.rpc_url.clone())
            .parse()
            .expect("sim url parses as an http url"),
    );
    // The gap-boundary probe samples the chain node's pending-pool lanes
    // whenever a frame claims a nonce ahead of the parent state.
    let gap_probe = degenbot_submission::gap_probe::GapProbe::new(sim_client.clone());

    // Bid mode legality was already decided in `decide`; the signer only
    // loads when the key material exists so observe-only runs need none.
    let signer: Option<TxSigner> = cfg.key_file.as_ref().map(|p| {
        let hex = std::fs::read_to_string(p)
            .expect("SIDECAR_KEY_FILE readable")
            .trim()
            .to_string();
        TxSigner::from_key_hex(&hex, 1).expect("SIDECAR_KEY_FILE parses as a secp256k1 key")
    });

    // Dry-run (offline-review): a captured frame JSONL replaces the live
    // feed — the capture's frames are processed once, in order.
    let fixture_frames = std::env::var("SIDECAR_DRY_RUN_JSONL").ok().map(|p| {
        let frames = load_fixture_frames(std::path::Path::new(&p));
        tracing::info!(frames = frames.len(), path = %p, "dry-run fixture loaded");
        frames
    });

    // DFYDYI B3: the DB-backed connector index -- ONE startup scan, never a
    // per-frame query. Optional (SIDECAR_DB_PATH): without it the discovery
    // fan stays shut and frames observe (connectors are never guessed).
    let (connector_index, connector_db): (
        Option<degenbot_bot::sidecar_paths::V2ConnectorIndex>,
        Option<degenbot_db::connection::DegenbotDb>,
    ) = match std::env::var("SIDECAR_DB_PATH") {
        Ok(path) => match degenbot_db::connection::DegenbotDb::open(std::path::Path::new(&path)) {
            Ok((db, _)) => match degenbot_bot::sidecar_paths::V2ConnectorIndex::load(&db, 1)
                .and_then(|mut ix| ix.load_v3(&db, 1).map(|()| ix))
            {
                Ok(mut ix) => {
                    ix.set_ranker(Arc::new(
                        degenbot_bot::sidecar_paths::OnChainLiquidityRanker::new(Arc::clone(
                            &provider,
                        )),
                    ));
                    tracing::info!(edges = ix.len(), "connector index loaded");
                    // Evidence mode (SIDECAR_RANK_EVIDENCE=1): a LIVE sanity
                    // probe before any frame trusts the depth truncation --
                    // the canonical deep USDC/WETH pair must top the ranking.
                    if std::env::var("SIDECAR_RANK_EVIDENCE").as_deref() == Ok("1") {
                        match degenbot_bot::sidecar_paths::deep_pair_ranking_evidence(&ix, &db)
                            .await
                        {
                            Ok(()) => {
                                tracing::info!(
                                    "rank evidence: deep USDC/WETH pair tops the ranking"
                                );
                            }
                            Err(e) => tracing::warn!(evidence = %e, "rank evidence FAILED"),
                        }
                    }
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

    // The strategy runtime OWNS the frame-surviving caches (index, token
    // joins, warm-code cache); each frame gets a fresh planning Workspace
    // scope (see frame_pipeline's module doc for the split).
    let mut runtime = StrategyRuntime::new(1, connector_index, connector_db, connector_cap);

    let exec: Address = std::env::var("SIDECAR_EXECUTOR")
        .unwrap_or_else(|_| String::from("0x30b28ed8aa581fbc0191c3b532b0697773070e97"))
        .parse()
        .expect("SIDECAR_EXECUTOR is a valid address");
    // The sim oracle's caller identity: the executor is OWNER-gated
    // (`execute()` asserts msg.sender == OWNER_ADDR), so the simulated
    // call must come from the OPERATOR address -- never the target tx's
    // original sender. The bid tx itself is signed by the operator key,
    // so sim-from == tx-from.
    let owner: Address = std::env::var("SIDECAR_OPERATOR")
        .or_else(|_| std::env::var("EXECUTOR_OWNER_ADDRESS"))
        .unwrap_or_else(|_| String::from("0x5c603b8a137A40426E0dDFA981EC10c245AF080e"))
        .parse()
        .expect("executor owner address parses");
    let pl = PipelineConfig {
        exec,
        owner,
        bribe_bips: bribe_bips(),
        gas_floor_wei: U256::from(GAS_FLOOR_WEI),
    };

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

    tracing::info!(
        bid_mode = cfg.bid_mode_legal(),
        budget = %cfg.budget_wei,
        stop_file = %cfg.stop_file.display(),
        "sidecar starting"
    );

    // The per-block replay handle: rebuilt whenever the observed head
    // advances (the scratch stack pins `BlockId::Number(head)` and frames
    // run in the NEXT block's env). The anchor is an EMPTY leaked snapshot
    // — the sidecar tracks no canonical-registry pools; the shared warm
    // cache carries the cross-block bytecode/account caches across rebuilds.
    let anchor: &'static SimAnchorState = Box::leak(Box::new(SimAnchorState::default()));
    let mut current_block = dispatcher
        .lock()
        .expect("dispatcher mutex poisoned")
        .current_block();
    let mut handle: Option<BlockSimHandle<'static>> =
        build_block_handle(&provider, current_block, &runtime.warm_cache, anchor).await;
    if handle.is_none() {
        tracing::warn!(
            "replay handle build failed - frames observe replay_unavailable until it recovers"
        );
    }

    let mut spent = U256::ZERO;

    let mut quarantine = Quarantine::new();

    if let Some(frames) = fixture_frames {
        // Dry-run over the capture: every frame processed once, in order.
        for ev in &frames {
            if cfg.stop_file.exists() {
                tracing::info!("kill switch present - halting dry-run");
                break;
            }
            let age_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| {
                    d.as_millis()
                        .saturating_sub(u128::from(ev.received_unix_ms))
                });
            let age_ms = u64::try_from(age_ms).unwrap_or(u64::MAX);
            run_frame(
                ev,
                &mut runtime,
                &provider,
                &sim_client,
                &cfg,
                &pl,
                &mut handle,
                current_block,
                age_ms,
                &mut spent,
                &dispatcher,
                operator_nonce,
                signer.as_ref(),
                &gap_probe,
                &mut quarantine,
            )
            .await;
        }
        tracing::info!("dry-run fixture complete - halting");
        return;
    }

    // Live mode: MEVBlocker feed.
    let feed = BackrunFeed::spawn(BackrunFeedConfig {
        url: if cfg.stream_url.is_empty() {
            BackrunFeedConfig::for_mainnet().url
        } else {
            cfg.stream_url.clone()
        },
        ..BackrunFeedConfig::for_mainnet()
    });
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
                match build_block_handle(&provider, head, &runtime.warm_cache, anchor).await {
                    Some(h) => handle = Some(h),
                    None => {
                        tracing::warn!(
                            "replay handle rebuild failed - frames observe replay_unavailable"
                        );
                    }
                }

                // Gap-quarantine FSM tick: on a fresh head every parked sender
                // re-polls against the new state; frames the state closure
                // covered rerun through the full frame path.
                for sender in quarantine.senders() {
                    let head_nonce: u64 = sim_client
                        .request::<(Address, &str), U256>(
                            std::borrow::Cow::from("eth_getTransactionCount"),
                            (sender, "latest"),
                        )
                        .await
                        .ok()
                        .map_or(u64::MAX, |v| u64::try_from(v).unwrap_or(u64::MAX));
                    for (frame, decision) in
                        quarantine.poll(sender, head_nonce, &[], std::time::Instant::now())
                    {
                        match decision {
                            QuarantineDecision::ClosedByHead => {
                                trace_jsonl(
                                    "quarantine",
                                    serde_json::json!({"action": "closed_by_head", "tx": frame.hash.to_string()}),
                                );
                                let ev = degenbot_rpc::backrun_feed::BackrunFeedEvent {
                                    hash: frame.hash,
                                    chain_id: 1,
                                    from: frame.from,
                                    to: frame.to,
                                    value: frame.value,
                                    data: frame.data,
                                    gas: frame.gas,
                                    max_fee_per_gas: frame.max_fee_per_gas,
                                    max_priority_fee_per_gas: frame.max_priority_fee_per_gas,
                                    nonce: frame.claimed_nonce,
                                    access_list: serde_json::Value::Null,
                                    tx_type: 2,
                                    received_unix_ms: std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0)),
                                };
                                run_frame(
                                    &ev,
                                    &mut runtime,
                                    &provider,
                                    &sim_client,
                                    &cfg,
                                    &pl,
                                    &mut handle,
                                    head,
                                    0,
                                    &mut spent,
                                    &dispatcher,
                                    operator_nonce,
                                    signer.as_ref(),
                                    &gap_probe,
                                    &mut quarantine,
                                )
                                .await;
                            }
                            QuarantineDecision::GapExpired => {
                                trace_jsonl(
                                    "quarantine",
                                    serde_json::json!({"action": "gap_expired", "tx": frame.hash.to_string()}),
                                );
                                tracing::info!(
                                    "observe tx=0x{:x} reason=\"gap_expired\"",
                                    frame.hash
                                );
                            }
                            QuarantineDecision::Rescue { predecessors } => {
                                trace_jsonl(
                                    "quarantine",
                                    serde_json::json!({"action": "rescue_event_unsupported",
                                        "tx": frame.hash.to_string(),
                                        "predecessors": predecessors,
                                    }),
                                );
                            }
                            QuarantineDecision::StillWaiting { unknown } => {
                                trace_jsonl(
                                    "quarantine",
                                    serde_json::json!({"action": "still_waiting",
                                        "tx": frame.hash.to_string(),
                                        "unknown_nonces": unknown,
                                    }),
                                );
                            }
                        }
                    }
                }
            }
        }

        for ev in feed.drain() {
            if cfg.stop_file.exists() {
                tracing::info!("kill switch present - halting");
                break;
            }
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis());
            let age_ms = u64::try_from(now_ms.saturating_sub(u128::from(ev.received_unix_ms)))
                .unwrap_or(u64::MAX);
            run_frame(
                &ev,
                &mut runtime,
                &provider,
                &sim_client,
                &cfg,
                &pl,
                &mut handle,
                current_block,
                age_ms,
                &mut spent,
                &dispatcher,
                operator_nonce,
                signer.as_ref(),
                &gap_probe,
                &mut quarantine,
            )
            .await;
        }
    }
}
