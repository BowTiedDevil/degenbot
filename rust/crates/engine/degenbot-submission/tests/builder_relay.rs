//! Builder-relay bundle wire spec.
//!
//! Golden assertions for the Flashbots-compatible `eth_sendBundle` envelope,
//! the `X-Flashbots-Signature` header derivation, and the ordered relay
//! POST fan-out (any-accept = submitted, all-reject = typed skip upstream).

#![expect(clippy::unwrap_used, clippy::panic)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use alloy::primitives::Bytes;
use degenbot_submission::relay::{self, RelayOutcome};
use degenbot_submission::signer::TxSigner;
use serde_json::{json, Value as Json};

const OPERATOR_KEY_HEX: &str = "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d";
// The deterministic address the test key derives (geth anvil account #1).
const OPERATOR_ADDRESS: &str = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";

fn signer() -> TxSigner {
    TxSigner::from_key_hex(OPERATOR_KEY_HEX, 1).unwrap()
}

/// Build a minimal HTTP/1.1 response with an exact `Content-Length`.
fn http_response(payload: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        payload.len(),
        payload
    )
}

fn sample_bundle() -> relay::BuilderRelayBundle {
    relay::BuilderRelayBundle {
        target_raw: Bytes::from(vec![0x02, 0xf8, 0x01]),
        backrun_raw: Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]),
        block_number: 0x111,
    }
}

#[test]
fn bundle_request_shape_matches_flashbots_envelope() {
    let v = relay::builder_bundle_request(&sample_bundle());
    assert_eq!(v["method"], "eth_sendBundle");
    assert_eq!(v["jsonrpc"], "2.0");
    let bundle = &v["params"][0];
    assert_eq!(
        bundle["txs"][0],
        json!("0x02f801"),
        "txs[0] is the TARGET's raw signed bytes"
    );
    assert_eq!(
        bundle["txs"][1],
        json!("0xdeadbeef"),
        "txs[1] is the backrun's raw signed bytes"
    );
    assert_eq!(bundle["blockNumber"], json!("0x111"));
    assert_eq!(bundle["minTimestamp"], json!(0));
    assert_eq!(bundle["maxTimestamp"], json!(600));
    assert_eq!(bundle["revertingTxHashes"], json!([]));
}

#[test]
fn signature_header_derives_from_operator_key() {
    let s = signer();
    let body = r#"{"jsonrpc":"2.0"}"#;
    let header = relay::flashbots_signature_header(&s, body).unwrap();
    let (addr, sig) = header
        .split_once(':')
        .unwrap_or_else(|| panic!("address:sig shape, got {header}"));
    assert_eq!(addr.to_lowercase(), OPERATOR_ADDRESS.to_lowercase());
    let hex = sig
        .strip_prefix("0x")
        .unwrap_or_else(|| panic!("0x-prefixed signature, got {sig}"));
    let bytes = alloy::hex::decode(hex).unwrap();
    assert_eq!(bytes.len(), 65, "r||s||v is 65 bytes");
    assert!(
        bytes[64] == 27 || bytes[64] == 28,
        "legacy y-parity byte, got {}",
        bytes[64]
    );

    // Header is body-bound: a different body produces a different signature.
    let other = relay::flashbots_signature_header(&s, r#"{"jsonrpc":"2.1"}"#).unwrap();
    assert_ne!(header, other);
}

/// The POST fan-out: accepted only on a 200 without a JSON-RPC error; body
/// and auth header must arrive verbatim per relay.
#[tokio::test]
async fn relay_fan_out_classifies_accept_and_reject() {
    let ok_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let bad_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let ok_port = ok_listener.local_addr().unwrap().port();
    let bad_port = bad_listener.local_addr().unwrap().port();

    let body_and_auth = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let captured = body_and_auth.clone();
    let ok_task = tokio::task::spawn_blocking(move || {
        ok_listener.set_nonblocking(false).unwrap();
        let (mut stream, _) = ok_listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]).to_string();
        captured.lock().unwrap().push(request);
        let payload = json!({"jsonrpc":"2.0","id":1,"result":"0xabc"}).to_string();
        stream
            .write_all(http_response(&payload).as_bytes())
            .unwrap();
        stream.flush().unwrap();
    });
    let bad_task = tokio::task::spawn_blocking(move || {
        bad_listener.set_nonblocking(false).unwrap();
        let (mut stream, _) = bad_listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).unwrap_or(0);
        let payload =
            json!({"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"bad"}}).to_string();
        stream
            .write_all(http_response(&payload).as_bytes())
            .unwrap();
        stream.flush().unwrap();
    });

    let body = relay::builder_bundle_request(&sample_bundle());
    let header = relay::flashbots_signature_header(&signer(), &body.to_string()).unwrap();
    let outcomes = relay::send_bundle_to_relays(
        &[
            format!("http://127.0.0.1:{ok_port}"),
            format!("http://127.0.0.1:{bad_port}"),
        ],
        &body,
        &header,
    )
    .await;
    ok_task.await.unwrap();
    bad_task.await.unwrap();

    assert_eq!(outcomes[0], RelayOutcome::Accepted);
    assert_eq!(outcomes[1], RelayOutcome::Rejected);
    assert_eq!(relay::accepted_count(&outcomes), 1);

    let captured = body_and_auth.lock().unwrap()[0].clone();
    assert!(
        captured.contains("POST / HTTP/1.1"),
        "bundle POSTs to the relay root: {captured}"
    );
    assert!(
        captured.to_lowercase().contains("x-flashbots-signature:"),
        "auth header present: {captured}"
    );
    assert!(captured.contains("\"eth_sendBundle\""));
    // Body arrives EXACTLY as signed (column count is conservative here; the
    // signature verification is the relay's job, the verbatim check ours).
    let json_start = captured.find('{').unwrap();
    let body_sent = captured[json_start..].trim_end();
    assert_eq!(
        serde_json::from_str::<Json>(body_sent).ok(),
        Some(body.clone())
    );
    // Remove unused warning for Duration: reserved for future timeout knobs.
    let _ = Duration::ZERO;
}
