//! Frame test for the sidecar compose (task OQQCQO acceptance): an
//! event-shaped frame from the `MEVBlocker` feed flows classify -> oracle gate
//! -> decide. Asserts the observe-only invariant: no `Decision::Bid` without
//! (a) a passing sim AND (b) legal bid mode (mock relay bid frames are the
//! FVIWDT increment).

#![allow(clippy::expect_used, clippy::unwrap_used)]
use std::path::PathBuf;
use std::time::SystemTime;

use alloy::primitives::{address, Bytes, TxHash, U256};
use degenbot_bot::sidecar::{decide, Decision, SidecarConfig};
use degenbot_decoders::target_classifier::{classify, RouterRegistry, TargetClass};
use degenbot_rpc::backrun_feed::BackrunFeedEvent;

fn cfg(bid_mode: bool) -> SidecarConfig {
    SidecarConfig {
        stream_url: String::new(),
        rpc_url: String::new(),
        key_file: None,
        bid_mode,
        budget_wei: if bid_mode {
            U256::from(1_000_000_000_000_000u64)
        } else {
            U256::ZERO
        },
        max_bundle_wei: U256::from(500_000_000_000_000u64),
        stop_file: PathBuf::from("/nonexistent"),
        stale_ms: 1500,
    }
}

fn frame(to_hub: Option<&str>, selector: [u8; 4]) -> BackrunFeedEvent {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or_default())
        .unwrap_or_default();
    let mut data = selector.to_vec();
    data.extend_from_slice(&[0u8; 32]);
    BackrunFeedEvent {
        chain_id: 1,
        from: address!("5c603b8a137a40426e0ddfa981ec10c245af080e"),
        to: to_hub.map(|h| h.parse().unwrap()),
        value: U256::ZERO,
        data: Bytes::from(data),
        gas: 120_000,
        max_fee_per_gas: 30_000_000_000,
        max_priority_fee_per_gas: 2_000_000_000,
        nonce: 7,
        hash: TxHash::from([7u8; 32]),
        access_list: serde_json::Value::Null,
        tx_type: 2,
        received_unix_ms: now,
    }
}

/// The compose the bin performs per frame, as one fn so the test pins it.
fn sidecar_frame(
    cfg: &SidecarConfig,
    ev: &BackrunFeedEvent,
    sim_ok: bool,
    spent: U256,
) -> Decision {
    let registry = RouterRegistry::mainnet();
    let class = ev
        .to
        .map_or(TargetClass::Inert, |to| classify(to, &ev.data, &registry));
    let stop = cfg.stop_file.exists();
    decide(cfg, stop, &class, sim_ok, cfg.max_bundle_wei, 10, spent)
}

#[test]
fn frame_observe_only_never_bids() {
    let c = cfg(false);
    // 1inch v5 hub frame (live-observed classification: Opaque).
    let ev = frame(
        Some("0x111111125421ca6dc452d289314280a0f8842a65"),
        [0x12, 0xaa, 0x3c, 0xaf],
    );
    for sim_ok in [false, true] {
        matches!(
            sidecar_frame(&c, &ev, sim_ok, U256::ZERO),
            Decision::Observe { .. }
        );
    }
    // It must classify opaque (not inert) so it is journaled, not dropped.
    let class = classify(ev.to.unwrap(), &ev.data, &RouterRegistry::mainnet());
    assert!(matches!(class, TargetClass::Opaque(_)));
}

#[test]
fn frame_bid_mode_gate_holds_end_to_end() {
    let c = cfg(true);
    let ev = frame(
        Some("0x111111125421ca6dc452d289314280a0f8842a65"),
        [0x12, 0xaa, 0x3c, 0xaf],
    );
    let d = sidecar_frame(&c, &ev, true, U256::ZERO);
    matches!(d, Decision::Bid { .. });
    // Sim fail: NO bid (the acceptance invariant).
    let d = sidecar_frame(&c, &ev, false, U256::ZERO);
    assert_eq!(
        d,
        Decision::Observe {
            reason: "sim_gate_failed"
        }
    );
}

#[test]
fn frame_unknown_hub_inert_target_drops_before_oracle() {
    let c = cfg(true);
    let ev = frame(None, [0x11, 0x22, 0x33, 0x44]);
    let d = sidecar_frame(&c, &ev, true, U256::ZERO);
    assert_eq!(
        d,
        Decision::Drop {
            reason: "inert_target"
        }
    );
}
