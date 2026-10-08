//! The concurrency PRAGMA prefix every open path asserts before the schema step
//! (binding #3), hardened against a concurrent fresh-open.
//!
//! The sequence is `busy_timeout=5000; synchronous=NORMAL; journal_mode=WAL;`.
//! WAL is file-persistent; `busy_timeout`/`synchronous` are per-connection.
//!
//! # Why `journal_mode=WAL` needs its own retry
//!
//! Converting a fresh, `journal_mode=DELETE` file to WAL takes a brief
//! EXCLUSIVE lock. SQLite does **not** invoke the connection's busy handler for
//! the journal-mode switch, so a second opener converting the *same* fresh file
//! at the same moment gets `SQLITE_BUSY`/`SQLITE_LOCKED` immediately — no
//! matter how large `busy_timeout` is. That is the one fresh-open step the
//! busy timeout cannot absorb, and it is why two processes racing
//! `db_upgrade_database` / `DegenbotDb::open` on a fresh path could still fail
//! even after the DDL became idempotent (`CREATE ... IF NOT EXISTS`): the loser
//! died in the PRAGMA prefix with `database is locked` before it ever reached
//! the schema step.
//!
//! The fix is a bounded retry on the WAL switch alone, under the workspace's
//! canonical `RetryPolicy`. It is safe because once ANY opener has switched
//! the file to WAL the pragma becomes a lock-free no-op that returns `"wal"`;
//! the retry only ever spins while a peer holds the conversion lock, and it is
//! capped so a genuinely stuck lock still surfaces as an error instead of
//! hanging the open forever.

use degenbot_core::retry::RetryPolicy;
use rusqlite::Connection;

use crate::error::DbError;

/// Bounded backoff for the WAL-activation retry: up to 12 attempts, base 5 ms
/// doubling to a 400 ms cap (~2.2 s total). Only ever consumed while a peer
/// holds the fresh-file WAL-conversion lock, so a generous budget costs nothing
/// on the uncontended steady-state reopen (attempt 1 succeeds immediately).
const WAL_RETRY: RetryPolicy = RetryPolicy {
    max_attempts: 12,
    base_delay: 0.005,
    max_delay: 0.4,
    jitter: 0.0,
};

/// Apply the concurrency PRAGMA prefix every open path asserts before the schema
/// step (binding #3): `busy_timeout` + `synchronous` (per-connection) then
/// `journal_mode=WAL` (file-persistent), with the WAL switch retried through
/// transient fresh-open contention (see the module docs).
///
/// # Errors
///
/// [`DbError::Sqlite`] on a non-contention PRAGMA failure, or when the WAL
/// switch stays contended past the retry budget.
pub(crate) fn apply_open_pragmas(conn: &Connection) -> Result<(), DbError> {
    // `busy_timeout` first: it covers every later statement on this connection
    // (the schema DDL included), and it is what makes the DDL batch itself
    // serialize cleanly between two fresh openers.
    conn.execute_batch("PRAGMA busy_timeout=5000;\nPRAGMA synchronous=NORMAL;")?;
    enable_wal(conn)
}

/// Switch this connection's file to WAL, retrying the switch through the
/// busy-handler-suppressed fresh-open contention (see the module docs).
fn enable_wal(conn: &Connection) -> Result<(), DbError> {
    let mut attempt: u32 = 1;
    loop {
        match conn.query_row("PRAGMA journal_mode=WAL;", [], |row| {
            row.get::<_, String>(0)
        }) {
            Ok(_) => return Ok(()),
            Err(error) if is_busy_locked(&error) && attempt < WAL_RETRY.max_attempts => {
                std::thread::sleep(WAL_RETRY.capped_backoff(attempt));
                attempt += 1;
            }
            Err(error) => return Err(DbError::Sqlite(error)),
        }
    }
}

/// Whether a `SQLite` failure is the busy/locked contention a retry may clear.
/// A busy handler reports an EXTENDED code; the primary code is the low byte.
fn is_busy_locked(error: &rusqlite::Error) -> bool {
    let rusqlite::Error::SqliteFailure(failure, _) = error else {
        return false;
    };
    let primary = failure.extended_code & 0xFF;
    primary == rusqlite::ffi::SQLITE_BUSY || primary == rusqlite::ffi::SQLITE_LOCKED
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests assert on known-valid inputs")]
mod tests {
    use super::*;

    #[test]
    fn apply_open_pragmas_sets_the_concurrency_trio_on_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pragmas.db");
        let conn = Connection::open(&path).unwrap();
        apply_open_pragmas(&conn).unwrap();

        let journal: String = conn
            .query_row("PRAGMA journal_mode;", [], |r| r.get(0))
            .unwrap();
        assert_eq!(journal.to_ascii_lowercase(), "wal");
        let busy: i64 = conn
            .query_row("PRAGMA busy_timeout;", [], |r| r.get(0))
            .unwrap();
        assert_eq!(busy, 5000);
        let sync: i64 = conn
            .query_row("PRAGMA synchronous;", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 1); // NORMAL == 1
    }

    #[test]
    fn apply_open_pragmas_is_idempotent_and_tolerant_of_memory() {
        // A second application on an already-WAL file is a no-op.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("twice.db");
        let conn = Connection::open(&path).unwrap();
        apply_open_pragmas(&conn).unwrap();
        apply_open_pragmas(&conn).unwrap();
        let journal: String = conn
            .query_row("PRAGMA journal_mode;", [], |r| r.get(0))
            .unwrap();
        assert_eq!(journal.to_ascii_lowercase(), "wal");

        // `:memory:` reports "memory" (SQLite has no WAL for it) and must not
        // error — the open path tolerates both.
        let mem = Connection::open_in_memory().unwrap();
        apply_open_pragmas(&mem).unwrap();
    }

    #[test]
    fn is_busy_locked_matches_busy_and_locked_only() {
        let busy = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(5), None);
        let locked = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(6), None);
        let constraint = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(19), None);
        assert!(is_busy_locked(&busy));
        assert!(is_busy_locked(&locked));
        assert!(!is_busy_locked(&constraint));
    }
}
