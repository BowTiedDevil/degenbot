//! The evm2 port of the RPC cold-miss fallback — the
//! `WrapDatabaseAsync<AlloyDB>` replacement.
//!
//! revm's seam is sync: `WrapDatabaseAsync` blocks an internal tokio handle
//! on every `AlloyDB` read. evm2's seam is async-native: an [`AsyncDatabase`]
//! returns futures, the `AsyncDb` adapter objectifies them into the
//! synchronous `DynDatabase` surface, and `Evm::transact_async` drives them
//! on stackful coroutines — no manual runtime blocking, no per-read
//! `block_on`. The engine's per-block discipline (fresh `CacheDB` per block,
//! settle-and-discard) is unchanged; only the cold-miss layer differs.
//!
//! The read semantics mirror `AlloyDB`: account reads pin one block and
//! eager-load code (so `get_code_by_hash` is unreachable, the same invariant
//! the revm side relies on), and an account with zero balance, zero nonce,
//! and no code reads as non-existent.
//!
//! # The pin is the anchor, not a height
//!
//! Every read this adapter issues takes its transport identifier from
//! [`ReadAnchor::block_id`]: an IDENTIFIED anchor states the block's own
//! canonical hash (EIP-1898 `{ blockHash, requireCanonical: true }`), so a node
//! either answers at exactly that block or refuses a read here — it can never
//! answer a same-height replacement instead. Only an explicit
//! [`ReadAnchor::unresolved`] (an observation that resolved no header) states
//! a height, and that form exists for the ingress callers that legitimately
//! hold no identity (ADR-105 D1/D3). There is no second, height-only read path
//! in this adapter: the anchor's identifier IS the read.
//!
//! The account read is ONE JSON-RPC batch round trip
//! ([`AlloyProvider::get_account_at`]): `eth_getBalance`,
//! `eth_getTransactionCount`, and `eth_getCode` at the identical pinned block
//! tag, answered together. Three serial round trips per cold account miss was
//! the measured cold-frame cost (the `cold_miss_fanout_probe` model).

use alloy::primitives::{Address, B256};
use degenbot_rpc::provider::{AccountRead, AlloyProvider};

use crate::sim::storage_memo::ReadAnchor;

use ::evm2::bytecode::Bytecode;
use ::evm2::evm::r#async::AsyncDatabase;
use ::evm2::evm::AccountInfo;
use ::evm2::interpreter::Word;

/// The adapter's error type — the provider's RPC failure, or a block the
/// node cannot serve (evm2 treats a benign block-hash miss as a database
/// failure, matching the trait contract).
#[derive(Debug, thiserror::Error)]
pub enum AlloyAsyncDbError {
    /// The underlying RPC read failed.
    #[error("RPC read failed: {0}")]
    Rpc(#[from] degenbot_core::errors::ProviderError),
    /// `eth_getBlockByNumber` returned null for a requested block.
    #[error("block {0} not found")]
    BlockNotFound(u64),
}

/// The account-row conversion every async adapter shares: an all-empty row is a
/// non-existent account, and a served row's code is eager-loaded with
/// `new_raw` (never `new_legacy`) — foreign code may be an EIP-7702 delegation
/// designator, and only the raw constructor auto-detects it; a legacy-wrapped
/// 7702 account fails validation (`RejectCallerWithCode`) and decodes wrong
/// downstream.
#[must_use]
pub fn account_info_from_read(read: &AccountRead) -> Option<AccountInfo> {
    if read.balance.is_zero() && read.nonce == 0 && read.code.is_empty() {
        return None;
    }
    Some(
        AccountInfo::default()
            .with_balance(read.balance)
            .with_nonce(read.nonce)
            .with_code(Bytecode::new_raw(read.code.clone())),
    )
}

/// The async batching read lane in front of these single reads is
/// [`read_hub`](super::read_hub): it coalesces concurrent reads at one anchor,
/// issues one provider batch per kind, and hands every requester its value. This
/// adapter stays the single-read leaf (one account, one slot), and the hub is
/// built beside it rather than through it.
/// An `AsyncDatabase` over the shared `AlloyProvider`, pinning every read to
/// one block — the per-block cold-miss fallback in the target evm2 stack
/// (`CacheDB<WarmCodeCache<BotStateDb<AlloyAsyncDb>>>`).
pub struct AlloyAsyncDb<'p> {
    /// The shared provider (same instance the revm engine's `AlloyDB`
    /// consumes, via the same retry/timeout policy).
    provider: &'p AlloyProvider,
    /// The exact block every read pins — the fan-out's solve anchor, never the
    /// consumer's execution clock.
    anchor: ReadAnchor,
}

impl<'p> AlloyAsyncDb<'p> {
    /// Wrap the provider, pinning every read to `anchor`.
    #[must_use]
    pub const fn new(provider: &'p AlloyProvider, anchor: ReadAnchor) -> Self {
        Self { provider, anchor }
    }

    /// The exact block this adapter's reads pin.
    #[must_use]
    pub const fn anchor(&self) -> ReadAnchor {
        self.anchor
    }
}

impl AsyncDatabase for AlloyAsyncDb<'_> {
    type Error = AlloyAsyncDbError;

    /// Balance + nonce + code at the pinned anchor, eager-loaded like
    /// `AlloyDB::basic_ref` — one JSON-RPC batch round trip. The all-empty
    /// account reads as non-existent.
    async fn get_account(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        let read = self
            .provider
            .get_account_at(&address, self.anchor.block_id())
            .await?;
        Ok(account_info_from_read(&read))
    }

    /// Unreachable: `get_account` eager-loads code, the same invariant the
    /// revm side relies on (`code_by_hash` is never invoked when `basic`
    /// returns code inline). Standard JSON-RPC has no code-by-hash read, so
    /// there is no honest implementation.
    #[expect(clippy::panic)]
    async fn get_code_by_hash(&mut self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
        panic!("get_account eager-loads code — get_code_by_hash is never invoked");
    }

    /// One storage slot at the pinned anchor.
    async fn get_storage(&mut self, address: Address, key: Word) -> Result<Word, Self::Error> {
        let value = self
            .provider
            .get_storage_at(&address, key, Some(self.anchor.block_id()))
            .await?;
        Ok(Word::from_be_bytes(value.0))
    }

    /// One block hash. A missing block is a database failure (the BLOCKHASH
    /// window contract), never a benign zero.
    async fn get_block_hash(&mut self, number: Word) -> Result<B256, Self::Error> {
        let number = number.to::<u64>();
        let block = self.provider.get_block(number).await?;
        let block = block.ok_or(AlloyAsyncDbError::BlockNotFound(number))?;
        Ok(block.header.hash)
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use alloy::primitives::{Bytes, U256};
    use alloy::providers::RootProvider;
    use alloy::rpc::client::ClientBuilder;
    use alloy::rpc::json_rpc::{
        RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
    };
    use alloy::transports::{TransportError, TransportFut};
    use serde_json::json;
    use serde_json::value::to_raw_value;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};

    /// The account the stub serves: 1 ETH, nonce 42, two code bytes.
    const BALANCE: &str = "0xde0b6b3a7640000";
    const NONCE: &str = "0x2a";
    const CODE: &str = "0x60016001";

    /// A chain stub that records every dispatched packet — one entry per
    /// `call`, so a JSON-RPC batch is ONE entry (the round-trip count).
    #[derive(Debug, Clone)]
    struct ChainStub {
        packets: Arc<Mutex<Vec<Vec<String>>>>,
        empty: bool,
    }

    impl ChainStub {
        fn serving() -> Self {
            Self {
                packets: Arc::new(Mutex::new(Vec::new())),
                empty: false,
            }
        }

        fn empty_account() -> Self {
            Self {
                packets: Arc::new(Mutex::new(Vec::new())),
                empty: true,
            }
        }

        fn packets(&self) -> Vec<Vec<String>> {
            self.packets.lock().unwrap().clone()
        }

        fn provider(&self) -> AlloyProvider {
            let client = ClientBuilder::default().transport(self.clone(), true);
            let root: RootProvider = RootProvider::new(client);
            AlloyProvider::from_provider(Arc::new(root))
        }

        fn answer(&self, req: &SerializedRequest) -> Response {
            let result = match (req.method(), self.empty) {
                (_, true) => match req.method() {
                    "eth_getCode" => json!("0x"),
                    _ => json!("0x0"),
                },
                ("eth_getBalance", false) => json!(BALANCE),
                ("eth_getTransactionCount", false) => json!(NONCE),
                ("eth_getCode", false) => json!(CODE),
                (other, false) => panic!("an account read must not issue {other}"),
            };
            Response {
                id: req.id().clone(),
                payload: ResponsePayload::Success(to_raw_value(&result).unwrap()),
            }
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
                let recorded: Vec<String> = match &req {
                    RequestPacket::Single(one) => vec![one.method().to_string()],
                    RequestPacket::Batch(many) => {
                        many.iter().map(|one| one.method().to_string()).collect()
                    }
                };
                stub.packets.lock().unwrap().push(recorded);
                Ok::<ResponsePacket, TransportError>(match req {
                    RequestPacket::Single(one) => ResponsePacket::Single(stub.answer(&one)),
                    RequestPacket::Batch(many) => {
                        ResponsePacket::Batch(many.iter().map(|one| stub.answer(one)).collect())
                    }
                })
            })
        }
    }

    /// One account read is ONE dispatched batch packet (three members, never
    /// three requests) and hydrates exactly what the serial reads return.
    #[tokio::test]
    async fn get_account_is_one_batch_round_trip() {
        let stub = ChainStub::serving();
        let provider = stub.provider();
        let address = Address::from([0x11; 20]);
        let mut db = AlloyAsyncDb::new(&provider, ReadAnchor::unresolved(0x1234));

        let info = db.get_account(address).await.unwrap().unwrap();
        assert_eq!(info.balance, U256::from(1_000_000_000_000_000_000_u64));
        assert_eq!(info.nonce, 42);
        assert_eq!(
            info.code
                .expect("the read eager-loads code")
                .original_bytes(),
            Bytes::from_static(&[0x60, 0x01, 0x60, 0x01])
        );

        let packets = stub.packets();
        assert_eq!(
            packets.len(),
            1,
            "one account read dispatches ONE packet, got {packets:?}"
        );
        assert_eq!(
            packets[0],
            vec![
                "eth_getBalance".to_string(),
                "eth_getTransactionCount".to_string(),
                "eth_getCode".to_string(),
            ],
            "the one packet is the three-read batch, in hydration order"
        );
    }

    /// The all-empty rule survives the batch: a zero balance, zero nonce, and
    /// empty code read as non-existent — still one round trip.
    #[tokio::test]
    async fn all_empty_account_in_one_batch_reads_as_none() {
        let stub = ChainStub::empty_account();
        let provider = stub.provider();
        let address = Address::from([0x22; 20]);
        let mut db = AlloyAsyncDb::new(&provider, ReadAnchor::unresolved(7));

        assert!(db.get_account(address).await.unwrap().is_none());
        assert_eq!(stub.packets().len(), 1);
    }

    /// The height the pinned chain stub holds.
    const PINNED_NUMBER: u64 = 0x1234;
    /// The one slot the pinned stub serves.
    const SLOT_VALUE: &str = "0x000000000000000000000000000000000000000000000000000000000000002a";

    /// A chain stub that answers ONLY the hash-pinned identifier naming `pin`
    /// — the node an abandoned or replaced hash cannot reach, and which records
    /// every request's params so the wire shape is asserted, not assumed.
    #[derive(Debug, Clone)]
    struct PinnedChain {
        pin: B256,
        requests: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    }

    impl PinnedChain {
        fn serving(pin: B256) -> Self {
            Self {
                pin,
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn requests(&self) -> Vec<(String, serde_json::Value)> {
            self.requests.lock().unwrap().clone()
        }

        fn provider(&self) -> AlloyProvider {
            let client = ClientBuilder::default().transport(self.clone(), true);
            let root: RootProvider = RootProvider::new(client);
            AlloyProvider::from_provider(Arc::new(root))
        }

        /// The param position the read's block identifier occupies: the
        /// account triple carries `[address, block]`, storage the slot too.
        fn identifier_index(method: &str) -> usize {
            if method == "eth_getStorageAt" {
                2
            } else {
                1
            }
        }

        /// Whether `params` names this stub's block by hash, canonically.
        fn names_pin(&self, method: &str, params: &serde_json::Value) -> bool {
            params
                .get(Self::identifier_index(method))
                .is_some_and(|id| {
                    id.get("blockHash").and_then(serde_json::Value::as_str)
                        == Some(format!("{}", self.pin).as_str())
                        && id
                            .get("requireCanonical")
                            .and_then(serde_json::Value::as_bool)
                            == Some(true)
                })
        }

        fn answer(&self, req: &SerializedRequest) -> Response {
            let params: serde_json::Value = req
                .params()
                .and_then(|raw| serde_json::from_str(raw.get()).ok())
                .unwrap_or(serde_json::Value::Null);
            self.requests
                .lock()
                .unwrap()
                .push((req.method().to_string(), params.clone()));
            let payload = if self.names_pin(req.method(), &params) {
                let result = match req.method() {
                    "eth_getBalance" => json!(BALANCE),
                    "eth_getTransactionCount" => json!(NONCE),
                    "eth_getCode" => json!(CODE),
                    "eth_getStorageAt" => json!(SLOT_VALUE),
                    other => panic!("a pinned write-free read must not issue {other}"),
                };
                ResponsePayload::Success(to_raw_value(&result).unwrap())
            } else {
                ResponsePayload::Failure(alloy::rpc::json_rpc::ErrorPayload {
                    code: -32_000,
                    message: "no canonical block for that identifier".into(),
                    data: None,
                })
            };
            Response {
                id: req.id().clone(),
                payload,
            }
        }
    }

    impl tower::Service<RequestPacket> for PinnedChain {
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

    /// An identified anchor pins every read on the wire: the account triple and
    /// the storage word both name `{ blockHash, requireCanonical: true }` with
    /// the anchor's own hash, and no request states a height.
    #[tokio::test]
    async fn an_identified_anchor_pins_every_read_by_hash() {
        let head = B256::new([0xa1; 32]);
        let stub = PinnedChain::serving(head);
        let provider = stub.provider();
        let address = Address::from([0x11; 20]);
        let mut db = AlloyAsyncDb::new(&provider, ReadAnchor::identified(PINNED_NUMBER, head));

        let info = db.get_account(address).await.unwrap().unwrap();
        assert_eq!(info.nonce, 42);
        let word = db.get_storage(address, U256::from(0x2au64)).await.unwrap();
        assert_eq!(word, Word::from(42u64));

        let requests = stub.requests();
        assert_eq!(
            requests.len(),
            4,
            "one packet per read kind: the three-member account batch plus the slot, got {requests:?}"
        );
        for (method, params) in requests {
            let identifier = &params[PinnedChain::identifier_index(&method)];
            assert!(
                identifier.is_object(),
                "{method} states an EIP-1898 identifier, never a height: {params}"
            );
            assert_eq!(
                identifier
                    .get("blockHash")
                    .and_then(serde_json::Value::as_str),
                Some(format!("{head}").as_str()),
                "{method} pins the anchor's own hash"
            );
            assert_eq!(
                identifier
                    .get("requireCanonical")
                    .and_then(serde_json::Value::as_bool),
                Some(true),
                "{method} requires the pinned block to still be canonical"
            );
        }
    }

    /// The red-first same-height replacement: an anchor naming the REPLACED
    /// block's hash refuses on every read. The stub serves the height only when
    /// the request pins the hash it holds, so a height-tagged read would be
    /// answered with the current canonical word — exactly the substitution this
    /// adapter must never allow.
    #[tokio::test]
    async fn a_replaced_same_height_hash_refuses_instead_of_reading_the_height() {
        let current = B256::new([0xa1; 32]);
        let replaced = B256::new([0xb1; 32]);
        let stub = PinnedChain::serving(current);
        let provider = stub.provider();
        let address = Address::from([0x11; 20]);
        let mut db = AlloyAsyncDb::new(&provider, ReadAnchor::identified(PINNED_NUMBER, replaced));

        assert!(
            db.get_account(address).await.is_err(),
            "the replaced branch's account read must refuse"
        );
        assert!(
            db.get_storage(address, U256::from(0x2au64)).await.is_err(),
            "the replaced branch's storage read must refuse"
        );

        let requests = stub.requests();
        assert_eq!(
            requests.len(),
            4,
            "every read was issued once, at the abandoned identity, got {requests:?}"
        );
        for (method, params) in requests {
            let identifier = &params[PinnedChain::identifier_index(&method)];
            assert_eq!(
                identifier
                    .get("blockHash")
                    .and_then(serde_json::Value::as_str),
                Some(format!("{replaced}").as_str()),
                "{method} pins the abandoned hash — never the height that holds the replacement"
            );
        }
    }
}
