//! `MEVBlocker` backrun-bundle submission (task GSUF22).
//!
//! Wire contract per docs.mevblocker.io (searcher onboarding):
//!
//! - bid: `eth_sendBundle` over `wss://searchers.mevblocker.io` with
//!   `params[0] = { `txs`: ["<targetTxHash>", "<rawSignedBackrunTx>"],
//!   `blockNumber`: "<hex>", `replacementUuid`: "<uuid>" }` — the FIRST element
//!   is the pending TARGET's hash (not an encoded tx) and the SECOND is the
//!   signed raw backrun bytes.
//! - cancel: `eth_cancelBundle` with `{ `replacementUuid` }` (also accepted as
//!   an empty-`txs` `eth_sendBundle`).
//!
//! The backrun pays its bid to the bundle fee recipient (`block.coinbase`)
//! via the executor's packed-config bribe fields (`bribe_bips`,
//! `bribe_recipient_idx = 0`) — see `encode_config_word`, single-sourced with
//! `cmd_executor.vy`.
//!
//! Transport is a minimal async WS relay (one connect per send batch); the
//! driver task owns session policy. JSON wire shapes are golden-tested.

use std::time::Duration;

use alloy::primitives::{B256, U256};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value as Json};
use tokio_tungstenite::{connect_async, tungstenite::Message};

pub const MEVBLOCKER_STREAM_URL: &str = "wss://searchers.mevblocker.io";
pub const MEVBLOCKER_HTTP_URL: &str = "https://rpc.mevblocker.io";

/// A backrun bundle bid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleBid {
    /// The pending TARGET transaction's hash (first `txs` element).
    pub target_tx_hash: B256,
    /// SIGNED raw backrun transaction bytes (second `txs` element).
    pub backrun_raw: alloy::primitives::Bytes,
    /// Hex-encoded block number the bundle is valid for.
    pub block_number: u64,
    /// Replacement/cancel UUID (deterministic per (target, block) upstream).
    pub replacement_uuid: String,
}

/// Build the `eth_sendBundle` JSON-RPC request (golden-tested wire shape).
#[must_use]
pub fn eth_send_bundle_request(bid: &BundleBid) -> Json {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_sendBundle",
        "params": [ {
            "txs": [
                format!("0x{}", alloy::hex::encode(bid.target_tx_hash)),
                format!("0x{}", alloy::hex::encode(&bid.backrun_raw)),
            ],
            "blockNumber": format!("0x{:x}", bid.block_number),
            "replacementUuid": bid.replacement_uuid,
        } ]
    })
}

/// Build the `eth_cancelBundle` request (``replacementUuid`` only).
#[must_use]
pub fn eth_cancel_bundle_request(replacement_uuid: &str) -> Json {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_cancelBundle",
        "params": [ { "replacementUuid": replacement_uuid } ]
    })
}

/// Build the empty-`txs` `eth_sendBundle` cancel (Option 1 on the docs page).
#[must_use]
pub fn eth_send_bundle_cancel_request(replacement_uuid: &str) -> Json {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_sendBundle",
        "params": [ { "txs": [], "replacementUuid": replacement_uuid } ]
    })
}

/// Deterministic ``replacementUuid`` per (target hash, block): idempotent
/// replace/cancel across retries without a UUID crate (32 hex chars).
#[must_use]
pub fn replacement_uuid_for(target: B256, block: u64) -> String {
    let seed = alloy::primitives::keccak256(*target);
    format!("degenbot-{block:08x}-{}", &alloy::hex::encode(seed)[..16])
}

/// Packed `execute()` config word, single-sourced with `cmd_executor.vy`:
/// `(expected_value << 32) | (bribe_recipient_idx << 24) | (bribe_bips << 8)
/// | check_mode`.
///
/// - bits 0-7: check mode (non-zero keeps the seatbelt: the on-chain profit
///   check bounds a landed tx's worst case to gas — `S6` policy),
/// - bits 8-23: bribe bips over the pool-index-`bribe_recipient_idx`
///   recipient (0 = `block.coinbase` = the bundle fee recipient),
/// - bits 32-255: expected pre-tx balance for the configured check mode.
#[must_use]
pub fn encode_config_word(
    check_mode: u8,
    bribe_bips: u16,
    bribe_recipient_idx: u8,
    expected_value: U256,
) -> U256 {
    (expected_value << 32)
        | (U256::from(bribe_recipient_idx) << 24)
        | (U256::from(bribe_bips) << 8)
        | U256::from(check_mode)
}

/// Decode helper for round-trip tests (and the telemetry logger).
#[must_use]
pub fn decode_config_word(config: U256) -> (u8, u16, u8, U256) {
    let mode_word = config & U256::from(0xff_u64);
    let check_mode = u8::try_from(u64::try_from(mode_word).unwrap_or_default()).unwrap_or_default();
    let bips_word = (config >> 8) & U256::from(0xff_ff_u64);
    let bribe_bips =
        u16::try_from(u64::try_from(bips_word).unwrap_or_default()).unwrap_or_default();
    let idx_word = (config >> 24) & U256::from(0xff_u64);
    let bribe_recipient_idx =
        u8::try_from(u64::try_from(idx_word).unwrap_or_default()).unwrap_or_default();
    let expected_value = config >> 32;
    (check_mode, bribe_bips, bribe_recipient_idx, expected_value)
}

/// Errors from the relay round-trip.
#[derive(Debug, thiserror::Error)]
pub enum BundleRelayError {
    #[error("ws connect failed: {0}")]
    Connect(String),
    #[error("ws send failed: {0}")]
    Send(String),
    #[error("ws closed before response")]
    Closed,
    #[error("timeout waiting for relay response")]
    Timeout,
    #[error("relay returned an error object: {0}")]
    RelayError(String),
}

/// One-shot WS submit: connect, send, await the response frame, close.
///
/// # Errors
///
/// Typed `BundleRelayError` on connect/send failures, response timeout, a
/// closed stream before the response, or a JSON-RPC error object.
/// The driver owns retry/session policy (bids are single-shot per block; a
/// dropped bid is a no-cost miss under the revert shield).
pub async fn send_request(
    url: &str,
    request: Json,
    timeout: Duration,
) -> Result<Json, BundleRelayError> {
    let request_url = url.to_string();
    let connect_fut = connect_async(request_url.as_str());
    let (mut ws, _) = tokio::time::timeout(timeout, connect_fut)
        .await
        .map_err(|_| BundleRelayError::Timeout)?
        .map_err(|e| BundleRelayError::Connect(e.to_string()))?;
    ws.send(Message::Text(request.to_string().into()))
        .await
        .map_err(|e| BundleRelayError::Send(e.to_string()))?;
    loop {
        let frame = tokio::time::timeout(timeout, ws.next())
            .await
            .map_err(|_| BundleRelayError::Timeout)?;
        match frame {
            None => return Err(BundleRelayError::Closed),
            Some(Err(e)) => return Err(BundleRelayError::Send(e.to_string())),
            Some(Ok(Message::Text(t))) => {
                let v: Json =
                    serde_json::from_str(t.as_str()).map_err(|_| BundleRelayError::Closed)?;
                if v.get("error").is_some() {
                    return Err(BundleRelayError::RelayError(v["error"].to_string()));
                }
                let _ = ws.send(Message::Close(None)).await;
                return Ok(v);
            }
            Some(Ok(Message::Ping(p))) => {
                let _ = ws.send(Message::Pong(p)).await;
            }
            Some(Ok(_)) => {}
        }
    }
}
