//! The `Bot` snapshot + pump-lifecycle + block-stream cluster (ADR-006 D4).
//!
//! The retained-tx snapshot open/teardown pair
//! ([`Bot::load_snapshot_from_db_path`] + [`RetainedSnapshotTx`]), the driver
//! subscribe/resume/stop wrappers, the verification-config setters, and the
//! once-only block-stream hand-off. The PyO3 shell keeps only arg
//! parsing + `PyErr` mapping for this surface; the typed errors are
//! re-exported from `bot.rs` so the shell's import paths hold.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use degenbot_core::{op_info, op_warn};

use crate::arb_engine::{
    ConnectRefusal, Connected, ConsumerClaim, DriverError, EngineDriver, TeardownReport,
};
use crate::bot_core::bot::Bot;
use crate::bot_core::registration_ledger::RefusalKind;

/// Error from [`Bot::load_snapshot_from_db_path`]: the DB-open failure
/// (`Open`) is distinguishable from the snapshot-load failure (`Load`) so the
/// `PyO3` shell can preserve its two historical error surfaces byte-identically
/// (the DB `ValueError` convention and the `load_snapshot_from_db failed:`
/// `RuntimeError`).
#[derive(Debug)]
pub enum SnapshotOpenError {
    /// `SnapshotDb::open` failed (connection/PRAGMA/schema).
    Open(degenbot_db::DbError),
    /// The seed-block read inside the held tx failed — the one
    /// [`RefusalKind::SnapshotLoadFailed`] refusal.
    Load(RefusalKind),
}

/// Error from [`RetainedSnapshotTx::close`].
#[derive(Debug)]
pub enum CloseSnapshotTxError {
    /// The quiescence wait expired with tick-map assemblers still holding
    /// handles, so the sole-holder canary could only false-positive: the tx is
    /// left uncommitted rather than committing under a live reader.
    AssemblersHeld,
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

    /// Arm the retained read-tx guard over a freshly-opened handle.
    #[must_use]
    pub fn retain_snapshot_tx(
        &self,
        db: degenbot_db::snapshot_db::SnapshotDb,
    ) -> RetainedSnapshotTx {
        RetainedSnapshotTx::new(
            db,
            self.snapshot_seed_block(),
            i64::try_from(self.chain_id()).unwrap_or(0),
        )
    }

    /// Connect the session — the `block_on` wrapper over
    /// [`EngineDriver::connect`] the `PyO3` shells (`PyBot`, `PyArbEngine`)
    /// drive detached from the GIL. Blocks (sync, via the shared tokio runtime)
    /// until the first complete block is observed, then reports it as the link
    /// block (the backfill target).
    ///
    /// # Errors
    /// [`ConnectRefusal`] when the pose does not admit the move (including a
    /// concurrent one in flight) or the WS transport refuses the handshake.
    pub fn connect(driver: &EngineDriver, rpc_url: &str) -> Result<Connected, ConnectRefusal> {
        degenbot_core::runtime::get_runtime().block_on(driver.connect(rpc_url))
    }

    /// Arm the live loop — begin normal WS processing, including the driver's
    /// synchronous `S+1..W` auto-backfill. The `block_on` wrapper over
    /// [`EngineDriver::run`], which CONSUMES the session's consumer claim: the
    /// caller must already hold the hand-off's stream, so "a consumer is
    /// attached before production starts" is a type fact.
    ///
    /// # Errors
    /// [`DriverError`] when the pose does not admit the move, or the backfill
    /// fails (the gap is then unapplied).
    pub fn run(driver: &EngineDriver, claim: ConsumerClaim) -> Result<(), DriverError> {
        degenbot_core::runtime::get_runtime().block_on(driver.run(claim))
    }

    /// Close the session — the driver's total, asynchronous, idempotent
    /// teardown, blocked at this adapter boundary (never inside the core
    /// runtime).
    pub fn close(driver: &EngineDriver) -> TeardownReport {
        degenbot_core::runtime::get_runtime().block_on(driver.close())
    }

    /// Install the verify home's RPC provider from `rpc_url`. `None` (no engine
    /// constructed against the bot) is a no-op — the guard the `PyO3` shell
    /// held as a swallowed `pump_state` error.
    ///
    /// # Panics
    ///
    /// Panics if called from within a Tokio runtime (it parks on the shared
    /// runtime via `block_on`).
    pub fn set_verify_rpc_url(driver: Option<&EngineDriver>, rpc_url: &str) {
        if let Some(pump) = driver {
            let verify = pump.verify();
            let rpc_url = rpc_url.to_owned();
            let verify = Arc::clone(verify);
            degenbot_core::runtime::get_runtime().block_on(async move {
                let provider =
                    crate::bot_core::verify_lifecycle::build_verify_provider(&rpc_url).await;
                let mut config = verify.config();
                config.provider = provider;
                verify.configure(config);
            });
        }
    }

    /// Set the `StateView` contract address for V4 verification. `None` (no
    /// engine constructed against the bot) is a no-op.
    pub fn set_verify_state_view(driver: Option<&EngineDriver>, state_view_address: &str) {
        if let Some(pump) = driver {
            let verify = pump.verify();
            let mut config = verify.config();
            config.state_view = Some(
                state_view_address
                    .parse()
                    .unwrap_or(alloy::primitives::Address::ZERO),
            );
            verify.configure(config);
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

/// How long [`RetainedSnapshotTx`] waits for the tick-map assemblers still
/// holding a handle before it refuses the canary.
///
/// The wait is a drain, not a deadline in the normal case: an assembler holds
/// its handle for one `fetch_liquidity_map` read. The budget exists so a
/// genuinely stuck worker becomes a loud refusal instead of a wedged teardown.
const QUIESCENCE_BUDGET: Duration = Duration::from_secs(30);

/// The first probe interval of the quiescence wait.
const QUIESCENCE_PROBE_START: Duration = Duration::from_millis(1);

/// The largest probe interval of the quiescence wait.
const QUIESCENCE_PROBE_MAX: Duration = Duration::from_millis(25);

/// The retained read transaction over a snapshot DB: the sole owner of the
/// handle, and of the quiescence its canary depends on.
///
/// `SnapshotDb` carries a held deferred read transaction, so the snapshot load
/// and every per-pool tick-map read during registration share one frozen WAL
/// view. Releasing it (COMMIT + the operator-discipline canary) is only lawful
/// once nothing else reads through the handle — `Arc::try_unwrap` is exactly
/// that question, and answering it by *guessing* is what makes a canary
/// false-positive: an `assemble_*_tick_map` call that still holds a clone makes
/// the unwrap fail and masks the real teardown reason with a clone-leak error.
///
/// So the guard owns the invariant instead of documenting it:
///
/// - [`Self::acquire`] is the ONE way to clone the handle — every assembler
///   call goes through it, and the clone it answers is tracked by the handle's
///   own strong count;
/// - [`Self::close`] (and [`Drop`]) first empties the slot, which makes
///   [`Self::acquire`] answer `None` forever, so the holder count can only
///   fall from that moment on — no new holder can appear mid-drain;
/// - the drain then waits for the count to reach one (the guard's own handle)
///   before it unwraps and commits. A holder that never releases is
///   [`CloseSnapshotTxError::AssemblersHeld`], never a false-positive canary.
///
/// Misuse is therefore not expressible: there is no argument a caller can pass
/// to skip the drain, and no second way to reach the handle.
pub struct RetainedSnapshotTx {
    /// `Some` until the tx is closed. Interior mutability because the guard is
    /// held behind an `Arc` (the shell's `db` slot) and both paths — explicit
    /// close and `Drop` — must consume the same one handle.
    db: Mutex<Option<Arc<degenbot_db::snapshot_db::SnapshotDb>>>,
    /// `S` captured inside the held tx, the canary's basis.
    s_snapshot: Option<u64>,
    /// The chain the canary re-reads.
    chain_id: i64,
}

impl RetainedSnapshotTx {
    /// Wrap an opened handle. `s_snapshot` is the seed block read inside the
    /// held transaction, which is what the canary compares against.
    #[must_use]
    pub fn new(
        db: degenbot_db::snapshot_db::SnapshotDb,
        s_snapshot: Option<u64>,
        chain_id: i64,
    ) -> Self {
        Self {
            db: Mutex::new(Some(Arc::new(db))),
            s_snapshot,
            chain_id,
        }
    }

    /// A handle for one tick-map assembly call, or `None` once the tx is
    /// closing (the caller then takes the no-DB arm: chain reads).
    ///
    /// The answered clone is a tracked holder: it keeps the commit from
    /// running until it drops.
    #[must_use]
    pub fn acquire(&self) -> Option<Arc<degenbot_db::snapshot_db::SnapshotDb>> {
        self.db.lock().as_ref().map(Arc::clone)
    }

    /// Commit the held read transaction and report the canary.
    ///
    /// Returns `Ok(None)` when there is nothing to close (never armed, or
    /// already closed) — the idempotent no-op a second teardown call gets.
    ///
    /// # Errors
    /// [`CloseSnapshotTxError::AssemblersHeld`] when the drain budget expires
    /// with a handle still held (the tx is then left uncommitted),
    /// [`CloseSnapshotTxError::Close`] when the `COMMIT` or the canary re-read
    /// fails.
    pub fn close(
        &self,
    ) -> Result<Option<degenbot_db::snapshot_db::CanaryReport>, CloseSnapshotTxError> {
        self.finish(QUIESCENCE_BUDGET)
    }

    /// The shared body of [`Self::close`] and [`Drop`], with an explicit drain
    /// budget so a test can observe the refusal without waiting a full budget.
    fn finish(
        &self,
        budget: Duration,
    ) -> Result<Option<degenbot_db::snapshot_db::CanaryReport>, CloseSnapshotTxError> {
        let Some(db) = self.db.lock().take() else {
            return Ok(None);
        };
        // Acquire is closed from here on, so the holder count is monotone
        // downward: this is what makes the unwrap below answerable instead of
        // hopeful.
        let held = drain_holders(&db, budget);
        if held > 0 {
            op_warn!(
                domain = state,
                held,
                budget_ms = u64::try_from(budget.as_millis()).unwrap_or(u64::MAX),
                "retained snapshot tx: tick-map assemblers still hold the SnapshotDb — canary skipped"
            );
            return Err(CloseSnapshotTxError::AssemblersHeld);
        }
        match Arc::try_unwrap(db) {
            Ok(snap) => snap
                .close_with_canary(self.s_snapshot, self.chain_id)
                .map(Some)
                .map_err(CloseSnapshotTxError::Close),
            // The drain reported a sole holder, so this arm is unreachable
            // without a foreign clone the guard never handed out.
            Err(_) => Err(CloseSnapshotTxError::AssemblersHeld),
        }
    }
}

impl Drop for RetainedSnapshotTx {
    fn drop(&mut self) {
        // `Ok(None)` is the already-closed no-op, and a refusal was already
        // reported by `finish` — which leaves the connection to roll back with
        // the handle it kept.
        let Ok(Some(report)) = self.finish(QUIESCENCE_BUDGET) else {
            return;
        };
        if report.advanced {
            op_warn!(
                domain = state,
                s_snapshot = report.s_snapshot.unwrap_or(0),
                s_live = report.s_live.unwrap_or(0),
                "DB advanced during startup — operator discipline violation"
            );
        }
        op_info!(domain = state, "retained snapshot tx committed on drop");
    }
}

/// Wait until `db` has exactly one strong reference (the caller's), returning
/// the number still held when the budget expires.
///
/// An `Arc` exposes no drop notification, and giving every assembler call site
/// a lease type would put the invariant back in the callers' hands — the
/// arrangement this guard exists to replace. Probing the strong count is the
/// borrow the standard library does expose, and the backoff keeps a short drain
/// (the normal case) cheap.
fn drain_holders(db: &Arc<degenbot_db::snapshot_db::SnapshotDb>, budget: Duration) -> usize {
    let started = Instant::now();
    let mut probe = QUIESCENCE_PROBE_START;
    loop {
        let held = Arc::strong_count(db).saturating_sub(1);
        if held == 0 {
            return 0;
        }
        let elapsed = started.elapsed();
        if elapsed >= budget {
            return held;
        }
        std::thread::sleep(probe.min(budget.saturating_sub(elapsed)));
        probe = (probe * 2).min(QUIESCENCE_PROBE_MAX);
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
        // The plain opens never heal, so this read cannot rewrite the
        // committed fixture.
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

    /// Arm a guard over a fresh in-memory snapshot the bot has loaded.
    fn armed_guard() -> (Bot, std::sync::Arc<super::RetainedSnapshotTx>) {
        let (snap, _schema) = degenbot_db::snapshot_db::SnapshotDb::open_in_memory()
            .expect("test setup: in-memory snapshot db");
        let bot = Bot::new(1);
        bot.load_snapshot_from_db(&snap, 1)
            .expect("test setup: cold-start load");
        let tx = std::sync::Arc::new(bot.retain_snapshot_tx(snap));
        (bot, tx)
    }

    /// The teardown: `close` commits the held read tx (the handle is consumed,
    /// the WAL snapshot released) and reports the canary; on an already-closed
    /// guard it is an idempotent no-op (`None`).
    #[test]
    fn retained_snapshot_tx_commits_then_is_an_idempotent_noop() {
        let (_bot, tx) = armed_guard();

        let report = tx
            .close()
            .expect("the drain finds a sole holder")
            .expect("a held handle closes with a canary report");
        assert!(!report.advanced, "no concurrent writer in the test");
        assert_eq!(report.s_snapshot, None, "cold start: S = None");

        // After the tx is closed the teardown is a no-op (idempotent), and no
        // handle can be taken from it.
        assert!(
            tx.close().expect("no handle: Ok").is_none(),
            "a closed guard closes idempotently"
        );
        assert!(
            tx.acquire().is_none(),
            "a closed guard hands out no handles"
        );
    }

    /// The quiescence invariant: a tick-map assembler still holding a handle
    /// keeps the canary from running. The edge asserted is the release, not a
    /// duration — the closer cannot report before that drop.
    #[test]
    fn retained_snapshot_tx_waits_for_the_last_assembler_before_the_canary() {
        let (_bot, tx) = armed_guard();

        // Two assembler calls take their own handles; a second ask must still
        // be served (the guard is a source, not a once-only hand-off).
        let first = tx.acquire().expect("an armed guard hands out handles");
        drop(tx.acquire().expect("every call gets its own handle"));

        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let closer = {
            let tx = std::sync::Arc::clone(&tx);
            std::thread::spawn(move || {
                let report = tx.close();
                events_tx.send(()).expect("test channel");
                report
            })
        };

        assert!(
            events_rx.try_recv().is_err(),
            "the canary ran while an assembler still held the handle"
        );

        drop(first);
        events_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the drain completes once the last holder releases");
        let report = closer
            .join()
            .expect("the closer thread")
            .expect("the drain finds a sole holder")
            .expect("a held handle closes with a canary report");
        assert!(!report.advanced);
    }

    /// A holder that outlives the drain budget is the typed refusal — never the
    /// false-positive canary, and never a commit under a live reader.
    #[test]
    fn retained_snapshot_tx_refuses_the_canary_when_the_drain_budget_expires() {
        let (_bot, tx) = armed_guard();
        let held = tx.acquire().expect("an armed guard hands out handles");

        assert!(matches!(
            tx.finish(std::time::Duration::from_millis(20)),
            Err(super::CloseSnapshotTxError::AssemblersHeld)
        ));

        // The refusal leaves the tx uncommitted and the slot emptied: no new
        // holder can appear, and a second teardown is the idempotent no-op.
        assert!(tx.acquire().is_none());
        assert!(tx.close().expect("no handle: Ok").is_none());

        drop(held);
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

    /// `Bot::close` drives the driver's total, idempotent close (the shell keeps
    /// only the error map), and concurrent callers observe one report.
    #[test]
    fn close_drives_the_driver_close() {
        let bot = std::sync::Arc::new(Bot::new(8453));
        let driver =
            crate::arb_engine::EngineDriver::new(bot, degenbot_config::holder::config_arc());
        let first = Bot::close(&driver);
        assert_eq!(first.from, crate::arb_engine::PumpPhase::Detached);
        assert_eq!(Bot::close(&driver), first, "close is idempotent");
    }
}
