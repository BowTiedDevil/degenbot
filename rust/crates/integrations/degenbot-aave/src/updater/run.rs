//! The Aave V3 updater driver: the shared-runtime sync entry
//! ([`run_aave_update`]) plus the ONE async driver future orchestrating the
//! chunk loop over its stage modules (`apply`, `process`, `fetch`,
//! `activate`).
//!
//! The runtime-facing invariants — the shared-runtime `block_on` constraint,
//! the `!Send` `Transaction`-across-`.await` soundness argument, and the
//! atomicity-ownership duties of the loop — are documented on
//! [`run_aave_update`] / [`run_aave_update_driver`]. The chunk-atomicity
//! contract itself is owned by the `apply` stage.

mod activate;
mod apply;
mod fetch;
mod process;
pub mod substrate;

pub use activate::{
    activate_aave_market, deactivate_aave_market, ActivatedMarket, ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK,
};
pub use apply::{
    apply_aave_chunk_writes_on_conn, apply_chunk_events_on_conn, AaveChunkEvent,
    AaveChunkWriteReport,
};
use fetch::{bootstrap_pool_contracts, build_fetch_spec};
use process::{group_logs_by_tx, process_chunk_on_conn};

// ── the outer chunk loop (RPC-bound; the chunk-atomicity owner) ──

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::Address;
use degenbot_core::errors::ProviderError;
use degenbot_core::op_info;
use degenbot_core::runtime::get_runtime;
use degenbot_core::updater_telemetry::{UpdaterKind, UpdaterStage};
use degenbot_db::{DbError, DegenbotDb};
use degenbot_rpc::provider::{AlloyProvider, LogFetcher};

use crate::aave_fetch::{
    fetch_aave_chunk_logs, fetch_scaled_token_logs, fetch_stk_aave_logs,
    sort_logs_by_block_and_index,
};
use crate::config_dispatch::ConfigDispatchError;
use crate::transaction_processor::ProcessTxError;

/// The max RPC retries for the runtime-bound `AlloyProvider` the activation
/// path still builds from its `rpc_url` (the run entries are provider-INJECTED
/// per ADR-068 D5 and build nothing).
const RPC_MAX_RETRIES: u32 = 5;

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
    /// The total `AaveChunkEvent`s the apply fn wrote this chunk.
    pub events_applied: usize,
    /// `true` iff the chunk's transaction committed; `false` if it rolled
    /// back (an error mid-chunk → the whole chunk reverted → restart will
    /// re-process).
    pub committed: bool,
    /// `true` iff this is the run's final chunk (`chunk_end >= last_block` or
    /// `max_chunks` hit). Reported POST-commit; the Python shell uses it to
    /// fire the completion-time backup. The Rust completion full-verify
    /// computes finality inline.
    pub is_final: bool,
    /// The user addresses touched by ANY log in this chunk (topics[1]/[2]
    /// extracted as addresses). A programmatic [`ProgressSink`] consumer can
    /// drive the per-chunk value-correctness gate from this list via
    /// [`crate::verify::verify_touched_positions_on_conn`] against cand.db
    /// after the commit (small-set per-position RPC verification — multicall3
    /// batching is the market-wide extension).
    pub touched_user_addresses: Vec<Address>,
    /// Wall time of the chunk's RPC log fetches (the multi-pass fetch plus
    /// the same-chunk staleness re-fetches). Plain `Instant` spans (the
    /// replay-bench baseline measurement; the telemetry chunk lands its own spans
    /// later). Mirrors `degenbot-pool-updater::ChunkProgress`.
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
    /// commit/drop. The headline number for the hoist-RPC-out-of-the-tx fix
    /// (survey finding 1; the `await_holding_lock` shape).
    pub write_lock_hold_time: Duration,
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
    /// Total chunks committed.
    pub chunks_committed: usize,
    /// Total `AaveChunkEvent`s written across all chunks.
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
    #[error("runtime error: {0}")]
    Runtime(#[from] std::io::Error),
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
}

/// Run the Aave V3 chunk-update loop for `market_id`, advancing
/// `aave_v3_markets.last_update_block` to `to_block` (or the chain tip if
/// `to_block` is `None`). The chunk-atomicity owner.
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
///    `process_transaction` (C3's operations parser, sync, substrate lookups),
///    and apply THAT tx's op events to `conn` (so tx N+1 sees them).
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
/// - **`treasury_address` is `None`**: `process_transaction` accepts it but the
///   current dispatch doesn't consume it (forward-compat). The Python resolves
///   it via the Pool's `RESERVE_TREASURY_ADDRESS()` RPC — not wired here.
/// - **`vtoken_revision` drift**: the discount pre-pass reads the GHO vToken's
///   revision at chunk-start (the in-chunk `Upgraded` write is DEFERRED to
///   Apply). If an `Upgraded` event lands mid-chunk (the deprecation),
///   txs AFTER it would see the OLD revision → a non-zero discount instead of
///   0. In practice a vToken upgrade fires once per market lifetime, so the
///   drift is rare; flagged for the orchestrator's parity-gate review.
#[expect(clippy::missing_errors_doc, clippy::too_many_arguments)]
pub fn run_aave_update(
    database_path: &Path,
    chain_id: i64,
    market_id: i64,
    to_block: Option<u64>,
    chunk_size: u64,
    provider: AlloyProvider,
    cancel: Arc<AtomicBool>,
    progress: Arc<dyn ProgressSink>,
    verify_chunk: bool,
    // When `Some(n)`, run a pre-commit FULL (market-wide, all 4-check)
    // verification when a chunk's `[working_start, chunk_end]` crosses or
    // lands-on a multiple of `n` blocks. A divergence rolls back the chunk
    // + does NOT advance `last_update_block`. `None` = no interval gate.
    verify_all_interval: Option<u64>,
    // When `true`, run a pre-commit FULL verification on the run's final
    // chunk (`chunk_end >= last_block` or `max_chunks` hit). A divergence
    // rolls back the chunk + does NOT advance `last_update_block`.
    verify_all_at_completion: bool,
    max_chunks: Option<usize>,
) -> Result<AaveUpdateReport, RunError> {
    if chunk_size == 0 {
        return Err(RunError::Provider(ProviderError::InvalidBlockRange {
            from: 1,
            to: 0,
        }));
    }

    // Open ONE writeable handle for the whole run, then delegate to the
    // pre-opened-handle variant (same shape as the pool updater's seam).
    let (db, _schema_state) = DegenbotDb::open_for_writes(database_path)?;
    run_aave_update_on_db(
        &db,
        chain_id,
        market_id,
        to_block,
        chunk_size,
        provider,
        cancel,
        progress,
        verify_chunk,
        verify_all_interval,
        verify_all_at_completion,
        max_chunks,
    )
}

/// Pre-opened-handle variant of [`run_aave_update`] — behaviorally identical.
/// The statement-ledger golden machinery ([`degenbot_db::sql_ledger::LedgerDb`],
/// ADR-068 D3) needs to hand the run ITS traced connection, so the chunk loop
/// must accept a handle it did not open (the Aave golden captures are wired to
/// this seam; the committed goldens land with the replay-suite chunk).
///
/// # Errors
///
/// Same conditions as [`run_aave_update`].
#[expect(clippy::too_many_arguments)]
pub fn run_aave_update_on_db(
    db: &DegenbotDb,
    chain_id: i64,
    market_id: i64,
    to_block: Option<u64>,
    chunk_size: u64,
    provider: AlloyProvider,
    cancel: Arc<AtomicBool>,
    progress: Arc<dyn ProgressSink>,
    verify_chunk: bool,
    verify_all_interval: Option<u64>,
    verify_all_at_completion: bool,
    max_chunks: Option<usize>,
) -> Result<AaveUpdateReport, RunError> {
    if chunk_size == 0 {
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
    get_runtime().block_on(run_aave_update_driver(
        db,
        chain_id,
        market_id,
        to_block,
        chunk_size,
        provider,
        cancel,
        progress,
        verify_chunk,
        verify_all_interval,
        verify_all_at_completion,
        max_chunks,
    ))
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
#[expect(
    clippy::await_holding_lock,
    clippy::too_many_arguments,
    clippy::too_many_lines
)]
async fn run_aave_update_driver(
    db: &DegenbotDb,
    chain_id: i64,
    market_id: i64,
    to_block: Option<u64>,
    chunk_size: u64,
    provider: AlloyProvider,
    cancel: Arc<AtomicBool>,
    progress: Arc<dyn ProgressSink>,
    verify_chunk: bool,
    verify_all_interval: Option<u64>,
    verify_all_at_completion: bool,
    max_chunks: Option<usize>,
) -> Result<AaveUpdateReport, RunError> {
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
    let fetcher = LogFetcher::new(provider.clone(), chunk_size);

    // Resolve the chain tip if `to_block` is None.
    let last_block = match to_block {
        Some(n) => n,
        None => provider.get_block_number().await?,
    };

    let from_block = u64::try_from(last_update_block).unwrap_or(0) + 1;
    if from_block > last_block {
        // Already up to date — nothing to do.
        return Ok(AaveUpdateReport {
            chain_id,
            market_id,
            from_block: last_block,
            to_block: last_block,
            ..Default::default()
        });
    }

    // Cold-boot bootstrap. On a fresh market `activate` seeds only
    // `POOL_ADDRESS_PROVIDER`; `build_fetch_spec` below hard-errors if `POOL`/
    // `POOL_CONFIGURATOR` are missing. The bootstrap pass fetches the
    // `ProxyCreated` events from the `POOL_ADDRESS_PROVIDER` over the bootstrap
    // window + applies them idempotently (so the chunk loop's later
    // re-encounter of the same events is a no-op). No-op on a warm boot (both
    // rows already present). Mirrors the Python `update_aave_market` Phase-1
    // bootstrap (commands.py:1010-1062 + `_process_proxy_creation_event`).
    bootstrap_pool_contracts(db, &provider, &fetcher, market_id, from_block).await?;

    // Build the fetch spec + the GHO asset (chain-unique). The per-chunk
    // loop's refresh re-reads `scaled_token_addresses` +
    // `stk_aave_address` from the DB at the START of each chunk, so the
    // frozen run-start snapshot here is just the seed for chunk 1.
    let (mut spec, _gho_asset) = build_fetch_spec(db, market_id, chain_id)?;
    let pool_address = spec.pool_address;
    let oracle_address = spec.oracle_address;

    let mut report = AaveUpdateReport {
        chain_id,
        market_id,
        from_block,
        to_block: last_block,
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
        // PASSIVE telemetry: the loop body below is untouched, so the
        // statement ledger, the RPC request stream, and the chunk-atomicity rollback
        // boundary (the three golden gates) cannot move.
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

        // The replay-bench stage spans (plain `Instant`; the telemetry chunk lands
        // its own spans later). `chunk_lock_hold_time` is the write-lock
        // hold: `transaction()` open → commit/drop.
        let mut chunk_fetch_time = Duration::ZERO;
        let mut chunk_verify_time = Duration::ZERO;
        // Assigned (then read) in every branch that reaches a report; left
        // uninitialized so the compiler proves that.
        let chunk_lock_hold_time;

        // 1. RPC fetch the chunk's logs (GIL-free, async, sorted by
        //    (block_number, log_index)).
        //
        // (a) Refresh the scaled-token address set from the DB at the
        //     START of each chunk — matches the Python's per-chunk
        //     `_get_all_scaled_token_addresses` (commands.py:1153). The frozen
        //     run-start set (build_fetch_spec above) misses assets created in
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
        //     DiscountTokenUpdated applies. Mirrors Python's per-chunk
        //     `_get_stk_aave_address` (would-be analog of
        //     `_get_all_scaled_token_addresses`).
        spec.stk_aave_address = db
            .fetch_aave_gho_asset(chain_id)?
            .as_ref()
            .and_then(|g| g.v_gho_discount_token.as_deref())
            .and_then(|s| s.parse().ok());
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
        //     (block_number, log_index)). `process_chunk_on_conn` is
        //     UNCHANGED — the per-tx config-dispatch → ops interleave is
        //     preserved, so the `v_token_revision` conn reads stay per-tx-
        //     correct per the intra-dispatch apply (no rev-boundary regression — the
        //     rejected Option A two-pass split would have made tx N's ops see
        //     a later tx's `Upgraded`).
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
            let mut extra =
                fetch_scaled_token_logs(&fetcher, working_start, chunk_end, &new_tokens).await?;
            chunk_fetch_time += fetch_started.elapsed();
            // De-dup by (block_number, log_index) — the re-fetch may overlap
            // the frozen fetch for tokens partially known (rare). Logs
            // missing either field sort last (shouldn't happen for fetched
            // logs — the fetcher fills both).
            let existing: HashSet<(u64, u64)> = logs
                .iter()
                .filter_map(|l| Some((l.block_number?, l.log_index?)))
                .collect();
            extra.retain(|l| {
                let key = (
                    l.block_number.unwrap_or(u64::MAX),
                    l.log_index.unwrap_or(u64::MAX),
                );
                !existing.contains(&key)
            });
            if !extra.is_empty() {
                logs.extend(extra);
                sort_logs_by_block_and_index(&mut logs);
            }
        }
        // (c) Same-chunk staleness for the discount token: a
        //     `DiscountTokenUpdated` event in tx N sets the new
        //     `v_gho_discount_token` mid-chunk — but `spec.stk_aave_address`
        //     was resolved from committed DB state BEFORE the chunk's dispatch
        //     (still `None` on a cold-boot where the event that first sets the
        //     token fires mid-drive). Pre-scan the frozen logs for
        //     `DiscountTokenUpdated`, re-resolve the new discount token, +
        //     re-fetch the stkAAVE Transfer/Staked/Redeem logs for the chunk
        //     range, merging with de-dup (same pattern as `(b)` above). The
        //     `process_chunk_on_conn` dispatch is UNCHANGED — the per-tx
        //     config-dispatch still applies `DiscountTokenUpdated` at its
        //     correct logIndex, so read-your-own-writes within the transaction
        //     is preserved.
        let mut discount_token_from_event: Option<Address> = None;
        for log in &logs {
            if let Some(ev) =
                degenbot_decoders::aave_event_decoder::decode_aave_discount_token_updated_log(log)
            {
                discount_token_from_event = Some(ev.new_discount_token);
            }
        }
        if let Some(token) = discount_token_from_event.filter(|_| spec.stk_aave_address.is_none()) {
            let fetch_started = Instant::now();
            let mut extra =
                fetch_stk_aave_logs(&fetcher, working_start, chunk_end, Some(token)).await?;
            chunk_fetch_time += fetch_started.elapsed();
            let existing: HashSet<(u64, u64)> = logs
                .iter()
                .filter_map(|l| Some((l.block_number?, l.log_index?)))
                .collect();
            extra.retain(|l| {
                let key = (
                    l.block_number.unwrap_or(u64::MAX),
                    l.log_index.unwrap_or(u64::MAX),
                );
                !existing.contains(&key)
            });
            if !extra.is_empty() {
                logs.extend(extra);
                sort_logs_by_block_and_index(&mut logs);
            }
        }
        drop(fetch_guard);
        fetch_span.record(
            "rpc.round_trips",
            degenbot_rpc::provider::rpc_round_trips().saturating_sub(fetch_rt_start),
        );
        fetch_span.record("logs.n", u64::try_from(logs.len()).unwrap_or(u64::MAX));

        let tx_groups = group_logs_by_tx(&logs);

        // 2. The single-transaction chunk write (chunk atomicity). The per-tx
        //    processing (discount pre-pass + config dispatch + parse) borrows
        //    the Transaction's `&Connection` for substrate lookups + writes —
        //    they MUST be atomic with the chunk's apply.
        let chunk_report = {
            let mut guard = db.lock();
            // The write-lock hold starts at the `transaction()` open and
            // ends at the commit/drop (measured in every branch below).
            let lock_hold_started = Instant::now();
            let tx = guard.transaction().map_err(DbError::from)?;
            let result = process_chunk_on_conn(
                &tx,
                &provider,
                market_id,
                chain_id,
                pool_address,
                oracle_address,
                &tx_groups,
                working_start,
                chunk_end,
            )
            .await;
            match result {
                Ok(r) => {
                    // Pre-commit verification: if `verify_chunk` is set, run
                    // `verify_touched_positions_on_conn` on the
                    // (uncommitted) transaction. This catches divergences
                    // BEFORE the commit — a divergence drops `tx` (rollback)
                    // so `last_update_block` does NOT advance + the next run
                    // re-processes the same chunk. Without this, the bad
                    // commit would land first + the post-commit verify in
                    // the progress callback would find it but too late
                    // (the data is already durable).
                    if verify_chunk {
                        let touched: Vec<Address> =
                            r.touched_user_addresses.iter().copied().collect();
                        // Skip when no users were touched (matches the
                        // Python's `if not touched: return` — an empty
                        // chunk has nothing to verify). Passing `None`
                        // would verify ALL positions rather than none.
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
                                &tx,
                                &provider,
                                market_id,
                                chunk_end,
                                Some(&touched),
                            )
                            .await?;
                            chunk_verify_time += verify_started.elapsed();
                            drop(verify_guard);
                            verify_span.record(
                                "rpc.round_trips",
                                degenbot_rpc::provider::rpc_round_trips()
                                    .saturating_sub(verify_rt_start),
                            );
                            if !divergences.is_empty() {
                                // Drop `tx` (rollback) — the chunk's writes +
                                // the stamp advance are reverted.
                                drop(tx);
                                chunk_lock_hold_time = lock_hold_started.elapsed();
                                if let Some(t) = degenbot_core::updater_telemetry::updaters() {
                                    t.observe_lock_hold(
                                        UpdaterKind::Aave,
                                        chunk_lock_hold_time.as_secs_f64(),
                                    );
                                }
                                progress.report_chunk(&AaveChunkProgress {
                                    chain_id,
                                    market_id,
                                    chunk_start: working_start,
                                    chunk_end,
                                    events_applied: 0,
                                    committed: false,
                                    touched_user_addresses: Vec::new(),
                                    is_final: false,
                                    fetch_time: chunk_fetch_time,
                                    decode_compute_time: Duration::ZERO,
                                    verify_time: chunk_verify_time,
                                    apply_time: Duration::ZERO,
                                    write_lock_hold_time: chunk_lock_hold_time,
                                });
                                return Err(RunError::Verification {
                                    chunk_start: working_start,
                                    chunk_end,
                                    divergences,
                                });
                            }
                        }
                    }
                    // Full (market-wide) verification gate: interval
                    // boundary crossing + run completion. Runs on the
                    // uncommitted `tx` BEFORE commit — catches corrupt
                    // state before it lands. A divergence drops `tx`
                    // (rollback) so `last_update_block` does NOT advance.
                    let run_full = crate::verify::should_run_full_verify_at_interval(
                        working_start,
                        chunk_end,
                        verify_all_interval,
                    ) || (verify_all_at_completion
                        && crate::verify::is_final_chunk(
                            chunk_end,
                            last_block,
                            max_chunks.is_some_and(|limit| report.chunks_committed + 1 >= limit),
                        ));
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
                            &tx, &provider, market_id, chain_id, chunk_end, None,
                        )
                        .await?;
                        chunk_verify_time += verify_started.elapsed();
                        drop(full_verify_guard);
                        verify_span.record(
                            "rpc.round_trips",
                            degenbot_rpc::provider::rpc_round_trips()
                                .saturating_sub(full_verify_rt_start),
                        );
                        if !divergences.is_empty() {
                            drop(tx);
                            chunk_lock_hold_time = lock_hold_started.elapsed();
                            if let Some(t) = degenbot_core::updater_telemetry::updaters() {
                                t.observe_lock_hold(
                                    UpdaterKind::Aave,
                                    chunk_lock_hold_time.as_secs_f64(),
                                );
                            }
                            progress.report_chunk(&AaveChunkProgress {
                                chain_id,
                                market_id,
                                chunk_start: working_start,
                                chunk_end,
                                events_applied: 0,
                                committed: false,
                                touched_user_addresses: Vec::new(),
                                is_final: false,
                                fetch_time: chunk_fetch_time,
                                decode_compute_time: Duration::ZERO,
                                verify_time: chunk_verify_time,
                                apply_time: Duration::ZERO,
                                write_lock_hold_time: chunk_lock_hold_time,
                            });
                            return Err(RunError::FullVerification {
                                chunk_start: working_start,
                                chunk_end,
                                divergences,
                            });
                        }
                    }
                    tx.commit().map_err(DbError::from)?;
                    chunk_lock_hold_time = lock_hold_started.elapsed();
                    if let Some(t) = degenbot_core::updater_telemetry::updaters() {
                        t.observe_lock_hold(UpdaterKind::Aave, chunk_lock_hold_time.as_secs_f64());
                    }
                    r
                }
                Err(e) => {
                    // Drop `tx` (rollback) — the chunk's writes + the stamp
                    // advance are reverted; the committed prior chunks stand.
                    drop(tx);
                    chunk_lock_hold_time = lock_hold_started.elapsed();
                    if let Some(t) = degenbot_core::updater_telemetry::updaters() {
                        t.observe_lock_hold(UpdaterKind::Aave, chunk_lock_hold_time.as_secs_f64());
                    }
                    progress.report_chunk(&AaveChunkProgress {
                        chain_id,
                        market_id,
                        chunk_start: working_start,
                        chunk_end,
                        events_applied: 0,
                        committed: false,
                        touched_user_addresses: Vec::new(),
                        is_final: false,
                        // The failed core's report is consumed by the `Err`,
                        // so its stage splits are not attributable here.
                        fetch_time: chunk_fetch_time,
                        decode_compute_time: Duration::ZERO,
                        verify_time: chunk_verify_time,
                        apply_time: Duration::ZERO,
                        write_lock_hold_time: chunk_lock_hold_time,
                    });
                    return Err(e);
                }
            }
        };

        progress.report_chunk(&AaveChunkProgress {
            chain_id,
            market_id,
            chunk_start: working_start,
            chunk_end,
            events_applied: chunk_report.events_applied,
            committed: true,
            touched_user_addresses: chunk_report.touched_user_addresses.into_iter().collect(),
            is_final: chunk_end >= last_block
                || max_chunks.is_some_and(|limit| report.chunks_committed + 1 >= limit),
            fetch_time: chunk_fetch_time,
            decode_compute_time: chunk_report.decode_compute_time,
            verify_time: chunk_verify_time,
            apply_time: chunk_report.apply_time,
            write_lock_hold_time: chunk_lock_hold_time,
        });
        // The stage metrics ride the SAME AaveChunkProgress boundaries the
        // replay bench reports (one observation per committed chunk; the
        // golden numbers stay the source of truth — this is the passive
        // production twin). On a rollback the stage costs don't sample (a
        // rolled-back chunk's numbers are not attributable); the lock hold
        // DOES — it was held (the four assignment sites above).
        if let Some(t) = degenbot_core::updater_telemetry::updaters() {
            t.observe_stage(
                UpdaterKind::Aave,
                UpdaterStage::Fetch,
                chunk_fetch_time.as_secs_f64(),
            );
            t.observe_stage(
                UpdaterKind::Aave,
                UpdaterStage::Compute,
                chunk_report.decode_compute_time.as_secs_f64(),
            );
            t.observe_stage(
                UpdaterKind::Aave,
                UpdaterStage::Verify,
                chunk_verify_time.as_secs_f64(),
            );
            t.observe_stage(
                UpdaterKind::Aave,
                UpdaterStage::Apply,
                chunk_report.apply_time.as_secs_f64(),
            );
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
        // run resumes from there. Mirrors the Python pre-cutover `stop_after_n`
        // behavior (commands.py:454 pre-Rust-core — downgraded to a warning in
        // the cutover; restored here as a first-class loop cap).
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
mod tests {
    use super::*;

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
