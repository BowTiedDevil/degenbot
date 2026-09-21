use super::*;
use crate::connection::DegenbotDb;

/// Build a fresh in-memory **write-capable** DB seeded with a single
/// market (id 1) — the FK parent every Aave row references.
fn write_db_with_market() -> DegenbotDb {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    {
        let conn = db.conn.lock();
        conn.execute(
            "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
                 VALUES (1, 1, 'mainnet', 1, NULL)",
            [],
        )
        .unwrap();
    }
    db
}

/// Seed an `aave_v3_assets` parent row (FKs to `erc20_tokens`). Returns the
/// asset row id (1).
fn seed_asset(db: &DegenbotDb) -> i64 {
    let conn = db.conn.lock();
    // three erc20 tokens: underlying / aToken / vToken (ids 1/2/3)
    for (id, addr) in [(1_i64, "0xu1"), (2, "0xa1"), (3, "0xv1")] {
        conn.execute(
            "INSERT INTO erc20_tokens (id, chain, address) VALUES (?1, 1, ?2)",
            params![id, addr],
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
    1
}

fn seed_user(db: &DegenbotDb, address: &str) -> i64 {
    let conn = db.conn.lock();
    conn.execute(
        "INSERT INTO aave_v3_users \
                (market_id, address, e_mode, gho_discount, stk_aave_balance, \
                 isolation_mode_collateral_asset_id, isolation_mode_debt) \
             VALUES (1, ?1, 0, 0, NULL, NULL, '0')",
        params![address],
    )
    .unwrap();
    conn.last_insert_rowid()
}

// ── the pure bit-decode (§4.2 parity vs the Python oracle) ────────────

#[test]
fn bit_decode_zero_bitmap_yields_zero_defaults() {
    let cfg = decode_reserve_configuration_bitmap(U256::ZERO);
    assert_eq!(cfg, ReserveConfiguration::default());
    assert!(cfg.e_mode_category_id.is_none()); // 0 → None
}

#[test]
fn bit_decode_known_bitmap_round_trips_each_field() {
    // hand-assemble a bitmap exercising every field's mask:
    // ltv=7500 (0x1d4c), lt=8000 (0x1f40), bonus=10500 (0x2904),
    // decimals=6, active(56)=1, frozen(57)=0, borrowing(58)=1,
    // stable(59)=0, reserve_factor=1000, borrow_cap=2^32-1,
    // supply_cap=12345, debt_ceiling=999, liq_proto_fee=500,
    // unbacked_mint_cap=7, e_mode_category=2 (bits 168-175),
    // flash_loan(63)=1, isolation(62)=1, borrowable_in_isolation(61)=1.
    let mut b = U256::ZERO;
    b |= U256::from(7500_u64); // ltv bits 0-15
    b |= U256::from(8000_u64) << 16; // liquidation_threshold
    b |= U256::from(10500_u64) << 32; // liquidation_bonus
    b |= U256::from(6_u64) << 48; // decimals
    b |= U256::from(1_u64) << 56; // is_active
    b |= U256::from(1_u64) << 58; // borrowing_enabled
    b |= U256::from(1000_u64) << 64; // reserve_factor
    b |= U256::from(0xFFFF_FFFF_u64) << 80; // borrow_cap
    b |= U256::from(12345_u64) << 116; // supply_cap
    b |= U256::from(500_u64) << 152; // liquidation_protocol_fee
    b |= U256::from(7_u64) << 168; // unbacked_mint_cap (also e_mode byte)
    b |= U256::from(1_u64) << 61; // borrowable_in_isolation
    b |= U256::from(1_u64) << 62; // isolation_mode
    b |= U256::from(1_u64) << 63; // flash_loan_enabled
    b |= U256::from(999_u64) << 212; // debt_ceiling

    let cfg = decode_reserve_configuration_bitmap(b);
    assert_eq!(cfg.ltv, 7500);
    assert_eq!(cfg.liquidation_threshold, 8000);
    assert_eq!(cfg.liquidation_bonus, 10500);
    assert_eq!(cfg.decimals, 6);
    assert!(cfg.is_active);
    assert!(!cfg.is_frozen);
    assert!(cfg.borrowing_enabled);
    assert!(!cfg.stable_rate_borrowing_enabled);
    assert_eq!(cfg.reserve_factor, 1000);
    assert_eq!(cfg.borrow_cap, 0xFFFF_FFFF);
    assert_eq!(cfg.supply_cap, 12345);
    assert_eq!(cfg.debt_ceiling, 999);
    assert_eq!(cfg.liquidation_protocol_fee, 500);
    // the e_mode byte is the LOW byte of bits 168-175; unbacked_mint_cap
    // (bits 168-203) low byte == 7 → e_mode_category_id = Some(7).
    assert_eq!(cfg.unbacked_mint_cap, 7);
    assert_eq!(cfg.e_mode_category_id, Some(7));
    assert!(cfg.flash_loan_enabled);
    assert!(cfg.isolation_mode);
    assert!(cfg.borrowable_in_isolation);
}

#[test]
fn bit_decode_e_mode_zero_maps_to_none() {
    let cfg = decode_reserve_configuration_bitmap(U256::from(1_u64) << 168); // unbacked byte =1, e_mode byte=1
    assert_eq!(cfg.e_mode_category_id, Some(1));
    let cfg0 = decode_reserve_configuration_bitmap(U256::ZERO);
    assert_eq!(cfg0.e_mode_category_id, None);
}

// ── the write-handle (binding #2 read-only gate holds) ──────────────────

#[test]
fn open_for_writes_is_write_capable() {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    let conn = db.conn.lock();
    // a write must SUCCEED (no query_only=on)
    conn.execute("CREATE TABLE w (a INTEGER)", []).unwrap();
    conn.execute("INSERT INTO w (a) VALUES (1)", []).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM w", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn open_read_only_still_blocks_writes() {
    // SLHSM4 binding #2 hard AC: the default read handle stays read-only.
    let (db, _state) = DegenbotDb::open_in_memory().unwrap();
    let conn = db.conn.lock();
    let r: rusqlite::Result<usize> = conn.execute("CREATE TABLE x (a INT)", []);
    assert!(r.is_err(), "read handle should block writes");
}

// ── the upsert substrate (get_or_create_*) ────────────────────────────

#[test]
fn get_or_create_e_mode_category_creates_then_returns_existing() {
    let db = write_db_with_market();
    let id1 = db.get_or_create_e_mode_category(1, 5).unwrap();
    let id2 = db.get_or_create_e_mode_category(1, 5).unwrap();
    assert_eq!(id1, id2, "second call must return the existing row");
    // a different category creates a new row
    let id3 = db.get_or_create_e_mode_category(1, 6).unwrap();
    assert_ne!(id1, id3);
    // the created row has the Python ORM defaults
    let conn = db.conn.lock();
    let (label, ltv, lt, bonus): (Option<String>, i64, i64, i64) = conn
        .query_row(
            "SELECT label, ltv, liquidation_threshold, liquidation_bonus \
                 FROM aave_v3_emode_categories WHERE id = ?1",
            params![id1],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(label.as_deref(), Some(""));
    assert_eq!((ltv, lt, bonus), (0, 0, 0));
}

#[test]
fn get_or_create_asset_config_creates_with_defaults() {
    let db = write_db_with_market();
    let asset = seed_asset(&db);
    let id = db.get_or_create_asset_config(asset).unwrap();
    let id2 = db.get_or_create_asset_config(asset).unwrap();
    assert_eq!(id, id2);
    let conn = db.conn.lock();
    let (ltv, borr, stable, flash, iso, borr_iso, dc, emode): (
        i64,
        bool,
        bool,
        bool,
        bool,
        bool,
        Option<String>,
        Option<i64>,
    ) = conn
        .query_row(
            "SELECT ltv, borrowing_enabled, stable_borrowing_enabled, flash_loan_enabled, \
                 isolation_mode, borrowable_in_isolation, debt_ceiling, e_mode_category_id \
                 FROM aave_v3_asset_configs WHERE id = ?1",
            params![id],
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
                ))
            },
        )
        .unwrap();
    assert_eq!(ltv, 0);
    assert_eq!(
        (borr, stable, flash, iso, borr_iso),
        (false, false, false, false, false)
    );
    assert!(dc.is_none());
    assert!(emode.is_none());
}

#[test]
fn get_or_create_user_collateral_config_creates_with_disabled() {
    let db = write_db_with_market();
    let asset = seed_asset(&db);
    let user = seed_user(&db, "0xuser1");
    let id = db
        .get_or_create_user_collateral_config(user, asset)
        .unwrap();
    let id2 = db
        .get_or_create_user_collateral_config(user, asset)
        .unwrap();
    assert_eq!(id, id2);
    let conn = db.conn.lock();
    let enabled: bool = conn
        .query_row(
            "SELECT enabled FROM aave_v3_user_collateral_configs WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!enabled);
}

#[test]
fn get_or_create_user_creates_with_defaults_and_gho_discount() {
    let db = write_db_with_market();
    let id = db.get_or_create_user(1, "0xuser2", 1500).unwrap();
    let id2 = db.get_or_create_user(1, "0xuser2", 9999).unwrap();
    assert_eq!(id, id2);
    let conn = db.conn.lock();
    let (e_mode, gho, stk, iso_asset, iso_debt): (i64, i64, Option<String>, Option<i64>, String) =
        conn.query_row(
            "SELECT e_mode, gho_discount, stk_aave_balance, \
                 isolation_mode_collateral_asset_id, isolation_mode_debt \
                 FROM aave_v3_users WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(e_mode, 0);
    assert_eq!(gho, 1500); // caller-supplied
    assert!(stk.is_none());
    assert!(iso_asset.is_none());
    assert_eq!(iso_debt, "0");
}

#[test]
fn get_or_create_erc20_token_creates_then_preserves_metadata() {
    let db = write_db_with_market();
    let id = db
        .get_or_create_erc20_token(1, "0xtoken", Some("Weth"), Some("WETH"), Some(18))
        .unwrap();
    // second call returns existing row (metadata NOT overwritten)
    let id2 = db
        .get_or_create_erc20_token(1, "0xtoken", None, None, None)
        .unwrap();
    assert_eq!(id, id2);
    let conn = db.conn.lock();
    let (name, symbol, decimals): (Option<String>, Option<String>, Option<i64>) = conn
        .query_row(
            "SELECT name, symbol, decimals FROM erc20_tokens WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(name.as_deref(), Some("Weth"));
    assert_eq!(symbol.as_deref(), Some("WETH"));
    assert_eq!(decimals, Some(18));
}

/// when a row exists with NULL metadata (e.g. seeded by the
/// harness `_seed_market_db`) and a later `ReserveInitialized` dispatch
/// is resolved with freshly-RPC-fetched metadata, the existing row must
/// be UPDATED in place (mirrors the Python `activate_ethereum_aave_v3`
/// pre-pass that populates the GHO token metadata via `get_or_create_erc20_token`
/// → `_fetch_erc20_token_metadata`).
///
/// This is the Rust-native equivalent: defer the writeback from activate-time
/// to `ReserveInitialized` dispatch time (Rust's drive IS its bootstrap — no
/// separate activate step).
#[test]
fn get_or_create_erc20_token_backfills_null_metadata_on_existing_row() {
    let db = write_db_with_market();
    // Seed an existing GHO-like row with NULL metadata (the harness seed
    // path — `INSERT INTO erc20_tokens (chain, address) VALUES (...)`).
    {
        let conn = db.conn.lock();
        conn.execute(
            "INSERT INTO erc20_tokens (chain, address) VALUES (1, ?1)",
            params!["0xgho"],
        )
        .unwrap();
    }
    // Resolve via `get_or_create` with freshly-fetched metadata.
    let id2 = db
        .get_or_create_erc20_token(1, "0xgho", Some("Gho Token"), Some("GHO"), Some(18))
        .unwrap();
    let conn = db.conn.lock();
    let (name, symbol, decimals): (Option<String>, Option<String>, Option<i64>) = conn
        .query_row(
            "SELECT name, symbol, decimals FROM erc20_tokens WHERE address = ?1",
            params!["0xgho"],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    // The freshly-fetched metadata MUST have been written back.
    assert_eq!(name.as_deref(), Some("Gho Token"));
    assert_eq!(symbol.as_deref(), Some("GHO"));
    assert_eq!(decimals, Some(18));
    // idempotency: succeeded without PRIMARY KEY constraint violation.
    assert!(id2 >= 1);
}

/// BOPQZ3 defensive guard: a pre-POPULATED row is NOT clobbered by a
/// later `get_or_create` call (e.g. an inner-logger retry passing None).
/// Only cells that ARE NULL get backfilled.
#[test]
fn get_or_create_erc20_token_does_not_overwrite_existing_metadata() {
    let db = write_db_with_market();
    let id = db
        .get_or_create_erc20_token(1, "0xtoken2", Some("Weth"), Some("WETH"), Some(18))
        .unwrap();
    // a later call supplying contradictory/different metadata MUST NOT
    // overwrite the existing populated cells (defensive — the BOPQZ3 fix
    // only backfills NULL cells).
    let id2 = db
        .get_or_create_erc20_token(1, "0xtoken2", Some("Other Name"), Some("OTH"), Some(6))
        .unwrap();
    assert_eq!(id, id2);
    let conn = db.conn.lock();
    let (name, symbol, decimals): (Option<String>, Option<String>, Option<i64>) = conn
        .query_row(
            "SELECT name, symbol, decimals FROM erc20_tokens WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(name.as_deref(), Some("Weth"));
    assert_eq!(symbol.as_deref(), Some("WETH"));
    assert_eq!(decimals, Some(18));
}

#[test]
fn get_or_create_collateral_and_debt_positions_create_with_zero_balance() {
    let db = write_db_with_market();
    let asset = seed_asset(&db);
    let user = seed_user(&db, "0xuser3");
    let cid = db.get_or_create_collateral_position(user, asset).unwrap();
    let did = db.get_or_create_debt_position(user, asset).unwrap();
    // collateral + debt live in DIFFERENT tables, so their rowids may
    // legitimately coincide (both tables' first row is id 1). Verify they
    // are the right rows by checking the table, not by id inequality.
    assert!(cid >= 1);
    assert!(did >= 1);
    // idempotency (same table → same row)
    assert_eq!(
        db.get_or_create_collateral_position(user, asset).unwrap(),
        cid
    );
    assert_eq!(db.get_or_create_debt_position(user, asset).unwrap(), did);
    let conn = db.conn.lock();
    let (cbalance, clast): (String, Option<String>) = conn
        .query_row(
            "SELECT balance, last_index FROM aave_v3_collateral_positions WHERE id = ?1",
            params![cid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let (dbalance, dlast): (String, Option<String>) = conn
        .query_row(
            "SELECT balance, last_index FROM aave_v3_debt_positions WHERE id = ?1",
            params![did],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(cbalance, "0");
    assert!(clast.is_none());
    assert_eq!(dbalance, "0");
    assert!(dlast.is_none());
}

// ── the per-event apply fns ──────────────────────────────────────────

#[test]
fn apply_collateral_configuration_changed_creates_then_updates() {
    let db = write_db_with_market();
    let asset = seed_asset(&db);

    // a bitmap with ltv=7500, lt=8000, bonus=10500, decimals=6, active,
    // borrowing_enabled, flash_loan, isolation, borrowable_in_isolation,
    // e_mode_category=7, debt_ceiling=999.
    let mut b = U256::ZERO;
    b |= U256::from(7500_u64);
    b |= U256::from(8000_u64) << 16;
    b |= U256::from(10500_u64) << 32;
    b |= U256::from(6_u64) << 48;
    b |= U256::from(1_u64) << 56; // active
    b |= U256::from(1_u64) << 58; // borrowing_enabled
    b |= U256::from(1_u64) << 61; // borrowable_in_isolation
    b |= U256::from(1_u64) << 62; // isolation_mode
    b |= U256::from(1_u64) << 63; // flash_loan
    b |= U256::from(7_u64) << 168; // e_mode byte (also unbacked mint cap low)
    b |= U256::from(999_u64) << 212; // debt_ceiling

    let id = db.apply_collateral_configuration_changed(asset, b).unwrap();
    // created row has the decoded fields; stable_borrowing_enabled=False
    let conn = db.conn.lock();
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
            params![id],
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
    assert_eq!(ltv, 7500);
    assert_eq!(lt, 8000);
    assert_eq!(bonus, 10500);
    assert!(borr);
    assert!(!stable); // NOT in the bitmap → left False on create
    assert!(flash);
    assert!(iso);
    assert!(borr_iso);
    assert_eq!(dc, "999");
    assert_eq!(emode, Some(7));
    drop(conn);

    // update path — a new bitmap with different ltv flips fields
    let mut b2 = U256::ZERO;
    b2 |= U256::from(8000_u64); // ltv=8000
    b2 |= U256::from(1_u64) << 57; // frozen (bit 57)
    let id2 = db
        .apply_collateral_configuration_changed(asset, b2)
        .unwrap();
    assert_eq!(id, id2, "update returns the existing row id");
    let conn = db.conn.lock();
    let (ltv, is_frozen): (i64, bool) = conn
        .query_row(
            "SELECT ltv, isolation_mode FROM aave_v3_asset_configs WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(ltv, 8000);
    // note: isolation_mode is bit 62, not 57 — bit 57 is frozen (not stored
    // on asset_config; the Python handler doesn't persist `is_frozen` to
    // asset_config). So isolation_mode is False here.
    assert!(!is_frozen);
}

#[test]
fn apply_e_mode_category_added_creates_then_updates() {
    let db = write_db_with_market();
    let id = db
        .apply_e_mode_category_added(1, 3, 9000, 9500, 10000, Some("0xoracle"), "ETH")
        .unwrap();
    let id2 = db
        .apply_e_mode_category_added(1, 3, 9100, 9600, 10100, Some("0xoracle2"), "ETH-v2")
        .unwrap();
    assert_eq!(id, id2, "update returns the existing row id");
    let conn = db.conn.lock();
    let (label, ltv, lt, bonus, ps): (String, i64, i64, i64, Option<String>) = conn
        .query_row(
            "SELECT label, ltv, liquidation_threshold, liquidation_bonus, price_source \
                 FROM aave_v3_emode_categories WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(label, "ETH-v2");
    assert_eq!((ltv, lt, bonus), (9100, 9600, 10100));
    assert_eq!(ps.as_deref(), Some("0xoracle2"));
}

#[test]
fn apply_e_mode_category_added_zero_oracle_is_none() {
    let db = write_db_with_market();
    let id = db
        .apply_e_mode_category_added(1, 1, 0, 0, 0, None, "")
        .unwrap();
    let conn = db.conn.lock();
    let ps: Option<String> = conn
        .query_row(
            "SELECT price_source FROM aave_v3_emode_categories WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(ps.is_none());
}

#[test]
fn apply_emode_asset_category_changed_unconditional_set() {
    let db = write_db_with_market();
    let asset = seed_asset(&db);

    // no existing config → create with e_mode_category_id = Some(5)
    let id = db.apply_emode_asset_category_changed(asset, 5).unwrap();
    let conn = db.conn.lock();
    let emode: Option<i64> = conn
        .query_row(
            "SELECT e_mode_category_id FROM aave_v3_asset_configs WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(emode, Some(5));
    drop(conn);

    // new_category_id=0 → clear to None (the `> 0` gate)
    db.apply_emode_asset_category_changed(asset, 0).unwrap();
    let conn = db.conn.lock();
    let emode: Option<i64> = conn
        .query_row(
            "SELECT e_mode_category_id FROM aave_v3_asset_configs WHERE asset_id = ?1",
            params![asset],
            |r| r.get(0),
        )
        .unwrap();
    assert!(emode.is_none());
    drop(conn);

    // re-set to 7
    db.apply_emode_asset_category_changed(asset, 7).unwrap();
    let conn = db.conn.lock();
    let emode: Option<i64> = conn
        .query_row(
            "SELECT e_mode_category_id FROM aave_v3_asset_configs WHERE asset_id = ?1",
            params![asset],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(emode, Some(7));
}

#[test]
fn apply_asset_collateral_in_emode_changed_gated_set() {
    let db = write_db_with_market();
    let asset = seed_asset(&db);

    // is_collateral=true, category=3 → set
    db.apply_asset_collateral_in_emode_changed(asset, 3, true)
        .unwrap();
    // is_collateral=false → leave UNCHANGED (the Python elif gap), even
    // though category=9
    db.apply_asset_collateral_in_emode_changed(asset, 9, false)
        .unwrap();
    let conn = db.conn.lock();
    let emode: Option<i64> = conn
        .query_row(
            "SELECT e_mode_category_id FROM aave_v3_asset_configs WHERE asset_id = ?1",
            params![asset],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        emode,
        Some(3),
        "is_collateral=false leaves the category unchanged"
    );
    drop(conn);

    // category_id=0 + is_collateral=true → leave unchanged (the `> 0` gate)
    db.apply_asset_collateral_in_emode_changed(asset, 0, true)
        .unwrap();
    let conn = db.conn.lock();
    let emode: Option<i64> = conn
        .query_row(
            "SELECT e_mode_category_id FROM aave_v3_asset_configs WHERE asset_id = ?1",
            params![asset],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        emode,
        Some(3),
        "category_id=0 leaves the category unchanged"
    );
    drop(conn);

    // is_collateral=true, category=4 → set
    db.apply_asset_collateral_in_emode_changed(asset, 4, true)
        .unwrap();
    let conn = db.conn.lock();
    let emode: Option<i64> = conn
        .query_row(
            "SELECT e_mode_category_id FROM aave_v3_asset_configs WHERE asset_id = ?1",
            params![asset],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(emode, Some(4));
}

#[test]
fn apply_reserve_used_as_collateral_enable_then_disable() {
    let db = write_db_with_market();
    let asset = seed_asset(&db);
    let user = seed_user(&db, "0xuserC");

    // enable → create with enabled=true
    let id = db
        .apply_reserve_used_as_collateral(user, asset, true)
        .unwrap();
    let conn = db.conn.lock();
    let enabled: bool = conn
        .query_row(
            "SELECT enabled FROM aave_v3_user_collateral_configs WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(enabled);
    drop(conn);

    // disable → update existing to false
    let id2 = db
        .apply_reserve_used_as_collateral(user, asset, false)
        .unwrap();
    assert_eq!(id, id2, "update returns the existing row id");
    let conn = db.conn.lock();
    let enabled: bool = conn
        .query_row(
            "SELECT enabled FROM aave_v3_user_collateral_configs WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!enabled);
}

#[test]
fn apply_user_e_mode_set_updates_e_mode() {
    let db = write_db_with_market();
    let user = seed_user(&db, "0xuserE");
    db.apply_user_e_mode_set(user, 3).unwrap();
    let conn = db.conn.lock();
    let e_mode: i64 = conn
        .query_row(
            "SELECT e_mode FROM aave_v3_users WHERE id = ?1",
            params![user],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(e_mode, 3);
}

#[test]
fn apply_price_oracle_updated_inserts_then_updates() {
    let db = write_db_with_market();
    let id = db.apply_price_oracle_updated(1, "0xoracle1").unwrap();
    let conn = db.conn.lock();
    let (name, addr, rev): (String, String, Option<i64>) = conn
        .query_row(
            "SELECT name, address, revision FROM aave_v3_contracts WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(name, "PRICE_ORACLE");
    assert_eq!(addr, "0xoracle1");
    assert!(rev.is_none());
    drop(conn);

    let id2 = db.apply_price_oracle_updated(1, "0xoracle2").unwrap();
    assert_eq!(id, id2);
    let conn = db.conn.lock();
    let addr: String = conn
        .query_row(
            "SELECT address FROM aave_v3_contracts WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(addr, "0xoracle2");
}

#[test]
fn apply_asset_source_updated_sets_price_source() {
    let db = write_db_with_market();
    let asset = seed_asset(&db);
    db.apply_asset_source_updated(asset, "0xsource").unwrap();
    let conn = db.conn.lock();
    let ps: Option<String> = conn
        .query_row(
            "SELECT price_source FROM aave_v3_assets WHERE id = ?1",
            params![asset],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ps.as_deref(), Some("0xsource"));
}

// ── the GHO-discount substrate fns (C3) ────────────────────────────

/// Seed a debt position with a non-zero balance + `last_index`.
fn seed_debt_position_with_balance(
    db: &DegenbotDb,
    balance: &str,
    last_index: Option<&str>,
) -> i64 {
    let user = seed_user(db, "0xdebtor");
    let asset = seed_asset(db);
    let conn = db.conn.lock();
    conn.execute(
        "INSERT INTO aave_v3_debt_positions \
                (user_id, asset_id, balance, last_index) VALUES (?1, ?2, ?3, ?4)",
        params![user, asset, balance, last_index],
    )
    .unwrap();
    conn.last_insert_rowid()
}

#[test]
fn lookup_position_balance_index_reads_debt_position() {
    let db = write_db_with_market();
    let pid = seed_debt_position_with_balance(&db, "1000", Some("123"));
    let conn = db.conn.lock();
    let (balance, last_index) =
        DegenbotDb::lookup_position_balance_index_on_conn(&conn, ScaledTokenPosition::Debt, pid)
            .unwrap();
    assert_eq!(balance, alloy::primitives::U256::from(1_000u64));
    assert_eq!(last_index, Some(alloy::primitives::U256::from(123u64)));
}

#[test]
fn lookup_position_balance_index_missing_row_returns_err() {
    let db = write_db_with_market();
    let conn = db.conn.lock();
    let err =
        DegenbotDb::lookup_position_balance_index_on_conn(&conn, ScaledTokenPosition::Debt, 9999)
            .unwrap_err();
    assert!(matches!(err, DbError::MissingRow(_)), "got {err:?}");
}

#[test]
fn delete_zero_balance_positions_clears_only_this_markets_zero_rows() {
    // The ported Python cleanup_zero_balance_positions: zero-balance
    // collateral + debt rows are deleted, scoped to the given market's
    // users; nonzero balances + other markets' rows stay.
    let db = write_db_with_market();
    seed_asset(&db);
    // A second market as the cross-market control.
    {
        let conn = db.conn.lock();
        conn.execute(
            "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
                 VALUES (2, 1, 'other', 1, NULL)",
            [],
        )
        .unwrap();
    }
    let user_m1 = seed_user(&db, "0xm1user");
    // A second market-1 user (the (user_id, asset_id) unique index allows
    // one position per user+asset).
    let user_m1b = seed_user(&db, "0xm1userb");
    let user_m2 = {
        let conn = db.conn.lock();
        conn.execute(
            "INSERT INTO aave_v3_users \
                    (market_id, address, e_mode, gho_discount, stk_aave_balance, \
                     isolation_mode_collateral_asset_id, isolation_mode_debt) \
                 VALUES (2, '0xm2user', 0, 0, NULL, NULL, '0')",
            [],
        )
        .unwrap();
        conn.last_insert_rowid()
    };
    let conn = db.conn.lock();
    let insert = |table: &str, user_id: i64, balance: &str| -> i64 {
        conn.execute(
            &format!(
                "INSERT INTO {table} (user_id, asset_id, balance, last_index) \
                     VALUES (?1, 1, ?2, NULL)"
            ),
            params![user_id, balance],
        )
        .unwrap();
        conn.last_insert_rowid()
    };
    let m1_zero_col = insert("aave_v3_collateral_positions", user_m1, "0");
    let m1_nonzero_col = insert("aave_v3_collateral_positions", user_m1b, "5000");
    let m1_zero_debt = insert("aave_v3_debt_positions", user_m1, "0");
    let m1_nonzero_debt = insert("aave_v3_debt_positions", user_m1b, "2500");
    let m2_zero_col = insert("aave_v3_collateral_positions", user_m2, "0");
    let m2_zero_debt = insert("aave_v3_debt_positions", user_m2, "0");
    drop(conn);

    let deleted = {
        let conn = db.conn.lock();
        DegenbotDb::delete_zero_balance_positions_on_conn(&conn, 1).unwrap()
    };
    assert_eq!(
        deleted, 2,
        "one zero collateral + one zero debt row for market 1"
    );
    let conn = db.conn.lock();
    let exists = |pid: i64, table: &str| -> bool {
        conn.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE id = ?1"),
            params![pid],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
            > 0
    };
    assert!(!exists(m1_zero_col, "aave_v3_collateral_positions"));
    assert!(exists(m1_nonzero_col, "aave_v3_collateral_positions"));
    assert!(!exists(m1_zero_debt, "aave_v3_debt_positions"));
    assert!(exists(m1_nonzero_debt, "aave_v3_debt_positions"));
    assert!(
        exists(m2_zero_col, "aave_v3_collateral_positions"),
        "the other market's zero rows are untouched"
    );
    assert!(
        exists(m2_zero_debt, "aave_v3_debt_positions"),
        "the other market's zero rows are untouched"
    );
}

#[test]
fn register_aave_market_creates_inactive_row_and_is_idempotent() {
    // The auto-registration substrate: a supported market not found in the
    // DB registers as an INACTIVE row with a NULL stamp (the bootstrap
    // stamp arrives with `aave activate`); a re-registration is a no-op
    // returning the same id.
    let db = write_db_with_market();
    assert!(db
        .fetch_aave_market_by_name(1, "Aave Ethereum Market")
        .unwrap()
        .is_none());

    let (id, created) = db.register_aave_market(1, "Aave Ethereum Market").unwrap();
    assert!(created, "first registration creates");

    let row = db
        .fetch_aave_market_by_name(1, "Aave Ethereum Market")
        .unwrap()
        .expect("registered row");
    assert_eq!(row.id, id);
    assert!(!row.active, "registered inactive");
    assert_eq!(row.last_update_block, None, "bare registration, no stamp");

    let (id2, created2) = db.register_aave_market(1, "Aave Ethereum Market").unwrap();
    assert_eq!(id2, id, "idempotent: same row");
    assert!(!created2, "idempotent: nothing created");

    let count: i64 = {
        let conn = db.conn.lock();
        conn.query_row(
                "SELECT COUNT(*) FROM aave_v3_markets WHERE chain_id = 1 AND name = 'Aave Ethereum Market'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(count, 1, "no duplicate rows");
}

#[test]
fn reset_debt_position_sets_balance_to_zero_and_advances_index() {
    let db = write_db_with_market();
    // balance=5000, last_index=100. New index=200 > 100 → advances.
    let pid = seed_debt_position_with_balance(&db, "5000", Some("100"));
    let conn = db.conn.lock();
    DegenbotDb::reset_debt_position_to_zero_on_conn(
        &conn,
        pid,
        alloy::primitives::U256::from(200u64),
    )
    .unwrap();
    let (balance, last_index) =
        DegenbotDb::lookup_position_balance_index_on_conn(&conn, ScaledTokenPosition::Debt, pid)
            .unwrap();
    assert_eq!(balance, alloy::primitives::U256::ZERO, "balance reset to 0");
    assert_eq!(last_index, Some(alloy::primitives::U256::from(200u64)));
}

#[test]
fn reset_debt_position_keeps_higher_index_when_new_is_lower() {
    let db = write_db_with_market();
    // balance=5000, last_index=500. New index=200 < 500 → keep 500.
    let pid = seed_debt_position_with_balance(&db, "5000", Some("500"));
    let conn = db.conn.lock();
    DegenbotDb::reset_debt_position_to_zero_on_conn(
        &conn,
        pid,
        alloy::primitives::U256::from(200u64),
    )
    .unwrap();
    let (_, last_index) =
        DegenbotDb::lookup_position_balance_index_on_conn(&conn, ScaledTokenPosition::Debt, pid)
            .unwrap();
    assert_eq!(
        last_index,
        Some(alloy::primitives::U256::from(500u64)),
        "max-with-prev keeps 500"
    );
}

#[test]
fn reset_debt_position_missing_row_returns_err() {
    let db = write_db_with_market();
    let conn = db.conn.lock();
    let err = DegenbotDb::reset_debt_position_to_zero_on_conn(
        &conn,
        9999,
        alloy::primitives::U256::from(200u64),
    )
    .unwrap_err();
    assert!(matches!(err, DbError::MissingRow(_)), "got {err:?}");
}

// ── crash #11 family: Aave V3 `_burnScaled` cap-to-scaledBalance ─────
// On-chain, `_burnScaled` clamps the burn amount to the position's
// `scaledBalance` before decrementing (the contract NEVER goes below
// zero). Rust's stored balance can drift by 1-2 wei vs the chain on
// long-running positions; a full-withdraw Burn whose `amountTotal` was
// computed against the on-chain (slightly higher) `scaledBalance` then
// produces a `balance_delta` whose magnitude exceeds the stored balance,
// and the unguarded `current + delta` would go negative and crash the
// chunk. The apply fn must mirror the contract: clamp to zero.
#[test]
fn apply_scaled_burn_clamps_to_zero_when_delta_exceeds_balance() {
    let db = write_db_with_market();
    // Stored balance = 1000; burn delta = -1500 (magnitude exceeds).
    let pid = seed_debt_position_with_balance(&db, "1000", Some("123"));
    let conn = db.conn.lock();
    DegenbotDb::apply_scaled_token_burn_on_conn(
        &conn,
        ScaledTokenPosition::Debt,
        pid,
        alloy::primitives::I256::try_from(-1_500_i64).unwrap(),
        alloy::primitives::U256::from(200u64),
    )
    .expect("clamp must not error");
    let (balance, last_index) =
        DegenbotDb::lookup_position_balance_index_on_conn(&conn, ScaledTokenPosition::Debt, pid)
            .unwrap();
    assert_eq!(
        balance,
        alloy::primitives::U256::ZERO,
        "burn > balance clamps to zero (Aave V3 `_burnScaled` cap)"
    );
    assert_eq!(
        last_index,
        Some(alloy::primitives::U256::from(200u64)),
        "last_index advances to new_index when new > prev"
    );
}

/// Partial burn: delta magnitude is LESS than the stored balance → the
/// clamp must NOT fire; the remainder is written back unchanged.
#[test]
fn apply_scaled_burn_partial_does_not_clamp() {
    let db = write_db_with_market();
    // Stored balance = 1000; burn delta = -600 (partial).
    let pid = seed_debt_position_with_balance(&db, "1000", Some("100"));
    let conn = db.conn.lock();
    DegenbotDb::apply_scaled_token_burn_on_conn(
        &conn,
        ScaledTokenPosition::Debt,
        pid,
        alloy::primitives::I256::try_from(-600_i64).unwrap(),
        alloy::primitives::U256::from(200u64),
    )
    .unwrap();
    let (balance, last_index) =
        DegenbotDb::lookup_position_balance_index_on_conn(&conn, ScaledTokenPosition::Debt, pid)
            .unwrap();
    assert_eq!(
        balance,
        alloy::primitives::U256::from(400u64),
        "partial burn leaves the remainder"
    );
    assert_eq!(
        last_index,
        Some(alloy::primitives::U256::from(200u64)),
        "last_index advances"
    );
}

/// Exact-match burn (delta magnitude == balance): the IF predicate is
/// `new_balance < 0 && delta.is_negative()`. At exactly zero, the
/// predicate is false, so we take the else branch — `0_i256` converts to
/// `U256::ZERO` cleanly. Verifies the boundary is `>= 0`, not `> 0`.
#[test]
fn apply_scaled_burn_exact_match_lands_on_zero() {
    let db = write_db_with_market();
    // Stored balance = 1000; burn delta = -1000 (exact full withdraw).
    let pid = seed_debt_position_with_balance(&db, "1000", Some("100"));
    let conn = db.conn.lock();
    DegenbotDb::apply_scaled_token_burn_on_conn(
        &conn,
        ScaledTokenPosition::Debt,
        pid,
        alloy::primitives::I256::try_from(-1_000_i64).unwrap(),
        alloy::primitives::U256::from(200u64),
    )
    .unwrap();
    let (balance, _) =
        DegenbotDb::lookup_position_balance_index_on_conn(&conn, ScaledTokenPosition::Debt, pid)
            .unwrap();
    assert_eq!(
        balance,
        alloy::primitives::U256::ZERO,
        "exact-match burn lands on zero (no clamp, no error)"
    );
}

// ── 2QGL6G: ReserveInitialized GHO-vToken-FK link (divergence #8) ────
// When the new asset's underlying IS the GHO token, the apply links the
// GHO token row to the new vToken (`aave_gho_tokens.v_token_id`), mirroring
// the Python's `_process_reserve_initialized_event` (event_handlers.py:
// 689-698). `None` for a regular reserve → no link.
#[test]
fn apply_reserve_initialized_links_gho_vtoken_fk_when_set() {
    let db = write_db_with_market();
    // Seed the underlying erc20 (id 1) + the GHO token row referencing it.
    let gho_token_row_id = {
        let conn = db.conn.lock();
        DegenbotDb::get_or_create_gho_token_on_conn(&conn, 1, "0xgho1").unwrap()
    };
    // The new asset's underlying erc20 + aToken + vToken (ids 2/3/4).
    for (id, addr) in [(2_i64, "0xund"), (3, "0xa1"), (4, "0xv1")] {
        let conn = db.conn.lock();
        conn.execute(
            "INSERT INTO erc20_tokens (id, chain, address) VALUES (?1, 1, ?2)",
            params![id, addr],
        )
        .unwrap();
    }
    let v_token_id = 4_i64;
    let conn = db.conn.lock();
    DegenbotDb::apply_reserve_initialized_on_conn(
        &conn,
        1,
        2,
        3,
        7,
        v_token_id,
        9,
        None,
        Some(gho_token_row_id),
    )
    .unwrap();
    let fk: Option<i64> = conn
        .query_row(
            "SELECT v_token_id FROM aave_gho_tokens WHERE id = ?1",
            params![gho_token_row_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        fk,
        Some(v_token_id),
        "the GHO token row's v_token_id FK should be linked to the new vToken"
    );
}

#[test]
fn apply_reserve_initialized_leaves_gho_vtoken_fk_null_for_regular_reserve() {
    let db = write_db_with_market();
    // Seed a GHO token row with v_token_id = NULL — a regular-reserve
    // ReserveInitialized must NOT touch it.
    let gho_token_row_id = {
        let conn = db.conn.lock();
        DegenbotDb::get_or_create_gho_token_on_conn(&conn, 1, "0xgho1").unwrap()
    };
    for (id, addr) in [(2_i64, "0xund"), (3, "0xa1"), (4, "0xv1")] {
        let conn = db.conn.lock();
        conn.execute(
            "INSERT INTO erc20_tokens (id, chain, address) VALUES (?1, 1, ?2)",
            params![id, addr],
        )
        .unwrap();
    }
    let conn = db.conn.lock();
    DegenbotDb::apply_reserve_initialized_on_conn(&conn, 1, 2, 3, 7, 4, 9, None, None).unwrap();
    let fk: Option<i64> = conn
        .query_row(
            "SELECT v_token_id FROM aave_gho_tokens WHERE id = ?1",
            params![gho_token_row_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        fk, None,
        "a regular-reserve event must not touch the GHO FK"
    );
}
