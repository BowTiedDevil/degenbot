//! The standalone backrun sidecar binary (epic 6ZOGIT, task OQQCQO).
//!
//! Loop: `MEVBlocker` feed -> hub classification -> exact-sim oracle
//! (`eth_simulateV1`) -> [`degenbot_bot::sidecar::decide`] -> bid through the
//! submission leaf ([`dispatch_and_submit`], with an optional `MEVBlocker`
//! private-RPC extra broadcast). Observe-only default; all state is local; zero
//! touches to the live engine block pump (FORK-1).
//!
//! Run (observe-only): `SIDECAR_RPC_URL=$RPC cargo run --bin backrun_sidecar`.
//! Bid mode adds `SIDECAR_BID_MODE=1`, `SIDECAR_BUDGET_WEI=<wei>` and
//! `SIDECAR_KEY_FILE=<hex path>`; `SIDECAR_MEVBLOCKER_URL=<http>` routes the
//! raw tx to `MEVBlocker` private broadcast alongside the public mempool.
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
use degenbot_submission::dispatcher::Dispatcher;
use degenbot_submission::monitor::ReceiptProbe;
use degenbot_submission::signer::TxSigner;
use degenbot_submission::submit::{dispatch_and_submit, SubmitCandidate};

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

    // `MEVBlocker` private broadcast (the /noreverts route studied in the
    // journal) is a SECOND transport, not a replacement.
    let extra_broadcast: Vec<Arc<AlloyProvider>> = match std::env::var("SIDECAR_MEVBLOCKER_URL") {
        Ok(url) => {
            let cl = alloy::rpc::client::ClientBuilder::default()
                .http(url.parse().expect("SIDECAR_MEVBLOCKER_URL valid"));
            vec![Arc::new(AlloyProvider::from_provider(Arc::new(
                alloy::providers::ProviderBuilder::default().connect_client(cl),
            )))]
        }
        Err(_) => Vec::new(),
    };

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

    // The gate-2 sweep leg (executor sweep = coinbase bid). The overlay solver
    // (S7KG7E) will prepend the backrun target tx; the bid leg is standard.
    let sweep = {
        let mut cd = Bytes::from_static(&[0xab, 0x58, 0x98, 0xe8]).to_vec();
        cd.extend_from_slice(&U256::from(0x40u64).to_be_bytes::<32>());
        cd.extend_from_slice(&U256::from(2_560_003u64).to_be_bytes::<32>());
        cd.extend_from_slice(&U256::from(1u64).to_be_bytes::<32>());
        cd.push(0x15);
        cd.extend_from_slice(&[0u8; 31]);
        Bytes::from(cd)
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
            if let TargetClass::Swap(legs) = &class {
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
                }
            }

            // Exact-sim gate: the candidate bundle (sweep leg) must succeed on
            // the current head state or nothing downstream may bid.
            let sim_ok = simulate_sweep(&provider, exec, ev.from, sweep.clone())
                .await
                .is_some_and(|blocks| {
                    blocks
                        .first()
                        .is_some_and(|b| b.calls.first().is_some_and(|c| c.status))
                });

            let decision = decide(
                &cfg,
                cfg.stop_file.exists(),
                &class,
                sim_ok,
                requested_bid,
                age_ms,
                spent,
            );

            match decision {
                Decision::Bid { bid_wei } => {
                    let Some(s) = signer.as_ref() else {
                        tracing::warn!("bid decided without a signer loaded - skipping");
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
                        execute_calldata: sweep.clone(),
                        executor_address: exec,
                        access_list: None,
                        path_pools: HashSet::new(),
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
                        &extra_broadcast,
                    )
                    .await
                    {
                        Ok(outcome) => {
                            if outcome.submitted_count() > 0 {
                                spent += bid_wei;
                            }
                            tracing::info!(
                                tx = %ev.hash,
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
