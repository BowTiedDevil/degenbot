//! [`SnapshotDb`] — a held read-transaction handle that replaces [`SnapshotStore`]
//! (epic `XEANMB`).
//!
//! `SQLite` WAL is MVCC: a read transaction sees the database as of when its
//! first read started, and concurrent writer commits (the `pool_updater`
//! process, on its own connection) do not perturb the reader's view. So the
//! "freeze the DB cut at bot startup + hand it out across `build_paths`"
//! guarantee the in-memory [`SnapshotStore`] was hand-rolling is already a
//! primitive `SQLite` provides — the bot just has to read inside one
//! transaction.
//!
//! [`SnapshotDb`] wraps ONE `Mutex<Connection>` with `BEGIN` issued at
//! [`SnapshotDb::open`] + `COMMIT` at [`SnapshotDb::commit`] (or implicit
//! rollback on drop — harmless for a read-only tx). Every `fetch_*` call
//! re-locks the `Mutex` and runs its `SELECT` on the SAME connection —
//! re-entering the open transaction. No `rusqlite::Transaction<'_>` object is
//! stored (that would be a self-referential borrow against the
//! `MutexGuard`); the transaction state lives on the `Connection` in `SQLite`.
//!
//! Behavior test: `tests/wal_snapshot_isolation.rs` confirms a held deferred
//! read tx freezes the view across concurrent writer commits + releases on
//! commit.
//!
//! Owned (not borrowing): `PyBot` stores an `Arc<SnapshotDb>` alongside the
//! `Arc<Bot>` — no self-referential lifetime. `Send + Sync` via the `Mutex`.

use std::path::Path;

use parking_lot::Mutex;
use rusqlite::Connection;

use crate::error::DbError;
use crate::migrate::{ensure_schema, SchemaState};
use crate::read::{fetch_newest_update_block_on_conn, ExchangeFamily};
use crate::snapshot::{
    fetch_liquidity_map_on_conn, fetch_liquidity_map_v4_on_conn, LiquidityMap, TickMapDb,
};
use alloy::primitives::{Address, B256};

/// The per-connection PRAGMAs the open path always sets (mirrors
/// `DegenbotDb`): the three concurrency PRAGMAs that must run BEFORE
/// [`ensure_schema`] (WAL is file-persistent; `busy_timeout`/`synchronous`
/// are per-connection).
const PRE_SCHEMA_PRAGMAS: &str = "PRAGMA journal_mode=WAL;\n\
                                  PRAGMA busy_timeout=5000;\n\
                                  PRAGMA synchronous=NORMAL;";

/// A read-only DB handle with an open deferred read transaction held for the
/// lifetime of the handle. Every `fetch_*` call runs inside that one
/// transaction, so all reads share a single DB snapshot frozen at the first
/// read — immune to concurrent writer commits (epic `XEANMB`).
///
/// `query_only=on` is set after `ensure_schema` (mirrors `DegenbotDb::open`),
/// so the held tx stays a read tx — it can't accidentally upgrade and take a
/// write lock.
///
/// Drop semantics: dropping without [`Self::commit`] ends the transaction
/// (rollback — harmless for a read-only tx, releases the WAL snapshot). Call
/// [`Self::commit`] explicitly to release the snapshot early (e.g. at the end
/// of `build_paths`) so the WAL can checkpoint.
pub struct SnapshotDb {
    conn: Mutex<Connection>,
}

/// Report from [`SnapshotDb::close_with_canary`] — the operator-discipline
/// canary (epic `XEANMB` task 5.7). `advanced == true` means the DB advanced
/// between bot startup (the held-tx snapshot `s_snapshot`) and end-of-
/// `build_paths` (the fresh post-commit re-read `s_live`) — the
/// `pool_updater` committed concurrently with startup. Correctness was
/// already preserved by the held tx; the canary only surfaces the violation.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct CanaryReport {
    /// The snapshot seed block `S` captured inside the held tx (read at bot
    /// startup). `None` if no snapshot was loaded (cold-start).
    pub s_snapshot: Option<u64>,
    /// The live newest-update block re-read after `COMMIT` (the live DB
    /// state at end-of-`build_paths`).
    pub s_live: Option<u64>,
    /// `true` iff `s_live > s_snapshot` (the DB advanced during startup).
    pub advanced: bool,
}

impl SnapshotDb {
    /// Open a file-backed `SnapshotDb`: PRAGMAs + [`ensure_schema`] +
    /// `query_only=on` + `BEGIN` (deferred). The snapshot is established at
    /// the first `fetch_*` read inside.
    ///
    /// # Errors
    /// [`DbError::Sqlite`] on a connection/PRAGMA/DDL failure,
    /// [`DbError::AlembicStale`] / [`DbError::UnrecognizedSchema`] for stale
    /// or unrecognized files.
    pub fn open(path: &Path) -> Result<(Self, SchemaState), DbError> {
        let conn = if path == Path::new(":memory:") {
            Connection::open_in_memory()?
        } else {
            Connection::open(path)?
        };
        let state = Self::finish_open(&conn)?;
        Ok((
            Self {
                conn: Mutex::new(conn),
            },
            state,
        ))
    }

    /// Open an in-memory `SnapshotDb` (for tests). Same open sequence +
    /// `BEGIN` as [`Self::open`].
    ///
    /// # Errors
    /// [`DbError::Sqlite`] on a connection/PRAGMA/DDL failure.
    pub fn open_in_memory() -> Result<(Self, SchemaState), DbError> {
        let conn = Connection::open_in_memory()?;
        let state = Self::finish_open(&conn)?;
        Ok((
            Self {
                conn: Mutex::new(conn),
            },
            state,
        ))
    }

    /// PRAGMAs + `ensure_schema` + `query_only=on` + `BEGIN`. Mirrors
    /// `DegenbotDb::finish_open` then issues the held read tx.
    fn finish_open(conn: &Connection) -> Result<SchemaState, DbError> {
        conn.execute_batch(PRE_SCHEMA_PRAGMAS)?;
        let state = ensure_schema(conn)?;
        conn.pragma_update(None, "query_only", "on")?;
        // Begin the held deferred read tx. `query_only=on` blocks
        // INSERT/UPDATE/DELETE but NOT transaction control (BEGIN/COMMIT) —
        // verified by `tests/wal_snapshot_isolation.rs`. The snapshot is
        // established at the first SELECT inside (deferred behavior).
        conn.execute_batch("BEGIN;")?;
        match state {
            SchemaState::AlembicStale { head, expected } => {
                Err(DbError::AlembicStale { head, expected })
            }
            SchemaState::Unrecognized => Err(DbError::UnrecognizedSchema),
            other => Ok(other),
        }
    }

    /// Commit the held read transaction + consume the handle. Releases the
    /// WAL snapshot so the updater's checkpoint can reclaim `-wal` space.
    /// After this, the `SnapshotDb` is unusable (consumed).
    ///
    /// # Errors
    /// [`DbError::Sqlite`] if the `COMMIT` fails.
    pub fn commit(self) -> Result<(), DbError> {
        let conn = self.conn.into_inner();
        conn.execute_batch("COMMIT;")?;
        Ok(())
    }

    /// Commit the held read tx, then re-read `S_live =
    /// min(fetch_newest_update_block(V3), V4)` on the same connection (now
    /// in autocommit mode — the held snapshot was released by `COMMIT`, so
    /// the next `SELECT` sees the live DB). Returns a [`CanaryReport`]
    /// flagging whether the DB advanced during startup (`s_live >
    /// s_snapshot`) — the operator-discipline canary (epic `XEANMB` task
    /// 5.7).
    ///
    /// Correctness was already preserved by the held tx (every per-pool read
    /// during `build_paths` shared the frozen `s_snapshot` cut); the canary
    /// only surfaces that the `pool_updater` committed concurrently with
    /// startup — a discipline violation an operator may want to know about.
    /// The caller logs/acts on `report.advanced`.
    ///
    /// # Errors
    /// [`DbError::Sqlite`] if the `COMMIT` or the post-commit re-read fails.
    ///
    /// # Panics
    /// Panics if a `last_update_block` is negative — invalid DB state (`SQLite`
    /// stores block numbers as signed `i64`, but on-chain block numbers are
    /// non-negative). Mirrors `Bot::load_snapshot_from_db`'s contract.
    pub fn close_with_canary(
        self,
        s_snapshot: Option<u64>,
        chain: i64,
    ) -> Result<CanaryReport, DbError> {
        let conn = self.conn.into_inner();
        conn.execute_batch("COMMIT;")?;
        // Re-read in a fresh autocommit tx — snapshot released by COMMIT.
        let now_v3 = fetch_newest_update_block_on_conn(&conn, chain, ExchangeFamily::V3)?;
        let now_v4 = fetch_newest_update_block_on_conn(&conn, chain, ExchangeFamily::V4)?;
        let s_live = match (now_v3, now_v4) {
            (Some(v3), Some(v4)) => {
                Some(u64::try_from(v3.min(v4)).expect("block number non-negative"))
            }
            (Some(v3), None) => Some(u64::try_from(v3).expect("block number non-negative")),
            (None, Some(v4)) => Some(u64::try_from(v4).expect("block number non-negative")),
            (None, None) => None,
        };
        let advanced = match (s_snapshot, s_live) {
            (Some(snap), Some(live)) => live > snap,
            _ => false,
        };
        Ok(CanaryReport {
            s_snapshot,
            s_live,
            advanced,
        })
    }

    /// Lock the underlying connection (for the `_on_conn` free functions).
    fn lock(&self) -> parking_lot::MutexGuard<'_, Connection> {
        self.conn.lock()
    }
}

impl TickMapDb for SnapshotDb {
    fn fetch_liquidity_map(&self, pool_address: Address) -> Result<Option<LiquidityMap>, DbError> {
        let conn = self.lock();
        fetch_liquidity_map_on_conn(&conn, pool_address)
    }
    fn fetch_liquidity_map_v4(
        &self,
        pool_manager: Address,
        pool_id_hash: B256,
    ) -> Result<Option<LiquidityMap>, DbError> {
        let conn = self.lock();
        fetch_liquidity_map_v4_on_conn(&conn, pool_manager, pool_id_hash)
    }
    fn fetch_newest_update_block(
        &self,
        chain: i64,
        family: ExchangeFamily,
    ) -> Result<Option<i64>, DbError> {
        let conn = self.lock();
        fetch_newest_update_block_on_conn(&conn, chain, family)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A held `SnapshotDb` sees a frozen DB view: a write to the underlying
    /// file (via a separate writer connection) after the first read does not
    /// perturb `fetch_*` results read through the held tx. This is the
    /// behavior `tests/wal_snapshot_isolation.rs` confirms at the raw
    /// `rusqlite` level; here it's confirmed through the `SnapshotDb` API
    /// (the surface `assemble_*_tick_map`'s Db arm reads through).
    #[test]
    fn snapshot_db_freezes_view_across_concurrent_writer() {
        let dir = tempfile::TempDir::new().expect("tmpdir");
        let db_path: PathBuf = dir.path().join("snap.sqlite");

        // Seed via a write-capable connection (mirrors the updater writing
        // rows before the bot starts). `open_for_writes` runs `ensure_schema`
        // (creates the tables); inserts use `foreign_keys=OFF` to avoid the
        // token FK (the seed is a synthetic fixture, not a real pool).
        {
            let (w, _) = crate::connection::DegenbotDb::open_for_writes(&db_path).unwrap();
            let conn = w.lock();
            conn.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
            conn.execute_batch(
                "INSERT OR IGNORE INTO erc20_tokens (id, chain, address, name, symbol, decimals) \
                 VALUES (0, 1, '0x0', 'T', 'T', 18);\
                 INSERT OR IGNORE INTO exchanges (id, chain_id, name, active, last_update_block, factory) \
                 VALUES (1, 1, 'uniswap_v3', 1, 100, '0x1F98431c8aD98523631AE4a59f267346ea31F984');\
                 INSERT OR IGNORE INTO pools (id, address, chain, kind, token0_id, token1_id, exchange_id) \
                 VALUES (10, '0xPool', 1, 'uniswap_v3', 0, 0, 1);\
                 INSERT OR IGNORE INTO liquidity_positions (id, pool_id, tick, liquidity_net, liquidity_gross) \
                 VALUES (1, 10, -100, '1000', '1000');",
            )
            .unwrap();
        }

        // Open the SnapshotDb (held tx begins).
        let (snap, _) = SnapshotDb::open(&db_path).expect("open snapshot db");

        // First read establishes the snapshot at block 100.
        let s1 = snap
            .fetch_newest_update_block(1, ExchangeFamily::V3)
            .unwrap();
        assert_eq!(s1, Some(100));

        // Concurrent writer advances the exchange to block 110.
        {
            let w = Connection::open(&db_path).unwrap();
            w.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
                .unwrap();
            w.execute(
                "UPDATE exchanges SET last_update_block = 110 WHERE id = 1",
                [],
            )
            .unwrap();
        }

        // The held tx STILL sees block 100 — the writer's commit is invisible.
        let s2 = snap
            .fetch_newest_update_block(1, ExchangeFamily::V3)
            .unwrap();
        assert_eq!(s2, Some(100), "held SnapshotDb tx freezes the view");

        // Commit releases the snapshot.
        snap.commit().unwrap();

        // After commit, a fresh SnapshotDb sees the writer's 110.
        let (snap2, _) = SnapshotDb::open(&db_path).unwrap();
        let s3 = snap2
            .fetch_newest_update_block(1, ExchangeFamily::V3)
            .unwrap();
        assert_eq!(s3, Some(110), "fresh tx after commit sees the live DB");
    }
}
