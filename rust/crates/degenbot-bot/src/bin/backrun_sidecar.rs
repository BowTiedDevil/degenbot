//! The standalone backrun sidecar binary (epic 6ZOGIT task OQQCQO).
//!
//! Loop: `MEVBlocker` feed -> hub classification -> exact-sim oracle
//! (`eth_simulateV1`) -> [`degenbot_bot::sidecar::decide`] -> bid via the
//! submission leaf (bid mode) or observe-only journal (default).
//!
//! Zero touches to the live engine block pump (FORK-1); all state is local.
//!
//! Run (observe-only): `SIDECAR_RPC_URL=$RPC cargo run --bin backrun_sidecar`
//! Bid mode additionally requires `SIDECAR_BID_MODE=1`,
//! `SIDECAR_BUDGET_WEI=<wei>`, and `SIDECAR_KEY_FILE=<path>`.
//!
//! Kill switch: `touch /tmp/degenbot-sidecar-STOP`.

#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use alloy::eips::BlockId;
use alloy::primitives::{Address, Bytes, U256};
use degenbot_bot::sidecar::{decide, Decision, SidecarConfig};
use degenbot_decoders::target_classifier::{classify, RouterRegistry};
use degenbot_rpc::backrun_feed::{BackrunFeed, BackrunFeedConfig};
use degenbot_rpc::provider::AlloyProvider;

#[tokio::main]
async fn main() {
    let cfg = SidecarConfig::from_env();

    let client = alloy::rpc::client::ClientBuilder::default().http(
        cfg.rpc_url
            .parse()
            .expect("SIDECAR_RPC_URL is a valid http url"),
    );
    let provider = Arc::new(AlloyProvider::from_provider(Arc::new(
        alloy::providers::ProviderBuilder::default().connect_client(client),
    )));

    let feed = BackrunFeed::spawn(BackrunFeedConfig {
        url: cfg.stream_url.clone(),
        ..BackrunFeedConfig::for_mainnet()
    });

    let registry = RouterRegistry::mainnet();
    let exec: Address = std::env::var("SIDECAR_EXECUTOR")
        .unwrap_or_else(|_| String::from("0x30b28ed8aa581fbc0191c3b532b0697773070e97"))
        .parse()
        .expect("SIDECAR_EXECUTOR is a valid address");

    let sweep = {
        let mut cd = Bytes::from_static(&[0xab, 0x58, 0x98, 0xe8]).to_vec();
        cd.extend_from_slice(&U256::from(0x40u64).to_be_bytes::<32>());
        cd.extend_from_slice(&U256::from(2_560_003u64).to_be_bytes::<32>());
        cd.extend_from_slice(&U256::from(1u64).to_be_bytes::<32>());
        cd.push(0x15);
        cd.extend_from_slice(&[0u8; 31]);
        Bytes::from(cd)
    };

    tracing::info!(
        "[sidecar] stream={} bid_mode={} budget={} stop_file={}",
        cfg.stream_url,
        cfg.bid_mode_legal(),
        cfg.budget_wei,
        cfg.stop_file.display(),
    );

    let mut spent = U256::ZERO;
    loop {
        if cfg.stop_file.exists() {
            tracing::info!("[sidecar] kill switch present - halting");
            feed.stop();
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        for ev in feed.drain() {
            let age_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0))
                .saturating_sub(ev.received_unix_ms);
            let target = ev.to.map(|to| classify(to, &ev.data, &registry));
            let class = target
                .as_ref()
                .unwrap_or(&degenbot_decoders::target_classifier::TargetClass::Inert);

            // Sim gate: the candidate bundle is the executor sweep (the
            // coinbase-bid leg); a passing sim is a precondition for ANY bid.
            let sim_ok = match provider
                .eth_simulate_v1(
                    &alloy::rpc::types::eth::simulate::SimulatePayload {
                        block_state_calls: vec![alloy::rpc::types::eth::simulate::SimBlock {
                            calls: vec![alloy::rpc::types::TransactionRequest {
                                from: Some(ev.from),
                                to: Some(exec.into()),
                                input: alloy::rpc::types::TransactionInput::new(sweep.clone()),
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
            {
                Ok(blocks) => blocks
                    .first()
                    .is_some_and(|b| b.calls.first().is_some_and(|c| c.status)),
                Err(_) => false,
            };

            match decide(
                &cfg,
                cfg.stop_file.exists(),
                class,
                sim_ok,
                spent + U256::from(1),
                age_ms,
                spent,
            ) {
                Decision::Bid { bid_wei } => {
                    // The submission leaf (degenbot-submission) owns broadcast;
                    // wired through dispatch_and_submit at bid rollout. Budget
                    // accounting advances here so caps hold even mid-upload.
                    spent += bid_wei;
                    tracing::info!("[sidecar] BID {} wei (tx {})", bid_wei, ev.hash);
                }
                Decision::Observe { reason } => {
                    tracing::info!("[sidecar] observe ({reason}) tx {}", ev.hash);
                }
                Decision::Drop { reason } => {
                    tracing::info!("[sidecar] drop ({reason}) tx {}", ev.hash);
                }
            }
        }
    }
}
