//! The Aave V3 updater driver: the shared-runtime sync entry
//! ([`run_aave_update`]) plus the ONE async driver future orchestrating the
//! chunk loop over its stage modules (`apply`, `process`, `fetch`,
//! `activate`).
//!
//! The runtime-facing invariants — the shared-runtime `block_on` constraint,
//! the `!Send` `Transaction`-across-`.await` soundness argument, and the §3.4
//! atomicity-ownership duties of the loop — are documented on
//! [`run_aave_update`] / [`run_aave_update_driver`]. The §3.4 atomicity
//! contract itself is owned by the `apply` stage.

mod activate;
mod apply;
mod fetch;
mod process;

pub use activate::{activate_aave_market, deactivate_aave_market, ActivatedMarket};
pub use apply::{
    apply_aave_chunk_writes_on_conn, apply_chunk_events_on_conn, AaveChunkEvent,
    AaveChunkWriteReport,
};
use fetch::{bootstrap_pool_contracts, build_fetch_spec};
use process::{group_logs_by_tx, process_chunk_on_conn};

// ── the outer chunk loop (RPC-bound; the §4.4 atomicity owner) ──

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::Address;
use degenbot_core::errors::ProviderError;
use degenbot_core::op_info;
use degenbot_core::runtime::get_runtime;
use degenbot_db::{DbError, DegenbotDb};
use degenbot_rpc::provider::{AlloyProvider, LogFetcher};

use crate::aave_fetch::{
    fetch_aave_chunk_logs, fetch_scaled_token_logs, fetch_stk_aave_logs,
    sort_logs_by_block_and_index,
};
use crate::config_dispatch::ConfigDispatchError;
use crate::transaction_processor::ProcessTxError;

/// The max RPC retries for the runtime-bound `AlloyProvider` (mirrors
/// `degenbot-pool-updater`'s `RPC_MAX_RETRIES`).
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
    /// (YWEUIR) computes finality inline.
    pub is_final: bool,
    /// The user addresses touched by ANY log in this chunk (topics[1]/[2]
    /// extracted as addresses). A programmatic [`ProgressSink`] consumer can
    /// drive the per-chunk value-correctness gate from this list via
    /// [`crate::verify::verify_touched_positions_on_conn`] against cand.db
    /// after the commit (small-set per-position RPC verification — multicall3
    /// batching for the market-wide verify is BE474R-full).
    pub touched_user_addresses: Vec<Address>,
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
/// `to_block` is `None`). The §3.4 atomicity owner.
///
/// Per market per chunk:
/// 1. `fetch_aave_chunk_logs` returns the raw `Vec<Log>` sorted by
///    `(block_number, log_index)`.
/// 2. `group_logs_by_tx` returns the per-tx groups (mirrors `_build_transaction_contexts`).
/// 3. Open ONE `Transaction`. For each tx group, re-resolve the GHO vToken
///    revision (GJQGKN per-tx, sees prior txs' `Upgraded` writes), build the
///    discount snapshot (RPC + the DB-cache path), dispatch the config events
///    (RPC for revisions and metadata plus the substrate lookups), apply THAT
///    tx's config events to `conn` (so the ops parser sees them), run
///    `process_transaction` (C3's operations parser, sync, substrate lookups),
///    and apply THAT tx's op events to `conn` (so tx N+1 sees them).
/// 4. Stamp `last_update_block = chunk_end` as the LAST write (end-of-chunk).
///    The caller's `Transaction` commits (or drops, rolling back). The stamp
///    is the LAST write.
///
/// # The §3.4 atomicity invariant (LOAD-BEARING)
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
/// # §4.2-parity notes (flagged)
///
/// - **`treasury_address` is `None`**: `process_transaction` accepts it but the
///   current dispatch doesn't consume it (forward-compat). The Python resolves
///   it via the Pool's `RESERVE_TREASURY_ADDRESS()` RPC — not wired here.
/// - **`vtoken_revision` drift**: the discount pre-pass reads the GHO vToken's
///   revision at chunk-start (the in-chunk `Upgraded` write is DEFERRED to
///   Apply — §3.4). If an `Upgraded` event lands mid-chunk (the deprecation),
///   txs AFTER it would see the OLD revision → a non-zero discount instead of
///   0. In practice a vToken upgrade fires once per market lifetime, so the
///   drift is rare; flagged for the orchestrator's §4.2 review.
#[expect(clippy::missing_errors_doc, clippy::too_many_arguments)]
pub fn run_aave_update(
    database_path: &Path,
    chain_id: i64,
    market_id: i64,
    to_block: Option<u64>,
    chunk_size: u64,
    rpc_url: &str,
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

    // ONE block_on of the process-wide SHARED runtime at the fleet/CLI entry
    // seam — see "# Shared runtime" above. The driver future is polled
    // on the calling thread only, so the `!Send` `&Transaction` borrow and
    // the DB ` MutexGuard` held across `.await` stay sound (no `Send` hop,
    // no concurrent poll).
    get_runtime().block_on(run_aave_update_driver(
        database_path,
        chain_id,
        market_id,
        to_block,
        chunk_size,
        rpc_url,
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
/// inside this future; the doc contract (§3.4 atomicity, shared-runtime
/// nesting constraint, §4.2-parity notes) lives on the sync entry fn.
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
    database_path: &Path,
    chain_id: i64,
    market_id: i64,
    to_block: Option<u64>,
    chunk_size: u64,
    rpc_url: &str,
    cancel: Arc<AtomicBool>,
    progress: Arc<dyn ProgressSink>,
    verify_chunk: bool,
    verify_all_interval: Option<u64>,
    verify_all_at_completion: bool,
    max_chunks: Option<usize>,
) -> Result<AaveUpdateReport, RunError> {
    // Open ONE writeable handle for the whole run.
    let (db, _schema_state) = DegenbotDb::open_for_writes(database_path)?;

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
    let provider = AlloyProvider::new(rpc_url, RPC_MAX_RETRIES).await?;
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
    bootstrap_pool_contracts(&db, &provider, &fetcher, market_id, from_block).await?;

    // Build the fetch spec + the GHO asset (chain-unique). The per-chunk
    // loop's GJXURV refresh re-reads `scaled_token_addresses` +
    // `stk_aave_address` from the DB at the START of each chunk, so the
    // frozen run-start snapshot here is just the seed for chunk 1.
    let (mut spec, _gho_asset) = build_fetch_spec(&db, market_id, chain_id)?;
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

        // 1. RPC fetch the chunk's logs (GIL-free, async, sorted by
        //    (block_number, log_index)).
        //
        // (a) W2S3WH: refresh the scaled-token address set from the DB at the
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
        // (a)' GJXURV: refresh `spec.stk_aave_address` from the DB at the
        //     START of each chunk — the W2S3WH sibling of
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
        let mut logs = fetch_aave_chunk_logs(&spec, &fetcher, working_start, chunk_end).await?;
        // (b) W2S3WH same-chunk staleness: an asset created mid-chunk (a
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
        //     correct per I2RHGP Fix 2c (no rev-boundary regression — the
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
            let mut extra =
                fetch_scaled_token_logs(&fetcher, working_start, chunk_end, &new_tokens).await?;
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
        // (c) GJXURV same-chunk staleness for the discount token: a
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
            let mut extra =
                fetch_stk_aave_logs(&fetcher, working_start, chunk_end, Some(token)).await?;
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

        let tx_groups = group_logs_by_tx(&logs);

        // 2. The single-transaction chunk write (§4.4 atomicity). The per-tx
        //    processing (discount pre-pass + config dispatch + parse) borrows
        //    the Transaction's `&Connection` for substrate lookups + writes —
        //    they MUST be atomic with the chunk's apply.
        let chunk_report = {
            let mut guard = db.lock();
            let tx = guard.transaction().map_err(DbError::from)?;
            let result = process_chunk_on_conn(
                &tx,
                &provider,
                market_id,
                chain_id,
                pool_address,
                oracle_address,
                &tx_groups,
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
                            let divergences = crate::verify::verify_touched_positions_on_conn(
                                &tx,
                                &provider,
                                market_id,
                                chunk_end,
                                Some(&touched),
                            )
                            .await?;
                            if !divergences.is_empty() {
                                // Drop `tx` (rollback) — the chunk's writes +
                                // the stamp advance are reverted.
                                drop(tx);
                                progress.report_chunk(&AaveChunkProgress {
                                    chain_id,
                                    market_id,
                                    chunk_start: working_start,
                                    chunk_end,
                                    events_applied: 0,
                                    committed: false,
                                    touched_user_addresses: Vec::new(),
                                    is_final: false,
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
                        let divergences = crate::verify::verify_all_positions_on_conn(
                            &tx, &provider, market_id, chain_id, chunk_end, None,
                        )
                        .await?;
                        if !divergences.is_empty() {
                            drop(tx);
                            progress.report_chunk(&AaveChunkProgress {
                                chain_id,
                                market_id,
                                chunk_start: working_start,
                                chunk_end,
                                events_applied: 0,
                                committed: false,
                                touched_user_addresses: Vec::new(),
                                is_final: false,
                            });
                            return Err(RunError::FullVerification {
                                chunk_start: working_start,
                                chunk_end,
                                divergences,
                            });
                        }
                    }
                    tx.commit().map_err(DbError::from)?;
                    r
                }
                Err(e) => {
                    // Drop `tx` (rollback) — the chunk's writes + the stamp
                    // advance are reverted; the committed prior chunks stand.
                    drop(tx);
                    progress.report_chunk(&AaveChunkProgress {
                        chain_id,
                        market_id,
                        chunk_start: working_start,
                        chunk_end,
                        events_applied: 0,
                        committed: false,
                        touched_user_addresses: Vec::new(),
                        is_final: false,
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
        });
        report.chunks_committed += 1;
        report.total_events_applied += chunk_report.events_applied;

        // Operator-facing progress (Q5IKHX: the Rust core owns CLI progress —
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
