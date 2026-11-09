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

use alloy::primitives::{Address, StorageValue, B256, U256};
use alloy::providers::RootProvider;
use alloy::rpc::client::ClientBuilder;
use alloy::rpc::json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{TransportError, TransportErrorKind, TransportFut};
use degenbot_core::errors::ProviderError;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::read_hub::{ReadHub, ReadHubCache, ReadHubError};
use degenbot_simulation::sim::ReadAnchor;
use serde_json::value::to_raw_value;
use serde_json::Value;
use std::borrow::Cow;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use tokio::sync::Notify;

/// One dispatched packet, recorded as its `(method, params)` members — one
/// entry per transport `call`, so a JSON-RPC batch is ONE entry.
type Packet = Vec<(String, String)>;

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
    /// The release signal parked answers wait on.
    gate: Arc<Notify>,
}

impl ChainStub {
    fn serving() -> Self {
        Self {
            packets: Arc::new(Mutex::new(Vec::new())),
            down: Arc::new(AtomicBool::new(false)),
            empty_accounts: Arc::new(AtomicBool::new(false)),
            refusing: Arc::new(AtomicBool::new(true)),
            parked: Arc::new(AtomicBool::new(false)),
            gate: Arc::new(Notify::new()),
        }
    }

    fn set_down(&self, down: bool) {
        self.down.store(down, Ordering::Relaxed);
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

    /// Release parked answers and stop parking new ones.
    fn release(&self) {
        self.parked.store(false, Ordering::Relaxed);
        self.gate.notify_waiters();
    }

    fn packets(&self) -> Vec<Packet> {
        self.packets.lock().unwrap().clone()
    }

    fn members(&self) -> usize {
        self.packets().iter().map(Vec::len).sum()
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
        let result = if self.empty_accounts.load(Ordering::Relaxed)
            && req.method() != "eth_getStorageAt"
        {
            match req.method() {
                "eth_getBalance" | "eth_getTransactionCount" => serde_json::json!("0x0"),
                "eth_getCode" => serde_json::json!("0x"),
                other => panic!("a hub read must not issue {other}"),
            }
        } else {
            match req.method() {
                "eth_getStorageAt" | "eth_getBalance" => serde_json::json!(format!("{SERVED:#x}")),
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

    /// Whether this member belongs to the refused address.
    fn refused(req: &SerializedRequest) -> bool {
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
                RequestPacket::Single(one) => vec![(one.method().to_string(), String::new())],
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
            if stub.parked.load(Ordering::Relaxed) {
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
