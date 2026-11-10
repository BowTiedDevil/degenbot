//! The scripted chain the dispatch-lane suites and the dispatch-scaling probe
//! drive: one canonical head table, scripted storage words (zero unless a run
//! names an identity), a configurable raw BAL payload, a deterministic RPC
//! latency model, and a wire census of every dispatched request.
//!
//! # What is scripted, and what is not
//!
//! The chain answers the few methods a dispatch fan-out issues — the anchor's
//! identity (`eth_getBlockByNumber`), account basics, storage words, and the raw
//! `debug_getRawBlockAccessList` representation — and refuses everything else,
//! so an unexpected method fails the run instead of silently answering. The
//! official `eth_getBlockAccessList` method is refused rather than served: the
//! scripted node holds the raw bytes only, and the refusal keeps the
//! acquisition's fallback visible in the wire census ([`BalMode`]). It holds no
//! market state: the corpora driven through it are scripted candidates, and the
//! only storage words a run scripts are the ones its frames read
//! ([`DeltaRecorder::serve_storage`]); every other identity answers zero.
//!
//! # The latency model
//!
//! [`StorageLatency`] prices DISPATCHED packets, never the work inside them:
//! one storage batch round trip plus one charge per member, one charge per
//! serial storage read, and one charge per BAL fetch. The transport sleeps
//! exactly those durations before answering, so a run's measured wall contains
//! the model and [`WireCensus::modeled_us`] states the same number from the
//! recorded wire. Account and header reads are deliberately unpriced: the
//! quantity under test is the difference between the lanes, not the shared
//! identity read.
//!
//! Nothing here is a latency CLAIM about a live node — the durations are
//! synthetic, deterministic inputs, and the report says so.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::primitives::{Address, B256, U256};
use alloy::providers::RootProvider;
use alloy::rpc::client::RpcClient;
use alloy::rpc::json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{TransportError, TransportFut};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::delta_snapshot::{BalHeadDriver, HeadOutcome};
use degenbot_simulation::sim::ReadAnchor;
use serde_json::value::{to_raw_value, RawValue};

/// The height every dispatch fan-out in these corpora anchors at.
pub const BLOCK: u64 = 100;

/// The empty EIP-7928 list: one complete, decodable list carrying no final
/// write, so a head that serves it settles READY and carries nothing.
pub const EMPTY_BAL: &[u8] = &[0xc0];

/// The raw RLP BAL method the scripted node serves.
const RAW_BAL_METHOD: &str = "debug_getRawBlockAccessList";

/// The official JSON BAL method: refused here, so the acquisition's fallback to
/// it is on the wire whenever the raw method is missing.
const JSON_BAL_METHOD: &str = "eth_getBlockAccessList";

/// Whether `method` is one of the two official block-access-list
/// representations.
fn is_bal_method(method: &str) -> bool {
    method == RAW_BAL_METHOD || method == JSON_BAL_METHOD
}

/// One deterministic RPC latency model, in microseconds, for the packets a
/// dispatch fan-out dispatches. NOT a measurement of any node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StorageLatency {
    /// One storage batch round trip, independent of its member count.
    pub batch_round: u64,
    /// One member of a storage batch.
    pub batch_member: u64,
    /// One serial (non-batched) storage read: the dynamic-fallback shape.
    pub single: u64,
    /// One BAL list fetch, the background head pipeline's round trip.
    pub bal_fetch: u64,
}

impl StorageLatency {
    /// No added latency: the CPU and scheduler cost alone.
    pub const ZERO: Self = Self {
        batch_round: 0,
        batch_member: 0,
        single: 0,
        bal_fetch: 0,
    };

    /// The default NONZERO point: a 400 us storage round trip carrying a 20 us
    /// per-member transfer cost, a 400 us serial read, and a 400 us list fetch.
    pub const STORAGE_400US: Self = Self {
        batch_round: 400,
        batch_member: 20,
        single: 400,
        bal_fetch: 400,
    };

    /// The model's name, for a report row.
    #[must_use]
    pub fn label(&self) -> String {
        format!(
            "batch {}us + {}us/member, single {}us, bal {}us",
            self.batch_round, self.batch_member, self.single, self.bal_fetch
        )
    }
}

/// What one run dispatched, counted from the transport's own record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WireCensus {
    /// Anchor identity reads (`eth_getBlockByNumber`).
    pub header_reads: u64,
    /// Storage batch packets (an all-`eth_getStorageAt` batch).
    pub storage_batches: u64,
    /// Storage batch members, summed over those packets.
    pub storage_members: u64,
    /// Serial storage reads (the dynamic-fallback shape).
    pub storage_singles: u64,
    /// BAL list fetches, through EITHER official representation.
    pub bal_fetches: u64,
    /// Requests of any other method (account basics, code, ...).
    pub other_requests: u64,
    /// Every dispatched request, a batch counted as one packet.
    pub packets: u64,
}

impl WireCensus {
    /// The wall this census pays under `model`, in microseconds: the model's
    /// own arithmetic over the RECORDED wire, independent of the clock.
    #[must_use]
    pub fn modeled_us(&self, model: &StorageLatency) -> u64 {
        self.storage_batches * model.batch_round
            + self.storage_members * model.batch_member
            + self.storage_singles * model.single
            + self.bal_fetches * model.bal_fetch
    }

    /// Every storage word this run read (batch members plus serial reads).
    #[must_use]
    pub const fn storage_words(&self) -> u64 {
        self.storage_members + self.storage_singles
    }
}

/// The BAL endpoint's scripted behaviour.
type BalScript = Mutex<BalMode>;

/// How the scripted BAL endpoint answers.
///
/// The scripted node holds the raw representation, so it serves
/// `debug_getRawBlockAccessList` under every mode but [`BalMode::Unsupported`]
/// and refuses the official JSON one; an unsupported endpoint refuses BOTH, as a
/// node without the methods does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalMode {
    /// Serve the configured raw list.
    Serving,
    /// Refuse BOTH official methods: the raw request is refused first and the
    /// official JSON one it falls back to is refused too, so no successful
    /// negotiation becomes sticky and every acquisition pays both refusals.
    Unsupported,
    /// Answer, but only after a fixed delay — the wait-shapes' deliberately
    /// slow preparation, whose wall the report measures and never asserts.
    Slow,
}

/// The default script: serve the list.
fn serving_bal() -> BalScript {
    Mutex::new(BalMode::Serving)
}

/// A scripted chain: one canonical head (`hash` + `parentHash`), zero storage
/// words, a configurable raw BAL payload, a deterministic latency model, and
/// the records a lane comparison reads.
#[derive(Clone)]
pub struct DeltaRecorder {
    bal_requests: Arc<AtomicU64>,
    head_reads: Arc<AtomicU64>,
    storage_batches: Arc<AtomicU64>,
    storage_members: Arc<AtomicU64>,
    storage_singles: Arc<AtomicU64>,
    other_requests: Arc<AtomicU64>,
    heads: Arc<Mutex<std::collections::BTreeMap<u64, (B256, B256)>>>,
    bal: Arc<BalScript>,
    bal_payload: Arc<Mutex<Vec<u8>>>,
    /// The deferred BAL gate: while armed, a BAL fetch parks on
    /// [`Self::release_bal`] and nothing else answers it — the deterministic
    /// "genuinely in flight" state a wait's registration is observed in.
    bal_deferred: Arc<AtomicBool>,
    bal_release: Arc<tokio::sync::watch::Sender<bool>>,
    /// Gated BAL fetches currently parked on [`Self::release_bal`].
    bal_in_flight: Arc<AtomicU64>,
    /// The scripted storage words, keyed by `(address, slot)`. An identity this
    /// map does not name serves zero, so a run scripts exactly the words its
    /// frames read and nothing else.
    storage: Arc<Mutex<std::collections::BTreeMap<(Address, U256), U256>>>,
    latency: StorageLatency,
    /// Every dispatched request's wire body — method + serialized params, in
    /// dispatch order — so a lane's traffic can be compared byte for byte
    /// against another construction's.
    wire: Arc<Mutex<Vec<String>>>,
}

impl Default for DeltaRecorder {
    fn default() -> Self {
        Self {
            bal_requests: Arc::default(),
            head_reads: Arc::default(),
            storage_batches: Arc::default(),
            storage_members: Arc::default(),
            storage_singles: Arc::default(),
            other_requests: Arc::default(),
            heads: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            bal: Arc::new(serving_bal()),
            bal_payload: Arc::new(Mutex::new(EMPTY_BAL.to_vec())),
            bal_deferred: Arc::default(),
            bal_release: Arc::new(tokio::sync::watch::channel(false).0),
            bal_in_flight: Arc::default(),
            storage: Arc::default(),
            latency: StorageLatency::ZERO,
            wire: Arc::default(),
        }
    }
}

impl DeltaRecorder {
    /// The same scripted chain, every storage packet delayed by `latency`.
    #[must_use]
    pub fn with_latency(latency: StorageLatency) -> Self {
        Self {
            latency,
            ..Self::default()
        }
    }

    /// Serve `number` as the canonical block `hash`, built on `parent`.
    pub fn serve_head(&self, number: u64, hash: B256, parent: B256) {
        self.heads
            .lock()
            .expect("recorder mutex")
            .insert(number, (hash, parent));
    }

    fn head(&self, number: u64) -> (B256, B256) {
        *self
            .heads
            .lock()
            .expect("recorder mutex")
            .get(&number)
            .expect("the scripted chain serves this height")
    }

    pub fn set_bal_mode(&self, mode: BalMode) {
        *self.bal.lock().expect("recorder mutex") = mode;
    }

    /// Serve these raw list bytes for every BAL request.
    pub fn set_bal_payload(&self, bytes: Vec<u8>) {
        *self.bal_payload.lock().expect("recorder mutex") = bytes;
    }

    /// Defer every BAL answer until [`Self::release_bal`]: a fetch is
    /// genuinely in flight — parked on a gate only the test opens — so a
    /// preparation's claim is observably live before anything settles it.
    pub fn defer_bal(&self) {
        self.bal_release.send_replace(false);
        self.bal_in_flight.store(0, Ordering::Relaxed);
        self.bal_deferred.store(true, Ordering::Relaxed);
    }

    /// Answer the deferred BAL fetches, completing every preparation parked
    /// on the gate.
    pub fn release_bal(&self) {
        self.bal_deferred.store(false, Ordering::Relaxed);
        self.bal_release.send_replace(true);
    }

    /// BAL fetches dispatched but not yet answered: the observable "the
    /// preparation is in flight" signal.
    #[must_use]
    pub fn bal_in_flight(&self) -> u64 {
        self.bal_in_flight.load(Ordering::Relaxed)
    }

    /// Serve `value` for the storage identity `(address, slot)`. Every identity
    /// this map does not name answers zero.
    pub fn serve_storage(&self, address: Address, slot: U256, value: U256) {
        self.storage
            .lock()
            .expect("recorder mutex")
            .insert((address, slot), value);
    }

    /// The word this chain serves for `(address, slot)`: the scripted value, or
    /// zero for an identity no run scripted.
    fn storage_word(&self, address: Address, slot: U256) -> U256 {
        self.storage
            .lock()
            .expect("recorder mutex")
            .get(&(address, slot))
            .copied()
            .unwrap_or(U256::ZERO)
    }

    pub fn bal_requests(&self) -> u64 {
        self.bal_requests.load(Ordering::Relaxed)
    }

    pub fn head_reads(&self) -> u64 {
        self.head_reads.load(Ordering::Relaxed)
    }

    pub fn storage_batches(&self) -> u64 {
        self.storage_batches.load(Ordering::Relaxed)
    }

    pub fn storage_members(&self) -> u64 {
        self.storage_members.load(Ordering::Relaxed)
    }

    pub fn storage_singles(&self) -> u64 {
        self.storage_singles.load(Ordering::Relaxed)
    }

    /// Every dispatched request's wire body, in dispatch order.
    pub fn wire(&self) -> Vec<String> {
        self.wire.lock().expect("recorder mutex").clone()
    }

    /// Forget the recorded wire bodies (never the counts a run already read).
    pub fn clear_wire(&self) {
        self.wire.lock().expect("recorder mutex").clear();
    }

    /// The storage identities the recorded wire named, in dispatch order:
    /// every batch member and every serial read, as `(address, slot)`.
    pub fn storage_identities(&self) -> Vec<(Address, U256)> {
        self.wire()
            .iter()
            .filter_map(|request| {
                let params = request.strip_prefix("eth_getStorageAt ")?;
                let params: serde_json::Value = serde_json::from_str(params).ok()?;
                let address: Address = params.get(0)?.as_str()?.parse().ok()?;
                let slot: U256 = params.get(1)?.as_str()?.parse().ok()?;
                Some((address, slot))
            })
            .collect()
    }

    /// The census every counter above adds up to.
    #[must_use]
    pub fn census(&self) -> WireCensus {
        let storage_batches = self.storage_batches.load(Ordering::Relaxed);
        let storage_members = self.storage_members.load(Ordering::Relaxed);
        let storage_singles = self.storage_singles.load(Ordering::Relaxed);
        let bal_fetches = self.bal_requests.load(Ordering::Relaxed);
        let header_reads = self.head_reads.load(Ordering::Relaxed);
        let other = self.other_requests.load(Ordering::Relaxed);
        WireCensus {
            header_reads,
            storage_batches,
            storage_members,
            storage_singles,
            bal_fetches,
            other_requests: other,
            packets: storage_batches + storage_singles + bal_fetches + header_reads + other,
        }
    }
}

#[derive(Clone)]
struct RecordingTransport {
    recorder: DeltaRecorder,
}

pub fn provider(recorder: &DeltaRecorder) -> AlloyProvider {
    let client = RpcClient::new(
        RecordingTransport {
            recorder: recorder.clone(),
        },
        true,
    );
    let root: RootProvider = RootProvider::new(client);
    AlloyProvider::from_provider(
        Arc::new(root) as Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>
    )
}

pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("multi-thread runtime")
}

fn success(value: &str) -> ResponsePayload {
    let raw: Box<RawValue> =
        to_raw_value(&serde_json::Value::String(value.into())).expect("valid JSON string");
    ResponsePayload::Success(raw)
}

/// Record one dispatched request's wire body — method + serialized params —
/// in dispatch order.
fn record_wire(recorder: &DeltaRecorder, req: &SerializedRequest) {
    recorder.wire.lock().expect("recorder mutex").push(format!(
        "{} {}",
        req.method(),
        req.params().map_or("", |params| params.get())
    ));
}

/// The storage identity a read names: `(address, slot)` from its params.
fn storage_key(req: &SerializedRequest) -> Option<(Address, U256)> {
    let params = req.params()?;
    let value: serde_json::Value = serde_json::from_str(params.get()).ok()?;
    let address: Address = value.get(0)?.as_str()?.parse().ok()?;
    let slot: U256 = value.get(1)?.as_str()?.parse().ok()?;
    Some((address, slot))
}

/// The block tag a request names, when its first parameter is one.
fn block_number(req: &SerializedRequest) -> Option<u64> {
    let params = req.params()?;
    let value: serde_json::Value = serde_json::from_str(params.get()).ok()?;
    let number = value.get(0)?;
    number
        .as_str()
        .and_then(|text| u64::from_str_radix(text.trim_start_matches("0x"), 16).ok())
        .or_else(|| number.as_u64())
}

/// Whether this packet carries a BAL fetch.
fn is_bal_packet(req: &RequestPacket) -> bool {
    match req {
        RequestPacket::Single(one) => is_bal_method(one.method()),
        RequestPacket::Batch(many) => many.iter().any(|one| is_bal_method(one.method())),
    }
}

/// Whether this packet is a BAL fetch that must pay the slow endpoint's delay.
fn pays_slow_bal_delay(recorder: &DeltaRecorder, req: &RequestPacket) -> bool {
    is_bal_packet(req) && matches!(*recorder.bal.lock().expect("recorder mutex"), BalMode::Slow)
}

/// The one packet's charges under the latency model: a serial storage read, the
/// members of a storage batch, whether the packet IS a storage batch, and the
/// BAL requests it carries. Both the sleep and the census derive from this, so
/// a recorded wire and a reported model can never disagree.
fn packet_charges(req: &RequestPacket) -> (u64, u64, bool, u64) {
    match req {
        RequestPacket::Single(one) => {
            let serial = u64::from(one.method() == "eth_getStorageAt");
            let bal = u64::from(is_bal_method(one.method()));
            (serial, 0, false, bal)
        }
        RequestPacket::Batch(many) => {
            let is_storage_batch = many.iter().all(|one| one.method() == "eth_getStorageAt");
            let members = if is_storage_batch {
                u64::try_from(many.len()).unwrap_or(u64::MAX)
            } else {
                0
            };
            let bal = u64::try_from(
                many.iter()
                    .filter(|one| is_bal_method(one.method()))
                    .count(),
            )
            .unwrap_or(u64::MAX);
            (0, members, is_storage_batch, bal)
        }
    }
}

/// Sleep exactly the wall the latency model states for one dispatched packet —
/// the same arithmetic [`WireCensus::modeled_us`] reports from the recorded wire,
/// so the sleep and the census can never disagree.
async fn pay_modeled_latency(recorder: &DeltaRecorder, req: &RequestPacket) {
    let latency = recorder.latency;
    let (serial, members, is_storage_batch, bal) = packet_charges(req);
    let modeled_us = serial * latency.single
        + u64::from(is_storage_batch) * latency.batch_round
        + members * latency.batch_member
        + bal * latency.bal_fetch;
    if modeled_us > 0 {
        tokio::time::sleep(Duration::from_micros(modeled_us)).await;
    }
}

impl tower::Service<RequestPacket> for RecordingTransport {
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
        let recorder = self.recorder.clone();
        Box::pin(async move {
            let is_bal = is_bal_packet(&req);
            if is_bal {
                recorder.bal_in_flight.fetch_add(1, Ordering::Relaxed);
            }
            // A gated BAL fetch parks on the test's gate: the request is
            // genuinely in flight — and the claim that owns it observably
            // live — until the release answers it.
            if is_bal && recorder.bal_deferred.load(Ordering::Relaxed) {
                let mut released = recorder.bal_release.subscribe();
                let _ = released.wait_for(|open| *open).await;
            }
            // A slow BAL endpoint PENDS on a timer rather than blocking this
            // worker, so the fetch stays observably outstanding while the rest
            // of the run proceeds.
            if pays_slow_bal_delay(&recorder, &req) {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            // The deterministic latency model, paid before the packet is
            // answered.
            pay_modeled_latency(&recorder, &req).await;
            if is_bal {
                recorder.bal_in_flight.fetch_sub(1, Ordering::Relaxed);
            }
            let respond = |req: &SerializedRequest| -> Response {
                record_wire(&recorder, req);
                let payload = match req.method() {
                    "eth_getBalance" | "eth_getTransactionCount" => success("0x0"),
                    "eth_getCode" => success("0x"),
                    "eth_getStorageAt" => {
                        let (address, slot) =
                            storage_key(req).expect("the storage read names its key");
                        let word = recorder.storage_word(address, slot);
                        success(&format!("0x{word:064x}"))
                    }
                    "eth_getBlockByNumber" => {
                        recorder.head_reads.fetch_add(1, Ordering::Relaxed);
                        let number = block_number(req).expect("the lineage read names its block");
                        let (hash, parent) = recorder.head(number);
                        let raw: Box<RawValue> = to_raw_value(&serde_json::json!({
                            "hash": hash,
                            "parentHash": parent,
                        }))
                        .expect("valid JSON object");
                        ResponsePayload::Success(raw)
                    }
                    "debug_getRawBlockAccessList" | "eth_getBlockAccessList" => {
                        recorder.bal_requests.fetch_add(1, Ordering::Relaxed);
                        let mode = *recorder.bal.lock().expect("recorder mutex");
                        // The packet already paid the delay before this response
                        // was built, so a slow endpoint answers exactly like a
                        // serving one once it arrives. The scripted node holds
                        // the raw bytes only, so the official JSON method is a
                        // method absence here; an unsupported endpoint answers
                        // that way for BOTH methods.
                        if mode == BalMode::Unsupported || req.method() == JSON_BAL_METHOD {
                            return Response {
                                id: req.id().clone(),
                                payload: ResponsePayload::Failure(ErrorPayload {
                                    code: -32_601,
                                    message: format!("method not found: {}", req.method()).into(),
                                    data: None,
                                }),
                            };
                        }
                        let payload = recorder.bal_payload.lock().expect("recorder mutex").clone();
                        let raw: Box<RawValue> = to_raw_value(&serde_json::Value::String(
                            alloy::primitives::hex::encode_prefixed(&payload),
                        ))
                        .expect("valid JSON value");
                        ResponsePayload::Success(raw)
                    }
                    other => {
                        recorder.other_requests.fetch_add(1, Ordering::Relaxed);
                        return Response {
                            id: req.id().clone(),
                            payload: ResponsePayload::Failure(ErrorPayload {
                                code: -32_601,
                                message: format!("method not found: {other}").into(),
                                data: None,
                            }),
                        };
                    }
                };
                Response {
                    id: req.id().clone(),
                    payload,
                }
            };
            Ok(match req {
                RequestPacket::Single(r) => {
                    if r.method() == "eth_getStorageAt" {
                        recorder.storage_singles.fetch_add(1, Ordering::Relaxed);
                    }
                    ResponsePacket::Single(respond(&r))
                }
                RequestPacket::Batch(rs) => {
                    if rs.iter().all(|r| r.method() == "eth_getStorageAt") {
                        recorder.storage_batches.fetch_add(1, Ordering::Relaxed);
                        recorder.storage_members.fetch_add(
                            u64::try_from(rs.len()).unwrap_or(u64::MAX),
                            Ordering::Relaxed,
                        );
                    } else {
                        recorder.other_requests.fetch_add(
                            u64::try_from(rs.len()).unwrap_or(u64::MAX),
                            Ordering::Relaxed,
                        );
                    }
                    ResponsePacket::Batch(rs.iter().map(respond).collect())
                }
            })
        })
    }
}

/// Deliver, prepare and apply ONE head exactly as the canonical-head pipeline
/// does on the pump's header edge — deliberately on the OTHER side of the
/// fan-out, which is what makes the selection a query.
pub async fn settle_head(
    driver: &Arc<BalHeadDriver>,
    anchor: ReadAnchor,
    parent_hash: B256,
) -> HeadOutcome {
    let ticket = driver.deliver(anchor, parent_hash);
    driver.advance(ticket, &[]).await
}
