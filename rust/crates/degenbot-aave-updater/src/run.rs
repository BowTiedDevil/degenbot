//! The transactional Aave V3 chunk-write apply core + its atomicity tests.
//!
//! See the crate-level docs for the §3.4 atomicity invariant this file enforces.

use alloy::primitives::U256;
use degenbot_db::DegenbotDb;
use rusqlite::Connection;

/// One pre-decoded Aave V3 event for the chunk apply loop.
///
/// Each variant carries the resolved ids/fields the matching
/// `DegenbotDb::apply_*_on_conn` fn consumes — the RPC fetch + decode + the
/// `get_or_create_user` / `get_asset_by_token_type` resolution happen in the
/// `run_aave_update` orchestrator (sibling `6SWY4R`, NOT this task), which
/// constructs this enum. This core does NO RPC, NO ABI decode, NO
/// address→id resolution.
///
/// # The event→fn dispatch map
///
/// | Variant                              | `_on_conn` fn                                    |
/// |--------------------------------------|---------------------------------------------------|
/// | [`CollateralConfigurationChanged`]  | [`apply_collateral_configuration_changed_on_conn`] |
/// | [`EModeCategoryAdded`]               | [`apply_e_mode_category_added_on_conn`]            |
/// | [`EModeAssetCategoryChanged`]        | [`apply_emode_asset_category_changed_on_conn`]     |
/// | [`AssetCollateralInEModeChanged`]    | [`apply_asset_collateral_in_emode_changed_on_conn`] |
/// | [`ReserveUsedAsCollateral`]          | [`apply_reserve_used_as_collateral_on_conn`]       |
/// | [`UserEModeSet`]                     | [`apply_user_e_mode_set_on_conn`]                  |
/// | [`PriceOracleUpdated`]               | [`apply_price_oracle_updated_on_conn`]             |
/// | [`AssetSourceUpdated`]               | [`apply_asset_source_updated_on_conn`]             |
///
/// [`apply_collateral_configuration_changed_on_conn`]: DegenbotDb::apply_collateral_configuration_changed_on_conn
/// [`apply_e_mode_category_added_on_conn`]: DegenbotDb::apply_e_mode_category_added_on_conn
/// [`apply_emode_asset_category_changed_on_conn`]: DegenbotDb::apply_emode_asset_category_changed_on_conn
/// [`apply_asset_collateral_in_emode_changed_on_conn`]: DegenbotDb::apply_asset_collateral_in_emode_changed_on_conn
/// [`apply_reserve_used_as_collateral_on_conn`]: DegenbotDb::apply_reserve_used_as_collateral_on_conn
/// [`apply_user_e_mode_set_on_conn`]: DegenbotDb::apply_user_e_mode_set_on_conn
/// [`apply_price_oracle_updated_on_conn`]: DegenbotDb::apply_price_oracle_updated_on_conn
/// [`apply_asset_source_updated_on_conn`]: DegenbotDb::apply_asset_source_updated_on_conn
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AaveChunkEvent {
    /// `CollateralConfigurationChanged(asset, config_bitmap)` — decode the
    /// bitmap + upsert the `aave_v3_asset_configs` row.
    CollateralConfigurationChanged { asset_id: i64, config_bitmap: U256 },
    /// `EModeCategoryAdded(id, label, ltv, lt, bonus, price_source)` — upsert
    /// the `aave_v3_emode_categories` row.
    EModeCategoryAdded {
        market_id: i64,
        category_id: i64,
        ltv: u64,
        liquidation_threshold: u64,
        liquidation_bonus: u64,
        /// The checksummed oracle address (`None` when the event's address is zero).
        price_source: Option<String>,
        label: String,
    },
    /// `EModeAssetCategoryChanged(asset, category)` — the older variant:
    /// unconditionally set `e_mode_category_id` (`None` when `category_id` is 0).
    EModeAssetCategoryChanged { asset_id: i64, new_category_id: i64 },
    /// `AssetCollateralInEModeChanged(asset, category, is_collateral)` — the
    /// newer Aave v3.4+ variant.
    AssetCollateralInEModeChanged {
        asset_id: i64,
        category_id: i64,
        is_collateral: bool,
    },
    /// `ReserveUsedAsCollateral{Enabled,Disabled}(user, asset)` — collapses to
    /// setting the `enabled` flag on the `aave_v3_user_collateral_configs` row.
    ReserveUsedAsCollateral {
        user_id: i64,
        asset_id: i64,
        enabled: bool,
    },
    /// `UserEModeSet(user, e_mode)` — set the user's `aave_v3_users.e_mode`.
    UserEModeSet { user_id: i64, e_mode: i64 },
    /// `PriceOracleUpdated(market, new_oracle)` — register/replace the
    /// `PRICE_ORACLE` `aave_v3_contracts` row.
    PriceOracleUpdated {
        market_id: i64,
        new_oracle_address: String,
    },
    /// `AssetSourceUpdated(asset, source)` — set the
    /// `aave_v3_assets.price_source` column.
    AssetSourceUpdated {
        asset_id: i64,
        source_address: String,
    },
}

/// Per-event-type apply counts for a chunk (mirrors `ChunkWriteReport`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AaveChunkWriteReport {
    pub collateral_configuration_changed: usize,
    pub e_mode_category_added: usize,
    pub e_mode_asset_category_changed: usize,
    pub asset_collateral_in_emode_changed: usize,
    pub reserve_used_as_collateral: usize,
    pub user_e_mode_set: usize,
    pub price_oracle_updated: usize,
    pub asset_source_updated: usize,
    /// The `chunk_end_block` stamped onto `aave_v3_markets.last_update_block`
    /// as the LAST write in the transaction. `None` if `events` was empty (no
    /// stamp written — mirrors the precedent's "no events ⇒ no stamp" guard
    /// is NOT taken here; `chunk_end_block` is always stamped when this core is
    /// invoked with a chunk range). Set on commit.
    pub stamped_block: Option<u64>,
}

/// Apply a chunk's worth of pre-decoded Aave V3 events under the caller's
/// `Transaction` (borrowed as a `&Connection`), then stamp
/// `aave_v3_markets.last_update_block = chunk_end_block` as the LAST write.
///
/// Pure, synchronous, transactional. NONE of: RPC, ABI decode, `pyo3`,
/// `database_path`, `open_for_writes`. The caller owns the `Connection` +
/// its `Transaction`'s commit/rollback — every write goes through here on the
/// single connection, + the commit is the single point of durability.
///
/// # The §3.4 atomicity invariant
///
/// All `apply_*` calls + the `last_update_block` stamp go through `_on_conn`
/// fns on this one connection. Any `?` early-return (a `UNIQUE` violation, a
/// constraint failure, ...) leaves the caller's `Transaction` uncommitted →
/// it drops → the whole chunk reverts → the stamp does NOT advance → a
/// restart re-processes the chunk clean (restart-invariant).
///
/// # Errors
///
/// Returns [`degenbot_db::DbError`] on any apply/lookup failure — the caller
/// drops the `Transaction` (rollback) on `Err`.
#[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
pub fn apply_aave_chunk_writes_on_conn(
    conn: &Connection,
    market_id: i64,
    events: &[AaveChunkEvent],
    chunk_end_block: u64,
) -> Result<AaveChunkWriteReport, degenbot_db::DbError> {
    let mut report = AaveChunkWriteReport::default();

    for event in events {
        match event {
            AaveChunkEvent::CollateralConfigurationChanged {
                asset_id,
                config_bitmap,
            } => {
                DegenbotDb::apply_collateral_configuration_changed_on_conn(
                    conn,
                    *asset_id,
                    *config_bitmap,
                )?;
                report.collateral_configuration_changed += 1;
            }
            AaveChunkEvent::EModeCategoryAdded {
                market_id: ev_market_id,
                category_id,
                ltv,
                liquidation_threshold,
                liquidation_bonus,
                price_source,
                label,
            } => {
                DegenbotDb::apply_e_mode_category_added_on_conn(
                    conn,
                    *ev_market_id,
                    *category_id,
                    *ltv,
                    *liquidation_threshold,
                    *liquidation_bonus,
                    price_source.as_deref(),
                    label,
                )?;
                report.e_mode_category_added += 1;
            }
            AaveChunkEvent::EModeAssetCategoryChanged {
                asset_id,
                new_category_id,
            } => {
                DegenbotDb::apply_emode_asset_category_changed_on_conn(
                    conn,
                    *asset_id,
                    *new_category_id,
                )?;
                report.e_mode_asset_category_changed += 1;
            }
            AaveChunkEvent::AssetCollateralInEModeChanged {
                asset_id,
                category_id,
                is_collateral,
            } => {
                DegenbotDb::apply_asset_collateral_in_emode_changed_on_conn(
                    conn,
                    *asset_id,
                    *category_id,
                    *is_collateral,
                )?;
                report.asset_collateral_in_emode_changed += 1;
            }
            AaveChunkEvent::ReserveUsedAsCollateral {
                user_id,
                asset_id,
                enabled,
            } => {
                DegenbotDb::apply_reserve_used_as_collateral_on_conn(
                    conn, *user_id, *asset_id, *enabled,
                )?;
                report.reserve_used_as_collateral += 1;
            }
            AaveChunkEvent::UserEModeSet { user_id, e_mode } => {
                DegenbotDb::apply_user_e_mode_set_on_conn(conn, *user_id, *e_mode)?;
                report.user_e_mode_set += 1;
            }
            AaveChunkEvent::PriceOracleUpdated {
                market_id: ev_market_id,
                new_oracle_address,
            } => {
                DegenbotDb::apply_price_oracle_updated_on_conn(
                    conn,
                    *ev_market_id,
                    new_oracle_address,
                )?;
                report.price_oracle_updated += 1;
            }
            AaveChunkEvent::AssetSourceUpdated {
                asset_id,
                source_address,
            } => {
                DegenbotDb::apply_asset_source_updated_on_conn(conn, *asset_id, source_address)?;
                report.asset_source_updated += 1;
            }
        }
    }

    // Stamp `last_update_block` as the LAST write (§3.4 restart-invariant:
    // on rollback the stamp does NOT advance, so a restart re-processes the
    // chunk clean).
    let chunk_end_i64 = i64::try_from(chunk_end_block).unwrap_or(i64::MAX);
    DegenbotDb::set_market_last_update_block_on_conn(conn, market_id, chunk_end_i64)?;
    report.stamped_block = Some(chunk_end_block);

    Ok(report)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::too_many_lines)]
mod tests {
    use super::*;

    /// A fresh in-memory **write-capable** DB seeded with a single market
    /// (id 1, `last_update_block = NULL`) — the FK parent every Aave row
    /// references. Mirrors `write.rs::write_db_with_market`.
    fn fresh_db() -> DegenbotDb {
        // `:memory:` DB — the `DegenbotDb` handle owns it; `db.lock()` for the
        // tx + `db.lock()` for assertions hit the same DB. Mirrors the
        // pool-updater precedent's `write_db()`.
        let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
                 VALUES (1, 1, 'mainnet', 1, NULL)",
                [],
            )
            .unwrap();
        }
        db
    }

    /// Read the market's `last_update_block` back (independent read path).
    fn market_stamp(db: &DegenbotDb) -> Option<i64> {
        let conn = db.lock();
        conn.query_row(
            "SELECT last_update_block FROM aave_v3_markets WHERE id = 1",
            [],
            |r| r.get::<_, Option<i64>>(0),
        )
        .unwrap()
    }

    // ── §3.4 atomicity: commit path writes the rows + stamps together ──────

    #[test]
    fn apply_aave_chunk_writes_on_conn_commits_events_and_stamp_together() {
        let db = fresh_db();
        // Seed an `aave_v3_assets` parent (FK target for `asset_configs`) +
        // its erc20 parents so `apply_collateral_configuration_changed` can
        // upsert the asset_config row (FK `asset_id → aave_v3_assets.id`).
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (1, 1, '0xu1')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_assets \
                    (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
                     v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
                     borrow_index, borrow_rate) \
                 VALUES (1, 1, 1, 1, 1, 1, 1, '0', '0', '1', '0')",
                [],
            )
            .unwrap();
        }

        let events = vec![AaveChunkEvent::CollateralConfigurationChanged {
            asset_id: 1,
            config_bitmap: U256::ZERO, // all-zero bitmap → all-default decode + create the asset_config row
        }];

        // ONE transaction wraps the apply + the stamp.
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 1_000).unwrap();
            assert_eq!(report.collateral_configuration_changed, 1);
            assert_eq!(report.stamped_block, Some(1_000));
            tx.commit().unwrap();
        }

        // Both the asset_config row + the stamp landed (atomicity).
        {
            let conn = db.lock();
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM aave_v3_asset_configs WHERE asset_id = 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "the apply must persist its asset_config row");
        }
        assert_eq!(
            market_stamp(&db),
            Some(1_000),
            "the stamp must land on commit"
        );
    }

    // ── §3.4 atomicity: rollback path writes NOTHING + stamp unchanged ─────

    #[test]
    fn apply_aave_chunk_writes_on_conn_rolls_back_on_injected_failure() {
        // The Aave apply fns are idempotent get-or-create (check-existing then
        // UPDATE-or-INSERT), so they never naturally hit a UNIQUE violation in
        // single-connection flow. To inject a deterministic `Err` we enable
        // SQLite FK enforcement on the test connection (SQLite defaults it
        // OFF; `open_for_writes` doesn't toggle it) + send a
        // `PriceOracleUpdated` event pointing at `market_id=9999` — the INSERT
        // path of `apply_price_oracle_updated_on_conn` violates the
        // `aave_v3_contracts.market_id → aave_v3_markets` FK + returns Err,
        // which propagates via `?` → the caller drops the tx → rollback. FK
        // enforcement is the production portable failure mode: the constraint
        // fires wherever `aave_v3_markets` is FK-owned, regardless of the
        // apply fns' idempotence.
        let db = fresh_db();
        // Seed the asset parent (so the first chunk's collateral-config apply
        // succeeds) + seed the market's stamp at 100 (the prior chunk's
        // committed stamp).
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (1, 1, '0xu1')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_assets \
                    (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
                     v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
                     borrow_index, borrow_rate) \
                 VALUES (1, 1, 1, 1, 1, 1, 1, '0', '0', '1', '0')",
                [],
            )
            .unwrap();
            conn.execute(
                "UPDATE aave_v3_markets SET last_update_block = 100 WHERE id = 1",
                [],
            )
            .unwrap();
        }

        // Build a chunk whose FIRST event succeeds + whose SECOND event fails.
        // The failure: `PriceOracleUpdated { market_id: 9999 }` — the INSERT
        // path of `apply_price_oracle_updated_on_conn` violates the
        // `aave_v3_contracts.market_id → aave_v3_markets` FK + returns Err →
        // the caller drops the tx → rollback.
        let events = vec![
            AaveChunkEvent::CollateralConfigurationChanged {
                asset_id: 1,
                config_bitmap: U256::ZERO, // succeeds: creates asset_config(1)
            },
            AaveChunkEvent::PriceOracleUpdated {
                market_id: 9999, // FAILS: FK violation on aave_v3_markets
                new_oracle_address: "0xoracle".to_string(),
            },
        ];

        let err = {
            let mut guard = db.lock();
            // FK enforcement must be set OUTSIDE a transaction (SQLite quirk).
            guard.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
            let tx = guard.transaction().unwrap();
            let result = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 200);
            assert!(result.is_err(), "the FK violation must surface as Err");
            drop(tx); // ← rollback (the `?` already returned).
            result.err()
        };
        let _ = err;

        // (a) The stamp stayed at 100 (the 200 advance rolled back).
        assert_eq!(
            market_stamp(&db),
            Some(100),
            "rolled-back chunk's stamp advance must not be durable (restart-safe)",
        );
        // (b) The asset_config row that was written before the failure did NOT
        // land (whole-chunk revert).
        {
            let conn = db.lock();
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM aave_v3_asset_configs WHERE asset_id = 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 0, "rolled-back chunk's writes must not be durable");
        }
    }

    // ── §3.4 atomicity: empty events still stamps (chunk-end semantics) ─────

    #[test]
    fn apply_aave_chunk_writes_on_conn_empty_events_stamps_block() {
        // Degenerate but correct: a chunk with no decoded events still advances
        // the cursor (the driver's "no changes this chunk" path stamps + commits
        // so the next chunk starts at working_end_block + 1). Mirrors how the
        // Python `aave_update` driver stamps `market.last_update_block` even
        // when `update_aave_market` produced no writes.
        let db = fresh_db();
        let events: Vec<AaveChunkEvent> = vec![];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 500).unwrap();
            assert_eq!(report.stamped_block, Some(500));
            tx.commit().unwrap();
        }
        assert_eq!(market_stamp(&db), Some(500));
    }

    // ── the apply fns reach the substrate (a representative dispatch) ──────

    #[test]
    fn apply_aave_chunk_writes_on_conn_dispatches_user_e_mode_set() {
        let db = fresh_db();
        // Seed a user (FK parent) at e_mode=0.
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO aave_v3_users \
                    (market_id, address, e_mode, gho_discount, stk_aave_balance, \
                     isolation_mode_collateral_asset_id, isolation_mode_debt) \
                 VALUES (1, '0xuser1', 0, 0, NULL, NULL, '0')",
                [],
            )
            .unwrap();
        }

        let events = vec![AaveChunkEvent::UserEModeSet {
            user_id: 1,
            e_mode: 2,
        }];
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let report = apply_aave_chunk_writes_on_conn(&tx, 1, &events, 1_000).unwrap();
            assert_eq!(report.user_e_mode_set, 1);
            tx.commit().unwrap();
        }

        // e_mode landed.
        let e_mode: i64 = {
            let conn = db.lock();
            conn.query_row("SELECT e_mode FROM aave_v3_users WHERE id = 1", [], |r| {
                r.get(0)
            })
            .unwrap()
        };
        assert_eq!(e_mode, 2);
        assert_eq!(market_stamp(&db), Some(1_000));
    }
}
