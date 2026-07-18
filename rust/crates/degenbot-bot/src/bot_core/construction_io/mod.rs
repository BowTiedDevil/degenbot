//! Construction-I/O core traits (architecture review 2025-07-18 / candidate 1).
//!
//! Two async core traits + a composite [`ConstructionIo`] handle that `Bot`
//! owns (ADR-003: `Bot` is the single state owner; construction-I/O is part of
//! its lifecycle). The standalone-Rust consumer reaches construction-I/O
//! through these traits — no `pyo3` here (the no-pyo3-in-cores invariant,
//! enforced by `just check-no-pyo3-in-cores`).
//!
//! # Shape — mirrors the `TickMapDb` / `TickBootstrapRpc` precedent
//!
//! `tick_assembly` already composes two core traits: [`TickMapDb`] (the
//! DB-read seam, `degenbot_db::snapshot::TickMapDb`) and
//! [`TickBootstrapRpc`] (the RPC-read seam,
//! `degenbot_pools::tick_fetch::TickBootstrapRpc`). Each is one trait per
//! concern, with real adapters. This module follows the same shape: one
//! trait per concern (`DbConstruction` for DB, `RpcConstruction` for RPC),
//! composed into one handle.
//!
//! [`TickMapDb`]: degenbot_db::snapshot::TickMapDb
//! [`TickBootstrapRpc`]: degenbot_pools::tick_fetch::TickBootstrapRpc
//!
//! # Posture — Decision 8 (A), loud errors
//!
//! `tick_assembly` set the canonical posture: DB errors **propagate** as
//! `DbError` (never swallowed at the seam); the choreography decides whether
//! to degrade. This trait inherits that posture unchanged — every method
//! returns `Result<_, DbError>` / `Result<_, ProviderError>`, never an
//! infallible `Option`. The 12x `DegenbotDb::open` boilerplate that lived on
//! `PyBotIo` (per-call open) is replaced by a held `DegenbotDb` in the native
//! adapter ([`DegenbotDbConstruction`]); a [`NoDb`] adapter covers the no-DB
//! path (and doubles as the first in-memory test fake).
//!
//! # Scope (slice A)
//!
//! Only the 19 atomic methods are here: the 12 DB reads/writes + the 7
//! generic RPC methods. The 27 choreographed encode→call→decode wrappers
//! (`fetch_v2_reserves`, `fetch_erc20_metadata`, …) stay on `PyBotIo` for
//! this slice, composing over the trait's `call` / DB primitives. They move
//! core-side in a follow-up (the builder-choreography port). See the
//! migration note.
//!
//! # Async shape
//!
//! The traits are `async fn` — `degenbot-rpc`'s `RateLimitingProvider` is
//! already async, so the native [`RpcConstruction`] adapter is a thin
//! delegation. The `PyO3` boundary does the `block_on` (release GIL → drive
//! the future → wrap result), matching how `PyBotIo::forward_call_to_provider`
//! already bridges to the native alloy provider via `get_runtime().block_on`.

use alloy::primitives::{Address, Bytes, U256};
use async_trait::async_trait;
use degenbot_core::errors::ProviderError;
use degenbot_db::error::DbError;
use degenbot_db::rows::{
    Erc20TokenRow, ExchangeRow, InitializationMapRow, LiquidityPoolRow, LiquidityPositionRow,
    ManagedPoolInitializationMapRow, ManagedPoolLiquidityPositionRow, PoolKindRow, PoolManagerRow,
    V4PoolRow,
};
use degenbot_rpc::provider::EthBlock;

// Re-export the traits + handle at the `bot_core` root for consumers. The
// adapters (`NoDb`, `DegenbotDbConstruction`, the RPC native adapter) live in
// `adapters.rs`.
pub mod adapters;
pub use self::adapters::{AlloyRpcConstruction, DegenbotDbConstruction, NoDb};
pub use self::handle::ConstructionIo;

/// The 12 construction-time DB reads/writes — one trait per concern.
///
/// Every method returns `degenbot_db::rows::*` core row types directly (no
/// `Py*` mirror at the seam) and propagates [`DbError`] **loudly** (Decision
/// 8 (A) — the trait never swallows; the choreography decides whether to
/// degrade). The native adapter [`DegenbotDbConstruction`] holds a persistent
/// `DegenbotDb` (held connection, not per-call `DegenbotDb::open`); the no-DB
/// path is the [`NoDb`] adapter, so `ConstructionIo.db` is always `Some`.
///
/// Method names mirror `DegenbotDb::fetch_*` where possible; the singular
/// `fetch_initialization_map` / `fetch_managed_initialization_map` match the
/// `DegenbotDb` inherent-method names (`PyBotIo`'s plural Python-visible
/// names are a choreography-level alias, not a trait concern).
#[async_trait]
pub trait DbConstruction: Send + Sync {
    async fn fetch_erc20_token(
        &self,
        chain_id: i64,
        address: Address,
    ) -> Result<Option<Erc20TokenRow>, DbError>;

    async fn fetch_pool_row(
        &self,
        chain_id: i64,
        address: Address,
    ) -> Result<Option<LiquidityPoolRow>, DbError>;

    async fn fetch_pool_kind(
        &self,
        kind: &str,
        pool_id: i64,
    ) -> Result<Option<PoolKindRow>, DbError>;

    async fn fetch_token_by_id(&self, token_id: i64) -> Result<Option<Erc20TokenRow>, DbError>;

    async fn fetch_exchange(&self, exchange_id: i64) -> Result<Option<ExchangeRow>, DbError>;

    async fn fetch_liquidity_positions(
        &self,
        pool_id: i64,
    ) -> Result<Vec<LiquidityPositionRow>, DbError>;

    async fn fetch_initialization_map(
        &self,
        pool_id: i64,
    ) -> Result<Vec<InitializationMapRow>, DbError>;

    async fn fetch_pool_manager(
        &self,
        chain_id: i64,
        address: Address,
    ) -> Result<Option<PoolManagerRow>, DbError>;

    async fn fetch_v4_pool_by_pool_hash(
        &self,
        pool_hash_hex: &str,
    ) -> Result<Option<V4PoolRow>, DbError>;

    async fn fetch_managed_liquidity_positions(
        &self,
        managed_pool_id: i64,
    ) -> Result<Vec<ManagedPoolLiquidityPositionRow>, DbError>;

    async fn fetch_managed_initialization_map(
        &self,
        managed_pool_id: i64,
    ) -> Result<Vec<ManagedPoolInitializationMapRow>, DbError>;

    /// Write back an ERC-20 token row's metadata (`name` / `symbol` /
    /// `decimals`) by `(chain_id, address)`. Each `None` field writes `NULL`
    /// (matches the ORM attribute assignment that preceded this trait).
    async fn update_erc20_token_metadata(
        &self,
        chain_id: i64,
        address: &str,
        name: Option<&str>,
        symbol: Option<&str>,
        decimals: Option<i64>,
    ) -> Result<(), DbError>;
}

/// The 7 generic RPC methods — the atomic surface the choreography composes
/// over. Alloy-shaped signature types; propagates [`ProviderError`].
///
/// `call` / `call_raw` both return raw `Bytes` here — the distinction between
/// "decoded return" and "raw return" lives in the choreography (still on
/// `PyBotIo` this slice), not the trait. The native adapter backs onto
/// `degenbot-rpc`'s `RateLimitingProvider` (alloy-only — the legacy
/// non-alloy Python-provider fallback is dropped at the trait; the
/// choreography's fallback survives temporarily and is deleted with the
/// builder-choreography port).
#[async_trait]
pub trait RpcConstruction: Send + Sync {
    async fn get_block_number(&self) -> Result<u64, ProviderError>;

    async fn get_block(&self, block_number: u64) -> Result<Option<EthBlock>, ProviderError>;

    /// Derive the timestamp from `get_block(block_number).header.timestamp`.
    async fn get_block_timestamp(&self, block_number: u64) -> Result<Option<u64>, ProviderError>;

    async fn get_code(
        &self,
        address: Address,
        block_number: Option<u64>,
    ) -> Result<Bytes, ProviderError>;

    async fn get_balance(
        &self,
        address: Address,
        block_number: Option<u64>,
    ) -> Result<U256, ProviderError>;

    /// `eth_call(to, data, block)` — returns the raw return bytes.
    async fn call(
        &self,
        to: Address,
        data: Bytes,
        block_number: Option<u64>,
    ) -> Result<Bytes, ProviderError>;
}

mod handle;

// The adapters (`NoDb`, `DegenbotDbConstruction`, the RPC native adapter) +
// their tests live in `adapters.rs` (next task) + `tests.rs`.
#[cfg(test)]
mod tests;
