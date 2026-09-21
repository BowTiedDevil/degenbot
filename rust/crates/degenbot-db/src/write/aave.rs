//! Aave market/asset-config/user upserts, config-event applies, and asset lookups.

use super::{params, DbError, DegenbotDb, OptionalExtension, U256};

// ── the pure bit-decode (no I/O) ───────────────────────────────────────────

/// The decoded Aave V3 reserve-configuration bitmap. Port of the dict returned
/// by `_decode_reserve_configuration_bitmap` (`event_handlers.py` L133–L214).
/// Every field maps 1:1 to a Python dict key (`snake_case` preserved) so the
/// §4.2 parity fixture asserts field-by-field equivalence.
#[expect(clippy::struct_excessive_bools)] // mirrors the Python dict's flag set 1:1
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReserveConfiguration {
    /// bits 0–15. Loan-to-value (basis points).
    pub ltv: u64,
    /// bits 16–31. Liquidation threshold (basis points).
    pub liquidation_threshold: u64,
    /// bits 32–47. Liquidation bonus (basis points).
    pub liquidation_bonus: u64,
    /// bits 48–55. Asset decimals.
    pub decimals: u64,
    /// bit 56. Reserve is active.
    pub is_active: bool,
    /// bit 57. Reserve is frozen.
    pub is_frozen: bool,
    /// bit 58. Borrowing is enabled.
    pub borrowing_enabled: bool,
    /// bit 59. Stable-rate borrowing is enabled.
    pub stable_rate_borrowing_enabled: bool,
    /// bits 64–79. Reserve factor (basis points).
    pub reserve_factor: u64,
    /// bits 80–115. Borrow cap.
    pub borrow_cap: u64,
    /// bits 116–151. Supply cap.
    pub supply_cap: u64,
    /// bits 212–251. Debt ceiling (isolation mode).
    pub debt_ceiling: u64,
    /// bits 152–167. Liquidation protocol fee (basis points).
    pub liquidation_protocol_fee: u64,
    /// bits 168–203. Unbacked mint cap.
    pub unbacked_mint_cap: u64,
    /// bits 168–175 (overlap, depends on version). E-mode category id;
    /// `None` when the decoded byte is `0` (matches the Python
    /// `e_mode_category if e_mode_category > 0 else None`).
    pub e_mode_category_id: Option<i64>,
    /// bit 63. Flash loan is enabled.
    pub flash_loan_enabled: bool,
    /// bit 62. Reserve is in isolation mode.
    pub isolation_mode: bool,
    /// bit 61. Reserve is borrowable in isolation.
    pub borrowable_in_isolation: bool,
}

/// Decode the Aave V3 reserve-configuration `uint256` bitmap into the typed
/// [`ReserveConfiguration`]. Port of `_decode_reserve_configuration_bitmap`
/// (`event_handlers.py` L133–L214) — the exact bit masks + shifts.
///
/// `config_bitmap` is the raw `uint256` returned by the Pool contract's
/// `getConfiguration(address)`; the caller (Python driver) RPC-fetches it and
/// passes it in (the RPC fetch is `stays-python`; this fn is pure CPU).
#[must_use]
pub fn decode_reserve_configuration_bitmap(config_bitmap: U256) -> ReserveConfiguration {
    // bits as documented in the Python oracle (L138–L211); the masks + shifts
    // are reproduced verbatim.
    let ltv = mask_shift(config_bitmap, 0, 0xFFFF);
    let liquidation_threshold = mask_shift(config_bitmap, 16, 0xFFFF);
    let liquidation_bonus = mask_shift(config_bitmap, 32, 0xFFFF);
    let decimals = mask_shift(config_bitmap, 48, 0xFF);
    let is_active = bit(config_bitmap, 56);
    let is_frozen = bit(config_bitmap, 57);
    let borrowing_enabled = bit(config_bitmap, 58);
    let stable_rate_borrowing_enabled = bit(config_bitmap, 59);
    let reserve_factor = mask_shift(config_bitmap, 64, 0xFFFF);
    let borrow_cap = mask_shift(config_bitmap, 80, 0xFFFF_FFFF);
    let supply_cap = mask_shift(config_bitmap, 116, 0xFFFF_FFFF);
    let debt_ceiling = mask_shift(config_bitmap, 212, 0x00FF_FFFF_FFFF);
    let liquidation_protocol_fee = mask_shift(config_bitmap, 152, 0xFFFF);
    let unbacked_mint_cap = mask_shift(config_bitmap, 168, 0xFFFF_FFFF);
    let e_mode_category = mask_shift(config_bitmap, 168, 0xFF);
    let flash_loan_enabled = bit(config_bitmap, 63);
    let isolation_mode = bit(config_bitmap, 62);
    let borrowable_in_isolation = bit(config_bitmap, 61);

    ReserveConfiguration {
        ltv,
        liquidation_threshold,
        liquidation_bonus,
        decimals,
        is_active,
        is_frozen,
        borrowing_enabled,
        stable_rate_borrowing_enabled,
        reserve_factor,
        borrow_cap,
        supply_cap,
        debt_ceiling,
        liquidation_protocol_fee,
        unbacked_mint_cap,
        e_mode_category_id: (e_mode_category > 0)
            .then_some(i64::try_from(e_mode_category).unwrap_or(i64::MAX)),
        flash_loan_enabled,
        isolation_mode,
        borrowable_in_isolation,
    }
}

/// `(config_bitmap >> shift) & mask` as a `u64` (every decode field fits in
/// `u64`; the bitmap's highest used bit is 251). The mask is applied in
/// `U256` space BEFORE the `u64` narrowing so high bits above the mask do
/// not overflow the `u64` conversion.
fn mask_shift(bitmap: U256, shift: u32, mask: u64) -> u64 {
    ((bitmap >> shift) & U256::from(mask)).to::<u64>()
}

/// `(config_bitmap >> bit) & 1 != 0` — a single boolean flag bit. The mask is
/// applied in `U256` space BEFORE the `u64` narrowing so high bits above
/// the flag do not overflow the conversion.
fn bit(bitmap: U256, b: u32) -> bool {
    ((bitmap >> b) & U256::from(1u64)).to::<u64>() != 0
}

/// The asset-row view the parser uses (aToken-vToken-Underlying triplet +
/// revisions). Returned by [`DegenbotDb::lookup_asset_by_token_address_on_conn`].
/// Owned (no lifetime) — small enough to clone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetRow {
    /// `aave_v3_assets.id`.
    pub id: i64,
    /// The aToken's revision (drives [`ScaledTokenProcessor::collateral`]).
    pub a_token_revision: u32,
    /// The vToken's revision (drives [`ScaledTokenProcessor::debt`]).
    pub v_token_revision: u32,
    /// The underlying-token address (HexBytes-free lowercase hex string).
    pub underlying_token_address: String,
    /// The aToken address (lowercase hex string).
    pub a_token_address: String,
    /// The vToken address (lowercase hex string).
    pub v_token_address: String,
}

// ── existing-row lookups (substrate `SELECT … WHERE …`) ────────────────────

fn existing_emode_category(
    conn: &rusqlite::Connection,
    market_id: i64,
    category_id: i64,
) -> Result<Option<i64>, DbError> {
    // OONKWO: prepare_cached caches the compiled statement across calls (spike-7
    // measured ~4× over query_row for this constant-SQL single-row shape). Block-
    // scoped: the `s` drops at the fn boundary, releasing back to the cache before
    // any commit. FN signature UNCHANGED.
    let mut s = conn.prepare_cached(
        "SELECT id FROM aave_v3_emode_categories \
         WHERE market_id = ?1 AND category_id = ?2",
    )?;
    Ok(s.query_row(params![market_id, category_id], |r| r.get(0))
        .optional()?)
}

fn existing_asset_config(
    conn: &rusqlite::Connection,
    asset_id: i64,
) -> Result<Option<i64>, DbError> {
    // OONKWO: prepare_cached caches the compiled statement across calls.
    let mut s = conn.prepare_cached("SELECT id FROM aave_v3_asset_configs WHERE asset_id = ?1")?;
    Ok(s.query_row(params![asset_id], |r| r.get(0)).optional()?)
}

fn existing_user_collateral_config(
    conn: &rusqlite::Connection,
    user_id: i64,
    asset_id: i64,
) -> Result<Option<i64>, DbError> {
    // OONKWO: prepare_cached caches the compiled statement across calls.
    let mut s = conn.prepare_cached(
        "SELECT id FROM aave_v3_user_collateral_configs \
         WHERE user_id = ?1 AND asset_id = ?2",
    )?;
    Ok(s.query_row(params![user_id, asset_id], |r| r.get(0))
        .optional()?)
}

fn existing_user(
    conn: &rusqlite::Connection,
    market_id: i64,
    address: &str,
) -> Result<Option<i64>, DbError> {
    // OONKWO: prepare_cached caches the compiled statement across calls.
    let mut s =
        conn.prepare_cached("SELECT id FROM aave_v3_users WHERE market_id = ?1 AND address = ?2")?;
    Ok(s.query_row(params![market_id, address], |r| r.get(0))
        .optional()?)
}
impl DegenbotDb {
    /// Get-or-create an `aave_v3_emode_categories` row by `(market_id,
    /// category_id)`. Port of `db_market.py::get_or_create_e_mode_category`
    /// (L37–L67). On create, the row is inserted with the Python ORM defaults
    /// (`label=""`, `ltv=0`, `liquidation_threshold=0`, `liquidation_bonus=0`).
    /// Returns the row `id`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure (including
    /// "attempt to write a readonly database" if called on a read-only handle).
    pub fn get_or_create_e_mode_category(
        &self,
        market_id: i64,
        category_id: i64,
    ) -> Result<i64, DbError> {
        let conn = self.conn.lock();
        Self::get_or_create_e_mode_category_on_conn(&conn, market_id, category_id)
    }

    /// The single-transaction-bound variant of
    /// [`Self::get_or_create_e_mode_category`] — accepts a borrowed
    /// [`rusqlite::Connection`] (a chunk-loop `Transaction` derefs to one) so
    /// the Aave-updater chunk loop can call it on its ONE owned connection
    /// without re-locking the `Mutex` or opening a per-call write handle
    /// (the §3.4 atomicity fix).
    ///
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn get_or_create_e_mode_category_on_conn(
        conn: &rusqlite::Connection,
        market_id: i64,
        category_id: i64,
    ) -> Result<i64, DbError> {
        if let Some(id) = existing_emode_category(conn, market_id, category_id)? {
            return Ok(id);
        }
        conn.execute(
            "INSERT INTO aave_v3_emode_categories \
                (market_id, category_id, label, ltv, liquidation_threshold, liquidation_bonus) \
             VALUES (?1, ?2, '', 0, 0, 0)",
            params![market_id, category_id],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Get-or-create an `aave_v3_asset_configs` row by `asset_id`. Port of
    /// `db_market.py::get_or_create_asset_config` (L81–L129). On create, the
    /// row is inserted with the Python ORM defaults (all zero/`false`/`None`).
    /// Returns the row `id`.
    ///
    /// `get_or_create_asset_config` is the no-arg-defaults substrate variant;
    /// the full-field apply path is
    /// [`Self::apply_collateral_configuration_changed`] (which upserts the
    /// decoded bitmap values rather than the defaults).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure (a write on a read-only
    /// handle surfaces "attempt to write a readonly database").
    pub fn get_or_create_asset_config(&self, asset_id: i64) -> Result<i64, DbError> {
        let conn = self.conn.lock();
        Self::get_or_create_asset_config_on_conn(&conn, asset_id)
    }

    /// The single-transaction-bound variant of
    /// [`Self::get_or_create_asset_config`] (the §3.4 atomicity
    /// fix). See [`Self::get_or_create_e_mode_category_on_conn`] for the
    /// rationale.
    ///
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn get_or_create_asset_config_on_conn(
        conn: &rusqlite::Connection,
        asset_id: i64,
    ) -> Result<i64, DbError> {
        if let Some(id) = existing_asset_config(conn, asset_id)? {
            return Ok(id);
        }
        conn.execute(
            "INSERT INTO aave_v3_asset_configs \
                (asset_id, ltv, liquidation_threshold, liquidation_bonus, \
                 e_mode_category_id, borrowing_enabled, stable_borrowing_enabled, \
                 flash_loan_enabled, isolation_mode, borrowable_in_isolation, debt_ceiling) \
             VALUES (?1, 0, 0, 0, NULL, 0, 0, 0, 0, 0, NULL)",
            params![asset_id],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Get-or-create an `aave_v3_user_collateral_configs` row by `(user_id,
    /// asset_id)`. Port of
    /// `db_market.py::get_or_create_user_collateral_config` (L131–L162). On
    /// create, the row is inserted with `enabled=false` (the Python default).
    /// Returns the row `id`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn get_or_create_user_collateral_config(
        &self,
        user_id: i64,
        asset_id: i64,
    ) -> Result<i64, DbError> {
        let conn = self.conn.lock();
        Self::get_or_create_user_collateral_config_on_conn(&conn, user_id, asset_id)
    }

    /// The single-transaction-bound variant of
    /// [`Self::get_or_create_user_collateral_config`] (the §3.4
    /// atomicity fix). See [`Self::get_or_create_e_mode_category_on_conn`]
    /// for the rationale.
    ///
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn get_or_create_user_collateral_config_on_conn(
        conn: &rusqlite::Connection,
        user_id: i64,
        asset_id: i64,
    ) -> Result<i64, DbError> {
        if let Some(id) = existing_user_collateral_config(conn, user_id, asset_id)? {
            return Ok(id);
        }
        conn.execute(
            "INSERT INTO aave_v3_user_collateral_configs (user_id, asset_id, enabled) \
             VALUES (?1, ?2, 0)",
            params![user_id, asset_id],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Get-or-create an `aave_v3_users` row by `(market_id, address)`. Port of
    /// `db_users.py::get_or_create_user` (L67–…). On create, the row is
    /// inserted with the Python ORM defaults (`e_mode=0`, `gho_discount=0`,
    /// `stk_aave_balance=NULL`, `isolation_mode_collateral_asset_id=NULL`,
    /// `isolation_mode_debt="0"`).
    ///
    /// `gho_discount` is caller-supplied (the Python path RPC-fetches the
    /// discount for GHO; that fetch is `stays-python` — the driver computes it
    /// and passes it here). For non-GHO markets pass `0`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure. The `address` is stored
    /// verbatim (no checksum normalization — byte-exact parity vs the Python
    /// `String(42)` trajectory regardless of stored case).
    pub fn get_or_create_user(
        &self,
        market_id: i64,
        address: &str,
        gho_discount: i64,
    ) -> Result<i64, DbError> {
        let conn = self.conn.lock();
        Self::get_or_create_user_on_conn(&conn, market_id, address, gho_discount)
    }

    /// The single-transaction-bound variant of [`Self::get_or_create_user`]
    /// (the §3.4 atomicity fix). See
    /// [`Self::get_or_create_e_mode_category_on_conn`] for the rationale.
    ///
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn get_or_create_user_on_conn(
        conn: &rusqlite::Connection,
        market_id: i64,
        address: &str,
        gho_discount: i64,
    ) -> Result<i64, DbError> {
        if let Some(id) = existing_user(conn, market_id, address)? {
            return Ok(id);
        }
        conn.execute(
            "INSERT INTO aave_v3_users \
                (market_id, address, e_mode, gho_discount, stk_aave_balance, \
                 isolation_mode_collateral_asset_id, isolation_mode_debt) \
             VALUES (?1, ?2, 0, ?3, NULL, NULL, '0')",
            params![market_id, address, gho_discount],
        )?;
        Ok(conn.last_insert_rowid())
    }

    // ── the per-event apply fns (built on the upsert substrate) ──────────

    /// Apply a `CollateralConfigurationChanged` event's decoded config
    /// bitmap to the asset's `aave_v3_asset_configs` row. Port of
    /// `event_handlers.py::_process_collateral_configuration_changed_event`
    /// (L40-L131), minus the RPC fetch of the bitmap (the Python path
    /// `raw_call`s `Pool.getConfiguration(address)`; the driver passes the
    /// fetched bitmap here — the RPC fetch is `stays-python`).
    ///
    /// Decodes the bitmap via [`decode_reserve_configuration_bitmap`] then
    /// upserts the `asset_config` row: on a new row, inserts every decoded
    /// field; on an existing row, `UPDATE`s every field (matching the Python create vs
    /// mutate-then-`session.add` trajectory — `stable_borrowing_enabled` is
    /// NOT in the bitmap, left `false` on create, untouched on update, the
    /// Python path). Returns the row `id`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure (a write on a
    /// read-only handle surfaces the readonly error).
    pub fn apply_collateral_configuration_changed(
        &self,
        asset_id: i64,
        config_bitmap: U256,
    ) -> Result<i64, DbError> {
        let conn = self.conn.lock();
        Self::apply_collateral_configuration_changed_on_conn(&conn, asset_id, config_bitmap)
    }

    /// The single-transaction-bound variant of
    /// [`Self::apply_collateral_configuration_changed`] (the §3.4
    /// atomicity fix). See [`Self::get_or_create_e_mode_category_on_conn`]
    /// for the rationale.
    ///
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn apply_collateral_configuration_changed_on_conn(
        conn: &rusqlite::Connection,
        asset_id: i64,
        config_bitmap: U256,
    ) -> Result<i64, DbError> {
        let decoded = decode_reserve_configuration_bitmap(config_bitmap);
        if let Some(id) = existing_asset_config(conn, asset_id)? {
            // UPDATE every bitmap-sourced field (matches the Python mutate path).
            conn.execute(
                "UPDATE aave_v3_asset_configs SET \
                    ltv = ?1, liquidation_threshold = ?2, liquidation_bonus = ?3, \
                    borrowing_enabled = ?4, flash_loan_enabled = ?5, \
                    borrowable_in_isolation = ?6, isolation_mode = ?7, \
                    debt_ceiling = ?8, e_mode_category_id = ?9 \
                 WHERE asset_id = ?10",
                params![
                    i64::try_from(decoded.ltv).unwrap_or(i64::MAX),
                    i64::try_from(decoded.liquidation_threshold).unwrap_or(i64::MAX),
                    i64::try_from(decoded.liquidation_bonus).unwrap_or(i64::MAX),
                    decoded.borrowing_enabled,
                    decoded.flash_loan_enabled,
                    decoded.borrowable_in_isolation,
                    decoded.isolation_mode,
                    decoded.debt_ceiling.to_string(),
                    decoded.e_mode_category_id,
                    asset_id,
                ],
            )?;
            return Ok(id);
        }
        // CREATE — matches the Python `AaveV3AssetConfig(...)` constructor:
        // every decoded field + stable_borrowing_enabled=False (NOT in the
        // bitmap; the Python create path hard-codes it False).
        conn.execute(
            "INSERT INTO aave_v3_asset_configs \
                (asset_id, ltv, liquidation_threshold, liquidation_bonus, \
                 e_mode_category_id, borrowing_enabled, stable_borrowing_enabled, \
                 flash_loan_enabled, isolation_mode, borrowable_in_isolation, debt_ceiling) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9, ?10)",
            params![
                asset_id,
                i64::try_from(decoded.ltv).unwrap_or(i64::MAX),
                i64::try_from(decoded.liquidation_threshold).unwrap_or(i64::MAX),
                i64::try_from(decoded.liquidation_bonus).unwrap_or(i64::MAX),
                decoded.e_mode_category_id,
                decoded.borrowing_enabled,
                decoded.flash_loan_enabled,
                decoded.isolation_mode,
                decoded.borrowable_in_isolation,
                decoded.debt_ceiling.to_string(),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Apply an `EModeCategoryAdded` event's decoded fields to the
    /// `aave_v3_emode_categories` row. Port of
    /// `event_handlers.py::_process_e_mode_category_added_event` (L216-L277).
    ///
    /// On create, inserts `(label, ltv, liquidation_threshold, liquidation_bonus,
    /// price_source)`; on existing, `UPDATE`s the same five fields (matching
    /// the Python create-vs-mutate trajectory). `price_source` is the
    /// checksummed oracle address — the canonical caller
    /// ([`degenbot_aave::config_dispatch::dispatch_e_mode_category_added`])
    /// always passes `Some(checksum(&oracle))`, so a zero-oracle event
    /// produces the literal `"0x0000000000000000000000000000000000000000"`
    /// string (matching Python's `get_checksum_address(Address::ZERO)`
    /// gold-parity reference behavior). Returns the row `id`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    #[expect(clippy::too_many_arguments)] // mirrors the Python event arg list 1:1
    pub fn apply_e_mode_category_added(
        &self,
        market_id: i64,
        category_id: i64,
        ltv: u64,
        liquidation_threshold: u64,
        liquidation_bonus: u64,
        price_source: Option<&str>,
        label: &str,
    ) -> Result<i64, DbError> {
        let conn = self.conn.lock();
        Self::apply_e_mode_category_added_on_conn(
            &conn,
            market_id,
            category_id,
            ltv,
            liquidation_threshold,
            liquidation_bonus,
            price_source,
            label,
        )
    }

    /// The single-transaction-bound variant of
    /// [`Self::apply_e_mode_category_added`] (the §3.4 atomicity
    /// fix). See [`Self::get_or_create_e_mode_category_on_conn`] for the
    /// rationale.
    ///
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    #[expect(clippy::too_many_arguments)] // mirrors the Python event arg list 1:1
    pub fn apply_e_mode_category_added_on_conn(
        conn: &rusqlite::Connection,
        market_id: i64,
        category_id: i64,
        ltv: u64,
        liquidation_threshold: u64,
        liquidation_bonus: u64,
        price_source: Option<&str>,
        label: &str,
    ) -> Result<i64, DbError> {
        if let Some(id) = existing_emode_category(conn, market_id, category_id)? {
            conn.execute(
                "UPDATE aave_v3_emode_categories SET \
                    label = ?1, ltv = ?2, liquidation_threshold = ?3, \
                    liquidation_bonus = ?4, price_source = ?5 \
                 WHERE id = ?6",
                params![
                    label,
                    i64::try_from(ltv).unwrap_or(i64::MAX),
                    i64::try_from(liquidation_threshold).unwrap_or(i64::MAX),
                    i64::try_from(liquidation_bonus).unwrap_or(i64::MAX),
                    price_source,
                    id,
                ],
            )?;
            return Ok(id);
        }
        conn.execute(
            "INSERT INTO aave_v3_emode_categories \
                (market_id, category_id, label, ltv, liquidation_threshold, liquidation_bonus, price_source) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                market_id,
                category_id,
                label,
                i64::try_from(ltv).unwrap_or(i64::MAX),
                i64::try_from(liquidation_threshold).unwrap_or(i64::MAX),
                i64::try_from(liquidation_bonus).unwrap_or(i64::MAX),
                price_source,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Apply an `EModeAssetCategoryChanged` event's category assignment to
    /// the asset's `aave_v3_asset_configs` row's `e_mode_category_id`.
    /// Port of `_process_emode_asset_category_changed_event` (L279-L347) —
    /// the older Aave variant. Unconditionally sets `e_mode_category_id` to
    /// the new category (`None` when the category id is `0`). Returns the row
    /// `id`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn apply_emode_asset_category_changed(
        &self,
        asset_id: i64,
        new_category_id: i64,
    ) -> Result<i64, DbError> {
        let conn = self.conn.lock();
        Self::apply_emode_asset_category_changed_on_conn(&conn, asset_id, new_category_id)
    }

    /// The single-transaction-bound variant of
    /// [`Self::apply_emode_asset_category_changed`] (the §3.4
    /// atomicity fix). See [`Self::get_or_create_e_mode_category_on_conn`]
    /// for the rationale.
    ///
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn apply_emode_asset_category_changed_on_conn(
        conn: &rusqlite::Connection,
        asset_id: i64,
        new_category_id: i64,
    ) -> Result<i64, DbError> {
        let new_value = (new_category_id > 0).then_some(new_category_id);
        Self::set_asset_emode_category_on_conn(conn, asset_id, new_value)
    }

    /// Apply an `AssetCollateralInEModeChanged` event's category assignment
    /// to the asset's `aave_v3_asset_configs` row's `e_mode_category_id`.
    /// Port of `_process_asset_collateral_in_emode_changed_event`
    /// (L349-L420) — the newer Aave v3.4+ variant. Sets `e_mode_category_id`
    /// to the category ONLY when `is_collateral && category_id > 0`; when
    /// `is_collateral` is false the row is LEFT UNCHANGED (the Python `elif`
    /// branch does nothing on removal). Returns the row `id`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn apply_asset_collateral_in_emode_changed(
        &self,
        asset_id: i64,
        category_id: i64,
        is_collateral: bool,
    ) -> Result<i64, DbError> {
        let conn = self.conn.lock();
        Self::apply_asset_collateral_in_emode_changed_on_conn(
            &conn,
            asset_id,
            category_id,
            is_collateral,
        )
    }

    /// The single-transaction-bound variant of
    /// [`Self::apply_asset_collateral_in_emode_changed`] (the §3.4
    /// atomicity fix). See [`Self::get_or_create_e_mode_category_on_conn`]
    /// for the rationale.
    ///
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn apply_asset_collateral_in_emode_changed_on_conn(
        conn: &rusqlite::Connection,
        asset_id: i64,
        category_id: i64,
        is_collateral: bool,
    ) -> Result<i64, DbError> {
        if is_collateral && category_id > 0 {
            Self::set_asset_emode_category_on_conn(conn, asset_id, Some(category_id))
        } else {
            // the Python elif branch: removal (is_collateral=false) or
            // category_id=0 leaves the row's e_mode_category_id unchanged.
            // If there's no existing row, create one with the None category
            // (matching the Python create-time `is_collateral && category_id
            // > 0` gate evaluating to None here).
            if let Some(id) = existing_asset_config(conn, asset_id)? {
                Ok(id)
            } else {
                Self::set_asset_emode_category_on_conn(conn, asset_id, None)
            }
        }
    }

    /// The shared set-e_mode_category_id seam for the two emode variants.
    /// Get-or-create the `asset_config` row, setting `e_mode_category_id` to
    /// `new_value` (`None` clears). On a new row, the row uses the all-default
    /// substrate columns + `new_value`. On an existing row, `UPDATE` the
    /// single column.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    /// The single-transaction-bound variant (the only form now exercised — the
    /// `&self` apply wrappers delegate here via their `_on_conn` siblings).
    /// the §3.4 atomicity fix. See
    /// [`Self::get_or_create_e_mode_category_on_conn`] for the rationale.
    ///
    /// # Errors
    ///
    /// Same error conditions as the apply wrappers.
    fn set_asset_emode_category_on_conn(
        conn: &rusqlite::Connection,
        asset_id: i64,
        new_value: Option<i64>,
    ) -> Result<i64, DbError> {
        if let Some(id) = existing_asset_config(conn, asset_id)? {
            conn.execute(
                "UPDATE aave_v3_asset_configs SET e_mode_category_id = ?1 \
                 WHERE asset_id = ?2",
                params![new_value, asset_id],
            )?;
            return Ok(id);
        }
        conn.execute(
            "INSERT INTO aave_v3_asset_configs \
                (asset_id, ltv, liquidation_threshold, liquidation_bonus, \
                 e_mode_category_id, borrowing_enabled, stable_borrowing_enabled, \
                 flash_loan_enabled, isolation_mode, borrowable_in_isolation, debt_ceiling) \
             VALUES (?1, 0, 0, 0, ?2, 0, 0, 0, 0, 0, NULL)",
            params![asset_id, new_value],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Apply a `ReserveUsedAsCollateralEnabled`/`Disabled` event to the
    /// `aave_v3_user_collateral_configs` row's `enabled` flag. Ports
    /// `_process_reserve_used_as_collateral_enabled_event` (L422-L483) AND
    /// `_process_reserve_used_as_collateral_disabled_event` (L485-L547):
    /// both collapse to setting `enabled` (`true` for enabled, `false` for
    /// disabled). On create, inserts with the given `enabled` value; on
    /// existing, `UPDATE`s the flag. Returns the row `id`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn apply_reserve_used_as_collateral(
        &self,
        user_id: i64,
        asset_id: i64,
        enabled: bool,
    ) -> Result<i64, DbError> {
        let conn = self.conn.lock();
        Self::apply_reserve_used_as_collateral_on_conn(&conn, user_id, asset_id, enabled)
    }

    /// The single-transaction-bound variant of
    /// [`Self::apply_reserve_used_as_collateral`] (the §3.4
    /// atomicity fix). See [`Self::get_or_create_e_mode_category_on_conn`]
    /// for the rationale.
    ///
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn apply_reserve_used_as_collateral_on_conn(
        conn: &rusqlite::Connection,
        user_id: i64,
        asset_id: i64,
        enabled: bool,
    ) -> Result<i64, DbError> {
        if let Some(id) = existing_user_collateral_config(conn, user_id, asset_id)? {
            conn.execute(
                "UPDATE aave_v3_user_collateral_configs SET enabled = ?1 \
                 WHERE user_id = ?2 AND asset_id = ?3",
                params![enabled, user_id, asset_id],
            )?;
            return Ok(id);
        }
        conn.execute(
            "INSERT INTO aave_v3_user_collateral_configs (user_id, asset_id, enabled) \
             VALUES (?1, ?2, ?3)",
            params![user_id, asset_id, enabled],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Apply a `UserEModeSet` event to the user's `aave_v3_users.e_mode`
    /// column. Port of `_process_user_e_mode_set_event` (L688-L715). The user
    /// row must already exist (the Python path `get_or_create_user`s first;
    /// the driver passes the existing `user_id` here). Returns the row `id`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn apply_user_e_mode_set(&self, user_id: i64, e_mode: i64) -> Result<i64, DbError> {
        let conn = self.conn.lock();
        Self::apply_user_e_mode_set_on_conn(&conn, user_id, e_mode)
    }

    /// The single-transaction-bound variant of [`Self::apply_user_e_mode_set`]
    /// (the §3.4 atomicity fix). See
    /// [`Self::get_or_create_e_mode_category_on_conn`] for the rationale.
    ///
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn apply_user_e_mode_set_on_conn(
        conn: &rusqlite::Connection,
        user_id: i64,
        e_mode: i64,
    ) -> Result<i64, DbError> {
        conn.execute(
            "UPDATE aave_v3_users SET e_mode = ?1 WHERE id = ?2",
            params![e_mode, user_id],
        )?;
        Ok(user_id)
    }

    /// Apply a `PriceOracleUpdated` event: register the new `PRICE_ORACLE`
    /// `aave_v3_contracts` row for `market_id`. Port of
    /// `_process_price_oracle_updated_event` (L1080-L1126). The Python path
    /// asserts no existing `PRICE_ORACLE` row (a fresh insert); this Rust fn
    /// upserts — if a `PRICE_ORACLE` row exists it `UPDATE`s the address,
    /// else it inserts (defensive vs the Python `assert existing_oracle is
    /// None`). Returns the row `id`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn apply_price_oracle_updated(
        &self,
        market_id: i64,
        new_oracle_address: &str,
    ) -> Result<i64, DbError> {
        let conn = self.conn.lock();
        Self::apply_price_oracle_updated_on_conn(&conn, market_id, new_oracle_address)
    }

    /// The single-transaction-bound variant of
    /// [`Self::apply_price_oracle_updated`] (the §3.4 atomicity
    /// fix). See [`Self::get_or_create_e_mode_category_on_conn`] for the
    /// rationale.
    ///
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn apply_price_oracle_updated_on_conn(
        conn: &rusqlite::Connection,
        market_id: i64,
        new_oracle_address: &str,
    ) -> Result<i64, DbError> {
        if let Some(id) = conn
            .query_row::<i64, _, _>(
                "SELECT id FROM aave_v3_contracts \
                 WHERE market_id = ?1 AND name = 'PRICE_ORACLE'",
                params![market_id],
                |r| r.get(0),
            )
            .optional()?
        {
            conn.execute(
                "UPDATE aave_v3_contracts SET address = ?1 WHERE id = ?2",
                params![new_oracle_address, id],
            )?;
            return Ok(id);
        }
        conn.execute(
            "INSERT INTO aave_v3_contracts (market_id, name, address, revision) \
             VALUES (?1, 'PRICE_ORACLE', ?2, NULL)",
            params![market_id, new_oracle_address],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Apply an `AssetSourceUpdated` event: set the asset's
    /// `aave_v3_assets.price_source` column. Port of
    /// `_process_asset_source_updated_event` (L1128-L1166). The `asset_id`
    /// must already exist (the Python `assert asset is not None`). Returns the
    /// row `id`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn apply_asset_source_updated(
        &self,
        asset_id: i64,
        source_address: &str,
    ) -> Result<i64, DbError> {
        let conn = self.conn.lock();
        Self::apply_asset_source_updated_on_conn(&conn, asset_id, source_address)
    }

    /// The single-transaction-bound variant of
    /// [`Self::apply_asset_source_updated`] (the §3.4 atomicity
    /// fix). See [`Self::get_or_create_e_mode_category_on_conn`] for the
    /// rationale.
    ///
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn apply_asset_source_updated_on_conn(
        conn: &rusqlite::Connection,
        asset_id: i64,
        source_address: &str,
    ) -> Result<i64, DbError> {
        conn.execute(
            "UPDATE aave_v3_assets SET price_source = ?1 WHERE id = ?2",
            params![source_address, asset_id],
        )?;
        Ok(asset_id)
    }

    /// Resolve the asset-row id for an emitter token address (the
    /// `_get_asset_by_token_type` port). Mirrors the Python `_get_token_type` +
    /// `_get_asset_by_token` JOIN — the parser classifies each decoded
    /// `Mint/Burn/BalanceTransfer` event by the contract that emitted it: aToken →
    /// [`ScaledTokenEventType`][crate::operations::ScaledTokenEventType] collateral, vToken → debt.
    /// The `token_type` discriminates which column to JOIN against
    /// (`a_token_id` / `v_token_id`); returns `None` if the address isn't a
    /// known market asset (the caller decides — for plain ERC20 `Transfer`
    /// events this is the GHO-discount-token branch).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn lookup_asset_id_by_token_address_on_conn(
        conn: &rusqlite::Connection,
        market_id: i64,
        token_address: &str,
        token_type: &str,
    ) -> Result<Option<i64>, DbError> {
        // The two-column JOIN: aave_v3_assets.{a_token_id,v_token_id} JOIN
        // erc20_tokens.address. The `token_type` selects the column.
        let column = match token_type {
            "a_token" => "a_token_id",
            "v_token" => "v_token_id",
            _ => {
                return Err(DbError::Decode(format!(
                    "unexpected TokenType '{token_type}' (expected 'a_token' or 'v_token')"
                )));
            }
        };
        let sql = format!(
            "SELECT a.id FROM aave_v3_assets a
             JOIN erc20_tokens t ON t.id = a.{column}
             WHERE a.market_id = ?1 AND t.address = ?2"
        );
        Ok(conn
            .query_row(&sql, rusqlite::params![market_id, token_address], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?)
    }

    /// Resolve the emitter token's full row (id + the partner token addresses
    /// and revisions). The parser uses the partner addresses to validate the
    /// contract-emitter match the Python does in each `_create_*_operation`
    /// (e.g. `ev.event["address"] != expected_a_token`).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn lookup_asset_by_token_address_on_conn(
        conn: &rusqlite::Connection,
        market_id: i64,
        token_address: &str,
        token_type: &str,
    ) -> Result<Option<AssetRow>, DbError> {
        let column = match token_type {
            "a_token" => "a_token_id",
            "v_token" => "v_token_id",
            _ => {
                return Err(DbError::Decode(format!(
                    "unexpected TokenType '{token_type}' (expected 'a_token' or 'v_token')"
                )));
            }
        };
        let sql = format!(
            "SELECT
                a.id,
                a.a_token_revision,
                a.v_token_revision,
                t_underlying.address AS underlying,
                t_a.address AS a_token,
                t_v.address AS v_token
             FROM aave_v3_assets a
             JOIN erc20_tokens t_underlying ON t_underlying.id = a.underlying_asset_id
             JOIN erc20_tokens t_a ON t_a.id = a.a_token_id
             JOIN erc20_tokens t_v ON t_v.id = a.v_token_id
             WHERE a.market_id = ?1 AND a.{column} IN (
                 SELECT id FROM erc20_tokens WHERE address = ?2
             )"
        );
        // NB: rusqlite's prepared-statement approach — the column is formatted
        // into the SQL string (it's an internal constant, not user input) and
        // the bind params cover only the WHERE-clause values.
        let mut stmt = conn.prepare(&sql)?;
        let row = stmt
            .query_row(rusqlite::params![market_id, token_address], |r| {
                Ok(AssetRow {
                    id: r.get(0)?,
                    a_token_revision: r
                        .get::<_, Option<i64>>(1)?
                        .map_or(0, |v| u32::try_from(v).unwrap_or(0)),
                    v_token_revision: r
                        .get::<_, Option<i64>>(2)?
                        .map_or(0, |v| u32::try_from(v).unwrap_or(0)),
                    underlying_token_address: r.get::<_, String>(3)?,
                    a_token_address: r.get::<_, String>(4)?,
                    v_token_address: r.get::<_, String>(5)?,
                })
            })
            .optional()?;
        Ok(row)
    }

    /// Resolve the asset row by the underlying-token address (mirror of the
    /// Python `_get_a_token_for_asset` + `_get_v_token_for_asset` JOINs — both
    /// Python helpers run the same SELECT against `aave_v3_assets JOIN
    /// erc20_tokens ON underlying_asset_id` and project only the aToken or
    /// vToken address. This Rust substrate returns the full `AssetRow` so the
    /// caller can read `.a_token_address` or `.v_token_address` as needed).
    ///
    /// Used by `_create_liquidation_operation` (passing the `LiquidationCall`'s
    /// `collateralAsset` topic index 1 as `underlying_address` for the aToken
    /// sibling lookup, and `debtAsset` topic index 2 for the vToken sibling).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn lookup_asset_by_underlying_address_on_conn(
        conn: &rusqlite::Connection,
        market_id: i64,
        underlying_address: &str,
    ) -> Result<Option<AssetRow>, DbError> {
        let mut stmt = conn.prepare(
            "SELECT
                a.id,
                a.a_token_revision,
                a.v_token_revision,
                t_underlying.address AS underlying,
                t_a.address AS a_token,
                t_v.address AS v_token
             FROM aave_v3_assets a
             JOIN erc20_tokens t_underlying ON t_underlying.id = a.underlying_asset_id
             JOIN erc20_tokens t_a ON t_a.id = a.a_token_id
             JOIN erc20_tokens t_v ON t_v.id = a.v_token_id
             WHERE a.market_id = ?1 AND t_underlying.address = ?2",
        )?;
        let row = stmt
            .query_row(rusqlite::params![market_id, underlying_address], |r| {
                Ok(AssetRow {
                    id: r.get(0)?,
                    a_token_revision: r
                        .get::<_, Option<i64>>(1)?
                        .map_or(0, |v| u32::try_from(v).unwrap_or(0)),
                    v_token_revision: r
                        .get::<_, Option<i64>>(2)?
                        .map_or(0, |v| u32::try_from(v).unwrap_or(0)),
                    underlying_token_address: r.get::<_, String>(3)?,
                    a_token_address: r.get::<_, String>(4)?,
                    v_token_address: r.get::<_, String>(5)?,
                })
            })
            .optional()?;
        stmt.finalize()?;
        Ok(row)
    }

    /// Register a supported Aave V3 market that is not found in the DB as an
    /// INACTIVE, BARE row (`active = 0`, `last_update_block = NULL`). The
    /// auto-registration seam the console's write arms run (fresh `database
    /// reset` + the `pool update` / `aave update` arms): the operator then
    /// flips it active with `aave activate`, which COMPLETES the row (the
    /// contract/GHO substrate + the bootstrap stamp) in
    /// `degenbot_aave::activate_aave_market_on_conn`.
    ///
    /// Returns `(market_id, created)` — `created` is `false` when the row
    /// pre-existed (idempotent no-op).
    ///
    /// # Errors
    ///
    /// [`DbError::Sqlite`] on a query failure.
    pub fn register_aave_market(
        &self,
        chain_id: i64,
        market_name: &str,
    ) -> Result<(i64, bool), DbError> {
        let conn = self.lock();
        Self::register_aave_market_on_conn(&conn, chain_id, market_name)
    }

    /// The `&Connection`-bound variant of [`Self::register_aave_market`].
    #[expect(clippy::missing_errors_doc)]
    pub fn register_aave_market_on_conn(
        conn: &rusqlite::Connection,
        chain_id: i64,
        market_name: &str,
    ) -> Result<(i64, bool), DbError> {
        use rusqlite::OptionalExtension;
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM aave_v3_markets WHERE chain_id = ?1 AND name = ?2",
                rusqlite::params![chain_id, market_name],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Ok((id, false));
        }
        conn.execute(
            "INSERT INTO aave_v3_markets (chain_id, name, active, last_update_block) \
             VALUES (?1, ?2, 0, NULL)",
            rusqlite::params![chain_id, market_name],
        )?;
        Ok((conn.last_insert_rowid(), true))
    }
}
