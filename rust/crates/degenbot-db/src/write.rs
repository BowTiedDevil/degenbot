//! Aave V3 lending-market DB writers — the per-event apply fns +
//! `get_or_create_*` upsert substrate (row N4 of the AZGJUN writer scope
//! `RQXEKH` — `port-now`).
//!
//! Port of `src/degenbot/cli/aave/event_handlers.py::_process_*` handlers +
//! `db_market.py` / `db_assets.py` / `db_users.py` / `db_positions.py`
//! `get_or_create_*` upsert helpers. The Python `aave_update` driver loop +
//! RPC event fetch stay as the shell (`stays-python`); the Rust core owns the
//! pure-typed upsert substrate: get-or-create + event-decode→row-write.
//!
//! # The write-capable connection (`open_for_writes`)
//!
//! SLHSM4 binding #2 hard-AC is "every **read** connection opened by
//! degenbot-db MUST set `query_only=on`" — [`DegenbotDb::open`] /
//! [`DegenbotDb::open_in_memory`] stay read-only. Writers use
//! [`DegenbotDb::open_for_writes`] / [`DegenbotDb::open_in_memory_for_writes`]:
//! the same `PRE_SCHEMA_PRAGMAS` + [`ensure_schema`][crate::migrate::ensure_schema]
//! sequence, but `query_only` is NEVER set — the connection is write-capable.
//! The writer methods on [`DegenbotDb`] (`get_or_create_*` / `process_*`)
//! execute `INSERT` / `UPDATE` directly on the locked connection; called on a
//! read-only handle they fail at the `SQLite` layer
//! (`attempt to write a readonly database`), surfacing [`DbError::Sqlite`].
//!
//! # The bit-decode (the pure CPU seam)
//!
//! [`decode_reserve_configuration_bitmap`] ports
//! `_decode_reserve_configuration_bitmap` (`event_handlers.py` L133–L214) verbatim:
//! the Aave V3 reserve-config `uint256` bitmap bits → a typed
//! [`ReserveConfiguration`] (ltv / liquidation-threshold / -bonus / decimals /
//! active / frozen / borrowing-enabled / stable-rate / reserve-factor /
//! borrow-cap / supply-cap / debt-ceiling / liquidation-protocol-fee /
//! unbacked-mint-cap / e-mode-category-id / flash-loan / isolation-mode /
//! borrowable-in-isolation). Pure CPU, no I/O — the §4.2 parity pin ports the
//! exact bit masks + shifts.
//!
//! # The upsert substrate (`get_or_create_*`)
//!
//! Each `get_or_create_*` mirrors the Python `session.scalar(select(...)) →
//! mutate ORM → session.add()` trajectory as a single `SELECT … WHERE …` →
//! `INSERT` (or `UPDATE` for the mutate path). The Python `Erc20TokenTable` /
//! `AaveV3Asset` / `AaveV3User` / `AaveV3EModeCategory` / `AaveV3AssetConfig` /
//! `AaveV3UserCollateralConfig` / `AaveV3CollateralPosition` /
//! `AaveV3DebtPosition` row-create defaults are reproduced verbatim (field
//! defaults match the `SQLAlchemy` model column defaults so byte-identical DB
//! state vs the Python ORM trajectory holds — §4.2 AC).
//!
//! # RPC-coupling carve-out (`stays-python` inside the upserts)
//!
//! The Python `get_or_create_erc20_token` + `get_or_create_user` fetch
//! on-chain metadata (name/symbol/decimals) / GHO discount via `raw_call`.
//! The Rust core fns take these as caller-supplied `Option` params (the
//! Python driver computes them + passes them in); no `RPC` in the core
//! (ADR-005 standalone — `degenbot-db` has no `degenbot-rpc` dependency).

use alloy::primitives::U256;
use rusqlite::{params, OptionalExtension};

use crate::connection::DegenbotDb;
use crate::error::DbError;

// ── the pure bit-decode (no I/O) ───────────────────────────────────────────

/// The decoded Aave V3 reserve-configuration bitmap. Port of the dict returned
/// by `_decode_reserve_configuration_bitmap` (`event_handlers.py` L133–L214).
/// Every field maps 1:1 to a Python dict key (`snake_case` preserved) so the
/// §4.2 parity fixture asserts field-by-field equivalence.
#[allow(clippy::struct_excessive_bools)] // mirrors the Python dict's flag set 1:1
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

// ── the upsert substrate (`get_or_create_*`) ──────────────────────────────

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
    /// (CXRGX4 — the §3.4 atomicity fix).
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
    /// [`Self::get_or_create_asset_config`] (CXRGX4 — the §3.4 atomicity
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
    /// [`Self::get_or_create_user_collateral_config`] (CXRGX4 — the §3.4
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
    /// (CXRGX4 — the §3.4 atomicity fix). See
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

    /// Get-or-create an `erc20_tokens` row by `(chain, address)`. Port of
    /// `db_assets.py::get_or_create_erc20_token` (L18–…). On create, inserts
    /// with caller-supplied metadata (`name` / `symbol` / `decimals`); the
    /// Python path RPC-fetches these via `_fetch_erc20_token_metadata` — that
    /// fetch is `stays-python` (the driver computes + passes them in). Pass
    /// `None` for each to leave the column `NULL` (matches the Python
    /// "metadata fetch returned None" trajectory).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure. `address` stored
    /// verbatim.
    pub fn get_or_create_erc20_token(
        &self,
        chain: i64,
        address: &str,
        name: Option<&str>,
        symbol: Option<&str>,
        decimals: Option<i64>,
    ) -> Result<i64, DbError> {
        let conn = self.lock();
        Self::get_or_create_erc20_token_on_conn(&conn, chain, address, name, symbol, decimals)
    }

    /// The single-transaction-bound variant of [`Self::get_or_create_erc20_token`]
    /// — accepts a borrowed [`rusqlite::Connection`] (a chunk-loop `Transaction`
    /// derefs to one) so the pool-updater chunk loop can call it on its ONE
    /// owned connection without re-locking the `Mutex` (avoids the
    /// `parking_lot` non-reentrant deadlock + retires the per-row lock cycle
    /// the `discovery::upsert_v*_pools` paths previously needed). CKXCOB 3a.
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant (CKXCOB 3a).
    pub fn get_or_create_erc20_token_on_conn(
        conn: &rusqlite::Connection,
        chain: i64,
        address: &str,
        name: Option<&str>,
        symbol: Option<&str>,
        decimals: Option<i64>,
    ) -> Result<i64, DbError> {
        if let Some(id) = existing_erc20_token(conn, chain, address)? {
            return Ok(id);
        }
        conn.execute(
            "INSERT INTO erc20_tokens (chain, address, name, symbol, decimals) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![chain, address, name, symbol, decimals],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Update an existing `erc20_tokens` row's metadata (`name` / `symbol` /
    /// `decimals`) by `(chain, address)`. QVMWQC: the construction-time write-back
    /// from `Erc20Builder.build` (a token row fetched with `NULL` metadata, then
    /// populated from RPC + committed) routes through here instead of the `SQLAlchemy`
    /// `session.commit()` dirty-tracking path.
    ///
    /// Mirrors `erc20_builder.py::build`'s write-back block: `token_from_db.decimals
    /// = decimals; token_from_db.name = name; token_from_db.symbol = symbol;
    /// session.commit()`. Each `None` field writes `NULL` (matches the ORM
    /// attribute assignment).
    ///
    /// No-op (returns `Ok(())`) when no row matches `(chain, address)` — the
    /// Python path only reaches the write-back when the row was already fetched,
    /// so a miss is a benign race (the caller's `contextlib.suppress` would have
    /// caught the prior fetch).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn update_erc20_token_metadata(
        &self,
        chain: i64,
        address: &str,
        name: Option<&str>,
        symbol: Option<&str>,
        decimals: Option<i64>,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE erc20_tokens SET name = ?3, symbol = ?4, decimals = ?5 \
             WHERE chain = ?1 AND address = ?2",
            params![chain, address, name, symbol, decimals],
        )?;
        Ok(())
    }

    /// Get-or-create an `aave_v3_collateral_positions` row by `(user_id,
    /// asset_id)`. Port of `db_positions.py::get_or_create_collateral_position`
    /// (L51–…). On create, inserts `balance='0'`, `last_index=NULL` (the
    /// Python `AaveV3CollateralPosition` defaults). Returns the row `id`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn get_or_create_collateral_position(
        &self,
        user_id: i64,
        asset_id: i64,
    ) -> Result<i64, DbError> {
        let conn = self.conn.lock();
        Self::get_or_create_collateral_position_on_conn(&conn, user_id, asset_id)
    }

    /// The single-transaction-bound variant of
    /// [`Self::get_or_create_collateral_position`] (CXRGX4 — the §3.4
    /// atomicity fix). See [`Self::get_or_create_e_mode_category_on_conn`]
    /// for the rationale.
    ///
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn get_or_create_collateral_position_on_conn(
        conn: &rusqlite::Connection,
        user_id: i64,
        asset_id: i64,
    ) -> Result<i64, DbError> {
        get_or_create_position_on_conn(conn, user_id, asset_id, "aave_v3_collateral_positions")
    }

    /// Get-or-create an `aave_v3_debt_positions` row by `(user_id, asset_id)`.
    /// Port of `db_positions.py::get_or_create_debt_position` (L73–…). On
    /// create, inserts `balance='0'`, `last_index=NULL`. Returns the row `id`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn get_or_create_debt_position(&self, user_id: i64, asset_id: i64) -> Result<i64, DbError> {
        let conn = self.conn.lock();
        Self::get_or_create_debt_position_on_conn(&conn, user_id, asset_id)
    }

    /// The single-transaction-bound variant of
    /// [`Self::get_or_create_debt_position`] (CXRGX4 — the §3.4 atomicity
    /// fix). See [`Self::get_or_create_e_mode_category_on_conn`] for the
    /// rationale.
    ///
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn get_or_create_debt_position_on_conn(
        conn: &rusqlite::Connection,
        user_id: i64,
        asset_id: i64,
    ) -> Result<i64, DbError> {
        get_or_create_position_on_conn(conn, user_id, asset_id, "aave_v3_debt_positions")
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
    /// [`Self::apply_collateral_configuration_changed`] (CXRGX4 — the §3.4
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
    /// checksummed oracle address (`None` when zero). Returns the row `id`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    #[allow(clippy::too_many_arguments)] // mirrors the Python event arg list 1:1
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
    /// [`Self::apply_e_mode_category_added`] (CXRGX4 — the §3.4 atomicity
    /// fix). See [`Self::get_or_create_e_mode_category_on_conn`] for the
    /// rationale.
    ///
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    #[allow(clippy::too_many_arguments)] // mirrors the Python event arg list 1:1
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
    /// [`Self::apply_emode_asset_category_changed`] (CXRGX4 — the §3.4
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
    /// [`Self::apply_asset_collateral_in_emode_changed`] (CXRGX4 — the §3.4
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
    /// CXRGX4 — the §3.4 atomicity fix. See
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
    /// [`Self::apply_reserve_used_as_collateral`] (CXRGX4 — the §3.4
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
    /// (CXRGX4 — the §3.4 atomicity fix). See
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
    /// [`Self::apply_price_oracle_updated`] (CXRGX4 — the §3.4 atomicity
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
    /// [`Self::apply_asset_source_updated`] (CXRGX4 — the §3.4 atomicity
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

    /// Set the `aave_v3_markets.last_update_block` stamp for `market_id`.
    /// This is the Aave-updater chunk loop's end-of-chunk stamp, the mirror of
    /// [`DegenbotDb::set_exchange_last_update_block_on_conn`] for the pool
    /// loop. CXRGX4: callable on the chunk's `Transaction` so the stamp
    /// commits atomically with the chunk's Aave writes (the §3.4 atomicity
    /// invariant's structural fix — on rollback the stamp does NOT advance,
    /// so a restart re-processes the chunk clean).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn set_market_last_update_block_on_conn(
        conn: &rusqlite::Connection,
        market_id: i64,
        block: i64,
    ) -> Result<(), DbError> {
        conn.execute(
            "UPDATE aave_v3_markets SET last_update_block = ?1 WHERE id = ?2",
            params![block, market_id],
        )?;
        Ok(())
    }

    // ── Pool-event direct writers (UR7QNL — Option A) ───────────────────
    //
    // The two Aave V3 Pool events that write DB rows directly:
    // `ReserveDataUpdated` (index/rate UPDATE) + `ReserveInitialized`
    // (asset row seed). The other Pool events (Supply/Borrow/Repay/Withdraw/
    // LiquidationCall/MintedToTreasury/DeficitCreated) do NOT write DB rows
    // directly — they're consumed by the parser (sibling OPERATIONSPARSER) to
    // GROUP ScaledToken events whose balance mutations live in SCALEAPPLY.

    /// Apply a `ReserveDataUpdated` event's decoded fields to the
    /// `aave_v3_assets` row. Port of
    /// `event_handlers.py::_process_reserve_data_update_event` (L788–L847).
    ///
    /// Updates `liquidity_rate` / `borrow_rate` (= variable borrow rate;
    /// stable borrow rate is deprecated on Aave V3, ignored as in Python) /
    /// `liquidity_index` / `borrow_index` (= variable borrow index) /
    /// `last_update_block` on the asset row keyed by `asset_id`. Unconditional
    /// UPDATE (the Python path asserts the asset exists via a `select(...)` +
    /// `assert ... is not None`; the apply fn's contract is the caller resolved
    /// the `asset_id` already — a no-op UPDATE on a missing row surfaces as
    /// [`DbError::NoRow`]).
    ///
    /// No ray-math: the indices/rates are stored raw (as the event emits them,
    /// 27-decimal ray values persisted as decimal `VARCHAR(78)` per the schema).
    /// Apply a `ReserveDataUpdated` event's decoded fields to the
    /// `aave_v3_assets` row — the `&self` wrapper.
    /// See [`Self::apply_reserve_data_updated_on_conn`] for the contract.
    ///
    /// # Errors
    ///
    /// Same error conditions as the `_on_conn` variant.
    pub fn apply_reserve_data_updated(
        &self,
        asset_id: i64,
        liquidity_rate: U256,
        variable_borrow_rate: U256,
        liquidity_index: U256,
        variable_borrow_index: U256,
        block_number: u64,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock();
        Self::apply_reserve_data_updated_on_conn(
            &conn,
            asset_id,
            liquidity_rate,
            variable_borrow_rate,
            liquidity_index,
            variable_borrow_index,
            block_number,
        )
    }

    /// The single-transaction-bound variant of
    /// [`Self::apply_reserve_data_updated`] (UR7QNL — the §3.4 atomicity fix;
    /// see [`Self::get_or_create_e_mode_category_on_conn`] for the rationale).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn apply_reserve_data_updated_on_conn(
        conn: &rusqlite::Connection,
        asset_id: i64,
        liquidity_rate: U256,
        variable_borrow_rate: U256,
        liquidity_index: U256,
        variable_borrow_index: U256,
        block_number: u64,
    ) -> Result<(), DbError> {
        let block = i64::try_from(block_number).unwrap_or(i64::MAX);
        let updated = conn.execute(
            "UPDATE aave_v3_assets SET \
                liquidity_rate = ?1, borrow_rate = ?2, \
                liquidity_index = ?3, borrow_index = ?4, \
                last_update_block = ?5 \
             WHERE id = ?6",
            params![
                liquidity_rate.to_string(),
                variable_borrow_rate.to_string(),
                liquidity_index.to_string(),
                variable_borrow_index.to_string(),
                block,
                asset_id,
            ],
        )?;
        if updated == 0 {
            return Err(DbError::MissingRow(format!(
                "aave_v3_assets id={asset_id} (ReserveDataUpdated target)"
            )));
        }
        Ok(())
    }

    /// Seed the `aave_v3_assets` row for a freshly-initialized reserve. Port
    /// of `event_handlers.py::_process_asset_initialization_event`
    /// (L549–L687). Get-or-create on `(market_id, underlying_asset_id)`:
    /// - **create** — insert `a_token_id`/`a_token_revision`/`v_token_id`/
    ///   `v_token_revision`/`price_source` + the zero defaults
    ///   (`liquidity_index='0'`/`liquidity_rate='0'`/`borrow_index='0'`/
    ///   `borrow_rate='0'`, matching the Python `AaveV3Asset` constructor's
    ///   zero defaults — `event_handlers.py:645-651`).
    /// - **existing** — `UPDATE` `a_token_id`/`a_token_revision`/`v_token_id`/
    ///   `v_token_revision`/`price_source` to the event's current values (a
    ///   reserve can be re-initialized across a Pool revision upgrade; the
    ///   Python path re-runs `get_or_create` + mutates on the existing row).
    ///
    /// The RPC-resolved revisions (`ATOKEN_REVISION()`/`DEBT_TOKEN_REVISION()`
    /// via the EIP-1967 implementation slot) + `price_source`
    /// (`getSourceOfAsset`) happen in the orchestrator (6SWY4R); the apply fn
    /// takes pre-resolved fields (mirrors CXRGX4 design decision #1 — the
    /// apply core is pure substrate, no RPC). The GHO cross-link setup if the
    /// asset IS the GHO token is RYKCC4's concern, NOT this fn's.
    ///
    /// Returns the asset row `id`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    #[allow(clippy::too_many_arguments)] // mirrors the Python event arg list 1:1
    pub fn apply_reserve_initialized(
        &self,
        market_id: i64,
        underlying_asset_id: i64,
        a_token_id: i64,
        a_token_revision: i64,
        v_token_id: i64,
        v_token_revision: i64,
        price_source: Option<&str>,
    ) -> Result<i64, DbError> {
        let conn = self.conn.lock();
        Self::apply_reserve_initialized_on_conn(
            &conn,
            market_id,
            underlying_asset_id,
            a_token_id,
            a_token_revision,
            v_token_id,
            v_token_revision,
            price_source,
        )
    }

    /// The single-transaction-bound variant of
    /// [`Self::apply_reserve_initialized`] (UR7QNL — the §3.4 atomicity fix;
    /// see [`Self::get_or_create_e_mode_category_on_conn`] for the rationale).
    ///
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    ///
    /// [`DbError::MissingRow`]: crate::error::DbError::MissingRow
    #[allow(clippy::too_many_arguments)] // mirrors the Python event arg list 1:1
    pub fn apply_reserve_initialized_on_conn(
        conn: &rusqlite::Connection,
        market_id: i64,
        underlying_asset_id: i64,
        a_token_id: i64,
        a_token_revision: i64,
        v_token_id: i64,
        v_token_revision: i64,
        price_source: Option<&str>,
    ) -> Result<i64, DbError> {
        if let Some(id) = existing_aave_v3_asset(conn, market_id, underlying_asset_id)? {
            // UPDATE the fields the Python `_process_asset_initialization_event`
            // mutates on the existing row (event_handlers.py:643-685: the
            // constructor overwrites a_token_id/a_token_revision/v_token_id/
            // v_token_revision + the RPC-resolved price_source; the index/rate
            // fields are left alone on existing rows — they're owned by
            // `apply_reserve_data_updated_on_conn`).
            conn.execute(
                "UPDATE aave_v3_assets SET \
                    a_token_id = ?1, a_token_revision = ?2, \
                    v_token_id = ?3, v_token_revision = ?4, \
                    price_source = ?5 \
                 WHERE id = ?6",
                params![
                    a_token_id,
                    a_token_revision,
                    v_token_id,
                    v_token_revision,
                    price_source,
                    id,
                ],
            )?;
            return Ok(id);
        }
        // CREATE — matches the Python `AaveV3Asset(...)` constructor's zero
        // defaults for the index/rate fields (event_handlers.py:645-651).
        conn.execute(
            "INSERT INTO aave_v3_assets \
                (market_id, underlying_asset_id, a_token_id, a_token_revision, \
                 v_token_id, v_token_revision, price_source, \
                 liquidity_index, liquidity_rate, borrow_index, borrow_rate) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '0', '0', '0', '0')",
            params![
                market_id,
                underlying_asset_id,
                a_token_id,
                a_token_revision,
                v_token_id,
                v_token_revision,
                price_source,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }
}

/// The shared `get_or_create_position` body for collateral + debt (the Python
/// `get_or_create_position[T]` is generic over both table types; the only
/// difference is the table name — both have identical `(user_id, asset_id,
/// balance, last_index)` columns). `table` is the literal `aave_v3_*` name.
fn get_or_create_position_on_conn(
    conn: &rusqlite::Connection,
    user_id: i64,
    asset_id: i64,
    table: &str,
) -> Result<i64, DbError> {
    // the existing-row lookup is parameterized by table (constant string, no
    // injection surface — both table names are compile-time literals below).
    let sql = match table {
        "aave_v3_collateral_positions" => {
            "SELECT id FROM aave_v3_collateral_positions \
             WHERE user_id = ?1 AND asset_id = ?2"
        }
        "aave_v3_debt_positions" => {
            "SELECT id FROM aave_v3_debt_positions WHERE user_id = ?1 AND asset_id = ?2"
        }
        _ => unreachable!("get_or_create_position: bad table {table:?}"),
    };
    if let Some(id) = conn
        .query_row::<i64, _, _>(sql, params![user_id, asset_id], |r| r.get(0))
        .optional()?
    {
        return Ok(id);
    }
    let insert_sql = match table {
        "aave_v3_collateral_positions" => {
            "INSERT INTO aave_v3_collateral_positions (user_id, asset_id, balance, last_index) \
             VALUES (?1, ?2, '0', NULL)"
        }
        "aave_v3_debt_positions" => {
            "INSERT INTO aave_v3_debt_positions (user_id, asset_id, balance, last_index) \
             VALUES (?1, ?2, '0', NULL)"
        }
        _ => unreachable!(),
    };
    conn.execute(insert_sql, params![user_id, asset_id])?;
    Ok(conn.last_insert_rowid())
}

// ── existing-row lookups (substrate `SELECT … WHERE …`) ────────────────────

fn existing_emode_category(
    conn: &rusqlite::Connection,
    market_id: i64,
    category_id: i64,
) -> Result<Option<i64>, DbError> {
    Ok(conn
        .query_row(
            "SELECT id FROM aave_v3_emode_categories \
             WHERE market_id = ?1 AND category_id = ?2",
            params![market_id, category_id],
            |r| r.get(0),
        )
        .optional()?)
}

fn existing_asset_config(
    conn: &rusqlite::Connection,
    asset_id: i64,
) -> Result<Option<i64>, DbError> {
    Ok(conn
        .query_row(
            "SELECT id FROM aave_v3_asset_configs WHERE asset_id = ?1",
            params![asset_id],
            |r| r.get(0),
        )
        .optional()?)
}

/// Lookup `aave_v3_assets.id` by the `(market_id, underlying_asset_id)`
/// natural key. The `ReserveInitialized` get-or-create path's existing-row
/// probe (UR7QNL).
fn existing_aave_v3_asset(
    conn: &rusqlite::Connection,
    market_id: i64,
    underlying_asset_id: i64,
) -> Result<Option<i64>, DbError> {
    Ok(conn
        .query_row(
            "SELECT id FROM aave_v3_assets \
             WHERE market_id = ?1 AND underlying_asset_id = ?2",
            params![market_id, underlying_asset_id],
            |r| r.get(0),
        )
        .optional()?)
}

fn existing_user_collateral_config(
    conn: &rusqlite::Connection,
    user_id: i64,
    asset_id: i64,
) -> Result<Option<i64>, DbError> {
    Ok(conn
        .query_row(
            "SELECT id FROM aave_v3_user_collateral_configs \
             WHERE user_id = ?1 AND asset_id = ?2",
            params![user_id, asset_id],
            |r| r.get(0),
        )
        .optional()?)
}

fn existing_user(
    conn: &rusqlite::Connection,
    market_id: i64,
    address: &str,
) -> Result<Option<i64>, DbError> {
    Ok(conn
        .query_row(
            "SELECT id FROM aave_v3_users WHERE market_id = ?1 AND address = ?2",
            params![market_id, address],
            |r| r.get(0),
        )
        .optional()?)
}

fn existing_erc20_token(
    conn: &rusqlite::Connection,
    chain: i64,
    address: &str,
) -> Result<Option<i64>, DbError> {
    Ok(conn
        .query_row(
            "SELECT id FROM erc20_tokens WHERE chain = ?1 AND address = ?2",
            params![chain, address],
            |r| r.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
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
        assert!((borr, stable, flash, iso, borr_iso) == (false, false, false, false, false));
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
        let (e_mode, gho, stk, iso_asset, iso_debt): (
            i64,
            i64,
            Option<String>,
            Option<i64>,
            String,
        ) = conn
            .query_row(
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
}
