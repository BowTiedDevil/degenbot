//! Gap-prefix hydration probes: sample the four RPC lanes the
//! quarantine task relies on (per-sender only, NEVER a full-pool scan) and
//! emit their latency/presence evidence through the JSONL trace. The honest
//! cost model of gap rescue is measured here, not retro-fitted after the FSM
//! ships.

use std::sync::Arc;
use std::time::Instant;

use alloy::primitives::Address;
use degenbot_rpc::provider::AlloyProvider;

/// Per-lane latency + whether the lane returned usable evidence.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct LaneResult {
    /// Wall-clock of the underlying JSON-RPC call.
    pub ms: u64,
    /// The lane returned valid data (`u64::MAX` sentinel means no usable value).
    pub valid: bool,
}

/// One quarantine-boundary probe: four lanes sampled against one sender.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GapProbeOutcome {
    /// `eth_getTransactionCount(sender, "latest")` evidence.
    pub latest_count: LaneResult,
    /// Parent-state nonce (`u64::MAX` when the lane did not answer).
    pub latest_nonce: u64,
    /// `eth_getTransactionCount(sender, "pending")` evidence.
    pub pending_count: LaneResult,
    pub pending_nonce: u64,
    /// First gap-nonce fetch (`eth_getTransactionBySenderAndNonce`): wired
    /// fast lookup for one missing predecessor.
    pub gap_first: LaneResult,
    pub gap_first_present: bool,
    /// `txpool_contentFrom(sender)`: presence check (pending ∪ queued); the
    /// actual payload decoding lives with the quarantine task.
    pub pool_entry: LaneResult,
    pub pool_entry_present: bool,
}

/// One sender's gap-boundary probe operations. Cheap: at most one call per
/// lane per invocation.
pub struct GapProbe {
    /// The chain node the driver reads through. The probe speaks its raw
    /// methods on the same provider every other chain read uses, so a lane can
    /// never be sampled over a transport the driver does not otherwise hold.
    provider: Arc<AlloyProvider>,
}

impl GapProbe {
    /// Wrap the chain node the driver already carries (NOT the relay socket —
    /// pool evidence and nonce provisioning chain-side only).
    #[must_use]
    pub fn new(provider: Arc<AlloyProvider>) -> Self {
        Self { provider }
    }

    /// Sample all four lanes once at the boundary between the chain head' + chr(39) + 's
    /// account nonce and a relayed frame' + chr(39) + 's claimed nonce. Returns each lane' + chr(39) + 's
    /// latency + presence evidence.
    pub async fn probe_gap(&self, sender: Address, claimed: u64) -> GapProbeOutcome {
        let _ = claimed;
        let t = Instant::now();
        let latest = self.nonce_of(sender, "latest").await;
        let latest_ms = ms(&t);
        let latest_nonce = latest.unwrap_or(u64::MAX);

        let t = Instant::now();
        let pending = self.nonce_of(sender, "pending").await;
        let pending_ms = ms(&t);
        let pending_nonce = pending.unwrap_or(u64::MAX);

        let t = Instant::now();
        let expected = latest_nonce;
        let nonce_hex = format!("0x{expected:x}");
        let first = self
            .provider
            .make_request(
                "eth_getTransactionBySenderAndNonce",
                serde_json::json!([sender, nonce_hex]),
            )
            .await;
        let gap_first_ms = ms(&t);
        let gap_first_present = first.as_ref().is_ok_and(|v| !v.is_null());

        let t = Instant::now();
        let entry = self
            .provider
            .make_request("txpool_contentFrom", serde_json::json!([sender]))
            .await;
        let pool_entry_ms = ms(&t);
        let pool_entry_present = entry.as_ref().is_ok_and(|v| {
            v.as_object().is_some_and(|m| {
                m.get("pending")
                    .and_then(serde_json::Value::as_object)
                    .is_some_and(|p| !p.is_empty())
                    || m.get("queued")
                        .and_then(serde_json::Value::as_object)
                        .is_some_and(|q| !q.is_empty())
            })
        });

        GapProbeOutcome {
            latest_count: LaneResult {
                ms: latest_ms,
                valid: latest.is_some(),
            },
            latest_nonce,
            pending_count: LaneResult {
                ms: pending_ms,
                valid: pending.is_some(),
            },
            pending_nonce,
            gap_first: LaneResult {
                ms: gap_first_ms,
                valid: first.is_ok(),
            },
            gap_first_present,
            pool_entry: LaneResult {
                ms: pool_entry_ms,
                valid: entry.is_ok(),
            },
            pool_entry_present,
        }
    }

    /// `eth_getTransactionCount` at `block` (a tag: `latest` or `pending`).
    /// `None` is an unanswered or unparsable lane, which the outcome reports
    /// as the `u64::MAX` sentinel rather than a nonce.
    async fn nonce_of(&self, sender: Address, block: &str) -> Option<u64> {
        let value = self
            .provider
            .make_request(
                "eth_getTransactionCount",
                serde_json::json!([sender, block]),
            )
            .await
            .ok()?;
        quantity_u64(&value)
    }
}

/// A JSON-RPC quantity (`"0x1f"`) as a `u64`.
///
/// A nonce lane that answers with a value too wide for `u64` is NO evidence,
/// not a saturated one: the callers turn `None` into a skipped tick, so a
/// fabricated `u64::MAX` would read as consumption.
#[must_use]
pub fn quantity_u64(value: &serde_json::Value) -> Option<u64> {
    u64::from_str_radix(value.as_str()?.trim_start_matches("0x"), 16).ok()
}

fn ms(t: &Instant) -> u64 {
    u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX)
}
