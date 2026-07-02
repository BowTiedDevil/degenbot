//! Error type for the degenbot-db substrate.

use thiserror::Error;

/// Errors returned by the degenbot-db read substrate.
#[derive(Debug, Error)]
pub enum DbError {
    /// A [`rusqlite::Error`] surfaced from the `SQLite` layer.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A typed-row decode failure (e.g. a malformed `VARCHAR(78)` big-int, an
    /// address that fails EIP-55 checksum, an unrecognized `kind` discriminator).
    #[error("decode error: {0}")]
    Decode(String),

    /// A single-row lookup that expected exactly one row found none.
    #[error("required row not found: {0}")]
    MissingRow(String),

    /// The `alembic_version` row does not match [`crate::schema::ALEMBIC_HEAD`].
    ///
    /// The Rust core is a **reader** of Alembic-headed DBs, never a migrator;
    /// the operator must run the Python Alembic upgrade (`alembic upgrade head`)
    /// — the writer/orchestration migration in Epic AZGJUN owns Alembic stamps.
    #[error("alembic schema is stale: head={head:?} expected={expected:?}; run the Python Alembic upgrade (`alembic upgrade head`)")]
    AlembicStale {
        /// The `version_num` actually stamped in the DB.
        head: String,
        /// The constant the Rust core expects ([`crate::schema::ALEMBIC_HEAD`]).
        expected: String,
    },

    /// The opened file is neither an Alembic-stamped degenbot DB nor an empty
    /// fresh-standalone file — likely a foreign `SQLite` file passed by mistake.
    #[error(
        "unrecognized database schema (not a degenbot Alembic DB and not a fresh standalone DB)"
    )]
    UnrecognizedSchema,

    /// `PRAGMA integrity_check` returned a value other than `"ok"` — mirrors the
    /// Python `backup_sqlite_database` post-backup assertion. The inner string is
    /// the verbatim `SQLite` message.
    #[error("sqlite integrity check failed: {0}")]
    IntegrityCheckFailed(String),
}

/// Convert a [`DbError`] into a [`rusqlite::Error`] so row-decode closures
/// (which must return `Result<_, rusqlite::Error>` for `query_map`) can reuse
/// the typed `from_row` decoders. The round-trip back through
/// [`DbError::Sqlite`][crate::error::DbError::Sqlite] (on `collect`) preserves
/// the original cause text.
impl From<DbError> for rusqlite::Error {
    fn from(e: DbError) -> Self {
        // `FromSqlConversionFailure` semantically fits: the typed row decoder
        // rejected a column value (bad U256 string, bad address checksum, …).
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    }
}
