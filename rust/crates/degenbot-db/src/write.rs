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
        gho_link_token_id: Option<i64>,
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
            gho_link_token_id,
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
        gho_link_token_id: Option<i64>,
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
        let asset_id = conn.last_insert_rowid();
        // 2QGL6G / divergence #8: mirror the Python's GHO-vToken-FK link
        // (event_handlers.py:689-698) — when the new asset's underlying IS
        // the GHO token, set `aave_gho_tokens.v_token_id` to the new vToken's
        // erc20 id. The FK is the precondition for the ULDUAC emitter guard
        // (resolved via `gho_asset.v_token_address`). `None` for a regular
        // reserve (no link).
        if let Some(gho_id) = gho_link_token_id {
            conn.execute(
                "UPDATE aave_gho_tokens SET v_token_id = ?1 WHERE id = ?2",
                params![v_token_id, gho_id],
            )?;
        }
        Ok(asset_id)
    }

    // ── 6SWY4R-2b: the 6 missing-variant config-event apply fns ──────────

    /// Apply an `Upgraded` event: set the aToken or vToken revision on the
    /// `aave_v3_assets` row + conditionally fire the GHO-discount-deprecation
    /// side effect. Port of `_process_scaled_token_upgrade_event`
    /// (event_handlers.py:848-940).
    ///
    /// When `is_a_token` is `true`, updates `a_token_revision`; otherwise
    /// `v_token_revision`. When `deprecated_gho_token_id` is `Some(id)`, also
    /// clears `aave_gho_tokens.v_gho_discount_token`/
    /// `v_gho_discount_rate_strategy` + bulk-resets all users' `gho_discount`
    /// to 0 in the asset's market (the protocol deprecated the discount).
    ///
    /// # §4.2 parity
    ///
    /// The Python's `_process_scaled_token_upgrade_event` updates the
    /// attribute on the `SQLAlchemy` record + (on deprecation) sets
    /// `gho_asset.v_gho_discount_token = None`,
    /// `v_gho_discount_rate_strategy = None`, + iterates every
    /// `AaveV3User` in the market with `gho_discount != 0` → `0`. The bulk
    /// `UPDATE ... WHERE market_id = ? AND gho_discount != 0` is the SQL
    /// equivalent (the Python's loop bodies a per-row
    /// `session.commit()`-tracked update).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure, or
    /// [`DbError::MissingRow`] if the `aave_v3_assets` row + the `aave_gho_tokens`
    /// row (on deprecation) don't match the given ids.
    #[allow(clippy::missing_errors_doc)]
    pub fn apply_upgraded_on_conn(
        conn: &rusqlite::Connection,
        asset_id: i64,
        is_a_token: bool,
        new_revision: i64,
        deprecated_gho_token_id: Option<i64>,
    ) -> Result<(), DbError> {
        let column = if is_a_token {
            "a_token_revision"
        } else {
            "v_token_revision"
        };
        let sql = format!("UPDATE aave_v3_assets SET {column} = ?1 WHERE id = ?2");
        let updated = conn.execute(&sql, params![new_revision, asset_id])?;
        if updated == 0 {
            return Err(DbError::MissingRow(format!(
                "aave_v3_assets id={asset_id} (Upgraded apply target)"
            )));
        }
        if let Some(gho_id) = deprecated_gho_token_id {
            // Clear the GHO discount config.
            let cleared = conn.execute(
                "UPDATE aave_gho_tokens \
                 SET v_gho_discount_token = NULL, v_gho_discount_rate_strategy = NULL \
                 WHERE id = ?1",
                params![gho_id],
            )?;
            if cleared == 0 {
                return Err(DbError::MissingRow(format!(
                    "aave_gho_tokens id={gho_id} (Upgraded GHO-deprecation clear target)"
                )));
            }
            // Bulk-reset all non-zero GHO discounts on the asset's market.
            let _bulk = conn.execute(
                "UPDATE aave_v3_users SET gho_discount = 0 \
                 WHERE market_id = (SELECT market_id FROM aave_v3_assets WHERE id = ?1) \
                 AND gho_discount != 0",
                params![asset_id],
            )?;
        }
        Ok(())
    }

    /// Apply a `PoolUpdated`/`PoolConfiguratorUpdated` event: set the
    /// `aave_v3_contracts` row's `revision` (looked up by `market_id` +
    /// `contract_name`). Port of `_update_contract_revision`
    /// (event_handlers.py:944-974).
    ///
    /// # §4.2 parity
    ///
    /// The Python updates ONLY `revision` — NOT `address` (the proxy address is
    /// stable; the `new_address` is used only for the `*_REVISION()` RPC call).
    /// The apply mirrors this exactly (no `address` param). The Python
    /// `assert contract is not None`; this returns `MissingRow` if no row
    /// matches (a non-fatal `Err` the transaction rolls back).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure, or
    /// [`DbError::MissingRow`] if no `aave_v3_contracts` row matches
    /// `(market_id, contract_name)`.
    #[allow(clippy::missing_errors_doc)]
    pub fn apply_contract_revision_updated_on_conn(
        conn: &rusqlite::Connection,
        market_id: i64,
        contract_name: &str,
        new_revision: i64,
    ) -> Result<(), DbError> {
        let updated = conn.execute(
            "UPDATE aave_v3_contracts SET revision = ?1 \
             WHERE market_id = ?2 AND name = ?3",
            params![new_revision, market_id, contract_name],
        )?;
        if updated == 0 {
            return Err(DbError::MissingRow(format!(
                "aave_v3_contracts (market_id={market_id}, name='{contract_name}') \
                 (ContractRevisionUpdated apply target)"
            )));
        }
        Ok(())
    }

    /// Apply a `PoolDataProviderUpdated(old, new)` event: INSERT the
    /// `POOL_DATA_PROVIDER` contract row when `old_address` is `None` (the event's
    /// `old` is zero), else UPDATE the existing row's `address` (looked up by
    /// the old address). Port of `_process_pool_data_provider_updated_event`
    /// (event_handlers.py:1017-1046). Pure substrate — no RPC.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure, or
    /// [`DbError::MissingRow`] if the UPDATE path finds no row matching
    /// `old_address` (the Python `assert pool_data_provider is not None`).
    #[allow(clippy::missing_errors_doc)]
    pub fn apply_pool_data_provider_updated_on_conn(
        conn: &rusqlite::Connection,
        market_id: i64,
        old_address: Option<&str>,
        new_address: &str,
    ) -> Result<(), DbError> {
        match old_address {
            None => {
                // The INSERT path (old == ZERO_ADDRESS).
                conn.execute(
                    "INSERT INTO aave_v3_contracts (market_id, name, address, revision) \
                     VALUES (?1, 'POOL_DATA_PROVIDER', ?2, NULL)",
                    params![market_id, new_address],
                )?;
            }
            Some(old) => {
                // The UPDATE-by-old-address path.
                let updated = conn.execute(
                    "UPDATE aave_v3_contracts SET address = ?1 WHERE address = ?2",
                    params![new_address, old],
                )?;
                if updated == 0 {
                    return Err(DbError::MissingRow(format!(
                        "aave_v3_contracts address='{old}' \
                         (PoolDataProviderUpdated UPDATE path target)"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Apply an `AddressSet`/`ProxyCreated` event: INSERT a contract row.
    /// Port of `_process_address_set_event` (event_handlers.py:1048-1078) +
    /// `_process_proxy_creation_event` (event_handlers.py:977-1008). For
    /// `AddressSet`, `revision` is `None`; for `ProxyCreated`, `revision` is
    /// the RPC-fetched `POOL_REVISION()`/`CONFIGURATOR_REVISION()`. A plain
    /// INSERT — the Python's `session.add(AaveV3Contract(...))` /
    /// `market.contracts.append(...)` (no upsert; a duplicate would hit the
    /// UNIQUE constraint + rollback the transaction).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure (incl. a UNIQUE
    /// constraint violation).
    #[allow(clippy::missing_errors_doc)]
    pub fn apply_contract_inserted_on_conn(
        conn: &rusqlite::Connection,
        market_id: i64,
        name: &str,
        address: &str,
        revision: Option<i64>,
    ) -> Result<(), DbError> {
        conn.execute(
            "INSERT INTO aave_v3_contracts (market_id, name, address, revision) \
             VALUES (?1, ?2, ?3, ?4)",
            params![market_id, name, address, revision],
        )?;
        Ok(())
    }

    // ── ScaledToken position-state mutations (5Z3QQ2 — SCALEAPPLY) ────────
    //
    // The aToken/vToken `Mint`/`Burn`/`BalanceTransfer` events drive the
    // `balance` + `last_index` columns on `aave_v3_collateral_positions` /
    // `aave_v3_debt_positions`. These apply fns are the pure substrate: they
    // take a PRE-COMPUTED signed `balance_delta` (the revision-aware
    // `process_mint_event`/`process_burn_event` math in
    // `degenbot-aave-updater::processors` computed it from the event's
    // `value`/`balance_increase`/`index`/`scaled_amount` + the token
    // revision's rounding strategy) + the new index, then mutate the row.
    //
    // The `last_index` reconciliation mirrors the Python
    // `token_processor._process_scaled_token_operation` L91/L109/L131/L148:
    // `position.last_index = max(position.last_index or 0, new_index)` — only
    // advance last_index when the event's index is strictly greater (so
    // out-of-log-order events don't clobber a later event's index).

    /// Apply a `ScaledToken` `Mint` event's pre-computed `balance_delta` +
    /// `new_index` to a collateral or debt position. Port of
    /// `token_processor._process_scaled_token_operation`'s
    /// `CollateralMintEvent`/`DebtMintEvent` arm (the `position.balance +=
    /// mint_result.balance_delta` + the `last_index` reconciliation).
    ///
    /// `balance_delta` is signed: positive for a true mint (deposit/borrow),
    /// negative for the interest-exceeds-withdrawal edge case (the Python
    /// `process_mint_event` returns a negative delta when `balance_increase >
    /// value`, treating the Mint as an effective burn). The apply fn is the
    /// same shape as [`Self::apply_scaled_token_burn_on_conn`] — the only
    /// difference is the caller's semantic (mint vs burn); the math (read
    /// balance, add delta, write back, conditionally advance `last_index`) is
    /// identical.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure, or
    /// [`DbError::Decode`] if the persisted `balance`/`last_index` string is
    /// malformed, or [`DbError::MissingRow`] if no position matches
    /// `position_id`.
    pub fn apply_scaled_token_mint_on_conn(
        conn: &rusqlite::Connection,
        position: ScaledTokenPosition,
        position_id: i64,
        balance_delta: alloy::primitives::I256,
        new_index: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        apply_scaled_token_balance_delta_on_conn(
            conn,
            position,
            position_id,
            balance_delta,
            new_index,
        )
    }

    /// Apply a `ScaledToken` `Burn` event's pre-computed `balance_delta` +
    /// `new_index` to a collateral or debt position. Port of
    /// `token_processor._process_scaled_token_operation`'s
    /// `CollateralBurnEvent`/`DebtBurnEvent` arm. `balance_delta` is negative
    /// (a burn decrements the scaled balance); the apply math is shared with
    /// [`Self::apply_scaled_token_mint_on_conn`] (both are signed deltas).
    ///
    /// # Errors
    ///
    /// Same as [`Self::apply_scaled_token_mint_on_conn`].
    pub fn apply_scaled_token_burn_on_conn(
        conn: &rusqlite::Connection,
        position: ScaledTokenPosition,
        position_id: i64,
        balance_delta: alloy::primitives::I256,
        new_index: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        apply_scaled_token_balance_delta_on_conn(
            conn,
            position,
            position_id,
            balance_delta,
            new_index,
        )
    }

    /// Apply a `ScaledToken` `BalanceTransfer` event: debit `from_position_id`'s
    /// balance by `scaled_amount`, credit `to_position_id` by the same, +
    /// advance both positions' `last_index` to `transfer_index` (the transfer
    /// carries its own index; both positions reconcile to it). Port of
    /// `transfers._process_collateral_transfer` (the sender's `last_index =
    /// max(prev, transfer_index)` + the recipient's `last_index =
    /// transfer_index` unconditional set).
    ///
    /// `BalanceTransfer` is aToken-only (collateral positions); there's no
    /// vToken `BalanceTransfer` in Aave V3 (debt transfers are ERC20 Transfer +
    /// paired Burn, not `BalanceTransfer`). So both positions are collateral.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure (either position's
    /// SELECT/UPDATE), or [`DbError::MissingRow`] if either position id has no
    /// row, or [`DbError::Decode`] on a malformed persisted balance string.
    pub fn apply_scaled_token_transfer_on_conn(
        conn: &rusqlite::Connection,
        from_position_id: i64,
        to_position_id: i64,
        scaled_amount: alloy::primitives::U256,
        transfer_index: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        // Debit the sender (negative delta). On existing positions, the
        // Python mutates `sender_position.balance -= scaled_amount` then
        // `sender_position.last_index = max(prev, transfer_index)`.
        apply_scaled_token_balance_delta_on_conn(
            conn,
            ScaledTokenPosition::Collateral,
            from_position_id,
            -alloy::primitives::I256::try_from(scaled_amount)
                .unwrap_or(alloy::primitives::I256::MIN),
            transfer_index,
        )?;
        // Credit the recipient (positive delta). The Python sets
        // `recipient_position.last_index = transfer_index` UNCONDITIONALLY
        // (not max-with-prev) — the recipient's index becomes the transfer's
        // index. Reuse the shared helper which does max-with-prev; the
        // difference is benign because a fresh recipient position's
        // `last_index` is NULL (treated as 0), so max(0, transfer_index) =
        // transfer_index matches the unconditional set. For an existing
        // recipient (a re-transfer into a non-empty position), max-with-prev
        // is SAFER (avoids clobbering a higher index from a prior transfer);
        // the Python's unconditional set is a latent bug only visible if a
        // prior transfer into the same recipient had a higher index, which
        // can't happen within a single chunk's monotonic block order.
        apply_scaled_token_balance_delta_on_conn(
            conn,
            ScaledTokenPosition::Collateral,
            to_position_id,
            alloy::primitives::I256::try_from(scaled_amount)
                .unwrap_or(alloy::primitives::I256::MAX),
            transfer_index,
        )?;
        Ok(())
    }

    // ── `&self` wrappers (2QPBUJ path: thin lock-and-delegate) ─────────────

    /// `&self` wrapper for [`Self::apply_scaled_token_mint_on_conn`].
    ///
    /// # Errors
    ///
    /// Same as the `_on_conn` variant.
    pub fn apply_scaled_token_mint(
        &self,
        position: ScaledTokenPosition,
        position_id: i64,
        balance_delta: alloy::primitives::I256,
        new_index: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock();
        Self::apply_scaled_token_mint_on_conn(
            &conn,
            position,
            position_id,
            balance_delta,
            new_index,
        )
    }

    /// `&self` wrapper for [`Self::apply_scaled_token_burn_on_conn`].
    ///
    /// # Errors
    ///
    /// Same as the `_on_conn` variant.
    pub fn apply_scaled_token_burn(
        &self,
        position: ScaledTokenPosition,
        position_id: i64,
        balance_delta: alloy::primitives::I256,
        new_index: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock();
        Self::apply_scaled_token_burn_on_conn(
            &conn,
            position,
            position_id,
            balance_delta,
            new_index,
        )
    }

    /// `&self` wrapper for [`Self::apply_scaled_token_transfer_on_conn`].
    ///
    /// # Errors
    ///
    /// Same as the `_on_conn` variant.
    pub fn apply_scaled_token_transfer(
        &self,
        from_position_id: i64,
        to_position_id: i64,
        scaled_amount: alloy::primitives::U256,
        transfer_index: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock();
        Self::apply_scaled_token_transfer_on_conn(
            &conn,
            from_position_id,
            to_position_id,
            scaled_amount,
            transfer_index,
        )
    }

    // ── RYKCC4: GHO / stkAAVE / Rewards apply fns ──────────────────────────

    /// Apply a GHO `DiscountPercentUpdated` event: set the user's
    /// `gho_discount` column (an `INTEGER` percentage; the Python path stores
    /// the raw uint256 but the Aave protocol caps discount at 100% so `i64` is
    /// the correct Rust type). Port of
    /// `event_handlers._process_discount_percent_updated_event` (L1167).
    ///
    /// `user_id` is pre-resolved by the orchestrator/parser (the Python calls
    /// `get_or_create_user` upstream; the apply fn takes the resolved id — the
    /// CXRGX4 precedent).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure, or
    /// [`DbError::MissingRow`] if no user matches `user_id`.
    pub fn apply_gho_discount_percent_updated_on_conn(
        conn: &rusqlite::Connection,
        user_id: i64,
        new_discount_percent: i64,
    ) -> Result<(), DbError> {
        let updated = conn.execute(
            "UPDATE aave_v3_users SET gho_discount = ?1 WHERE id = ?2",
            params![new_discount_percent, user_id],
        )?;
        if updated == 0 {
            return Err(DbError::MissingRow(format!(
                "aave_v3_users id={user_id} (DiscountPercentUpdated apply target)"
            )));
        }
        Ok(())
    }

    /// `&self` wrapper for [`Self::apply_gho_discount_percent_updated_on_conn`].
    ///
    /// # Errors
    ///
    /// Same as the `_on_conn` variant.
    pub fn apply_gho_discount_percent_updated(
        &self,
        user_id: i64,
        new_discount_percent: i64,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock();
        Self::apply_gho_discount_percent_updated_on_conn(&conn, user_id, new_discount_percent)
    }

    /// Apply a GHO `DiscountRateStrategyUpdated` event: set the GHO token's
    /// `v_gho_discount_rate_strategy` column (a checksummed address or
    /// `NULL`). Port of
    /// `event_handlers._process_discount_rate_strategy_updated_event` (L752).
    ///
    /// `gho_token_id` is the `aave_gho_tokens.id` (NOT the erc20 token id —
    /// the GHO token row is chain-unique, identified by its primary key).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure, or
    /// [`DbError::MissingRow`] if no `aave_gho_tokens` row matches
    /// `gho_token_id`.
    pub fn apply_gho_discount_rate_strategy_updated_on_conn(
        conn: &rusqlite::Connection,
        gho_token_id: i64,
        new_strategy: Option<&str>,
    ) -> Result<(), DbError> {
        let updated = conn.execute(
            "UPDATE aave_gho_tokens SET v_gho_discount_rate_strategy = ?1 WHERE id = ?2",
            params![new_strategy, gho_token_id],
        )?;
        if updated == 0 {
            return Err(DbError::MissingRow(format!(
                "aave_gho_tokens id={gho_token_id} (DiscountRateStrategyUpdated apply target)"
            )));
        }
        Ok(())
    }

    /// `&self` wrapper for [`Self::apply_gho_discount_rate_strategy_updated_on_conn`].
    ///
    /// # Errors
    ///
    /// Same as the `_on_conn` variant.
    pub fn apply_gho_discount_rate_strategy_updated(
        &self,
        gho_token_id: i64,
        new_strategy: Option<&str>,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock();
        Self::apply_gho_discount_rate_strategy_updated_on_conn(&conn, gho_token_id, new_strategy)
    }

    /// Apply a GHO `DiscountTokenUpdated` event: set the GHO token's
    /// `v_gho_discount_token` column (a checksummed address or `NULL`). Port
    /// of `event_handlers._process_discount_token_updated_event` (L717).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure, or
    /// [`DbError::MissingRow`] if no `aave_gho_tokens` row matches
    /// `gho_token_id`.
    pub fn apply_gho_discount_token_updated_on_conn(
        conn: &rusqlite::Connection,
        gho_token_id: i64,
        new_discount_token: Option<&str>,
    ) -> Result<(), DbError> {
        let updated = conn.execute(
            "UPDATE aave_gho_tokens SET v_gho_discount_token = ?1 WHERE id = ?2",
            params![new_discount_token, gho_token_id],
        )?;
        if updated == 0 {
            return Err(DbError::MissingRow(format!(
                "aave_gho_tokens id={gho_token_id} (DiscountTokenUpdated apply target)"
            )));
        }
        Ok(())
    }

    /// `&self` wrapper for [`Self::apply_gho_discount_token_updated_on_conn`].
    ///
    /// # Errors
    ///
    /// Same as the `_on_conn` variant.
    pub fn apply_gho_discount_token_updated(
        &self,
        gho_token_id: i64,
        new_discount_token: Option<&str>,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock();
        Self::apply_gho_discount_token_updated_on_conn(&conn, gho_token_id, new_discount_token)
    }

    /// Apply a stkAAVE `Staked` event: increment the user's `stk_aave_balance`
    /// by `amount`. Port of `stkaave.process_stk_aave_transfer_event` (the
    /// `to_user` path; the Python `Transfer(from=0, to=X)` arm).
    ///
    /// The stored `stk_aave_balance` is a decimal `VARCHAR(78)` (matches the
    /// Python `Mapped[int | None]` which `SQLAlchemy` stores as a string under
    /// `SQLite`). The apply reads the current value (`NULL` is treated as 0 —
    /// matches the Python `get_or_init_stk_aave_balance` which would RPC-fetch
    /// when `None`; the orchestrator is responsible for ensuring the balance
    /// is non-NULL before this call, but the apply fn is defensive + treats
    /// `NULL` as 0).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure, or
    /// [`DbError::MissingRow`] if no user matches `user_id`, or
    /// [`DbError::Decode`] if the persisted `stk_aave_balance` string is
    /// malformed.
    pub fn apply_stk_aave_staked_on_conn(
        conn: &rusqlite::Connection,
        user_id: i64,
        amount: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        let current: Option<Option<String>> = conn
            .query_row(
                "SELECT stk_aave_balance FROM aave_v3_users WHERE id = ?1",
                [user_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?;
        let Some(current_str) = current else {
            return Err(DbError::MissingRow(format!(
                "aave_v3_users id={user_id} (StkAaveStaked apply target)"
            )));
        };
        let current_balance = current_str
            .as_deref()
            .map(parse_decimal_u256)
            .transpose()?
            .unwrap_or(alloy::primitives::U256::ZERO);
        let new_balance = current_balance.checked_add(amount).ok_or_else(|| {
            DbError::Decode(format!(
                "stk_aave_balance overflow on Staked: {current_balance} + {amount}"
            ))
        })?;
        let updated = conn.execute(
            "UPDATE aave_v3_users SET stk_aave_balance = ?1 WHERE id = ?2",
            params![new_balance.to_string(), user_id],
        )?;
        if updated == 0 {
            return Err(DbError::MissingRow(format!(
                "aave_v3_users id={user_id} (StkAaveStaked apply target — row vanished mid-tx)"
            )));
        }
        Ok(())
    }

    /// `&self` wrapper for [`Self::apply_stk_aave_staked_on_conn`].
    ///
    /// # Errors
    ///
    /// Same as the `_on_conn` variant.
    pub fn apply_stk_aave_staked(
        &self,
        user_id: i64,
        amount: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock();
        Self::apply_stk_aave_staked_on_conn(&conn, user_id, amount)
    }

    /// Apply a stkAAVE `Redeem` event: decrement the user's `stk_aave_balance`
    /// by `amount`. Port of `stkaave.process_stk_aave_transfer_event` (the
    /// `from_user` path; the Python `Transfer(from=X, to=0)` arm). The Python
    /// asserts `stk_aave_balance >= 0` before applying — the Rust apply
    /// returns [`DbError::Decode`] if the burn would underflow (amount >
    /// current balance), mirroring the Python's `assert from_user.stk_aave_balance >= 0`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure, [`DbError::MissingRow`]
    /// if no user matches `user_id`, [`DbError::Decode`] if the persisted
    /// string is malformed or the burn would underflow the balance.
    pub fn apply_stk_aave_redeem_on_conn(
        conn: &rusqlite::Connection,
        user_id: i64,
        amount: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        let current: Option<Option<String>> = conn
            .query_row(
                "SELECT stk_aave_balance FROM aave_v3_users WHERE id = ?1",
                [user_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?;
        let Some(current_str) = current else {
            return Err(DbError::MissingRow(format!(
                "aave_v3_users id={user_id} (StkAaveRedeem apply target)"
            )));
        };
        let current_balance = current_str
            .as_deref()
            .map(parse_decimal_u256)
            .transpose()?
            .unwrap_or(alloy::primitives::U256::ZERO);
        // Mirror the Python `assert from_user.stk_aave_balance >= 0` —
        // since `amount` is unsigned, an underflow here means the user's
        // balance would go negative, which is a protocol violation.
        let new_balance = current_balance.checked_sub(amount).ok_or_else(|| {
            DbError::Decode(
                format!(
                    "stk_aave_balance underflow on Redeem: {current_balance} - {amount} (user_id={user_id})"
                ),
            )
        })?;
        let updated = conn.execute(
            "UPDATE aave_v3_users SET stk_aave_balance = ?1 WHERE id = ?2",
            params![new_balance.to_string(), user_id],
        )?;
        if updated == 0 {
            return Err(DbError::MissingRow(format!(
                "aave_v3_users id={user_id} (StkAaveRedeem apply target — row vanished mid-tx)"
            )));
        }
        Ok(())
    }

    /// `&self` wrapper for [`Self::apply_stk_aave_redeem_on_conn`].
    ///
    /// # Errors
    ///
    /// Same as the `_on_conn` variant.
    pub fn apply_stk_aave_redeem(
        &self,
        user_id: i64,
        amount: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock();
        Self::apply_stk_aave_redeem_on_conn(&conn, user_id, amount)
    }

    /// Apply a `RewardsController` `RewardsClaimed` event. **No-op** —
    /// investigation (RYKCC4, 2026-07-04) confirmed the Python `event_handlers.py`
    /// defines `AaveV3RewardsControllerEvent.REWARDS_CLAIMED` in
    /// `src/degenbot/aave/events.py` but has NO handler for it: rewards claims
    /// surface only via the stkAAVE token's `Transfer` events (see
    /// `transaction_processor.py:249` — "This handles cases where stkAAVE
    /// transfers (e.g., rewards claims) occur"). The DB has no rewards table.
    /// The variant exists in `AaveChunkEvent` so the parser can route the
    /// event through the chunk pipeline (for operation-classification + log
    /// accounting), but the apply dispatch records the count + writes nothing.
    ///
    /// # Errors
    ///
    /// Never errors — the no-op always returns `Ok(())`.
    pub fn apply_rewards_claimed_on_conn(
        _conn: &rusqlite::Connection,
        _user_id: i64,
        _reward_token_id: i64,
        _claimer_id: i64,
        _claimed_amount: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        Ok(())
    }

    /// `&self` wrapper for [`Self::apply_rewards_claimed_on_conn`].
    ///
    /// # Errors
    ///
    /// Same as the `_on_conn` variant (never errors).
    pub fn apply_rewards_claimed(
        &self,
        user_id: i64,
        reward_token_id: i64,
        claimer_id: i64,
        claimed_amount: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock();
        Self::apply_rewards_claimed_on_conn(
            &conn,
            user_id,
            reward_token_id,
            claimer_id,
            claimed_amount,
        )
    }

    // ── HQF5NQ-A substrate lookups (the parser's address→id resolution) ──
    //
    // Four lookups the parser needs (each verified non-existent via
    // `grep "pub fn get_or_create\|pub fn lookup" write.rs` before being
    // added — Finding 2 of HQF5NQ's BLOCKED-FOR-SPLIT-DECISION). Each mirrors
    // the Python `operations_parser.py::_get_*` helpers in shape — `&Connection`
    // for the chunk-tx §3.4 invariant (one `Transaction` per chunk).

    /// Get-or-create an `aave_gho_tokens` row by `(chain_id, token_address)`.
    /// Port of the GHO-token resolution the parser + apply glue uses to
    /// resolve the `gho_token_id` parameter the GHO apply fns take (RYKCC4
    /// flag #6 follow-up; the apply fns `apply_gho_discount_rate_strategy_updated_on_conn`
    /// / `apply_gho_discount_token_updated_on_conn` require a pre-resolved
    /// `gho_token_id`).
    ///
    /// On create, inserts a bare row (`token_id` resolved via
    /// [`Self::get_or_create_erc20_token_on_conn`]; `v_token_id` left `NULL`; the
    /// `v_gho_discount_rate_strategy` / `v_gho_discount_token` columns are
    /// filled later by their apply fns). On an existing row, returns the
    /// `id` (no UPSERT — the columns are managed by the GHO-config apply
    /// fns).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn get_or_create_gho_token_on_conn(
        conn: &rusqlite::Connection,
        chain_id: i64,
        token_address: &str,
    ) -> Result<i64, DbError> {
        if let Some(id) = existing_gho_token(conn, chain_id, token_address)? {
            return Ok(id);
        }
        // Resolve the erc20_tokens row id (the FK) — the caller passes the
        // address; we look up / insert the token-row first.
        let token_id = Self::get_or_create_erc20_token_on_conn(
            conn,
            chain_id,
            token_address,
            None,
            None,
            None,
        )?;
        conn.execute(
            "INSERT INTO aave_gho_tokens (token_id) VALUES (?1)",
            params![token_id],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// The `&self` wrapper for [`Self::get_or_create_gho_token_on_conn`].
    /// # Errors
    ///
    /// Same error conditions as the `_on_conn` variant.
    pub fn get_or_create_gho_token(
        &self,
        chain_id: i64,
        token_address: &str,
    ) -> Result<i64, DbError> {
        let conn = self.conn.lock();
        Self::get_or_create_gho_token_on_conn(&conn, chain_id, token_address)
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

    /// Resolve the Pool contract revision (the `_get_pool_revision` port).
    /// DP4: read once per tx at parse-start; mid-tx `PoolUpdated` config
    /// events are the orchestrator's concern.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn lookup_pool_revision_on_conn(
        conn: &rusqlite::Connection,
        market_id: i64,
        contract_name: &str,
    ) -> Result<Option<u32>, DbError> {
        // `revision` is NULL-able; the row may also be missing. `.optional()` on
        // the `QueryResult` converts a no-row case to `Ok(None)`; `.flatten()`
        // then collapses the NULL column.
        let rev: Option<Option<i64>> = conn
            .query_row(
                "SELECT revision FROM aave_v3_contracts WHERE market_id = ?1 AND name = ?2",
                params![market_id, contract_name],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()?;
        Ok(rev.flatten().map(|v| u32::try_from(v).unwrap_or(0)))
    }

    /// Resolve the collateral (aToken) or debt (vToken) position-row id by
    /// `(user_id, asset_id, position_type)`. Mirrors the parser's need to
    /// look up the `BalanceTransfer` recipient's collateral position for the
    /// `DEFICIT_COVERAGE` triplet. `position_type` is `'collateral'` or `'debt'`
    /// (the parser / dispatch glue selects the table).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn lookup_position_by_user_asset_on_conn(
        conn: &rusqlite::Connection,
        user_id: i64,
        asset_id: i64,
        position_type: &str,
    ) -> Result<Option<i64>, DbError> {
        let (table, fk) = match position_type {
            "collateral" => ("aave_v3_collateral_positions", "asset_id"),
            "debt" => ("aave_v3_debt_positions", "asset_id"),
            _ => {
                return Err(DbError::Decode(format!(
                    "unexpected position_type '{position_type}' (expected 'collateral' or 'debt')"
                )));
            }
        };
        let sql = format!("SELECT id FROM {table} WHERE user_id = ?1 AND {fk} = ?2");
        Ok(conn
            .query_row(&sql, rusqlite::params![user_id, asset_id], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?)
    }

    /// Read a position's current `balance` + `last_index`. Used by the GHO
    /// apply dispatch (C3 — `UnifiedGhoProcessor::accrue_debt_on_action` +
    /// `get_discounted_balance` need the position's prev balance/index to
    /// compute the discount-scaled amount). Mirrors the Python's read of
    /// `debt_position.balance` / `.last_index` on the ORM-tracked position.
    ///
    /// Returns `(balance, None)` for a position whose `last_index` is NULL
    /// (a fresh position). Returns [`DbError::MissingRow`] if the position id
    /// has no row.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::MissingRow`] if no position matches `position_id`,
    /// or [`DbError::Decode`] on a malformed persisted balance string.
    pub fn lookup_position_balance_index_on_conn(
        conn: &rusqlite::Connection,
        position: ScaledTokenPosition,
        position_id: i64,
    ) -> Result<(alloy::primitives::U256, Option<alloy::primitives::U256>), DbError> {
        let sql = match position.table() {
            "aave_v3_collateral_positions" => {
                "SELECT balance, last_index FROM aave_v3_collateral_positions WHERE id = ?1"
            }
            "aave_v3_debt_positions" => {
                "SELECT balance, last_index FROM aave_v3_debt_positions WHERE id = ?1"
            }
            _ => unreachable!("bad table"),
        };
        let row: (String, Option<String>) = conn
            .query_row(sql, rusqlite::params![position_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    DbError::MissingRow(format!("{} id={position_id}", position.table()))
                }
                e => DbError::Sqlite(e),
            })?;
        let balance = parse_decimal_u256(&row.0)?;
        let last_index = row.1.as_deref().and_then(|s| parse_decimal_u256(s).ok());
        Ok((balance, last_index))
    }

    /// Reset a debt position's balance to 0 + advance `last_index` (the
    /// bad-debt liquidation path — C3). Mirrors the Python's
    /// `debt_position.balance = 0` + `if index > current_index: last_index =
    /// index` guard in `_process_debt_burn_with_match`'s bad-debt arm. The
    /// contract burns the ENTIRE remaining debt when a `DeficitCreated`
    /// accompanies a `LiquidationCall`; the delta-based apply may be off by 1
    /// wei, so this reset is the faithful path.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::MissingRow`] if `position_id` has no row, or
    /// [`DbError::Sqlite`] on an UPDATE failure.
    pub fn reset_debt_position_to_zero_on_conn(
        conn: &rusqlite::Connection,
        position_id: i64,
        new_index: alloy::primitives::U256,
    ) -> Result<(), DbError> {
        // Read the current last_index (max-with-prev reconciliation — mirrors
        // the Python's `if scaled_event.index > current_index` guard).
        let current_index: Option<String> = conn
            .query_row(
                "SELECT last_index FROM aave_v3_debt_positions WHERE id = ?1",
                rusqlite::params![position_id],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => DbError::MissingRow(format!(
                    "aave_v3_debt_positions id={position_id} (reset target)"
                )),
                e => DbError::Sqlite(e),
            })?;
        let current_index_u256 = current_index
            .as_deref()
            .and_then(|s| parse_decimal_u256(s).ok());
        let new_last_index = match current_index_u256 {
            Some(cur) if new_index > cur => Some(new_index),
            Some(_) => current_index_u256, // keep the prior higher index
            None => Some(new_index),       // first event: set it
        };
        conn.execute(
            "UPDATE aave_v3_debt_positions SET balance = ?1, last_index = ?2 WHERE id = ?3",
            rusqlite::params!["0", new_last_index.map(|i| i.to_string()), position_id],
        )?;
        Ok(())
    }
}

/// The asset-row view the parser uses (aToken-vToken-Underlying triplet +
/// revisions). Returned by [`DegenbotDb::lookup_asset_by_token_address_on_conn`].
/// Owned (no lifetime) — small enough to clone.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::module_name_repetitions)]
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

/// Which position table a `ScaledToken` event applies to (mirrors the
/// `CollateralMintEvent`/`DebtMintEvent` dispatch in the Python `_process_
/// scaled_token_operation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaledTokenPosition {
    /// `aave_v3_collateral_positions` (aToken Mint/Burn/BalanceTransfer).
    Collateral,
    /// `aave_v3_debt_positions` (vToken Mint/Burn).
    Debt,
}

impl ScaledTokenPosition {
    /// The literal table name (compile-time-dispatched; no injection surface).
    #[must_use]
    pub const fn table(self) -> &'static str {
        match self {
            Self::Collateral => "aave_v3_collateral_positions",
            Self::Debt => "aave_v3_debt_positions",
        }
    }

    /// The literal `balance`/`last_index` column pair (identical across both
    /// tables — `(balance VARCHAR(78) NOT NULL, last_index VARCHAR(78))`).
    #[must_use]
    pub const fn balance_col(self) -> &'static str {
        "balance"
    }

    #[must_use]
    pub const fn last_index_col(self) -> &'static str {
        "last_index"
    }
}

/// The shared `mint`/`burn` apply body (both are signed deltas). Reads the
/// position's current `balance` + `last_index`, adds the signed
/// `balance_delta`, conditionally advances `last_index`, writes both back.
///
/// # `last_index` reconciliation
///
/// Advances `last_index` to `new_index` ONLY when `new_index >
/// COALESCE(current_last_index, 0)` — mirrors the Python
/// `if mint_result.new_index > (position.last_index or 0) { position.last_index
/// = mint_result.new_index }`. The max-with-prev guard prevents an out-of-log-
/// order event from clobbering a newer event's index.
///
/// # Errors
///
/// Returns [`DbError::MissingRow`] if no position matches `position_id`.
fn apply_scaled_token_balance_delta_on_conn(
    conn: &rusqlite::Connection,
    position: ScaledTokenPosition,
    position_id: i64,
    balance_delta: alloy::primitives::I256,
    new_index: alloy::primitives::U256,
) -> Result<(), DbError> {
    let table = position.table();
    let sql = match table {
        "aave_v3_collateral_positions" => {
            "SELECT balance, last_index FROM aave_v3_collateral_positions WHERE id = ?1"
        }
        "aave_v3_debt_positions" => {
            "SELECT balance, last_index FROM aave_v3_debt_positions WHERE id = ?1"
        }
        _ => unreachable!("ScaledTokenPosition: bad table {table:?}"),
    };
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(sql, params![position_id], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
            ))
        })
        .optional()?;
    let Some((balance_str, last_index_str)) = row else {
        return Err(DbError::MissingRow(format!(
            "{table} id={position_id} (ScaledToken apply target)"
        )));
    };
    let current_balance =
        parse_decimal_u256(&balance_str.ok_or_else(|| {
            DbError::Decode(format!("{table} id={position_id}: balance is NULL"))
        })?)?;
    let current_index = match last_index_str {
        Some(s) => Some(parse_decimal_u256(&s)?),
        None => None,
    };
    // Compute new balance (I256 arithmetic — delta may be negative).
    let current_balance_i =
        alloy::primitives::I256::try_from(current_balance).unwrap_or(alloy::primitives::I256::MAX);
    let new_balance_i = current_balance_i + balance_delta;
    if new_balance_i < alloy::primitives::I256::ZERO {
        return Err(DbError::Decode(format!(
            "{table} id={position_id}: balance would go negative (current={current_balance}, delta={balance_delta})"
        )));
    }
    let new_balance: alloy::primitives::U256 = new_balance_i.try_into().map_err(|_| {
        DbError::Decode(format!(
            "{table} id={position_id}: new balance overflows U256 (={new_balance_i})"
        ))
    })?;
    // last_index reconciliation — max-with-prev (mirrors the Python guard).
    let new_last_index = match current_index {
        Some(cur) if new_index > cur => Some(new_index),
        Some(_) => current_index, // keep the prior higher index
        None => Some(new_index),  // first event: set it
    };
    let update_sql = match table {
        "aave_v3_collateral_positions" => {
            "UPDATE aave_v3_collateral_positions SET balance = ?1, last_index = ?2 WHERE id = ?3"
        }
        "aave_v3_debt_positions" => {
            "UPDATE aave_v3_debt_positions SET balance = ?1, last_index = ?2 WHERE id = ?3"
        }
        _ => unreachable!(),
    };
    conn.execute(
        update_sql,
        params![
            new_balance.to_string(),
            new_last_index.map(|i| i.to_string()),
            position_id,
        ],
    )?;
    Ok(())
}

/// Parse a decimal-`VARCHAR` `U256` value. Mirrors the Python `int(s)` parse
/// of an `AaveV3*Position.balance` attribute read back from the DB.
///
/// # Errors
///
/// Returns [`DbError::Decode`] if the string is not a valid decimal
/// `U256`.
fn parse_decimal_u256(s: &str) -> Result<alloy::primitives::U256, DbError> {
    alloy::primitives::U256::from_str_radix(s, 10)
        .map_err(|e| DbError::Decode(format!("bad decimal U256 {s:?}: {e}")))
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

/// Look up an `aave_gho_tokens.id` by the underlying GHO token's `(chain, address)`
/// — joins through `erc20_tokens` (the `aave_gho_tokens` table is keyed by
/// `token_id`, not by address).
#[allow(clippy::missing_errors_doc)]
fn existing_gho_token(
    conn: &rusqlite::Connection,
    chain_id: i64,
    token_address: &str,
) -> Result<Option<i64>, DbError> {
    Ok(conn
        .query_row(
            "SELECT g.id FROM aave_gho_tokens g
             JOIN erc20_tokens t ON t.id = g.token_id
             WHERE t.chain = ?1 AND t.address = ?2",
            params![chain_id, token_address],
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
        let (balance, last_index) = DegenbotDb::lookup_position_balance_index_on_conn(
            &conn,
            ScaledTokenPosition::Debt,
            pid,
        )
        .unwrap();
        assert_eq!(balance, alloy::primitives::U256::from(1_000u64));
        assert_eq!(last_index, Some(alloy::primitives::U256::from(123u64)));
    }

    #[test]
    fn lookup_position_balance_index_missing_row_returns_err() {
        let db = write_db_with_market();
        let conn = db.conn.lock();
        let err = DegenbotDb::lookup_position_balance_index_on_conn(
            &conn,
            ScaledTokenPosition::Debt,
            9999,
        )
        .unwrap_err();
        assert!(matches!(err, DbError::MissingRow(_)), "got {err:?}");
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
        let (balance, last_index) = DegenbotDb::lookup_position_balance_index_on_conn(
            &conn,
            ScaledTokenPosition::Debt,
            pid,
        )
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
        let (_, last_index) = DegenbotDb::lookup_position_balance_index_on_conn(
            &conn,
            ScaledTokenPosition::Debt,
            pid,
        )
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
}
