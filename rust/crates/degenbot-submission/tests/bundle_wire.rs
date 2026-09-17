//! Bundle wire-format + relay spec (task GSUF22).
//!
//! Seams: `degenbot_submission::bundle::{eth_send_bundle_request,
//! eth_cancel_bundle_request, eth_send_bundle_cancel_request,
//! replacement_uuid_for, encode_config_word, decode_config_word,
//! send_request}`. The relay round-trip runs against a local mock WS server.

#![expect(clippy::unwrap_used)]

use std::time::Duration;

use alloy::primitives::{B256, U256};
use degenbot_submission::bundle::{
    decode_config_word, encode_config_word, eth_cancel_bundle_request,
    eth_send_bundle_cancel_request, eth_send_bundle_request, replacement_uuid_for, send_request,
    BundleBid,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};

fn sample_bid() -> BundleBid {
    BundleBid {
        target_tx_hash: B256::new([0x5f; 32]),
        backrun_raw: alloy::primitives::Bytes::from(vec![0x02, 0xf8, 0x01, 0x02, 0x03]),
        block_number: 0x0102_286b,
        replacement_uuid: "degenbot-0102286b-beefbeefdeadcafe".into(),
    }
}

#[test]
fn t01_send_bundle_wire_shape_matches_docs() {
    let req = eth_send_bundle_request(&sample_bid());
    assert_eq!(req["jsonrpc"], "2.0");
    assert_eq!(req["method"], "eth_sendBundle");
    let p = &req["params"][0];
    let txs = p["txs"].as_array().unwrap();
    assert_eq!(txs.len(), 2, "target hash first, raw backrun second");
    assert_eq!(txs[0], json!(format!("0x{}", "5f".repeat(32))));
    assert_eq!(
        txs[1],
        json!("0x02f8010203"),
        "raw signed bytes, noHash prefix trimmed"
    );
    assert_eq!(
        txs[0].as_str().unwrap().len(),
        66,
        "target is a HASH (32-byte hex)"
    );
    assert_eq!(p["blockNumber"], json!("0x102286b"));
    assert_eq!(
        p["replacementUuid"],
        json!("degenbot-0102286b-beefbeefdeadcafe")
    );
}

#[test]
fn t02_cancel_wire_shapes() {
    let c = eth_cancel_bundle_request("blinklabsxyz");
    assert_eq!(c["method"], "eth_cancelBundle");
    assert_eq!(c["params"][0]["replacementUuid"], json!("blinklabsxyz"));
    let c2 = eth_send_bundle_cancel_request("blinklabsxyz");
    assert_eq!(c2["method"], "eth_sendBundle");
    assert!(c2["params"][0]["txs"].as_array().unwrap().is_empty());
}

#[test]
fn t03_replacement_uuid_is_deterministic_and_scoped() {
    let target = B256::new([1; 32]);
    let same_block = replacement_uuid_for(target, 100);
    assert_eq!(
        same_block,
        replacement_uuid_for(target, 100),
        "deterministic per (target, block)"
    );
    let next_block = replacement_uuid_for(target, 101);
    assert_ne!(same_block, next_block, "block scoped");
    let other_target = replacement_uuid_for(B256::new([2; 32]), 100);
    assert_ne!(same_block, other_target, "target scoped");
}

#[test]
fn t04_config_word_packs_bribe_bits_and_round_trips() {
    // (500 << 8) | 2 == 5% coinbase bribe with check mode 2 — the SE idea :=
    // docs example from cmd_executor.vy.
    let cfg = encode_config_word(2, 500, 0, U256::from(123u64));
    let (mode, bips, idx, expected) = decode_config_word(cfg);
    assert_eq!(mode, 2);
    assert_eq!(bips, 500);
    assert_eq!(idx, 0, "0 = block.coinbase");
    assert_eq!(expected, U256::from(123u64));
    // Round-trip determinism.
    assert_eq!(encode_config_word(mode, bips, idx, expected), cfg);
}

#[test]
fn t05_config_word_bribe_recipient_table_index() {
    // (500 << 8) | (3 << 24) | 1: 5% bribe to address-table entry 3.
    let cfg = encode_config_word(1, 500, 3, U256::ZERO);
    let (mode, bips, idx, _) = decode_config_word(cfg);
    assert_eq!(mode, 1);
    assert_eq!(bips, 500);
    assert_eq!(idx, 3);
}

#[test]
fn t06_expected_value_occupies_upper_bits() {
    let ev = U256::from(0xdead_beefu64) * U256::from(1_000_000u64);
    let cfg = encode_config_word(1, 0, 0, ev);
    let (_, _, _, got) = decode_config_word(cfg);
    assert_eq!(got, ev);
}

// ---- relay round-trip ----

async fn spawn_relay() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        let req = ws.next().await.unwrap().unwrap();
        let v: serde_json::Value = serde_json::from_str(req.to_text().unwrap()).unwrap();
        assert_eq!(v["method"], "eth_sendBundle");
        let resp = json!({"id": 1, "jsonrpc": "2.0", "result": "0x164d7d41f24b333a"});
        ws.send(Message::Text(resp.to_string().into()))
            .await
            .unwrap();
    });
    port
}

#[tokio::test]
async fn t07_relay_round_trip_delivers_result() {
    let port = spawn_relay().await;
    let req = eth_send_bundle_request(&sample_bid());
    let resp = send_request(
        &format!("ws://127.0.0.1:{port}"),
        req,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(resp["result"], json!("0x164d7d41f24b333a"));
}

#[tokio::test]
async fn t08_relay_error_object_is_typed() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        let _ = ws.next().await;
        let resp = json!({"id": 1, "jsonrpc": "2.0", "error": {"code": -32000, "message": "invalid bundle"}});
        ws.send(Message::Text(resp.to_string().into()))
            .await
            .unwrap();
    });
    let req = eth_send_bundle_request(&sample_bid());
    let err = send_request(
        &format!("ws://127.0.0.1:{port}"),
        req,
        Duration::from_secs(5),
    )
    .await;
    assert!(
        matches!(
            err,
            Err(degenbot_submission::bundle::BundleRelayError::RelayError(_))
        ),
        "{err:?}"
    );
}
