//! The driver's activation stage: `activate_aave_market` /
//! `deactivate_aave_market` + the one-transaction market-seed substrate fns.

use std::path::Path;
use std::sync::Arc;

use alloy::primitives::Address;
use degenbot_core::runtime::get_runtime;
use degenbot_db::{DbError, DegenbotDb};
use degenbot_rpc::provider::AlloyProvider;
use rusqlite::Connection;

use super::{RunError, RPC_MAX_RETRIES};
#[cfg(test)]
use rusqlite::params;

// ── Market activation / deactivation (MPI6Q3) ─────────────────────────────
//
// The one-time market-activation path — the LAST Python ORM writer on the
// Aave path after the §4.2 retirement (CZM7TI). These substrate fns seed the
// `aave_v3_markets` row + the `POOL_ADDRESS_PROVIDER` contract row + the GHO
// `erc20_tokens` row (with caller-supplied metadata) + a bare `aave_gho_tokens`
// row (token_id only; the discount columns are filled later by the GHO-config
// apply fns). Pure SQL — NO RPC. The orchestrator `activate_aave_market`
// (below) does the RPC fetches (`getMarketId()` + ERC20 metadata) then calls
// `activate_aave_market_on_conn` in ONE transaction.
//
// Idempotent: re-activating an existing market sets `active = true` + does
// NOT insert duplicate contract / GHO rows (mirrors the Python
// `activate_ethereum_aave_v3`'s `if market is not None: market.active = True`
// branch).

/// The block the Aave V3 Ethereum mainnet `PoolAddressProvider` was deployed
/// (TX `0x75fb6e6b…`). The activation path stamps `last_update_block` to the
/// PARENT block (`16_291_070`) so the updater's chunk loop starts AT the
/// deployment block (the `ProxyCreated` events the bootstrap pass resolves
/// fire in `16_291_071`).
const ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK: i64 = 16_291_070;

/// Seed (or re-activate) an Aave V3 market in ONE transaction.
///
/// - If the market `(chain_id, market_name)` already exists: set `active =
///   true` (no duplicate contract / GHO rows).
/// - If new: INSERT `aave_v3_markets` (`active = true`, `last_update_block =
///   bootstrap_block`), then INSERT the `POOL_ADDRESS_PROVIDER` contract row
///   (idempotent), then get-or-create the GHO `erc20_tokens` row (with
///   metadata `name`/`symbol`/`decimals`), then INSERT a bare
///   `aave_gho_tokens` row referencing it (if absent).
///
/// Returns the market `id`. Pure substrate — NO RPC. The orchestrator
/// [`activate_aave_market`] fetches the market name + GHO metadata via RPC
/// then calls this.
///
/// # Errors
///
/// Returns [`DbError`] on a `SQLite` query failure.
#[expect(clippy::too_many_arguments)]
pub(crate) fn activate_aave_market_on_conn(
    conn: &Connection,
    chain_id: i64,
    market_name: &str,
    pool_address_provider: &str,
    gho_token_address: &str,
    gho_name: Option<&str>,
    gho_symbol: Option<&str>,
    gho_decimals: Option<i64>,
    bootstrap_block: i64,
) -> Result<i64, DbError> {
    use rusqlite::OptionalExtension;

    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM aave_v3_markets WHERE chain_id = ?1 AND name = ?2",
            rusqlite::params![chain_id, market_name],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(id) = existing {
        conn.execute(
            "UPDATE aave_v3_markets SET active = 1 WHERE id = ?1",
            rusqlite::params![id],
        )?;
        Ok(id)
    } else {
        conn.execute(
            "INSERT INTO aave_v3_markets (chain_id, name, active, last_update_block) \
                 VALUES (?1, ?2, 1, ?3)",
            rusqlite::params![chain_id, market_name, bootstrap_block],
        )?;
        let market_id = conn.last_insert_rowid();

        DegenbotDb::apply_contract_inserted_if_absent_on_conn(
            conn,
            market_id,
            "POOL_ADDRESS_PROVIDER",
            pool_address_provider,
            None,
        )?;

        // Seed the GHO erc20 row WITH metadata (the orchestrator RPC-
        // fetched it). Must precede the gho-token row (FK).
        let _gho_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
            conn,
            chain_id,
            gho_token_address,
            gho_name,
            gho_symbol,
            gho_decimals,
        )?;
        // Bare `aave_gho_tokens` row (token_id only). `v_token_id` /
        // discount columns are filled later by the GHO-config apply fns.
        DegenbotDb::get_or_create_gho_token_on_conn(conn, chain_id, gho_token_address)?;
        Ok(market_id)
    }
}

/// Set `active = false` for `market_id` in ONE transaction. The deactivation
/// path — pure substrate, NO RPC.
///
/// # Errors
///
/// Returns [`DbError::MissingRow`] if `market_id` doesn't exist, or
/// [`DbError::Sqlite`] on a query failure.
pub(crate) fn deactivate_aave_market_on_conn(
    conn: &Connection,
    market_id: i64,
) -> Result<(), DbError> {
    let updated = conn.execute(
        "UPDATE aave_v3_markets SET active = 0 WHERE id = ?1",
        rusqlite::params![market_id],
    )?;
    if updated == 0 {
        return Err(DbError::MissingRow(format!(
            "aave_v3_markets id={market_id} (deactivate target)"
        )));
    }
    Ok(())
}

/// The result of a successful [`activate_aave_market`] call.
#[derive(Debug, Clone)]
pub struct ActivatedMarket {
    /// The `aave_v3_markets.id` (the FK every updater fn references).
    pub market_id: i64,
    /// The on-chain `getMarketId()` return (e.g. `"Aave Ethereum Market"`).
    pub market_name: String,
    /// `true` if the market was newly created; `false` if it pre-existed
    /// (re-activation — only `active` was flipped to `true`).
    pub created: bool,
}

/// Activate (or re-activate) an Aave V3 market — the ONE-TIME seed the chunk
/// loop's [`run_aave_update`] bootstraps from.
///
/// RPC fetches (`getMarketId()` on the pool address provider + the GHO
/// token's `name()`/`symbol()`/`decimals()`), then ONE transaction seeds the
/// `aave_v3_markets` row (+ the `POOL_ADDRESS_PROVIDER` contract + the GHO
/// `erc20_tokens` + `aave_gho_tokens` rows) via [`activate_aave_market_on_conn`].
/// Idempotent: re-activating an existing market sets `active = true` + inserts
/// no duplicate rows.
///
/// This is the Rust-owned replacement for the Python
/// `activate_ethereum_aave_v3` (commands.py) — the last ORM writer on the Aave
/// path after the §4.2 retirement (CZM7TI). A standalone Rust consumer (`cargo
/// add degenbot`) can activate a market without Python.
///
/// # Args
///
/// - `database_path` — the writeable `DegenbotDb` path.
/// - `chain_id` — the chain.
/// - `pool_address_provider` — the `PoolAddressProvider` contract address
///   (checksummed; stored verbatim).
/// - `gho_token_address` — the chain's GHO token address (checksummed).
/// - `rpc_url` — the HTTP RPC endpoint.
///
/// # Errors
///
/// Returns [`RunError::Provider`] on an RPC failure or [`RunError::Db`] on a
/// DB failure.
///
/// # Shared runtime
///
/// The RPC half runs as ONE future under
/// `degenbot_core::runtime::get_runtime().block_on` at this entry fn
/// (mirrors [`run_aave_update`]'s shape). That `block_on` is legal on the bare
/// `PyO3` fleet threads + the CLI main thread (no ambient tokio context); it
/// MUST NOT be called from within ANY `tokio` runtime context — nested
/// `block_on` panics, the shared runtime included.
pub fn activate_aave_market(
    database_path: &Path,
    chain_id: i64,
    pool_address_provider: &str,
    gho_token_address: &str,
    rpc_url: &str,
) -> Result<ActivatedMarket, RunError> {
    let (db, _state) = DegenbotDb::open_for_writes(database_path)?;

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

    // The ONE `get_runtime().block_on` — the RPC steps are `.await`s inside
    // the driver future, not sequential ad-hoc `block_on`s.
    get_runtime().block_on(activate_aave_market_rpc(
        &db,
        chain_id,
        pool_address_provider,
        gho_token_address,
        rpc_url,
        ap_address,
        gho_address,
    ))
}

/// The RPC half of [`activate_aave_market`] — the ONE future under
/// `get_runtime().block_on`. The DB seed transaction runs synchronously at
/// the end (no `.await` while the `db.lock()` guard is held — no
/// `await_holding_lock` exposure).
///
/// # Errors
///
/// See [`activate_aave_market`].
async fn activate_aave_market_rpc(
    db: &DegenbotDb,
    chain_id: i64,
    pool_address_provider: &str,
    gho_token_address: &str,
    rpc_url: &str,
    ap_address: Address,
    gho_address: Address,
) -> Result<ActivatedMarket, RunError> {
    let provider = Arc::new(AlloyProvider::new(rpc_url, RPC_MAX_RETRIES).await?);

    // 1. RPC: `getMarketId()` on the PoolAddressProvider (a dynamic `string`
    //    return — reuse the same decoder as `fetch_erc20_string_metadata`).
    let market_name = fetch_market_id(&provider, &ap_address).await?;

    // 2. RPC: GHO token `name()`/`symbol()`/`decimals()` (string + bytes32 +
    //    uint256 fallbacks). `block_number` is irrelevant (token metadata is
    //    immutable) — pass `None` for latest.
    let preview_block = provider.get_block_number().await?;
    let (gho_name, gho_symbol, gho_decimals) =
        crate::config_dispatch::fetch_erc20_metadata(&provider, &gho_address, preview_block).await;

    // 3. ONE transaction: seed/activate the market + contract + GHO rows.
    let (market_id, created) = {
        use rusqlite::OptionalExtension;
        let mut guard = db.lock();
        let tx = guard.transaction().map_err(DbError::from)?;
        // Detect re-activation BEFORE the substrate fn (it returns the same id
        // either way; we report `created` for the caller).
        let existed = tx
            .query_row(
                "SELECT 1 FROM aave_v3_markets WHERE chain_id = ?1 AND name = ?2",
                rusqlite::params![chain_id, market_name],
                |_| Ok(()),
            )
            .optional()
            .map_err(DbError::from)?
            .is_some();
        let id = activate_aave_market_on_conn(
            &tx,
            chain_id,
            &market_name,
            pool_address_provider,
            gho_token_address,
            gho_name.as_deref(),
            gho_symbol.as_deref(),
            gho_decimals,
            ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK,
        )?;
        tx.commit().map_err(DbError::from)?;
        (id, !existed)
    };

    Ok(ActivatedMarket {
        market_id,
        market_name,
        created,
    })
}

/// RPC-fetch `getMarketId()` on the `PoolAddressProvider` (a dynamic `string`
/// return). Reuses [`decode_dynamic_string`] (the same ABI decoder
/// `fetch_erc20_string_metadata` uses). Attempts the lowercase selector first,
/// then the uppercase fallback.
async fn fetch_market_id(
    provider: &AlloyProvider,
    ap_address: &Address,
) -> Result<String, RunError> {
    for func in ["getMarketId()", "GETMARKETID()"] {
        let calldata = crate::config_dispatch::encode_no_arg_call(func);
        if let Ok(ret) = provider.eth_call(ap_address, calldata, None).await {
            if let Some(s) = crate::config_dispatch::decode_dynamic_string(&ret) {
                return Ok(s);
            }
            // bytes32 fallback (some impls return a right-padded bytes32).
            if ret.len() >= 32 {
                let s = String::from_utf8_lossy(&ret[..32])
                    .trim_end_matches('\0')
                    .to_string();
                if !s.is_empty() {
                    return Ok(s);
                }
            }
        }
    }
    Err(RunError::Db(DbError::Decode(format!(
        "getMarketId() returned no decodable string for {ap_address:?}"
    ))))
}

/// Deactivate an Aave V3 market — set `active = false`. The thin Rust-owned
/// replacement for the Python `deactivate_mainnet_aave_v3` (commands.py).
///
/// # Args
///
/// - `database_path` — the writeable `DegenbotDb` path.
/// - `market_id` — the `aave_v3_markets.id` to deactivate.
///
/// # Errors
///
/// Returns [`RunError::MarketNotFound`] if `market_id` doesn't exist, or
/// [`RunError::Db`] on a DB failure.
pub fn deactivate_aave_market(database_path: &Path, market_id: i64) -> Result<(), RunError> {
    let (db, _state) = DegenbotDb::open_for_writes(database_path)?;
    {
        let mut guard = db.lock();
        let tx = guard.transaction().map_err(DbError::from)?;
        deactivate_aave_market_on_conn(&tx, market_id).map_err(|e| match e {
            DbError::MissingRow(_) => RunError::MarketNotFound(market_id),
            other => RunError::Db(other),
        })?;
        tx.commit().map_err(DbError::from)?;
    }
    Ok(())
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    // ── Market activation substrate (MPI6Q3) ──────────────────────────────

    /// A fresh in-memory writeable DB with NO pre-seeded market (the
    /// activation path seeds it from scratch).
    fn fresh_db_empty() -> DegenbotDb {
        DegenbotDb::open_in_memory_for_writes().unwrap().0
    }

    #[test]
    fn activate_aave_market_on_conn_seeds_market_contract_gho_token() {
        let db = fresh_db_empty();
        let pool_ap = "0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e";
        let gho_addr = "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f";

        let market_id = {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let id = activate_aave_market_on_conn(
                &tx,
                1,
                "Aave Ethereum Market",
                pool_ap,
                gho_addr,
                Some("GHO Token"),
                Some("GHO"),
                Some(18),
                16_291_070,
            )
            .unwrap();
            tx.commit().unwrap();
            id
        };

        let conn = db.lock();
        // Market row: active + stamped.
        let (name, active, stamp): (String, bool, Option<i64>) = conn
            .query_row(
                "SELECT name, active, last_update_block FROM aave_v3_markets WHERE id = ?1",
                params![market_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(name, "Aave Ethereum Market");
        assert!(active);
        assert_eq!(stamp, Some(16_291_070));

        // POOL_ADDRESS_PROVIDER contract row.
        let n_contracts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM aave_v3_contracts \
                 WHERE market_id = ?1 AND name = 'POOL_ADDRESS_PROVIDER' AND address = ?2",
                params![market_id, pool_ap],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n_contracts, 1);

        // GHO erc20 token row with metadata.
        let (sym, dec): (String, i64) = conn
            .query_row(
                "SELECT symbol, decimals FROM erc20_tokens \
                 WHERE chain = 1 AND address = ?1",
                params![gho_addr],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(sym, "GHO");
        assert_eq!(dec, 18);

        // aave_gho_tokens row referencing the GHO erc20 token.
        let n_gho: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM aave_gho_tokens g \
                 JOIN erc20_tokens e ON e.id = g.token_id \
                 WHERE e.chain = 1 AND e.address = ?1",
                params![gho_addr],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n_gho, 1);
    }

    #[test]
    fn activate_aave_market_on_conn_reactivates_existing_market_idempotently() {
        let db = fresh_db_empty();
        let pool_ap = "0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e";
        let gho_addr = "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f";

        let first_id = {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let id = activate_aave_market_on_conn(
                &tx,
                1,
                "Aave Ethereum Market",
                pool_ap,
                gho_addr,
                Some("GHO Token"),
                Some("GHO"),
                Some(18),
                16_291_070,
            )
            .unwrap();
            tx.commit().unwrap();
            id
        };

        // Deactivate to prove re-activation flips active back to true.
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            deactivate_aave_market_on_conn(&tx, first_id).unwrap();
            tx.commit().unwrap();
        }

        // Second activation: market exists → set active=true, no dup rows.
        let second_id = {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let id = activate_aave_market_on_conn(
                &tx,
                1,
                "Aave Ethereum Market",
                pool_ap,
                gho_addr,
                Some("GHO Token"),
                Some("GHO"),
                Some(18),
                16_291_070,
            )
            .unwrap();
            tx.commit().unwrap();
            id
        };
        assert_eq!(
            first_id, second_id,
            "re-activation returns the same market_id"
        );

        let conn = db.lock();
        let active: bool = conn
            .query_row(
                "SELECT active FROM aave_v3_markets WHERE id = ?1",
                params![first_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(active, "re-activation sets active=true");

        // No duplicate contract rows.
        let n_contracts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM aave_v3_contracts \
                 WHERE market_id = ?1 AND name = 'POOL_ADDRESS_PROVIDER'",
                params![first_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n_contracts, 1);

        // No duplicate gho token rows.
        let n_gho: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM aave_gho_tokens g \
                 JOIN erc20_tokens e ON e.id = g.token_id \
                 WHERE e.chain = 1 AND e.address = ?1",
                params![gho_addr],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n_gho, 1);
    }

    #[test]
    fn deactivate_aave_market_on_conn_sets_active_false() {
        let db = fresh_db_empty();
        let pool_ap = "0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e";
        let gho_addr = "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f";

        let market_id = {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let id = activate_aave_market_on_conn(
                &tx,
                1,
                "Aave Ethereum Market",
                pool_ap,
                gho_addr,
                Some("GHO Token"),
                Some("GHO"),
                Some(18),
                16_291_070,
            )
            .unwrap();
            tx.commit().unwrap();
            id
        };

        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            deactivate_aave_market_on_conn(&tx, market_id).unwrap();
            tx.commit().unwrap();
        }

        let conn = db.lock();
        let active: bool = conn
            .query_row(
                "SELECT active FROM aave_v3_markets WHERE id = ?1",
                params![market_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!active, "deactivate sets active=false");
    }
}
