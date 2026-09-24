//! Aave Pool-event direct writers: reserve data/init, upgrades, contract rows.

use super::{params, DbError, DegenbotDb, OptionalExtension, U256};

/// Lookup `aave_v3_assets.id` by the `(market_id, underlying_asset_id)`
/// natural key. The `ReserveInitialized` get-or-create path's existing-row
/// probe.
fn existing_aave_v3_asset(
    conn: &rusqlite::Connection,
    market_id: i64,
    underlying_asset_id: i64,
) -> Result<Option<i64>, DbError> {
    // OONKWO: prepare_cached caches the compiled statement across calls.
    let mut s = conn.prepare_cached(
        "SELECT id FROM aave_v3_assets \
         WHERE market_id = ?1 AND underlying_asset_id = ?2",
    )?;
    Ok(
        s.query_row(params![market_id, underlying_asset_id], |r| r.get(0))
            .optional()?,
    )
}
impl DegenbotDb {
    /// Set the `aave_v3_markets.last_update_block` stamp for `market_id`.
    /// This is the Aave-updater chunk loop's end-of-chunk stamp, the mirror of
    /// [`DegenbotDb::set_exchange_last_update_block_on_conn`] for the pool
    /// loop. Callable on the chunk's `Transaction` so the stamp
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
    /// (`getSourceOfAsset`) happen in the orchestrator; the apply fn
    /// takes pre-resolved fields (mirrors design decision #1 — the
    /// apply core is pure substrate, no RPC). The GHO cross-link setup if the
    /// asset IS the GHO token is RYKCC4's concern, NOT this fn's.
    ///
    /// Returns the asset row `id`.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    #[expect(clippy::too_many_arguments)] // mirrors the Python event arg list 1:1
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
    #[expect(clippy::too_many_arguments)] // mirrors the Python event arg list 1:1
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

    // ── the 6 missing-variant config-event apply fns ──────────

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

    /// Idempotent variant of [`Self::apply_contract_inserted_on_conn`]: INSERT
    /// the contract row ONLY if no row with the same `(market_id, name,
    /// address)` already exists. Returns `true` if a row was inserted, `false`
    /// if a matching row pre-existed (no-op). A pre-existing row's `revision`
    /// is NOT overwritten (the "Phase-1 wins" semantics — the row from the first
    /// `ProxyCreated` event is canonical, mirroring the Python
    /// `_process_proxy_creation_event` which appends unconditionally but is fed
    /// each `ProxyCreated` exactly once).
    ///
    /// # Why this exists (O4BOST cold-boot)
    ///
    /// The Rust `run_aave_update` bootstrap pass fetches `ProxyCreated` events
    /// over `[from_block, from_block + BOOTSTRAP_WINDOW]` + applies them BEFORE
    /// `build_fetch_spec`. The chunk loop then re-processes `[from_block,
    /// to_block]` (overlapping the bootstrap window) + the chunk dispatcher's
    /// `ContractInserted` arm re-encounters the same `ProxyCreated(POOL)` /
    /// `ProxyCreated(POOL_CONFIGURATOR)` events. Without idempotency, the
    /// re-encounter would insert a duplicate row. The chunk arm routes ONLY
    /// `name ∈ {"POOL", "POOL_CONFIGURATOR"}` through this idempotent variant;
    /// all other `ContractInserted` names (`POOL_DATA_PROVIDER`, `PRICE_ORACLE`,
    /// `AddressSet`-decoded names) keep the unconditional
    /// [`Self::apply_contract_inserted_on_conn`] (parity with the Python which
    /// also unconditional-appends them).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a query failure.
    pub fn apply_contract_inserted_if_absent_on_conn(
        conn: &rusqlite::Connection,
        market_id: i64,
        name: &str,
        address: &str,
        revision: Option<i64>,
    ) -> Result<bool, DbError> {
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM aave_v3_contracts \
                 WHERE market_id = ?1 AND name = ?2 AND address = ?3",
                params![market_id, name, address],
                |row| row.get(0),
            )
            .optional()?;
        if existing.is_some() {
            return Ok(false);
        }
        conn.execute(
            "INSERT INTO aave_v3_contracts (market_id, name, address, revision) \
             VALUES (?1, ?2, ?3, ?4)",
            params![market_id, name, address, revision],
        )?;
        Ok(true)
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
}
