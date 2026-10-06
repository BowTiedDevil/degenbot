//! The cassette replay transport (ADR-068 D1): a committed cassette answers
//! the updaters' RPC surface in place of a live node.
//!
//! # Replay contract
//!
//! A replay request is looked up in the cassette ledger with the recorder's
//! own canonical key ([`entry_key`]), so a wire-form request (hex quantity
//! block tags, unsorted object keys) collides with its recorded entry exactly
//! as when the capture was taken. A recorded answer replays as-is:
//!
//! - `Success` results are re-hexed back to wire form. The canonical form
//!   stores u64 hex quantities as decimal strings (`"0x1234"` → `"4660"`,
//!   see `canonical_hex_quantity`), while the typed clients expect the wire
//!   form back (`"0x1234"`). Addresses, topics, and data hex are verbatim in
//!   the canonical form and pass through unchanged.
//! - `Failure` entries replay as JSON-RPC errors with their recorded
//!   `code`/`message`/`data`, so a recorded failure classifies at the
//!   provider layer exactly as it did live: a revert-classified message stays
//!   a revert, everything else is an RPC error.
//!
//! A served-method request missing from the ledger is a **fixture gap** — a
//! non-revert JSON-RPC error (the [`crate::offline`] unrecorded-call
//! contract): a missing fixture entry is a harness problem, not on-chain
//! semantics. Methods outside the served surface keep the offline
//! "method not found" contract; anything the ledger carries replays even
//! outside the served set.
//!
//! Wrap as a real [`AlloyProvider`] via
//! [`CassetteReplayTransport::as_alloy_provider`] and run `LogFetcher` (or
//! any updater fetch surface) unchanged over the committed corpus in
//! `tests/fixtures/cassettes/`.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use alloy::providers::{Provider, RootProvider};
use alloy::rpc::client::RpcClient;
use alloy::rpc::json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{TransportError, TransportFut};
use serde_json::value::to_raw_value;
use serde_json::Value;

use crate::cassette::{canonicalize_value, entry_key, Cassette, CassetteEntry, CassetteResponse};
use crate::provider::AlloyProvider;

/// The methods the replay surface serves from the ledger. A miss on one of
/// these is a fixture gap; a miss anywhere else is "method not found" (the
/// ledger is authoritative for anything it actually carries).
const SERVED_METHODS: [&str; 4] = [
    "eth_getLogs",
    "eth_getBlockByNumber",
    "eth_chainId",
    "eth_blockNumber",
];

/// The replay half of a golden capture: answers JSON-RPC requests from a
/// committed [`Cassette`]'s ledger — the sibling of
/// [`crate::offline::OfflineTransport`] over the wire-form capture instead of
/// per-block calls/code maps.
#[derive(Clone, Debug)]
pub struct CassetteReplayTransport {
    cassette: Arc<Cassette>,
    served: Arc<ServedTrace>,
}

impl CassetteReplayTransport {
    /// Build a replay transport over a committed cassette.
    #[must_use]
    pub fn new(cassette: Cassette) -> Self {
        Self {
            cassette: Arc::new(cassette),
            served: Arc::new(ServedTrace::default()),
        }
    }

    /// The serving stats accumulated since construction - every JSON-RPC
    /// request answered and every ledger entry served, with the wire bytes
    /// those answers carried. The snapshot is cumulative; delta two
    /// snapshots around one run (or build a fresh transport per run) to
    /// scope it. This is the replay bench's and the replay suites' RPC
    /// round-trip + response-bytes counter (ADR-068 D6 measurement): a run
    /// over a golden capture reports exactly the fetch surface the chunk
    /// loop actually issued, so a shaping regression (an added or removed
    /// round trip) is a plain-assertion red.
    ///
    /// Clones of the transport (including the one inside
    /// [`Self::as_alloy_provider`]'s client) share one trace - snapshot from
    /// the handle you kept.
    #[must_use]
    pub fn served_snapshot(&self) -> ServedSnapshot {
        self.served.snapshot()
    }

    /// Wrap this replay transport as a live [`AlloyProvider`] (no network) —
    /// the same wiring [`crate::offline::OfflineProvider::as_alloy_provider`]
    /// uses, so the fetchers run unchanged over the cassette.
    #[must_use]
    pub fn as_alloy_provider(&self) -> AlloyProvider {
        let client = RpcClient::new(self.clone(), true);
        let root: RootProvider = RootProvider::new(client);
        let arc: Arc<dyn Provider<_>> = Arc::new(root);
        AlloyProvider::from_provider(arc)
    }

    fn answer(&self, req: &SerializedRequest) -> Result<ResponsePayload, ErrorPayload> {
        let method = req.method();
        let params = req
            .params()
            .and_then(|rv| serde_json::from_str::<Value>(rv.get()).ok())
            .unwrap_or(Value::Null);
        // The recorder's own canonicalization, applied to the wire-form
        // request: hex quantities decimalize, object keys sort, verbatim hex
        // (addresses/topics/data) passes through — so the key collides with
        // the ledger entry exactly as recorded.
        self.served.record_request(method);
        let key = entry_key(method, &params);
        let Some(entry) = self.lookup(method, &params, &key) else {
            return Err(if SERVED_METHODS.contains(&method) {
                fixture_gap(&key)
            } else {
                method_not_found(method)
            });
        };
        match &entry.response {
            CassetteResponse::Success { result } => {
                let wire = if method == "eth_getLogs" {
                    wire_logs(result)
                } else {
                    wire_value(result)
                };
                self.served.record_served(method, json_byte_len(&wire));
                success(&wire)
            }
            CassetteResponse::Failure { error } => {
                self.served.record_served(method, json_byte_len(error));
                Err(failure_payload(error))
            }
        }
    }

    /// Ledger lookup: the exact canonical key first, then — for `eth_getLogs`
    /// only — an OR-set-order-insensitive scan (see [`logs_semantic_key`]).
    fn lookup(&self, method: &str, params: &Value, key: &str) -> Option<&CassetteEntry> {
        if let Some(entry) = self.cassette.entries.get(key) {
            return Some(entry);
        }
        if method != "eth_getLogs" {
            return None;
        }
        let want = logs_semantic_key(params.as_array()?.first()?)?;
        self.cassette.entries.iter().find_map(|(k, entry)| {
            let recorded: Value = serde_json::from_str(k).ok()?;
            let recorded_filter = recorded.as_array()?.get(1)?.as_array()?.first()?.clone();
            (logs_semantic_key(&recorded_filter).as_deref() == Some(want.as_str())).then_some(entry)
        })
    }

    fn respond(&self, req: &SerializedRequest) -> Response {
        Response {
            id: req.id().clone(),
            payload: self.answer(req).unwrap_or_else(ResponsePayload::Failure),
        }
    }

    fn handle(&self, packet: RequestPacket) -> ResponsePacket {
        match packet {
            RequestPacket::Single(req) => ResponsePacket::Single(self.respond(&req)),
            RequestPacket::Batch(reqs) => {
                ResponsePacket::Batch(reqs.iter().map(|req| self.respond(req)).collect())
            }
        }
    }
}

/// Per-method serving stats: how many ledger entries the transport answered
/// for one JSON-RPC method and how many wire bytes those answers carried.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MethodServed {
    /// Ledger entries served for this method.
    pub served: u64,
    /// Serialized payload bytes of the served answers (result or error
    /// member, wire form - not the JSON-RPC envelope).
    pub response_bytes: u64,
}

/// The serving stats a [`CassetteReplayTransport`] accumulates while it
/// answers a run. Shared by every clone of the transport (the client inside
/// [`CassetteReplayTransport::as_alloy_provider`] included), so a run's
/// counter is readable from the handle that built the provider.
#[derive(Debug, Default)]
struct ServedTrace {
    inner: Mutex<ServedTraceInner>,
}

#[derive(Debug, Default)]
struct ServedTraceInner {
    /// Every JSON-RPC request the transport answered (served entries,
    /// fixture gaps, and method-not-found misses alike).
    requests: u64,
    /// Ledger entries served - the RPC round-trip count a replayed run
    /// reports (ADR-068 D6).
    served: u64,
    /// Sum of the served answers' serialized payload bytes.
    response_bytes: u64,
    per_method: BTreeMap<String, MethodServed>,
}

impl ServedTrace {
    fn record_request(&self, method: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.requests = inner.requests.saturating_add(1);
            inner.per_method.entry(method.to_string()).or_default();
        }
    }

    fn record_served(&self, method: &str, response_bytes: u64) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.served = inner.served.saturating_add(1);
            inner.response_bytes = inner.response_bytes.saturating_add(response_bytes);
            let entry = inner.per_method.entry(method.to_string()).or_default();
            entry.served = entry.served.saturating_add(1);
            entry.response_bytes = entry.response_bytes.saturating_add(response_bytes);
        }
    }

    fn snapshot(&self) -> ServedSnapshot {
        let Ok(inner) = self.inner.lock() else {
            return ServedSnapshot::default();
        };
        ServedSnapshot {
            requests: inner.requests,
            served: inner.served,
            response_bytes: inner.response_bytes,
            per_method: inner.per_method.clone(),
        }
    }
}

/// A point-in-time copy of a transport's serving stats: the per-run RPC
/// round-trip count ([`ServedSnapshot::served`]), the response bytes those
/// answers carried, and the per-method breakdown (the fetch surface's
/// shape - how many getLogs passes, how many ancillary tag reads).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ServedSnapshot {
    /// Every JSON-RPC request answered (served + fixture gaps +
    /// method-not-found). Equal to [`ServedSnapshot::served`] on a healthy
    /// replay; a gap means the run asked for an unrecorded entry (a loud
    /// fixture problem, never a silent miss).
    pub requests: u64,
    /// Ledger entries served - the run's RPC round-trip count.
    pub served: u64,
    /// Serialized payload bytes of the served answers (the recorded
    /// `result`/`error` member in wire form; the JSON-RPC envelope is not
    /// counted). The response-bytes measure the perf program tracks.
    pub response_bytes: u64,
    /// The per-method breakdown, methods in sorted order.
    pub per_method: BTreeMap<String, MethodServed>,
}

/// The serialized byte size of a response value - the response-bytes
/// measure of one served answer. `serde_json` cannot fail on an in-memory
/// `Value`; the impossible error contributes 0 bytes rather than poisoning
/// the counter.
fn json_byte_len(value: &Value) -> u64 {
    u64::try_from(serde_json::to_vec(value).map_or(0, |bytes| bytes.len())).unwrap_or(u64::MAX)
}

impl tower::Service<RequestPacket> for CassetteReplayTransport {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        Box::pin(std::future::ready(Ok(self.handle(req))))
    }
}

/// Replay-side semantic key for an `eth_getLogs` filter: the recorder's
/// canonical form with one further normalization the ledger keys cannot
/// carry — OR-set option order. alloy serializes filter option sets (the
/// `address` list, each topic position) through a `HashSet` (`FilterSet`),
/// so their wire order is per-process random: the recorder's key stored ONE
/// such order, and a byte-equal collision across processes is not
/// recoverable from the committed corpus. Sorting the option sets makes the
/// match order-insensitive — semantically exact, since an OR-set is
/// order-free. Block tags, the address set contents, and every topic
/// position's options must still match exactly.
fn logs_semantic_key(filter: &Value) -> Option<String> {
    let mut canonical = canonicalize_value(filter);
    if let Some(address) = canonical.get_mut("address") {
        sort_json_array(address);
    }
    if let Some(topics) = canonical.get_mut("topics").and_then(Value::as_array_mut) {
        for position in topics {
            sort_json_array(position);
        }
    }
    serde_json::to_string(&canonical).ok()
}

/// Sort a JSON array value in place — the OR-set-order normalization. No-op
/// for non-arrays (a bare-string topic position is a single option, already
/// order-free).
fn sort_json_array(value: &mut Value) {
    if let Some(items) = value.as_array_mut() {
        items.sort_by_key(std::string::ToString::to_string);
    }
}

// ---------------------------------------------------------------------------
// Wire-form restoration (the inverse of the recorder's canonicalization)
// ---------------------------------------------------------------------------

/// Build a `Success` payload from a serializable value.
fn success<T: serde::Serialize>(value: &T) -> Result<ResponsePayload, ErrorPayload> {
    let raw =
        to_raw_value(value).map_err(|e| internal_err(format!("cassette replay encode: {e}")))?;
    Ok(ResponsePayload::Success(raw))
}

/// Restore the wire form of a canonical response value: decimal quantity
/// strings re-hex to `0x…` (the inverse of `canonical_hex_quantity` — the
/// canonical form stores u64 hex quantities as decimal), while addresses,
/// topics, and data hex pass through verbatim.
fn wire_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (key, item) in map {
                out.insert(key.clone(), wire_value(item));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(wire_value).collect()),
        Value::String(s) => match wire_hex_quantity(s) {
            Some(hex) => Value::String(hex),
            None => Value::String(s.clone()),
        },
        other => other.clone(),
    }
}

/// Re-hex a decimal quantity string: the inverse of the recorder's
/// `canonical_hex_quantity` (`"4660"` → `"0x1234"`). Verbatim hex (addresses,
/// data, topics) never parses as decimal and stays untouched.
fn wire_hex_quantity(s: &str) -> Option<String> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u128 = s.parse().ok()?;
    Some(format!("0x{n:x}"))
}

/// Serve a recorded `eth_getLogs` result: re-hexed to wire form and sorted by
/// `(block_number, log_index)` — the fetchers' deterministic-ordering contract.
fn wire_logs(result: &Value) -> Value {
    let Value::Array(logs) = result else {
        return wire_value(result);
    };
    let mut logs: Vec<Value> = logs.iter().map(wire_value).collect();
    logs.sort_by_key(|log| {
        (
            log_quantity(log, "blockNumber"),
            log_quantity(log, "logIndex"),
        )
    });
    Value::Array(logs)
}

/// Read a recorded log quantity field (`blockNumber`/`logIndex`) as a u64 for
/// ordering. The canonical form carries decimal strings; hex also parses
/// defensively. Missing fields sort first (the fetchers' `unwrap_or(0)`
/// fallback).
fn log_quantity(log: &Value, field: &str) -> u64 {
    match log.get(field) {
        Some(Value::String(s)) => s
            .strip_prefix("0x")
            .and_then(|hex| u64::from_str_radix(hex, 16).ok())
            .or_else(|| s.parse().ok())
            .unwrap_or(0),
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        _ => 0,
    }
}

/// Replay a recorded JSON-RPC error verbatim (recorded `code`/`message`/`data`,
/// wire-restored): the live node's failure, so the provider layer classifies
/// it exactly as it did live.
fn failure_payload(error: &Value) -> ErrorPayload {
    serde_json::from_value::<ErrorPayload>(wire_value(error)).unwrap_or_else(|e| {
        internal_err(format!(
            "cassette: malformed recorded error entry {error}: {e}"
        ))
    })
}

// ---------------------------------------------------------------------------
// JSON-RPC error builders (mirror the offline.rs contracts)
// ---------------------------------------------------------------------------

/// A served-method request the cassette does not carry. The message
/// deliberately omits "revert" so it is NOT misclassified as an on-chain
/// revert by the provider's revert marker check.
fn fixture_gap(key: &str) -> ErrorPayload {
    internal_err(format!(
        "cassette: fixture gap: no recorded entry for request {key}"
    ))
}

fn method_not_found(method: &str) -> ErrorPayload {
    ErrorPayload {
        code: -32601,
        message: Cow::Owned(format!("cassette: method not found: {method}")),
        data: None,
    }
}

fn internal_err(msg: String) -> ErrorPayload {
    ErrorPayload {
        code: -32603,
        message: Cow::Owned(msg),
        data: None,
    }
}

#[cfg(test)]
#[expect(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::cassette::{
        canonicalize_value, entry_digest, verify_cassette_bytes, CassetteEntry, CassetteProvenance,
        CassetteSpan, CASSETTE_SCHEMA_V1,
    };
    use crate::provider::{LogFetcher, LogFilter};
    use alloy::primitives::B256;
    use degenbot_core::errors::ProviderError;
    use serde_json::json;
    use std::collections::BTreeMap;

    // ── corpus seam: the committed cassettes replay through LogFetcher ────

    const POOL_CASSETTE_FILE: &str = "pool_v3_created_26102622-26102626.json";
    const AAVE_CASSETTE_FILE: &str = "aave_market_chunk_26130440-26130445.json";
    const POOL_CREATED_TOPIC0: &str =
        "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118";

    fn corpus_cassette(file: &str) -> Cassette {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../tests/fixtures/cassettes")
            .join(file);
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("cassette corpus missing at {}: {e}", path.display()));
        verify_cassette_bytes(&bytes).expect("committed corpus must pass the drift gate");
        Cassette::from_json_bytes(&bytes).unwrap()
    }

    /// The recorder example's pool request shape: `fetch_pool_created_logs` →
    /// `fetch_logs_chunked`, one full-span chunk at the recorder's
    /// 2000-block chunk size (the chunk-boundary collision precondition).
    #[tokio::test]
    async fn pool_corpus_replays_recorded_pool_created_log() {
        let transport = CassetteReplayTransport::new(corpus_cassette(POOL_CASSETTE_FILE));
        let fetcher = LogFetcher::new(Arc::new(transport.as_alloy_provider()), 2000);
        let logs = fetcher
            .fetch_logs_chunked(
                26_102_622,
                26_102_626,
                Some(vec![
                    "0x1F98431c8aD98523631AE4a59f267346ea31F984".to_string()
                ]),
                Some(vec![vec![POOL_CREATED_TOPIC0.to_string()]]),
            )
            .await
            .unwrap();
        assert_eq!(logs.len(), 1, "the cassette records exactly one log");
        let log = &logs[0];
        // Independent literals copied from the committed cassette entry (the
        // recorded wire answer), not recomputed through the code under test.
        // alloy `Address` Display is EIP-55 checksummed.
        assert_eq!(
            log.address().to_string(),
            "0x1F98431c8aD98523631AE4a59f267346ea31F984"
        );
        assert_eq!(log.block_number, Some(26_102_624));
        assert_eq!(log.log_index, Some(7));
        assert_eq!(log.block_timestamp, Some(1_790_919_803));
        assert_eq!(log.transaction_index, Some(1));
        assert!(!log.removed);
        assert_eq!(
            log.topics()[0],
            POOL_CREATED_TOPIC0.parse::<B256>().unwrap()
        );
        assert_eq!(
            log.topics()[1].to_string(),
            "0x0000000000000000000000006985884c4392d348587b19cb9eaaf157f13271cd"
        );
        assert_eq!(
            log.topics()[3].to_string(),
            "0x0000000000000000000000000000000000000000000000000000000000000bb8"
        );
        assert_eq!(
            log.data().data.to_string(),
            "0x000000000000000000000000000000000000000000000000000000000000003c\
             0000000000000000000000001cd938df5700d97b393dd9ad3bd5ff2dd7fa8e13"
        );
        assert_eq!(
            log.transaction_hash.unwrap().to_string(),
            "0x71b80cef7bb9eb804e747c7784f4f46f7178007bf9f5d73aa6d3eddc60308b2c"
        );
    }

    /// The recorder example's Aave request shape: one address + the 11-topic
    /// topic0 OR-group, full span, 2000-block chunk size.
    #[tokio::test]
    async fn aave_corpus_replays_recorded_log_set_in_fetcher_order() {
        let aave_topic0_group: [&str; 11] = [
            "0x2b627736bca15cd5381dcf80b0bf11fd197d01a037c52b927a881a10fb73ba61",
            "0x804c9b842b2748a22bb64b345453a3de7ca54a6ca45ce00d415894979e22897a",
            "0xa534c8dbe71f871f9f3530e97a74601fea17b426cae02e1c5aee42c96c784051",
            "0x44c58d81365b66dd4b1a7f36c25aa97b8c71c361ee4937adc1a00000227db5dd",
            "0x3115d1449a7b732c986cba18244e897a450f61e1bb8d589cd2e69e6c8924f9f7",
            "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286",
            "0xd728da875fc88944cbf17638bcbe4af0eedaef63becd1d1c57cc097eb4608d84",
            "0x2bccfb3fad376d59d7accf970515eb77b2f27b082c90ed0fb15583dd5a942699",
            "0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0",
            "0x00058a56ea94653cdf4f152d227ace22d4c00ad99e2a43f58cb7d9e3feb295f2",
            "0xbfa21aa5d5f9a1f0120a95e7c0749f389863cbdbfff531aa7339077a5bc919de",
        ];
        let transport = CassetteReplayTransport::new(corpus_cassette(AAVE_CASSETTE_FILE));
        let fetcher = LogFetcher::new(Arc::new(transport.as_alloy_provider()), 2000);
        let logs = fetcher
            .fetch_logs_chunked(
                26_130_440,
                26_130_445,
                Some(vec![
                    "0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2".to_string()
                ]),
                Some(vec![aave_topic0_group
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect::<Vec<_>>()]),
            )
            .await
            .unwrap();
        assert_eq!(logs.len(), 20, "the recorded answer carries 20 logs");
        // Deterministic (block_number, log_index) order — the fetchers' contract.
        let keys: Vec<(u64, u64)> = logs
            .iter()
            .map(|log| (log.block_number.unwrap(), log.log_index.unwrap()))
            .collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted);
        // First/last recorded entries — independent literals from the cassette.
        assert_eq!(keys.first(), Some(&(26_130_441, 349)));
        assert_eq!(keys.last(), Some(&(26_130_445, 547)));
        // Spot-check a log's verbatim fields.
        assert_eq!(logs[0].transaction_index, Some(101));
        assert_eq!(
            logs[0].topics()[1].to_string(),
            "0x000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
        );
    }

    #[tokio::test]
    async fn replay_serves_chain_id_and_head_from_the_ledger() {
        let provider =
            CassetteReplayTransport::new(corpus_cassette(AAVE_CASSETTE_FILE)).as_alloy_provider();
        assert_eq!(provider.get_chain_id().await.unwrap(), 1);
        assert_eq!(provider.get_block_number().await.unwrap(), 26_130_527);
    }

    // ── the negative probe: an unrecorded served-method request is LOUD ──

    #[tokio::test]
    async fn unrecorded_getlogs_is_a_fixture_gap_not_a_revert() {
        let transport = CassetteReplayTransport::new(corpus_cassette(POOL_CASSETTE_FILE));
        let fetcher = LogFetcher::new(Arc::new(transport.as_alloy_provider()), 2000);
        let err = fetcher
            .fetch_logs_chunked(
                26_109_999,
                26_110_000,
                Some(vec![
                    "0x1F98431c8aD98523631AE4a59f267346ea31F984".to_string()
                ]),
                Some(vec![vec![POOL_CREATED_TOPIC0.to_string()]]),
            )
            .await
            .unwrap_err();
        assert!(
            !matches!(err, ProviderError::ExecutionReverted { .. }),
            "a fixture gap must not classify as a revert, got {err:?}"
        );
        match &err {
            ProviderError::RpcError { code, message } => {
                assert_eq!(*code, -32603);
                assert!(message.contains("fixture gap"), "got {message}");
            }
            other => panic!("expected RpcError (fixture gap), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unrecorded_method_keeps_the_method_not_found_contract() {
        let transport = CassetteReplayTransport::new(corpus_cassette(POOL_CASSETTE_FILE));
        let err = transport
            .as_alloy_provider()
            .make_request(
                "eth_getBalance",
                json!(["0x1f98431c8ad98523631ae4a59f267346ea31f984", "0x0"]),
            )
            .await
            .unwrap_err();
        match &err {
            ProviderError::RpcError { code, message } => {
                assert_eq!(*code, -32601);
                assert!(message.contains("method not found"), "got {message}");
            }
            other => panic!("expected method-not-found RPC error, got {other:?}"),
        }
    }

    // ── in-memory ledger entries (the Failure + getBlock + ordering seams) ──

    fn test_provenance() -> CassetteProvenance {
        CassetteProvenance {
            source: "test".to_string(),
            recorded_at: "1970-01-01T00:00:00Z".to_string(),
            span: CassetteSpan {
                from_block: 0,
                to_block: 0,
            },
        }
    }

    fn in_memory_cassette(key: String, response: CassetteResponse) -> Cassette {
        let entry = CassetteEntry {
            digest: entry_digest(&response),
            response,
        };
        Cassette {
            schema: CASSETTE_SCHEMA_V1.to_string(),
            chain_id: 1,
            provenance: test_provenance(),
            entries: BTreeMap::from([(key, entry)]),
        }
    }

    #[tokio::test]
    async fn recorded_failure_entries_replay_as_json_rpc_errors() {
        let params = json!([
            {
                "address": "0x1f98431c8ad98523631ae4a59f267346ea31f984",
                "fromBlock": "0x10",
                "toBlock": "0x14"
            }
        ]);
        let key = entry_key("eth_getLogs", &params);
        let transport = CassetteReplayTransport::new(in_memory_cassette(
            key,
            CassetteResponse::Failure {
                error: json!({ "code": -32001, "message": "query returned no results" }),
            },
        ));
        let err = transport
            .as_alloy_provider()
            .make_request("eth_getLogs", params)
            .await
            .unwrap_err();
        match &err {
            ProviderError::RpcError { code, message } => {
                assert_eq!(*code, -32001, "the recorded code replays verbatim");
                assert!(
                    message.contains("query returned no results"),
                    "the recorded message replays verbatim, got {message}"
                );
            }
            other => panic!("expected the recorded failure verbatim, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn replay_serves_recorded_get_block_by_number_with_wire_quantities() {
        let params = json!(["0x18e4b5e", false]);
        let key = entry_key("eth_getBlockByNumber", &params);
        let recorded_block = json!({
            "number": "26102622",
            "timestamp": "1790919803",
            "hash": "0xa1a5bb3c37fd83e481030db0443928109d1eef21539f3f5fbdfdaa985e980c55",
            "parentHash": "0xcbd773d1ccd86627efe1962bff8486a6212cd27e121af5e9d161015c4c190f0f",
            "transactions": []
        });
        let transport = CassetteReplayTransport::new(in_memory_cassette(
            key,
            CassetteResponse::Success {
                result: recorded_block,
            },
        ));
        let block = transport
            .as_alloy_provider()
            .make_request("eth_getBlockByNumber", params)
            .await
            .unwrap();
        // Quantities re-hexed to the wire form; verbatim hex passes through.
        assert_eq!(block["number"], "0x18e4b5e");
        assert_eq!(block["timestamp"], "0x6abf447b");
        assert_eq!(
            block["hash"],
            "0xa1a5bb3c37fd83e481030db0443928109d1eef21539f3f5fbdfdaa985e980c55"
        );
    }

    // ── wire-form restoration (the re-hex contract) ──────────────────────

    #[test]
    fn wire_value_rehexes_decimal_quantities_and_leaves_hex_verbatim() {
        let canonical: Value = serde_json::from_str(
            r#"{
                "blockNumber": "16",
                "logIndex": "255",
                "zero": "0",
                "address": "0x1f98431c8ad98523631ae4a59f267346ea31f984",
                "data": "0x",
                "blockHash": "0xa1a5bb3c37fd83e481030db0443928109d1eef21539f3f5fbdfdaa985e980c55",
                "removed": false,
                "nested": {
                    "transactionIndex": "1",
                    "topics": [
                        "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118"
                    ]
                }
            }"#,
        )
        .unwrap();
        let wire = wire_value(&canonical);
        // Independent expected literal: quantities re-hexed, verbatim hex and
        // non-string values untouched.
        let expected: Value = serde_json::from_str(
            r#"{
                "blockNumber": "0x10",
                "logIndex": "0xff",
                "zero": "0x0",
                "address": "0x1f98431c8ad98523631ae4a59f267346ea31f984",
                "data": "0x",
                "blockHash": "0xa1a5bb3c37fd83e481030db0443928109d1eef21539f3f5fbdfdaa985e980c55",
                "removed": false,
                "nested": {
                    "transactionIndex": "0x1",
                    "topics": [
                        "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118"
                    ]
                }
            }"#,
        )
        .unwrap();
        assert_eq!(wire, expected);
    }

    #[test]
    fn wire_value_round_trips_through_the_recorder_canonicalization() {
        // canonical → wire → canonical is the identity on the recorder's form.
        let canonical: Value = serde_json::from_str(
            r#"{
                "blockNumber": "26102624",
                "logIndex": "7",
                "address": "0x1f98431c8ad98523631ae4a59f267346ea31f984",
                "topics": ["0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118"],
                "data": "0x",
                "removed": false
            }"#,
        )
        .unwrap();
        assert_eq!(canonicalize_value(&wire_value(&canonical)), canonical);
    }

    fn recorded_log(block: &str, index: &str) -> Value {
        json!({
            "address": "0x1111111111111111111111111111111111111111",
            "topics": [POOL_CREATED_TOPIC0],
            "data": "0x",
            "blockNumber": block,
            "blockHash": "0xa1a5bb3c37fd83e481030db0443928109d1eef21539f3f5fbdfdaa985e980c55",
            "transactionHash": "0x71b80cef7bb9eb804e747c7784f4f46f7178007bf9f5d73aa6d3eddc60308b2c",
            "transactionIndex": "1",
            "logIndex": index,
            "removed": false
        })
    }

    #[tokio::test]
    async fn replayed_getlogs_are_sorted_by_block_then_log_index() {
        let wire_params = json!([
            {
                "address": "0x1111111111111111111111111111111111111111",
                "fromBlock": "0x10",
                "toBlock": "0x10",
                "topics": [POOL_CREATED_TOPIC0]
            }
        ]);
        let key = entry_key("eth_getLogs", &wire_params);
        // The recorded array is deliberately out of order: (16, 7) before
        // (16, 5). The replay surface must hand the fetchers the canonical order.
        let transport = CassetteReplayTransport::new(in_memory_cassette(
            key,
            CassetteResponse::Success {
                result: json!([recorded_log("16", "7"), recorded_log("16", "5")]),
            },
        ));
        let filter = LogFilter::new(
            16,
            16,
            Some(vec![
                "0x1111111111111111111111111111111111111111".to_string()
            ]),
            Some(vec![vec![POOL_CREATED_TOPIC0.to_string()]]),
        )
        .unwrap();
        let logs = transport
            .as_alloy_provider()
            .get_logs(&filter)
            .await
            .unwrap();
        assert_eq!(
            logs.iter().map(|log| log.log_index).collect::<Vec<_>>(),
            vec![Some(5), Some(7)],
            "replayed getLogs serve in (block_number, log_index) order"
        );
    }
}
