//! Public-seam tests for the anchor-scoped read hub
//! (`degenbot_simulation::sim::evm2::read_hub`).
//!
//! Every test drives the hub through its public interface against a stub chain
//! that counts dispatched JSON-RPC packets: one packet is one round trip, and one
//! packet member is one `eth_getStorageAt` / `eth_getBalance` /
//! `eth_getTransactionCount` / `eth_getCode` read. That is the whole
//! coalescing contract — duplicates share a member, distinct reads share a
//! batch, and a failed member or round reaches only the requesters it owns.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the stub chain and its assertions fail loudly on a broken fixture"
)]

use alloy::primitives::{Address, Bytes, StorageValue, B256, U256};
use alloy::providers::RootProvider;
use alloy::rpc::client::ClientBuilder;
use alloy::rpc::json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{TransportError, TransportErrorKind, TransportFut};
use degenbot_core::errors::ProviderError;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::bal_prefetch::{BalPlan, BalSkip};
use degenbot_simulation::sim::evm2::read_hub::{
    CarryOutcome, CarryRefusal, ReadHub, ReadHubCache, ReadHubError, SnapshotRetention,
};
use degenbot_simulation::sim::ReadAnchor;
use serde_json::value::to_raw_value;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use tokio::sync::Notify;

/// One dispatched packet, recorded as its `(method, params)` members — one
/// entry per transport `call`, so a JSON-RPC batch is ONE entry.
type Packet = Vec<(String, String)>;

/// The stub's per-`(height, account, slot)` answer table — one block's chain
/// truth.
type ChainValues = HashMap<(u64, Address, U256), u64>;

/// The address whose members the stub refuses (a node that answers the batch but
/// cannot serve this account's state).
const REFUSED: Address = Address::new([0xbb; 20]);

/// The slot/balance value every served read answers with.
const SERVED: u64 = 42;

/// The anchor every test reads at, unless the test is about anchor identity.
const HEIGHT: u64 = 100;

/// A chain stub serving the hub's two batch shapes and recording every packet.
#[derive(Debug, Clone)]
struct ChainStub {
    packets: Arc<Mutex<Vec<Packet>>>,
    /// Fail the whole transport instead of answering.
    down: Arc<AtomicBool>,
    /// Answer every account as an all-empty row (an EOA's balance/nonce/code).
    empty_accounts: Arc<AtomicBool>,
    /// Refuse [`REFUSED`]'s members with a JSON-RPC failure (a served batch
    /// that cannot answer one member), cleared by [`Self::stop_refusing`].
    refusing: Arc<AtomicBool>,
    /// Whether answers park until the test releases the round — an in-flight
    /// window with no timer in it.
    parked: Arc<AtomicBool>,
    /// Whether the BAL stage's own fetch parks — the window a carry transition
    /// spends inside its list fetch.
    bal_parked: Arc<AtomicBool>,
    /// The release signal parked answers wait on.
    gate: Arc<Notify>,
    /// Raw block access list bytes served to the BAL stage.
    bal: Arc<Mutex<Vec<u8>>>,
    /// Per-block-hash BAL overrides — the multi-generation chains.
    bal_by_hash: Arc<Mutex<HashMap<B256, Vec<u8>>>>,
    /// Refuse the BAL method the way a node without it answers.
    bal_unsupported: Arc<AtomicBool>,
    /// Answer the BAL request with JSON `null` — no list at that hash.
    bal_null: Arc<AtomicBool>,
    /// Per-`(height, account, slot)` storage answers — anything unlisted
    /// answers [`SERVED`].
    values: Arc<Mutex<ChainValues>>,
}

impl ChainStub {
    fn serving() -> Self {
        Self {
            packets: Arc::new(Mutex::new(Vec::new())),
            down: Arc::new(AtomicBool::new(false)),
            empty_accounts: Arc::new(AtomicBool::new(false)),
            refusing: Arc::new(AtomicBool::new(true)),
            parked: Arc::new(AtomicBool::new(false)),
            bal_parked: Arc::new(AtomicBool::new(false)),
            gate: Arc::new(Notify::new()),
            bal: Arc::new(Mutex::new(Vec::new())),
            bal_by_hash: Arc::new(Mutex::new(HashMap::new())),
            bal_unsupported: Arc::new(AtomicBool::new(false)),
            bal_null: Arc::new(AtomicBool::new(false)),
            values: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn set_down(&self, down: bool) {
        self.down.store(down, Ordering::Relaxed);
    }
    /// Serve `debug_getRawBlockAccessList` with these raw bytes.
    fn serve_bal(&self, bytes: Vec<u8>) {
        *self.bal.lock().unwrap() = bytes;
    }
    /// Refuse the BAL method the way a node without it answers.
    fn set_bal_unsupported(&self, unsupported: bool) {
        self.bal_unsupported.store(unsupported, Ordering::Relaxed);
    }
    /// Answer the BAL request with JSON `null` — the node holds no list there.
    fn set_bal_null(&self, null: bool) {
        self.bal_null.store(null, Ordering::Relaxed);
    }
    /// Serve `debug_getRawBlockAccessList` for ONE exact block hash.
    fn serve_bal_at(&self, hash: B256, bytes: Vec<u8>) {
        self.bal_by_hash.lock().unwrap().insert(hash, bytes);
    }
    /// Serve one storage word of one block's post state — the chain truth a
    /// cold read at `height` returns.
    fn serve_at(&self, height: u64, address: Address, slot: U256, value: u64) {
        self.values
            .lock()
            .unwrap()
            .insert((height, address, slot), value);
    }

    /// Serve every account read as an all-empty row.
    fn serve_empty_accounts(&self, empty: bool) {
        self.empty_accounts.store(empty, Ordering::Relaxed);
    }

    /// Stop refusing [`REFUSED`]'s members — the healed node a retried identity
    /// reads from.
    fn stop_refusing(&self) {
        self.refusing.store(false, Ordering::Relaxed);
    }

    /// Park every answer until [`Self::release`] — the round stays in flight.
    fn park(&self) {
        self.parked.store(true, Ordering::Relaxed);
    }

    /// Park the BAL stage's fetch until [`Self::release`] — the carry
    /// transition stays inside its list fetch.
    fn park_bal(&self) {
        self.bal_parked.store(true, Ordering::Relaxed);
    }

    /// Release parked answers and stop parking new ones.
    fn release(&self) {
        self.parked.store(false, Ordering::Relaxed);
        self.bal_parked.store(false, Ordering::Relaxed);
        self.gate.notify_waiters();
    }

    fn packets(&self) -> Vec<Packet> {
        self.packets.lock().unwrap().clone()
    }

    fn members(&self) -> usize {
        self.packets().iter().map(Vec::len).sum()
    }

    /// Storage members dispatched — one `eth_getStorageAt` per slot.
    fn storage_members(&self) -> usize {
        self.packets()
            .iter()
            .map(|packet| {
                packet
                    .iter()
                    .filter(|(method, _)| method == "eth_getStorageAt")
                    .count()
            })
            .sum()
    }

    /// BAL fetches dispatched.
    fn bal_requests(&self) -> usize {
        self.packets()
            .iter()
            .filter(|packet| packet.len() == 1 && packet[0].0 == "debug_getRawBlockAccessList")
            .count()
    }

    fn clear(&self) {
        self.packets.lock().unwrap().clear();
    }

    fn provider(&self) -> AlloyProvider {
        let client = ClientBuilder::default().transport(self.clone(), true);
        let root: RootProvider = RootProvider::new(client);
        AlloyProvider::from_provider(Arc::new(root))
    }

    /// A dispatched request's identity in wire form.
    fn answer(&self, req: &SerializedRequest) -> Response {
        if req.method() == "debug_getRawBlockAccessList" {
            return self.answer_bal(req);
        }
        let failed = self.refusing.load(Ordering::Relaxed) && Self::refused(req);
        if failed {
            return Response {
                id: req.id().clone(),
                payload: ResponsePayload::Failure(ErrorPayload {
                    code: -32_000,
                    message: Cow::Borrowed("missing trie node"),
                    data: None,
                }),
            };
        }
        let result =
            if self.empty_accounts.load(Ordering::Relaxed) && req.method() != "eth_getStorageAt" {
                match req.method() {
                    "eth_getBalance" | "eth_getTransactionCount" => serde_json::json!("0x0"),
                    "eth_getCode" => serde_json::json!("0x"),
                    other => panic!("a hub read must not issue {other}"),
                }
            } else {
                match req.method() {
                    "eth_getStorageAt" => {
                        let (address, slot, height) = Self::storage_params(req);
                        let value = self
                            .values
                            .lock()
                            .unwrap()
                            .get(&(height, address, slot))
                            .copied()
                            .unwrap_or(SERVED);
                        serde_json::json!(format!("{value:#x}"))
                    }
                    "eth_getBalance" => serde_json::json!(format!("{SERVED:#x}")),
                    "eth_getTransactionCount" => serde_json::json!("0x2a"),
                    "eth_getCode" => serde_json::json!("0x60016001"),
                    other => panic!("a hub read must not issue {other}"),
                }
            };
        Response {
            id: req.id().clone(),
            payload: ResponsePayload::Success(to_raw_value(&result).unwrap()),
        }
    }

    /// The BAL stage's own request: the raw list bytes, or the node's refusal.
    fn answer_bal(&self, req: &SerializedRequest) -> Response {
        let payload = if self.bal_unsupported.load(Ordering::Relaxed) {
            ResponsePayload::Failure(ErrorPayload {
                code: -32_601,
                message: Cow::Borrowed("the method debug_getRawBlockAccessList does not exist"),
                data: None,
            })
        } else if self.bal_null.load(Ordering::Relaxed) {
            ResponsePayload::Success(to_raw_value(&Value::Null).unwrap())
        } else {
            let hash = Self::bal_hash(req);
            let raw = self
                .bal_by_hash
                .lock()
                .unwrap()
                .get(&hash)
                .cloned()
                .unwrap_or_else(|| self.bal.lock().unwrap().clone());
            ResponsePayload::Success(to_raw_value(&Bytes::copy_from_slice(&raw)).unwrap())
        };
        Response {
            id: req.id().clone(),
            payload,
        }
    }
    /// A storage member's `(address, slot, height)` in wire form.
    fn storage_params(req: &SerializedRequest) -> (Address, U256, u64) {
        let params: Value =
            serde_json::from_str(req.params().expect("a member carries params").get())
                .expect("member params are JSON");
        let address = params
            .get(0)
            .and_then(Value::as_str)
            .expect("the address is the first param")
            .parse()
            .expect("the member address parses");
        let slot = params
            .get(1)
            .and_then(Value::as_str)
            .expect("the slot is the second param")
            .parse()
            .expect("the member slot parses");
        let height = params
            .get(2)
            .and_then(Value::as_str)
            .expect("the block tag is the third param")
            .trim_start_matches("0x");
        (
            address,
            slot,
            u64::from_str_radix(height, 16).expect("the block tag is hex"),
        )
    }

    /// The one block identity a BAL request carries.
    fn bal_hash(req: &SerializedRequest) -> B256 {
        let params: Value =
            serde_json::from_str(req.params().expect("a BAL request carries params").get())
                .expect("BAL params are JSON");
        params
            .get(0)
            .and_then(Value::as_str)
            .expect("the block hash is the first param")
            .parse()
            .expect("the block hash parses")
    }

    /// Whether this member belongs to the refused address.
    fn refused(req: &SerializedRequest) -> bool {
        if req.method() == "debug_getRawBlockAccessList" {
            return false;
        }
        let params: Value =
            serde_json::from_str(req.params().expect("a member carries params").get())
                .expect("member params are JSON");
        let address: Address = params
            .get(0)
            .and_then(Value::as_str)
            .expect("the address is the first param")
            .parse()
            .expect("the member address parses");
        address == REFUSED
    }
}

impl tower::Service<RequestPacket> for ChainStub {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        let stub = self.clone();
        Box::pin(async move {
            let recorded: Packet = match &req {
                RequestPacket::Single(one) => vec![(
                    one.method().to_string(),
                    one.params()
                        .map_or_else(String::new, |p| p.get().to_string()),
                )],
                RequestPacket::Batch(many) => many
                    .iter()
                    .map(|one| {
                        (
                            one.method().to_string(),
                            one.params()
                                .map_or_else(String::new, |p| p.get().to_string()),
                        )
                    })
                    .collect(),
            };
            stub.packets.lock().unwrap().push(recorded);
            // The storage park and the BAL park are separate knobs: a test
            // holds exactly the window it is about, and the BAL fetch does not
            // park behind a storage round unless asked to.
            let holds_round = stub.parked.load(Ordering::Relaxed) && is_storage_batch(&req);
            let holds_bal = stub.bal_parked.load(Ordering::Relaxed) && is_bal_request(&req);
            if holds_round || holds_bal {
                stub.gate.notified().await;
            }
            if stub.down.load(Ordering::Relaxed) {
                return Err(TransportErrorKind::custom_str("stub transport down"));
            }
            Ok(match req {
                RequestPacket::Single(one) => ResponsePacket::Single(stub.answer(&one)),
                RequestPacket::Batch(many) => {
                    ResponsePacket::Batch(many.iter().map(|one| stub.answer(one)).collect())
                }
            })
        })
    }
}

/// The value every served storage word and account balance answers with.
fn served_value() -> StorageValue {
    StorageValue::from(SERVED)
}

/// A slot index that is not zero, so a zero answer cannot be mistaken for a miss.
fn slot() -> U256 {
    U256::from(7u64)
}

/// An address distinct from [`REFUSED`].
fn address(tag: u8) -> Address {
    Address::new([tag; 20])
}

/// Two requesters of one storage identity cost one fetch, and both receive the
/// anchor's word.
#[tokio::test]
async fn duplicate_storage_reads_share_one_fetch() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let hub = ReadHub::new(&provider, ReadAnchor::at(HEIGHT));

    let first = hub.read_storage(address(0x11), slot());
    let second = hub.read_storage(address(0x11), slot());
    let report = hub.drain_round().await;

    assert_eq!(
        first.await.unwrap(),
        served_value(),
        "the first requester reads the anchor's word"
    );
    assert_eq!(
        second.await.unwrap(),
        served_value(),
        "the duplicate requester reads the same word"
    );
    assert_eq!(
        report.storage_members, 1,
        "one identity in the round, got {report:?}"
    );
    assert_eq!(
        stub.packets().len(),
        1,
        "duplicates share ONE dispatched packet, got {:?}",
        stub.packets()
    );
    assert_eq!(stub.members(), 1, "and ONE batch member");
    assert_eq!(hub.stats().coalesced, 1, "the duplicate coalesced");
}
/// Distinct storage reads share ONE dispatched packet, one member each, every
/// member pinned to the anchor's height.
#[tokio::test]
async fn distinct_storage_reads_share_one_batch() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let hub = ReadHub::new(&provider, ReadAnchor::at(HEIGHT));

    let first = hub.read_storage(address(0x11), slot());
    let other_account = hub.read_storage(address(0x12), slot());
    let other_slot = hub.read_storage(address(0x11), U256::from(9u64));
    let report = hub.drain_round().await;

    assert_eq!(
        report.storage_members, 3,
        "three distinct identities in the round, got {report:?}"
    );
    assert_eq!(report.account_members, 0, "no account read was queued");
    let packets = stub.packets();
    assert_eq!(
        packets.len(),
        1,
        "distinct reads share ONE dispatched packet, got {packets:?}"
    );
    let batch = &packets[0];
    assert_eq!(batch.len(), 3, "one member per identity: {batch:?}");
    assert!(
        batch.iter().all(|(method, _)| method == "eth_getStorageAt"),
        "a storage batch carries only storage reads: {batch:?}"
    );
    assert!(
        batch
            .iter()
            .all(|(_, params)| params.contains(&format!("{HEIGHT:#x}"))),
        "every member pins the anchor's height: {batch:?}"
    );
    for read in [first, other_account, other_slot] {
        assert_eq!(read.await.unwrap(), served_value());
    }
    assert_eq!(hub.stats().rounds, 1);
    assert_eq!(hub.memoized_slots(), 3, "all three words are memoized");
}

/// Duplicate account reads share one fetch, and one account read is the three
/// member reads (`eth_getBalance` / `eth_getTransactionCount` / `eth_getCode`).
#[tokio::test]
async fn duplicate_account_reads_share_one_fetch() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let hub = ReadHub::new(&provider, ReadAnchor::at(HEIGHT));

    let first = hub.read_account(address(0x21));
    let second = hub.read_account(address(0x21));
    let report = hub.drain_round().await;

    let read = first.await.unwrap();
    assert_eq!(
        read,
        second.await.unwrap(),
        "both requesters read the same row"
    );
    assert_eq!(read.balance, U256::from(SERVED));
    assert_eq!(read.nonce, 0x2a);
    assert_eq!(read.code.as_ref(), &[0x60, 0x01, 0x60, 0x01]);
    assert_eq!(
        report.account_members, 1,
        "one identity in the round, got {report:?}"
    );
    assert_eq!(
        stub.members(),
        3,
        "one account is balance + nonce + code, got {:?}",
        stub.packets()
    );
    assert_eq!(hub.stats().coalesced, 1);
}

/// One round dispatches both read kinds concurrently: a storage batch and a
/// multi-account batch, each its own packet, and account rows hydrate in full.
#[tokio::test]
async fn one_round_dispatches_both_batch_kinds() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let hub = ReadHub::new(&provider, ReadAnchor::at(HEIGHT));

    let word = hub.read_storage(address(0x11), slot());
    let first_account = hub.read_account(address(0x21));
    let second_account = hub.read_account(address(0x22));
    let report = hub.drain_round().await;

    assert_eq!(report.storage_members, 1, "{report:?}");
    assert_eq!(report.account_members, 2, "{report:?}");
    assert_eq!(
        report.account_rpc_members(),
        6,
        "three member reads per account"
    );
    let packets = stub.packets();
    assert_eq!(packets.len(), 2, "one packet per read kind: {packets:?}");
    let account_packet = packets
        .iter()
        .find(|packet| packet.len() == 6)
        .expect("the account batch is the six-member packet");
    assert_eq!(
        account_packet
            .iter()
            .map(|(method, _)| method.as_str())
            .collect::<Vec<_>>(),
        [
            "eth_getBalance",
            "eth_getTransactionCount",
            "eth_getCode",
            "eth_getBalance",
            "eth_getTransactionCount",
            "eth_getCode",
        ],
        "balance + nonce + code, per address, in registration order"
    );
    assert_eq!(
        packets.iter().filter(|packet| packet.len() == 1).count(),
        1,
        "the storage batch is the one-member packet: {packets:?}"
    );
    assert_eq!(word.await.unwrap(), served_value());
    let row = first_account.await.unwrap();
    assert_eq!(row.balance, U256::from(SERVED));
    assert_eq!(row.nonce, 0x2a);
    assert_eq!(row.code.as_ref(), &[0x60, 0x01, 0x60, 0x01]);
    assert_eq!(second_account.await.unwrap(), row);
}

/// A settled value is answered from the anchor memo: no queue, no packet.
#[tokio::test]
async fn a_memoized_read_does_not_reach_the_chain() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let hub = ReadHub::new(&provider, ReadAnchor::at(HEIGHT));

    let first = hub.read_storage(address(0x11), slot());
    let first_account = hub.read_account(address(0x21));
    hub.drain_round().await;
    first.await.unwrap();
    first_account.await.unwrap();
    stub.clear();

    let second = hub.read_storage(address(0x11), slot());
    let second_account = hub.read_account(address(0x21));
    let report = hub.drain_round().await;

    assert_eq!(second.await.unwrap(), served_value());
    assert_eq!(second_account.await.unwrap().nonce, 0x2a);
    assert!(report.is_empty(), "nothing was queued: {report:?}");
    assert!(
        stub.packets().is_empty(),
        "a memo hit dispatches nothing: {:?}",
        stub.packets()
    );
    assert_eq!(hub.stats().memo_hits, 2, "both reads were memo hits");
}
/// A refused account member fails only its own requester: the round trip
/// succeeds and the neighbouring address in the same batch is answered.
#[tokio::test]
async fn a_refused_account_member_fails_only_its_requester() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let hub = ReadHub::new(&provider, ReadAnchor::at(HEIGHT));

    let refused = hub.read_account(REFUSED);
    let served = hub.read_account(address(0x21));
    let report = hub.drain_round().await;

    match refused.await {
        Err(ReadHubError::Rpc(ProviderError::RpcError { code, message })) => {
            assert_eq!(code, -32_000, "the member's own failure travels");
            assert!(message.contains("missing trie node"), "{message}");
        }
        other => panic!("the refused address must fail on its own: {other:?}"),
    }
    assert_eq!(
        served.await.unwrap().nonce,
        0x2a,
        "a refused member must not fail its neighbour"
    );
    assert_eq!(
        report.account_member_errors, 1,
        "one member failed, got {report:?}"
    );
    assert_eq!(
        stub.packets().len(),
        1,
        "the whole attribution is one round trip"
    );
}

/// A refused storage member fails only its own requester: the round trip
/// succeeds, the neighbouring slots in the same packet are answered and
/// memoized, and a later round re-issues only the failed identity.
#[tokio::test]
async fn a_refused_storage_member_fails_only_its_requester() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let hub = ReadHub::new(&provider, ReadAnchor::at(HEIGHT));

    let refused = hub.read_storage(REFUSED, slot());
    let served = hub.read_storage(address(0x11), slot());
    let other_slot = hub.read_storage(address(0x11), U256::from(9u64));
    let report = hub.drain_round().await;

    match refused.await {
        Err(ReadHubError::Rpc(ProviderError::RpcError { code, message })) => {
            assert_eq!(code, -32_000, "the member's own failure travels");
            assert!(message.contains("missing trie node"), "{message}");
        }
        other => panic!("the refused slot must fail on its own: {other:?}"),
    }
    assert_eq!(
        served.await.unwrap(),
        served_value(),
        "a refused storage member must not fail its neighbour"
    );
    assert_eq!(other_slot.await.unwrap(), served_value());
    assert_eq!(report.storage_members, 3, "{report:?}");
    assert_eq!(
        report.storage_member_errors, 1,
        "one slot was refused, got {report:?}"
    );
    let packets = stub.packets();
    assert_eq!(
        packets.len(),
        1,
        "the whole attribution is ONE dispatched packet, got {packets:?}"
    );
    assert_eq!(packets[0].len(), 3, "one member per slot: {:?}", packets[0]);
    assert_eq!(
        hub.memoized_slots(),
        2,
        "only the answered slots are memoized"
    );
    assert_eq!(
        hub.stats().storage_member_errors,
        1,
        "the refusal is counted, not hidden"
    );

    stub.stop_refusing();
    stub.clear();
    let retried = hub.read_storage(REFUSED, slot());
    let neighbour = hub.read_storage(address(0x11), slot());
    let retry_report = hub.drain_round().await;

    assert_eq!(
        retry_report.storage_members, 1,
        "only the failed identity re-queued, got {retry_report:?}"
    );
    assert_eq!(
        retry_report.storage_member_errors, 0,
        "the retried identity was served, got {retry_report:?}"
    );
    let packets = stub.packets();
    assert_eq!(
        packets.len(),
        1,
        "the retry is one dispatched packet, got {packets:?}"
    );
    assert_eq!(packets[0].len(), 1, "carrying one member: {:?}", packets[0]);
    assert_eq!(
        retried.await.unwrap(),
        served_value(),
        "the retried read reaches the healed chain"
    );
    assert_eq!(
        neighbour.await.unwrap(),
        served_value(),
        "the answered neighbour is served from the anchor memo"
    );
    assert_eq!(hub.memoized_slots(), 3, "the retried slot joined the memo");
}

/// A round failure (a downed transport) reaches every requester of the round,
/// memoizes nothing, and a later read re-queues instead of waiting on it.
#[tokio::test]
async fn a_failed_round_fails_its_requesters_and_a_later_read_retries() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let hub = ReadHub::new(&provider, ReadAnchor::at(HEIGHT));
    stub.set_down(true);

    let word = hub.read_storage(address(0x11), slot());
    let account = hub.read_account(address(0x21));
    let report = hub.drain_round().await;

    assert!(
        matches!(word.await, Err(ReadHubError::Rpc(_))),
        "the storage requester learns the round failed"
    );
    assert!(
        matches!(account.await, Err(ReadHubError::Rpc(_))),
        "the account requester learns the round failed"
    );
    assert_eq!(
        report.storage_members, 1,
        "the round still dispatched its member, got {report:?}"
    );
    assert_eq!(hub.memoized_slots(), 0, "a failed round memoizes nothing");

    stub.set_down(false);
    stub.clear();
    let retry = hub.read_storage(address(0x11), slot());
    let retry_account = hub.read_account(address(0x21));
    let retry_report = hub.drain_round().await;

    assert_eq!(
        retry_report.storage_members, 1,
        "the identity queued a fresh round, got {retry_report:?}"
    );
    assert_eq!(
        retry.await.unwrap(),
        served_value(),
        "the retried read reaches the healed chain"
    );
    assert_eq!(retry_account.await.unwrap().nonce, 0x2a);
    assert_eq!(
        stub.packets().len(),
        2,
        "one packet per kind for the retry, got {:?}",
        stub.packets()
    );
}

/// A cancelled round abandons its in-flight identities: the waiter is told no
/// answer will come, and the next read of that identity queues a fresh round.
#[tokio::test]
async fn a_cancelled_round_frees_its_in_flight_identities() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let hub = ReadHub::new(&provider, ReadAnchor::at(HEIGHT));
    stub.park();

    let abandoned = hub.read_storage(address(0x11), slot());
    let mut drain = Box::pin(hub.drain_round());
    let mut cx = Context::from_waker(Waker::noop());
    // Two polls: the batch future wakes itself once before it calls the
    // transport, so the first poll only reaches the dispatch.
    assert!(drain.as_mut().poll(&mut cx).is_pending());
    assert!(
        drain.as_mut().poll(&mut cx).is_pending(),
        "the round is parked inside the RPC, so it has not settled"
    );
    assert_eq!(stub.members(), 1, "the round reached the chain");
    drop(drain);

    assert!(
        matches!(abandoned.await, Err(ReadHubError::Abandoned)),
        "the abandoned requester learns no answer will arrive"
    );

    stub.release();
    stub.clear();
    let retried = hub.read_storage(address(0x11), slot());
    let report = hub.drain_round().await;

    assert_eq!(
        report.storage_members, 1,
        "the identity is readable again, got {report:?}"
    );
    assert_eq!(retried.await.unwrap(), served_value());
    assert_eq!(
        stub.members(),
        1,
        "the retry is one member, got {:?}",
        stub.packets()
    );
}

/// A read registered while a round is in flight joins that round: one fetch,
/// one packet, and both requesters get the same value.
///
/// The registrations that follow the parked poll are the lock-suspension probe:
/// they have to acquire the hub's lock while the round sits inside the RPC, so a
/// guard held across that await would deadlock here instead of coalescing.
#[tokio::test]
async fn a_read_registered_during_a_round_joins_that_round() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let hub = ReadHub::new(&provider, ReadAnchor::at(HEIGHT));
    stub.park();

    let first = hub.read_storage(address(0x11), slot());
    let mut drain = Box::pin(hub.drain_round());
    let mut cx = Context::from_waker(Waker::noop());
    // Two polls: the batch future wakes itself once before it calls the
    // transport, so the first poll only reaches the dispatch.
    assert!(drain.as_mut().poll(&mut cx).is_pending());
    assert!(
        drain.as_mut().poll(&mut cx).is_pending(),
        "the round is in flight"
    );
    assert_eq!(stub.members(), 1, "the round dispatched its one member");

    let second = hub.read_storage(address(0x11), slot());
    assert_eq!(hub.stats().coalesced, 1, "the in-flight read coalesced");
    let idle = hub.drain_round().await;
    assert!(
        idle.is_empty(),
        "the in-flight round owns the identity, got {idle:?}"
    );
    assert_eq!(stub.members(), 1, "the second drain dispatched nothing");

    stub.release();
    let report = drain.await;

    assert_eq!(report.storage_members, 1, "{report:?}");
    assert_eq!(
        first.await.unwrap(),
        served_value(),
        "the round's owner reads the anchor's word"
    );
    assert_eq!(
        second.await.unwrap(),
        served_value(),
        "the in-flight joiner reads the same word from the same fetch"
    );
    assert_eq!(
        stub.members(),
        1,
        "one fetch answered both requesters, got {:?}",
        stub.packets()
    );
}
/// One anchor's hub is shared: the second caller at the same anchor inherits
/// the memo the first one filled, and pays no chain read.
#[tokio::test]
async fn one_anchor_shares_one_hub() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let cache = ReadHubCache::new(&provider);
    assert_eq!(cache.anchor(), None, "no anchor installed yet");
    let anchor = ReadAnchor::identified(HEIGHT, B256::new([0xaa; 32]));

    let first = cache.for_anchor(anchor);
    let word = first.read_storage(address(0x11), slot());
    first.drain_round().await;
    assert_eq!(word.await.unwrap(), served_value());

    let second = cache.for_anchor(anchor);
    assert!(
        Arc::ptr_eq(&first, &second),
        "the same anchor hands out the same hub"
    );
    let memoized = second.read_storage(address(0x11), slot());
    let report = second.drain_round().await;

    assert_eq!(
        memoized.await.unwrap(),
        served_value(),
        "the second caller reads the first caller's fetch"
    );
    assert!(report.is_empty(), "the memo answered it: {report:?}");
    assert_eq!(
        stub.members(),
        1,
        "the anchor's word is paid for once, got {:?}",
        stub.packets()
    );
    assert_eq!(cache.anchor(), Some(anchor));
}

/// A same-height reorg (same number, different hash) is a different anchor: the
/// replacement block's hub starts cold, so the abandoned block's word is never
/// served at the replacement.
#[tokio::test]
async fn a_same_height_reorg_never_serves_the_abandoned_block() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let cache = ReadHubCache::new(&provider);
    let abandoned = ReadAnchor::identified(HEIGHT, B256::new([0xaa; 32]));
    let replacement = ReadAnchor::identified(HEIGHT, B256::new([0xbb; 32]));

    let pre_reorg = cache.for_anchor(abandoned);
    let stale = pre_reorg.read_storage(address(0x11), slot());
    pre_reorg.drain_round().await;
    assert_eq!(stale.await.unwrap(), served_value());

    let post_reorg = cache.for_anchor(replacement);
    assert!(
        !Arc::ptr_eq(&pre_reorg, &post_reorg),
        "the replacement block is a different anchor at the same height"
    );
    let fresh = post_reorg.read_storage(address(0x11), slot());
    let report = post_reorg.drain_round().await;

    assert_eq!(
        report.storage_members, 1,
        "the replacement block's read is its own fetch, got {report:?}"
    );
    assert_eq!(fresh.await.unwrap(), served_value());
    assert_eq!(
        stub.members(),
        2,
        "one member per block, got {:?}",
        stub.packets()
    );
    assert_eq!(
        pre_reorg.memoized_slots(),
        1,
        "the abandoned hub keeps only its own block's word"
    );
    assert_eq!(post_reorg.memoized_slots(), 1);
    assert_eq!(cache.anchor(), Some(replacement));
}

/// An all-empty account row is memoized at its anchor too — the hub's memo is
/// anchor-scoped, so it is not the cross-block warm cache's EOA refusal, and a
/// moved anchor reads the row again.
#[tokio::test]
async fn an_empty_account_row_is_memoized_only_at_its_anchor() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let cache = ReadHubCache::new(&provider);
    stub.serve_empty_accounts(true);

    let hub = cache.for_anchor(ReadAnchor::identified(HEIGHT, B256::new([0xaa; 32])));
    let first = hub.read_account(address(0x21));
    hub.drain_round().await;
    let row = first.await.unwrap();
    assert!(row.balance.is_zero() && row.nonce == 0 && row.code.is_empty());

    let second = hub.read_account(address(0x21));
    let report = hub.drain_round().await;
    assert!(
        report.is_empty(),
        "the anchor's memo answers the empty row: {report:?}"
    );
    assert_eq!(second.await.unwrap(), row);
    assert_eq!(
        stub.members(),
        3,
        "the empty row cost one batch, not two: {:?}",
        stub.packets()
    );

    let moved = cache.for_anchor(ReadAnchor::at(HEIGHT + 1));
    let fresh = moved.read_account(address(0x21));
    let moved_report = moved.drain_round().await;
    assert_eq!(moved_report.account_members, 1, "{moved_report:?}");
    assert_eq!(fresh.await.unwrap(), row);
    assert_eq!(
        stub.members(),
        6,
        "the new anchor pays its own three members: {:?}",
        stub.packets()
    );
}

/// Whether every member of this packet is a storage read — the shape a parked
/// round dispatches.
fn is_storage_batch(req: &RequestPacket) -> bool {
    match req {
        RequestPacket::Single(one) => one.method() == "eth_getStorageAt",
        RequestPacket::Batch(many) => many.iter().all(|one| one.method() == "eth_getStorageAt"),
    }
}

/// Whether this request is the BAL stage's own exact-hash fetch.
fn is_bal_request(req: &RequestPacket) -> bool {
    match req {
        RequestPacket::Single(one) => one.method() == "debug_getRawBlockAccessList",
        RequestPacket::Batch(_) => false,
    }
}

/// The worked BAL fixture: one account at `address(0x11)` whose slot `0x01` is
/// written twice (index 0 -> 7, index 2 -> 42) and slot `0x02` twice (index 1 ->
/// 1000, index 3 -> 42), plus read-only slots `0x03` and `0x04`. Hand-encoded
/// RLP of the canonical EIP-7928 shape (`AccountChanges = [address,
/// storage_changes, storage_reads, balance_changes, nonce_changes,
/// code_changes]`), so the expected final values (42, 42) and read identities
/// (3, 4) come from outside the decoder under test.
fn worked_bal() -> Vec<u8> {
    alloy::hex::decode(
        "f1f0941111111111111111111111111111111111111111d4c801c6c28007c2022aca02c8c4018203e8c2032ac20304c0c0c0",
    )
    .expect("the fixture is valid hex")
}
/// The worked fixture plus trailing junk: a valid list followed by a byte the
/// decoder must reject WHOLESALE, so the valid prefix cannot partially seed.
fn worked_bal_with_trailing_junk() -> Vec<u8> {
    let mut bytes = worked_bal();
    bytes.push(0x00);
    bytes
}
/// A child BAL whose only write is `address(0x11)` slot `0x01`, once
/// (index 2 -> 3): the final value 3. Same hand-encoded wire shape as
/// [`worked_bal`], so 3 comes from outside the decoder under test.
fn bal_writes_x3() -> Vec<u8> {
    alloy::hex::decode("e1e0941111111111111111111111111111111111111111c6c501c3c20203c0c0c0c0")
        .expect("the fixture is valid hex")
}
/// A child BAL whose finals come from SEVERAL writes and include a deletion:
/// slot `0x01` (index 0 -> 9, index 2 -> 0) has final value 0, and slot `0x02`
/// (index 1 -> 1000, index 3 -> 42) has final value 42.
fn bal_writes_zero_and_final() -> Vec<u8> {
    alloy::hex::decode(
        "efee941111111111111111111111111111111111111111d4c801c6c28009c20280ca02c8c4018203e8c2032ac0c0c0c0",
    )
    .expect("the fixture is valid hex")
}
/// A child BAL whose only write is `address(0x11)` slot `0x09`, final value 5.
fn bal_writes_z5() -> Vec<u8> {
    alloy::hex::decode("e1e0941111111111111111111111111111111111111111c6c509c3c20205c0c0c0c0")
        .expect("the fixture is valid hex")
}
/// The second generation's BAL: `address(0x11)` slot `0x01`, final value 5.
fn bal_writes_x5() -> Vec<u8> {
    alloy::hex::decode("e1e0941111111111111111111111111111111111111111c6c501c3c20205c0c0c0c0")
        .expect("the fixture is valid hex")
}
/// The third generation's BAL: `address(0x11)` slot `0x09`, final value 9.
fn bal_writes_z9() -> Vec<u8> {
    alloy::hex::decode("e1e0941111111111111111111111111111111111111111c6c509c3c20209c0c0c0c0")
        .expect("the fixture is valid hex")
}
/// A parity leg's first written block: slot `0x01` (index 2 -> 99, index 4 ->
/// 111) and slot `0x04` (index 1 -> 445, index 3 -> 444), finals 111 and 444.
fn bal_parity_writes() -> Vec<u8> {
    alloy::hex::decode(
        "f1f0941111111111111111111111111111111111111111d6c801c6c20263c2046fcc04cac4018201bdc4038201bcc0c0c0c0",
    )
    .expect("the fixture is valid hex")
}
/// The parity leg's second written block: slot `0x02`, final value 222.
fn bal_parity_writes_2() -> Vec<u8> {
    alloy::hex::decode("e2e1941111111111111111111111111111111111111111c7c602c4c30281dec0c0c0c0")
        .expect("the fixture is valid hex")
}
/// A syntactically valid BAL that writes nothing.
fn bal_empty() -> Vec<u8> {
    alloy::hex::decode("c0").expect("the fixture is valid hex")
}
/// The carry-forward's worked case: the child snapshot is the settled parent
/// chain PLUS the child's final writes. A written slot reads its BAL value, an
/// unwritten slot reads the carried parent value — neither costs a storage RPC
/// — while a slot neither source knows stays on the ordinary batch path and
/// reads the CHILD block's chain value.
#[tokio::test]
async fn carry_forward_serves_parent_values_and_child_writes_without_storage_rpc() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    // Chain truth pins y's child value DIFFERENTLY from the parent's, so a
    // leaked dynamic fetch (or a leaked parent value) cannot pass by accident.
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    stub.serve_at(HEIGHT, address(0x11), U256::from(2u64), 2);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(2u64), 99);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(3u64), 77);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_writes_x3());
    let cache = ReadHubCache::new(&provider);

    let parent = cache.for_anchor(ReadAnchor::identified(HEIGHT, parent_hash));
    let x = parent.read_storage(address(0x11), U256::from(1u64));
    let y = parent.read_storage(address(0x11), U256::from(2u64));
    parent.drain_round().await;
    assert_eq!(
        x.await.unwrap(),
        StorageValue::from(1u64),
        "the parent's chain value"
    );
    assert_eq!(
        y.await.unwrap(),
        StorageValue::from(2u64),
        "the parent's chain value"
    );
    stub.clear();

    let (child, report) = cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, child_hash),
            parent_hash,
            &[],
        )
        .await;

    assert_eq!(
        report.outcome,
        CarryOutcome::Carried { carried_values: 2 },
        "{report:?}"
    );
    assert_eq!(
        report.writes,
        vec![(address(0x11), U256::from(1u64))],
        "one final write applied: {report:?}"
    );
    let x_at_child = child.read_storage(address(0x11), U256::from(1u64));
    let y_at_child = child.read_storage(address(0x11), U256::from(2u64));
    assert_eq!(
        x_at_child.await.unwrap(),
        StorageValue::from(3u64),
        "the child's final write wins over the carried parent value"
    );
    assert_eq!(
        y_at_child.await.unwrap(),
        StorageValue::from(2u64),
        "the carried parent value — not the child chain's 99"
    );
    assert_eq!(
        stub.storage_members(),
        0,
        "neither read costs a storage RPC: {:?}",
        stub.packets()
    );
    let z = child.read_storage(address(0x11), U256::from(3u64));
    let round = child.drain_round().await;
    assert_eq!(
        round.storage_members, 1,
        "the unknown slot is one existing batch member: {round:?}"
    );
    assert_eq!(
        z.await.unwrap(),
        StorageValue::from(77u64),
        "the child chain's value through the ordinary path"
    );
    assert_eq!(stub.storage_members(), 1, "{:?}", stub.packets());
}
/// The carry applies EVERY final write with overwrite semantics: the FINAL of
/// a slot's several writes replaces the carried parent value, and a zero
/// (deletion) final replaces it too.
#[tokio::test]
async fn carry_forward_final_writes_and_zero_deletions_overwrite_parent_values() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    stub.serve_at(HEIGHT, address(0x11), U256::from(2u64), 2);
    // Chain truth at the child agrees with the BAL's finals (0 and 42).
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(1u64), 0);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(2u64), 42);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_writes_zero_and_final());
    let cache = ReadHubCache::new(&provider);

    let parent = cache.for_anchor(ReadAnchor::identified(HEIGHT, parent_hash));
    let x = parent.read_storage(address(0x11), U256::from(1u64));
    let y = parent.read_storage(address(0x11), U256::from(2u64));
    parent.drain_round().await;
    assert_eq!(x.await.unwrap(), StorageValue::from(1u64));
    assert_eq!(y.await.unwrap(), StorageValue::from(2u64));
    stub.clear();

    let (child, report) = cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, child_hash),
            parent_hash,
            &[],
        )
        .await;

    assert_eq!(
        report.outcome,
        CarryOutcome::Carried { carried_values: 2 },
        "{report:?}"
    );
    assert_eq!(
        report.writes,
        vec![
            (address(0x11), U256::from(1u64)),
            (address(0x11), U256::from(2u64))
        ],
        "both final writes applied: {report:?}"
    );
    let x_at_child = child.read_storage(address(0x11), U256::from(1u64));
    let y_at_child = child.read_storage(address(0x11), U256::from(2u64));
    assert_eq!(
        x_at_child.await.unwrap(),
        StorageValue::from(0u64),
        "the zero (deletion) final overwrites the carried 1"
    );
    assert_eq!(
        y_at_child.await.unwrap(),
        StorageValue::from(42u64),
        "the FINAL of several writes overwrites the carried 2"
    );
    assert_eq!(
        stub.storage_members(),
        0,
        "both reads are snapshot hits: {:?}",
        stub.packets()
    );
}
/// Parent and child are independent afterwards: later fetches on either hub
/// never mutate the other, and a parent's queued or in-flight read is neither
/// carried into the child nor stranded at the parent.
#[tokio::test]
async fn carry_forward_isolates_parent_and_child_and_strands_nothing() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    // Chain truth: the same slots read DIFFERENTLY at the two blocks.
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    stub.serve_at(HEIGHT, address(0x11), U256::from(5u64), 44);
    stub.serve_at(HEIGHT, address(0x11), U256::from(6u64), 66);
    stub.serve_at(HEIGHT, address(0x11), U256::from(7u64), 71);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(5u64), 55);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(6u64), 67);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(8u64), 81);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_writes_z5());
    let cache = ReadHubCache::new(&provider);

    let parent = cache.for_anchor(ReadAnchor::identified(HEIGHT, parent_hash));
    // x settles; v is still in flight when the chain moves; w queues behind it.
    let x_settled = parent.read_storage(address(0x11), U256::from(1u64));
    parent.drain_round().await;
    assert_eq!(x_settled.await.unwrap(), StorageValue::from(1u64));
    stub.park();
    let v_in_flight = parent.read_storage(address(0x11), U256::from(6u64));
    let mut drain = Box::pin(parent.drain_round());
    let mut cx = Context::from_waker(Waker::noop());
    assert!(drain.as_mut().poll(&mut cx).is_pending());
    assert!(
        drain.as_mut().poll(&mut cx).is_pending(),
        "the parent's round is in flight"
    );
    let w_queued = parent.read_storage(address(0x11), U256::from(5u64));
    stub.clear();

    let (child, report) = cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, child_hash),
            parent_hash,
            &[address(0x11)],
        )
        .await;

    assert_eq!(
        report.outcome,
        CarryOutcome::Carried { carried_values: 1 },
        "only the settled word crossed: {report:?}"
    );
    assert_eq!(
        report.writes,
        vec![(address(0x11), U256::from(9u64))],
        "{report:?}"
    );
    assert_eq!(
        child.memoized_slots(),
        2,
        "the settled word and the final write — neither pending entry crossed"
    );

    // The child fetches the two unsettled identities itself, at its own block.
    let w_at_child = child.read_storage(address(0x11), U256::from(5u64));
    let v_at_child = child.read_storage(address(0x11), U256::from(6u64));
    stub.release();
    let round = child.drain_round().await;
    assert_eq!(round.storage_members, 2, "{round:?}");
    assert_eq!(
        w_at_child.await.unwrap(),
        StorageValue::from(55u64),
        "the child's own chain value"
    );
    assert_eq!(v_at_child.await.unwrap(), StorageValue::from(67u64));

    // The parent's in-flight round and queued read settle normally — neither
    // was stranded, and each reads the PARENT's chain value.
    let parent_round = drain.await;
    assert_eq!(parent_round.storage_members, 1, "{parent_round:?}");
    assert_eq!(
        v_in_flight.await.unwrap(),
        StorageValue::from(66u64),
        "the parent's own chain value"
    );
    let parent_w_round = parent.drain_round().await;
    assert_eq!(parent_w_round.storage_members, 1, "{parent_w_round:?}");
    assert_eq!(w_queued.await.unwrap(), StorageValue::from(44u64));

    // Later fetches on either hub never mutate the other.
    let parent_memo = parent.memoized_slots();
    let child_memo = child.memoized_slots();
    let u_later = parent.read_storage(address(0x11), U256::from(7u64));
    parent.drain_round().await;
    assert_eq!(u_later.await.unwrap(), StorageValue::from(71u64));
    assert_eq!(
        child.memoized_slots(),
        child_memo,
        "a parent fetch never mutates the child"
    );
    let t_later = child.read_storage(address(0x11), U256::from(8u64));
    child.drain_round().await;
    assert_eq!(t_later.await.unwrap(), StorageValue::from(81u64));
    assert_eq!(
        parent.memoized_slots(),
        parent_memo + 1,
        "a child fetch never mutates the parent"
    );
}
/// The carry is STORAGE-only: account rows are never cloned, and an account
/// read at the child keeps its ordinary one-batch behavior.
#[tokio::test]
async fn carry_forward_never_clones_accounts() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_writes_x3());
    let cache = ReadHubCache::new(&provider);

    let parent = cache.for_anchor(ReadAnchor::identified(HEIGHT, parent_hash));
    let account = parent.read_account(address(0x21));
    let word = parent.read_storage(address(0x11), U256::from(1u64));
    parent.drain_round().await;
    let parent_row = account.await.unwrap();
    assert_eq!(parent_row.nonce, 0x2a);
    assert_eq!(word.await.unwrap(), served_value());
    stub.clear();

    let (child, report) = cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, child_hash),
            parent_hash,
            &[],
        )
        .await;
    assert_eq!(
        report.outcome,
        CarryOutcome::Carried { carried_values: 1 },
        "the storage word crossed, the account row did not: {report:?}"
    );

    let at_child = child.read_account(address(0x21));
    let round = child.drain_round().await;
    assert_eq!(
        round.account_members, 1,
        "the account read is its own fetch at the child: {round:?}"
    );
    assert_eq!(round.account_rpc_members(), 3, "balance + nonce + code");
    let child_row = at_child.await.unwrap();
    assert_eq!(
        child_row, parent_row,
        "the stub serves one row at both blocks"
    );
    let packets = stub.packets();
    assert!(
        packets
            .iter()
            .flatten()
            .all(|(method, _)| method != "eth_getStorageAt"),
        "the carried word never reached the chain: {packets:?}"
    );
    assert_eq!(
        stub.members(),
        4,
        "one BAL fetch plus balance + nonce + code: {packets:?}"
    );

    // The parent's account memo is untouched and still answers free.
    stub.clear();
    let again = parent.read_account(address(0x21));
    let idle = parent.drain_round().await;
    assert!(idle.is_empty(), "{idle:?}");
    assert_eq!(again.await.unwrap(), parent_row);
    assert!(stub.packets().is_empty(), "{:?}", stub.packets());
}
/// A child whose BAL cannot be read — missing method, transport error, a null
/// answer, or malformed RLP — is COLD: the parent's values are never served at
/// it, and every read takes the dynamic path to the CHILD block's chain value.
#[tokio::test]
async fn a_child_bal_failure_returns_a_cold_child_that_never_serves_parent_values() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(1u64), 77);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);

    // (a) the method does not exist at this node; (b) the transport fails;
    // (c) the node answers `null`; (d) the bytes do not decode.
    for (name, mode) in [
        ("unsupported method", 0),
        ("transport error", 1),
        ("null answer", 2),
        ("malformed RLP", 3),
    ] {
        stub.set_bal_unsupported(false);
        stub.set_bal_null(false);
        stub.set_down(false);
        stub.serve_bal(worked_bal());
        let cache = ReadHubCache::new(&provider);
        let parent = cache.for_anchor(ReadAnchor::identified(HEIGHT, parent_hash));
        let x = parent.read_storage(address(0x11), U256::from(1u64));
        parent.drain_round().await;
        assert_eq!(x.await.unwrap(), StorageValue::from(1u64), "{name}");
        stub.clear();

        match mode {
            0 => stub.set_bal_unsupported(true),
            1 => stub.set_down(true),
            2 => stub.set_bal_null(true),
            _ => stub.serve_bal(vec![0xff, 0x00]),
        }
        let (child, report) = cache
            .advance_bal(
                ReadAnchor::identified(HEIGHT + 1, child_hash),
                parent_hash,
                &[],
            )
            .await;

        let (lineage, bal) = match &report.outcome {
            CarryOutcome::Cold { lineage, bal } => (lineage, bal),
            other => panic!("{name}: expected a cold child, got {other:?}"),
        };
        assert!(lineage.is_none(), "{name}: the lineage matched: {report:?}");
        let skip = bal
            .as_ref()
            .unwrap_or_else(|| panic!("{name}: a skip was reported: {report:?}"));
        match mode {
            0..=2 => assert!(matches!(skip, BalSkip::Rpc(_)), "{name}: {skip:?}"),
            _ => assert!(matches!(skip, BalSkip::Decode(_)), "{name}: {skip:?}"),
        }
        assert!(
            report.writes.is_empty(),
            "{name}: nothing seeded: {report:?}"
        );

        // Heal the node, then read: the child pays a dynamic fetch for the
        // CHILD block's value — the parent's 1 must never be served here.
        stub.set_bal_unsupported(false);
        stub.set_bal_null(false);
        stub.set_down(false);
        stub.clear();
        let x_at_child = child.read_storage(address(0x11), U256::from(1u64));
        let round = child.drain_round().await;
        assert_eq!(round.storage_members, 1, "{name}: {round:?}");
        assert_eq!(
            x_at_child.await.unwrap(),
            StorageValue::from(77u64),
            "{name}: the CHILD chain value"
        );

        // The parent keeps its own values, still free.
        stub.clear();
        let again = parent.read_storage(address(0x11), U256::from(1u64));
        let idle = parent.drain_round().await;
        assert!(idle.is_empty(), "{name}: {idle:?}");
        assert_eq!(again.await.unwrap(), StorageValue::from(1u64), "{name}");
        assert!(stub.packets().is_empty(), "{name}: {:?}", stub.packets());
    }
}
/// The chain truth the refusal cases share: the parent block settles x=1, y=2,
/// and the next block's chain reads x=3, y=99.
fn refusal_chain_truth(stub: &ChainStub) {
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    stub.serve_at(HEIGHT, address(0x11), U256::from(2u64), 2);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(1u64), 3);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(2u64), 99);
    stub.serve_at(HEIGHT + 2, address(0x11), U256::from(1u64), 5);
    stub.serve_at(HEIGHT + 2, address(0x11), U256::from(2u64), 98);
}

/// Settle the parent's x and y in a fresh cache; the packet log is clean after.
async fn settled_parent<'p>(
    stub: &ChainStub,
    provider: &'p AlloyProvider,
    parent: ReadAnchor,
) -> (ReadHubCache<'p>, Arc<ReadHub<'p>>) {
    let cache = ReadHubCache::new(provider);
    let hub = cache.for_anchor(parent);
    let x = hub.read_storage(address(0x11), U256::from(1u64));
    let y = hub.read_storage(address(0x11), U256::from(2u64));
    hub.drain_round().await;
    assert_eq!(x.await.unwrap(), StorageValue::from(1u64));
    assert_eq!(y.await.unwrap(), StorageValue::from(2u64));
    stub.clear();
    (cache, hub)
}

/// A refused link's child: x reads its expected value (the seeded final write,
/// or the chain's word when nothing was seeded), y is NOT carried and reads the
/// child chain's word through the ordinary batch, and the seeded variant pays
/// no storage RPC for x.
async fn assert_cold_child(
    stub: &ChainStub,
    child: &ReadHub<'_>,
    write_seeded: bool,
    x_value: u64,
    y_value: u64,
) {
    let x = child.read_storage(address(0x11), U256::from(1u64));
    let y = child.read_storage(address(0x11), U256::from(2u64));
    let round = child.drain_round().await;
    assert_eq!(
        y.await.unwrap(),
        StorageValue::from(y_value),
        "the child chain's value — the parent's 2 must not be served"
    );
    assert_eq!(x.await.unwrap(), StorageValue::from(x_value));
    let dynamic = if write_seeded { 1 } else { 2 };
    assert_eq!(
        round.storage_members, dynamic,
        "only the unseeded reads are dynamic: {round:?}"
    );
    assert_eq!(stub.storage_members(), dynamic, "{:?}", stub.packets());
}

/// The one BAL fetch a refusal case dispatches carries the child's exact hash.
fn assert_bal_requested(stub: &ChainStub, hash: B256) {
    let requests: Vec<_> = stub
        .packets()
        .into_iter()
        .filter(|packet| packet.len() == 1 && packet[0].0 == "debug_getRawBlockAccessList")
        .collect();
    assert_eq!(requests.len(), 1, "one BAL request: {requests:?}");
    assert!(
        requests[0][0].1.contains(&format!("{hash:#x}")),
        "the request carries the child's exact hash: {:?}",
        requests[0]
    );
}
/// Every refused link — a wrong parent hash, a same-height reorg's abandoned
/// block, a nonconsecutive number, a number-only parent, a number-only child,
/// or no parent at all — starts the child COLD: no parent value is served
/// there. The BAL request still carries the child's EXACT hash, and a cold
/// child whose list decoded may still apply the child's own final writes.
#[expect(
    clippy::too_many_lines,
    reason = "the whole refusal matrix side by side is the contract"
)]
#[tokio::test]
async fn refused_links_start_a_cold_child_and_request_the_child_hash_bal() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    refusal_chain_truth(&stub);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_writes_x3());

    // A wrong parent hash (the numbers line up; the hash does not).
    {
        let (cache, parent_hub) = settled_parent(
            &stub,
            &provider,
            ReadAnchor::identified(HEIGHT, parent_hash),
        )
        .await;
        let (child, report) = cache
            .advance_bal(
                ReadAnchor::identified(HEIGHT + 1, child_hash),
                B256::new([0xcc; 32]),
                &[address(0x11)],
            )
            .await;
        assert_eq!(
            report.outcome,
            CarryOutcome::Cold {
                lineage: Some(CarryRefusal::ParentHashMismatch),
                bal: None
            },
            "{report:?}"
        );
        assert_eq!(
            report.writes,
            vec![(address(0x11), U256::from(1u64))],
            "{report:?}"
        );
        assert_bal_requested(&stub, child_hash);
        assert_cold_child(&stub, &child, true, 3, 99).await;
        let x = parent_hub.read_storage(address(0x11), U256::from(1u64));
        let idle = parent_hub.drain_round().await;
        assert!(idle.is_empty(), "{idle:?}");
        assert_eq!(x.await.unwrap(), StorageValue::from(1u64));
    }

    // A same-height reorg: the installed anchor is the REPLACEMENT block, and
    // the child still names the abandoned block as its parent.
    {
        let (cache, _abandoned) = settled_parent(
            &stub,
            &provider,
            ReadAnchor::identified(HEIGHT, parent_hash),
        )
        .await;
        let replacement = cache.for_anchor(ReadAnchor::identified(HEIGHT, B256::new([0xdd; 32])));
        assert_eq!(
            replacement.memoized_slots(),
            0,
            "the replacement starts cold"
        );
        stub.clear();
        let (child, report) = cache
            .advance_bal(
                ReadAnchor::identified(HEIGHT + 1, child_hash),
                parent_hash,
                &[address(0x11)],
            )
            .await;
        assert_eq!(
            report.outcome,
            CarryOutcome::Cold {
                lineage: Some(CarryRefusal::ParentHashMismatch),
                bal: None
            },
            "{report:?}"
        );
        assert_bal_requested(&stub, child_hash);
        assert_cold_child(&stub, &child, true, 3, 99).await;
    }

    // A nonconsecutive number — a gap (and, checked below, a child at the
    // parent's own height) is a refused link even with a matching hash.
    {
        let (cache, _parent) = settled_parent(
            &stub,
            &provider,
            ReadAnchor::identified(HEIGHT, parent_hash),
        )
        .await;
        stub.serve_bal_at(B256::new([0xee; 32]), bal_empty());
        let (child, report) = cache
            .advance_bal(
                ReadAnchor::identified(HEIGHT + 2, B256::new([0xee; 32])),
                B256::new([0xbb; 32]),
                &[address(0x11)],
            )
            .await;
        assert_eq!(
            report.outcome,
            CarryOutcome::Cold {
                lineage: Some(CarryRefusal::NotConsecutive),
                bal: None
            },
            "{report:?}"
        );
        assert!(report.writes.is_empty(), "{report:?}");
        assert_cold_child(&stub, &child, false, 5, 98).await;
    }

    // A child at the parent's OWN height is nonconsecutive too.
    {
        let (cache, _parent) = settled_parent(
            &stub,
            &provider,
            ReadAnchor::identified(HEIGHT, parent_hash),
        )
        .await;
        stub.serve_bal_at(B256::new([0x99; 32]), bal_empty());
        let (at_same_height, report) = cache
            .advance_bal(
                ReadAnchor::identified(HEIGHT, B256::new([0x99; 32])),
                parent_hash,
                &[address(0x11)],
            )
            .await;
        assert_eq!(
            report.outcome,
            CarryOutcome::Cold {
                lineage: Some(CarryRefusal::NotConsecutive),
                bal: None
            },
            "{report:?}"
        );
        assert_eq!(
            at_same_height.memoized_slots(),
            0,
            "the same-height child carries nothing"
        );
    }

    // A number-only parent: its hash cannot verify the child's parent hash, so
    // the link is unverifiable, never assumed.
    {
        let (cache, _parent) = settled_parent(&stub, &provider, ReadAnchor::at(HEIGHT)).await;
        let (child, report) = cache
            .advance_bal(
                ReadAnchor::identified(HEIGHT + 1, child_hash),
                parent_hash,
                &[address(0x11)],
            )
            .await;
        assert_eq!(
            report.outcome,
            CarryOutcome::Cold {
                lineage: Some(CarryRefusal::ParentHashUnknown),
                bal: None
            },
            "{report:?}"
        );
        assert_bal_requested(&stub, child_hash);
        assert_cold_child(&stub, &child, true, 3, 99).await;
    }

    // A number-only child: the lineage matches, but there is no exact hash to
    // read the list at, so the BAL stage steps aside and NOTHING is cloned — a
    // matched link is never carried without the child's list.
    {
        let (cache, _parent) = settled_parent(
            &stub,
            &provider,
            ReadAnchor::identified(HEIGHT, parent_hash),
        )
        .await;
        let (child, report) = cache
            .advance_bal(ReadAnchor::at(HEIGHT + 1), parent_hash, &[address(0x11)])
            .await;
        assert_eq!(
            report.outcome,
            CarryOutcome::Cold {
                lineage: None,
                bal: Some(BalSkip::NoAnchorHash)
            },
            "{report:?}"
        );
        assert!(report.writes.is_empty(), "{report:?}");
        assert_eq!(stub.bal_requests(), 0, "no list can be requested");
        assert_cold_child(&stub, &child, false, 3, 99).await;
    }

    // No parent at all: cold rather than guessed lineage — but the child's own
    // final writes still seed.
    {
        let cache = ReadHubCache::new(&provider);
        stub.clear();
        let (child, report) = cache
            .advance_bal(
                ReadAnchor::identified(HEIGHT + 1, child_hash),
                parent_hash,
                &[address(0x11)],
            )
            .await;
        assert_eq!(
            report.outcome,
            CarryOutcome::Cold {
                lineage: Some(CarryRefusal::NoParent),
                bal: None
            },
            "{report:?}"
        );
        assert_eq!(
            report.writes,
            vec![(address(0x11), U256::from(1u64))],
            "{report:?}"
        );
        assert_bal_requested(&stub, child_hash);
        assert_cold_child(&stub, &child, true, 3, 99).await;
    }
}
/// Multi-generation carry: unchanged values follow the latest SUCCESSFUL state,
/// and a cold generation (its BAL unreadable) breaks the chain — later
/// generations carry only what that generation itself settled and can never
/// resurrect an earlier block's values.
#[expect(
    clippy::too_many_lines,
    reason = "one five-generation chain reads top-to-bottom"
)]
#[tokio::test]
async fn a_cold_generation_breaks_the_chain_and_cannot_resurrect_earlier_values() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    // x, y, z at blocks 100..=104. The COLD block 103 rewrote all three (77, 88,
    // 99) but its list is unreadable, so only a dynamic read can know them.
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    stub.serve_at(HEIGHT, address(0x11), U256::from(2u64), 2);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(1u64), 5);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(2u64), 2);
    stub.serve_at(HEIGHT + 2, address(0x11), U256::from(1u64), 5);
    stub.serve_at(HEIGHT + 2, address(0x11), U256::from(2u64), 2);
    stub.serve_at(HEIGHT + 2, address(0x11), U256::from(9u64), 9);
    stub.serve_at(HEIGHT + 3, address(0x11), U256::from(1u64), 77);
    stub.serve_at(HEIGHT + 3, address(0x11), U256::from(2u64), 88);
    stub.serve_at(HEIGHT + 3, address(0x11), U256::from(9u64), 99);
    stub.serve_at(HEIGHT + 4, address(0x11), U256::from(1u64), 77);
    stub.serve_at(HEIGHT + 4, address(0x11), U256::from(2u64), 88);
    stub.serve_at(HEIGHT + 4, address(0x11), U256::from(9u64), 99);
    let hashes: [B256; 5] = [
        B256::new([0xa0; 32]),
        B256::new([0xa1; 32]),
        B256::new([0xa2; 32]),
        B256::new([0xa3; 32]),
        B256::new([0xa4; 32]),
    ];
    stub.serve_bal_at(hashes[1], bal_writes_x5());
    stub.serve_bal_at(hashes[2], bal_writes_z9());
    stub.serve_bal_at(hashes[3], vec![0xff, 0x00]);
    stub.serve_bal_at(hashes[4], bal_empty());
    let cache = ReadHubCache::new(&provider);

    // A settles x=1, y=2.
    let hub_a = cache.for_anchor(ReadAnchor::identified(HEIGHT, hashes[0]));
    let x = hub_a.read_storage(address(0x11), U256::from(1u64));
    let y = hub_a.read_storage(address(0x11), U256::from(2u64));
    hub_a.drain_round().await;
    assert_eq!(x.await.unwrap(), StorageValue::from(1u64));
    assert_eq!(y.await.unwrap(), StorageValue::from(2u64));
    stub.clear();

    // B: x rewritten to 5; y unchanged and still carried.
    let (hub_b, report) = cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, hashes[1]),
            hashes[0],
            &[address(0x11)],
        )
        .await;
    assert_eq!(
        report.outcome,
        CarryOutcome::Carried { carried_values: 2 },
        "{report:?}"
    );
    let x = hub_b.read_storage(address(0x11), U256::from(1u64));
    let y = hub_b.read_storage(address(0x11), U256::from(2u64));
    assert_eq!(
        x.await.unwrap(),
        StorageValue::from(5u64),
        "B's final write"
    );
    assert_eq!(
        y.await.unwrap(),
        StorageValue::from(2u64),
        "A's value, carried"
    );
    assert_eq!(stub.storage_members(), 0, "{:?}", stub.packets());

    // C: z written to 9; x and y follow the latest successful state (B's).
    let (hub_c, report) = cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 2, hashes[2]),
            hashes[1],
            &[address(0x11)],
        )
        .await;
    assert_eq!(
        report.outcome,
        CarryOutcome::Carried { carried_values: 2 },
        "{report:?}"
    );
    let x = hub_c.read_storage(address(0x11), U256::from(1u64));
    let y = hub_c.read_storage(address(0x11), U256::from(2u64));
    let z = hub_c.read_storage(address(0x11), U256::from(9u64));
    assert_eq!(x.await.unwrap(), StorageValue::from(5u64));
    assert_eq!(
        y.await.unwrap(),
        StorageValue::from(2u64),
        "unchanged, still A's"
    );
    assert_eq!(
        z.await.unwrap(),
        StorageValue::from(9u64),
        "C's final write"
    );
    assert_eq!(stub.storage_members(), 0, "{:?}", stub.packets());

    // D is COLD — block 103 rewrote everything but its list is unreadable.
    let (hub_d, report) = cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 3, hashes[3]),
            hashes[2],
            &[],
        )
        .await;
    assert!(
        matches!(
            report.outcome,
            CarryOutcome::Cold {
                lineage: None,
                bal: Some(BalSkip::Decode(_))
            }
        ),
        "{report:?}"
    );
    let x = hub_d.read_storage(address(0x11), U256::from(1u64));
    let z = hub_d.read_storage(address(0x11), U256::from(9u64));
    let round = hub_d.drain_round().await;
    assert_eq!(
        round.storage_members, 2,
        "a cold child is all dynamic: {round:?}"
    );
    assert_eq!(
        x.await.unwrap(),
        StorageValue::from(77u64),
        "D's chain value — B/C's 5 must not be served"
    );
    assert_eq!(z.await.unwrap(), StorageValue::from(99u64), "not C's 9");

    // E carries ONLY what D settled: x and z follow D's state, and y — read at
    // A, B, C but never at D — is fetched again rather than resurrected.
    let (hub_e, report) = cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 4, hashes[4]),
            hashes[3],
            &[],
        )
        .await;
    assert_eq!(
        report.outcome,
        CarryOutcome::Carried { carried_values: 2 },
        "{report:?}"
    );
    stub.clear();
    let x = hub_e.read_storage(address(0x11), U256::from(1u64));
    let z = hub_e.read_storage(address(0x11), U256::from(9u64));
    let y = hub_e.read_storage(address(0x11), U256::from(2u64));
    let round = hub_e.drain_round().await;
    assert_eq!(
        x.await.unwrap(),
        StorageValue::from(77u64),
        "D's value, carried"
    );
    assert_eq!(
        z.await.unwrap(),
        StorageValue::from(99u64),
        "D's value, carried"
    );
    assert_eq!(
        y.await.unwrap(),
        StorageValue::from(88u64),
        "the chain's value — the cold break must not resurrect A's 2"
    );
    assert_eq!(round.storage_members, 1, "only y is dynamic: {round:?}");
    assert_eq!(stub.storage_members(), 1, "{:?}", stub.packets());
}
/// A same-anchor advance is idempotent: the installed hub comes back unchanged
/// and no BAL work is repeated.
#[tokio::test]
async fn same_anchor_advance_is_idempotent_and_repeats_no_bal_work() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_writes_x3());
    let cache = ReadHubCache::new(&provider);
    let parent = cache.for_anchor(ReadAnchor::identified(HEIGHT, parent_hash));
    let x = parent.read_storage(address(0x11), U256::from(1u64));
    parent.drain_round().await;
    assert_eq!(x.await.unwrap(), served_value());
    stub.clear();

    let (child, report) = cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, child_hash),
            parent_hash,
            &[],
        )
        .await;
    assert_eq!(
        report.outcome,
        CarryOutcome::Carried { carried_values: 1 },
        "{report:?}"
    );
    assert_eq!(stub.bal_requests(), 1, "one BAL fetch");

    let (again, report) = cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, child_hash),
            parent_hash,
            &[],
        )
        .await;
    assert!(
        Arc::ptr_eq(&child, &again),
        "the same anchor hands back the installed hub"
    );
    assert_eq!(report.outcome, CarryOutcome::AlreadyCurrent, "{report:?}");
    assert!(report.writes.is_empty(), "no writes re-applied: {report:?}");
    assert!(report.bal_latency.is_zero(), "no BAL work: {report:?}");
    assert!(report.clone_time.is_zero(), "no clone: {report:?}");
    assert_eq!(
        stub.bal_requests(),
        1,
        "the idempotent call repeated no BAL work: {:?}",
        stub.packets()
    );
    assert_eq!(
        child.memoized_slots(),
        1,
        "the snapshot is unchanged (the one word, overwritten by its final write)"
    );
    let via_for_anchor = cache.for_anchor(ReadAnchor::identified(HEIGHT + 1, child_hash));
    assert!(
        Arc::ptr_eq(&child, &via_for_anchor),
        "for_anchor agrees on the installed anchor"
    );
    let x = child.read_storage(address(0x11), U256::from(1u64));
    let idle = child.drain_round().await;
    assert!(idle.is_empty(), "{idle:?}");
    assert_eq!(x.await.unwrap(), StorageValue::from(3u64));
}
/// Multi-block values match an independently served COLD hub at every anchor,
/// not just expectations derived from the chain that produced them.
#[tokio::test]
async fn carry_forward_matches_an_independent_cold_hub_across_blocks() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    // Chain truth, consistent with the BALs: 101 writes slots 1 and 4, 102
    // writes slot 2, and slot 3 is never written.
    for (height, x, y, z, w) in [
        (HEIGHT, 11, 22, 33, 44),
        (HEIGHT + 1, 111, 22, 33, 444),
        (HEIGHT + 2, 111, 222, 33, 444),
    ] {
        stub.serve_at(height, address(0x11), U256::from(1u64), x);
        stub.serve_at(height, address(0x11), U256::from(2u64), y);
        stub.serve_at(height, address(0x11), U256::from(3u64), z);
        stub.serve_at(height, address(0x11), U256::from(4u64), w);
    }
    let hashes: [B256; 3] = [
        B256::new([0xa0; 32]),
        B256::new([0xa1; 32]),
        B256::new([0xa2; 32]),
    ];
    stub.serve_bal_at(hashes[1], bal_parity_writes());
    stub.serve_bal_at(hashes[2], bal_parity_writes_2());
    let cache = ReadHubCache::new(&provider);
    let slots = [1u64, 2, 3, 4].map(U256::from);

    // The chain's first block settles everything it reads.
    let hub_a = cache.for_anchor(ReadAnchor::identified(HEIGHT, hashes[0]));
    let first: Vec<_> = slots
        .iter()
        .map(|slot| hub_a.read_storage(address(0x11), *slot))
        .collect();
    hub_a.drain_round().await;
    for read in first {
        read.await.unwrap();
    }

    for (offset, height) in [(1usize, HEIGHT + 1), (2, HEIGHT + 2)] {
        let anchor = ReadAnchor::identified(height, hashes[offset]);
        let (hub, report) = cache.advance_bal(anchor, hashes[offset - 1], &[]).await;
        assert!(
            matches!(report.outcome, CarryOutcome::Carried { .. }),
            "{report:?}"
        );
        stub.clear();
        let carried: Vec<_> = slots
            .iter()
            .map(|slot| hub.read_storage(address(0x11), *slot))
            .collect();
        let mut carried_values = Vec::new();
        for read in carried {
            carried_values.push(read.await.unwrap());
        }
        assert_eq!(
            stub.storage_members(),
            0,
            "every value is a snapshot hit at {height}: {:?}",
            stub.packets()
        );

        // The same anchor, served COLD and independently: the values must agree.
        let cold = ReadHub::new(&provider, anchor);
        let reads: Vec<_> = slots
            .iter()
            .map(|slot| cold.read_storage(address(0x11), *slot))
            .collect();
        cold.drain_round().await;
        let mut cold_values = Vec::new();
        for read in reads {
            cold_values.push(read.await.unwrap());
        }
        assert_eq!(
            carried_values, cold_values,
            "the carried snapshot must match the cold chain at {height}"
        );
    }
}
/// The transition clones the parent its lineage check VALIDATED. A parent that
/// installs while the child's list is in flight replaces the chain head, and
/// the child must carry THAT parent's values — never a pre-fetch hub's, which
/// the check no longer vouches for.
#[tokio::test]
async fn carry_forward_clones_the_parent_its_lineage_check_validated() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    // The stale block 99 settles x=1; the real block 100 settles y=2; at the
    // child's block 101 the chain reads x=77, y=99.
    stub.serve_at(HEIGHT - 1, address(0x11), U256::from(1u64), 1);
    stub.serve_at(HEIGHT, address(0x11), U256::from(2u64), 2);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(1u64), 77);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(2u64), 99);
    let stale_hash = B256::new([0x99; 32]);
    let real_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_empty());
    let cache = ReadHubCache::new(&provider);

    let stale = cache.for_anchor(ReadAnchor::identified(HEIGHT - 1, stale_hash));
    let x = stale.read_storage(address(0x11), U256::from(1u64));
    stale.drain_round().await;
    assert_eq!(x.await.unwrap(), StorageValue::from(1u64));
    stub.clear();

    // The advance parks inside the child's list fetch ...
    stub.park_bal();
    let mut advance = Box::pin(cache.advance_bal(
        ReadAnchor::identified(HEIGHT + 1, child_hash),
        real_hash,
        &[],
    ));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(advance.as_mut().poll(&mut cx).is_pending());
    assert!(
        advance.as_mut().poll(&mut cx).is_pending(),
        "the advance is parked inside the BAL fetch"
    );
    assert_eq!(stub.bal_requests(), 1, "the fetch reached the chain");

    // ... and while it is, the chain's REAL parent installs and settles y.
    let real = cache.for_anchor(ReadAnchor::identified(HEIGHT, real_hash));
    let y = real.read_storage(address(0x11), U256::from(2u64));
    real.drain_round().await;
    assert_eq!(y.await.unwrap(), StorageValue::from(2u64));
    stub.clear();

    stub.release();
    let (child, report) = advance.await;

    assert_eq!(
        report.outcome,
        CarryOutcome::Carried { carried_values: 1 },
        "the validated parent's one settled word: {report:?}"
    );
    let y_at_child = child.read_storage(address(0x11), U256::from(2u64));
    let x_at_child = child.read_storage(address(0x11), U256::from(1u64));
    let round = child.drain_round().await;
    assert_eq!(
        y_at_child.await.unwrap(),
        StorageValue::from(2u64),
        "the REAL parent's value, carried"
    );
    assert_eq!(
        x_at_child.await.unwrap(),
        StorageValue::from(77u64),
        "the stale hub's 1 must not travel — x reads the child chain's value"
    );
    assert_eq!(round.storage_members, 1, "only x is dynamic: {round:?}");
}
/// A write to a slot the snapshot already holds OVERWRITES it whatever the
/// relevance set says: a stale carried value outliving the block that changed
/// it is the one outcome the carry must never produce, so relevance may add
/// entries but never veto updates.
#[tokio::test]
async fn carry_forward_overwrites_known_keys_regardless_of_relevance() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    // The child's chain reads x=5 — the BAL's final write agrees with it.
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(1u64), 5);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_writes_x5());
    let cache = ReadHubCache::new(&provider);

    let parent = cache.for_anchor(ReadAnchor::identified(HEIGHT, parent_hash));
    let x = parent.read_storage(address(0x11), U256::from(1u64));
    parent.drain_round().await;
    assert_eq!(x.await.unwrap(), StorageValue::from(1u64));
    stub.clear();

    // The write's ADDRESS is not in the relevance set — the known key must
    // take the update anyway.
    let (child, report) = cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, child_hash),
            parent_hash,
            &[address(0x22)],
        )
        .await;

    assert_eq!(
        report.writes,
        vec![(address(0x11), U256::from(1u64))],
        "{report:?}"
    );
    assert_eq!(report.writes_overwritten, 1, "{report:?}");
    assert_eq!(report.writes_skipped, 0, "{report:?}");
    let x_at_child = child.read_storage(address(0x11), U256::from(1u64));
    assert_eq!(
        x_at_child.await.unwrap(),
        StorageValue::from(5u64),
        "the known key took the child's final write"
    );
    assert_eq!(stub.storage_members(), 0, "{:?}", stub.packets());
}
/// An unknown-slot write whose address is outside the relevance set is left
/// UNKNOWN — not memoized, counted, and a later read fetches the chain's value
/// through the ordinary path. A COLD child (no carried values) follows the
/// same new-write rule.
#[tokio::test]
async fn carry_forward_leaves_irrelevant_unknown_writes_for_dynamic_fallback() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    // The child's BAL writes slot 9 -> 5; its chain agrees (a real node's BAL
    // write is the chain's post-block word).
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(9u64), 5);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_writes_z5());

    // Once linked, once cold: the new-write rule is the same.
    for cold in [false, true] {
        let cache = ReadHubCache::new(&provider);
        let parent = cache.for_anchor(ReadAnchor::identified(HEIGHT, parent_hash));
        let x = parent.read_storage(address(0x11), U256::from(1u64));
        parent.drain_round().await;
        assert_eq!(x.await.unwrap(), StorageValue::from(1u64));
        stub.clear();

        let supplied = if cold {
            B256::new([0xcc; 32])
        } else {
            parent_hash
        };
        let (child, report) = cache
            .advance_bal(
                ReadAnchor::identified(HEIGHT + 1, child_hash),
                supplied,
                &[address(0x22)],
            )
            .await;

        if cold {
            assert!(
                matches!(
                    report.outcome,
                    CarryOutcome::Cold {
                        lineage: Some(CarryRefusal::ParentHashMismatch),
                        bal: None
                    }
                ),
                "{report:?}"
            );
        } else {
            assert_eq!(
                report.outcome,
                CarryOutcome::Carried { carried_values: 1 },
                "{report:?}"
            );
        }
        assert!(
            report.writes.is_empty(),
            "irrelevant AND unknown: {report:?}"
        );
        assert_eq!(report.writes_overwritten, 0, "{report:?}");
        assert_eq!(
            report.writes_skipped, 1,
            "counted, never hidden: {report:?}"
        );
        assert_eq!(
            child.memoized_slots(),
            usize::from(!cold),
            "the skipped write memoizes nothing"
        );
        let z = child.read_storage(address(0x11), U256::from(9u64));
        let round = child.drain_round().await;
        assert_eq!(
            round.storage_members, 1,
            "the skipped write is dynamic: {round:?}"
        );
        assert_eq!(
            z.await.unwrap(),
            StorageValue::from(5u64),
            "fetched correctly through the ordinary path"
        );
    }
}
/// An unknown-slot write whose address IS in the relevance set seeds an
/// immediate hit, and the retention split shows it as BAL-only until a
/// candidate reads it.
#[tokio::test]
async fn carry_forward_seeds_relevant_unknown_writes_as_immediate_hits() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(9u64), 5);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_writes_z5());
    let cache = ReadHubCache::new(&provider);

    let parent = cache.for_anchor(ReadAnchor::identified(HEIGHT, parent_hash));
    let x = parent.read_storage(address(0x11), U256::from(1u64));
    parent.drain_round().await;
    assert_eq!(x.await.unwrap(), StorageValue::from(1u64));
    stub.clear();

    let (child, report) = cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, child_hash),
            parent_hash,
            &[address(0x11)],
        )
        .await;

    assert_eq!(
        report.writes,
        vec![(address(0x11), U256::from(9u64))],
        "{report:?}"
    );
    assert_eq!(report.writes_overwritten, 0, "{report:?}");
    assert_eq!(report.writes_skipped, 0, "{report:?}");
    assert_eq!(
        child.memoized_slots(),
        2,
        "the carried word plus the seeded write"
    );
    assert_eq!(
        child.snapshot_breakdown(),
        SnapshotRetention {
            working_set: 1,
            bal_only: 1
        },
        "slot 9 is BAL-only retention until a candidate reads it"
    );

    let z = child.read_storage(address(0x11), U256::from(9u64));
    let idle = child.drain_round().await;
    assert!(idle.is_empty(), "the seeded write answers it: {idle:?}");
    assert_eq!(z.await.unwrap(), StorageValue::from(5u64));
    assert_eq!(
        child.snapshot_breakdown(),
        SnapshotRetention {
            working_set: 2,
            bal_only: 0
        },
    );
}
/// A worked BAL seeds each written slot's FINAL value (an earlier change said
/// 7 / 1000), queues the read-only slots into ONE existing storage batch, and
/// every later read of every touched slot is a memo hit. The seeded values are
/// the plain hub's answers for the same anchor.
#[tokio::test]
async fn worked_bal_seeds_final_write_values_and_one_batch_for_read_only_slots() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_bal(worked_bal());
    let anchor = ReadAnchor::identified(HEIGHT, B256::new([0xaa; 32]));
    let hub = ReadHub::new(&provider, anchor);
    let report = hub.prefetch_bal(&BalPlan::default()).await;
    stub.clear();
    assert!(report.skip.is_none(), "the stage ran: {report:?}");
    assert_eq!(report.seeded.writes_seeded, 2, "{report:?}");
    assert_eq!(report.seeded.reads_queued, 2, "{report:?}");
    assert_eq!(
        hub.memoized_slots(),
        2,
        "only final write values are memoized"
    );
    let first = hub.read_storage(address(0x11), U256::from(1u64));
    let second = hub.read_storage(address(0x11), U256::from(2u64));
    assert_eq!(
        first.await.unwrap(),
        StorageValue::from(42u64),
        "final write wins"
    );
    assert_eq!(
        second.await.unwrap(),
        StorageValue::from(42u64),
        "final write wins"
    );
    assert!(
        stub.packets().is_empty(),
        "seeded writes never reach the chain: {:?}",
        stub.packets()
    );
    let third = hub.read_storage(address(0x11), U256::from(3u64));
    let fourth = hub.read_storage(address(0x11), U256::from(4u64));
    let round = hub.drain_round().await;
    assert_eq!(
        round.storage_members, 2,
        "read-only slots join one round: {round:?}"
    );
    assert_eq!(third.await.unwrap(), served_value());
    assert_eq!(fourth.await.unwrap(), served_value());
    let packets = stub.packets();
    assert_eq!(packets.len(), 1, "ONE existing storage batch: {packets:?}");
    assert_eq!(
        packets[0].len(),
        2,
        "one member per read-only slot: {:?}",
        packets[0]
    );
    stub.clear();
    let plain = ReadHub::new(&provider, anchor);
    let plain_first = plain.read_storage(address(0x11), U256::from(1u64));
    plain.drain_round().await;
    assert_eq!(
        plain_first.await.unwrap(),
        StorageValue::from(42u64),
        "the seeded value is the plain hub's answer"
    );
    stub.clear();
    let memo_first = hub.read_storage(address(0x11), U256::from(1u64));
    let memo_third = hub.read_storage(address(0x11), U256::from(3u64));
    let idle = hub.drain_round().await;
    assert!(idle.is_empty(), "nothing was queued: {idle:?}");
    assert_eq!(memo_first.await.unwrap(), StorageValue::from(42u64));
    assert_eq!(memo_third.await.unwrap(), served_value());
    assert!(
        stub.packets().is_empty(),
        "later reads are memo hits: {:?}",
        stub.packets()
    );
}
/// A node that does not serve the method (or a transport failure) is a BAL
/// stage SKIP: nothing is seeded, and ordinary hub reads still settle.
#[tokio::test]
async fn bal_stage_skips_unsupported_method_and_transport_error_and_reads_still_settle() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_bal(worked_bal());
    stub.set_bal_unsupported(true);
    let anchor = ReadAnchor::identified(HEIGHT, B256::new([0xaa; 32]));
    let hub = ReadHub::new(&provider, anchor);
    let report = hub.prefetch_bal(&BalPlan::default()).await;
    assert!(matches!(report.skip, Some(BalSkip::Rpc(_))), "{report:?}");
    assert_eq!(report.seeded.writes_seeded, 0, "{report:?}");
    assert_eq!(report.seeded.reads_queued, 0, "{report:?}");
    assert_eq!(hub.memoized_slots(), 0, "a skip seeds nothing");
    stub.set_bal_unsupported(false);
    stub.set_down(true);
    let report = hub.prefetch_bal(&BalPlan::default()).await;
    assert!(matches!(report.skip, Some(BalSkip::Rpc(_))), "{report:?}");
    assert_eq!(hub.memoized_slots(), 0, "a transport failure seeds nothing");
    stub.set_down(false);
    stub.clear();
    let read = hub.read_storage(address(0x11), slot());
    let round = hub.drain_round().await;
    assert_eq!(round.storage_members, 1, "{round:?}");
    assert_eq!(
        read.await.unwrap(),
        served_value(),
        "ordinary reads still settle"
    );
}
/// Malformed RLP seeds NOTHING — the whole input is rejected, valid prefix
/// included — and the ordinary dynamic path still answers every slot.
#[tokio::test]
async fn malformed_bal_bytes_seed_nothing_and_the_plain_path_works() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let anchor = ReadAnchor::identified(HEIGHT, B256::new([0xaa; 32]));
    let hub = ReadHub::new(&provider, anchor);
    stub.serve_bal(worked_bal_with_trailing_junk());
    let report = hub.prefetch_bal(&BalPlan::default()).await;
    assert!(
        matches!(report.skip, Some(BalSkip::Decode(_))),
        "{report:?}"
    );
    assert_eq!(
        hub.memoized_slots(),
        0,
        "the valid prefix must not partially seed"
    );
    let idle = hub.drain_round().await;
    assert!(
        idle.is_empty(),
        "the valid prefix must not queue reads: {idle:?}"
    );
    stub.serve_bal(vec![0xff, 0x00]);
    let report = hub.prefetch_bal(&BalPlan::default()).await;
    assert!(
        matches!(report.skip, Some(BalSkip::Decode(_))),
        "{report:?}"
    );
    assert_eq!(hub.memoized_slots(), 0);
    stub.clear();
    let read = hub.read_storage(address(0x11), U256::from(1u64));
    let round = hub.drain_round().await;
    assert_eq!(round.storage_members, 1, "{round:?}");
    assert_eq!(
        read.await.unwrap(),
        served_value(),
        "the fallback path answers"
    );
}
/// Entries the plan filters or caps are omitted from the assist and every one
/// of their slots follows the normal dynamic read path with identical values.
#[tokio::test]
async fn filtered_and_capped_bal_entries_fall_back_to_the_dynamic_path() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_bal(worked_bal());
    let anchor = ReadAnchor::identified(HEIGHT, B256::new([0xaa; 32]));
    let hub = ReadHub::new(&provider, anchor);
    let filtered = BalPlan {
        addresses: Some(vec![address(0x22)]),
        ..BalPlan::default()
    };
    let report = hub.prefetch_bal(&filtered).await;
    assert!(report.skip.is_none(), "{report:?}");
    assert_eq!(
        report.filtered_entries, 4,
        "the whole account's entries: {report:?}"
    );
    assert_eq!(report.seeded.writes_seeded, 0, "{report:?}");
    assert_eq!(report.seeded.reads_queued, 0, "{report:?}");
    let reads = [
        hub.read_storage(address(0x11), U256::from(1u64)),
        hub.read_storage(address(0x11), U256::from(2u64)),
        hub.read_storage(address(0x11), U256::from(3u64)),
        hub.read_storage(address(0x11), U256::from(4u64)),
    ];
    let round = hub.drain_round().await;
    assert_eq!(
        round.storage_members, 4,
        "every omitted slot is dynamic: {round:?}"
    );
    for read in reads {
        assert_eq!(read.await.unwrap(), served_value(), "identical values");
    }
    let capped_hub = ReadHub::new(&provider, anchor);
    let capped = BalPlan {
        max_write_seeds: Some(1),
        max_read_prefetches: Some(1),
        ..BalPlan::default()
    };
    let report = capped_hub.prefetch_bal(&capped).await;
    assert!(report.skip.is_none(), "{report:?}");
    assert_eq!(report.seeded.writes_seeded, 1, "{report:?}");
    assert_eq!(report.seeded.reads_queued, 1, "{report:?}");
    assert_eq!(
        report.capped_entries, 2,
        "one write and one read dropped: {report:?}"
    );
    stub.clear();
    let seeded = capped_hub.read_storage(address(0x11), U256::from(1u64));
    let mut seeded = Box::pin(seeded);
    let mut cx = Context::from_waker(Waker::noop());
    assert!(
        matches!(seeded.as_mut().poll(&mut cx), Poll::Ready(Ok(value)) if value == StorageValue::from(42u64)),
        "the capped seed is a memo hit"
    );
    assert!(stub.packets().is_empty(), "{:?}", stub.packets());
    let dropped_write = capped_hub.read_storage(address(0x11), U256::from(2u64));
    let dropped_read = capped_hub.read_storage(address(0x11), U256::from(4u64));
    let round = capped_hub.drain_round().await;
    assert_eq!(
        round.storage_members, 3,
        "the queued read plus the two capped-away slots: {round:?}"
    );
    assert_eq!(
        dropped_write.await.unwrap(),
        served_value(),
        "identical values"
    );
    assert_eq!(
        dropped_read.await.unwrap(),
        served_value(),
        "identical values"
    );
}
/// The BAL request is exact-hash anchored (a number-only anchor cannot make
/// it), and a seeded value is reachable only from its own anchor's hub.
#[tokio::test]
async fn bal_seeding_is_exact_hash_anchored_and_never_crosses_anchors() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_bal(worked_bal());
    let unanchored = ReadHub::new(&provider, ReadAnchor::at(HEIGHT));
    let report = unanchored.prefetch_bal(&BalPlan::default()).await;
    assert_eq!(report.skip, Some(BalSkip::NoAnchorHash), "{report:?}");
    assert!(
        stub.packets().is_empty(),
        "no request was made: {:?}",
        stub.packets()
    );
    assert_eq!(unanchored.memoized_slots(), 0);
    let cache = ReadHubCache::new(&provider);
    let abandoned = cache.for_anchor(ReadAnchor::identified(HEIGHT, B256::new([0xaa; 32])));
    let report = abandoned.prefetch_bal(&BalPlan::default()).await;
    assert!(report.skip.is_none(), "{report:?}");
    assert_eq!(report.seeded.writes_seeded, 2, "{report:?}");
    let packets = stub.packets();
    assert_eq!(packets.len(), 1, "one exact-hash request: {packets:?}");
    assert!(
        packets[0][0]
            .1
            .contains(&format!("{:#x}", B256::new([0xaa; 32]))),
        "the request carries this anchor's hash: {:?}",
        packets[0]
    );
    let on_abandoned = abandoned.read_storage(address(0x11), U256::from(1u64));
    assert_eq!(on_abandoned.await.unwrap(), StorageValue::from(42u64));
    let replacement = cache.for_anchor(ReadAnchor::identified(HEIGHT, B256::new([0xbb; 32])));
    assert!(
        !Arc::ptr_eq(&abandoned, &replacement),
        "same height, different hash"
    );
    stub.clear();
    let on_replacement = replacement.read_storage(address(0x11), U256::from(1u64));
    let round = replacement.drain_round().await;
    assert_eq!(
        round.storage_members, 1,
        "the replacement reads its own block: {round:?}"
    );
    assert_eq!(on_replacement.await.unwrap(), served_value());
    assert_eq!(
        replacement.memoized_slots(),
        1,
        "no value crossed from the abandoned hub"
    );
    assert_eq!(
        abandoned.memoized_slots(),
        2,
        "the abandoned hub keeps its own values"
    );
}
/// Seeding races a QUEUED read: the seeded final value settles its waiter on
/// the spot, the queue entry is consumed exactly once, and no read is lost.
#[tokio::test]
async fn bal_seeding_settles_a_queued_read_without_stranding_its_waiter() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_bal(worked_bal());
    let anchor = ReadAnchor::identified(HEIGHT, B256::new([0xaa; 32]));
    let hub = ReadHub::new(&provider, anchor);
    let queued = hub.read_storage(address(0x11), U256::from(1u64));
    let report = hub.prefetch_bal(&BalPlan::default()).await;
    stub.clear();
    assert!(report.skip.is_none(), "{report:?}");
    assert_eq!(report.seeded.writes_seeded, 2, "{report:?}");
    let mut queued = Box::pin(queued);
    let mut cx = Context::from_waker(Waker::noop());
    assert!(
        matches!(queued.as_mut().poll(&mut cx), Poll::Ready(Ok(value)) if value == StorageValue::from(42u64)),
        "the seed settles the queued waiter itself"
    );
    assert!(stub.packets().is_empty(), "{:?}", stub.packets());
    let read_only = hub.read_storage(address(0x11), U256::from(3u64));
    let round = hub.drain_round().await;
    assert_eq!(
        round.storage_members, 2,
        "the two read-only slots, once each: {round:?}"
    );
    assert_eq!(read_only.await.unwrap(), served_value());
    assert_eq!(
        hub.stats().storage_members,
        2,
        "no entry lost, none duplicated"
    );
}
/// Seeding races an IN-FLIGHT read: the round owns its identity and settles it
/// untouched, its waiter is not stranded, and the settled value is memoized.
#[tokio::test]
async fn bal_seeding_never_disturbs_an_in_flight_round() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_bal(worked_bal());
    let anchor = ReadAnchor::identified(HEIGHT, B256::new([0xaa; 32]));
    let hub = ReadHub::new(&provider, anchor);
    stub.park();
    let in_flight = hub.read_storage(address(0x11), U256::from(1u64));
    let mut drain = Box::pin(hub.drain_round());
    let mut cx = Context::from_waker(Waker::noop());
    assert!(drain.as_mut().poll(&mut cx).is_pending());
    assert!(
        drain.as_mut().poll(&mut cx).is_pending(),
        "the round is in flight"
    );
    assert_eq!(stub.members(), 1, "the round reached the chain");
    let report = hub.prefetch_bal(&BalPlan::default()).await;
    assert!(report.skip.is_none(), "{report:?}");
    assert_eq!(report.seeded.writes_ignored_in_flight, 1, "{report:?}");
    assert_eq!(
        report.seeded.writes_seeded, 1,
        "only the free slot is seeded: {report:?}"
    );
    stub.release();
    let round = drain.await;
    assert_eq!(
        round.storage_members, 1,
        "the round settles its own member: {round:?}"
    );
    assert_eq!(
        in_flight.await.unwrap(),
        served_value(),
        "the waiter is not stranded"
    );
    assert_eq!(hub.stats().rounds, 1, "no round lost or duplicated");
    stub.clear();
    let later = hub.read_storage(address(0x11), U256::from(1u64));
    let mut later = Box::pin(later);
    assert!(
        matches!(later.as_mut().poll(&mut cx), Poll::Ready(Ok(value)) if value == served_value()),
        "the round's settle memoized the value"
    );
    assert!(stub.packets().is_empty(), "{:?}", stub.packets());
}
