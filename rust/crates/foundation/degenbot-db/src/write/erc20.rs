//! ERC-20 token get-or-create + metadata write-back.

use super::{params, DbError, DegenbotDb, OptionalExtension};

fn existing_erc20_token(
    conn: &rusqlite::Connection,
    chain: i64,
    address: &str,
) -> Result<Option<i64>, DbError> {
    // OONKWO: prepare_cached caches the compiled statement across calls. The only
    // PRODUCTION caller today (discovery.rs per-pool get_or_create_erc20_token)
    // is cold/sparse; this banks the ~4× for when the Aave migration ports the
    // Python event handlers to these Rust get_or_create_* paths.
    let mut s =
        conn.prepare_cached("SELECT id FROM erc20_tokens WHERE chain = ?1 AND address = ?2")?;
    Ok(s.query_row(params![chain, address], |r| r.get(0))
        .optional()?)
}
impl DegenbotDb {
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
    /// accepts a borrowed [`rusqlite::Connection`] (a chunk-loop `Transaction`
    /// derefs to one) so the pool-updater chunk loop can call it on its ONE
    /// owned connection without re-locking the `Mutex` (avoids the
    /// `parking_lot` non-reentrant deadlock + retires the per-row lock cycle
    /// the `discovery::upsert_v*_pools` paths previously needed).
    /// # Errors
    ///
    /// Same error conditions as the `&self` wrapper variant.
    pub fn get_or_create_erc20_token_on_conn(
        conn: &rusqlite::Connection,
        chain: i64,
        address: &str,
        name: Option<&str>,
        symbol: Option<&str>,
        decimals: Option<i64>,
    ) -> Result<i64, DbError> {
        if let Some(id) = existing_erc20_token(conn, chain, address)? {
            // backfill any NULL metadata cells on the existing row
            // with the freshly-passed values. Mirrors the Python
            // `activate_ethereum_aave_v3` pre-pass (commands.py:213) which
            // drives `get_or_create_erc20_token` on a freshly-seeded row →
            // `_fetch_erc20_token_metadata` (RPC `name()`/`symbol()`/`decimals()`) +
            // INSERT with metadata. Rust defers the equivalent to drive-time:
            // when `resolve_reserve_initialized` fetches the metadata via
            // `fetch_erc20_metadata` + resolves the token here, the existing
            // row's NULL cells get backfilled. Pre-populated cells are NOT
            // clobbered (defensive guard — the `WHERE name IS NULL` etc. on
            // each COALESCE branch ensures only-null cells update).
            if name.is_some() || symbol.is_some() || decimals.is_some() {
                conn.execute(
                    "UPDATE erc20_tokens SET \
                     name = COALESCE(name, ?3), \
                     symbol = COALESCE(symbol, ?4), \
                     decimals = COALESCE(decimals, ?5) \
                     WHERE id = ?2 AND \
                     (name IS NULL OR symbol IS NULL OR decimals IS NULL)",
                    params![chain, id, name, symbol, decimals],
                )?;
            }
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
}
