//! End-to-end §4.2 writer-parity: replay a representative Aave V3 event
//! sequence through the Rust apply fns (`get_or_create_*` + `apply_*`) into an
//! in-memory write-capable DB and assert the resulting multi-table state
//! matches the Python ORM trajectory field-for-field.
//!
//! This is the Rust-internal half of the §4.2 parity gate. The Python-written
//! Aave oracle (`cli/aave/event_handlers.py::_process_*` + the
//! `get_or_create_*` upsert helpers) is the source of truth; the per-handler
//! unit tests in `write.rs` pin each handler's field mapping vs the Python
//! ORM defaults + the create-vs-mutate trajectory, and this integration test
//! composes them into a realistic per-block backfill sequence (the "warm/bulk
//! path" of §2.1) to prove the substrate produces a coherent multi-table DB
//! state when the handlers fire in event order.
//!
//! The cross-DB byte-comparison (Rust-written rows vs a Python-written
//! `aave_writer_parity.db` fixture) is the sibling Python-side gate; this
//! Rust test pins the Rust trajectory so that gate has a stable oracle.

use alloy::primitives::U256;

use degenbot_db::DegenbotDb;

/// The canonical Aave V3 reserve-config bitmap used across the parity tests:
/// a hand-assembled bitmap exercising every decoded field (ltv=7500,
/// liquidation-threshold=8000, -bonus=10500, decimals=6, active,
/// borrowing-enabled, flash-loan, isolation-mode, borrowable-in-isolation,
/// e-mode-category=7, debt-ceiling=999).
fn known_config_bitmap() -> U256 {
    let mut b = U256::ZERO;
    b |= U256::from(7500_u64);
    b |= U256::from(8000_u64) << 16;
    b |= U256::from(10500_u64) << 32;
    b |= U256::from(6_u64) << 48;
    b |= U256::from(1_u64) << 56; // is_active
    b |= U256::from(1_u64) << 58; // borrowing_enabled
    b |= U256::from(1_u64) << 61; // borrowable_in_isolation
    b |= U256::from(1_u64) << 62; // isolation_mode
    b |= U256::from(1_u64) << 63; // flash_loan_enabled
    b |= U256::from(7_u64) << 168; // e_mode byte
    b |= U256::from(999_u64) << 212; // debt_ceiling
    b
}

/// Build a fresh in-memory write-capable DB + seed the market (id 1) + the
/// three erc20 tokens (underlying / aToken / vToken) + the asset row that
/// every Aave writer references. Returns `(db, asset_id)` — the substrate the
/// handlers operate over.
fn fresh_seeded_db() -> (DegenbotDb, i64) {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
             VALUES (1, 1, 'mainnet', 1, NULL)",
            [],
        )
        .unwrap();
        for (id, addr) in [(1_i64, "0xu1"), (2, "0xa1"), (3, "0xv1")] {
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (?1, 1, ?2)",
                rusqlite::params![id, addr],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO aave_v3_assets \
                (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
                 v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
                 borrow_index, borrow_rate) \
             VALUES (1, 1, 1, 2, 1, 3, 1, '0', '0', '1', '0')",
            [],
        )
        .unwrap();
    }
    (db, 1)
}

#[allow(clippy::too_many_lines)] // one coherent end-to-end replay scenario
#[test]
fn replay_event_sequence_produces_coherent_multitable_state() {
    let (db, asset) = fresh_seeded_db();

    // ── block N: a CollateralConfigurationChanged event arrives ────────
    //   (the Python path: RPC-fetch the bitmap → decode → upsert asset_config)
    let config_id = db
        .apply_collateral_configuration_changed(asset, known_config_bitmap())
        .unwrap();

    // ── block N: an EModeCategoryAdded event arrives ───────────────────
    let emode_cat = db
        .apply_e_mode_category_added(1, 7, 9000, 9500, 10000, Some("0xoracle"), "ETH")
        .unwrap();

    // ── block N: a user is created (e.g. their first ReserveUsedAsCollateral)
    //   + collateral enabled for the asset.
    let user = db.get_or_create_user(1, "0xuser1", 0).unwrap();
    let ucc = db
        .apply_reserve_used_as_collateral(user, asset, true)
        .unwrap();

    // ── block N+1: the user's eMode category is set → UserEModeSet
    db.apply_user_e_mode_set(user, 7).unwrap();

    // ── block N+1: a price oracle is registered → PriceOracleUpdated
    let oracle_contract = db.apply_price_oracle_updated(1, "0xpriceoracle").unwrap();

    // ── block N+1: the asset's price source is set → AssetSourceUpdated
    db.apply_asset_source_updated(asset, "0xsource").unwrap();

    // ── block N+1: the user accrues a collateral + debt position
    let collat = db.get_or_create_collateral_position(user, asset).unwrap();
    let debt = db.get_or_create_debt_position(user, asset).unwrap();

    // ── §4.2 assertions: the final DB state is coherent across all tables ──
    let conn = db.lock();

    // asset_config: the decoded bitmap fields (stable_borrowing_enabled=False —
    // NOT in the bitmap, the Python create path hard-codes it False).
    let (ltv, lt, bonus, borr, stable, flash, iso, borr_iso, dc, emode): (
        i64,
        i64,
        i64,
        bool,
        bool,
        bool,
        bool,
        bool,
        String,
        Option<i64>,
    ) = conn
        .query_row(
            "SELECT ltv, liquidation_threshold, liquidation_bonus, borrowing_enabled, \
             stable_borrowing_enabled, flash_loan_enabled, isolation_mode, \
             borrowable_in_isolation, debt_ceiling, e_mode_category_id \
             FROM aave_v3_asset_configs WHERE id = ?1",
            rusqlite::params![config_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                ))
            },
        )
        .unwrap();
    assert_eq!((ltv, lt, bonus), (7500, 8000, 10500));
    assert_eq!((borr, flash, iso, borr_iso), (true, true, true, true));
    assert!(!stable);
    assert_eq!(dc, "999");
    // note: emode on the asset_config is the bitmap-decoded e_mode_category_id
    // (Some(7)), NOT the emode_cat row created by EModeCategoryAdded (those are
    // independent — the bitmap byte + the category row are separate concepts in
    // the Python trajectory too).
    assert_eq!(emode, Some(7));

    // emode_category row: the EModeCategoryAdded fields
    let (label, eltv, elt, ebonus, ps): (String, i64, i64, i64, Option<String>) = conn
        .query_row(
            "SELECT label, ltv, liquidation_threshold, liquidation_bonus, price_source \
             FROM aave_v3_emode_categories WHERE id = ?1",
            rusqlite::params![emode_cat],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(label, "ETH");
    assert_eq!((eltv, elt, ebonus), (9000, 9500, 10000));
    assert_eq!(ps.as_deref(), Some("0xoracle"));

    // user: e_mode was updated to 7 by UserEModeSet (was 0 at create)
    let user_e_mode: i64 = conn
        .query_row(
            "SELECT e_mode FROM aave_v3_users WHERE id = ?1",
            rusqlite::params![user],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(user_e_mode, 7);

    // user_collateral_config: enabled=True (the ReserveUsedAsCollateralEnabled)
    let ucc_enabled: bool = conn
        .query_row(
            "SELECT enabled FROM aave_v3_user_collateral_configs WHERE id = ?1",
            rusqlite::params![ucc],
            |r| r.get(0),
        )
        .unwrap();
    assert!(ucc_enabled);

    // price oracle contract row
    let (cname, caddr): (String, String) = conn
        .query_row(
            "SELECT name, address FROM aave_v3_contracts WHERE id = ?1",
            rusqlite::params![oracle_contract],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(cname, "PRICE_ORACLE");
    assert_eq!(caddr, "0xpriceoracle");

    // asset price_source was set by AssetSourceUpdated
    let asset_ps: Option<String> = conn
        .query_row(
            "SELECT price_source FROM aave_v3_assets WHERE id = ?1",
            rusqlite::params![asset],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(asset_ps.as_deref(), Some("0xsource"));

    // collateral + debt positions: balance='0', last_index=NULL (the defaults)
    for (table, id) in [
        ("aave_v3_collateral_positions", collat),
        ("aave_v3_debt_positions", debt),
    ] {
        let (balance, last_index): (String, Option<String>) = conn
            .query_row(
                &format!("SELECT balance, last_index FROM {table} WHERE id = ?1"),
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(balance, "0");
        assert!(last_index.is_none());
    }

    // a reserve-collateral-DISABLE event later flips enabled to False
    drop(conn);
    db.apply_reserve_used_as_collateral(user, asset, false)
        .unwrap();
    let conn = db.lock();
    let ucc_enabled_after: bool = conn
        .query_row(
            "SELECT enabled FROM aave_v3_user_collateral_configs WHERE user_id = ?1 AND asset_id = ?2",
            rusqlite::params![user, asset],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!ucc_enabled_after);
    drop(conn);

    // an EModeAssetCategoryChanged later overrides the asset_config's
    // e_mode_category_id to a new category (unconditional older variant)
    db.apply_emode_asset_category_changed(asset, 9).unwrap();
    let conn = db.lock();
    let emode_after: Option<i64> = conn
        .query_row(
            "SELECT e_mode_category_id FROM aave_v3_asset_configs WHERE asset_id = ?1",
            rusqlite::params![asset],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(emode_after, Some(9));
}

#[test]
fn idempotent_replay_of_same_event_yields_unchanged_rows() {
    // §4.2: replaying the SAME event sequence twice (a re-org / re-indexing
    // scenario) must yield identical rows — the get-or-create + UPDATE path
    // is idempotent (no duplicate rows, no field drift).
    let (db, asset) = fresh_seeded_db();

    let bitmap = known_config_bitmap();
    let id1 = db
        .apply_collateral_configuration_changed(asset, bitmap)
        .unwrap();
    let id2 = db
        .apply_collateral_configuration_changed(asset, bitmap)
        .unwrap();
    assert_eq!(id1, id2, "replay returns the existing row, no duplicate");

    // the row count for asset_configs is exactly 1 (no duplicate insert)
    let conn = db.lock();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM aave_v3_asset_configs WHERE asset_id = ?1",
            rusqlite::params![asset],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);

    // same for the user_collateral_config enable path
    drop(conn);
    let user = db.get_or_create_user(1, "0xuser2", 0).unwrap();
    let u1 = db
        .apply_reserve_used_as_collateral(user, asset, true)
        .unwrap();
    let u2 = db
        .apply_reserve_used_as_collateral(user, asset, true)
        .unwrap();
    assert_eq!(u1, u2);
    let conn = db.lock();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM aave_v3_user_collateral_configs \
             WHERE user_id = ?1 AND asset_id = ?2",
            rusqlite::params![user, asset],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}
