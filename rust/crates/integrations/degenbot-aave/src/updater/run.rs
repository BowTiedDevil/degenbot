//! The Aave V3 updater driver: the shared-runtime sync entry
//! ([`run_aave_update`]) plus the ONE async driver future orchestrating the
//! chunk loop over its stage modules (`apply`, `process`, `fetch`,
//! `activate`).
//!
//! The runtime-facing invariants — the shared-runtime `block_on` constraint,
//! the `!Send` `Transaction`-across-`.await` soundness argument, and the
//! atomicity-ownership duties of the loop — are documented on
//! `run_aave_update` / `run_aave_update_driver`. The chunk-atomicity
//! contract itself is owned by the `apply` stage.

mod activate;
mod apply;
mod evidence;
mod fetch;
mod process;
mod reset;
pub mod substrate;

pub use activate::{
    activate_aave_market, activate_aave_market_on_conn, deactivate_aave_market, ActivatedMarket,
    ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK,
};
pub use apply::{
    apply_aave_chunk_writes_on_conn, apply_canonical_deltas_on_conn, apply_chunk_events_on_conn,
    AaveChunkEvent, AaveChunkWriteReport, CanonicalDeltaWriteReport,
};
pub use evidence::EvidenceAcquisitionError;
use evidence::{bind_chunk_evidence, trace_backed_count};
use fetch::{bootstrap_pool_contracts, build_fetch_spec, refresh_chunk_fetch_spec};
use process::{group_logs_by_tx, guard_terminal_transaction_identity, process_chunk_on_conn};
pub use reset::{reset_market_to_cold_boot, MarketResetFacts};
use substrate::{candidate_addresses, ChunkSubstrate};

// ── the outer chunk loop (RPC-bound; the chunk-atomicity owner) ──

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::Address;
use alloy::rpc::types::Log;
use degenbot_core::canonical_head::ChainAnchor;
use degenbot_core::errors::ProviderError;
use degenbot_core::op_info;
use degenbot_core::runtime::get_runtime;
use degenbot_core::updater_telemetry::{UpdaterKind, UpdaterStage};
use degenbot_db::{DbError, DegenbotDb};
use degenbot_rpc::chain_evidence::{
    place_logs_at, resolve_terminal_anchor, ChainEvidenceError, ProjectionRefusal,
};
use degenbot_rpc::provider::{AlloyProvider, LogFetcher};
use rusqlite::{Connection, Transaction};

use crate::aave_fetch::{
    chunk_endpoint_replacements, fetch_aave_chunk_logs, fetch_endpoint_replacement_logs,
    fetch_scaled_token_logs, fetch_stk_aave_logs, is_endpoint_replacement_event,
    merge_chunk_log_extension,
};
use crate::config_dispatch::ConfigDispatchError;
use crate::transaction_processor::ProcessTxError;

/// The cadence for the chunk loop's operator-facing progress line. A short
/// time-throttle keeps a long backfill's console output readable while still
/// proving forward progress; the final chunk always logs regardless of the
/// throttle (see the loop's log site).
const PROGRESS_LOG_INTERVAL: Duration = Duration::from_secs(2);

/// A per-chunk progress snapshot reported to [`ProgressSink`] at each chunk
/// boundary (after a successful commit OR a rollback). Mirrors
/// `degenbot-pool-updater::ChunkProgress`.
#[derive(Debug, Clone)]
pub struct AaveChunkProgress {
    pub chain_id: i64,
    pub market_id: i64,
    pub chunk_start: u64,
    pub chunk_end: u64,
    /// The total deltas + configuration events the chunk wrote.
    pub events_applied: usize,
    /// `true` iff the chunk's transaction committed; `false` if it rolled
    /// back (an error mid-chunk → the whole chunk reverted → restart will
    /// re-process).
    pub committed: bool,
    /// `true` iff this is the run's final chunk (`chunk_end >= last_block` or
    /// `max_chunks` hit). Reported POST-commit; the completion-time backup
    /// fires off it. The completion full-verify computes finality inline.
    pub is_final: bool,
    /// The user addresses touched by ANY log in this chunk (topics\[1\]/\[2\]
    /// extracted as addresses). A programmatic [`ProgressSink`] consumer can
    /// drive the per-chunk value-correctness gate from this list via
    /// [`crate::verify::verify_touched_positions_on_conn`] against cand.db
    /// after the commit (small-set per-position RPC verification — multicall3
    /// batching is the market-wide extension).
    pub touched_user_addresses: Vec<Address>,
    /// Wall time of the chunk's RPC log fetches (the multi-pass fetch plus
    /// the same-chunk staleness re-fetches). Plain `Instant` spans (the
    /// replay-bench baseline measurement; the telemetry spans carry the same
    /// regions). Mirrors `degenbot-pool-updater::ChunkProgress`.
    pub fetch_time: Duration,
    /// Wall time of the in-transaction per-tx decode+compute block (the
    /// GHO/revision conn reads, the discount pre-pass + the config-event
    /// dispatch - both hold the write lock across their RPC reads).
    pub decode_compute_time: Duration,
    /// Wall time of the pre-commit on-chain-truth verification RPC (the
    /// touched-positions gate + the market-wide gate). Zero when the gates
    /// are off.
    pub verify_time: Duration,
    /// Wall time of the remaining in-transaction SQL apply (per-tx event
    /// applies, the zero-balance cleanup, the stamp): the chunk's write-lock
    /// span minus [`Self::decode_compute_time`] and [`Self::verify_time`].
    pub apply_time: Duration,
    /// The write-lock hold: the span from `transaction()` open to
    /// commit/drop. The headline number for keeping RPC reads out of the
    /// chunk transaction (the `await_holding_lock` shape).
    pub write_lock_hold_time: Duration,
}

impl AaveChunkProgress {
    /// The per-chunk report skeleton: the chunk's identity, span and fetch
    /// time, with every outcome-dependent field at its FAILURE default
    /// (zeroed counters, empty touched set, `committed: false`,
    /// `is_final: false`, zeroed stage splits and lock hold).
    ///
    /// The rollback path reports this verbatim (plus the verify split it
    /// accrued and the measured hold); the commit path overwrites the applied
    /// counters, the touched set and the stage splits. Writing the defaults
    /// once here is what lets the chunk's transaction guard own the
    /// end-of-chunk report shape without repeating the field list per exit.
    #[must_use]
    pub fn from_chunk(
        chain_id: i64,
        market_id: i64,
        chunk_start: u64,
        chunk_end: u64,
        fetch_time: Duration,
    ) -> Self {
        Self {
            chain_id,
            market_id,
            chunk_start,
            chunk_end,
            events_applied: 0,
            committed: false,
            is_final: false,
            touched_user_addresses: Vec::new(),
            fetch_time,
            decode_compute_time: Duration::ZERO,
            verify_time: Duration::ZERO,
            apply_time: Duration::ZERO,
            write_lock_hold_time: Duration::ZERO,
        }
    }
}

/// The sink the chunk loop reports per-chunk progress to. Implementations:
/// [`NoProgress`] (silent) or a programmatic consumer's sink (a test harness
/// collecting per-chunk state). `report_chunk` is synchronous (called between chunks, off
/// the async path). Mirrors `degenbot-pool-updater::ProgressSink`.
pub trait ProgressSink: Send + Sync {
    fn report_chunk(&self, progress: &AaveChunkProgress);
}

/// A no-op [`ProgressSink`] — silent runs (the default when no sink is
/// supplied). Mirrors `degenbot-pool-updater::NoProgress`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn report_chunk(&self, _progress: &AaveChunkProgress) {}
}

/// Refuse a chunk whose terminal-height logs were emitted in another block than
/// the run's resolved terminal anchor.
///
/// The anchor is the range's CLOSING evidence (ADR-105 D1/D2): resolution named a
/// block, and a log at that height carrying another block's hash means the height
/// was re-resolved to a same-height branch between the two reads. Logs below the
/// terminal height belong to the chunk's own range rather than to the anchor, so
/// only the terminal height is fenced here.
fn guard_terminal_evidence(terminal: ChainAnchor, logs: &[Log]) -> Result<(), RunError> {
    let at_terminal: Vec<Log> = logs
        .iter()
        .filter(|log| log.block_number == Some(terminal.number()))
        .cloned()
        .collect();
    place_logs_at(terminal, at_terminal)
        .map(|_placed| ())
        .map_err(RunError::TerminalEvidence)
}

/// The percentage (0–100) of the run's block span covered through `chunk_end`.
/// The denominator is the run's actual range (`from_block..=last_block`), so
/// the estimate is meaningful even when the run starts well below the chain
/// tip. Saturating integer math keeps a single-block (or already-complete) run
/// at 100.
fn progress_percent(from_block: u64, chunk_end: u64, last_block: u64) -> u64 {
    let total = last_block.saturating_sub(from_block).saturating_add(1);
    if total == 0 {
        return 100;
    }
    let processed = chunk_end
        .saturating_sub(from_block)
        .saturating_add(1)
        .min(total);
    processed.saturating_mul(100) / total
}

/// The final report from a [`run_aave_update`] run. Mirrors
/// `degenbot-pool-updater::UpdateReport`.
#[derive(Debug, Default, Clone, Copy)]
pub struct AaveUpdateReport {
    pub chain_id: i64,
    pub market_id: i64,
    /// The first block processed (inclusive).
    pub from_block: u64,
    /// The last block the run advanced `last_update_block` to.
    pub to_block: u64,
    /// The exact anchor the run's terminal block resolved to. `None` when the
    /// run had nothing to do (a caught-up market stays a pure no-op: it selects
    /// a range end and stops), and on the `Default` shape; any run that fetches
    /// resolves its terminal anchor first and states it here.
    pub terminal: Option<ChainAnchor>,
    /// Total chunks committed.
    pub chunks_committed: usize,
    /// Total deltas + configuration events written across all chunks.
    pub total_events_applied: usize,
}

/// An error from [`run_aave_update`]. Mirrors `degenbot-pool-updater::RunError`
/// + adds the Aave dispatch/parse errors.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("database error: {0}")]
    Db(#[from] DbError),
    #[error("rpc error: {0}")]
    Provider(#[from] ProviderError),
    #[error("config dispatch error: {0}")]
    ConfigDispatch(#[from] ConfigDispatchError),
    #[error("transaction parse error: {0}")]
    ProcessTx(#[from] ProcessTxError),
    /// A candidate transaction could not be evidence-bound: its block header, its
    /// complete receipt, or its required call trace is missing, foreign, or
    /// contradictory. Raised in the FETCH stage — before the chunk's transaction
    /// opens — so the cursor cannot advance on evidence no parse would accept
    /// (ADR-105 D2, D11).
    #[error("transaction evidence: {0}")]
    Evidence(Box<EvidenceAcquisitionError>),
    #[error("runtime error: {0}")]
    Runtime(#[from] std::io::Error),
    /// A chunk log carried no exact coordinate, or the chunk's log set contradicted
    /// itself. Raised in the FETCH stage — before the chunk's transaction opens — so
    /// the cursor cannot advance on log evidence the parse could not place.
    #[error("chunk log identity: {0}")]
    LogIdentity(#[from] crate::updater::aave_fetch::LogIdentityRefusal),
    #[error("cancelled by cancel flag at chunk boundary")]
    Cancelled,
    #[error("market {0} not found")]
    MarketNotFound(i64),
    #[error("market {0} has no last_update_block — bootstrap the stamp before the loop")]
    NotBootstrapped(i64),
    #[error(
        "market {0} cold-boot bootstrap failed — POOL/POOL_CONFIGURATOR remain \
         missing after the ProxyCreated fetch over the bootstrap window"
    )]
    BootstrapFailed(i64),
    /// Pre-commit verification found divergences. The chunk's `Transaction`
    /// was DROPPED (rolled back) — `last_update_block` did NOT advance, so
    /// the next run re-processes the same chunk. Carries the divergence
    /// details so the caller can format them.
    #[error("verification failed at chunk {chunk_start}-{chunk_end}")]
    Verification {
        chunk_start: u64,
        chunk_end: u64,
        divergences: Vec<crate::verify::PositionDivergence>,
    },
    /// Full (market-wide) pre-commit verification failed at the interval or
    /// completion boundary. Same rollback contract as `Verification`: drop
    /// `tx` so `last_update_block` does NOT advance; the next run re-processes
    /// the same chunk. Carries the broader `VerificationDivergence` enum
    /// (all 4 checks: scaled-token, stkAAVE, GHO discount).
    #[error("full verification failed at chunk {chunk_start}-{chunk_end}")]
    FullVerification {
        chunk_start: u64,
        chunk_end: u64,
        divergences: Vec<crate::verify::VerificationDivergence>,
    },
    /// The run's terminal block never became an exact anchor: a missing header,
    /// a cross-chain endpoint, or a failed header read. Refused before any chunk
    /// is fetched, so no cursor can move on a block the run cannot name.
    #[error("terminal anchor unresolved: {0}")]
    TerminalAnchor(#[from] ChainEvidenceError),
    /// The final chunk's terminal-height evidence disagreed with the anchor the
    /// run resolved: the height was re-resolved to another branch between
    /// resolution and fetch (a same-height substitution). The chunk is refused
    /// before its transaction opens.
    #[error("terminal block evidence disagreed with the resolved anchor: {0}")]
    TerminalEvidence(#[from] ProjectionRefusal),
}

/// The run's pre-commit verification policy — the core twin of the CLI's
/// `--verify*` flags.
#[derive(Debug, Clone, Copy, Default)]
pub struct VerifyPolicy {
    /// Run the per-chunk touched-position verification on the uncommitted
    /// transaction before every commit. A divergence rolls the chunk back.
    pub verify_chunk: bool,
    /// When `Some(n)`, run the FULL (market-wide, all 4-check) verification on
    /// any chunk whose `[working_start, chunk_end]` crosses or lands on a
    /// multiple of `n` blocks. `None` = no interval gate.
    pub verify_all_interval: Option<u64>,
    /// Run the FULL verification on the run's final chunk (`chunk_end >=
    /// last_block` or the chunk cap hit).
    pub verify_all_at_completion: bool,
}

/// Every knob ONE chunked Aave run is parameterized by, as a single value.
///
/// The run's target ([`Self::chain_id`], [`Self::market_id`],
/// [`Self::to_block`]), its chunking ([`Self::chunk_size`],
/// [`Self::max_chunks`]), its [`VerifyPolicy`], the cooperative cancel flag,
/// the per-chunk progress sink and the injected provider travel as one
/// `RunSpec` through [`run_aave_update`], [`run_aave_update_on_db`] and the
/// driver. The named fields are the single source of truth for the run's
/// interface: a caller that owned a mirrored argument list builds a `RunSpec`
/// instead.
pub struct RunSpec {
    /// The chain the market lives on (`aave_v3_markets.chain_id`).
    pub chain_id: i64,
    /// The market row's id (`aave_v3_markets.id`).
    pub market_id: i64,
    /// The last block to process (inclusive); `None` = the chain tip.
    pub to_block: Option<u64>,
    /// The chunk width in blocks. Must be non-zero.
    pub chunk_size: u64,

    /// The widest block range a single `eth_getLogs` request may span. Must be
    /// non-zero. The commit granularity ([`Self::chunk_size`]) is
    /// independent: a chunk wider than this is fetched as several
    /// `max_block_range`-sized requests: the per-request clamp
    /// (`fetch_logs_retrying`'s `max_blocks_per_request`),
    /// so an operator can point the run at a public RPC with a tighter
    /// `eth_getLogs` range limit without shrinking the commit chunk.
    pub max_block_range: u64,
    /// Stop after committing this many chunks; `None` = run through
    /// `to_block`.
    pub max_chunks: Option<usize>,
    /// The run's pre-commit verification policy.
    pub verify: VerifyPolicy,
    /// The cooperative cancel flag, checked at each chunk boundary.
    pub cancel: Arc<AtomicBool>,
    /// The per-chunk progress sink.
    pub progress: Arc<dyn ProgressSink>,
    /// The injected RPC provider (ADR-068 D5 — the caller owns construction).
    pub provider: AlloyProvider,
}

/// Run the Aave V3 chunk-update loop for `market_id`, advancing
/// `aave_v3_markets.last_update_block` to `to_block` (or the chain tip if
/// `to_block` is `None`). The chunk-atomicity owner. The run's knobs
/// travel as one [`RunSpec`].
///
/// Per market per chunk:
/// 1. `fetch_aave_chunk_logs` returns the raw `Vec<Log>` sorted by
///    `(block_number, log_index)`.
/// 2. `group_logs_by_tx` returns the per-tx groups (mirrors `_build_transaction_contexts`).
/// 3. Open ONE `Transaction`. For each tx group, re-resolve the GHO vToken
///    revision (per-tx, sees prior txs' `Upgraded` writes), build the
///    discount snapshot (RPC + the DB-cache path), dispatch the config events
///    (RPC for revisions and metadata plus the substrate lookups), apply THAT
///    tx's config events to `conn` (so the ops parser sees them), run
///    `process_transaction` (the operations parser, sync, substrate lookups),
///    and persist THAT tx's canonical deltas to `conn` (so tx N+1 sees them).
/// 4. Stamp `last_update_block = chunk_end` as the LAST write (end-of-chunk).
///    The caller's `Transaction` commits (or drops, rolling back). The stamp
///    is the LAST write.
///
/// # The chunk-atomicity invariant (LOAD-BEARING)
///
/// ONE `Transaction` per chunk. Failure mid-chunk → drop the tx → the whole
/// chunk reverts → `last_update_block` unchanged → a restart re-processes the
/// chunk clean (no skipped blocks, no partial commit). The Transaction is
/// held open across the per-tx RPC (the discount pre-pass + the config dispatch
/// do substrate lookups + writes via `get_or_create_*` that MUST be atomic with
/// the chunk; the apply is the last step). The ONE `get_runtime().block_on`
/// polls the whole driver future on the calling thread, so the `!Send`
/// `&Transaction` borrow (and the `db.lock()` guard) across `.await` are
/// safe (single-thread poll — no other runtime worker can touch this future).
///
/// # Shared runtime
///
/// The body runs as ONE future under
/// `degenbot_core::runtime::get_runtime().block_on` at this entry fn — the
/// process-wide shared runtime, not an ad-hoc Builder. That `block_on` is legal
/// on the bare `PyO3` fleet worker threads (they carry NO ambient tokio context
/// — see `degenbot-python::aave_updater`) and on the CLI's main thread. It
/// MUST NOT be called from within ANY tokio runtime context: `block_on`
/// inside a runtime — the shared one included — panics ("Cannot start a
/// runtime from within a runtime"). Mirror `degenbot-pool-updater`'s constraint.
///
/// # Parity-gate notes (flagged)
///
/// - **`vtoken_revision`**: re-resolved per transaction — stage 1 of
///   `ChunkContext::dispatch_transaction` — so a transaction after an in-chunk
///   `Upgraded` apply reads the bumped revision; a chunk-start snapshot would
///   not (`tests/fixtures/cassettes/wave4/UPGRADE-MAP.md` §(C)).
#[expect(clippy::missing_errors_doc)]
pub fn run_aave_update(database_path: &Path, spec: RunSpec) -> Result<AaveUpdateReport, RunError> {
    if spec.chunk_size == 0 || spec.max_block_range == 0 {
        return Err(RunError::Provider(ProviderError::InvalidBlockRange {
            from: 1,
            to: 0,
        }));
    }

    // Open ONE writeable handle for the whole run, then delegate to the
    // pre-opened-handle variant (same shape as the pool updater's seam).
    let (db, _schema_state) = DegenbotDb::open_for_writes(database_path)?;
    run_aave_update_on_db(&db, spec)
}

/// Pre-opened-handle variant of [`run_aave_update`] — behaviorally identical.
/// The statement-ledger golden machinery (`degenbot_db::sql_ledger::LedgerDb`,
/// ADR-068 D3) needs to hand the run ITS traced connection, so the chunk loop
/// must accept a handle it did not open (the Aave golden captures are wired to
/// this seam; `tests/aave_cassette_replay.rs` replays the committed
/// statement-ledger + DB-dump goldens through it).
///
/// # Errors
///
/// Same conditions as [`run_aave_update`].
pub fn run_aave_update_on_db(db: &DegenbotDb, spec: RunSpec) -> Result<AaveUpdateReport, RunError> {
    if spec.chunk_size == 0 || spec.max_block_range == 0 {
        return Err(RunError::Provider(ProviderError::InvalidBlockRange {
            from: 1,
            to: 0,
        }));
    }

    // ONE block_on of the process-wide SHARED runtime at the fleet/CLI entry
    // seam — see "# Shared runtime" above. The driver future is polled
    // on the calling thread only, so the `!Send` `&Transaction` borrow and
    // the DB ` MutexGuard` held across `.await` stay sound (no `Send` hop,
    // no concurrent poll).
    get_runtime().block_on(run_aave_update_driver(db, spec))
}

/// The chunk's transaction guard — the ONE owner of the chunk's end-of-chunk
/// contract.
///
/// Constructed around the chunk's freshly-opened `rusqlite` transaction.
/// [`Self::commit`] runs the success path exactly once: the `COMMIT`, the
/// write-lock-hold measurement, the telemetry observation, and the committed
/// [`AaveChunkProgress`] report. Dropping the guard without committing runs
/// the rollback path exactly once: the transaction drops (the chunk rolls
/// back), the hold is measured, and the zeroed failure report lands. A failure
/// exit inside the chunk body is therefore just `return Err(..)` — the guard's
/// drop owns the rest of the ritual, so the failure-vs-success report shape is
/// written in exactly one place.
struct ChunkTx<'a> {
    /// The chunk's transaction — `Some` until [`Self::commit`] takes it (the
    /// committed state, in which the guard's drop does nothing).
    tx: Option<Transaction<'a>>,
    /// Taken immediately before the `transaction()` open: the start of the
    /// reported write-lock-hold span.
    lock_hold_started: Instant,
    /// The chunk's report skeleton ([`AaveChunkProgress::from_chunk`]):
    /// identity, span, fetch time, and the verify split accrued mid-chunk.
    /// [`Self::commit`] fills in the applied counters and the remaining stage
    /// splits; the rollback path reports it as it stands.
    progress: AaveChunkProgress,
    /// Where the chunk's report goes.
    sink: Arc<dyn ProgressSink>,
}

impl<'a> ChunkTx<'a> {
    /// Guard `tx` for one chunk. `base` is the chunk's report skeleton
    /// ([`AaveChunkProgress::from_chunk`]) and `sink` is where the report
    /// lands; `lock_hold_started` is the `Instant` taken immediately before
    /// the `transaction()` open.
    fn new(
        tx: Transaction<'a>,
        lock_hold_started: Instant,
        base: AaveChunkProgress,
        sink: Arc<dyn ProgressSink>,
    ) -> Self {
        Self {
            tx: Some(tx),
            lock_hold_started,
            progress: base,
            sink,
        }
    }

    /// The guarded transaction's connection — the chunk body's write target.
    #[expect(clippy::expect_used)]
    fn conn(&self) -> &Connection {
        self.tx
            .as_ref()
            .expect("the chunk transaction is live until commit")
    }

    /// Accrue the wall time of a pre-commit verification pass run on the
    /// guarded transaction into the chunk report's verify split.
    fn add_verify_time(&mut self, elapsed: Duration) {
        self.progress.verify_time += elapsed;
    }

    /// The success path. Called at most once; consumes the guard so the drop
    /// cannot run the rollback path afterwards.
    ///
    /// Commits the chunk, measures the write-lock hold, observes the lock hold
    /// and the four stage metrics, and reports the committed chunk progress. A
    /// failed `COMMIT` is a rollback: the failure report lands and the database
    /// error is returned.
    #[expect(clippy::expect_used)]
    fn commit(mut self, report: &process::ChunkCoreReport, is_final: bool) -> Result<(), RunError> {
        let tx = self.tx.take().expect("the chunk transaction commits once");
        if let Err(error) = tx.commit() {
            // The `COMMIT` failed — the chunk rolled back. Report it through
            // the same rollback path the guard's drop would have taken.
            self.release_rollback();
            return Err(RunError::Db(DbError::from(error)));
        }
        self.progress.events_applied = report.events_applied;
        self.progress.committed = true;
        self.progress.is_final = is_final;
        self.progress.touched_user_addresses =
            report.touched_user_addresses.iter().copied().collect();
        self.progress.decode_compute_time = report.decode_compute_time;
        self.progress.apply_time = report.apply_time;
        self.progress.write_lock_hold_time = self.lock_hold_started.elapsed();
        if let Some(t) = degenbot_core::updater_telemetry::updaters() {
            t.observe_lock_hold(
                UpdaterKind::Aave,
                self.progress.write_lock_hold_time.as_secs_f64(),
            );
            t.observe_stage(
                UpdaterKind::Aave,
                UpdaterStage::Fetch,
                self.progress.fetch_time.as_secs_f64(),
            );
            t.observe_stage(
                UpdaterKind::Aave,
                UpdaterStage::Compute,
                self.progress.decode_compute_time.as_secs_f64(),
            );
            t.observe_stage(
                UpdaterKind::Aave,
                UpdaterStage::Verify,
                self.progress.verify_time.as_secs_f64(),
            );
            t.observe_stage(
                UpdaterKind::Aave,
                UpdaterStage::Apply,
                self.progress.apply_time.as_secs_f64(),
            );
        }
        self.sink.report_chunk(&self.progress);
        Ok(())
    }

    /// The rollback release: measure the write-lock hold, observe it, and
    /// report the chunk's zeroed failure progress. The transaction must already
    /// be gone (the guard's drop takes it first, so the hold spans the
    /// rollback).
    fn release_rollback(&mut self) {
        self.progress.write_lock_hold_time = self.lock_hold_started.elapsed();
        if let Some(t) = degenbot_core::updater_telemetry::updaters() {
            t.observe_lock_hold(
                UpdaterKind::Aave,
                self.progress.write_lock_hold_time.as_secs_f64(),
            );
        }
        // The failed core's report is consumed by the `Err`, so its stage
        // splits are not attributable here; the fetch time the chunk accrued
        // (and the verify split) are.
        self.sink.report_chunk(&self.progress);
    }
}

impl Drop for ChunkTx<'_> {
    /// Drop-without-commit: roll the chunk back and report the failure.
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            drop(tx);
            self.release_rollback();
        }
    }
}

/// The async driver body of [`run_aave_update`] — the ONE future under
/// `get_runtime().block_on`. Every RPC fetch/verify is an `.await`
/// inside this future; the doc contract (the chunk-atomicity contract,
/// shared-runtime nesting constraint, the parity-gate notes) lives on the
/// sync entry fn.
///
/// # The `await_holding_lock` expectation
///
/// The chunk body holds `db.lock()` (the writeable `Connection`) across the
/// per-tx RPC `.await`s (config dispatch + verify). Sound because the
/// driver future is never polled concurrently: the entry fn's `block_on`
/// polls it on the calling thread only, so no other thread can contend the
/// guard mid-await. This is the pre-existing shape (the guard already spanned
/// each `rt.block_on` call) — the single future just makes it explicit.
///
/// # Errors
///
/// See [`run_aave_update`].
#[expect(clippy::await_holding_lock, clippy::too_many_lines)]
async fn run_aave_update_driver(
    db: &DegenbotDb,
    spec: RunSpec,
) -> Result<AaveUpdateReport, RunError> {
    let RunSpec {
        chain_id,
        market_id,
        to_block,
        chunk_size,
        max_block_range,
        max_chunks,
        verify,
        cancel,
        progress,
        provider,
    } = spec;
    let VerifyPolicy {
        verify_chunk,
        verify_all_interval,
        verify_all_at_completion,
    } = verify;
    // Resolve the market row (the `last_update_block` cursor).
    let market = db
        .fetch_aave_market_row(market_id)?
        .ok_or(RunError::MarketNotFound(market_id))?;
    if market.chain_id != chain_id {
        return Err(RunError::Db(DbError::Decode(format!(
            "market {market_id} chain_id {} != requested {chain_id}",
            market.chain_id
        ))));
    }
    let last_update_block = market
        .last_update_block
        .ok_or(RunError::NotBootstrapped(market_id))?;

    // The RPC fetches + the per-tx verification ride the ONE driver future
    // (`get_runtime().block_on` at the entry fn); the DB writes stay
    // synchronous substrate ops on the calling thread.
    // The injected provider serves the whole run (ADR-068 D5) — the core
    // never builds a transport; the caller owns the construction.
    let provider = Arc::new(provider);
    // The RPC request width is the `max_block_range` clamp, NOT the commit
    // chunk: a chunk wider than the clamp is fetched as several
    // `max_block_range`-sized `eth_getLogs` requests, while the commit stays
    // `chunk_size`-wide. The per-request clamp
    // (`fetch_logs_retrying`'s `max_blocks_per_request`) is decoupled from its
    // `--chunk` commit granularity.
    let fetcher = LogFetcher::new(provider.clone(), max_block_range);

    // The range's END is selected first and stays a number for exactly that:
    // a run with nothing left to do must cost no evidence read (the restart
    // invariant's RPC face). The height is not carried anywhere — the moment
    // there IS work, it resolves to ONE exact anchor through the resolver both
    // arms share, and that anchor travels into chunk acquisition (ADR-105 D1).
    let last_block = match to_block {
        Some(bound) => bound,
        None => provider.get_block_number().await?,
    };

    let from_block = u64::try_from(last_update_block).unwrap_or(0) + 1;
    if from_block > last_block {
        // Already up to date — nothing to do, and nothing to anchor.
        return Ok(AaveUpdateReport {
            chain_id,
            market_id,
            from_block: last_block,
            to_block: last_block,
            ..Default::default()
        });
    }

    // Both arms (the tip's selected height and the caller's own bound) resolve
    // through ONE path: the endpoint's chain and one header read. The anchor is
    // carried into the chunk loop, where the final chunk's terminal-height logs
    // are fenced by it — a same-height substitution between resolution and fetch
    // is a refusal rather than a chunk accepted on a height alone.
    let terminal_chain = u64::try_from(chain_id).map_err(|_| {
        RunError::Provider(ProviderError::InvalidParams {
            message: format!("chain id {chain_id} is not a chain identifier"),
        })
    })?;
    let terminal = resolve_terminal_anchor(&provider, terminal_chain, Some(last_block)).await?;
    // The terminal anchor just made the endpoint's chain a verified fact, so bind
    // it once: the candidate-header reads below inherit that binding instead of
    // asking the node for its chain id again per header (one `eth_chainId` for
    // the whole run, not one per candidate block).
    provider.bind_to_chain(terminal_chain).await?;

    // Cold-boot bootstrap. On a fresh market `activate` seeds only
    // `POOL_ADDRESS_PROVIDER`; `build_fetch_spec` below hard-errors if `POOL`/
    // `POOL_CONFIGURATOR` are missing. The bootstrap pass fetches the
    // `ProxyCreated` events from the `POOL_ADDRESS_PROVIDER` over the bootstrap
    // window + applies them idempotently (so the chunk loop's later
    // re-encounter of the same events is a no-op). No-op on a warm boot (both
    // rows already present): the first-pass bootstrap that seeds the market's
    // pool contracts + proxy rows before the chunk loop runs.
    bootstrap_pool_contracts(db, &provider, &fetcher, market_id, from_block).await?;

    // Build the fetch spec + the GHO asset (chain-unique). The per-chunk
    // loop's refresh re-reads the scaled-token set, the stkAAVE address, AND
    // the four endpoint addresses (POOL/POOL_CONFIGURATOR/
    // POOL_ADDRESS_PROVIDER/PRICE_ORACLE) from the DB at the START of each
    // chunk, so the frozen run-start snapshot here is just the seed for
    // chunk 1.
    let (mut spec, _gho_asset) = build_fetch_spec(db, market_id, chain_id)?;
    // The DB's endpoint rows change ONLY through a replacement event's apply;
    // the flag scopes the per-chunk DB endpoint refresh to the chunk AFTER
    // such a commit, so a no-replacement run issues no extra statements (the
    // statement-ledger goldens stay byte-identical).
    let mut endpoints_dirty = false;

    let mut report = AaveUpdateReport {
        chain_id,
        market_id,
        from_block,
        to_block: last_block,
        terminal: Some(terminal),
        ..Default::default()
    };

    let mut working_start = from_block;
    let mut last_progress_log = Instant::now();
    while working_start <= last_block {
        // Cooperative cancel at the chunk boundary (the most recent committed
        // chunk is durable; the next chunk's writes haven't started).
        if cancel.load(Ordering::Acquire) {
            return Err(RunError::Cancelled);
        }

        let chunk_end = last_block.min(working_start + chunk_size - 1);

        // The chunk's trace ROOT (the `degenbot.epoch` shape): every stage
        // span below parents onto it through the thread-local span stack —
        // one chunk = one Jaeger waterfall. The guards ride the driver
        // future's single-thread poll (the same soundness argument as the
        // `await_holding_lock` expectation above: the future is never polled
        // concurrently, so the thread-local span context cannot tear).
        // The span wrap does not touch the loop body: the statement ledger,
        // the RPC request stream, and the chunk-atomicity rollback boundary
        // (the three golden gates) are unaffected.
        let chunk_span = tracing::info_span!(
            "degenbot.updater.aave.chunk",
            chain.id = chain_id,
            market.id = market_id,
            chunk.start = working_start,
            chunk.end = chunk_end,
        );
        let _chunk_guard = chunk_span.enter();
        // The JSON-RPC round trips this chunk issues (the production twin of
        // the cassette gates' round-trip counters). Process-wide count — see
        // `degenbot_rpc::provider::rpc_round_trips` for the scope contract.
        let chunk_rpc_round_trips_start = degenbot_rpc::provider::rpc_round_trips();

        // The replay-bench stage span (plain `Instant`; the telemetry spans
        // carry the same regions). The write-lock hold — `transaction()`
        // open → commit/drop — and the verify split accrue inside the chunk's
        // transaction guard below, which also reports them, so the loop
        // carries no caller-local measurement a new failure exit could
        // forget.
        let mut chunk_fetch_time = Duration::ZERO;

        // 1. RPC fetch the chunk's logs (GIL-free, async, sorted by
        //    (block_number, log_index)).
        //
        // (a) Refresh the scaled-token address set from the DB at the
        //     START of each chunk — the frozen run-start set (build_fetch_spec
        //     above) misses assets created in
        //     PRIOR chunks (cross-chunk + run-spanning staleness). Read BEFORE
        //     the transaction (committed state — assets from prior chunks).
        //     The remaining same-chunk case (asset created mid-chunk) is
        //     handled by the (b) pre-scan below.
        spec.scaled_token_addresses = db
            .fetch_aave_scaled_token_addresses(chain_id)?
            .into_iter()
            .filter_map(|s| s.parse::<Address>().ok())
            .collect();
        // (a)' Refresh `spec.stk_aave_address` from the DB at the
        //     START of each chunk — the sibling of
        //     `spec.scaled_token_addresses` above. The frozen run-start set
        //     (`build_fetch_spec` above) captures `v_gho_discount_token` from
        //     the cold-boot DB; on cold-boot states where `v_gho_discount_token
        //     IS NULL` (the on-chain `DiscountTokenUpdated` event that fills it
        //     fires mid-drive), the cached `None` short-circuits
        //     `fetch_stk_aave_logs` to an empty Vec for EVERY subsequent
        //     chunk — no stkAAVE Transfer events dispatched, no balance updates,
        //     the (C) refresh's on-chain backfill is the sole writer. Reading
        //     BEFORE the transaction sees committed state from prior chunks'
        //     DiscountTokenUpdated applies.
        spec.stk_aave_address = db
            .fetch_aave_gho_asset(chain_id)?
            .as_ref()
            .and_then(|g| g.v_gho_discount_token.as_deref())
            .and_then(|s| s.parse().ok());
        // (a)'' Refresh the four endpoint addresses (POOL/POOL_CONFIGURATOR/
        //     POOL_ADDRESS_PROVIDER/PRICE_ORACLE) from the DB when a PRIOR
        //     chunk committed a replacement event — its apply repointed the
        //     rows; the run-start snapshot would fetch every later chunk from
        //     the previous endpoint generation. Scoped by `endpoints_dirty`
        //     so a no-replacement run stays statement-identical.
        if endpoints_dirty {
            refresh_chunk_fetch_spec(db, market_id, &mut spec)?;
            endpoints_dirty = false;
        }
        let pool_address = spec.pool_address;
        // The fetch stage span wraps the same region the plain-`Instant`
        // `fetch_started` anchors time: the main pass + the same-chunk
        // staleness re-fetches below (the replay-bench baseline boundary).
        let fetch_span = tracing::info_span!(
            "degenbot.updater.aave.fetch",
            rpc.round_trips = tracing::field::Empty,
            logs.n = tracing::field::Empty,
        );
        let fetch_rt_start = degenbot_rpc::provider::rpc_round_trips();
        let fetch_guard = fetch_span.enter();
        let fetch_started = Instant::now();
        let mut logs = fetch_aave_chunk_logs(&spec, &fetcher, working_start, chunk_end).await?;
        chunk_fetch_time += fetch_started.elapsed();
        // (b) Same-chunk staleness: an asset created mid-chunk (a
        //     `ReserveInitialized` in tx N + the first `Supply`/`Borrow` on
        //     it in tx N+M, same chunk) has its aToken/vToken NOT in the
        //     (just-refreshed) spec set — the asset doesn't exist until the
        //     `ReserveInitialized` dispatches inside `process_chunk_on_conn`.
        //     Pre-scan the frozen logs for `ReserveInitialized` events whose
        //     aToken/vToken isn't in the known set, re-fetch those tokens'
        //     scaled-token logs for the chunk range, + merge (de-dup by
        //     (block_number, log_index)). The per-tx config-dispatch → ops
        //     interleave is preserved, so the `v_token_revision` conn reads
        //     stay per-tx-correct per the intra-dispatch apply (no
        //     rev-boundary regression — a two-pass split would let tx N's ops
        //     see a later tx's `Upgraded`).
        let known: HashSet<Address> = spec.scaled_token_addresses.iter().copied().collect();
        let mut new_tokens: Vec<Address> = Vec::new();
        for log in &logs {
            if let Some(ev) =
                degenbot_decoders::aave_event_decoder::decode_aave_reserve_initialized_log(log)
            {
                if !known.contains(&ev.a_token) {
                    new_tokens.push(ev.a_token);
                }
                if !known.contains(&ev.variable_debt_token) {
                    new_tokens.push(ev.variable_debt_token);
                }
            }
        }
        if !new_tokens.is_empty() {
            new_tokens.sort_unstable();
            new_tokens.dedup();
            let fetch_started = Instant::now();
            let extra =
                fetch_scaled_token_logs(&fetcher, working_start, chunk_end, &new_tokens).await?;
            chunk_fetch_time += fetch_started.elapsed();
            merge_chunk_log_extension(&mut logs, extra)?;
        }
        // (c) Same-chunk staleness for the discount token: a
        //     `DiscountTokenUpdated` event in tx N sets the new
        //     `v_gho_discount_token` mid-chunk — but the main pass fetched the
        //     whole chunk from the chunk-start (`spec.stk_aave_address`)
        //     generation only, so every OTHER generation's stkAAVE
        //     Transfer/Staked/Redeem logs over the chunk range are unreachable
        //     without a supplemental pass. Pre-scan the frozen logs for
        //     `DiscountTokenUpdated` and fetch every UNCOVERED generation —
        //     the tokens the chunk introduces that the main pass does not
        //     already cover (multiple updates in one chunk keep every
        //     intermediate generation; a committed generation, including a
        //     no-op `DiscountTokenUpdated(T0, T0)`, is already covered and
        //     never re-requested) — merging with de-dup (same seam as
        //     `(b)`/`(d)` above). The initial `None -> stkAAVE` set is the
        //     single-generation special case (no committed coverage to
        //     subtract), and a chunk with no `DiscountTokenUpdated` issues no
        //     supplemental pass (the statement-ledger + cassette goldens stay
        //     byte-identical). The per-tx config-dispatch still applies
        //     `DiscountTokenUpdated` at its correct logIndex, so
        //     read-your-own-writes within the transaction is preserved.
        let mut uncovered_generations: Vec<Address> = Vec::new();
        for log in &logs {
            if let Some(ev) =
                degenbot_decoders::aave_event_decoder::decode_aave_discount_token_updated_log(log)
            {
                let covered = spec
                    .stk_aave_address
                    .is_some_and(|committed| committed == ev.new_discount_token)
                    || uncovered_generations.contains(&ev.new_discount_token);
                if !covered {
                    uncovered_generations.push(ev.new_discount_token);
                }
            }
        }
        if !uncovered_generations.is_empty() {
            let fetch_started = Instant::now();
            let mut extra = Vec::new();
            for token in &uncovered_generations {
                extra.extend(
                    fetch_stk_aave_logs(&fetcher, working_start, chunk_end, Some(*token)).await?,
                );
            }
            chunk_fetch_time += fetch_started.elapsed();
            merge_chunk_log_extension(&mut logs, extra)?;
        }
        // (d) Same-chunk endpoint replacement: an `AddressSet` rotation or a
        //     `PriceOracleUpdated` in tx N repoints a fetch endpoint (POOL/
        //     POOL_CONFIGURATOR/POOL_ADDRESS_PROVIDER/PRICE_ORACLE)
        //     mid-chunk — the main pass fetched the whole chunk from the
        //     chunk-start endpoints, so the replacement endpoint's logs over
        //     the same range are unreachable without a supplemental pass.
        //     Pre-scan the frozen logs for the replacement events, fetch the
        //     uncovered addresses' logs for the chunk range, + merge with
        //     de-dup (the same seam as (b)/(c)). The tracking policy follows
        //     the replacement (the contract permits it); a refusal would
        //     abort in dispatch before the cursor can advance. The chunk's
        //     committed apply is what repoints the DB rows, so any tracked
        //     replacement event marks the endpoints dirty for the NEXT
        //     chunk's DB refresh.
        let replacements = chunk_endpoint_replacements(&logs, &spec);
        if !replacements.is_empty() {
            let fetch_started = Instant::now();
            let extra =
                fetch_endpoint_replacement_logs(&fetcher, working_start, chunk_end, &replacements)
                    .await?;
            chunk_fetch_time += fetch_started.elapsed();
            merge_chunk_log_extension(&mut logs, extra)?;
        }
        if chunk_end == last_block {
            guard_terminal_evidence(terminal, &logs)?;
        }
        if logs.iter().any(is_endpoint_replacement_event) {
            endpoints_dirty = true;
        }
        drop(fetch_guard);
        fetch_span.record(
            "rpc.round_trips",
            degenbot_rpc::provider::rpc_round_trips().saturating_sub(fetch_rt_start),
        );
        fetch_span.record("logs.n", u64::try_from(logs.len()).unwrap_or(u64::MAX));

        let tx_groups = group_logs_by_tx(&logs)?;
        if chunk_end == last_block {
            guard_terminal_transaction_identity(terminal, &tx_groups)?;
        }

        // 1b. Candidate-transaction evidence (ADR-105 D2, D5). Every candidate
        //     transaction acquires its complete receipt, and — only where a parse
        //     leg is named by a call (a liquidation's collateral sub-call) — its
        //     call trace; both are bound to the filtered logs before the parse can
        //     see either. The reads happen HERE, outside the chunk's transaction:
        //     a missing, foreign, or contradictory receipt refuses the chunk with
        //     no write and no cursor movement (ADR-105 D11).
        let evidence_span = tracing::info_span!(
            "degenbot.updater.aave.evidence",
            candidates.n = tracing::field::Empty,
            traces.n = tracing::field::Empty,
            rpc.round_trips = tracing::field::Empty,
        );
        let evidence_rt_start = degenbot_rpc::provider::rpc_round_trips();
        let candidates = {
            let _evidence_guard = evidence_span.enter();
            bind_chunk_evidence(&provider, terminal_chain, terminal, tx_groups)
                .await
                .map_err(|error| RunError::Evidence(Box::new(error)))?
        };
        evidence_span.record(
            "candidates.n",
            u64::try_from(candidates.len()).unwrap_or(u64::MAX),
        );
        evidence_span.record(
            "traces.n",
            u64::try_from(trace_backed_count(&candidates)).unwrap_or(u64::MAX),
        );
        evidence_span.record(
            "rpc.round_trips",
            degenbot_rpc::provider::rpc_round_trips().saturating_sub(evidence_rt_start),
        );

        // 1c. The operation family's read-side substrate access, taken BEFORE
        //     the chunk's transaction opens (ADR-105 D4): one set-shaped read of
        //     the chunk's touched entities (candidate users and their
        //     positions/configs, the market's assets and contracts, the chain's
        //     GHO row). Resolution then runs against this read position — the
        //     projection view seeds from it and advances by deltas — instead of
        //     holding the write lock across its own reads.
        let substrate = if candidates.is_empty() {
            ChunkSubstrate::lazy(chain_id)
        } else {
            let all_logs: Vec<&Log> = candidates
                .iter()
                .flat_map(|candidate| candidate.group.logs.iter().copied())
                .collect();
            let (user_candidates, asset_candidates) = candidate_addresses(&all_logs);
            let read = db.lock();
            ChunkSubstrate::prefetch_on_conn(
                &read,
                market_id,
                chain_id,
                &user_candidates,
                &asset_candidates,
            )?
        };

        // 2. The single-transaction chunk write (chunk atomicity). The per-tx
        //    processing (discount pre-pass + config dispatch + parse) borrows
        //    the Transaction's `&Connection` for substrate lookups + writes —
        //    they MUST be atomic with the chunk's apply.
        let chunk_report = {
            let mut guard = db.lock();
            // The write-lock hold starts at the `transaction()` open and ends
            // at the commit/drop — measured by the chunk's guard.
            let lock_hold_started = Instant::now();
            let tx = guard.transaction().map_err(DbError::from)?;
            // The guard owns the rest of the end-of-chunk contract: every
            // failure exit below is one `?` or `return Err(..)`, and the
            // guard's drop rolls the chunk back + reports the zeroed failure
            // progress.
            let mut chunk_tx = ChunkTx::new(
                tx,
                lock_hold_started,
                AaveChunkProgress::from_chunk(
                    chain_id,
                    market_id,
                    working_start,
                    chunk_end,
                    chunk_fetch_time,
                ),
                progress.clone(),
            );
            let r = process_chunk_on_conn(
                chunk_tx.conn(),
                &provider,
                market_id,
                chain_id,
                pool_address,
                &candidates,
                substrate,
                working_start,
                chunk_end,
            )
            .await?;
            // Pre-commit verification: if `verify_chunk` is set, run
            // `verify_touched_positions_on_conn` on the (uncommitted)
            // transaction. This catches divergences BEFORE the commit — a
            // divergence rolls the chunk back so `last_update_block` does NOT
            // advance + the next run re-processes the same chunk. Without
            // this, the bad commit would land first + the post-commit verify
            // in the progress callback would find it but too late (the data
            // is already durable).
            if verify_chunk {
                let touched: Vec<Address> = r.touched_user_addresses.iter().copied().collect();
                // Skip when no users were touched — an empty chunk has
                // nothing to verify. Passing `None` would verify ALL positions rather
                // than none.
                if !touched.is_empty() {
                    let verify_span = tracing::info_span!(
                        "degenbot.updater.aave.verify",
                        verify.kind = "touched",
                        rpc.round_trips = tracing::field::Empty,
                    );
                    let verify_rt_start = degenbot_rpc::provider::rpc_round_trips();
                    let verify_guard = verify_span.enter();
                    let verify_started = Instant::now();
                    let divergences = crate::verify::verify_touched_positions_on_conn(
                        chunk_tx.conn(),
                        &provider,
                        market_id,
                        chunk_end,
                        Some(&touched),
                    )
                    .await?;
                    chunk_tx.add_verify_time(verify_started.elapsed());
                    drop(verify_guard);
                    verify_span.record(
                        "rpc.round_trips",
                        degenbot_rpc::provider::rpc_round_trips().saturating_sub(verify_rt_start),
                    );
                    if !divergences.is_empty() {
                        return Err(RunError::Verification {
                            chunk_start: working_start,
                            chunk_end,
                            divergences,
                        });
                    }
                }
            }
            // Full (market-wide) verification gate: interval boundary crossing
            // + run completion. Runs on the uncommitted transaction BEFORE
            // commit — catches corrupt state before it lands. A divergence
            // rolls the chunk back so `last_update_block` does NOT advance.
            //
            // The final-chunk test is also the committed report's `is_final`.
            let max_chunks_hit =
                max_chunks.is_some_and(|limit| report.chunks_committed + 1 >= limit);
            let is_final = crate::verify::is_final_chunk(chunk_end, last_block, max_chunks_hit);
            let run_full = crate::verify::should_run_full_verify_at_interval(
                working_start,
                chunk_end,
                verify_all_interval,
            ) || (verify_all_at_completion
                && crate::verify::is_final_chunk(chunk_end, last_block, max_chunks_hit));
            if run_full {
                let verify_span = tracing::info_span!(
                    "degenbot.updater.aave.verify",
                    verify.kind = "full",
                    rpc.round_trips = tracing::field::Empty,
                );
                let full_verify_rt_start = degenbot_rpc::provider::rpc_round_trips();
                let full_verify_guard = verify_span.enter();
                let verify_started = Instant::now();
                let divergences = crate::verify::verify_all_positions_on_conn(
                    chunk_tx.conn(),
                    &provider,
                    market_id,
                    chain_id,
                    chunk_end,
                    None,
                )
                .await?;
                chunk_tx.add_verify_time(verify_started.elapsed());
                drop(full_verify_guard);
                verify_span.record(
                    "rpc.round_trips",
                    degenbot_rpc::provider::rpc_round_trips().saturating_sub(full_verify_rt_start),
                );
                if !divergences.is_empty() {
                    return Err(RunError::FullVerification {
                        chunk_start: working_start,
                        chunk_end,
                        divergences,
                    });
                }
            }
            // The success path: commit, measure the hold, observe the
            // lock-hold + stage telemetry, report the committed chunk.
            chunk_tx.commit(&r, is_final)?;
            r
        };
        // The chunk's RPC round trips ride the SAME chunk boundaries the
        // replay bench reports (one observation per committed chunk; the
        // golden numbers stay the source of truth — this is the passive
        // production twin). The stage metrics + the lock hold are observed by
        // the chunk's guard: a rolled-back chunk samples the lock hold (it was
        // held) but none of the stage splits (a rolled-back chunk's numbers
        // are not attributable).
        if let Some(t) = degenbot_core::updater_telemetry::updaters() {
            t.add_rpc_round_trips(
                UpdaterKind::Aave,
                degenbot_rpc::provider::rpc_round_trips()
                    .saturating_sub(chunk_rpc_round_trips_start),
            );
        }
        report.chunks_committed += 1;
        report.total_events_applied += chunk_report.events_applied;

        // Operator-facing progress (the Rust core owns CLI progress —
        // no per-chunk FFI hop). Time-throttled so a long backfill's console
        // stays readable; the run's final chunk always logs so completion is
        // observable even when the last chunks land inside one throttle window.
        if last_progress_log.elapsed() >= PROGRESS_LOG_INTERVAL || chunk_end >= last_block {
            op_info!(
                domain = aave,
                chain_id,
                market_id,
                chunk_start = working_start,
                chunk_end,
                chunks_committed = report.chunks_committed,
                events_applied = chunk_report.events_applied,
                progress_pct = progress_percent(from_block, chunk_end, last_block),
                "aave update: chunk committed"
            );
            last_progress_log = Instant::now();
        }

        working_start = chunk_end + 1;

        // `--one-chunk` cap: stop after committing `max_chunks` chunks (NOT
        // mid-loop and NOT before the first chunk). `last_update_block` is
        // advanced to the last committed chunk's `chunk_end` (the stamp write
        // happened inside `apply_chunk_events_on_conn`'s commit), so the next
        // run resumes from there. `--stop-after-n` is a first-class loop cap: a
        // bounded run stops cleanly instead of being killed mid-chunk.
        if let Some(limit) = max_chunks {
            if report.chunks_committed >= limit {
                report.to_block = chunk_end;
                break;
            }
        }
    }

    Ok(report)
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Collects the reported chunks (the shape the replay suite's collector
    /// asserts).
    #[derive(Default)]
    struct CollectSink(Mutex<Vec<AaveChunkProgress>>);

    impl ProgressSink for CollectSink {
        fn report_chunk(&self, progress: &AaveChunkProgress) {
            self.0.lock().unwrap().push(progress.clone());
        }
    }

    /// The rollback defaults are written in one place: `from_chunk` keeps the
    /// chunk's identity + fetch time and zeroes everything the committed path
    /// fills in.
    #[test]
    fn from_chunk_carries_the_failure_defaults() {
        let progress = AaveChunkProgress::from_chunk(1, 2, 100, 199, Duration::from_millis(7));
        assert_eq!(progress.chain_id, 1);
        assert_eq!(progress.market_id, 2);
        assert_eq!(progress.chunk_start, 100);
        assert_eq!(progress.chunk_end, 199);
        assert_eq!(progress.fetch_time, Duration::from_millis(7));
        assert_eq!(progress.events_applied, 0);
        assert!(!progress.committed);
        assert!(!progress.is_final);
        assert!(progress.touched_user_addresses.is_empty());
        assert_eq!(progress.decode_compute_time, Duration::ZERO);
        assert_eq!(progress.verify_time, Duration::ZERO);
        assert_eq!(progress.apply_time, Duration::ZERO);
        assert_eq!(progress.write_lock_hold_time, Duration::ZERO);
    }

    /// The helper under the driver-level `address_set_pool_rotation_fetches_
    /// new_pool_logs_next_chunk` replay: a committed `AddressSet` POOL
    /// rotation must be re-readable into the fetch spec — the row-level half
    /// of the next-chunk repoint.
    #[test]
    fn refresh_chunk_fetch_spec_rereads_rotated_endpoint_rows() {
        use crate::aave_fetch::AaveFetchSpec;

        let (db, _state) = DegenbotDb::open_for_writes(Path::new(":memory:")).unwrap();
        let old_pool = Address::repeat_byte(0x11);
        let new_pool = Address::repeat_byte(0x22);
        let checksummed = |a: Address| degenbot_core::address_utils::address_to_checksum_string(&a);
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
                 VALUES (1, 1, 'm', 1, NULL)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_contracts (market_id, name, address) \
                 VALUES (1, 'POOL', ?1)",
                [checksummed(old_pool)],
            )
            .unwrap();
        }
        let mut spec = AaveFetchSpec::default();
        fetch::refresh_chunk_fetch_spec(&db, 1, &mut spec).unwrap();
        assert_eq!(spec.pool_address, old_pool);
        {
            let conn = db.lock();
            DegenbotDb::apply_contract_address_changed_on_conn(
                &conn,
                1,
                "POOL",
                &checksummed(old_pool),
                &checksummed(new_pool),
            )
            .unwrap();
        }
        fetch::refresh_chunk_fetch_spec(&db, 1, &mut spec).unwrap();
        assert_eq!(
            spec.pool_address, new_pool,
            "chunk N+1's fetch plan must target the rotated POOL"
        );
    }

    #[test]
    fn dropping_the_guard_reports_the_rolled_back_chunk() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        let sink = Arc::new(CollectSink::default());
        let tx = conn.transaction().unwrap();
        let guard = ChunkTx::new(
            tx,
            Instant::now(),
            AaveChunkProgress::from_chunk(1, 2, 10, 19, Duration::from_millis(3)),
            sink.clone(),
        );
        assert!(!guard.conn().is_autocommit(), "the chunk tx is open");
        drop(guard);
        let chunks = sink.0.lock().unwrap().clone();
        assert_eq!(chunks.len(), 1, "one rollback report");
        assert_eq!(chunks[0].chunk_start, 10);
        assert_eq!(chunks[0].chunk_end, 19);
        assert_eq!(chunks[0].events_applied, 0);
        assert!(!chunks[0].committed);
        assert!(conn.is_autocommit(), "the chunk tx rolled back on drop");
    }

    /// The progress estimate spans the run's actual range (not the chain
    /// tip) and saturates at 100 on the final chunk.
    #[test]
    fn progress_percent_spans_the_run_range() {
        assert_eq!(progress_percent(1, 1, 100), 1);
        assert_eq!(progress_percent(1, 50, 100), 50);
        assert_eq!(progress_percent(1, 100, 100), 100);
        // A non-1 start block: the denominator is the run span (50 blocks),
        // so halfway through the run reads 50%, not 75%.
        assert_eq!(progress_percent(51, 75, 100), 50);
        // A single-block run reports 100 on its only chunk.
        assert_eq!(progress_percent(100, 100, 100), 100);
    }
}
