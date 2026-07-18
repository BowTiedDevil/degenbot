//! Tests for the `ConstructionIo` composite + the trait surface (task 1 / 2)
//! + the adapter tests (`NoDb`, `DegenbotDbConstruction`,
//!   `AlloyRpcConstruction`).

use std::sync::Arc;

use alloy::primitives::{Address, Bytes, U256};
use async_trait::async_trait;
use degenbot_core::errors::ProviderError;
use degenbot_db::connection::DegenbotDb;
use degenbot_db::rows::{
    Erc20TokenRow, ExchangeRow, InitializationMapRow, LiquidityPoolRow, LiquidityPositionRow,
    ManagedPoolInitializationMapRow, ManagedPoolLiquidityPositionRow, PoolKindRow, PoolManagerRow,
    V4PoolRow,
};

use super::adapters::{AlloyRpcConstruction, DegenbotDbConstruction, NoDb};
use super::handle::ConstructionIo;
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

    async fn fetch_initialization_map(
        &self,
        _pool_id: i64,
    ) -> Result<Vec<InitializationMapRow>, degenbot_db::error::DbError> {
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

    async fn fetch_managed_initialization_map(
        &self,
        _managed_pool_id: i64,
    ) -> Result<Vec<ManagedPoolInitializationMapRow>, degenbot_db::error::DbError> {
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

/// The composite routes `call` to the `rpc` half and returns the canned bytes.
#[tokio::test]
async fn composite_routes_call_to_rpc_half() {
    let io = ConstructionIo::new(Arc::new(FakeDb), Arc::new(FakeRpc { block_number: 0 }));
    let bytes = io.call(Address::ZERO, Bytes::new(), None).await.unwrap();
    assert_eq!(bytes.as_ref(), &[0xca, 0xfe]);
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
    assert!(no_db.fetch_initialization_map(1).await.unwrap().is_empty());
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
    assert!(no_db
        .fetch_managed_initialization_map(1)
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

    let adapter = DegenbotDbConstruction::new(db);
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

    let adapter = DegenbotDbConstruction::new(db);
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
