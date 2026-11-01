//! Tests for the `ConstructionIo` composite + the trait surface (task 1 / 2)
//! + the adapter tests (`NoDb`, `DegenbotDbConstruction`,
//!   `AlloyRpcConstruction`).

#![expect(clippy::unwrap_used, clippy::expect_used)]
use std::sync::Arc;

use alloy::primitives::{Address, Bytes, U256};
use async_trait::async_trait;
use degenbot_core::errors::ProviderError;
use degenbot_db::connection::DegenbotDb;
use degenbot_db::rows::{
    Erc20TokenRow, ExchangeRow, LiquidityPoolRow, LiquidityPositionRow,
    ManagedPoolLiquidityPositionRow, PoolKindRow, PoolManagerRow, V4PoolRow,
};

use super::adapters::{AlloyRpcConstruction, DegenbotDbConstruction, NoDb};
use super::block_selector::BlockSelector;
use super::handle::ConstructionIo;
use super::read_memo::ReadMemo;
use super::{DbConstruction, RpcConstruction};

/// A fake `DbConstruction` that returns canned rows — proves the composite
/// routes to the `db` half via `Arc<dyn DbConstruction + Send + Sync>`.
struct FakeDb;

#[async_trait]
impl DbConstruction for FakeDb {
    async fn fetch_erc20_token(
        &self,
        chain_id: i64,
        _address: Address,
    ) -> Result<Option<Erc20TokenRow>, degenbot_db::error::DbError> {
        Ok(Some(Erc20TokenRow {
            id: chain_id,
            chain: chain_id,
            address: Address::ZERO,
            name: Some("fake".into()),
            symbol: Some("FK".into()),
            decimals: Some(18),
        }))
    }

    async fn fetch_pool_row(
        &self,
        _chain_id: i64,
        _address: Address,
    ) -> Result<Option<LiquidityPoolRow>, degenbot_db::error::DbError> {
        Ok(None)
    }

    async fn fetch_pool_kind(
        &self,
        _kind: &str,
        _pool_id: i64,
    ) -> Result<Option<PoolKindRow>, degenbot_db::error::DbError> {
        Ok(None)
    }

    async fn fetch_token_by_id(
        &self,
        _token_id: i64,
    ) -> Result<Option<Erc20TokenRow>, degenbot_db::error::DbError> {
        Ok(None)
    }

    async fn fetch_exchange(
        &self,
        _exchange_id: i64,
    ) -> Result<Option<ExchangeRow>, degenbot_db::error::DbError> {
        Ok(None)
    }

    async fn fetch_liquidity_positions(
        &self,
        _pool_id: i64,
    ) -> Result<Vec<LiquidityPositionRow>, degenbot_db::error::DbError> {
        Ok(Vec::new())
    }

    async fn fetch_pool_manager(
        &self,
        _chain_id: i64,
        _address: Address,
    ) -> Result<Option<PoolManagerRow>, degenbot_db::error::DbError> {
        Ok(None)
    }

    async fn fetch_v4_pool_by_pool_hash(
        &self,
        _pool_hash_hex: &str,
    ) -> Result<Option<V4PoolRow>, degenbot_db::error::DbError> {
        Ok(None)
    }

    async fn fetch_managed_liquidity_positions(
        &self,
        _managed_pool_id: i64,
    ) -> Result<Vec<ManagedPoolLiquidityPositionRow>, degenbot_db::error::DbError> {
        Ok(Vec::new())
    }

    async fn update_erc20_token_metadata(
        &self,
        _chain_id: i64,
        _address: &str,
        _name: Option<&str>,
        _symbol: Option<&str>,
        _decimals: Option<i64>,
    ) -> Result<(), degenbot_db::error::DbError> {
        Ok(())
    }
}

/// A fake `RpcConstruction` returning canned bytes — proves the composite
/// routes to the `rpc` half.
struct FakeRpc {
    block_number: u64,
}

#[async_trait]
impl RpcConstruction for FakeRpc {
    async fn get_block_number(&self) -> Result<u64, ProviderError> {
        Ok(self.block_number)
    }

    async fn get_block(
        &self,
        _block_number: u64,
    ) -> Result<Option<degenbot_rpc::provider::EthBlock>, ProviderError> {
        Ok(None)
    }

    async fn get_block_timestamp(&self, _block_number: u64) -> Result<Option<u64>, ProviderError> {
        Ok(None)
    }

    async fn get_code(
        &self,
        _address: Address,
        _block_number: Option<u64>,
    ) -> Result<Bytes, ProviderError> {
        Ok(Bytes::new())
    }

    async fn get_balance(
        &self,
        _address: Address,
        _block_number: Option<u64>,
    ) -> Result<U256, ProviderError> {
        Ok(U256::ZERO)
    }

    async fn call(
        &self,
        _to: Address,
        _data: Bytes,
        _block_number: Option<u64>,
    ) -> Result<Bytes, ProviderError> {
        Ok(Bytes::from_static(&[0xca, 0xfe]))
    }
}

/// The composite routes `fetch_erc20_token` to the `db` half — canned row
/// comes back through the `Arc<dyn DbConstruction + Send + Sync>` dispatch.
#[tokio::test]
async fn composite_routes_db_call_to_db_half() {
    let io = ConstructionIo::new(Arc::new(FakeDb), Arc::new(FakeRpc { block_number: 99 }));
    let row = io.fetch_erc20_token(7, Address::ZERO).await.unwrap();
    let row = row.expect("FakeDb returns a canned row");
    assert_eq!(row.chain, 7);
    assert_eq!(row.symbol.as_deref(), Some("FK"));
}

/// The composite routes `get_block_number` to the `rpc` half.
#[tokio::test]
async fn composite_routes_rpc_call_to_rpc_half() {
    let io = ConstructionIo::new(Arc::new(FakeDb), Arc::new(FakeRpc { block_number: 42 }));
    assert_eq!(io.get_block_number().await.unwrap(), 42);
}

/// The handle's read primitive routes `call` to the `rpc` half and returns the
/// canned bytes — the one call step every construction read crosses.
#[tokio::test]
async fn composite_routes_call_to_rpc_half() {
    use degenbot_rpc::abi::ContractRead;

    let io = ConstructionIo::new(Arc::new(FakeDb), Arc::new(FakeRpc { block_number: 0 }));
    let bytes = io.call(Address::ZERO, Bytes::new(), None).await.unwrap();
    assert_eq!(bytes.as_ref(), &[0xca, 0xfe]);
}

/// A number passes through resolution untouched; a tag resolves against the
/// RPC clock the trait owns (`latest` is the head, `pending` the head + 1,
/// `earliest` block 0).
#[tokio::test]
async fn block_selector_resolves_numbers_and_tags_against_the_clock() {
    let io = ConstructionIo::new(Arc::new(FakeDb), Arc::new(FakeRpc { block_number: 42 }));
    assert_eq!(
        io.resolve_block_selector(BlockSelector::number(7))
            .await
            .unwrap(),
        7
    );
    let parse = |raw: &str| BlockSelector::parse_tag(raw).unwrap();
    assert_eq!(
        io.resolve_block_selector(parse("latest")).await.unwrap(),
        42
    );
    assert_eq!(
        io.resolve_block_selector(parse("pending")).await.unwrap(),
        43
    );
    assert_eq!(
        io.resolve_block_selector(parse("earliest")).await.unwrap(),
        0
    );
}

/// A tag the ladder does not name is refused rather than guessed at.
#[tokio::test]
async fn block_selector_refuses_an_unnamed_tag() {
    let io = ConstructionIo::new(Arc::new(FakeDb), Arc::new(FakeRpc { block_number: 42 }));
    let err = io
        .resolve_block_selector(BlockSelector::tag(alloy::eips::BlockNumberOrTag::Safe))
        .await
        .unwrap_err();
    assert!(matches!(err, ProviderError::InvalidResponse { .. }));
}

// ───────────────────────── NoDb adapter ─────────────────────────

/// `NoDb` returns `None` for every `Option`-returning method, `Vec::new()` for
/// every `Vec`-returning method, `()` for the write-back. The contract is the
/// entire point of the adapter — test it exhaustively.
#[tokio::test]
async fn no_db_returns_none_and_empty_everywhere() {
    let no_db = NoDb;
    assert!(no_db
        .fetch_erc20_token(1, Address::ZERO)
        .await
        .unwrap()
        .is_none());
    assert!(no_db
        .fetch_pool_row(1, Address::ZERO)
        .await
        .unwrap()
        .is_none());
    assert!(no_db
        .fetch_pool_kind("uniswap_v3", 1)
        .await
        .unwrap()
        .is_none());
    assert!(no_db.fetch_token_by_id(1).await.unwrap().is_none());
    assert!(no_db.fetch_exchange(1).await.unwrap().is_none());
    assert!(no_db.fetch_liquidity_positions(1).await.unwrap().is_empty());
    assert!(no_db
        .fetch_pool_manager(1, Address::ZERO)
        .await
        .unwrap()
        .is_none());
    assert!(no_db
        .fetch_v4_pool_by_pool_hash("0x00")
        .await
        .unwrap()
        .is_none());
    assert!(no_db
        .fetch_managed_liquidity_positions(1)
        .await
        .unwrap()
        .is_empty());
    no_db
        .update_erc20_token_metadata(1, "0x00", None, None, None)
        .await
        .unwrap();
}

/// A `ConstructionIo` with `NoDb` for the DB half is a valid no-DB bot handle
/// — `fetch_erc20_token` routes through the composite and returns `None`.
#[tokio::test]
async fn construction_io_with_no_db_returns_none_on_db_call() {
    let io = ConstructionIo::new(Arc::new(NoDb), Arc::new(FakeRpc { block_number: 0 }));
    assert!(io
        .fetch_erc20_token(1, Address::ZERO)
        .await
        .unwrap()
        .is_none());
}

// ───────────────────── DegenbotDbConstruction ─────────────────────

/// The native DB adapter holds a persistent `DegenbotDb` and delegates 1:1.
/// Seed an ERC-20 row into an in-memory DB, fetch it back through the adapter,
/// and prove the held-connection model works end-to-end (no per-call `open`).
#[tokio::test]
async fn degenbot_db_construction_round_trips_erc20_row() {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    // The reader queries by `address.to_checksum(None)`, so the stored string
    // must be the EIP-55 checksum form to round-trip.
    let addr: Address = "0x000000000000000000000000000000000000bEEF"
        .parse()
        .unwrap();
    let addr_str = addr.to_checksum(None);
    db.get_or_create_erc20_token(1, &addr_str, Some("Test"), Some("TST"), Some(18))
        .unwrap();

    let adapter = DegenbotDbConstruction::new(std::sync::Arc::new(db));
    let row = adapter
        .fetch_erc20_token(1, addr)
        .await
        .unwrap()
        .expect("row was seeded");
    assert_eq!(row.chain, 1);
    assert_eq!(row.symbol.as_deref(), Some("TST"));
    assert_eq!(row.decimals, Some(18));
    // NoDb-behaviour on a miss: returns Ok(None), not an error.
    let miss = adapter.fetch_erc20_token(1, Address::ZERO).await.unwrap();
    assert!(miss.is_none());
}

/// `update_erc20_token_metadata` writes back through the held connection.
#[tokio::test]
async fn degenbot_db_construction_updates_erc20_metadata() {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    let addr: Address = "0x000000000000000000000000000000000000C0Ff"
        .parse()
        .unwrap();
    let addr_str = addr.to_checksum(None);
    db.get_or_create_erc20_token(1, &addr_str, None, None, None)
        .unwrap();

    let adapter = DegenbotDbConstruction::new(std::sync::Arc::new(db));
    adapter
        .update_erc20_token_metadata(1, &addr_str, Some("X"), Some("Y"), Some(6))
        .await
        .unwrap();
    let row = adapter
        .fetch_erc20_token(1, addr)
        .await
        .unwrap()
        .expect("row exists");
    assert_eq!(row.name.as_deref(), Some("X"));
    assert_eq!(row.decimals, Some(6));
}

// ───────────────────── AlloyRpcConstruction ─────────────────────

/// The native RPC adapter against an `OfflineProvider` (no live RPC). Proves
/// the adapter delegates 1:1 to `AlloyProvider`'s existing methods — mirrors
/// `tick_assembly/tests.rs`'s offline-provider shape. `get_block_number` +
/// `get_block_timestamp` are the two methods derivable from a minimal
/// recorded fixture.
#[tokio::test]
async fn alloy_rpc_construction_reads_block_number_and_timestamp_offline() {
    // A one-block offline fixture: block 100, timestamp 1_700_000_000, no
    // calls/code. Mirrors the `_MIN_OFFLINE_JSON` shape in the PyBotIo tests.
    let json = r#"{
        "chain_id": 1,
        "block_number": 100,
        "timestamp": 1700000000,
        "calls": {},
        "code": {}
    }"#;
    let offline = degenbot_rpc::offline::OfflineProvider::from_json_str(json).unwrap();
    let provider = offline.as_alloy_provider();
    let adapter = AlloyRpcConstruction::new(provider);

    assert_eq!(adapter.get_block_number().await.unwrap(), 100);
    let ts = adapter
        .get_block_timestamp(100)
        .await
        .unwrap()
        .expect("block 100 is recorded");
    assert_eq!(ts, 1_700_000_000);
}

// ───────────────────── the block-scoped read memo ─────────────────────

/// A fake `RpcConstruction` that counts `eth_call` reads and answers with the
/// block it was asked at (32 bytes of that block's low byte), so a memo hit is
/// both a call-count and a *value* check.
struct CountingRpc {
    calls: std::sync::atomic::AtomicUsize,
    block_number: u64,
}

impl CountingRpc {
    fn new(block_number: u64) -> Arc<Self> {
        Arc::new(Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
            block_number,
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl RpcConstruction for CountingRpc {
    async fn get_block_number(&self) -> Result<u64, ProviderError> {
        Ok(self.block_number)
    }

    async fn get_block(
        &self,
        _block_number: u64,
    ) -> Result<Option<degenbot_rpc::provider::EthBlock>, ProviderError> {
        Ok(None)
    }

    async fn get_block_timestamp(&self, _block_number: u64) -> Result<Option<u64>, ProviderError> {
        Ok(None)
    }

    async fn get_code(
        &self,
        _address: Address,
        _block_number: Option<u64>,
    ) -> Result<Bytes, ProviderError> {
        Ok(Bytes::new())
    }

    async fn get_balance(
        &self,
        _address: Address,
        _block_number: Option<u64>,
    ) -> Result<U256, ProviderError> {
        Ok(U256::ZERO)
    }

    async fn call(
        &self,
        _to: Address,
        _data: Bytes,
        block_number: Option<u64>,
    ) -> Result<Bytes, ProviderError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // The return encodes the block, so a stale cross-block hit would show
        // up as an unexpected value, not just an unexpected call count.
        #[expect(clippy::cast_possible_truncation)]
        let stamp = block_number.unwrap_or(0) as u8;
        Ok(Bytes::from(vec![stamp; 32]))
    }
}

/// The pool address the read-memo tests read from.
const MEMO_TO: Address = alloy::primitives::address!("00000000000000000000000000000000000000A1");

/// The calldata the read-memo tests read with (`tickBitmap(int16)` word 0).
fn memo_calldata() -> Bytes {
    use alloy::sol_types::SolCall;
    Bytes::from(degenbot_rpc::abi::IUniswapV3Pool::tickBitmapCall { wordPosition: 0 }.abi_encode())
}

/// Two construction callers asking the same question at the same block share
/// one chain round-trip — the crawl's repeat read.
#[tokio::test]
async fn a_pinned_read_at_one_block_reaches_the_chain_once() {
    use degenbot_rpc::abi::ContractRead;

    let rpc = CountingRpc::new(0);
    let io = ConstructionIo::new(Arc::new(NoDb), rpc.clone());
    let data = memo_calldata();
    let first = io.call(MEMO_TO, data.clone(), Some(100)).await.unwrap();
    let second = io.call(MEMO_TO, data, Some(100)).await.unwrap();
    assert_eq!(first, second, "the memo serves the recorded return");
    assert_eq!(rpc.calls(), 1, "one block, one read: the repeat is a hit");
}

/// A block move is a MISS: the memo never serves a return recorded at another
/// block.
#[tokio::test]
async fn a_block_boundary_is_a_miss_not_a_stale_hit() {
    use degenbot_rpc::abi::ContractRead;

    let rpc = CountingRpc::new(0);
    let io = ConstructionIo::new(Arc::new(NoDb), rpc.clone());
    let data = memo_calldata();
    let at_100 = io.call(MEMO_TO, data.clone(), Some(100)).await.unwrap();
    let at_101 = io.call(MEMO_TO, data, Some(101)).await.unwrap();
    assert_ne!(at_100, at_101, "the block moved, so the read went to chain");
    assert_eq!(rpc.calls(), 2);
}

/// An unpinned (`latest`) read has no block to key on — the node resolves the
/// head at execution time — so it is never recorded and never served.
#[tokio::test]
async fn an_unpinned_read_never_enters_the_memo() {
    use degenbot_rpc::abi::ContractRead;

    let rpc = CountingRpc::new(7);
    let io = ConstructionIo::new(Arc::new(NoDb), rpc.clone());
    let data = memo_calldata();
    io.call(MEMO_TO, data.clone(), None).await.unwrap();
    io.call(MEMO_TO, data, None).await.unwrap();
    assert_eq!(rpc.calls(), 2, "an unpinned read is always a chain read");
    assert!(
        io.read_memo().is_empty(),
        "nothing recorded without a block"
    );
}

/// Two units in flight at neighbouring blocks each keep their own entry, and
/// neither is served the other's answer.
#[tokio::test]
async fn neighbouring_blocks_keep_their_own_entries() {
    use degenbot_rpc::abi::ContractRead;

    let rpc = CountingRpc::new(0);
    let io = ConstructionIo::new(Arc::new(NoDb), rpc.clone());
    let data = memo_calldata();
    let first = io.call(MEMO_TO, data.clone(), Some(100)).await.unwrap();
    io.call(MEMO_TO, data.clone(), Some(101)).await.unwrap();
    let again = io.call(MEMO_TO, data, Some(100)).await.unwrap();
    assert_eq!(first, again, "block 100's entry survived block 101's reads");
    assert_eq!(rpc.calls(), 2);
}

/// The memo is bounded: an epoch past the entry cap drops the memory instead of
/// growing without limit, and the reads still answer correctly.
#[tokio::test]
async fn the_memo_stays_bounded_and_still_answers() {
    use degenbot_rpc::abi::ContractRead;

    let rpc = CountingRpc::new(0);
    let io = ConstructionIo::new(Arc::new(NoDb), rpc.clone())
        .with_read_memo(Arc::new(ReadMemo::with_bounds(1, 4)));
    let mut values = Vec::new();
    for index in 0u8..8 {
        values.push(
            io.call(MEMO_TO, Bytes::from(vec![0, 0, 0, index]), Some(100))
                .await
                .unwrap(),
        );
    }
    assert_eq!(values.len(), 8);
    assert!(
        io.read_memo().len() <= 4,
        "the entry cap bounds the memo, got {}",
        io.read_memo().len()
    );
}

/// A handle sharing the session memo (how the per-pool tick-word source is
/// attached) reads through the SAME memory as the session handle.
#[tokio::test]
async fn a_handle_sharing_the_memo_reads_through_to_the_same_entries() {
    use degenbot_rpc::abi::ContractRead;

    let rpc = CountingRpc::new(0);
    let session = ConstructionIo::new(Arc::new(NoDb), rpc.clone());
    let attached = ConstructionIo::new(Arc::new(NoDb), rpc.clone())
        .with_read_memo(Arc::clone(session.read_memo()));
    let data = memo_calldata();
    session
        .call(MEMO_TO, data.clone(), Some(100))
        .await
        .unwrap();
    attached.call(MEMO_TO, data, Some(100)).await.unwrap();
    assert_eq!(rpc.calls(), 1, "the second handle hit the session's entry");
}

/// The core's tick-word source is attached per pool but reads through the
/// session's one memo: the word the source backfills is already answered when
/// the session asks the same question at the same block.
#[test]
fn an_attached_word_source_reads_through_the_session_memo() {
    use degenbot_rpc::abi::ContractRead;

    let rpc = CountingRpc::new(0);
    let io = ConstructionIo::new(Arc::new(NoDb), rpc.clone());
    let source = crate::bot_core::pool_builder::tick_source::attached(&io);
    let key = degenbot_pools::TickWordKey::V3 {
        address: MEMO_TO,
        tick_spacing: 1,
    };
    // Block 0 answers with an all-zero bitmap: a known-empty word, one read.
    assert!(source
        .fetch_tick_word(&key, 0, 0)
        .expect("the source reads through the construction io seam")
        .is_none());
    assert_eq!(rpc.calls(), 1);

    // The session asks the same word at the same block: served from the memo
    // the attached source recorded into.
    let data = memo_calldata();
    degenbot_core::runtime::get_runtime()
        .block_on(async { io.call(MEMO_TO, data, Some(0)).await })
        .expect("the memo answers");
    assert_eq!(rpc.calls(), 1, "the attached source shares the memo");
}
