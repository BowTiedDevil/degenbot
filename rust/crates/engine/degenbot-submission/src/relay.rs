//! Flashbots-compatible builder-relay bundle submission.
//!
//! The txpool backrun's submission slot: the signed backrun AND the target's
//! verbatim signed bytes leave as one `eth_sendBundle` to every builder relay
//! over HTTPS — no auction host in the path. Envelope shape is the
//! Flashbots-compatible `eth_sendBundle` (the relays' documented standard):
//!
//! - `txs`: `[<target raw signed>, <backrun raw signed>]` (raw txs, unlike the
//!   `MEVBlocker` wire's hash anchor),
//! - `blockNumber`: hex tag the bundle is valid for,
//! - `minTimestamp`/`maxTimestamp`: bracket the expected block,
//! - `revertingTxHashes`: empty — the backrun reverts safely on a failed
//!   guarantee, mirroring the executor's revert shield.
//!
//! Auth per relay-ops docs: `X-Flashbots-Signature: <operator address>:<sig>`
//! where `sig` is the EIP-191 `personal_sign` over the EXACT JSON body (65-byte
//! r||s||v, legacy y-parity `27 + v`). A signature binds the body bytes, so the
//! header is derived per request and never cached.
//!
//! Transport is one-shot HTTPS POSTs with the same round-trip budget as the
//! `MEVBlocker` WS bid: a dropped relay is a no-cost miss, never a hang.

use std::time::Duration;

use alloy::primitives::Bytes;
use degenbot_core::op_warn;
use serde_json::{json, Value as Json};

/// The per-relay HTTPS round-trip budget (matches `submit::BUNDLE_RELAY_TIMEOUT`).
pub const RELAY_POST_TIMEOUT: Duration = Duration::from_millis(750);

/// The bundle payload one relay POST carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuilderRelayBundle {
    /// The TARGET tx's verbatim signed bytes (the txpool frame's `raw`).
    pub target_raw: Bytes,
    /// SIGNED raw backrun transaction bytes.
    pub backrun_raw: alloy::primitives::Bytes,
    /// The block the bundle is valid for (dispatch head + 1).
    pub block_number: u64,
}

/// Build the relay `eth_sendBundle` JSON-RPC body (golden-tested).
#[must_use]
pub fn builder_bundle_request(bundle: &BuilderRelayBundle) -> Json {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_sendBundle",
        "params": [ {
            "txs": [
                format!("0x{}", alloy::hex::encode(&bundle.target_raw)),
                format!("0x{}", alloy::hex::encode(&bundle.backrun_raw)),
            ],
            "blockNumber": format!("0x{:x}", bundle.block_number),
            "minTimestamp": 0,
            "maxTimestamp": 600,
            "revertingTxHashes": [],
        } ]
    })
}

/// The `X-Flashbots-Signature` header value for one body:
/// `<operator address>:<0x-prefixed 65-byte r||s||v signature>`.
///
/// # Errors
///
/// [`crate::SubmissionError::Sign`] when the ECDSA signing fails (key
/// corruption; never for a validly constructed signer).
pub fn flashbots_signature_header(
    signer: &crate::signer::TxSigner,
    body: &str,
) -> Result<String, crate::SubmissionError> {
    let signature = signer.sign_message_eip191(body.as_bytes())?;
    Ok(format!("{}:0x{}", signer.address(), signature))
}

/// One relay's one-shot POST outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayOutcome {
    /// HTTP 200 with a JSON-RPC result (the relay accepted the bundle).
    Accepted,
    /// The relay rejected it (HTTP error or a JSON-RPC `error` object).
    Rejected,
}

async fn post_relay(relay_url: &str, body: &str, auth: &str) -> RelayOutcome {
    let client = match reqwest::Client::builder()
        .timeout(RELAY_POST_TIMEOUT)
        .connect_timeout(RELAY_POST_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            op_warn!(domain = exec, %e, url = %relay_url, "relay client build failed");
            return RelayOutcome::Rejected;
        }
    };
    let resp = client
        .post(relay_url)
        .header("X-Flashbots-Signature", auth)
        .header("Content-Type", "application/json")
        .body(body.to_owned())
        .send()
        .await;
    let Ok(resp) = resp else {
        return RelayOutcome::Rejected;
    };
    if !resp.status().is_success() {
        return RelayOutcome::Rejected;
    }
    // HTTP 200 + a JSON-RPC `error` is still a rejection (flashbots relays
    // answer wire-level 200 with typed bundle errors).
    match resp.json::<Json>().await {
        Ok(v) if v.get("error").is_none() => RelayOutcome::Accepted,
        _ => RelayOutcome::Rejected,
    }
}

/// POST one bundle to every relay (ordered), each within `RELAY_POST_TIMEOUT`.
///
/// Returns the per-relay outcomes in the input order: no URL produces an
/// `Accepted` through a rejected POST, so the caller's "first acceptance wins"
/// read is simply `.iter().any(Accepted)`.
pub async fn send_bundle_to_relays(
    relay_urls: &[String],
    bundle_request: &Json,
    signature_header: &str,
) -> Vec<RelayOutcome> {
    let body = bundle_request.to_string();
    let posts = relay_urls
        .iter()
        .map(|url| post_relay(url, &body, signature_header));
    futures_util::future::join_all(posts).await
}

/// Count helper the submit loop reads for its telemetry buckets.
#[must_use]
pub fn accepted_count(outcomes: &[RelayOutcome]) -> usize {
    outcomes
        .iter()
        .filter(|o| **o == RelayOutcome::Accepted)
        .count()
}
