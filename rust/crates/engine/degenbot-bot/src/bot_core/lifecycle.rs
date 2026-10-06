//! The `Bot` snapshot + pump-lifecycle + block-stream cluster (ADR-006 D4).
//!
//! The retained-tx snapshot open/teardown pair
//! ([`Bot::load_snapshot_from_db_path`] + [`Bot::close_snapshot_tx`]), the
//! driver subscribe/resume/stop wrappers, the verification-config setters,
//! and the once-only block-stream hand-off. The PyO3 shell keeps only arg
//! parsing + `PyErr` mapping for this surface; the typed errors are
//! re-exported from `bot.rs` so the shell's import paths hold.

use std::sync::Arc;

use crate::arb_engine::{DriverError, EngineDriver};
use crate::bot_core::bot::Bot;
use crate::bot_core::snapshot_verify::SnapshotLoadError;

/// Error from [`Bot::load_snapshot_from_db_path`]: the DB-open failure
/// (`Open`) is distinguishable from the snapshot-load failure (`Load`) so the
/// `PyO3` shell can preserve its two historical error surfaces byte-identically
/// (the DB `ValueError` convention and the `load_snapshot_from_db failed:`
/// `RuntimeError`).
#[derive(Debug)]
pub enum SnapshotOpenError {
    /// `SnapshotDb::open` failed (connection/PRAGMA/schema).
    Open(degenbot_db::DbError),
    /// The seed-block read inside the held tx failed.
    Load(SnapshotLoadError),
}

/// Error from [`Bot::close_snapshot_tx`].
#[derive(Debug)]
pub enum CloseSnapshotTxError {
    /// `Arc::try_unwrap` failed — a caller still holds a `SnapshotDb` clone
    /// (the shell's "clone leak" `RuntimeError`).
    ArcHeld,
    /// The `COMMIT` or the post-commit canary re-read failed.
    Close(degenbot_db::DbError),
}

/// Error from [`Bot::block_stream`].
#[derive(Debug)]
pub enum BlockStreamError {
    /// No engine was constructed against the bot (the shell's "no pump
    /// state" `RuntimeError`).
    NoPumpState,
    /// The receiver was already handed out (the shell's "can only be called
    /// once" `RuntimeError`).
    AlreadyTaken,
}

impl Bot {
    /// The snapshot seed block `S` (`None` = the cold-start path). The read
    /// the `PyO3` `snapshot_seed_block` getter routes through.
    #[must_use]
    pub fn snapshot_seed_block(&self) -> Option<u64> {
        self.state_arc()
            .read_at(degenbot_substrate::state_lock::LockSite::Orchestrator)
            .snapshot_seed_block()
    }

    /// Open the retained snapshot DB + load it in one call — the `PyO3` shell's
    /// `load_snapshot_from_db` path. Opens a read-only
    /// [`degenbot_db::snapshot_db::SnapshotDb`] handle from `db_path` (one
    /// `Mutex<Connection>` + a held deferred read transaction), runs
    /// [`Bot::load_snapshot_from_db`] INSIDE the held tx so `S` and every
    /// per-pool `fetch_liquidity_map` read share one frozen DB snapshot
    /// across `build_paths` (the consistency replacement for the retired
    /// `SnapshotStore`), then hands the STILL-OPEN handle back for the caller
    /// to retain. The `PyO3` shell keeps it in its D4 `db` slot and commits it
    /// via [`Bot::close_snapshot_tx`] at end of `build_paths` — that
    /// retained-tx pairing is load-bearing (WAL MVCC).
    ///
    /// # Errors
    /// [`SnapshotOpenError::Open`] on a DB open failure,
    /// [`SnapshotOpenError::Load`] on a DB read failure or a liquidity value
    /// out of range.
    pub fn load_snapshot_from_db_path(
        &self,
        db_path: &str,
        chain_id: u64,
    ) -> Result<degenbot_db::snapshot_db::SnapshotDb, SnapshotOpenError> {
        let (snap, _schema) =
            degenbot_db::snapshot_db::SnapshotDb::open(&std::path::PathBuf::from(db_path))
                .map_err(SnapshotOpenError::Open)?;
        self.load_snapshot_from_db(&snap, chain_id)
            .map_err(SnapshotOpenError::Load)?;
        Ok(snap)
    }

    /// Commit + drop the held snapshot read transaction — the teardown half
    /// of [`Bot::load_snapshot_from_db_path`], called at end of `build_paths`
    /// so the WAL snapshot is released and the updater's checkpoint can
    /// reclaim `-wal` space. `db` is the retained handle, taken out of the
    /// caller's D4 `db` slot: the sole `Arc` is unwrapped, `S_snapshot` is
    /// captured from the state, and `close_with_canary` commits + re-reads
    /// `S_live` on the same connection (the operator-discipline canary). A
    /// `None` handle (not-loaded / already-closed bot) is an idempotent
    /// no-op returning `Ok(None)`.
    ///
    /// # Errors
    /// [`CloseSnapshotTxError::ArcHeld`] when clones of the handle remain (a
    /// caller didn't drop its handle — surfaced rather than silently leaking
    /// the tx), [`CloseSnapshotTxError::Close`] when the `COMMIT` or the
    /// canary re-read fails.
    pub fn close_snapshot_tx(
        &self,
        db: Option<Arc<degenbot_db::snapshot_db::SnapshotDb>>,
    ) -> Result<Option<degenbot_db::snapshot_db::CanaryReport>, CloseSnapshotTxError> {
        let Some(snap) = db else {
            return Ok(None);
        };
        let s_snapshot = self.snapshot_seed_block();
        let chain = i64::try_from(self.chain_id()).unwrap_or(0);
        // `Arc::try_unwrap` succeeds only if the `assemble_*` calls released
        // their clones. During `build_paths` the Db arm clones per-call +
        // drops before the teardown runs, so at this point the only remaining
        // `Arc` is the caller's.
        match Arc::try_unwrap(snap) {
            Ok(snap) => snap
                .close_with_canary(s_snapshot, chain)
                .map(Some)
                .map_err(CloseSnapshotTxError::Close),
            Err(_) => Err(CloseSnapshotTxError::ArcHeld),
        }
    }

    /// Subscribe to the WS `newHeads` + logs streams — the `block_on` wrapper
    /// over [`EngineDriver::subscribe`] the `PyO3` shells (`PyBot`,
    /// `PyArbEngine`) drive detached from the GIL. Blocks (sync, via the
    /// shared tokio runtime) until the first block is observed, then returns
    /// the first WS block number (the backfill target).
    ///
    /// # Errors
    /// [`DriverError`] if the pump is already started/subscribed, the phase
    /// is wrong, or the WS subscribe fails.
    pub fn subscribe(driver: &EngineDriver, rpc_url: &str) -> Result<u64, DriverError> {
        degenbot_core::runtime::get_runtime().block_on(driver.subscribe(rpc_url))
    }

    /// Resume the pump — begin normal WS processing, including the driver's
    /// synchronous `S+1..W` auto-backfill. The `block_on` wrapper over
    /// [`EngineDriver::resume`].
    ///
    /// # Errors
    /// [`DriverError`] if the phase is wrong, subscribe wasn't called, the
    /// driver is stopped, or it was already resumed.
    pub fn resume(driver: &EngineDriver) -> Result<(), DriverError> {
        degenbot_core::runtime::get_runtime().block_on(driver.resume())
    }

    /// Stop the pump — the driver's any-phase, idempotent stop.
    ///
    /// # Errors
    /// Currently always `Ok`; the typed result keeps the surface symmetric.
    pub fn stop(driver: &EngineDriver) -> Result<(), DriverError> {
        driver.stop()
    }

    /// Set the HTTP RPC URL used for verification. `None` (no engine
    /// constructed against the bot) is a no-op — the guard the `PyO3` shell
    /// held as a swallowed `pump_state` error.
    pub fn set_verify_rpc_url(driver: Option<&EngineDriver>, rpc_url: &str) {
        if let Some(pump) = driver {
            pump.set_verify_rpc_url(rpc_url);
        }
    }

    /// Set the `StateView` contract address for V4 verification. `None` (no
    /// engine constructed against the bot) is a no-op.
    pub fn set_verify_state_view(driver: Option<&EngineDriver>, state_view_address: &str) {
        if let Some(pump) = driver {
            pump.set_verify_state_view(state_view_address);
        }
    }

    /// Take the block-clock receiver for the block stream — the once-only
    /// hand-off from the attached engine's block source channel. `None`
    /// (no engine constructed against the bot) is
    /// [`BlockStreamError::NoPumpState`]; a second take is
    /// [`BlockStreamError::AlreadyTaken`] (the receiver is moved into the
    /// iterator at the shell).
    ///
    /// # Errors
    /// [`BlockStreamError`] as above.
    pub fn block_stream(
        driver: Option<&EngineDriver>,
    ) -> Result<
        degenbot_eventhub::NamedReceiver<crate::bot_core::BlockNotification>,
        BlockStreamError,
    > {
        let pump = driver.ok_or(BlockStreamError::NoPumpState)?;
        pump.take_block_receiver()
            .ok_or(BlockStreamError::AlreadyTaken)
    }
}

#[expect(clippy::expect_used, clippy::print_stderr)]
#[cfg(test)]
mod tests {
    use crate::bot_core::bot::Bot;

    // The PyO3 shell keeps only arg parsing + error mapping for this surface;
    // the retained-tx snapshot open/teardown, the once-only block-stream
    // hand-off, and the driver lifecycle wrappers live on `Bot`.

    /// The parity fixture DB (sibling crate; skipped when absent, like
    /// `snapshot_load_tests`).
    fn parity_fixture() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../foundation/degenbot-db/tests/fixtures/parity.db")
    }

    /// `load_snapshot_from_db_path` opens the retained `SnapshotDb` (one
    /// `Mutex<Connection>` + open deferred read tx), reads `S` INSIDE the held
    /// tx, and hands the still-open handle back — the per-pool `build_paths`
    /// reads must share the same frozen WAL snapshot, so the returned handle
    /// is the one the shell retains.
    #[test]
    fn load_snapshot_from_db_path_seeds_block_and_returns_the_held_handle() {
        use degenbot_db::read::ExchangeFamily;
        use degenbot_db::snapshot::TickMapDb;

        let fixture = parity_fixture();
        if !fixture.exists() {
            eprintln!("skipping: parity fixture not at {}", fixture.display());
            return;
        }
        // Pin the ADR-052 D1 heal-at-open killswitch so this read never
        // rewrites the committed fixture.
        std::env::set_var(degenbot_db::AUTO_HEAL_ENV, "0");
        let bot = Bot::new(8453);
        let snap = bot
            .load_snapshot_from_db_path(fixture.to_str().expect("test fixture: utf8 path"), 8453)
            .expect("snapshot load");

        let state_arc = bot.state_arc();
        let core = state_arc.read_at(degenbot_substrate::state_lock::LockSite::Core);
        let v3 = snap
            .fetch_newest_update_block(8453, ExchangeFamily::V3)
            .expect("held tx readable");
        let v4 = snap
            .fetch_newest_update_block(8453, ExchangeFamily::V4)
            .expect("held tx readable");
        let expected_s = match (v3, v4) {
            (Some(a), Some(b)) => Some(u64::try_from(a.min(b)).expect("block number non-negative")),
            (Some(a), None) => Some(u64::try_from(a).expect("block number non-negative")),
            (None, Some(b)) => Some(u64::try_from(b).expect("block number non-negative")),
            (None, None) => None,
        };
        assert_eq!(
            core.snapshot_seed_block(),
            expected_s,
            "S must be min(newest_update_block(V3), V4) read inside the held tx"
        );
    }

    /// A nonexistent DB path is the typed `Open` arm (the shell maps it to its
    /// DB `ValueError` convention) — distinguishable from a load failure.
    #[test]
    fn load_snapshot_from_db_path_refuses_a_bad_path_as_open() {
        let bot = Bot::new(1);
        // `SnapshotDb` is not `Debug`, so the Ok arm is matched away rather
        // than `unwrap_err`ed.
        assert!(
            matches!(
                bot.load_snapshot_from_db_path("/nonexistent/degenbot-snapshot.db", 1),
                Err(super::SnapshotOpenError::Open(_))
            ),
            "a nonexistent DB path must be refused"
        );
    }

    /// The teardown pair: `close_snapshot_tx` commits the held read tx (the
    /// handle is consumed, the WAL snapshot released) and reports the canary;
    /// with no handle it is an idempotent no-op (`None`).
    #[test]
    fn close_snapshot_tx_commits_then_is_an_idempotent_noop() {
        let (snap, _schema) = degenbot_db::snapshot_db::SnapshotDb::open_in_memory()
            .expect("test setup: in-memory snapshot db");
        let bot = Bot::new(1);
        bot.load_snapshot_from_db(&snap, 1)
            .expect("test setup: cold-start load");

        let report = bot
            .close_snapshot_tx(Some(std::sync::Arc::new(snap)))
            .expect("the sole Arc unwraps")
            .expect("a held handle closes with a canary report");
        assert!(!report.advanced, "no concurrent writer in the test");
        assert_eq!(report.s_snapshot, None, "cold start: S = None");

        // After the handle is gone the teardown is a no-op (idempotent).
        assert!(
            bot.close_snapshot_tx(None)
                .expect("no handle: Ok")
                .is_none(),
            "a not-loaded bot closes idempotently"
        );
    }

    /// A leaked `SnapshotDb` clone (a caller kept a handle past `build_paths`)
    /// is the typed `ArcHeld` refusal — the shell maps it to the historical
    /// "clone leak" `RuntimeError` rather than silently rolling the tx back.
    #[test]
    fn close_snapshot_tx_refuses_a_leaked_clone() {
        let (snap, _schema) = degenbot_db::snapshot_db::SnapshotDb::open_in_memory()
            .expect("test setup: in-memory snapshot db");
        let bot = Bot::new(1);
        let arc = std::sync::Arc::new(snap);
        let leaked = std::sync::Arc::clone(&arc);
        assert!(matches!(
            bot.close_snapshot_tx(Some(arc)),
            Err(super::CloseSnapshotTxError::ArcHeld)
        ));
        drop(leaked); // keep leak trackers quiet
    }

    /// The block-clock receiver is handed out exactly once: a missing driver
    /// is the typed `NoPumpState` (the shell's "no pump state" `RuntimeError`)
    /// and a second take is `AlreadyTaken` ("can only be called once").
    #[test]
    fn block_stream_hands_the_receiver_out_exactly_once() {
        let bot = std::sync::Arc::new(Bot::new(8453));
        let driver =
            crate::arb_engine::EngineDriver::new(bot, degenbot_config::holder::config_arc());

        assert!(matches!(
            Bot::block_stream(None),
            Err(super::BlockStreamError::NoPumpState)
        ));

        let rx = Bot::block_stream(Some(&driver)).expect("first hand-off");
        drop(rx);
        assert!(matches!(
            Bot::block_stream(Some(&driver)),
            Err(super::BlockStreamError::AlreadyTaken)
        ));
    }

    /// The verify-config setters no-op without an attached engine (the shell's
    /// swallowed `pump_state` error), and accept one without side effects the
    /// shell could observe.
    #[test]
    fn set_verify_config_noops_without_an_attached_driver() {
        Bot::set_verify_rpc_url(None, "http://127.0.0.1:1");
        Bot::set_verify_state_view(None, "0x0000000000000000000000000000000000000001");

        let bot = std::sync::Arc::new(Bot::new(8453));
        let driver =
            crate::arb_engine::EngineDriver::new(bot, degenbot_config::holder::config_arc());
        Bot::set_verify_rpc_url(Some(&driver), "http://127.0.0.1:1");
        Bot::set_verify_state_view(Some(&driver), "0x0000000000000000000000000000000000000001");
    }

    /// `Bot::stop` drives the driver's any-phase idempotent stop (the shell
    /// keeps only the error map).
    #[test]
    fn stop_drives_the_driver_stop() {
        let bot = std::sync::Arc::new(Bot::new(8453));
        let driver =
            crate::arb_engine::EngineDriver::new(bot, degenbot_config::holder::config_arc());
        Bot::stop(&driver).expect("any-phase stop is Ok");
        Bot::stop(&driver).expect("stop is idempotent");
    }
}
