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
//!   the canonical form and pass through unchanged — non-minimal short hex
//!   (a leading-zero digit) included, so the wire form comes back
//!   byte-exactly for both canonicalization classes.
//! - `Failure` entries replay as JSON-RPC errors with their recorded
//!   `code`/`message`/`data`, so a recorded failure classifies at the
//!   provider layer exactly as it did live: a revert-classified message stays
//!   a revert, everything else is an RPC error.
//!
//! A served-method request missing from the ledger is a **fixture gap** — a
//! non-revert JSON-RPC error (the [`crate::offline`] unrecorded-call
//! contract): a missing fixture entry is a harness problem, not on-chain
//! semantics. The served surface includes the four read methods
//! (`eth_call`, `eth_getCode`, `eth_getStorageAt`, `eth_getBalance`), because
//! an exact read is served from this ledger too: a pin the capture does not
//! carry is a missing fixture, never a node without the capability. Methods
//! outside the served surface keep the offline "method not found" contract;
//! anything the ledger carries replays even outside the served set. The surface
//! is the read methods the account and storage lanes issue —
//! `eth_call`, `eth_getCode`, `eth_getStorageAt`, `eth_getBalance`, and
//! `eth_getTransactionCount` — because an account triple is ONE batch whose
//! three members must answer or the whole hydration is a fixture problem.
//!
//! # Hash-pinned reads resolve through the recorded headers
//!
//! A capture recorded before a hash-pinned reader existed carries the same read
//! at its HEIGHT, and the exact key therefore misses. The ledger's own recorded
//! `eth_getBlockByNumber` answers are the header evidence that bridges the two:
//! a read whose identifier is `{ blockHash, requireCanonical: true }` resolves
//! that hash to the height a recorded header names for it, and the height-keyed
//! entry answers — the SAME read position, proven by the recorded header rather
//! than assumed from the height alone.
//!
//! The bridge refuses rather than guesses. A hash no recorded header names is a
//! fixture gap (missing evidence, never a capability verdict); a pin that does
//! not require canonicality names a non-canonical branch, which height evidence
//! cannot identify; and recorded headers that contradict each other — one hash
//! at two heights, or one height holding two hashes — are a refusal, because a
//! replaced block's read can never be answered from the replacement's. Only the
//! five read methods take the bridge — the account triple's three members
//! included, so a pinned hydration resolves as one batch — and every other
//! method keeps its own key, so a filter or a header request is never rewritten.
//!
//! Wrap as a real [`AlloyProvider`] via
//! [`CassetteReplayTransport::as_alloy_provider`] and run `LogFetcher` (or
//! any updater fetch surface) unchanged over the committed corpus in
//! `tests/fixtures/cassettes/`.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use alloy::primitives::B256;
use alloy::providers::{Provider, RootProvider};
use alloy::rpc::client::RpcClient;
use alloy::rpc::json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{TransportError, TransportFut};
use serde_json::value::to_raw_value;
use serde_json::Value;

use crate::cassette::{
    canonicalize_value, entry_key, sort_json_array, Cassette, CassetteEntry, CassetteResponse,
};
use crate::provider::AlloyProvider;

/// The methods the replay surface serves from the ledger. A miss on one of
/// these is a fixture gap; a miss anywhere else is "method not found" (the
/// ledger is authoritative for anything it actually carries).
///
/// The five read methods are here because a hash-pinned exact read is served
/// from this ledger — the account triple's three members included, since a
/// hydration whose nonce answered "method not found" would be a half-served
/// read: a capture that does not carry the pin is a MISSING FIXTURE, and
/// reporting it as `-32601` would let the exact-read classifier read a harness
/// gap as a node without the capability (ADR-105 D11 keeps "no evidence
/// recorded" and "no capability" apart).
const SERVED_METHODS: [&str; 9] = [
    "eth_getLogs",
    "eth_getBlockByNumber",
    "eth_chainId",
    "eth_blockNumber",
    "eth_call",
    "eth_getCode",
    "eth_getStorageAt",
    "eth_getBalance",
    "eth_getTransactionCount",
];

/// The methods whose hash-pinned request the recorded header evidence can
/// resolve to a height-keyed entry. Every other method keeps its own key.
const PINNED_READ_METHODS: [&str; 5] = [
    "eth_call",
    "eth_getCode",
    "eth_getStorageAt",
    "eth_getBalance",
    "eth_getTransactionCount",
];

/// The method whose recorded answers ARE the header evidence a pin resolves
/// through.
const HEADER_METHOD: &str = "eth_getBlockByNumber";

/// The replay half of a golden capture: answers JSON-RPC requests from a
/// committed [`Cassette`]'s ledger — the sibling of
/// [`crate::offline::OfflineTransport`] over the wire-form capture instead of
/// per-block calls/code maps.
#[derive(Clone, Debug)]
pub struct CassetteReplayTransport {
    cassette: Arc<Cassette>,
    served: Arc<ServedTrace>,
    headers: Arc<OnceLock<Arc<HeaderIndex>>>,
}

impl CassetteReplayTransport {
    /// Build a replay transport over a committed cassette.
    #[must_use]
    pub fn new(cassette: Cassette) -> Self {
        Self {
            cassette: Arc::new(cassette),
            served: Arc::new(ServedTrace::default()),
            headers: Arc::new(OnceLock::new()),
        }
    }

    /// The header evidence this ledger carries, indexed once on first use.
    fn header_index(&self) -> &HeaderIndex {
        self.headers
            .get_or_init(|| Arc::new(HeaderIndex::of(&self.cassette)))
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
        let (entry, resolved) = match self.lookup(method, &params, &key) {
            Some(entry) => (entry, false),
            None => match self.lookup_by_header_hash(method, &params) {
                Ok(Some(entry)) => (entry, true),
                Ok(None) => {
                    return Err(if SERVED_METHODS.contains(&method) {
                        fixture_gap(&key)
                    } else {
                        method_not_found(method)
                    });
                }
                Err(payload) => return Err(payload),
            },
        };
        if resolved {
            self.served.record_hash_resolved(method);
        }
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

    /// The height-keyed entry a hash-pinned read resolves to through the
    /// ledger's own recorded headers (see the module docs): the pin must require
    /// canonicality, the hash must be one a recorded header names, and the
    /// recorded evidence must not contradict itself about either half of the
    /// identity. `Ok(None)` is "this request is not a pin this bridge serves".
    fn lookup_by_header_hash(
        &self,
        method: &str,
        params: &Value,
    ) -> Result<Option<&CassetteEntry>, ErrorPayload> {
        if !PINNED_READ_METHODS.contains(&method) {
            return Ok(None);
        }
        let Some((position, hash)) = pinned_identifier(params) else {
            return Ok(None);
        };
        if !params[position]
            .get("requireCanonical")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(contradictory_identity(&format!(
                "{method} names {hash} without requiring canonicality; a height-keyed entry cannot stand in for a non-canonical branch"
            )));
        }
        let headers = self.header_index();
        let Some(number) = headers.number_of(hash) else {
            return Err(missing_header_evidence(method, hash));
        };
        if headers.contradicts_hash(hash) || headers.contradicts_height(number) {
            return Err(contradictory_identity(&format!(
                "the recorded headers disagree about block {number} / {hash}"
            )));
        }
        let mut height_request = params.clone();
        height_request[position] = Value::String(format!("0x{number:x}"));
        Ok(self
            .cassette
            .entries
            .get(&entry_key(method, &height_request)))
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

/// The recorded header evidence a hash-pinned read resolves through: every
/// exact `(number, hash)` pair the ledger's `eth_getBlockByNumber` answers
/// name, plus the pairs that CONTRADICT (one hash at two heights, or one height
/// holding two hashes) so a contradictory ledger refuses instead of picking a
/// branch.
#[derive(Debug, Default)]
struct HeaderIndex {
    numbers: HashMap<B256, u64>,
    hashes: HashMap<u64, B256>,
    conflicting_hashes: HashSet<B256>,
    conflicting_heights: HashSet<u64>,
}

impl HeaderIndex {
    /// Index every recorded header the ledger carries.
    fn of(cassette: &Cassette) -> Self {
        let mut index = Self::default();
        for (key, entry) in &cassette.entries {
            let Ok(request) = serde_json::from_str::<Value>(key) else {
                continue;
            };
            if request.get(0).and_then(Value::as_str) != Some(HEADER_METHOD) {
                continue;
            }
            if let Some((number, hash)) = recorded_header(entry) {
                index.record(number, hash);
            }
        }
        index
    }

    fn record(&mut self, number: u64, hash: B256) {
        if let Some(previous) = self.numbers.insert(hash, number) {
            if previous != number {
                self.conflicting_hashes.insert(hash);
                self.conflicting_heights.insert(previous);
                self.conflicting_heights.insert(number);
            }
        }
        if let Some(previous) = self.hashes.insert(number, hash) {
            if previous != hash {
                self.conflicting_heights.insert(number);
                self.conflicting_hashes.insert(previous);
                self.conflicting_hashes.insert(hash);
            }
        }
    }

    fn number_of(&self, hash: B256) -> Option<u64> {
        self.numbers.get(&hash).copied()
    }

    fn contradicts_hash(&self, hash: B256) -> bool {
        self.conflicting_hashes.contains(&hash)
    }

    fn contradicts_height(&self, number: u64) -> bool {
        self.conflicting_heights.contains(&number)
    }
}

/// One recorded header's identity, when the ledger entry is a served header.
fn recorded_header(entry: &CassetteEntry) -> Option<(u64, B256)> {
    let CassetteResponse::Success { result } = &entry.response else {
        return None;
    };
    let number = recorded_quantity(result.get("number")?)?;
    let hash = result.get("hash")?.as_str()?.parse().ok()?;
    Some((number, hash))
}

/// A recorded quantity: the canonical form stores u64 hex quantities as
/// decimal strings, and a hand-written fixture may carry either form.
fn recorded_quantity(value: &Value) -> Option<u64> {
    match value {
        Value::String(text) => text
            .parse()
            .ok()
            .or_else(|| u64::from_str_radix(text.trim_start_matches("0x"), 16).ok()),
        Value::Number(number) => number.as_u64(),
        _ => None,
    }
}

/// The EIP-1898 identifier a request states, and where in its params it sits.
fn pinned_identifier(params: &Value) -> Option<(usize, B256)> {
    params
        .as_array()?
        .iter()
        .enumerate()
        .rev()
        .find_map(|(position, param): (usize, &Value)| {
            let hash = param.get("blockHash")?.as_str()?.parse().ok()?;
            Some((position, hash))
        })
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
    /// Of [`Self::served`], how many were hash-pinned reads resolved to their
    /// height-keyed entry through the recorded header evidence.
    pub hash_resolved: u64,
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
    /// Hash-pinned reads resolved through the recorded header evidence.
    hash_resolved: u64,
    per_method: BTreeMap<String, MethodServed>,
}

impl ServedTrace {
    fn record_request(&self, method: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.requests = inner.requests.saturating_add(1);
            inner.per_method.entry(method.to_string()).or_default();
        }
    }

    fn record_hash_resolved(&self, method: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.hash_resolved = inner.hash_resolved.saturating_add(1);
            let entry = inner.per_method.entry(method.to_string()).or_default();
            entry.hash_resolved = entry.hash_resolved.saturating_add(1);
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
            hash_resolved: inner.hash_resolved,
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
    /// Of [`Self::served`], how many were hash-pinned reads answered from the
    /// height-keyed entry their recorded header names — the fixture-migration
    /// signal, so a replay corpus records how much of its read surface still
    /// needs re-recording at the exact hash.
    pub hash_resolved: u64,
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
/// topics, and data hex pass through verbatim — non-minimal short hex
/// included, so both canonicalization classes restore byte-exactly.
///
/// `pub(crate)` so the `cassette` corpus guard can prove the committed
/// corpus is a byte-exact fixed point of this restoration.
pub(crate) fn wire_value(value: &Value) -> Value {
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
///
/// `pub(crate)` so the `cassette` corpus guard can demonstrate the pre-rule
/// lossy restore on a non-minimal token (its red-path counterfactual).
pub(crate) fn wire_hex_quantity(s: &str) -> Option<String> {
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

/// A hash-pinned read the ledger has no header evidence for: missing evidence,
/// never a missing capability (ADR-105 D11 keeps the two apart).
fn missing_header_evidence(method: &str, hash: B256) -> ErrorPayload {
    internal_err(format!(
        "cassette: fixture gap: {method} pins block {hash}, and no recorded header names that          block — the pinned read has no height-keyed evidence"
    ))
}

/// Recorded evidence that disagrees with itself about a block's identity: a
/// refusal, never a branch picked for the caller.
fn contradictory_identity(detail: &str) -> ErrorPayload {
    internal_err(format!(
        "cassette: contradictory recorded header identity: {detail}"
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
    use alloy::primitives::{Address, Bytes, B256, U256};
    use degenbot_core::canonical_head::ChainAnchor;
    use degenbot_core::errors::ProviderError;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::str::FromStr;

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
    async fn unrecorded_read_method_is_a_fixture_gap_not_a_capability_verdict() {
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
                assert_eq!(*code, -32603, "a missing fixture is not a missing method");
                assert!(message.contains("fixture gap"), "got {message}");
            }
            other => panic!("expected a fixture-gap RPC error, got {other:?}"),
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

    // ── recorded header + exact-read evidence (the acquisition interface) ─
    //
    // Live and recorded operation share ONE acquisition surface: the header
    // chain and hash-pinned read views take an AlloyProvider, so a cassette
    // transport answers them exactly as a node does. These cases are the
    // recorded half of that seam.

    fn success_entry(result: Value) -> CassetteEntry {
        let response = CassetteResponse::Success { result };
        CassetteEntry {
            digest: entry_digest(&response),
            response,
        }
    }

    fn failure_entry(code: i64, message: &str) -> CassetteEntry {
        let response = CassetteResponse::Failure {
            error: json!({ "code": code, "message": message }),
        };
        CassetteEntry {
            digest: entry_digest(&response),
            response,
        }
    }

    fn cassette_over(span: (u64, u64), entries: Vec<(String, CassetteEntry)>) -> Cassette {
        Cassette {
            schema: CASSETTE_SCHEMA_V1.to_string(),
            chain_id: 1,
            provenance: CassetteProvenance {
                source: "recorded-node/1.0".to_string(),
                recorded_at: "1970-01-01T00:00:00Z".to_string(),
                span: CassetteSpan {
                    from_block: span.0,
                    to_block: span.1,
                },
            },
            entries: entries.into_iter().collect(),
        }
    }

    /// The recorded chain-id read: alloy sends NO params, so the canonical
    /// ledger key carries a JSON null in the params position.
    fn chain_entry() -> (String, CassetteEntry) {
        (
            entry_key("eth_chainId", &Value::Null),
            success_entry(json!("0x1")),
        )
    }

    /// A recorded header in the cassette's CANONICAL form: the recorder
    /// decimalizes the wire quantities, so a height is a decimal string here.
    fn header_record(number: u64, hash_tag: u8, parent_tag: u8) -> Value {
        json!({
            "number": format!("{number}"),
            "hash": format!("{}", B256::new([hash_tag; 32])),
            "parentHash": format!("{}", B256::new([parent_tag; 32])),
        })
    }

    fn header_entry(number: u64, hash_tag: u8, parent_tag: u8) -> (String, CassetteEntry) {
        (
            entry_key(
                "eth_getBlockByNumber",
                &json!([format!("0x{number:x}"), false]),
            ),
            success_entry(header_record(number, hash_tag, parent_tag)),
        )
    }

    fn header_cassette(span: (u64, u64), headers: &[(u64, u8, u8)]) -> Cassette {
        let mut entries = vec![chain_entry()];
        for (number, hash_tag, parent_tag) in headers {
            entries.push(header_entry(*number, *hash_tag, *parent_tag));
        }
        cassette_over(span, entries)
    }

    #[tokio::test]
    async fn a_recorded_header_chain_replays_offline_through_the_acquisition_interface() {
        let transport = CassetteReplayTransport::new(header_cassette(
            (100, 102),
            &[(100, 0xa1, 0xa0), (101, 0xa2, 0xa1), (102, 0xa3, 0xa2)],
        ));
        let provider = transport.as_alloy_provider();

        let chain = crate::chain_evidence::acquire_header_chain(&provider, 100, 102)
            .await
            .expect("the ledger answers every header of the range");

        assert_eq!(chain.len(), 3);
        assert_eq!(chain.from_block(), 100);
        assert_eq!(chain.terminal().number(), 102);
        assert_eq!(chain.terminal().hash(), B256::new([0xa3; 32]));
        assert_eq!(chain.terminal().parent_hash(), B256::new([0xa2; 32]));
        let served = transport.served_snapshot();
        assert_eq!(
            served.requests, served.served,
            "every header read is a recorded entry — a fixture gap is loud"
        );
        assert_eq!(
            served.served, 6,
            "one chain read + one header read per block: an UNBOUND provider re-reads its chain id \
             per lineage read, and a bound one pays none"
        );
    }

    #[tokio::test]
    async fn an_empty_log_range_replays_over_its_complete_header_evidence() {
        let empty_logs = (
            entry_key(
                "eth_getLogs",
                &json!([{ "fromBlock": "0x64", "toBlock": "0x66", "topics": [] }]),
            ),
            success_entry(json!([])),
        );
        let mut cassette = header_cassette(
            (100, 102),
            &[(100, 0xa1, 0xa0), (101, 0xa2, 0xa1), (102, 0xa3, 0xa2)],
        );
        cassette.entries.insert(empty_logs.0, empty_logs.1);
        let transport = CassetteReplayTransport::new(cassette);
        let provider = transport.as_alloy_provider();
        let fetcher = LogFetcher::new(Arc::new(provider.clone()), 1_000);
        let filter = LogFilter::new(100, 102, None, None).expect("a valid fixture filter");

        let unit = crate::chain_evidence::ProjectionRange::acquire(&provider, &fetcher, &filter)
            .await
            .expect("a replayed range with no matching logs is still a complete unit");

        assert!(unit.logs().is_empty());
        assert_eq!(unit.span(), 100..=102);
        assert_eq!(unit.headers().len(), 3, "every block of the range is named");
        assert_eq!(
            transport.served_snapshot().served,
            7,
            "one header per block (each with its chain read) and one logs answer"
        );
    }

    #[tokio::test]
    async fn a_recorded_header_that_names_no_parent_refuses_the_replayed_range() {
        let mut cassette = header_cassette((100, 102), &[(100, 0xa1, 0xa0)]);
        cassette.entries.insert(
            entry_key("eth_getBlockByNumber", &json!(["0x65", false])),
            success_entry(json!({
                "number": "101",
                "hash": format!("{}", B256::new([0xa2; 32])),
            })),
        );
        let provider = CassetteReplayTransport::new(cassette).as_alloy_provider();

        let refusal = crate::chain_evidence::acquire_header_chain(&provider, 100, 101)
            .await
            .expect_err("a header that names no parent names no lineage");

        assert!(
            matches!(
                refusal,
                crate::chain_evidence::ChainEvidenceError::Header(
                    crate::chain_evidence::HeaderRefusal::MissingHeader { number: 101 }
                )
            ),
            "got {refusal:?}"
        );
    }

    #[tokio::test]
    async fn a_recorded_broken_parent_link_refuses_the_replayed_range() {
        // The cassette is canonical and its digests are intact: the recorded
        // evidence itself disagrees, which is what acquisition must refuse.
        let provider = CassetteReplayTransport::new(header_cassette(
            (100, 101),
            &[(100, 0xa1, 0xa0), (101, 0xa2, 0xb0)],
        ))
        .as_alloy_provider();

        let refusal = crate::chain_evidence::acquire_header_chain(&provider, 100, 101)
            .await
            .expect_err("a child naming another parent is a different branch");

        assert!(
            matches!(
                refusal,
                crate::chain_evidence::ChainEvidenceError::Header(
                    crate::chain_evidence::HeaderRefusal::BrokenParentLink { number: 101, .. }
                )
            ),
            "got {refusal:?}"
        );
    }

    #[tokio::test]
    async fn a_recorded_exact_read_replays_at_its_pinned_block_hash() {
        let pinned = ChainAnchor::new(1, 100, B256::new([0xa1; 32]), B256::new([0xa0; 32]))
            .expect("a complete fixture anchor");
        let to = Address::repeat_byte(0x11);
        let data = Bytes::from(vec![0xde, 0xad]);
        let params = json!([
            { "to": format!("{to}"), "data": format!("{data}") },
            {
                "blockHash": format!("{}", pinned.hash()),
                "requireCanonical": true
            }
        ]);
        let key = entry_key("eth_call", &params);
        assert!(
            key.contains("blockHash") && key.contains("requireCanonical"),
            "the recorded request shape is the hash-pinned identifier: {key}"
        );
        let transport = CassetteReplayTransport::new(cassette_over(
            (100, 100),
            vec![(key, success_entry(json!("0x1234")))],
        ));
        let provider = transport.as_alloy_provider();

        let answer = crate::chain_evidence::BlockReads::at(&provider, pinned)
            .call(to, data)
            .await
            .expect("the ledger answers the hash-pinned read");

        assert_eq!(answer, Bytes::from(vec![0x12, 0x34]));
        let served = transport.served_snapshot();
        assert_eq!(served.served, 1, "one exact read, served from the ledger");
        assert_eq!(
            served.requests, 1,
            "an exact read is never retried as a number-tagged read"
        );
    }

    #[tokio::test]
    async fn a_recorded_unsupported_exact_read_is_a_typed_capability_failure() {
        let pinned = ChainAnchor::new(1, 100, B256::new([0xa1; 32]), B256::new([0xa0; 32]))
            .expect("a complete fixture anchor");
        let to = Address::repeat_byte(0x11);
        let params = json!([
            { "to": format!("{to}"), "data": "0x" },
            {
                "blockHash": format!("{}", pinned.hash()),
                "requireCanonical": true
            }
        ]);
        let key = entry_key("eth_call", &params);
        let transport = CassetteReplayTransport::new(cassette_over(
            (100, 100),
            vec![(
                key,
                failure_entry(
                    -32_601,
                    "the method eth_call does not support a block hash identifier",
                ),
            )],
        ));
        let provider = transport.as_alloy_provider();

        let error = crate::chain_evidence::BlockReads::at(&provider, pinned)
            .call(to, Bytes::new())
            .await
            .expect_err("a recorded missing capability cannot serve an exact read");

        assert!(
            matches!(
                error,
                crate::chain_evidence::ExactReadError::Unsupported {
                    method: "eth_call",
                    ..
                }
            ),
            "a recorded capability refusal is a typed node-evidence failure: {error:?}"
        );
        assert_eq!(
            transport.served_snapshot().requests,
            1,
            "the capability verdict never degrades to a number-only read"
        );
    }

    #[tokio::test]
    async fn a_recorded_exact_batch_replays_offline_from_the_ledger() {
        use crate::multicall3::{encode_aggregate3, MULTICALL3_ADDRESS};
        use alloy::dyn_abi::DynSolValue;

        let word = |value: u8| {
            let mut out = [0u8; 32];
            out[31] = value;
            out
        };
        let rows = |entries: &[(bool, &[u8])]| {
            let values: Vec<DynSolValue> = entries
                .iter()
                .map(|(success, data)| {
                    DynSolValue::Tuple(vec![
                        DynSolValue::Bool(*success),
                        DynSolValue::Bytes(data.to_vec()),
                    ])
                })
                .collect();
            json!(format!(
                "0x{}",
                alloy::hex::encode(DynSolValue::Array(values).abi_encode())
            ))
        };

        let pinned = ChainAnchor::new(1, 100, B256::new([0xa1; 32]), B256::new([0xa0; 32]))
            .expect("a complete fixture anchor");
        let target = Address::repeat_byte(0x11);
        let calls = vec![
            (target, Bytes::from(vec![0xaa])),
            (target, Bytes::from(vec![0xbb])),
        ];
        let calldata = encode_aggregate3(&calls).expect("the aggregate3 envelope encodes");
        let params = json!([
            { "to": format!("0x{}", alloy::hex::encode(MULTICALL3_ADDRESS.as_slice())), "data": format!("{calldata}") },
            { "blockHash": format!("{}", pinned.hash()), "requireCanonical": true }
        ]);
        let key = entry_key("eth_call", &params);
        assert!(
            key.contains("blockHash"),
            "the recorded batch names its block by hash: {key}"
        );
        let transport = CassetteReplayTransport::new(cassette_over(
            (100, 100),
            vec![(
                key,
                success_entry(rows(&[(true, &word(42)), (true, &word(7))])),
            )],
        ));
        let provider = transport.as_alloy_provider();

        let outcomes = crate::chain_evidence::BlockReads::at(&provider, pinned)
            .batch(&calls)
            .await
            .expect("the ledger answers the pinned batch offline");

        assert_eq!(
            outcomes,
            vec![
                crate::multicall3::MulticallOutcome::Returned(Bytes::from(word(42).to_vec())),
                crate::multicall3::MulticallOutcome::Returned(Bytes::from(word(7).to_vec())),
            ]
        );
        let served = transport.served_snapshot();
        assert_eq!(
            served.requests, 1,
            "one aggregate3 request serves the batch"
        );
        assert_eq!(served.served, 1);
    }

    #[tokio::test]
    async fn a_mutated_pinned_hash_is_a_loud_replay_gap() {
        let recorded = ChainAnchor::new(1, 100, B256::new([0xa1; 32]), B256::new([0xa0; 32]))
            .expect("a complete fixture anchor");
        let abandoned = ChainAnchor::new(1, 100, B256::new([0xb1; 32]), B256::new([0xa0; 32]))
            .expect("a complete fixture anchor");
        let to = Address::repeat_byte(0x11);
        let params = json!([
            { "to": format!("{to}"), "data": "0x" },
            { "blockHash": format!("{}", recorded.hash()), "requireCanonical": true }
        ]);
        let key = entry_key("eth_call", &params);
        let transport = CassetteReplayTransport::new(cassette_over(
            (100, 100),
            vec![(key, success_entry(json!("0x1234")))],
        ));
        let provider = transport.as_alloy_provider();

        let error = crate::chain_evidence::BlockReads::at(&provider, abandoned)
            .call(to, Bytes::new())
            .await
            .expect_err("the ledger cannot serve a hash it never recorded");

        assert!(
            matches!(error, crate::chain_evidence::ExactReadError::Failed { .. }),
            "a mutated pin is a loud failed read, never a served or substituted one: {error:?}"
        );
        let served = transport.served_snapshot();
        assert_eq!(served.served, 0, "nothing was served");
        assert_eq!(served.requests, 1, "no number-only retry exists");
    }

    // ── hash-pinned reads over recorded header evidence ──────────────────

    /// The slot a resolved replay serves.
    const RESOLVED_SLOT: U256 = U256::from_limbs([7, 0, 0, 0]);
    /// The word recorded at the height-keyed entry.
    const RECORDED_WORD: &str =
        "0x000000000000000000000000000000000000000000000000000000000000002a";

    /// A header recorded at `number` naming `hash` (the recorder's canonical
    /// form: the height is a decimal string).
    fn recorded_header_entry(number: u64, hash: B256, full: bool) -> (String, CassetteEntry) {
        (
            entry_key(
                "eth_getBlockByNumber",
                &json!([format!("0x{number:x}"), full]),
            ),
            success_entry(json!({
                "number": format!("{number}"),
                "hash": format!("{hash}"),
            })),
        )
    }

    /// The height-keyed storage read a pre-exact-reader capture carries.
    fn height_keyed_storage_entry(
        number: u64,
        address: Address,
        word: &str,
    ) -> (String, CassetteEntry) {
        (
            entry_key(
                "eth_getStorageAt",
                &json!([
                    format!("{address}"),
                    format!("{RESOLVED_SLOT}"),
                    format!("0x{number:x}")
                ]),
            ),
            success_entry(json!(word)),
        )
    }

    /// The one read position these cases share.
    const PINNED_NUMBER: u64 = 100;
    const PINNED_HASH: B256 = B256::new([0xa1; 32]);

    fn pinned_params(address: Address, hash: B256, canonical: bool) -> Value {
        let mut pin = json!({ "blockHash": format!("{hash}") });
        if canonical {
            pin["requireCanonical"] = json!(true);
        }
        json!([format!("{address}"), format!("{RESOLVED_SLOT}"), pin])
    }

    /// The recorded-header bridge: a read pinned at a hash the ledger's own
    /// header names resolves to that header's height, and the height-keyed entry
    /// answers — the same read position, proven by the recorded evidence. The
    /// request is answered ONCE (never retried as a height request) and the
    /// serving stats name the resolution.
    #[tokio::test]
    async fn a_hash_pinned_read_resolves_through_its_recorded_header() {
        let address = Address::repeat_byte(0x11);
        let transport = CassetteReplayTransport::new(cassette_over(
            (PINNED_NUMBER, PINNED_NUMBER),
            vec![
                chain_entry(),
                recorded_header_entry(PINNED_NUMBER, PINNED_HASH, false),
                height_keyed_storage_entry(PINNED_NUMBER, address, RECORDED_WORD),
            ],
        ));

        let answer = transport
            .as_alloy_provider()
            .make_request(
                "eth_getStorageAt",
                pinned_params(address, PINNED_HASH, true),
            )
            .await
            .expect("the recorded header resolves the pin");

        assert_eq!(answer, json!(RECORDED_WORD));
        let served = transport.served_snapshot();
        assert_eq!(served.served, 1, "one entry served the read");
        assert_eq!(
            served.hash_resolved, 1,
            "the serving stats name the header resolution"
        );
        assert_eq!(
            served.per_method["eth_getStorageAt"].hash_resolved, 1,
            "the per-method split names it too"
        );
        assert_eq!(
            served.requests, 1,
            "the pin is answered once, never re-issued as a height read"
        );
    }

    /// The same bridge through the typed client: `BlockId::Hash` with
    /// `requireCanonical` is the wire shape the production exact read issues, so
    /// the resolution is proven on the request construction the engine uses, not
    /// only on a hand-written param array.
    #[tokio::test]
    async fn the_typed_canonical_pin_resolves_through_the_recorded_header() {
        let address = Address::repeat_byte(0x11);
        let transport = CassetteReplayTransport::new(cassette_over(
            (PINNED_NUMBER, PINNED_NUMBER),
            vec![
                chain_entry(),
                recorded_header_entry(PINNED_NUMBER, PINNED_HASH, false),
                height_keyed_storage_entry(PINNED_NUMBER, address, RECORDED_WORD),
            ],
        ));
        let provider = transport.as_alloy_provider();

        let word = provider
            .get_storage_at(
                &address,
                RESOLVED_SLOT,
                Some(alloy::eips::BlockId::Hash(
                    alloy::eips::RpcBlockHash::from_hash(PINNED_HASH, Some(true)),
                )),
            )
            .await
            .expect("the typed canonical pin resolves");

        assert_eq!(word, B256::from_str(RECORDED_WORD).expect("a word"));
        assert_eq!(transport.served_snapshot().hash_resolved, 1);
    }

    /// The account triple resolves as ONE batch through the same bridge: a
    /// pinned `eth_getBalance` / `eth_getTransactionCount` / `eth_getCode`
    /// group answers every member at the recorded height, so a pinned account
    /// hydration is served exactly as a height-keyed one would be. All three
    /// members resolve — the nonce included, which is why it is a served and
    /// bridged method rather than a fixture gap in the middle of a hydration.
    #[tokio::test]
    async fn an_account_triple_replays_at_its_pinned_hash() {
        let address = Address::repeat_byte(0x11);
        let height = json!([format!("{address}"), format!("0x{PINNED_NUMBER:x}")]);
        let transport = CassetteReplayTransport::new(cassette_over(
            (PINNED_NUMBER, PINNED_NUMBER),
            vec![
                chain_entry(),
                recorded_header_entry(PINNED_NUMBER, PINNED_HASH, false),
                (
                    entry_key("eth_getBalance", &height),
                    success_entry(json!("1000000000000000000")),
                ),
                (
                    entry_key("eth_getTransactionCount", &height),
                    success_entry(json!("42")),
                ),
                (
                    entry_key("eth_getCode", &height),
                    success_entry(json!("0x60016001")),
                ),
            ],
        ));
        let provider = transport.as_alloy_provider();

        let row = provider
            .get_account_at(
                &address,
                alloy::eips::BlockId::Hash(alloy::eips::RpcBlockHash::from_hash(
                    PINNED_HASH,
                    Some(true),
                )),
            )
            .await
            .expect("the pinned account triple resolves through the recorded header");

        assert_eq!(row.balance, U256::from(1_000_000_000_000_000_000u64));
        assert_eq!(row.nonce, 42);
        assert_eq!(row.code, Bytes::from_static(&[0x60, 0x01, 0x60, 0x01]));
        let served = transport.served_snapshot();
        assert_eq!(served.served, 3, "one answer per batch member");
        assert_eq!(
            served.hash_resolved, 3,
            "the whole triple resolved, the nonce included"
        );
        assert_eq!(
            served.per_method["eth_getTransactionCount"].hash_resolved, 1,
            "the nonce member is a first-class bridged read"
        );
    }

    /// A pin at a hash no recorded header names is a FIXTURE GAP: missing
    /// evidence, never a missing capability and never a served height.
    #[tokio::test]
    async fn a_pin_at_an_unrecorded_hash_is_a_fixture_gap() {
        let address = Address::repeat_byte(0x11);
        let transport = CassetteReplayTransport::new(cassette_over(
            (PINNED_NUMBER, PINNED_NUMBER),
            vec![
                chain_entry(),
                recorded_header_entry(PINNED_NUMBER, PINNED_HASH, false),
                height_keyed_storage_entry(PINNED_NUMBER, address, RECORDED_WORD),
            ],
        ));
        let abandoned = B256::new([0xb1; 32]);

        let error = transport
            .as_alloy_provider()
            .make_request("eth_getStorageAt", pinned_params(address, abandoned, true))
            .await
            .expect_err("nothing recorded names this block");

        match &error {
            ProviderError::RpcError { code, message } => {
                assert_eq!(*code, -32_603, "a fixture gap, not a capability verdict");
                assert!(message.contains("fixture gap"), "got {message}");
                assert!(
                    message.contains(&format!("{abandoned}")),
                    "the gap names the pinned block, got {message}"
                );
            }
            other => panic!("expected a fixture-gap RPC error, got {other:?}"),
        }
        let served = transport.served_snapshot();
        assert_eq!(served.served, 0, "nothing was served");
        assert_eq!(served.hash_resolved, 0);
        assert_eq!(served.requests, 1, "no height-only retry exists");
    }

    /// The red-first same-height replacement: a ledger whose recorded headers
    /// disagree — one height holding the abandoned block's hash in one answer and
    /// the replacement's in another — refuses. The height-keyed read is never
    /// served on the strength of a height whose identity the evidence cannot
    /// settle.
    #[tokio::test]
    async fn contradictory_recorded_headers_refuse_a_same_height_replacement() {
        let address = Address::repeat_byte(0x11);
        let replacement = B256::new([0xb1; 32]);
        let transport = CassetteReplayTransport::new(cassette_over(
            (PINNED_NUMBER, PINNED_NUMBER),
            vec![
                chain_entry(),
                recorded_header_entry(PINNED_NUMBER, PINNED_HASH, false),
                recorded_header_entry(PINNED_NUMBER, replacement, true),
                height_keyed_storage_entry(PINNED_NUMBER, address, RECORDED_WORD),
            ],
        ));

        for pin in [PINNED_HASH, replacement] {
            let error = transport
                .as_alloy_provider()
                .make_request("eth_getStorageAt", pinned_params(address, pin, true))
                .await
                .expect_err("a contradicted identity cannot serve a height's word");
            match &error {
                ProviderError::RpcError { code, message } => {
                    assert_eq!(*code, -32_603);
                    assert!(
                        message.contains("contradictory recorded header identity"),
                        "the refusal names the contradiction, got {message}"
                    );
                }
                other => panic!("expected a contradiction refusal, got {other:?}"),
            }
        }
        assert_eq!(
            transport.served_snapshot().served,
            0,
            "no branch is picked for the caller"
        );
    }

    /// A pin that does not require canonicality names a NON-canonical branch,
    /// which height evidence cannot identify: the bridge refuses instead of
    /// serving the canonical height's entry.
    #[tokio::test]
    async fn a_non_canonical_pin_refuses_the_height_bridge() {
        let address = Address::repeat_byte(0x11);
        let transport = CassetteReplayTransport::new(cassette_over(
            (PINNED_NUMBER, PINNED_NUMBER),
            vec![
                chain_entry(),
                recorded_header_entry(PINNED_NUMBER, PINNED_HASH, false),
                height_keyed_storage_entry(PINNED_NUMBER, address, RECORDED_WORD),
            ],
        ));

        let error = transport
            .as_alloy_provider()
            .make_request(
                "eth_getStorageAt",
                pinned_params(address, PINNED_HASH, false),
            )
            .await
            .expect_err("a non-canonical pin names no height-provable position");

        match &error {
            ProviderError::RpcError { code, message } => {
                assert_eq!(*code, -32_603);
                assert!(
                    message.contains("non-canonical"),
                    "the refusal says why, got {message}"
                );
            }
            other => panic!("expected a non-canonical refusal, got {other:?}"),
        }
        assert_eq!(transport.served_snapshot().served, 0);
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
