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
//! - hex quantity strings (`0x` + up to 16 hex digits — block tags, gas,
//!   value) are normalized to decimal ("decimal block keys");
//! - everything else recorded on the wire stays verbatim: addresses and data
//!   hex are NOT rewritten (the wire form is already canonical for them);
//! - entries live in a `BTreeMap`, so the ledger serializes sorted by
//!   `(method, canonical params)`.
//!
//! Responses are canonicalized the same way, so any byte-level mutation of a
//! recorded response changes the canonical bytes — the drift gate
//! ([`verify_cassette_bytes`]) goes red.
//!
//! The pipeline that WRITES a capture ([`RecordingTransport`] flushed through
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

/// Normalize a hex *quantity* string to decimal: `0x` + 1..=16 hex digits
/// (a `u64`). Long hex forms — addresses (40 digits), data words, topics —
/// and `0x` itself stay verbatim: their wire form is already canonical.
fn canonical_hex_quantity(s: &str) -> Option<String> {
    let hex = s.strip_prefix("0x")?;
    if hex.is_empty() || hex.len() > 16 {
        return None;
    }
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
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
    let (year, month, day) = civil_from_days(days as i64);
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
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
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
                    .map(|v| canonicalize_value(&v))
                    .unwrap_or(Value::Null),
            },
            ResponsePayload::Failure(err) => CassetteResponse::Failure {
                error: serde_json::to_value(err)
                    .map(|v| canonicalize_value(&v))
                    .unwrap_or(Value::Null),
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
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
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
        // Leading zeros collapse.
        assert_eq!(canonical_hex_quantity("0x0010"), Some("16".to_string()));
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
                from_block: 26102618,
                to_block: 26102619,
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
}
