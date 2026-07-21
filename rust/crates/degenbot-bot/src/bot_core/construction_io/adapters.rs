//! Native adapters for the construction-I/O traits + the `NoDb` test fake.
//!
//! Three adapters, one per concern + the no-DB path:
//!
//! - [`NoDb`] — every `DbConstruction` method returns the empty/`None` shape.
//!   The no-DB path (so `ConstructionIo.db` is always `Some`) AND the first
//!   in-memory test fake.
//! - [`DegenbotDbConstruction`] — holds a **persistent** `DegenbotDb` (held
//!   connection, not per-call `DegenbotDb::open`). Deletes the 12×-open
//!   boilerplate that lived on `PyBotIo`.
//! - [`AlloyRpcConstruction`] — wraps `degenbot-rpc`'s [`AlloyProvider`] (the
//!   native alloy path); alloy-only (the legacy non-alloy Python-provider
//!   fallback is dropped at the trait — see the module-level docs in
//!   [`super`]).
//!
//! [`AlloyProvider`]: degenbot_rpc::provider::AlloyProvider

use alloy::primitives::{Address, Bytes, U256};
use async_trait::async_trait;
use degenbot_core::errors::ProviderError;
use degenbot_db::connection::DegenbotDb;
use degenbot_db::error::DbError;
use degenbot_db::rows::{
    Erc20TokenRow, ExchangeRow, InitializationMapRow, LiquidityPoolRow, LiquidityPositionRow,
    ManagedPoolInitializationMapRow, ManagedPoolLiquidityPositionRow, PoolKindRow, PoolManagerRow,
    V4PoolRow,
};
use degenbot_rpc::provider::{AlloyProvider, EthBlock};

use super::{DbConstruction, RpcConstruction};

// ───────────────────────────── NoDb ─────────────────────────────

/// The no-DB adapter: every `DbConstruction` method returns the empty/`None`
/// shape. This is the no-DB path (a `Bot` constructed with no DB uses `NoDb`,
/// so `ConstructionIo.db` is always `Some`) AND the first in-memory test
/// fake — unit tests reach construction-I/O through `NoDb` without a live DB.
#[derive(Debug, Default)]
pub struct NoDb;

#[async_trait]
impl DbConstruction for NoDb {
    async fn fetch_erc20_token(
        &self,
        _chain_id: i64,
        _address: Address,
    ) -> Result<Option<Erc20TokenRow>, DbError> {
        Ok(None)
    }

    async fn fetch_pool_row(
        &self,
        _chain_id: i64,
        _address: Address,
    ) -> Result<Option<LiquidityPoolRow>, DbError> {
        Ok(None)
    }

    async fn fetch_pool_kind(
        &self,
        _kind: &str,
        _pool_id: i64,
    ) -> Result<Option<PoolKindRow>, DbError> {
        Ok(None)
    }

    async fn fetch_token_by_id(&self, _token_id: i64) -> Result<Option<Erc20TokenRow>, DbError> {
        Ok(None)
    }

    async fn fetch_exchange(&self, _exchange_id: i64) -> Result<Option<ExchangeRow>, DbError> {
        Ok(None)
    }

    async fn fetch_liquidity_positions(
        &self,
        _pool_id: i64,
    ) -> Result<Vec<LiquidityPositionRow>, DbError> {
        Ok(Vec::new())
    }

    async fn fetch_initialization_map(
        &self,
        _pool_id: i64,
    ) -> Result<Vec<InitializationMapRow>, DbError> {
        Ok(Vec::new())
    }

    async fn fetch_pool_manager(
        &self,
        _chain_id: i64,
        _address: Address,
    ) -> Result<Option<PoolManagerRow>, DbError> {
        Ok(None)
    }

    async fn fetch_v4_pool_by_pool_hash(
        &self,
        _pool_hash_hex: &str,
    ) -> Result<Option<V4PoolRow>, DbError> {
        Ok(None)
    }

    async fn fetch_managed_liquidity_positions(
        &self,
        _managed_pool_id: i64,
    ) -> Result<Vec<ManagedPoolLiquidityPositionRow>, DbError> {
        Ok(Vec::new())
    }

    async fn fetch_managed_initialization_map(
        &self,
        _managed_pool_id: i64,
    ) -> Result<Vec<ManagedPoolInitializationMapRow>, DbError> {
        Ok(Vec::new())
    }

    async fn update_erc20_token_metadata(
        &self,
        _chain_id: i64,
        _address: &str,
        _name: Option<&str>,
        _symbol: Option<&str>,
        _decimals: Option<i64>,
    ) -> Result<(), DbError> {
        Ok(())
    }
}

// ────────────────────── DegenbotDbConstruction ──────────────────────

/// Native `DbConstruction` adapter holding a **persistent** `DegenbotDb`.
///
/// Replaces `PyBotIo`'s 12× per-call `DegenbotDb::open` boilerplate with one
/// held connection. `DegenbotDb` is already `Send + Sync` (interior
/// `Mutex<Connection>`), so it slots into `Arc<dyn DbConstruction + Send +
/// Sync>` mechanically. Each method delegates 1:1 to the existing
/// `DegenbotDb::fetch_*` / `update_erc20_token_metadata` inherent methods,
/// which already return the core row types + `DbError` — the adapter body is
/// pure delegation (the choreography can no longer silently swallow or
/// re-open per call).
pub struct DegenbotDbConstruction {
    db: DegenbotDb,
}

impl DegenbotDbConstruction {
    #[must_use]
    pub fn new(db: DegenbotDb) -> Self {
        Self { db }
    }
}

#[async_trait]
impl DbConstruction for DegenbotDbConstruction {
    async fn fetch_erc20_token(
        &self,
        chain_id: i64,
        address: Address,
    ) -> Result<Option<Erc20TokenRow>, DbError> {
        self.db.fetch_token_by_address(address, chain_id)
    }

    async fn fetch_pool_row(
        &self,
        chain_id: i64,
        address: Address,
    ) -> Result<Option<LiquidityPoolRow>, DbError> {
        self.db.fetch_pool_by_address(address, chain_id)
    }

    async fn fetch_pool_kind(
        &self,
        kind: &str,
        pool_id: i64,
    ) -> Result<Option<PoolKindRow>, DbError> {
        self.db.fetch_pool_kind(kind, pool_id)
    }

    async fn fetch_token_by_id(&self, token_id: i64) -> Result<Option<Erc20TokenRow>, DbError> {
        self.db.fetch_token_by_id(token_id)
    }

    async fn fetch_exchange(&self, exchange_id: i64) -> Result<Option<ExchangeRow>, DbError> {
        self.db.fetch_exchange(exchange_id)
    }

    async fn fetch_liquidity_positions(
        &self,
        pool_id: i64,
    ) -> Result<Vec<LiquidityPositionRow>, DbError> {
        self.db.fetch_liquidity_positions(pool_id)
    }

    async fn fetch_initialization_map(
        &self,
        pool_id: i64,
    ) -> Result<Vec<InitializationMapRow>, DbError> {
        self.db.fetch_initialization_map(pool_id)
    }

    async fn fetch_pool_manager(
        &self,
        chain_id: i64,
        address: Address,
    ) -> Result<Option<PoolManagerRow>, DbError> {
        self.db.fetch_pool_manager(address, chain_id)
    }

    async fn fetch_v4_pool_by_pool_hash(
        &self,
        pool_hash_hex: &str,
    ) -> Result<Option<V4PoolRow>, DbError> {
        self.db.fetch_v4_pool_by_pool_hash(pool_hash_hex)
    }

    async fn fetch_managed_liquidity_positions(
        &self,
        managed_pool_id: i64,
    ) -> Result<Vec<ManagedPoolLiquidityPositionRow>, DbError> {
        self.db.fetch_managed_liquidity_positions(managed_pool_id)
    }

    async fn fetch_managed_initialization_map(
        &self,
        managed_pool_id: i64,
    ) -> Result<Vec<ManagedPoolInitializationMapRow>, DbError> {
        self.db.fetch_managed_initialization_map(managed_pool_id)
    }

    async fn update_erc20_token_metadata(
        &self,
        chain_id: i64,
        address: &str,
        name: Option<&str>,
        symbol: Option<&str>,
        decimals: Option<i64>,
    ) -> Result<(), DbError> {
        self.db
            .update_erc20_token_metadata(chain_id, address, name, symbol, decimals)
    }
}

// ────────────────────── AlloyRpcConstruction ──────────────────────

/// Native `RpcConstruction` adapter over [`AlloyProvider`] (the alloy path,
/// alloy-only). The provider's methods are already `async fn` returning
/// `ProviderResult<T>`, so the adapter is a thin delegation.
///
/// `call` → `AlloyProvider::eth_call`; both return raw `Bytes` (the
/// `call`/`call_raw` distinction lives in the choreography, not the trait —
/// the trait exposes a single `call` method). `get_block_timestamp` derives
/// from `get_block(n).header.inner.timestamp` (mirrors `PyBotIo`'s native
/// path).
pub struct AlloyRpcConstruction {
    provider: AlloyProvider,
}

impl AlloyRpcConstruction {
    #[must_use]
    pub fn new(provider: AlloyProvider) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl RpcConstruction for AlloyRpcConstruction {
    async fn get_block_number(&self) -> Result<u64, ProviderError> {
        self.provider.get_block_number().await
    }

    async fn get_block(&self, block_number: u64) -> Result<Option<EthBlock>, ProviderError> {
        self.provider.get_block(block_number).await
    }

    async fn get_block_timestamp(&self, block_number: u64) -> Result<Option<u64>, ProviderError> {
        // Derive from `get_block(n).header.inner.timestamp` — no separate RPC.
        // Mirrors `PyBotIo::get_block_timestamp`'s native alloy path.
        let block = self.provider.get_block(block_number).await?;
        Ok(block.map(|b| {
            let ts_u256 = U256::from(b.header.inner.timestamp);
            ts_u256.to::<u64>()
        }))
    }

    async fn get_code(
        &self,
        address: Address,
        block_number: Option<u64>,
    ) -> Result<Bytes, ProviderError> {
        self.provider.get_code(&address, block_number).await
    }

    async fn get_balance(
        &self,
        address: Address,
        block_number: Option<u64>,
    ) -> Result<U256, ProviderError> {
        self.provider.get_balance(&address, block_number).await
    }

    async fn call(
        &self,
        to: Address,
        data: Bytes,
        block_number: Option<u64>,
    ) -> Result<Bytes, ProviderError> {
        self.provider.eth_call(&to, data, block_number).await
    }
}
