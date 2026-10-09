//! The market-reset module: ONE transaction purges a market's populated state
//! and re-seeds its cold-boot substrate, then reports what it read back.
//!
//! The glossary term "Market reset" means a purged market is structurally
//! identical to a freshly activated one. This module composes the two
//! primitives that get there — the purge (`degenbot_db`'s
//! `reset_aave_market_on_conn`, which clears every row the updater populated
//! and rewinds the cursor to the bootstrap block) and the activation seam
//! (`activate_aave_market_on_conn`, which completes the surviving market row
//! and its address-provider / GHO substrate) — inside the ONE transaction the
//! caller owns.
//!
//! The returned [`MarketResetFacts`] are read back from that same transaction
//! after the writes; a constant would be a lie the moment a write changed
//! shape. The reset never runs an update: the next `aave update` cold-boots
//! from the cursor this leaves.

use alloy::primitives::Address;
use degenbot_core::runtime::get_runtime;
use degenbot_db::{DbError, DegenbotDb};
use degenbot_rpc::provider::AlloyProvider;
use rusqlite::Connection;

use super::activate::{
    activate_aave_market_on_conn, fetch_activation_metadata, ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK,
};
use super::RunError;

/// The facts a reset verifies by reading the transaction it just wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketResetFacts {
    /// The market row's `active` flag after the reset — read from the row, not
    /// assumed.
    pub market_activated: bool,
    /// Whether the `POOL_ADDRESS_PROVIDER` contract row is present after the
    /// reset. That row is the cold-boot bootstrap's fetch anchor.
    pub contract_row_present: bool,
    /// The cursor (`last_update_block`) the reset left — the bootstrap block on
    /// the shipped deployment, or `None` when the row carries no stamp.
    pub last_update_block: Option<u64>,
}

/// Purge `market_id` and re-seed its cold-boot substrate in ONE
/// caller-supplied transaction, returning the facts read back from that same
/// transaction.
///
/// The market row must exist; its `chain_id` is read from that row, so the
/// re-seed completes the SAME market the purge cleared (never a sibling row).
/// `provider` supplies the activation RPC — the on-chain market name plus the
/// GHO token's metadata — exactly as [`super::activate_aave_market`] does, so
/// the substrate written here matches a fresh activation.
///
/// The purge and the re-seed run on `conn`, which the caller passes as its
/// open `rusqlite::Transaction` (its `&Connection`). The caller commits; a
/// failure drops the transaction and the database stays byte-identical.
///
/// # Args
///
/// - `conn` — the caller's transaction.
/// - `market_id` — the `aave_v3_markets.id` to reset.
/// - `pool_address_provider` — the checksummed address provider the bootstrap
///   fetches `ProxyCreated` events from.
/// - `gho_token_address` — the chain's GHO token address.
/// - `provider` — the injected transport (ADR-068 D5).
///
/// # Errors
///
/// - [`RunError::MarketNotFound`] when no market row matches `market_id`.
/// - [`RunError::Provider`] on an RPC failure during the metadata fetch.
/// - [`RunError::Db`] on a query failure, or when the on-chain market name
///   resolves to a row other than `market_id` (the re-seed would otherwise
///   strand the purged market, leaving it inactive with a rewound cursor).
pub fn reset_market_to_cold_boot(
    conn: &Connection,
    market_id: i64,
    pool_address_provider: &str,
    gho_token_address: &str,
    provider: &AlloyProvider,
) -> Result<MarketResetFacts, RunError> {
    let chain_id: i64 = conn
        .query_row(
            "SELECT chain_id FROM aave_v3_markets WHERE id = ?1",
            rusqlite::params![market_id],
            |row| row.get(0),
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => RunError::MarketNotFound(market_id),
            other => RunError::Db(DbError::Sqlite(other)),
        })?;

    let ap_address: Address = pool_address_provider.parse().map_err(|_| {
        RunError::Db(DbError::Decode(format!(
            "invalid pool_address_provider address: {pool_address_provider}"
        )))
    })?;
    let gho_address: Address = gho_token_address.parse().map_err(|_| {
        RunError::Db(DbError::Decode(format!(
            "invalid gho_token_address: {gho_token_address}"
        )))
    })?;

    // The activation RPC runs first, ahead of any write; the purge and the
    // re-seed then ride the caller's transaction as pure substrate.
    let metadata = get_runtime().block_on(fetch_activation_metadata(
        provider,
        &ap_address,
        &gho_address,
    ))?;

    DegenbotDb::reset_aave_market_on_conn(conn, market_id, ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK)?;

    let reseeded_id = activate_aave_market_on_conn(
        conn,
        chain_id,
        &metadata.market_name,
        pool_address_provider,
        gho_token_address,
        metadata.gho_name.as_deref(),
        metadata.gho_symbol.as_deref(),
        metadata.gho_decimals,
        ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK,
    )?;

    // The re-seed keys the market by its on-chain name. When that name
    // resolves to a different row, the purge's market would be left inactive
    // with a rewound cursor — refuse rather than report a half-reset.
    if reseeded_id != market_id {
        return Err(RunError::Db(DbError::Decode(format!(
            "reset of market {market_id} re-seeded id {reseeded_id}: the on-chain name {} names another row",
            metadata.market_name
        ))));
    }

    read_reset_facts(conn, market_id)
}

/// Read the reset facts from `conn` — the same transaction the reset wrote.
///
/// # Errors
///
/// Returns [`RunError::Db`] when the market row is missing after the re-seed
/// or a read fails.
fn read_reset_facts(conn: &Connection, market_id: i64) -> Result<MarketResetFacts, RunError> {
    use rusqlite::OptionalExtension;

    let row: Option<(bool, Option<i64>)> = conn
        .query_row(
            "SELECT active, last_update_block FROM aave_v3_markets WHERE id = ?1",
            rusqlite::params![market_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(DbError::from)?;

    let (market_activated, stamp) = row.ok_or_else(|| {
        RunError::Db(DbError::MissingRow(format!(
            "aave_v3_markets id={market_id} (reset verification)"
        )))
    })?;

    let contract_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM aave_v3_contracts \
         WHERE market_id = ?1 AND name = 'POOL_ADDRESS_PROVIDER'",
            rusqlite::params![market_id],
            |r| r.get(0),
        )
        .map_err(DbError::from)?;

    let last_update_block = match stamp {
        Some(value) => Some(u64::try_from(value).map_err(|_| {
            RunError::Db(DbError::Decode(format!(
                "aave_v3_markets id={market_id} last_update_block {value} is negative"
            )))
        })?),
        None => None,
    };

    Ok(MarketResetFacts {
        market_activated,
        contract_row_present: contract_rows == 1,
        last_update_block,
    })
}
