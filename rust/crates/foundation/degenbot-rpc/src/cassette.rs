//! Golden-capture cassettes and the recording transport (ADR-068 D1/D2).
//!
//! A **cassette** is the wire-form half of a golden capture: the ordered RPC
//! `(method, canonical params) → raw JSON response` ledger for a pinned block
//! span, replayable in place of a live node. Schema v1 is additive to the
//! [`crate::offline`] recorded JSON — it carries a per-capture `chain_id`,
//! provenance (`source`, `recorded_at`, the pinned `span`), and the entry
//! ledger — while [`crate::offline`] parsing stays untouched.
//!
//! # Canonical entries (the drift-gate precondition)
//!
//! Entry keys canonicalize params so regeneration is **byte-identical**:
//!
//! - object keys are sorted recursively;
//! - hex quantity strings are normalized to decimal under a precision rule
//!   ("decimal block keys"): ONLY minimal-form quantities decimalize — a
//!   `0x` prefix followed by 1..=16 hex digits with no leading zero digit,
//!   with `0x0` the canonical zero — and that decimalization is exact
//!   (replay re-hexes the decimal to the same minimal wire form). A
//!   leading-zero digit marks a NON-minimal string — byte-hex, not a
//!   quantity (`0x00`, `0x00000000`, `0x06fdde03`) — which decimalizing
//!   would mangle (`0x00000000` → `0` → replayed `0x0`: different bytes,
//!   in an odd-length form `Bytes` decoding rejects), so non-minimal hex
//!   is preserved verbatim;
//! - everything else recorded on the wire stays verbatim: addresses and data
//!   hex are NOT rewritten (the wire form is already canonical for them),
//!   and ≥17-digit hex never parsed as a quantity anyway;
//! - entries live in a `BTreeMap`, so the ledger serializes sorted by
//!   `(method, canonical params)`.
//!
//! Replay restores the wire form byte-exactly for both classes
//! (`cassette_replay::wire_value`). The residual — deciding
//! quantity-vs-data by FIELD instead of by shape — is the v2
//! field-aware-canonicalization note (see
//! `docs/updater-rpc-sql-survey.md`, "Cassette hex canonicalization").
//!
//! Responses are canonicalized the same way, so any byte-level mutation of a
//! recorded response changes the canonical bytes — the drift gate
//! ([`verify_cassette_bytes`]) goes red.
//!
//! The pipeline that writes a capture ([`RecordingTransport`] flushed through
//! [`Cassette::canonical_bytes`]) is the same pipeline the gate runs.

use std::collections::BTreeMap;
use std::sync::Arc;

use alloy::providers::{Provider, RootProvider};
use alloy::pubsub::PubSubConnect as _;
use alloy::rpc::client::RpcClient;
use alloy::rpc::json_rpc::{
    RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{BoxTransport, Transport, TransportFut};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::AlloyProvider;

/// The schema marker written into every v1 cassette.
pub const CASSETTE_SCHEMA_V1: &str = "degenbot.cassette/v1";

/// A golden-capture cassette (schema v1).
///
/// Parse with [`Cassette::from_json_bytes`]; the committed artifact must
/// equal [`Cassette::canonical_bytes`] (see [`verify_cassette_bytes`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cassette {
    /// The schema marker — [`CASSETTE_SCHEMA_V1`] on any current capture.
    pub schema: String,
    /// The chain the capture was taken against (`eth_chainId`).
    pub chain_id: u64,
    /// Where and when the capture was taken, and the pinned block span.
    pub provenance: CassetteProvenance,
    /// The `(method, canonical params) → response` ledger. Ordered by the
    /// canonical key, so regeneration is byte-identical (the drift-gate
    /// precondition).
    pub entries: BTreeMap<String, CassetteEntry>,
}

/// Where a cassette's answers came from, and which blocks they pin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CassetteProvenance {
    /// Identity of the recording node (the `web3_clientVersion` string, e.g.
    /// `reth/v2.7.0-3d592ec/x86_64-unknown-linux-gnu`).
    pub source: String,
    /// RFC 3339 UTC wall-clock time the capture was taken
    /// (`YYYY-MM-DDTHH:MM:SSZ`).
    pub recorded_at: String,
    /// The pinned block span the capture covers.
    pub span: CassetteSpan,
}

/// A pinned block span (inclusive on both ends).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CassetteSpan {
    /// First recorded block (decimal).
    pub from_block: u64,
    /// Last recorded block (decimal).
    pub to_block: u64,
}

/// One ledger entry: the recorded answer plus the recorder's write digest.
///
/// The digest ([`entry_digest`]) pins the entry's canonical response bytes.
/// Without it, a single-byte flip inside a *value* of an already-canonical
/// cassette would re-canonicalize to itself and the formatting gate alone
/// would stay inertly green; the recomputed digest is what makes any
/// response mutation red (the negative-probe discipline).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CassetteEntry {
    /// The recorded answer.
    pub response: CassetteResponse,
    /// `0x`-prefixed keccak256 of the canonical compact JSON of
    /// [`CassetteEntry::response`] — written by the recorder, recomputed by
    /// the drift gate.
    pub digest: String,
}

/// The recorder's write digest for an entry: keccak256 over the canonical
/// compact JSON serialization of the response.
#[must_use]
pub fn entry_digest(response: &CassetteResponse) -> String {
    let canonical =
        serde_json::to_string(response).unwrap_or_else(|_| "unserializable".to_string());
    alloy::primitives::keccak256(canonical.as_bytes()).to_string()
}

/// A recorded RPC answer: the JSON-RPC `result` or the `error` payload.
///
/// Stored as canonical [`Value`]s (not raw wire strings) so the committed
/// cassette is fully canonical — a mutated response byte changes the
/// canonical bytes and fails [`verify_cassette_bytes`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CassetteResponse {
    /// A successful JSON-RPC answer (`result` — may be JSON `null`).
    Success {
        /// The canonicalized `result` value.
        result: Value,
    },
    /// A JSON-RPC error answer (revert, method-not-found, …).
    Failure {
        /// The canonicalized error object (`code`/`message`/optional `data`).
        error: Value,
    },
}

impl Cassette {
    /// Parse a cassette from a JSON byte slice.
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if the bytes are not a v1 cassette.
    pub fn from_json_bytes(bytes: &[u8]) -> serde_json::Result<Self> {
        serde_json::from_slice(bytes)
    }

    /// Parse a cassette from a JSON string.
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if the string is not a v1 cassette.
    pub fn from_json_str(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s)
    }

    /// The canonical serialization — the exact bytes a recorder writes and
    /// the only byte-form the drift gate accepts for a committed cassette.
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if serialization fails (cannot happen
    /// for values parsed from JSON or recorded from JSON-RPC).
    pub fn canonical_bytes(&self) -> serde_json::Result<Vec<u8>> {
        serde_json::to_vec_pretty(self)
    }
}

/// The drift gate for a committed cassette (GLOSSARY "drift gate" +
/// "negative probe"): parse, re-serialize through the recorder's own
/// canonical writer, and require byte-identity. Any hand edit or recorded
/// byte mutation diverges the canonical form and fails.
///
/// # Errors
///
/// Returns the reason the cassette failed the gate.
pub fn verify_cassette_bytes(bytes: &[u8]) -> Result<(), String> {
    let cassette =
        Cassette::from_json_bytes(bytes).map_err(|e| format!("cassette parse failed: {e}"))?;
    if cassette.schema != CASSETTE_SCHEMA_V1 {
        return Err(format!(
            "cassette schema {:?} is not {}",
            cassette.schema, CASSETTE_SCHEMA_V1
        ));
    }
    let canonical = cassette
        .canonical_bytes()
        .map_err(|e| format!("cassette re-serialization failed: {e}"))?;
    if canonical != bytes {
        return Err(format!(
            "cassette is not canonical ({} bytes on file, {} canonical): regenerate with \
             the recorder (`record_updater_cassette`) — the drift gate requires byte-identity",
            bytes.len(),
            canonical.len()
        ));
    }
    for (key, entry) in &cassette.entries {
        let recomputed = entry_digest(&entry.response);
        if recomputed != entry.digest {
            return Err(format!(
                "cassette entry {key} fails its write digest (recorded {}, recomputed {recomputed}): \
                 the recorded response was mutated — regenerate with the recorder",
                entry.digest
            ));
        }
    }
    Ok(())
}

/// Canonicalize a JSON value: sort object keys recursively and normalize hex
/// quantity strings to decimal. Addresses and data hex stay as recorded on
/// the wire (the wire form is already canonical for them).
#[must_use]
pub fn canonicalize_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            // Rebuild with sorted keys (robust even under a
            // serde_json `preserve_order` build, where Map is an IndexMap).
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::with_capacity(keys.len());
            for key in keys {
                out.insert(key.clone(), canonicalize_value(&map[key]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize_value).collect()),
        Value::String(s) => match canonical_hex_quantity(s) {
            Some(decimal) => Value::String(decimal),
            None => Value::String(s.clone()),
        },
        other => other.clone(),
    }
}

/// Decimalize a minimal-form hex *quantity* string: `0x` + 1..=16 hex
/// digits with no leading zero digit (`0x0` is the canonical zero) — a
/// `u64`.
///
/// The precision rule behind this shape: decimalization must be exact,
/// because replay re-hexes the decimal and the wire form has to come back
/// byte-identical. A minimal form does (`0x6fdde03` → `117300739` →
/// `0x6fdde03`; `0xf30dba93` → `4077763219` → `0xf30dba93`). A
/// leading-zero digit marks a NON-minimal string — byte-hex, not a
/// quantity (`0x00`, `0x00000000`, `0x06fdde03`) — and decimalizing it is
/// lossy: `0x00000000` → `0` → replayed `0x0`, different bytes in an
/// odd-length form `Bytes` decoding rejects. Non-minimal forms therefore
/// stay verbatim. Long hex — addresses (40 digits), data words, topics
/// (≥17 digits) — and `0x` itself stay verbatim too: their wire form is
/// already canonical.
fn canonical_hex_quantity(s: &str) -> Option<String> {
    let hex = s.strip_prefix("0x")?;
    if hex.is_empty() || hex.len() > 16 {
        return None;
    }
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    // The precision rule: a leading zero digit is a non-minimal form —
    // byte-hex, not a quantity — and stays verbatim (decimalizing it would
    // not re-hex back to the same wire string). The one-digit `0x0` is the
    // canonical zero and decimalizes.
    if hex.len() > 1 && hex.starts_with('0') {
        return None;
    }
    let n = u64::from_str_radix(hex, 16).ok()?;
    Some(n.to_string())
}

/// The ledger key for a request: a compact JSON pair
/// `[method, canonical params]`. Deterministic by construction.
pub(crate) fn entry_key(method: &str, params: &Value) -> String {
    let key = Value::Array(vec![
        Value::String(method.to_string()),
        canonicalize_value(params),
    ]);
    serde_json::to_string(&key).unwrap_or_else(|_| format!("{method:?}"))
}

/// Format unix seconds as an RFC 3339 UTC timestamp
/// (`YYYY-MM-DDTHH:MM:SSZ`). Dependency-free (no `time`/`chrono` in this
/// crate); verified against fixed literals in the tests.
#[must_use]
pub fn rfc3339_utc(unix_secs: u64) -> String {
    let days = unix_secs / 86_400;
    let secs_of_day = unix_secs % 86_400;
    // `days` is a quotient of non-negative integers, so the `i64` widening
    // `civil_from_days` expects cannot wrap.
    let (year, month, day) = civil_from_days(days.cast_signed());
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60,
        secs_of_day % 60
    )
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 → (y, m, d).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    // Hinnant-algorithm invariants: the day-of-month below lands in [1, 31]
    // and the month in [1, 12] (pinned by the `rfc3339_utc_known_instants`
    // test), so the u32 widenings never fail; the `unwrap_or` is unreachable.
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(0);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(0);
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The recording transport: an alloy `Transport` decorator (sibling of the
/// offline transport) that forwards every JSON-RPC packet to a live inner
/// transport and records each `(method, canonical params) → response` pair
/// into the in-memory ledger.
///
/// Wrap as a real [`AlloyProvider`] via [`RecordingTransport::as_alloy_provider`]
/// and run the updaters unchanged; flush the capture with
/// [`RecordingTransport::cassette`] → [`Cassette::canonical_bytes`].
#[derive(Clone, Debug)]
pub struct RecordingTransport {
    inner: BoxTransport,
    entries: Arc<Mutex<BTreeMap<String, CassetteEntry>>>,
}

impl RecordingTransport {
    /// Decorate a live inner transport.
    #[must_use]
    pub fn new(inner: BoxTransport) -> Self {
        Self {
            inner,
            entries: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Decorate a live endpoint (HTTP, WS, or IPC — the same transports the
    /// live provider accepts), resolved through alloy's `TransportConnect`.
    ///
    /// Note: the wrapped WS/IPC transport serves *requests* only — the
    /// recorder never subscribes, so the pubsub frontend is left unwired.
    ///
    /// # Errors
    ///
    /// Returns a message when the URL scheme is unrecognized or the
    /// WS/IPC handshake fails.
    pub async fn connect(rpc_url: &str) -> Result<Self, String> {
        let transport = if rpc_url.starts_with("http://") || rpc_url.starts_with("https://") {
            let url = rpc_url
                .parse()
                .map_err(|e| format!("invalid RPC URL {rpc_url:?}: {e}"))?;
            alloy::transports::http::Http::new(url).boxed()
        } else if rpc_url.starts_with("ws://") || rpc_url.starts_with("wss://") {
            alloy::transports::ws::WsConnect::new(rpc_url)
                .into_service()
                .await
                .map_err(|e| format!("ws connect {rpc_url:?}: {e}"))?
                .boxed()
        } else if rpc_url.starts_with("ipc://")
            || rpc_url.starts_with('/')
            || rpc_url.starts_with("\\\\")
        {
            let path = rpc_url.strip_prefix("ipc://").unwrap_or(rpc_url);
            alloy::transports::ipc::IpcConnect::new(path.to_string())
                .into_service()
                .await
                .map_err(|e| format!("ipc connect {rpc_url:?}: {e}"))?
                .boxed()
        } else {
            let scheme = rpc_url.split("://").next().unwrap_or(rpc_url);
            return Err(format!(
                "unsupported transport scheme {scheme:?} for recording: {rpc_url:?}"
            ));
        };
        Ok(Self::new(transport))
    }

    /// Wrap this recording transport as a live [`AlloyProvider`] — the same
    /// wiring [`crate::offline::OfflineProvider::as_alloy_provider`] uses, so
    /// updaters run unchanged over the recording seam. The clone shares the
    /// in-memory ledger, so the caller keeps reading entries while the
    /// provider runs.
    #[must_use]
    pub fn as_alloy_provider(&self) -> AlloyProvider {
        let client = RpcClient::new(self.clone(), false);
        let root: RootProvider = RootProvider::new(client);
        let arc: Arc<dyn Provider<_>> = Arc::new(root);
        AlloyProvider::from_provider(arc)
    }

    /// Snapshot the ledger into a cassette with the caller-supplied
    /// provenance (the span the driver pinned, the recording node identity,
    /// and the capture wall-clock time).
    #[must_use]
    pub fn cassette(&self, chain_id: u64, provenance: CassetteProvenance) -> Cassette {
        Cassette {
            schema: CASSETTE_SCHEMA_V1.to_string(),
            chain_id,
            provenance,
            entries: self.entries.lock().clone(),
        }
    }

    /// Number of distinct recorded entries so far.
    #[must_use]
    pub fn recorded_len(&self) -> usize {
        self.entries.lock().len()
    }

    /// Record one aligned request/response pair (first write wins — a
    /// repeated request against a pinned span is the same answer).
    fn record_pair(&self, req: &SerializedRequest, resp: &Response) {
        let params = req
            .params()
            .and_then(|rv| serde_json::from_str::<Value>(rv.get()).ok())
            .unwrap_or(Value::Null);
        let key = entry_key(req.method(), &params);
        let response = match &resp.payload {
            ResponsePayload::Success(raw) => CassetteResponse::Success {
                result: serde_json::from_str::<Value>(raw.get())
                    .map_or_else(|_| Value::Null, |v| canonicalize_value(&v)),
            },
            ResponsePayload::Failure(err) => CassetteResponse::Failure {
                error: serde_json::to_value(err)
                    .map_or_else(|_| Value::Null, |v| canonicalize_value(&v)),
            },
        };
        let entry = CassetteEntry {
            digest: entry_digest(&response),
            response,
        };
        self.entries.lock().entry(key).or_insert(entry);
    }

    fn record_packet(&self, req: &RequestPacket, resp: &ResponsePacket) {
        match (req, resp) {
            (RequestPacket::Single(req), ResponsePacket::Single(resp)) => {
                self.record_pair(req, resp);
            }
            (RequestPacket::Batch(reqs), ResponsePacket::Batch(resps)) => {
                for (req, resp) in reqs.iter().zip(resps.iter()) {
                    self.record_pair(req, resp);
                }
            }
            // Mismatched shapes cannot come from a conforming transport;
            // nothing meaningful is recordable.
            _ => {}
        }
    }
}

impl RecordingTransport {
    /// Forward the packet to the inner transport and record the answers.
    /// Shared by the `Service` impls for `RecordingTransport` and
    /// `Arc<RecordingTransport>` (the Arc form is what `RpcClient` boxes).
    fn forward(&self, req: RequestPacket) -> TransportFut<'static> {
        let mut inner = self.inner.clone();
        let this = self.clone();
        Box::pin(async move {
            let resp = tower::Service::call(&mut inner, req.clone()).await?;
            this.record_packet(&req, &resp);
            Ok(resp)
        })
    }
}

impl tower::Service<RequestPacket> for RecordingTransport {
    type Response = ResponsePacket;
    type Error = alloy::transports::TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        self.forward(req)
    }
}

#[cfg(test)]
// Test assertions fail by design through panic!/expect, so the panic-family
// deny is expected away for this module only — the same scoping as the
// unwrap/expect allowances beside it.
#[expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::cassette_replay::{wire_hex_quantity, wire_value};
    use alloy::rpc::json_rpc::ErrorPayload;
    use alloy::transports::mock::Asserter;
    use std::borrow::Cow;

    // ------------------------------------------------------------------
    // Canonical params — expected values are independent hand-written
    // literals, not recomputed through the code under test.
    // ------------------------------------------------------------------

    #[test]
    fn canonical_params_sort_keys_and_decimalize_block_tags() {
        // A wire-form eth_getLogs filter (alloy emits hex quantities): keys
        // out of order, hex block tags. The canonical form must sort the
        // keys and decimalize the quantities while leaving the 40-digit
        // address and the 64-digit topics verbatim.
        let wire: Value = serde_json::from_str(
            r#"{
                "toBlock": "0x20",
                "address": ["0x1f98431c8ad98523631ae4a59f267346ea31f984"],
                "fromBlock": "0x10",
                "topics": [[
                    "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118"
                ]]
            }"#,
        )
        .unwrap();
        let canonical = canonicalize_value(&wire);
        let expected: Value = serde_json::from_str(
            r#"{
                "address": ["0x1f98431c8ad98523631ae4a59f267346ea31f984"],
                "fromBlock": "16",
                "toBlock": "32",
                "topics": [[
                    "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118"
                ]]
            }"#,
        )
        .unwrap();
        assert_eq!(canonical, expected);
    }

    #[test]
    fn canonical_key_pairs_method_and_params() {
        let params: Value = serde_json::from_str(r#"{"toBlock":"0x10"}"#).unwrap();
        let key = entry_key("eth_getLogs", &params);
        // Independent literal: method first, canonical (sorted + decimalized)
        // params second, compact.
        assert_eq!(key, r#"["eth_getLogs",{"toBlock":"16"}]"#);
    }

    #[test]
    fn canonical_hex_quantity_leaves_addresses_and_data_alone() {
        // 40-digit address: verbatim.
        assert_eq!(
            canonical_hex_quantity("0x1f98431c8ad98523631ae4a59f267346ea31f984"),
            None
        );
        // 64-digit topic: verbatim.
        assert_eq!(
            canonical_hex_quantity(
                "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118"
            ),
            None
        );
        // Bare 0x: verbatim (call data can be empty).
        assert_eq!(canonical_hex_quantity("0x"), None);
        // 16-digit quantity (u64 max): decimalized.
        assert_eq!(
            canonical_hex_quantity("0xffffffffffffffff"),
            Some("18446744073709551615".to_string())
        );
        // The precision rule: minimal forms decimalize exactly — odd- and
        // even-length alike (independent decimal literals).
        assert_eq!(
            canonical_hex_quantity("0x6fdde03"),
            Some("117300739".to_string())
        );
        assert_eq!(
            canonical_hex_quantity("0xf30dba93"),
            Some("4077763219".to_string())
        );
        // `0x0` is the canonical zero.
        assert_eq!(canonical_hex_quantity("0x0"), Some("0".to_string()));
        // Leading-zero forms are NON-minimal — byte-hex, not quantities:
        // verbatim, not decimalized (decimalizing 0x0010 to 16 would
        // replay as 0x10, a different wire string; 0x00000000 would replay
        // as the odd-length 0x0, which Bytes decoding rejects).
        assert_eq!(canonical_hex_quantity("0x0010"), None);
        assert_eq!(canonical_hex_quantity("0x00000000"), None);
        assert_eq!(canonical_hex_quantity("0x06fdde03"), None);
        // Not hex.
        assert_eq!(canonical_hex_quantity("latest"), None);
    }

    #[test]
    fn rfc3339_utc_known_instants() {
        // Independent literals (well-known epoch facts).
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(1_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(rfc3339_utc(2_147_483_647), "2038-01-19T03:14:07Z");
    }

    // ------------------------------------------------------------------
    // RecordingTransport at the public seam: drive a real AlloyProvider
    // over a mock inner transport, inspect the flushed cassette.
    // ------------------------------------------------------------------

    fn provenance() -> CassetteProvenance {
        CassetteProvenance {
            source: "mock-node/1.0".to_string(),
            recorded_at: rfc3339_utc(1_770_000_000),
            span: CassetteSpan {
                from_block: 26_102_618,
                to_block: 26_102_619,
            },
        }
    }

    #[tokio::test]
    async fn recording_transport_records_forwarded_answers() {
        let asserter = Asserter::new();
        asserter.push_success(&"0x1234"); // eth_blockNumber → 4660
        let recorder =
            RecordingTransport::new(alloy::transports::mock::MockTransport::new(asserter).boxed());
        let provider = recorder.as_alloy_provider();

        let n = provider.get_block_number().await.unwrap();
        assert_eq!(n, 4_660); // independent literal: 0x1234

        let cassette = recorder.cassette(1, provenance());
        assert_eq!(cassette.schema, CASSETTE_SCHEMA_V1);
        assert_eq!(cassette.entries.len(), 1);
        let (key, entry) = cassette.entries.iter().next().unwrap();
        assert!(key.starts_with("[\"eth_blockNumber\","), "key was {key}");
        // The recorded quantity result is canonicalized to decimal
        // (0x1234 = 4660, an independent literal).
        assert_eq!(
            entry.response,
            CassetteResponse::Success {
                result: Value::String("4660".to_string())
            }
        );
        // The write digest pins the canonical response bytes.
        assert_eq!(entry.digest, entry_digest(&entry.response));
    }

    #[tokio::test]
    async fn recording_transport_records_error_payloads() {
        let asserter = Asserter::new();
        // A non-revert RPC failure (a revert-classified message would
        // surface as ExecutionReverted at the provider layer).
        asserter.push_failure(ErrorPayload {
            code: -32005,
            message: Cow::Borrowed("node overloaded"),
            data: None,
        });
        let recorder =
            RecordingTransport::new(alloy::transports::mock::MockTransport::new(asserter).boxed());
        let provider = recorder.as_alloy_provider();
        let err = provider.get_block_number().await.unwrap_err();
        assert!(matches!(
            err,
            degenbot_core::errors::ProviderError::RpcError { .. }
        ));

        let cassette = recorder.cassette(1, provenance());
        let (_, entry) = cassette.entries.iter().next().unwrap();
        let CassetteResponse::Failure { error } = &entry.response else {
            panic!("expected a Failure entry, got {:?}", entry.response)
        };
        assert_eq!(error["code"], -32005);
        assert_eq!(error["message"], "node overloaded");
    }

    #[test]
    fn cassette_round_trips_byte_identical_through_canonical_writer() {
        let asserter = Asserter::new();
        asserter.push_success(&"0x1");
        let recorder =
            RecordingTransport::new(alloy::transports::mock::MockTransport::new(asserter).boxed());
        let cassette = recorder.cassette(1, provenance());
        let bytes = cassette.canonical_bytes().unwrap();

        // The gate: parse the written bytes, re-serialize with the same
        // writer, require byte-identity.
        verify_cassette_bytes(&bytes).unwrap();
    }

    /// Negative probe (unit level): flip one recorded response byte and the
    /// drift gate must go red. The demonstrated red-path transcript (the
    /// same check run through the driver's --check mode) is in the task
    /// result.
    #[tokio::test]
    async fn mutated_response_byte_fails_the_drift_gate() {
        let asserter = Asserter::new();
        asserter.push_success(&"0x1234");
        let recorder =
            RecordingTransport::new(alloy::transports::mock::MockTransport::new(asserter).boxed());
        let provider = recorder.as_alloy_provider();
        provider.get_block_number().await.unwrap();
        let cassette = recorder.cassette(1, provenance());
        let bytes = cassette.canonical_bytes().unwrap();
        verify_cassette_bytes(&bytes).expect("unmutated cassette must pass");

        // Flip one byte inside the recorded response value: the recorded
        // eth_blockNumber answer "4660" becomes "4661" — a different
        // recorded answer.
        let mut mutated = bytes.clone();
        let marker = b"\"4660\"";
        let pos = mutated
            .windows(marker.len())
            .position(|w| w == marker)
            .expect("recorded response literal present in canonical bytes");
        mutated[pos + 3] = b'1'; // "4660" → "4661"
        assert_ne!(mutated, bytes);

        let verdict = verify_cassette_bytes(&mutated);
        assert!(
            verdict.is_err(),
            "a mutated recorded response MUST fail the drift gate, got {verdict:?}"
        );
    }

    // ------------------------------------------------------------------
    // Committed corpus: the drift gate runs over the real fixtures.
    // ------------------------------------------------------------------

    fn corpus_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../tests/fixtures/cassettes")
    }

    /// Drift gate over the committed seed corpus: every cassette must be
    /// byte-identical to its canonical regeneration through the recorder's
    /// writer, and the corpus must carry at least the two pinned spans
    /// (one pool-updater chunk, one Aave market chunk).
    #[test]
    fn committed_cassette_corpus_is_byte_identical() {
        let dir = corpus_dir();
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cassette corpus missing at {}: {e}", dir.display()))
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
            .collect();
        files.sort();
        assert!(
            files.len() >= 2,
            "seed corpus must carry at least the pool + Aave cassettes, found {}",
            files.len()
        );
        for path in &files {
            let bytes = std::fs::read(path).unwrap();
            verify_cassette_bytes(&bytes)
                .unwrap_or_else(|e| panic!("drift gate RED for {}: {e}", path.display()));
        }
    }

    // ------------------------------------------------------------------
    // Short-hex precision rule: the corpus guard.
    //
    // The recorder decimalizes minimal-form quantities at record time and
    // preserves every other 0x form verbatim, so a committed cassette can
    // only carry hex the rule leaves untouched — and replay must restore
    // every token byte-exactly. These tests pin both halves.
    // ------------------------------------------------------------------

    /// The precision-rule class of one `0x`-hex token. Total and explicit:
    /// every token lands in exactly one named class, and
    /// [`assert_token_matches_rule`] asserts each class's canonicalization
    /// outcome — there is no catch-all arm.
    #[derive(Debug, PartialEq, Eq)]
    enum HexTokenClass {
        /// Minimal form (no leading-zero digit, ≤16 digits): decimalized
        /// at record time; replay re-hexes to the same wire string.
        MinimalQuantity,
        /// ≥17 digits: verbatim on both sides (never parsed as a quantity).
        LongHex,
        /// Leading-zero digit, ≤16 digits: byte-hex, not a quantity —
        /// preserved verbatim by the precision rule.
        NonMinimalHex,
        /// `0x` with no digits or a non-hex tail: not a quantity — verbatim.
        NotAQuantity,
    }

    /// Classify one `0x`-hex token exactly as [`canonical_hex_quantity`]
    /// dispatches it (the two must never diverge — the guard asserts it).
    fn hex_token_class(token: &str) -> HexTokenClass {
        let hex = token
            .strip_prefix("0x")
            .expect("the scanner yields 0x-prefixed tokens");
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return HexTokenClass::NotAQuantity;
        }
        if hex.len() > 16 {
            return HexTokenClass::LongHex;
        }
        if hex.len() > 1 && hex.starts_with('0') {
            return HexTokenClass::NonMinimalHex;
        }
        HexTokenClass::MinimalQuantity
    }

    /// The guard's per-token dispatch: each class's canonicalization
    /// outcome, asserted against the real rule function.
    fn assert_token_matches_rule(token: &str) {
        let decimalized = canonical_hex_quantity(token);
        match hex_token_class(token) {
            HexTokenClass::MinimalQuantity => {
                assert!(decimalized.is_some(), "{token:?} must decimalize");
            }
            HexTokenClass::LongHex | HexTokenClass::NonMinimalHex | HexTokenClass::NotAQuantity => {
                assert!(
                    decimalized.is_none(),
                    "{token:?} must stay verbatim (untouched-by-construction)"
                );
            }
        }
    }

    /// Scan one raw string (a serialized ledger key or a string value) for
    /// `0x` + hex-digit runs. Ledger keys embed hex inside JSON string
    /// literals; the quote characters are not hex digits and end a run.
    fn push_hex_tokens(s: &str, tokens: &mut Vec<String>) {
        let bytes = s.as_bytes();
        let mut i = 0;
        while i + 1 < bytes.len() {
            if bytes[i] == b'0' && bytes[i + 1] == b'x' {
                let mut end = i + 2;
                while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
                    end += 1;
                }
                tokens.push(s[i..end].to_string());
                i = end;
            } else {
                i += 1;
            }
        }
    }

    /// Walk a parsed cassette JSON and collect every `0x`-hex token from
    /// object keys (the serialized ledger keys) and string values.
    fn collect_hex_tokens(value: &Value, tokens: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, item) in map {
                    push_hex_tokens(key, tokens);
                    collect_hex_tokens(item, tokens);
                }
            }
            Value::Array(items) => {
                for item in items {
                    collect_hex_tokens(item, tokens);
                }
            }
            Value::String(s) => push_hex_tokens(s, tokens),
            _ => {}
        }
    }

    /// Corpus guard (green path): every `0x`-hex token in the committed
    /// corpus — ledger keys AND response values — must be
    /// untouched-by-construction (≥17 digits or non-minimal, now preserved
    /// verbatim), and every entry must be a byte-exact fixed point of
    /// canonicalization ∘ wire restoration. Together with the drift gate
    /// this proves the corpus replays byte-identically under the rule.
    #[test]
    fn committed_corpus_hex_tokens_round_trip_byte_exactly() {
        let dir = corpus_dir();
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cassette corpus missing at {}: {e}", dir.display()))
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
            .collect();
        files.sort();

        let mut total_tokens = 0usize;
        let mut total_entries = 0usize;
        for path in &files {
            let bytes = std::fs::read(path).unwrap();
            verify_cassette_bytes(&bytes)
                .unwrap_or_else(|e| panic!("drift gate RED for {}: {e}", path.display()));
            let cassette = Cassette::from_json_bytes(&bytes).unwrap();
            let parsed = serde_json::to_value(&cassette).unwrap();

            let mut tokens = Vec::new();
            collect_hex_tokens(&parsed, &mut tokens);
            for token in &tokens {
                // The class dispatch agrees with the rule function...
                assert_token_matches_rule(token);
                // ...and a committed cassette carries NO minimal-form
                // quantity: the recorder decimalizes it at record time, so
                // one in a committed file means a hand edit or a bypassed
                // recorder (the drift gate would also fail the mutated
                // canonical bytes).
                assert!(
                    !matches!(hex_token_class(token), HexTokenClass::MinimalQuantity),
                    "{token:?} in {}: a minimal-form quantity cannot survive \
                     recording — regenerate through the recorder",
                    path.display()
                );
            }
            total_tokens += tokens.len();

            // Entry-level byte-exactness: the ledger key re-derives from
            // its parsed (method, params), and the canonical response is a
            // fixed point of canonicalize ∘ wire — replay restores the
            // recorded wire form and re-recording it reproduces the
            // committed bytes, for both classes.
            for (key, entry) in &cassette.entries {
                let pair: Value = serde_json::from_str(key).unwrap();
                let items = pair.as_array().expect("ledger key is [method, params]");
                let method = items[0].as_str().expect("method is a string");
                assert_eq!(
                    entry_key(method, &items[1]),
                    *key,
                    "ledger key does not re-derive in {}",
                    path.display()
                );
                let response_value = serde_json::to_value(&entry.response).unwrap();
                let restored = canonicalize_value(&wire_value(&response_value));
                assert_eq!(
                    restored,
                    response_value,
                    "canonicalize ∘ wire is not the identity on a response in {}",
                    path.display()
                );
                total_entries += 1;
            }
        }
        assert!(
            total_tokens > 0,
            "the corpus guard scanned zero 0x-hex tokens — vacuous; the corpus changed shape"
        );
        assert!(
            total_entries > 0,
            "the corpus guard scanned zero ledger entries — vacuous; the corpus changed shape"
        );
    }

    /// Red path for the precision rule, demonstrated on an in-memory copy:
    /// a committed cassette's copy carrying a short NON-minimal value —
    /// `0x00000000`, the exact class the earlier rule decimalized — is
    /// flagged, and both halves of the rule's necessity are asserted. Under
    /// the implemented rule the token is preserved verbatim
    /// (`canonical_hex_quantity` refuses it; the canonicalize ∘ wire round
    /// trip is byte-exact). Under the pre-fix rule the same token
    /// decimalized to `0` and replay re-hexed it to `0x0` — different
    /// bytes, in an odd-length form `Bytes` decoding rejects; that lossy
    /// decimalize-then-restore is asserted here as the counterfactual the
    /// rule exists to prevent.
    #[test]
    fn corpus_guard_flags_nonminimal_short_hex_on_in_memory_copy() {
        // In-memory copy (never written to disk): the pool cassette's
        // recorded eth_blockNumber answer becomes the non-minimal token.
        let path = corpus_dir().join("pool_v3_created_26102622-26102626.json");
        let bytes = std::fs::read(&path).unwrap();
        let mut cassette = Cassette::from_json_bytes(&bytes).unwrap();
        let block_key = cassette
            .entries
            .keys()
            .find(|k| k.starts_with("[\"eth_blockNumber\""))
            .expect("the pool cassette records eth_blockNumber")
            .clone();
        // Mutate the recorded answer inside a scoped borrow; the mutated
        // response's canonical JSON form is what the fixed-point check
        // replays below.
        let response_value = {
            let entry = cassette.entries.get_mut(&block_key).expect("entry present");
            entry.response = CassetteResponse::Success {
                result: Value::String("0x00000000".to_string()),
            };
            serde_json::to_value(&entry.response).unwrap()
        };

        // The guard's scan flags the injected token: it classes as
        // NonMinimalHex — the protected byte-hex class — and the rule
        // function refuses to decimalize it (preserved verbatim).
        let copy = serde_json::to_value(&cassette).unwrap();
        let mut tokens = Vec::new();
        collect_hex_tokens(&copy, &mut tokens);
        assert!(
            tokens.contains(&"0x00000000".to_string()),
            "the scanner must see the injected non-minimal token"
        );
        assert_token_matches_rule("0x00000000");
        assert_eq!(hex_token_class("0x00000000"), HexTokenClass::NonMinimalHex);
        assert_eq!(canonical_hex_quantity("0x00000000"), None);

        // The mutated entry round-trips byte-exactly: replay restores the
        // recorded wire form and re-recording it reproduces the canonical
        // bytes — the rule working as designed on this input class.
        assert_eq!(
            canonicalize_value(&wire_value(&response_value)),
            response_value
        );

        // The counterfactual (what the pre-fix rule did to this token): the
        // lossy decimalize-then-restore that mangled such values —
        // 0x00000000 → "0" → replayed "0x0".
        let pre_fix_decimal = u64::from_str_radix("00000000", 16)
            .expect("8 hex digits fit a u64")
            .to_string();
        assert_eq!(pre_fix_decimal, "0");
        let pre_fix_restored = wire_hex_quantity(&pre_fix_decimal).expect("decimal re-hexes");
        assert_eq!(pre_fix_restored, "0x0");
        assert_ne!(
            pre_fix_restored, "0x00000000",
            "the pre-fix decimalize-then-restore mangled this value class; \
             the precision rule must keep preserving it verbatim"
        );
    }
}
