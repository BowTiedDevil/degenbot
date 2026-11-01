#![expect(clippy::missing_errors_doc)]
//! The composite handle holding the two trait objects.
use std::sync::Arc;

use alloy::primitives::{Address, Bytes, U256};
use degenbot_core::errors::ProviderError;
use degenbot_db::error::DbError;
use degenbot_db::rows::{
    Erc20TokenRow, ExchangeRow, LiquidityPoolRow, LiquidityPositionRow,
    ManagedPoolLiquidityPositionRow, PoolKindRow, PoolManagerRow, V4PoolRow,
};
use degenbot_rpc::provider::EthBlock;

use super::block_selector::BlockSelector;
use super::tick_pace::TickFetchPace;
use super::{DbConstruction, ReadMemo, RpcConstruction};

/// The composite construction-I/O handle, held by `Bot` (ADR-003).
///
/// `db` is always `Some` — the no-DB path is the [`NoDb`][super::NoDb]
/// adapter, not a call-site `Option`. Both halves are `Arc<dyn Trait + Send +
/// Sync>` so the handle clones cheaply across builder threads.
///
/// The methods on `ConstructionIo` itself are thin forwarders — the real
/// behaviour lives behind the trait objects. Builders receive `&ConstructionIo`
/// and call the methods directly (trait dispatch), never needing to know
/// which adapter backs each half.
pub struct ConstructionIo {
    pub db: Arc<dyn DbConstruction + Send + Sync>,
    pub rpc: Arc<dyn RpcConstruction + Send + Sync>,
    /// The tick-RPC pace gate: `builders.tick_fetch_min_interval_ms`, honoured
    /// here so every construction path paces the same way (see
    /// [`TickFetchPace`]).
    tick_fetch_pace: TickFetchPace,
    /// The block-scoped memory of construction reads (see [`ReadMemo`]). Shared
    /// through an `Arc`, so the per-pool handles a build attaches read through
    /// the session's ONE memory rather than starting their own.
    read_memo: Arc<ReadMemo>,
}

impl ConstructionIo {
    /// The handle the process's resolved verdict paces.
    ///
    /// The pace comes from the installed config (the schema's declared default
    /// when nothing is installed), so an operator's `builders.` key reaches a
    /// construction path without the caller forwarding it.
    #[must_use]
    pub fn new(
        db: Arc<dyn DbConstruction + Send + Sync>,
        rpc: Arc<dyn RpcConstruction + Send + Sync>,
    ) -> Self {
        Self {
            db,
            rpc,
            tick_fetch_pace: TickFetchPace::from_config(
                &degenbot_config::holder::config().builders,
            ),
            read_memo: Arc::new(ReadMemo::new()),
        }
    }

    /// Share an existing read memo instead of starting a fresh one — how a
    /// handle derived from a session (the per-pool tick-word source) reads
    /// through the session's memory.
    #[must_use]
    pub fn with_read_memo(mut self, memo: Arc<ReadMemo>) -> Self {
        self.read_memo = memo;
        self
    }

    /// The handle's read memo.
    #[must_use]
    pub fn read_memo(&self) -> &Arc<ReadMemo> {
        &self.read_memo
    }

    /// Override the pace — the construction-time test knob.
    #[must_use]
    pub fn with_tick_fetch_pace(mut self, pace: TickFetchPace) -> Self {
        self.tick_fetch_pace = pace;
        self
    }

    /// The tick-RPC pace gate every tick fetch crosses.
    #[must_use]
    pub fn tick_fetch_pace(&self) -> &TickFetchPace {
        &self.tick_fetch_pace
    }
}

// Forwarders — the composite is the single call surface builders reach. Each
// is a one-line delegation so the trait dispatch stays in one place (the
// `Arc<dyn …>` field), and callers don't repeat the `.db.` / `.rpc.` hop.
impl ConstructionIo {
    pub async fn fetch_erc20_token(
        &self,
        chain_id: i64,
        address: Address,
    ) -> Result<Option<Erc20TokenRow>, DbError> {
        self.db.fetch_erc20_token(chain_id, address).await
    }

    pub async fn fetch_pool_row(
        &self,
        chain_id: i64,
        address: Address,
    ) -> Result<Option<LiquidityPoolRow>, DbError> {
        self.db.fetch_pool_row(chain_id, address).await
    }

    pub async fn fetch_pool_kind(
        &self,
        kind: &str,
        pool_id: i64,
    ) -> Result<Option<PoolKindRow>, DbError> {
        self.db.fetch_pool_kind(kind, pool_id).await
    }

    pub async fn fetch_token_by_id(&self, token_id: i64) -> Result<Option<Erc20TokenRow>, DbError> {
        self.db.fetch_token_by_id(token_id).await
    }

    pub async fn fetch_exchange(&self, exchange_id: i64) -> Result<Option<ExchangeRow>, DbError> {
        self.db.fetch_exchange(exchange_id).await
    }

    pub async fn fetch_liquidity_positions(
        &self,
        pool_id: i64,
    ) -> Result<Vec<LiquidityPositionRow>, DbError> {
        self.db.fetch_liquidity_positions(pool_id).await
    }

    pub async fn fetch_pool_manager(
        &self,
        chain_id: i64,
        address: Address,
    ) -> Result<Option<PoolManagerRow>, DbError> {
        self.db.fetch_pool_manager(chain_id, address).await
    }

    pub async fn fetch_v4_pool_by_pool_hash(
        &self,
        pool_hash_hex: &str,
    ) -> Result<Option<V4PoolRow>, DbError> {
        self.db.fetch_v4_pool_by_pool_hash(pool_hash_hex).await
    }

    pub async fn fetch_managed_liquidity_positions(
        &self,
        managed_pool_id: i64,
    ) -> Result<Vec<ManagedPoolLiquidityPositionRow>, DbError> {
        self.db
            .fetch_managed_liquidity_positions(managed_pool_id)
            .await
    }

    pub async fn update_erc20_token_metadata(
        &self,
        chain_id: i64,
        address: &str,
        name: Option<&str>,
        symbol: Option<&str>,
        decimals: Option<i64>,
    ) -> Result<(), DbError> {
        self.db
            .update_erc20_token_metadata(chain_id, address, name, symbol, decimals)
            .await
    }

    pub async fn get_block_number(&self) -> Result<u64, ProviderError> {
        self.rpc.get_block_number().await
    }

    pub async fn get_block(&self, block_number: u64) -> Result<Option<EthBlock>, ProviderError> {
        self.rpc.get_block(block_number).await
    }

    pub async fn get_block_timestamp(
        &self,
        block_number: u64,
    ) -> Result<Option<u64>, ProviderError> {
        self.rpc.get_block_timestamp(block_number).await
    }

    pub async fn get_code(
        &self,
        address: Address,
        block_number: Option<u64>,
    ) -> Result<Bytes, ProviderError> {
        self.rpc.get_code(address, block_number).await
    }

    pub async fn get_balance(
        &self,
        address: Address,
        block_number: Option<u64>,
    ) -> Result<U256, ProviderError> {
        self.rpc.get_balance(address, block_number).await
    }

    /// Resolve a typed [`BlockSelector`] to a concrete block number against
    /// the RPC clock the `rpc` half owns.
    pub async fn resolve_block_selector(
        &self,
        selector: BlockSelector,
    ) -> Result<u64, ProviderError> {
        self.rpc.resolve_block_selector(selector).await
    }
}
