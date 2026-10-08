//! The completed-market record writer: per-table row counts + canonical
//! per-table digests for the ten Aave market tables, and the manifest
//! rendering the record is committed as
//! (`tests/fixtures/sql_goldens/mainnet_completed_market.manifest.json`).
//!
//! A completed-market record is machine-emitted through THIS pipeline — one
//! writer, one serialization — never re-derived by ad-hoc shell. The digest
//! fns read a live DB through a `&Connection` (the `_on_conn` seam: lock a
//! read-only [`DegenbotDb::open`][crate::connection::DegenbotDb::open]
//! handle); [`render_completed_market_manifest`] renders the record bytes
//! the console's `aave digest` arm prints and a drive pins.
//!
//! # The canonical serialization (`MARKET_DIGEST_SERIALIZATION`)
//!
//! One table at a time, into a byte stream consumed only by SHA-256:
//!
//! 1. **Columns** — the table's declared column order, as `PRAGMA
//!    table_info` reports it at digest time (the schema head's declaration
//!    order for the schema version the DB carries). A migration that adds
//!    or reorders a column changes every digest by construction; the
//!    record must be re-pinned in the same change.
//! 2. **Row order** — primary key ascending: the `pk` columns of
//!    `PRAGMA table_info` in declared ordinality (every market table's key
//!    is `id`).
//! 3. **Field encoding** — fields joined by `|` (U+007C):
//!    - `NULL` → `\N` (distinct from empty TEXT, which encodes as the
//!      empty field);
//!    - `INTEGER` → plain decimal (`BOOLEAN` columns are 0/1 integers);
//!    - `TEXT` → escaped, backslash first: `\` → `\\`, then `|` → `\|`,
//!      LF → `\n`, CR → `\r`;
//!    - `REAL` / `BLOB` → refused ([`DbError::Decode`]). None of the ten
//!      tables declares one; a column of that kind must extend this spec
//!      and bump [`MARKET_DIGEST_SERIALIZATION`] rather than silently hash
//!      raw bytes.
//! 4. **Digest** — every encoded row is followed by one `\n` (an escaped
//!    TEXT field cannot contain a bare LF, so row boundaries are
//!    unambiguous) and fed to SHA-256; the table digest is the lowercase
//!    hex of the final hash. An empty table digests the empty stream.
//!
//! Manifests digested under different serialization versions carry
//! different digest bytes by construction; the manifest's `serialization`
//! key names the writer whose digests it carries.

use std::fmt::Write as _;

use rusqlite::types::ValueRef;
use rusqlite::Connection;
use sha2::{Digest as _, Sha256};

use crate::error::DbError;

/// The serialization version stamped on every manifest this writer emits.
/// Bump when the canonical serialization changes in a way that changes any
/// digest byte.
pub const MARKET_DIGEST_SERIALIZATION: &str = "degenbot-market-digest/v1";

/// The ten Aave market tables the completed-market record digests, sorted.
pub const MARKET_DIGEST_TABLES: [&str; 10] = [
    "aave_gho_tokens",
    "aave_v3_asset_configs",
    "aave_v3_assets",
    "aave_v3_collateral_positions",
    "aave_v3_contracts",
    "aave_v3_debt_positions",
    "aave_v3_emode_categories",
    "aave_v3_markets",
    "aave_v3_user_collateral_configs",
    "aave_v3_users",
];

/// One market table's record — a manifest `tables` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDigest {
    /// The table name.
    pub table: String,
    /// Rows hashed.
    pub rows: u64,
    /// The canonical serialization's SHA-256 over the table, lowercase hex.
    pub digest: String,
}

/// The `aave_v3_markets` row the record's `market` block carries, plus the
/// logical database size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketFacts {
    /// The market row's `id`.
    pub market_id: i64,
    /// The chain the market trades on.
    pub chain_id: i64,
    /// The market's name (the on-chain `getMarketId()` return).
    pub name: String,
    /// The market row's active flag.
    pub active: bool,
    /// The market's committed cursor (`None` before the first stamp).
    pub last_update_block: Option<i64>,
    /// The logical database size (`PRAGMA page_count * page_size`).
    pub db_bytes: u64,
}

/// The drive bookkeeping a completed-market record carries
/// (`market.drive`): facts only the drive run itself knows — not derivable
/// from the DB — so a re-pin carries them from the previous record
/// verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedMarketDrive {
    /// Chunks the drive committed.
    pub chunks: u64,
    /// Events the drive applied.
    pub events_applied: u64,
    /// First block the drive processed.
    pub from_block: u64,
    /// Last block the drive processed (the market's committed cursor).
    pub to_block: u64,
    /// The drive's verification policy, verbatim.
    pub verify: String,
}

/// The completed-market digest: the market facts + the ten table records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketDigest {
    /// The serialization version the digests were computed under.
    pub serialization: &'static str,
    /// The market row's facts + the logical DB size.
    pub market: MarketFacts,
    /// One record per market table, sorted by table name.
    pub tables: Vec<TableDigest>,
}

/// Digest the DB's single Aave V3 market.
///
/// # Errors
///
/// [`DbError::MissingRow`] when no `aave_v3_markets` row exists,
/// [`DbError::Decode`] when several exist (disambiguate with
/// [`aave_market_digest_for`]), and the digest/SQL errors of
/// [`aave_market_digest_for`].
pub fn aave_market_digest(conn: &Connection) -> Result<MarketDigest, DbError> {
    aave_market_digest_for(conn, None, None)
}

/// Digest one Aave V3 market, selected by chain and optional market name.
///
/// # Errors
///
/// [`DbError::MissingRow`] when no market row matches the selection,
/// [`DbError::Decode`] when the selection matches several rows (name the
/// market explicitly), when a `BOOLEAN` column is not 0/1, or when a table
/// declares a `REAL`/`BLOB` column the canonical serialization does not
/// define; [`DbError::Sqlite`] on query failures.
pub fn aave_market_digest_for(
    conn: &Connection,
    chain_id: Option<i64>,
    market_name: Option<&str>,
) -> Result<MarketDigest, DbError> {
    let market = fetch_market_facts(conn, chain_id, market_name)?;
    let mut tables = Vec::with_capacity(MARKET_DIGEST_TABLES.len());
    for table in MARKET_DIGEST_TABLES {
        tables.push(digest_market_table(conn, table)?);
    }
    Ok(MarketDigest {
        serialization: MARKET_DIGEST_SERIALIZATION,
        market,
        tables,
    })
}

/// Render the completed-market manifest JSON — the exact bytes a drive
/// commits (2-space indent, sorted keys, no trailing newline; the printer
/// adds the final LF).
///
/// `drive` is the record's only caller-supplied content: the drive facts
/// are the drive's own bookkeeping, not DB state, and render as `null`
/// when no previous record was named to carry them from.
#[must_use]
pub fn render_completed_market_manifest(
    digest: &MarketDigest,
    drive: Option<&CompletedMarketDrive>,
) -> String {
    let mut json = String::with_capacity(1024 + digest.tables.len() * 144);
    json.push_str("{\n  \"market\": {\n");
    let _ = writeln!(json, "    \"active\": {},", digest.market.active);
    let _ = writeln!(json, "    \"chain_id\": {},", digest.market.chain_id);
    let _ = writeln!(json, "    \"db_bytes\": {},", digest.market.db_bytes);
    match drive {
        Some(drive) => {
            json.push_str("    \"drive\": {\n");
            let _ = writeln!(json, "      \"chunks\": {},", drive.chunks);
            let _ = writeln!(json, "      \"events_applied\": {},", drive.events_applied);
            let _ = writeln!(json, "      \"from_block\": {},", drive.from_block);
            let _ = writeln!(json, "      \"to_block\": {},", drive.to_block);
            json.push_str("      \"verify\": ");
            push_json_string(&mut json, &drive.verify);
            json.push_str("\n    },\n");
        }
        None => json.push_str("    \"drive\": null,\n"),
    }
    match digest.market.last_update_block {
        Some(block) => {
            let _ = writeln!(json, "    \"last_update_block\": {block},");
        }
        None => json.push_str("    \"last_update_block\": null,\n"),
    }
    json.push_str("    \"name\": ");
    push_json_string(&mut json, &digest.market.name);
    json.push_str("\n  },\n");
    let _ = writeln!(
        json,
        "  \"serialization\": \"{MARKET_DIGEST_SERIALIZATION}\","
    );
    json.push_str("  \"tables\": {\n");
    for (index, table) in digest.tables.iter().enumerate() {
        // Table names are this crate's own const vocabulary
        // (`MARKET_DIGEST_TABLES`), so they interpolate raw.
        let _ = write!(
            json,
            "    \"{}\": {{\n      \"digest\": \"{}\",\n      \"rows\": {}\n    }}",
            table.table, table.digest, table.rows
        );
        if index + 1 < digest.tables.len() {
            json.push(',');
        }
        json.push('\n');
    }
    // No trailing LF: the console's `writeln!` printer adds the document's
    // one final newline, so a redirected pin ends in exactly one.
    json.push_str("  }\n}");
    json
}

/// Fetch the selected market row and the logical DB size.
fn fetch_market_facts(
    conn: &Connection,
    chain_id: Option<i64>,
    market_name: Option<&str>,
) -> Result<MarketFacts, DbError> {
    let mut sql =
        String::from("SELECT id, chain_id, name, active, last_update_block FROM aave_v3_markets");
    let mut params: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(chain) = chain_id {
        sql.push_str(" WHERE chain_id = ?");
        params.push(chain.into());
    }
    if let Some(name) = market_name {
        sql.push_str(if params.is_empty() {
            " WHERE name = ?"
        } else {
            " AND name = ?"
        });
        params.push(name.to_string().into());
    }
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(params))?;
    let mut found: Option<MarketFacts> = None;
    while let Some(row) = rows.next()? {
        if found.replace(market_facts_from_row(row)?).is_some() {
            return Err(DbError::Decode(
                "the digest selection matched several aave_v3_markets rows; name the \
                 market explicitly"
                    .to_string(),
            ));
        }
    }
    let mut facts = found.ok_or_else(|| {
        DbError::MissingRow("no aave_v3_markets row matched the digest selection".to_string())
    })?;
    facts.db_bytes = database_bytes(conn)?;
    Ok(facts)
}

/// Decode one `aave_v3_markets` row into the record's market facts.
fn market_facts_from_row(row: &rusqlite::Row<'_>) -> Result<MarketFacts, DbError> {
    let active_raw: i64 = row.get(3)?;
    let active = match active_raw {
        0 => false,
        1 => true,
        other => {
            return Err(DbError::Decode(format!(
                "aave_v3_markets.active is {other}, not a 0/1 boolean"
            )));
        }
    };
    Ok(MarketFacts {
        market_id: row.get(0)?,
        chain_id: row.get(1)?,
        name: row.get(2)?,
        active,
        last_update_block: row.get(4)?,
        db_bytes: 0,
    })
}

/// The logical database size the record pins: `PRAGMA page_count *
/// page_size` — derived from the connection, the digest fn's only input.
fn database_bytes(conn: &Connection) -> Result<u64, DbError> {
    let page_count: i64 = conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let page_size: i64 = conn.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    let bytes = page_count
        .checked_mul(page_size)
        .ok_or_else(|| DbError::Decode("page_count * page_size overflowed i64".to_string()))?;
    u64::try_from(bytes)
        .map_err(|_| DbError::Decode("page_count * page_size is negative".to_string()))
}

/// Hash one market table under the canonical serialization.
fn digest_market_table(conn: &Connection, table: &str) -> Result<TableDigest, DbError> {
    let (columns, primary_key) = declared_layout(conn, table)?;
    let mut sql = String::with_capacity(
        64 + 8 * columns.len() + columns.iter().map(String::len).sum::<usize>(),
    );
    sql.push_str("SELECT ");
    push_quoted_list(&mut sql, &columns);
    sql.push_str(" FROM ");
    push_quoted(&mut sql, table);
    sql.push_str(" ORDER BY ");
    push_quoted_list(&mut sql, &primary_key);
    sql.push_str(" ASC");

    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    let mut hasher = Sha256::new();
    let mut row_count = 0u64;
    let mut line = String::new();
    while let Some(row) = rows.next()? {
        line.clear();
        for (index, column) in columns.iter().enumerate() {
            if index > 0 {
                line.push('|');
            }
            encode_field(row.get_ref(index)?, table, column, &mut line)?;
        }
        line.push('\n');
        hasher.update(line.as_bytes());
        row_count += 1;
    }
    Ok(TableDigest {
        table: table.to_string(),
        rows: row_count,
        digest: to_hex(&hasher.finalize()),
    })
}

/// The table's declared column order and primary-key column order, as
/// `PRAGMA table_info` reports them.
fn declared_layout(conn: &Connection, table: &str) -> Result<(Vec<String>, Vec<String>), DbError> {
    // The name comes from this crate's own `MARKET_DIGEST_TABLES`
    // vocabulary, never operator input — the PRAGMA interpolation is not an
    // injection surface.
    let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let mut columns: Vec<String> = Vec::new();
    let mut key: Vec<(i64, String)> = Vec::new();
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        let ordinal: i64 = row.get(5)?;
        if ordinal > 0 {
            key.push((ordinal, name.clone()));
        }
        columns.push(name);
    }
    if columns.is_empty() {
        return Err(DbError::MissingRow(format!(
            "market-digest table {table:?} is not in the schema"
        )));
    }
    if key.is_empty() {
        return Err(DbError::Decode(format!(
            "market-digest table {table:?} declares no primary key; the digest row \
             order is undefined"
        )));
    }
    key.sort_unstable_by_key(|(ordinal, _)| *ordinal);
    Ok((columns, key.into_iter().map(|(_, name)| name).collect()))
}

/// Encode one field under the canonical serialization (see the module docs).
fn encode_field(
    value: ValueRef<'_>,
    table: &str,
    column: &str,
    out: &mut String,
) -> Result<(), DbError> {
    match value {
        ValueRef::Null => out.push_str("\\N"),
        ValueRef::Integer(integer) => {
            let _ = write!(out, "{integer}");
        }
        ValueRef::Text(text) => encode_text(text, table, column, out)?,
        ValueRef::Real(_) => {
            return Err(DbError::Decode(format!(
                "market-digest table {table} column {column}: REAL values are not \
                 defined in the canonical serialization"
            )));
        }
        ValueRef::Blob(_) => {
            return Err(DbError::Decode(format!(
                "market-digest table {table} column {column}: BLOB values are not \
                 defined in the canonical serialization"
            )));
        }
    }
    Ok(())
}

/// TEXT fields escape the encoding's metacharacters — the backslash first,
/// so nothing inserted later is itself re-escaped on decode.
fn encode_text(text: &[u8], table: &str, column: &str, out: &mut String) -> Result<(), DbError> {
    let text = std::str::from_utf8(text).map_err(|_| {
        DbError::Decode(format!(
            "market-digest table {table} column {column}: TEXT is not valid UTF-8"
        ))
    })?;
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '|' => out.push_str("\\|"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    Ok(())
}

/// Push `value` as a quoted JSON string — the minimal escape set (quote,
/// backslash, C0 controls); the record's strings are operator-or-chain
/// data, so the escaper is total.
fn push_json_string(json: &mut String, value: &str) {
    json.push('"');
    for character in value.chars() {
        match character {
            '"' => json.push_str("\\\""),
            '\\' => json.push_str("\\\\"),
            '\n' => json.push_str("\\n"),
            '\r' => json.push_str("\\r"),
            '\t' => json.push_str("\\t"),
            control if u32::from(control) < 0x20 => {
                let _ = write!(json, "\\u{:04x}", u32::from(control));
            }
            other => json.push(other),
        }
    }
    json.push('"');
}

/// Push a double-quoted, comma-separated identifier list.
fn push_quoted_list(sql: &mut String, columns: &[String]) {
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            sql.push_str(", ");
        }
        push_quoted(sql, column);
    }
}

/// Push a double-quoted SQL identifier.
fn push_quoted(sql: &mut String, identifier: &str) {
    sql.push('"');
    sql.push_str(&identifier.replace('"', "\"\""));
    sql.push('"');
}

/// Lowercase hex — the digest alphabet every committed record uses.
fn to_hex(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::connection::DegenbotDb;

    /// A write-capable in-memory DB carrying the embedded schema + one
    /// seeded market row (the schema head declares all ten market tables).
    fn seeded_db(market_name: &str) -> (DegenbotDb, crate::migrate::SchemaState) {
        let (db, state) = DegenbotDb::open_in_memory_for_writes().unwrap();
        db.lock()
            .execute(
                "INSERT INTO aave_v3_markets (id, chain_id, name, active, \
                 last_update_block) VALUES (1, 1, ?1, 1, 100)",
                [market_name],
            )
            .unwrap();
        (db, state)
    }

    #[test]
    fn digest_is_deterministic_and_covers_the_ten_tables() {
        let (db, _state) = seeded_db("Aave Ethereum Market");
        let first = {
            let conn = db.lock();
            aave_market_digest(&conn).unwrap()
        };
        let second = {
            let conn = db.lock();
            aave_market_digest(&conn).unwrap()
        };
        assert_eq!(first, second);
        assert_eq!(first.serialization, MARKET_DIGEST_SERIALIZATION);
        let names: Vec<&str> = first.tables.iter().map(|t| t.table.as_str()).collect();
        assert_eq!(names, MARKET_DIGEST_TABLES.to_vec());
        // One market row; every other market table is empty (the SHA-256 of
        // the empty stream).
        for table in &first.tables {
            let expected_rows = u64::from(table.table == "aave_v3_markets");
            assert_eq!(table.rows, expected_rows, "table {}", table.table);
            assert_eq!(table.digest.len(), 64);
        }
    }

    #[test]
    fn the_escaper_is_injective_over_the_metacharacters() {
        // `x|y` and the literal `x\|y` must encode differently — the
        // backslash-first escaping is what keeps field boundaries unambiguous.
        let (pipe_db, _state) = seeded_db("x|y");
        let (escaped_db, _state) = seeded_db("x\\|y");
        let pipe = {
            let conn = pipe_db.lock();
            aave_market_digest(&conn).unwrap()
        };
        let escaped = {
            let conn = escaped_db.lock();
            aave_market_digest(&conn).unwrap()
        };
        assert_ne!(pipe, escaped);
        // NULL and empty TEXT must also digest differently (the `\N`
        // sentinel) — probed on `aave_v3_contracts.revision`, whose only
        // foreign key the seeded market row already satisfies.
        let (null_db, _state) = seeded_db("m");
        null_db
            .lock()
            .execute(
                "INSERT INTO aave_v3_contracts (id, market_id, name, address) \
                 VALUES (1, 1, 'PoolAddressesProvider', '0xabc')",
                [],
            )
            .unwrap();
        let (empty_db, _state) = seeded_db("m");
        empty_db
            .lock()
            .execute(
                "INSERT INTO aave_v3_contracts (id, market_id, name, address, revision) \
                 VALUES (1, 1, 'PoolAddressesProvider', '0xabc', '')",
                [],
            )
            .unwrap();
        let null_digest = {
            let conn = null_db.lock();
            aave_market_digest(&conn).unwrap()
        };
        let empty_digest = {
            let conn = empty_db.lock();
            aave_market_digest(&conn).unwrap()
        };
        assert_ne!(null_digest, empty_digest);
    }

    #[test]
    fn the_rendered_manifest_is_valid_json_of_the_documented_shape() {
        let (db, _state) = seeded_db("Aave Ethereum Market");
        let digest = {
            let conn = db.lock();
            aave_market_digest(&conn).unwrap()
        };
        let drive = CompletedMarketDrive {
            chunks: 986,
            events_applied: 12_687_043,
            from_block: 16_291_071,
            to_block: 26_144_562,
            verify: "per-chunk on-chain truth \"+\" 1M boundaries".to_string(),
        };
        for drive in [Some(&drive), None] {
            let json = render_completed_market_manifest(&digest, drive);
            let record: serde_json::Value = serde_json::from_str(&json)
                .unwrap_or_else(|error| panic!("rendered manifest must parse: {error}"));
            let market = record.get("market").unwrap();
            assert_eq!(
                market.get("name").unwrap().as_str(),
                Some("Aave Ethereum Market")
            );
            assert_eq!(market.get("chain_id").unwrap().as_i64(), Some(1));
            assert_eq!(market.get("last_update_block").unwrap().as_i64(), Some(100));
            assert_eq!(
                market
                    .get("drive")
                    .and_then(|drive| drive.get("chunks"))
                    .and_then(serde_json::Value::as_u64),
                drive.map(|drive| drive.chunks)
            );
            assert_eq!(
                record.get("serialization").unwrap().as_str(),
                Some(MARKET_DIGEST_SERIALIZATION)
            );
            let tables = record.get("tables").unwrap().as_object().unwrap();
            assert_eq!(tables.len(), MARKET_DIGEST_TABLES.len());
        }
    }

    #[test]
    fn the_selection_refuses_ambiguous_and_absent_markets() {
        let (db, _state) = seeded_db("Aave Ethereum Market");
        db.lock()
            .execute(
                "INSERT INTO aave_v3_markets (id, chain_id, name, active) \
                 VALUES (2, 1, 'Second Market', 0)",
                [],
            )
            .unwrap();
        let ambiguous = {
            let conn = db.lock();
            aave_market_digest(&conn)
        };
        assert!(matches!(ambiguous, Err(DbError::Decode(_))));
        let named = {
            let conn = db.lock();
            aave_market_digest_for(&conn, Some(1), Some("Second Market")).unwrap()
        };
        assert_eq!(named.market.name, "Second Market");
        let absent = {
            let conn = db.lock();
            aave_market_digest_for(&conn, Some(2), None)
        };
        assert!(
            matches!(absent, Err(DbError::MissingRow(_))),
            "chain 2 has no market row"
        );
    }
}
