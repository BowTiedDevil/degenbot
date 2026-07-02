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
    /// `alembic_version` is absent but the file already holds tables — a foreign
    /// `SQLite` file passed by mistake. [`ensure_schema`] refuses; [`crate::connection::DegenbotDb::open`]
    /// maps this to [`crate::error::DbError::UnrecognizedSchema`].
    Unrecognized,
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
        // No Alembic history. Is the file empty (fresh standalone) or does it
        // already hold unrecognized tables (a foreign SQLite file)?
        let table_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )?;
        if table_count > 0 {
            Ok(SchemaState::Unrecognized)
        } else {
            apply_fresh_standalone(conn)?;
            Ok(SchemaState::FreshStandalone {
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
}
