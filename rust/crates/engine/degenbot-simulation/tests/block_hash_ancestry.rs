//! BLOCKHASH ancestry gates for the simulation read lane.
//!
//! The EVM's own window rule and the anchor's own header chain are the two
//! authorities these tests hold the adapters to: a current, future, or
//! out-of-window request is the EVM's zero with no read; an in-window request is
//! served only when the header chain links it to the exact anchor; a replaced,
//! missing, or otherwise contradictory link is a typed refusal rather than a
//! zero or another branch's hash.

#![expect(
    clippy::expect_used,
    reason = "the scripted chain stub and its assertions fail loudly on a broken fixture"
)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use alloy::primitives::{Address, Bytes, B256, U256};
use alloy::providers::RootProvider;
use alloy::rpc::client::ClientBuilder;
use alloy::rpc::json_rpc::{
    ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{TransportError, TransportFut};
use serde_json::value::to_raw_value;
use serde_json::{json, Value};
use tower::Service;

use ::evm2::evm::r#async::AsyncDatabase;
use ::evm2::interpreter::Word;

use degenbot_rpc::chain_evidence::{resolve_anchor_at, ChainEvidenceError, HeaderRefusal};
use degenbot_rpc::provider::{rpc_round_trips, AlloyProvider};
use degenbot_simulation::sim::evm2::alloy_async_db::{AlloyAsyncDb, AlloyAsyncDbError};
use degenbot_simulation::sim::evm2::async_fanout::{ReadHubDb, ReadHubDbError};
use degenbot_simulation::sim::evm2::block_hash_ancestry::{BlockHashError, BlockHashRefusal};
use degenbot_simulation::sim::evm2::blocking_async_db::BlockingAsyncDb;
use degenbot_simulation::sim::evm2::frame_replay::Evm2ScratchEvm;
use degenbot_simulation::sim::evm2::read_hub::ReadHub;
use degenbot_simulation::sim::frame_replay::{ReplayableTx, ScratchBlock};
use degenbot_simulation::sim::storage_memo::ReadAnchor;
use degenbot_simulation::sim::RunVerdict;
use degenbot_simulation::{Evm2BlockSimHandle, SimulationOverrideParams, WarmCodeCacheInner};

const HEIGHT: u64 = 1_000;
const CHAIN: u64 = 1;

/// A block identity on one branch: the tag names the branch, the number names
/// the height, so two branches disagree at the same height.
fn hash(tag: u8, number: u64) -> B256 {
    let mut bytes = [0u8; 32];
    bytes[0] = tag;
    bytes[24..].copy_from_slice(&number.to_be_bytes());
    B256::new(bytes)
}

/// One header the stub can serve: its own hash, the parent it names, and an
/// optional height override that makes it contradict the requested number.
#[derive(Debug, Clone, Copy)]
struct Header {
    hash: B256,
    parent_hash: B256,
    stated_number: Option<u64>,
}

impl Header {
    fn linked(tag: u8, number: u64) -> Self {
        Self {
            hash: hash(tag, number),
            parent_hash: hash(tag, number.saturating_sub(1)),
            stated_number: None,
        }
    }
}

/// A parent-linked branch of `from..=to`, tag `tag`.
fn branch(tag: u8, from: u64, to: u64) -> BTreeMap<u64, Header> {
    (from..=to)
        .map(|number| (number, Header::linked(tag, number)))
        .collect()
}

#[derive(Debug, Default)]
struct StubState {
    headers: BTreeMap<u64, Header>,
    requests: Vec<(String, Value)>,
}

/// A chain stub serving one branch's headers by height, recording every
/// dispatched request so a test can state the read budget instead of assuming
/// it.
#[derive(Debug, Clone, Default)]
struct ChainStub {
    state: Arc<Mutex<StubState>>,
}

fn ok_answer(value: &Value) -> ResponsePayload {
    ResponsePayload::Success(to_raw_value(value).expect("the stub answers serializable JSON"))
}

fn requested_number(params: &Value) -> Option<u64> {
    let raw = params.get(0)?.as_str()?;
    raw.strip_prefix("0x")
        .map_or_else(|| raw.parse().ok(), |hex| u64::from_str_radix(hex, 16).ok())
}

impl ChainStub {
    fn serving(headers: BTreeMap<u64, Header>) -> Self {
        Self {
            state: Arc::new(Mutex::new(StubState {
                headers,
                requests: Vec::new(),
            })),
        }
    }

    fn serve(&self, headers: BTreeMap<u64, Header>) {
        self.state.lock().expect("the stub is not poisoned").headers = headers;
    }

    fn header_reads(&self) -> usize {
        self.state
            .lock()
            .expect("the stub is not poisoned")
            .requests
            .iter()
            .filter(|(method, _)| method == "eth_getBlockByNumber")
            .count()
    }

    fn requests(&self) -> Vec<(String, Value)> {
        self.state
            .lock()
            .expect("the stub is not poisoned")
            .requests
            .clone()
    }

    fn clear(&self) {
        self.state
            .lock()
            .expect("the stub is not poisoned")
            .requests
            .clear();
    }

    async fn provider(&self) -> AlloyProvider {
        let client = ClientBuilder::default().transport(self.clone(), true);
        let root: RootProvider = RootProvider::new(client);
        let provider = AlloyProvider::from_provider(Arc::new(root));
        provider
            .bind_to_chain(CHAIN)
            .await
            .expect("the stub serves its chain id");
        self.clear();
        provider
    }

    fn answer(&self, req: &SerializedRequest) -> Response {
        let params: Value = req
            .params()
            .and_then(|raw| serde_json::from_str(raw.get()).ok())
            .unwrap_or(Value::Null);
        self.state
            .lock()
            .expect("the stub is not poisoned")
            .requests
            .push((req.method().to_string(), params.clone()));
        let payload = match req.method() {
            "eth_chainId" => ok_answer(&json!(format!("0x{CHAIN:x}"))),
            "eth_getBalance" | "eth_getTransactionCount" => ok_answer(&json!("0x0")),
            "eth_getCode" => ok_answer(&json!("0x")),
            "eth_getBlockByNumber" => {
                let served = requested_number(&params).and_then(|number| {
                    self.state
                        .lock()
                        .ok()?
                        .headers
                        .get(&number)
                        .map(|h| (number, *h))
                });
                match served {
                    Some((number, header)) => ok_answer(&json!({
                        "number": format!("0x{:x}", header.stated_number.unwrap_or(number)),
                        "hash": header.hash,
                        "parentHash": header.parent_hash,
                    })),
                    None => ok_answer(&Value::Null),
                }
            }
            other => ResponsePayload::Failure(ErrorPayload {
                code: -32_601,
                message: format!("the chain stub does not serve {other}").into(),
                data: None,
            }),
        };
        Response {
            id: req.id().clone(),
            payload,
        }
    }
}

impl Service<RequestPacket> for ChainStub {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        let stub = self.clone();
        Box::pin(async move {
            Ok::<ResponsePacket, TransportError>(match req {
                RequestPacket::Single(one) => ResponsePacket::Single(stub.answer(&one)),
                RequestPacket::Batch(many) => {
                    ResponsePacket::Batch(many.iter().map(|one| stub.answer(one)).collect())
                }
            })
        })
    }
}

fn anchor_at(tag: u8) -> ReadAnchor {
    ReadAnchor::identified(HEIGHT, hash(tag, HEIGHT))
}

/// Whether a refusal names a broken parent link at `number`.
fn is_broken_link(error: &AlloyAsyncDbError, number: u64) -> bool {
    matches!(
        error,
        AlloyAsyncDbError::Ancestry(BlockHashError::Refusal(BlockHashRefusal::Evidence(
            ChainEvidenceError::Header(HeaderRefusal::BrokenParentLink { number: broken, .. })
        ))) if *broken == number
    )
}

/// Whether a refusal names a height the node holds no complete header at.
fn is_missing_header(error: &AlloyAsyncDbError, number: u64) -> bool {
    matches!(
        error,
        AlloyAsyncDbError::Ancestry(BlockHashError::Refusal(BlockHashRefusal::Evidence(
            ChainEvidenceError::Header(HeaderRefusal::MissingHeader { number: missing })
        ))) if *missing == number
    )
}

#[tokio::test]
async fn in_window_ancestors_are_the_anchors_own_hashes_and_one_read_per_level() {
    let stub = ChainStub::serving(branch(0xa1, HEIGHT - 300, HEIGHT));
    let provider = stub.provider().await;
    let mut db = AlloyAsyncDb::new(&provider, anchor_at(0xa1));

    assert_eq!(
        db.get_block_hash(Word::from(HEIGHT - 1))
            .await
            .expect("the parent is proven"),
        hash(0xa1, HEIGHT - 1)
    );
    assert_eq!(
        stub.header_reads(),
        2,
        "the anchor and its parent are the whole depth-1 proof"
    );

    assert_eq!(
        db.get_block_hash(Word::from(HEIGHT - 1))
            .await
            .expect("the memo answers"),
        hash(0xa1, HEIGHT - 1)
    );
    assert_eq!(
        stub.header_reads(),
        2,
        "a repeat request is memoized, not re-read"
    );

    assert_eq!(
        db.get_block_hash(Word::from(HEIGHT - 10))
            .await
            .expect("the deeper ancestor is proven"),
        hash(0xa1, HEIGHT - 10)
    );
    assert_eq!(
        stub.header_reads(),
        11,
        "a deeper request reads only the levels below the proven chain"
    );

    assert_eq!(
        db.get_block_hash(Word::from(HEIGHT - 256))
            .await
            .expect("the window's deepest ancestor"),
        hash(0xa1, HEIGHT - 256)
    );
    assert_eq!(
        stub.header_reads(),
        257,
        "the whole window costs one header read per level, once"
    );

    assert_eq!(
        db.get_block_hash(Word::from(HEIGHT - 200))
            .await
            .expect("a proven height is memoized"),
        hash(0xa1, HEIGHT - 200)
    );
    assert_eq!(
        stub.header_reads(),
        257,
        "no in-window request re-reads a proven level"
    );
}

#[tokio::test]
async fn current_future_and_out_of_window_hashes_are_the_evms_zero_without_a_read() {
    let stub = ChainStub::serving(branch(0xa1, HEIGHT - 300, HEIGHT + 1));
    let provider = stub.provider().await;
    let mut db = AlloyAsyncDb::new(&provider, anchor_at(0xa1));

    assert_eq!(
        db.get_block_hash(Word::from(HEIGHT))
            .await
            .expect("the executing block"),
        B256::ZERO
    );
    assert_eq!(
        db.get_block_hash(Word::from(HEIGHT + 1))
            .await
            .expect("a future block"),
        B256::ZERO
    );
    assert_eq!(
        db.get_block_hash(Word::from(HEIGHT - 257))
            .await
            .expect("one block past the window"),
        B256::ZERO
    );
    assert_eq!(
        db.get_block_hash(Word::MAX)
            .await
            .expect("a number outside u64"),
        B256::ZERO
    );
    assert_eq!(
        stub.requests().len(),
        0,
        "the EVM zero rule issues no request at all: {:?}",
        stub.requests()
    );

    assert_eq!(
        db.get_block_hash(Word::from(HEIGHT - 256))
            .await
            .expect("the window's last ancestor"),
        hash(0xa1, HEIGHT - 256)
    );
    assert!(
        stub.header_reads() > 0,
        "the window's last ancestor IS a read"
    );
}

#[tokio::test]
async fn a_replaced_block_below_the_anchor_is_refused_not_served() {
    let mut branch_a = branch(0xa1, HEIGHT - 5, HEIGHT);
    branch_a.insert(
        HEIGHT - 1,
        Header {
            hash: hash(0xb2, HEIGHT - 1),
            parent_hash: hash(0xb2, HEIGHT - 2),
            stated_number: None,
        },
    );
    let stub = ChainStub::serving(branch_a);
    let provider = stub.provider().await;
    let mut db = AlloyAsyncDb::new(&provider, anchor_at(0xa1));

    let error = db
        .get_block_hash(Word::from(HEIGHT - 1))
        .await
        .expect_err("a replacement at the requested height cannot be proven from the anchor");
    match &error {
        AlloyAsyncDbError::Ancestry(BlockHashError::Refusal(BlockHashRefusal::Evidence(
            ChainEvidenceError::Header(HeaderRefusal::BrokenParentLink {
                number,
                expected,
                actual,
            }),
        ))) => {
            assert_eq!(
                *number, HEIGHT,
                "the anchor's own link is the contradiction"
            );
            assert_eq!(
                *expected,
                hash(0xb2, HEIGHT - 1),
                "the served replacement is named as what the height now holds"
            );
            assert_eq!(
                *actual,
                hash(0xa1, HEIGHT - 1),
                "the anchor still names the ancestry's block as its parent"
            );
        }
        other => {
            assert!(
                is_broken_link(other, HEIGHT),
                "expected a broken parent link, got {other:?}"
            );
        }
    }

    let reads = stub.header_reads();
    assert!(
        db.get_block_hash(Word::from(HEIGHT - 2)).await.is_err(),
        "the contradiction stays refused"
    );
    assert_eq!(
        stub.header_reads(),
        reads,
        "a contradiction is sticky, never re-acquired per request"
    );
}

#[tokio::test]
async fn a_replaced_block_deeper_in_the_ancestry_is_refused() {
    let mut branch_a = branch(0xa1, HEIGHT - 6, HEIGHT);
    branch_a.insert(
        HEIGHT - 3,
        Header {
            hash: hash(0xb2, HEIGHT - 3),
            parent_hash: hash(0xb2, HEIGHT - 4),
            stated_number: None,
        },
    );
    let stub = ChainStub::serving(branch_a);
    let provider = stub.provider().await;
    let mut db = AlloyAsyncDb::new(&provider, anchor_at(0xa1));

    let error = db
        .get_block_hash(Word::from(HEIGHT - 4))
        .await
        .expect_err("the branch breaks above the requested height");
    assert!(
        is_broken_link(&error, HEIGHT - 3),
        "the replacement's own link to its parent is the contradiction, got {error:?}"
    );
}

#[tokio::test]
async fn a_missing_ancestry_header_fails_closed() {
    let mut branch_a = branch(0xa1, HEIGHT - 5, HEIGHT);
    branch_a.remove(&(HEIGHT - 2));
    let stub = ChainStub::serving(branch_a);
    let provider = stub.provider().await;
    let mut db = AlloyAsyncDb::new(&provider, anchor_at(0xa1));

    let error = db
        .get_block_hash(Word::from(HEIGHT - 3))
        .await
        .expect_err("a gap in the ancestry is no evidence");
    assert!(
        is_missing_header(&error, HEIGHT - 2),
        "the missing level is named, got {error:?}"
    );
}

#[tokio::test]
async fn a_moved_anchor_is_refused_instead_of_reading_the_replacement_chain() {
    let stub = ChainStub::serving(branch(0xb2, HEIGHT - 5, HEIGHT));
    let provider = stub.provider().await;
    let mut db = AlloyAsyncDb::new(&provider, anchor_at(0xa1));

    let error = db
        .get_block_hash(Word::from(HEIGHT - 1))
        .await
        .expect_err("the node no longer serves the anchor's block");
    assert!(
        matches!(
            error,
            AlloyAsyncDbError::Ancestry(BlockHashError::Refusal(BlockHashRefusal::AnchorMoved {
                number,
                hash: moved,
            })) if number == HEIGHT && moved == hash(0xa1, HEIGHT)
        ),
        "the refusal names the anchor identity that moved, got {error:?}"
    );
}

#[tokio::test]
async fn an_unresolved_anchor_refuses_in_window_and_still_zeros_outside_it() {
    let stub = ChainStub::serving(branch(0xa1, HEIGHT - 5, HEIGHT));
    let provider = stub.provider().await;
    let mut db = AlloyAsyncDb::new(&provider, ReadAnchor::unresolved(HEIGHT));

    let error = db
        .get_block_hash(Word::from(HEIGHT - 1))
        .await
        .expect_err("a height-only anchor proves no ancestry");
    assert!(
        matches!(
            error,
            AlloyAsyncDbError::Ancestry(BlockHashError::Refusal(BlockHashRefusal::UnresolvedAnchor {
                anchor,
                requested,
            })) if anchor == HEIGHT && requested == HEIGHT - 1
        ),
        "the refusal names the unresolved anchor, got {error:?}"
    );
    assert_eq!(
        stub.header_reads(),
        0,
        "an unresolved in-window request issues no read"
    );

    assert_eq!(
        db.get_block_hash(Word::from(HEIGHT))
            .await
            .expect("the current block"),
        B256::ZERO
    );
    assert_eq!(
        db.get_block_hash(Word::from(HEIGHT - 257))
            .await
            .expect("past the window"),
        B256::ZERO
    );
    assert_eq!(
        stub.requests().len(),
        0,
        "the zero rule needs no ancestry at all"
    );
}

#[tokio::test]
async fn a_header_that_states_another_height_is_no_evidence() {
    let mut branch_a = branch(0xa1, HEIGHT - 5, HEIGHT);
    branch_a.insert(
        HEIGHT - 1,
        Header {
            hash: hash(0xa1, HEIGHT - 1),
            parent_hash: hash(0xa1, HEIGHT - 2),
            stated_number: Some(HEIGHT - 2),
        },
    );
    let stub = ChainStub::serving(branch_a);
    let provider = stub.provider().await;
    let mut db = AlloyAsyncDb::new(&provider, anchor_at(0xa1));

    let error = db
        .get_block_hash(Word::from(HEIGHT - 1))
        .await
        .expect_err("a header stating another height cannot identify this one");
    assert!(
        is_missing_header(&error, HEIGHT - 1),
        "the wrong-height header reads as missing evidence, got {error:?}"
    );
}

#[tokio::test]
async fn a_next_block_frame_measures_the_window_from_its_execution_height() {
    let stub = ChainStub::serving(branch(0xa1, HEIGHT - 300, HEIGHT));
    let provider = stub.provider().await;
    let mut db = AlloyAsyncDb::executing(&provider, anchor_at(0xa1), HEIGHT + 1);

    assert_eq!(
        db.get_block_hash(Word::from(HEIGHT))
            .await
            .expect("the anchor is in window at depth 1"),
        hash(0xa1, HEIGHT),
        "a next-block frame's BLOCKHASH(NUMBER - 1) is the anchor's own hash"
    );
    assert_eq!(
        stub.header_reads(),
        1,
        "one terminal header proves the anchor identity"
    );
    assert_eq!(
        db.get_block_hash(Word::from(HEIGHT))
            .await
            .expect("the memo answers"),
        hash(0xa1, HEIGHT)
    );
    assert_eq!(stub.header_reads(), 1, "the anchor answer is memoized");

    let before = stub.header_reads();
    assert_eq!(
        db.get_block_hash(Word::from(HEIGHT + 1))
            .await
            .expect("the executing block"),
        B256::ZERO
    );
    assert_eq!(
        db.get_block_hash(Word::from(HEIGHT + 2))
            .await
            .expect("a future block"),
        B256::ZERO
    );
    assert_eq!(
        db.get_block_hash(Word::from(HEIGHT - 256))
            .await
            .expect("past the window"),
        B256::ZERO
    );
    assert_eq!(
        stub.header_reads(),
        before,
        "the EVM zero rule follows the EXECUTION height and issues no read"
    );

    assert_eq!(
        db.get_block_hash(Word::from(HEIGHT - 255))
            .await
            .expect("the window's last ancestor"),
        hash(0xa1, HEIGHT - 255),
        "depth 256 from the execution height is the anchor's 255th ancestor"
    );
    assert_eq!(
        stub.header_reads(),
        before + 255,
        "one header read per level between the anchor and the window's last ancestor"
    );
}

#[tokio::test]
async fn a_request_above_the_read_anchor_is_refused_rather_than_fabricated() {
    let stub = ChainStub::serving(branch(0xa1, HEIGHT - 5, HEIGHT + 2));
    let provider = stub.provider().await;
    let mut db = AlloyAsyncDb::executing(&provider, anchor_at(0xa1), HEIGHT + 2);

    let error = db
        .get_block_hash(Word::from(HEIGHT + 1))
        .await
        .expect_err("a later block is not in the anchor's ancestry");
    assert!(
        matches!(
            error,
            AlloyAsyncDbError::Ancestry(BlockHashError::Refusal(BlockHashRefusal::AboveAnchor {
                anchor,
                requested,
            })) if anchor == HEIGHT && requested == HEIGHT + 1
        ),
        "the refusal names the anchor and the unprovable height, got {error:?}"
    );
    assert_eq!(
        stub.header_reads(),
        0,
        "no ancestry read can prove a later block"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_production_scratch_handle_executes_at_the_next_block_over_the_anchor() {
    let stub = ChainStub::serving(branch(0xa1, HEIGHT - 5, HEIGHT));
    let provider = stub.provider().await;
    let warm_cache = WarmCodeCacheInner::shared_default();
    let oracle = degenbot_substrate::SimAnchorState::default();
    let mut handle = Evm2BlockSimHandle::build(
        &provider,
        1,
        anchor_at(0xa1),
        1,
        &SimulationOverrideParams {
            owner: PROBE_SENDER,
            ..Default::default()
        },
        &oracle,
        &warm_cache,
        None,
        false,
        None,
        None,
    )
    .expect("the offline handle builds over the stub");
    let scratch = handle.scratch_evm().expect("the scratch stacks");
    scratch.ext_mut().insert_account_info(
        &PROBE_SENDER,
        ::evm2::evm::AccountInfo::default().with_balance(U256::from(1_000_000_000_u64)),
    );
    scratch.ext_mut().insert_account_info(
        &PROBE,
        ::evm2::evm::AccountInfo::default().with_code(::evm2::bytecode::Bytecode::new_raw(
            block_hash_probe_runtime(),
        )),
    );

    let outcome = scratch
        .replay(&probe_tx())
        .expect("the scratch frame executes");
    assert!(
        matches!(outcome.status, RunVerdict::Success),
        "the scratch frame settles: {:?}",
        outcome.status
    );
    assert_eq!(
        word_at(&outcome.output, 0),
        U256::from_be_slice(hash(0xa1, HEIGHT).as_slice()),
        "the production scratch frame's BLOCKHASH(NUMBER - 1) is the anchor's own hash"
    );
    assert_eq!(
        word_at(&outcome.output, 1),
        U256::ZERO,
        "BLOCKHASH(NUMBER) is the EVM zero"
    );
    assert_eq!(
        word_at(&outcome.output, 2),
        U256::ZERO,
        "BLOCKHASH(NUMBER - 257) is outside the EVM window"
    );
    assert_eq!(
        stub.header_reads(),
        1,
        "the production next-block scratch proves the anchor with its one header"
    );
}

#[tokio::test]
async fn proven_hashes_stay_scoped_to_the_anchor_they_were_proven_for() {
    let stub = ChainStub::serving(branch(0xa1, HEIGHT - 5, HEIGHT));
    let provider = stub.provider().await;
    let mut first = AlloyAsyncDb::new(&provider, anchor_at(0xa1));
    assert_eq!(
        first
            .get_block_hash(Word::from(HEIGHT - 1))
            .await
            .expect("branch a is proven"),
        hash(0xa1, HEIGHT - 1)
    );
    let reads = stub.header_reads();

    stub.serve(branch(0xb2, HEIGHT - 5, HEIGHT));
    let mut second = AlloyAsyncDb::new(&provider, anchor_at(0xb2));
    assert_eq!(
        second
            .get_block_hash(Word::from(HEIGHT - 1))
            .await
            .expect("branch b is proven"),
        hash(0xb2, HEIGHT - 1),
        "the second anchor's ancestry is its own branch, never the first's"
    );

    let reads_after_both = stub.header_reads();
    assert_eq!(
        reads_after_both,
        reads + 2,
        "the second anchor proves its own branch; the first anchor's repeat reads nothing"
    );
    assert_eq!(
        first
            .get_block_hash(Word::from(HEIGHT - 1))
            .await
            .expect("the first proof stands"),
        hash(0xa1, HEIGHT - 1),
        "a proven ancestry stays the anchor's own branch after the node moved"
    );
}

#[tokio::test]
async fn the_read_hub_adapter_proves_the_hub_anchors_ancestry() {
    let stub = ChainStub::serving(branch(0xa1, HEIGHT - 5, HEIGHT));
    let provider = stub.provider().await;
    let hub = ReadHub::new(&provider, anchor_at(0xa1));
    let mut db = ReadHubDb::new(&hub, &provider);

    assert_eq!(
        db.get_block_hash(Word::from(HEIGHT - 1))
            .await
            .expect("the hub anchor's parent"),
        hash(0xa1, HEIGHT - 1)
    );
    assert_eq!(
        db.get_block_hash(Word::from(HEIGHT))
            .await
            .expect("the executing block"),
        B256::ZERO
    );

    let stub = ChainStub::serving(branch(0xb2, HEIGHT - 5, HEIGHT));
    let provider = stub.provider().await;
    let hub = ReadHub::new(&provider, anchor_at(0xa1));
    let mut moved = ReadHubDb::new(&hub, &provider);
    let error = moved
        .get_block_hash(Word::from(HEIGHT - 1))
        .await
        .expect_err("the hub anchor's block is no longer served");
    assert!(
        matches!(
            error,
            ReadHubDbError::Ancestry(BlockHashError::Refusal(
                BlockHashRefusal::AnchorMoved { .. }
            ))
        ),
        "the hub adapter shares the ancestry refusal vocabulary, got {error:?}"
    );
}

/// The probe contract the EVM-level legs deploy: it returns three words —
/// `BLOCKHASH(NUMBER - 1)`, `BLOCKHASH(NUMBER)`, and
/// `BLOCKHASH(NUMBER - 257)` — so one frame reads the ancestry answer beside
/// the EVM's own zero rule.
fn block_hash_probe_runtime() -> Bytes {
    Bytes::from_static(&[
        0x60, 0x01, 0x43, 0x03, 0x40, // BLOCKHASH(NUMBER - 1)
        0x60, 0x00, 0x52, // MSTORE(0x00)
        0x43, 0x40, // BLOCKHASH(NUMBER)
        0x60, 0x20, 0x52, // MSTORE(0x20)
        0x61, 0x01, 0x01, 0x43, 0x03, 0x40, // BLOCKHASH(NUMBER - 257)
        0x60, 0x40, 0x52, // MSTORE(0x40)
        0x60, 0x60, 0x60, 0x00, 0xF3, // RETURN(0, 0x60)
    ])
}

const PROBE: Address = Address::repeat_byte(0x77);
const PROBE_SENDER: Address = Address::repeat_byte(0x51);

fn probe_scratch(
    provider: &AlloyProvider,
    anchor: ReadAnchor,
) -> Evm2ScratchEvm<::evm2::evm::CacheDB<::evm2::evm::Db<BlockingAsyncDb<'_>>>> {
    let blocking = BlockingAsyncDb::new(AlloyAsyncDb::executing(
        provider,
        anchor,
        anchor.number().saturating_add(1),
    ))
    .expect("a multi-threaded runtime is ambient");
    let mut ext = ::evm2::evm::CacheDB::new(::evm2::evm::Db::new(blocking));
    ext.insert_account_info(
        &PROBE_SENDER,
        ::evm2::evm::AccountInfo::default().with_balance(U256::from(1_000_000_000_u64)),
    );
    ext.insert_account_info(
        &PROBE,
        ::evm2::evm::AccountInfo::default().with_code(::evm2::bytecode::Bytecode::new_raw(
            block_hash_probe_runtime(),
        )),
    );
    Evm2ScratchEvm::new(
        ext,
        ScratchBlock {
            number: anchor.number().saturating_add(1),
            timestamp: 1,
            base_fee_next: 1,
        },
    )
}

fn probe_tx() -> ReplayableTx {
    ReplayableTx {
        from: PROBE_SENDER,
        to: Some(PROBE),
        value: U256::ZERO,
        data: Bytes::new(),
        gas_limit: 200_000,
        max_fee_per_gas: 1,
        max_priority_fee_per_gas: 0,
        nonce: 0,
    }
}

fn word_at(output: &Bytes, index: usize) -> U256 {
    U256::from_be_slice(&output[index * 32..(index + 1) * 32])
}

#[tokio::test(flavor = "multi_thread")]
async fn the_evm_blockhash_opcode_answers_from_the_anchors_ancestry() {
    let stub = ChainStub::serving(branch(0xa1, HEIGHT - 5, HEIGHT));
    let provider = stub.provider().await;
    let mut scratch = probe_scratch(&provider, anchor_at(0xa1));

    let outcome = scratch
        .replay(&probe_tx())
        .expect("the probe frame executes");
    assert!(
        matches!(outcome.status, RunVerdict::Success),
        "the probe frame settles: {:?}",
        outcome.status
    );
    assert_eq!(
        word_at(&outcome.output, 0),
        U256::from_be_slice(hash(0xa1, HEIGHT).as_slice()),
        "the next-block frame's BLOCKHASH(NUMBER - 1) is the anchor's own hash"
    );
    assert_eq!(
        word_at(&outcome.output, 1),
        U256::ZERO,
        "BLOCKHASH(NUMBER) is the EVM zero"
    );
    assert_eq!(
        word_at(&outcome.output, 2),
        U256::ZERO,
        "BLOCKHASH(NUMBER - 257) is outside the EVM window"
    );
    assert_eq!(
        stub.header_reads(),
        1,
        "the next-block frame's depth-1 answer is the one anchor header"
    );
}

async fn live_provider() -> AlloyProvider {
    let rpc_url = std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1")
        .expect("the fork lane exports a chain-1 archive endpoint");
    AlloyProvider::for_chain(&rpc_url, CHAIN, 3)
        .await
        .expect("the lane dials a reachable chain-1 archive endpoint")
}

/// The EVM's zero rule at `number`: the executing block, a future block, and
/// a block past the window are zero, and none of them dispatches a request.
async fn assert_evm_zero_rule(db: &mut AlloyAsyncDb<'_>, number: u64) {
    let before = rpc_round_trips();
    assert_eq!(
        db.get_block_hash(Word::from(number))
            .await
            .expect("the executing block"),
        B256::ZERO
    );
    assert_eq!(
        db.get_block_hash(Word::from(number + 1))
            .await
            .expect("a future block"),
        B256::ZERO
    );
    assert_eq!(
        db.get_block_hash(Word::from(number - 257))
            .await
            .expect("past the window"),
        B256::ZERO
    );
    assert_eq!(
        rpc_round_trips(),
        before,
        "the EVM zero rule dispatches no request"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network: needs a chain-1 RPC behind DEGENBOT_RPC_HTTP_CHAINID_1"]
async fn live_block_hash_ancestry_matches_the_archive_chain() {
    let provider = live_provider().await;
    provider
        .bind_to_chain(CHAIN)
        .await
        .expect("the lane dials chain 1");
    let head = provider
        .get_block_number()
        .await
        .expect("the node states its head");
    let anchor = resolve_anchor_at(&provider, head)
        .await
        .expect("the head resolves to one exact anchor");
    assert_eq!(anchor.number(), head);

    let depths = [1_u64, 2, 10, 255, 256];
    let mut truth = Vec::with_capacity(depths.len());
    for depth in depths {
        let number = anchor.number() - depth;
        truth.push(
            provider
                .get_block_hash_at(number)
                .await
                .expect("the truth read")
                .expect("the archive node holds the in-window ancestor"),
        );
    }

    let mut db = AlloyAsyncDb::new(
        &provider,
        ReadAnchor::identified(anchor.number(), anchor.hash()),
    );
    assert_evm_zero_rule(&mut db, anchor.number()).await;

    let before = rpc_round_trips();
    let mut budgets = Vec::with_capacity(depths.len());
    for (depth, expected) in depths.into_iter().zip(&truth) {
        let number = anchor.number() - depth;
        assert_eq!(
            db.get_block_hash(Word::from(number))
                .await
                .expect("the ancestry proves"),
            *expected,
            "depth {depth} matches the archive node's own block hash"
        );
        budgets.push(rpc_round_trips() - before);
    }
    assert_eq!(
        budgets,
        vec![2, 3, 11, 256, 257],
        "one header read per ancestry level, memoized: {budgets:?}"
    );

    let memo_before = rpc_round_trips();
    assert_eq!(
        db.get_block_hash(Word::from(anchor.number() - 10))
            .await
            .expect("the memo answers"),
        truth[2]
    );
    assert_eq!(
        rpc_round_trips(),
        memo_before,
        "a proven depth costs no second read"
    );

    let stale = anchor.hash();
    let mut moved = AlloyAsyncDb::new(
        &provider,
        ReadAnchor::identified(anchor.number(), B256::repeat_byte(0x5a)),
    );
    let error = moved
        .get_block_hash(Word::from(anchor.number() - 1))
        .await
        .expect_err("a non-canonical anchor proves no ancestry");
    assert!(
        matches!(
            error,
            AlloyAsyncDbError::Ancestry(BlockHashError::Refusal(BlockHashRefusal::AnchorMoved {
                number,
                hash,
            })) if number == anchor.number() && hash == B256::repeat_byte(0x5a)
        ),
        "the moved anchor is named, got {error:?} (canonical head is {stale})"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live network: needs a chain-1 RPC behind DEGENBOT_RPC_HTTP_CHAINID_1"]
async fn live_block_hash_ancestry_through_the_evm_matches_fork_truth() {
    let provider = live_provider().await;
    provider
        .bind_to_chain(CHAIN)
        .await
        .expect("the lane dials chain 1");
    let head = provider
        .get_block_number()
        .await
        .expect("the node states its head");
    let anchor = resolve_anchor_at(&provider, head)
        .await
        .expect("the head resolves to one exact anchor");
    let truth = provider
        .get_block_hash_at(anchor.number())
        .await
        .expect("the truth read")
        .expect("the archive node holds the anchor");
    assert_eq!(
        truth,
        anchor.hash(),
        "the resolved anchor is the node's own block"
    );

    let mut scratch = probe_scratch(
        &provider,
        ReadAnchor::identified(anchor.number(), anchor.hash()),
    );
    let before = rpc_round_trips();
    let outcome = scratch
        .replay(&probe_tx())
        .expect("the probe frame executes");
    assert!(
        matches!(outcome.status, RunVerdict::Success),
        "the probe frame settles: {:?}",
        outcome.status
    );
    assert_eq!(
        rpc_round_trips() - before,
        2,
        "the next-block frame's depth-1 answer is the one anchor header, plus the \
         frame's one account-hydration packet"
    );
    assert_eq!(
        word_at(&outcome.output, 0),
        U256::from_be_slice(truth.as_slice()),
        "the next-block frame's BLOCKHASH(NUMBER - 1) is the archive node's anchor hash"
    );
    assert_eq!(
        word_at(&outcome.output, 1),
        U256::ZERO,
        "BLOCKHASH(NUMBER) is the EVM zero"
    );
    assert_eq!(
        word_at(&outcome.output, 2),
        U256::ZERO,
        "BLOCKHASH(NUMBER - 257) is outside the EVM window"
    );
}
