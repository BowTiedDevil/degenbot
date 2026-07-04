//! The Alembic-aware migration runner.
//!
//! The HARD REQUIREMENT (from the SLHSM4 binding): open an existing
//! Alembic-stamped degenbot `SQLite` DB, read the `alembic_version` table head,
//! and treat a stamped DB as current WITHOUT re-running DDL or clobbering its
//! revision. Both [`sqlx::migrate!`] and [`refinery`] fail this — each writes
//! its own migration-tracking table and would replay DDL against a DB it does
//! not recognize. This module is the small custom alternative (Decision 2).
//!
//! Algorithm ([`ensure_schema`]):
//! 1. If the `alembic_version` table exists:
//!    - read its single `version_num` row, compare against [`ALEMBIC_HEAD`];
//!    - match → [`SchemaState::AlembicCurrent`], write **nothing** (the
//!      Alembic-stamped production DBs stay Alembic-owned);
//!    - older rev → [`SchemaState::AlembicStale`], refuse (the writer path in
//!      Epic AZGJUN owns Alembic stamps; the Rust core never downgrades or
//!      forwards an Alembic DB);
//! 2. If `alembic_version` is absent but the file already has tables (a foreign
//!    `SQLite` file passed by mistake) → [`SchemaState::Unrecognized`], refuse;
//! 3. If `alembic_version` is absent AND the file is empty (a fresh standalone
//!    DB with no Alembic history) → apply the embedded DDL
//!    ([`SCHEMA_HEAD`]) + stamp the private [`SCHEMA_VERSION_TABLE`], return
//!    [`SchemaState::FreshStandalone`].

use rusqlite::Connection;

use crate::error::DbError;
use crate::schema::{ALEMBIC_HEAD, RUST_SCHEMA_VERSION, SCHEMA_HEAD, SCHEMA_VERSION_TABLE};

/// The schema disposition [`ensure_schema`] reports for an opened DB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaState {
    /// An Alembic-stamped DB at the expected head — the common hybrid-period
    /// case. The Rust core wrote nothing; every subsequent read honors
    /// `PRAGMA query_only=on`.
    AlembicCurrent,
    /// An Alembic-stamped DB at an OLDER revision — refuse to open. Run the
    /// Python Alembic upgrade (`alembic upgrade head`) to advance the stamp.
    AlembicStale {
        /// The `version_num` actually stamped in the DB.
        head: String,
        /// The constant the Rust core expects ([`ALEMBIC_HEAD`]).
        expected: String,
    },
    /// A fresh standalone DB (no Alembic history) — the embedded DDL was applied
    /// and [`SCHEMA_VERSION_TABLE`] was stamped with [`RUST_SCHEMA_VERSION`].
    FreshStandalone {
        /// The Rust-owned schema version written to [`SCHEMA_VERSION_TABLE`].
        schema_version: u32,
    },
    /// A Rust-owned DB (post-cutover, or a re-opened `FreshStandalone`) —
    /// tables present, no `alembic_version`, and [`SCHEMA_VERSION_TABLE`]
    /// stamped. The Rust core may write schema here through future Rust-owned
    /// migrations. `FreshStandalone` is the one-shot "I just created this"
    /// report; `RustOwned` is the steady state for any Rust-owned DB
    /// thereafter (ADR-010 §2).
    RustOwned {
        /// The value stamped in [`SCHEMA_VERSION_TABLE`].
        schema_version: u32,
    },
    /// `alembic_version` is absent but the file already holds tables — a foreign
    /// `SQLite` file passed by mistake. [`ensure_schema`] refuses; [`crate::connection::DegenbotDb::open`]
    /// maps this to [`crate::error::DbError::UnrecognizedSchema`].
    Unrecognized,
}

/// The result of [`convert_alembic_to_rust_owned`] — the DB is now Rust-owned
/// at [`RUST_SCHEMA_VERSION`]. A struct (vs. a bare `u32`) so future fields
/// (e.g. an "`alembic_revision_at_cutover`" audit stamp) can land without
/// breaking the signature — matching the crate's struct-return idiom (see
/// [`crate::rows::ExchangeRow`] / [`crate::rows::PoolManagerRow`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustOwnedInfo {
    /// The value now stamped in [`SCHEMA_VERSION_TABLE`].
    pub schema_version: u32,
}

/// Inspect `conn`'s schema and bring a fresh-standalone DB up to the embedded
/// head, WITHOUT touching an Alembic-stamped DB.
///
/// # Writes
///
/// Writes ONLY on the [`SchemaState::FreshStandalone`] path (applies the DDL +
/// stamps [`SCHEMA_VERSION_TABLE`]). The caller is expected to have set
/// `PRAGMA query_only=on` only AFTER this returns on the fresh-standalone path
/// (see [`crate::connection::DegenbotDb::open`]); the other branches write
/// nothing and tolerate `query_only=on` being already set.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] on a query/DDL failure.
pub fn ensure_schema(conn: &Connection) -> Result<SchemaState, DbError> {
    let state = classify_schema(conn)?;
    if matches!(state, SchemaState::FreshStandalone { .. }) {
        apply_fresh_standalone(conn)?;
    }
    Ok(state)
}

/// The pure-predicate half of [`ensure_schema`]: inspect `conn`'s schema and
/// report the [`SchemaState`] WITHOUT applying any DDL. The
/// [`SchemaState::FreshStandalone`] arm here reports the *would-be* state
/// (an empty file) but does NOT create tables — so a read-only inspector (the
/// `database cutover --dry-run` path, [`crate::ops::inspect_schema_state`]) can
/// ask "what state is this DB in?" without the DDL side effect.
///
/// [`ensure_schema`] delegates here and applies DDL only when the classification
/// is `FreshStandalone`, preserving its byte-for-byte behavior.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] on a query failure (never refuses — returns the
/// state for ALL cases including `AlembicStale` / `Unrecognized`).
pub fn classify_schema(conn: &Connection) -> Result<SchemaState, DbError> {
    let has_alembic = table_exists(conn, "alembic_version")?;

    if has_alembic {
        let head: String = conn.query_row(
            &format!("SELECT version_num FROM {}", "alembic_version"),
            [],
            |row| row.get(0),
        )?;
        if head == ALEMBIC_HEAD {
            Ok(SchemaState::AlembicCurrent)
        } else {
            Ok(SchemaState::AlembicStale {
                head,
                expected: ALEMBIC_HEAD.to_string(),
            })
        }
    } else {
        // No Alembic history. Is the file empty (fresh standalone), a Rust-owned
        // DB (tables present + the private stamp table), or a foreign SQLite file?
        //
        // The private [`SCHEMA_VERSION_TABLE`] is filtered from the content-table
        // count (ADR-010 §2: "it is filtered, like `sqlite_%`") so a Rust-owned
        // DB whose only tables are the content tables + the stamp is not
        // miscounted.
        let table_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' \
             AND name NOT LIKE 'sqlite_%' AND name <> ?1",
            rusqlite::params![SCHEMA_VERSION_TABLE],
            |row| row.get(0),
        )?;
        if table_count > 0 {
            // Tables present, no Alembic. Is this a Rust-owned DB (post-cutover
            // or a re-opened FreshStandalone)? The stamp table is the tell.
            if table_exists(conn, SCHEMA_VERSION_TABLE)? {
                // Read the stamped version. A present stamp table with no row is
                // a corrupt cutover → refuse as Unrecognized.
                let row_exists: i64 = conn.query_row(
                    &format!("SELECT COUNT(*) FROM {SCHEMA_VERSION_TABLE}"),
                    [],
                    |row| row.get(0),
                )?;
                if row_exists == 0 {
                    return Ok(SchemaState::Unrecognized);
                }
                let v: i64 = conn.query_row(
                    &format!("SELECT schema_version FROM {SCHEMA_VERSION_TABLE}"),
                    [],
                    |row| row.get(0),
                )?;
                Ok(SchemaState::RustOwned {
                    schema_version: u32::try_from(v).unwrap_or(0),
                })
            } else {
                Ok(SchemaState::Unrecognized)
            }
        } else {
            // Empty file: the would-be state is FreshStandalone. classify_schema
            // does NOT apply the DDL here (the caller — ensure_schema or a
            // read-only inspector — decides whether to).
            Ok(SchemaState::FreshStandalone {
                schema_version: RUST_SCHEMA_VERSION,
            })
        }
    }
}

/// The opt-in cutover operation (ADR-010 §2): flip an `AlembicCurrent` DB
/// into Rust ownership. Verifies `alembic_version.version_num ==
/// ALEMBIC_HEAD`, drops the `alembic_version` table, and stamps
/// [`SCHEMA_VERSION_TABLE`] with [`RUST_SCHEMA_VERSION`]. Refuses
/// [`SchemaState::AlembicStale`] (upgrade via Alembic first) and
/// [`SchemaState::Unrecognized`] (foreign file).
///
/// This is the ONE operation that writes schema to an otherwise-Alembic DB,
/// and only because the DB is LEAVING Alembic ownership. Already-Rust-owned
/// (and a fresh standalone) DB is an idempotent re-stamp — `ensure_schema`
/// returns [`SchemaState::RustOwned`] / [`SchemaState::FreshStandalone`], and
/// we just re-stamp the version table (a `DELETE + INSERT` no-op).
///
/// # Errors
///
/// Returns [`DbError::AlembicStale`] for a stale Alembic DB,
/// [`DbError::UnrecognizedSchema`] for a foreign file, or [`DbError::Sqlite`]
/// on a query/DDL failure.
pub fn convert_alembic_to_rust_owned(conn: &Connection) -> Result<RustOwnedInfo, DbError> {
    let state = ensure_schema(conn)?;
    match state {
        SchemaState::AlembicCurrent => {
            // Drop the Alembic stamp table + stamp the Rust-owned version table.
            conn.execute_batch("DROP TABLE IF EXISTS alembic_version;")?;
            stamp_rust_schema_version(conn)?;
            Ok(RustOwnedInfo {
                schema_version: RUST_SCHEMA_VERSION,
            })
        }
        SchemaState::AlembicStale { head, expected } => {
            Err(DbError::AlembicStale { head, expected })
        }
        SchemaState::Unrecognized => Err(DbError::UnrecognizedSchema),
        // Idempotent re-stamp: the DB is already Rust-owned (or freshly
        // standalone) — reassert the stamp table (DELETE + INSERT no-op).
        SchemaState::RustOwned { .. } | SchemaState::FreshStandalone { .. } => {
            stamp_rust_schema_version(conn)?;
            Ok(RustOwnedInfo {
                schema_version: RUST_SCHEMA_VERSION,
            })
        }
    }
}

/// Returns `true` if a table named `name` exists in `conn`'s `sqlite_master`.
fn table_exists(conn: &Connection, name: &str) -> Result<bool, DbError> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        rusqlite::params![name],
        |row| row.get(0),
    )?;
    Ok(exists > 0)
}

/// Apply the embedded DDL and stamp the private Rust-owned schema-version
/// table (fresh-standalone path only).
fn apply_fresh_standalone(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch(SCHEMA_HEAD)?;
    stamp_rust_schema_version(conn)?;
    Ok(())
}

/// Create (if absent) and stamp the private Rust-owned schema-version table
/// with [`RUST_SCHEMA_VERSION`] (`CREATE TABLE IF NOT EXISTS` + `DELETE` +
/// `INSERT`). Idempotent — safe to re-assert on an already-stamped DB. Shared
/// by [`apply_fresh_standalone`] (fresh-standalone first open) and
/// [`convert_alembic_to_rust_owned`] (the cutover op).
fn stamp_rust_schema_version(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {SCHEMA_VERSION_TABLE} (schema_version INTEGER NOT NULL);\n\
         DELETE FROM {SCHEMA_VERSION_TABLE};"
    ))?;
    conn.execute(
        &format!("INSERT INTO {SCHEMA_VERSION_TABLE} (schema_version) VALUES (?1)"),
        rusqlite::params![i64::from(RUST_SCHEMA_VERSION)],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::table;

    #[test]
    fn fresh_in_memory_applies_ddl_and_stamps_version() {
        let conn = Connection::open_in_memory().unwrap();
        let state = ensure_schema(&conn).unwrap();
        assert_eq!(
            state,
            SchemaState::FreshStandalone {
                schema_version: RUST_SCHEMA_VERSION,
            }
        );
        // every core table is present
        for t in [
            table::EXCHANGES,
            table::ERC20_TOKENS,
            table::POOLS,
            table::LIQUIDITY_POSITIONS,
            table::INITIALIZATION_MAPS,
            table::POOL_MANAGERS,
            table::MANAGED_POOLS,
            table::UNISWAP_V4_POOLS,
            table::MANAGED_POOL_LIQUIDITY_POSITIONS,
            table::MANAGED_POOL_INITIALIZATION_MAPS,
        ] {
            assert!(
                table_exists(&conn, t).unwrap(),
                "{t} should exist after ensure_schema"
            );
        }
        // the private stamp table holds the version
        let v: i64 = conn
            .query_row(
                &format!("SELECT schema_version FROM {SCHEMA_VERSION_TABLE}"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v, i64::from(RUST_SCHEMA_VERSION));
    }

    #[test]
    fn alembic_current_writes_nothing() {
        let conn = Connection::open_in_memory().unwrap();
        // stamp an alembic_version row at the head
        conn.execute_batch(
            "CREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL);\n\
             INSERT INTO alembic_version (version_num) VALUES ('2606a6c7f5ee');",
        )
        .unwrap();
        let state = ensure_schema(&conn).unwrap();
        assert_eq!(state, SchemaState::AlembicCurrent);
        // no degenbot tables should have been created
        assert!(!table_exists(&conn, table::POOLS).unwrap());
        assert!(!table_exists(&conn, SCHEMA_VERSION_TABLE).unwrap());
    }

    #[test]
    fn alembic_stale_refuses() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL);\n\
             INSERT INTO alembic_version (version_num) VALUES ('deadbeefdead');",
        )
        .unwrap();
        let state = ensure_schema(&conn).unwrap();
        match state {
            SchemaState::AlembicStale { head, expected } => {
                assert_eq!(head, "deadbeefdead");
                assert_eq!(expected, ALEMBIC_HEAD);
            }
            other => panic!("expected AlembicStale, got {other:?}"),
        }
    }

    #[test]
    fn unrecognized_foreign_db_refuses() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE other_table (x INTEGER);")
            .unwrap();
        let state = ensure_schema(&conn).unwrap();
        assert_eq!(state, SchemaState::Unrecognized);
    }

    // ── RustOwned + the cutover (ADR-010 §2) ──────────────────────────────

    #[test]
    fn rustowned_on_reopen_after_fresh_standalone() {
        // apply_fresh_standalone applies DDL + stamps the version table.
        let conn = Connection::open_in_memory().unwrap();
        let first = ensure_schema(&conn).unwrap();
        assert_eq!(
            first,
            SchemaState::FreshStandalone {
                schema_version: RUST_SCHEMA_VERSION,
            }
        );

        // re-open the SAME conn — tables now present, no alembic, stamp table
        // stamped → RustOwned (the re-open bug the cutover closes).
        let second = ensure_schema(&conn).unwrap();
        assert_eq!(
            second,
            SchemaState::RustOwned {
                schema_version: RUST_SCHEMA_VERSION,
            }
        );
    }

    #[test]
    fn convert_alembic_current_to_rust_owned() {
        // Start from a Rust-owned DB (apply DDL + stamp), then flip it back to
        // an AlembicCurrent-shaped DB: drop the Rust stamp table, create +
        // stamp alembic_version at the head.
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap(); // FreshStandalone applies the full schema
        conn.execute_batch(&format!(
            "DROP TABLE {SCHEMA_VERSION_TABLE};\n\
             CREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL);\n\
             INSERT INTO alembic_version (version_num) VALUES ('{ALEMBIC_HEAD}');"
        ))
        .unwrap();
        assert_eq!(ensure_schema(&conn).unwrap(), SchemaState::AlembicCurrent);

        // Cutover.
        let info = convert_alembic_to_rust_owned(&conn).unwrap();
        assert_eq!(
            info,
            RustOwnedInfo {
                schema_version: RUST_SCHEMA_VERSION
            }
        );

        // alembic_version is GONE, the Rust stamp table is stamped.
        assert!(!table_exists(&conn, "alembic_version").unwrap());
        assert!(table_exists(&conn, SCHEMA_VERSION_TABLE).unwrap());
        let v: i64 = conn
            .query_row(
                &format!("SELECT schema_version FROM {SCHEMA_VERSION_TABLE}"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v, i64::from(RUST_SCHEMA_VERSION));

        // re-open → RustOwned.
        assert_eq!(
            ensure_schema(&conn).unwrap(),
            SchemaState::RustOwned {
                schema_version: RUST_SCHEMA_VERSION,
            }
        );
    }

    #[test]
    fn convert_refuses_alembic_stale() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL);\n\
             INSERT INTO alembic_version (version_num) VALUES ('deadbeefdead');",
        )
        .unwrap();
        match convert_alembic_to_rust_owned(&conn) {
            Err(DbError::AlembicStale { head, expected }) => {
                assert_eq!(head, "deadbeefdead");
                assert_eq!(expected, ALEMBIC_HEAD);
            }
            other => panic!("expected AlembicStale, got {other:?}"),
        }
    }

    #[test]
    fn convert_refuses_unrecognized() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE other_table (x INTEGER);")
            .unwrap();
        match convert_alembic_to_rust_owned(&conn) {
            Err(DbError::UnrecognizedSchema) => {}
            other => panic!("expected UnrecognizedSchema, got {other:?}"),
        }
    }

    #[test]
    fn convert_idempotent_on_rustowned() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap(); // FreshStandalone → tables + stamp
        assert!(matches!(
            ensure_schema(&conn).unwrap(),
            SchemaState::RustOwned { .. }
        ));

        // Convert an already-Rust-owned DB → succeeds, re-stamps, still RustOwned.
        let info = convert_alembic_to_rust_owned(&conn).unwrap();
        assert_eq!(
            info,
            RustOwnedInfo {
                schema_version: RUST_SCHEMA_VERSION
            }
        );
        assert_eq!(
            ensure_schema(&conn).unwrap(),
            SchemaState::RustOwned {
                schema_version: RUST_SCHEMA_VERSION,
            }
        );
    }

    #[test]
    fn classify_schema_on_empty_returns_fresh_standalone_without_creating_tables() {
        // classify_schema is the pure-predicate half of ensure_schema: it must
        // report the would-be state (FreshStandalone) WITHOUT applying DDL.
        let conn = Connection::open_in_memory().unwrap();
        let state = classify_schema(&conn).unwrap();
        assert_eq!(
            state,
            SchemaState::FreshStandalone {
                schema_version: RUST_SCHEMA_VERSION,
            }
        );
        // no tables were created — the file is still empty.
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "classify_schema must not apply DDL");
    }
}
