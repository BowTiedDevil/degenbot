//! The Rust-owned pool-updater chunk loop — `run_pool_update`.
//!
//! This is the standalone-Rust `pool_update` core: it opens ONE writeable
//! [`DegenbotDb`], loads the active exchange specs, resolves
//! the chain tip, + advances `last_update_block` chunk by chunk. Each chunk:
//!
//! 1. RPC-fetches (GIL-free, async) the chunk's `PoolCreated`/`Initialize`
//!    events per in-scope exchange + the V3/V4 liquidity events for the chunk
//!    range.
//! 2. Decodes them into typed row-inputs + grouped `LiquidityUpdateEvent`s.
//! 3. Writes the WHOLE chunk under ONE `rusqlite::Transaction` via
//!    [`apply_chunk_writes_on_conn`]: pool upserts + liquidity applies + the
//!    `exchanges.last_update_block` stamp. Any error before commit drops the
//!    `Transaction` → the chunk is fully reverted → `last_update_block`
//!    unchanged → the next run re-processes the chunk clean (no skipped
//!    blocks, no duplicate-pool `UNIQUE` violations).
//!
//! # The chunk-atomicity invariants (`docs/architecture/chunk-atomicity.md`) —
//! hold by construction
//!
//! - **Atomicity:** every write of a chunk goes through ONE
//!   [`apply_chunk_writes_on_conn`] call on ONE `Transaction`; the commit is
//!   the single point of durability (proven by 3a's atomicity round-trip
//!   tests + this module's [`apply_chunk_writes_on_conn`] tests).
//! - **Restart-invariance:** `last_update_block` is stamped INSIDE the
//!   transaction (the LAST write) — on rollback the stamp does NOT advance,
//!   so a restart re-fetches + re-writes the same chunk (the rolled-back
//!   pool rows aren't durable, so re-inserting is not a `UNIQUE` violation).
//! - **Idempotent re-run (dedup):** a committed chunk won't re-process — the
//!   outer loop derives `working_start_block` from each exchange's persisted
//!   `last_update_block`, so a restarted run resumes exactly where it left
//!   off (no double-processing of committed chunks).
//!
//! # The pre-commit gate is unconditional
//!
//! Every chunk's planned maps are compared against on-chain truth BEFORE its
//! `Transaction` opens - there is no switch that skips it. `verify_all_*`
//! (the market-wide interval/completion policies) are ADDITIONAL checks over
//! the whole in-scope market, never a substitute for the per-chunk gate.
//!
//! # The transaction boundary (read pass + verify outside it)
//!
//! The per-pool O(map) compute and the pre-commit verify RPC run BEFORE the
//! chunk `Transaction` opens ([`compute_preverified_liquidity`] + the
//! driver's gates): zero RPC round trips - and none of the planned pools' map
//! SELECTs - sit between `transaction()` open and commit/drop. The soundness
//! argument for that boundary:
//!
//! 1. The read pass computes each in-scope pool's map from the COMMITTED DB
//!    state and the apply persists that plan's delta — one derivation per
//!    pool, so the gated artifact IS the persisted artifact. A chunk-new
//!    pool's plan carries the sentinel `pool_id` 0 (no row existed at plan
//!    time) and the apply keys it to the id its own upsert assigned inside the
//!    tx. The only way the planned map diverges from committed state is a
//!    concurrent writer mutating the base between the read pass and the tx.
//! 2. A concurrent writer cannot exist in the bot's single-writer discipline
//!    (one `run_pool_update` per DB; the write path is the chunk loop). The
//!    invariant is nevertheless made STRUCTURAL: the end-of-chunk stamp is an
//!    optimistic UPDATE carrying the marker the read pass assumed
//!    (`set_exchange_last_update_block_if_unchanged_on_conn`); a moved marker
//!    matches zero rows -> [`RunError::MarkerMoved`] -> the transaction drops
//!    (nothing staged survives) and the chunk re-plans from refreshed specs.
//! 3. A verification RED never opens the transaction. The observable contract
//!    is:
//!    [`RunError::Verification`] raised, ZERO rows written, the stamp
//!    unadvanced - the verification-gate probes assert exactly this triple
//!    (post-run state) and stay green with unchanged assertions.
//!
//! # Why the write half is split out ([`apply_chunk_writes_on_conn`])
//!
//! The hard-to-test RPC boundary (fetch + decode) is separated from the
//! transaction-semantics core. [`apply_chunk_writes_on_conn`] is a pure
//! synchronous function: feed it pre-built fixture decoded events + a temp
//! `DegenbotDb` + a `Transaction`, + assert the all-or-nothing outcome. No
//! mock RPC, no live node. The fetchers themselves are unit-tested in
//! [`crate::fetch`] (decode-leaves) + integration-tested (live RPC).
//!
//! # D2: the shared `tokio` runtime
//!
//! [`run_pool_update`] blocks on the process-wide shared runtime
//! (`degenbot_core::runtime::get_runtime()`) — the `&'static` singleton, no
//! ad-hoc Builder. **It MUST NOT be called from within any tokio runtime
//! context** — `block_on` (the shared runtime included) panics there
//! ("Cannot start a runtime from within a runtime"). The Task 4 `PyO3` seam
//! (`db_run_pool_update`) is the entry from Python: Python calls it from a
//! worker thread (NO ambient tokio runtime), so the constraint holds. If
//! Python ever needs to drive `run_pool_update` from inside an async
//! context, wrap it in `tokio::task::spawn_blocking`.

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};
use degenbot_core::errors::ProviderError;
use degenbot_core::op_info;
use degenbot_core::updater_telemetry::{UpdaterKind, UpdaterStage};
use degenbot_db::{
    derive_liquidity_delta_from_computed, ComputedLiquidityUpdate, DegenbotDb, LiquidityDelta,
    LiquidityUpdateEvent, V2PoolRowInput, V3PoolRowInput, V4PoolRowInput,
};
use degenbot_rpc::liquidity_verifier::LiquidityMapTarget;
use degenbot_rpc::provider::{AlloyProvider, LogFetcher};
use rusqlite::Connection;

use crate::verify::{gate_liquidity_map_on_chain, LiquidityDivergence};

use crate::fetch::{
    fetch_pool_created_logs_for_spec, fetch_v3_liquidity_logs_grouped,
    fetch_v4_liquidity_logs_grouped, DecodedPoolCreated,
};
use crate::spec::{load_active_exchange_specs, ExchangeSpec};

/// The cadence for the chunk loop's operator-facing progress line. A short
/// time-throttle keeps a long backfill's console output readable while still
/// proving forward progress; the final chunk always logs regardless of the
/// throttle (see the loop's log site).
const PROGRESS_LOG_INTERVAL: Duration = Duration::from_secs(2);

/// The optimistic stamp's retry budget: how many times one run may drop a
/// chunk because its assumed `last_update_block` marker moved under it
/// (`RunError::MarkerMoved`) before the run surfaces the error. A retry
/// re-reads the exchange specs and re-plans the SAME chunk from the markers the
/// DB holds, without advancing the cursor. Under the bot's single-writer
/// discipline this is unreachable; the bound is the loud-failure backstop.
const MARKER_RETRY_CAP: usize = 4;

// ── progress reporting ─────────────────────────────────────────────────

/// A per-chunk progress snapshot reported to [`ProgressSink`] at each chunk
/// boundary (after a successful commit OR a rollback).
#[derive(Debug, Clone, Copy)]
pub struct ChunkProgress {
    /// The chain this chunk advanced.
    pub chain_id: i64,
    /// Inclusive start block of the chunk.
    pub chunk_start: u64,
    /// Inclusive end block of the chunk.
    pub chunk_end: u64,
    /// Pools newly written this chunk (V2/V3/V4 pool-creation rows).
    pub pools_written: usize,
    /// Per-pool liquidity applies performed this chunk (V3 + V4).
    pub liquidity_apply_count: usize,
    /// `true` iff the chunk's transaction committed; `false` if it rolled
    /// back (an error mid-chunk → the whole chunk reverted → restart will
    /// re-process).
    pub committed: bool,
    /// `true` iff this is the run's final chunk (`chunk_end >= last_block`).
    /// Reported POST-commit so it reflects a committed final chunk (the
    /// Python shell uses it to fire the completion-time backup). The Rust
    /// completion full-verify computes finality inline.
    pub is_final: bool,
    /// Wall time of the chunk's RPC fetch passes (pool creations + the V3
    /// whole-chain + V4 per-manager liquidity scans). Plain `Instant` spans
    /// (the replay-driven baseline measurement; the telemetry chunk lands its own
    /// spans later).
    pub fetch_time: Duration,
    /// Wall time of the in-transaction per-pool full-map read + compute
    /// (`compute_v3/v4_liquidity_update_on_conn`) - the O(map) SQL shape.
    pub decode_compute_time: Duration,
    /// Wall time of the pre-commit on-chain-truth verification RPC (the
    /// per-pool gate + the market-wide gate). Zero when the gates are off.
    pub verify_time: Duration,
    /// Wall time of the remaining in-transaction SQL apply (pool upserts,
    /// persists, the stamp): the chunk's write-lock span minus
    /// [`Self::decode_compute_time`] and [`Self::verify_time`].
    pub apply_time: Duration,
    /// The write-lock hold: the span from `transaction()` open to
    /// commit/drop. The headline number for the hoist-RPC-out-of-the-tx
    /// fix (survey finding 1).
    pub write_lock_hold_time: Duration,
}

/// The sink the chunk loop reports per-chunk progress to. Implementations:
/// `NoProgress` (silent) or a programmatic consumer's sink (a test harness
/// collecting per-chunk state).
///
/// `report_chunk` is synchronous (called between chunks, off the async path).
pub trait ProgressSink: Send + Sync {
    /// Report a chunk's outcome. Called once per chunk (chunk boundary).
    fn report_chunk(&self, progress: &ChunkProgress);
}

/// A no-op [`ProgressSink`] — silent runs (the default for `run_pool_update`
/// when no sink is supplied).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn report_chunk(&self, _progress: &ChunkProgress) {}
}

/// The percentage (0–100) of the run's block span covered through
/// `working_end_block`. The denominator is the run's actual range
/// (`initial_start_block..=last_block`), so the estimate is meaningful even
/// when the run starts well below the chain tip. Saturating integer math keeps
/// a single-block (or already-complete) run at 100.
fn progress_percent(initial_start_block: u64, working_end_block: u64, last_block: u64) -> u64 {
    let total = last_block
        .saturating_sub(initial_start_block)
        .saturating_add(1);
    if total == 0 {
        return 100;
    }
    let processed = working_end_block
        .saturating_sub(initial_start_block)
        .saturating_add(1)
        .min(total);
    processed.saturating_mul(100) / total
}

// ── the pre-mapped, pre-fee-resolved chunk inputs ───────────────────────

/// A decoded pool-creation event already mapped to its DB `V*PoolRowInput`
/// (+ the `exchange_id` it belongs to) — the fruit of the outer loop's
/// `fetch_pool_created_logs_for_spec` + [`map_pool_creation`] (the latter may
/// resolve a per-pool RPC fee for Aerodrome V2). The inner writer consumes
/// these directly so it does no decode or RPC — it just upserts.
#[derive(Debug, Clone)]
pub enum PoolCreationToWrite {
    /// A V2 pool row (canonical `Uniswap V2` / `PancakeSwap V2` / `SushiSwap V2` /
    /// `Swapbased V2` — constant fee from the spec's `v2_fee_token`).
    V2 {
        exchange_id: i64,
        row: V2PoolRowInput,
    },
    /// A V3 pool row (`Uniswap V3` / `PancakeSwap V3` / `SushiSwap V3` /
    /// `Aerodrome V3` — fee + `tick_spacing` from the event).
    V3 {
        exchange_id: i64,
        row: V3PoolRowInput,
    },
    /// A V4 pool row (`Uniswap V4` — fee + `tick_spacing` + hooks from the
    /// `Initialize` event).
    V4 {
        exchange_id: i64,
        row: V4PoolRowInput,
    },
}

/// The decoded chunk data the outer loop built from RPC — the input to
/// [`apply_chunk_writes_on_conn`]. Carries NO RPC handle (the fetches already
/// happened); the inner fn writes it all under ONE transaction.
#[derive(Debug, Default)]
pub struct ChunkInputs {
    /// Pool-creation rows to upsert, pre-mapped + pre-fee-resolved.
    pub pool_creations: Vec<PoolCreationToWrite>,
    /// V3 liquidity events grouped by emitting pool address (whole-chain
    /// scan for the chunk range; the inner fn in-scope-filters).
    pub v3_liquidity: HashMap<Address, Vec<LiquidityUpdateEvent>>,
    /// V4 liquidity events grouped by `pool_hash` hex (per-PoolManager scan).
    pub v4_liquidity: HashMap<String, Vec<LiquidityUpdateEvent>>,
    /// `pool_hash` hex → the V4 `PoolManager` address that emitted each pool's
    /// `ModifyLiquidity` events (collected during the per-PoolManager fetch
    /// loop). The pre-commit on-chain gate needs this to call `extsload` on
    /// the right singleton per pool.
    pub v4_manager_addresses: HashMap<String, Address>,
    /// The `pool_manager_chain` for V4 applies (the chain of the
    /// PoolManager(s) whose `ModifyLiquidity` events were scanned — equals
    /// `chain_id` for the per-chain chunk loop).
    pub pool_manager_chain: i64,
}

/// A pre-transaction-computed liquidity map for one in-scope pool - the
/// read pass's output and the artifact the apply persists. `is_new_pool`
/// marks a pool whose row is created by THIS chunk's pool-creation upsert: it
/// has no committed base state to read pre-transaction, so its planned map
/// (empty base + events) carries the sentinel `pool_id` 0 and the
/// in-transaction apply keys the delta to the id its upsert assigned.
#[derive(Debug, Clone)]
pub struct PlannedLiquidityMap {
    /// The FULL post-apply map the on-chain-truth gate consumes — the
    /// verifier's input contract is a complete map, and every plan is
    /// verified before its chunk commits. A chunk-new pool's persisted rows
    /// are exactly this map; an existing pool's are its event-touched
    /// projection.
    pub computed: ComputedLiquidityUpdate,
    /// The event-touched write set the apply persists: dirty
    /// survivors + drained keys, O(events) — the full map is verification
    /// input, the delta is the write set.
    pub delta: LiquidityDelta,
    /// `true` = chunk-new pool (the delta's sentinel `pool_id` 0 is keyed to
    /// the upserted row id before persist).
    pub is_new_pool: bool,
}

/// The read pass's output: per-pool planned maps for the chunk's in-scope
/// liquidity applies, computed BEFORE the chunk `Transaction` opens - the
/// O(map) full-map SELECTs leave the write lock.
/// The maps are then verified against on-chain truth pre-transaction (no lock
/// held across the RPC), and the apply persists only verified maps; a RED
/// never opens the transaction.
#[derive(Debug, Default, Clone)]
pub struct PreVerifiedLiquidity {
    /// V3 planned maps by pool address.
    pub v3: HashMap<Address, PlannedLiquidityMap>,
    /// V4 planned maps by `pool_hash` hex.
    pub v4: HashMap<String, PlannedLiquidityMap>,
}

/// What [`apply_chunk_writes_on_conn`] wrote in the chunk.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ChunkWriteReport {
    /// V2/V3/V4 pool-creation rows upserted.
    pub pools_written: usize,
    /// Per-pool liquidity applies (V3 + V4) that found + mutated a pool row.
    pub liquidity_apply_count: usize,
    /// Wall time of the per-pool full-map read + compute spans
    /// (`compute_v3/v4_liquidity_update_on_conn`).
    pub decode_compute_time: Duration,
    /// Wall time of the per-pool on-chain verification spans.
    pub verify_time: Duration,
    /// Wall time of the remaining SQL apply (the fn's total span minus the
    /// compute + verify spans).
    pub apply_time: Duration,
    /// The `last_update_block` value the chunk's stamps wrote (`chunk_end`
    /// through the same i64 clamp the stamp used) — the caller advances its
    /// in-memory markers from this instead of re-reading the DB post-commit
    /// (Perf E's dead-query removal). Meaningful only on the `Ok` path: a
    /// stamp whose optimistic WHERE fires zero rows errors out before a
    /// caller could read this.
    pub stamped_last_update_block: i64,
}

/// An error from [`map_pool_creation`] — the pure decode→row-input mapping.
#[derive(Debug, thiserror::Error)]
pub enum MapError {
    /// An Aerodrome V2 pool whose per-pool `getFee(address,bool)` RPC fee
    /// wasn't supplied. The outer loop MUST resolve the fee (an on-chain call)
    /// before mapping; passing `None` for `aerodrome_fee` yields this error so
    /// no pool is silently written with a fake/zero fee. The outer loop skips
    /// these pools (reporting via [`ProgressSink`]) until the fee-RPC path is
    /// wired (a follow-up; canonical V2/V3/V4 + Aerodrome V3 fees come from the
    /// event or a constant + are NOT affected).
    #[error(
        "Aerodrome V2 pool {pool_address} needs a per-pool getFee RPC resolution \
         (aerodrome_fee is None); the outer loop must resolve it before mapping"
    )]
    AerodromeFeeNeeded {
        /// The pool whose fee couldn't be resolved.
        pool_address: Address,
    },
}

/// Map a decoded `PoolCreated`/`Initialize` event to its DB `V*PoolRowInput`,
/// carrying the `exchange_id` context (which [`DecodedPoolCreated`] correctly
/// avoids — it's decode-pure). Pure (no RPC) EXCEPT the Aerodrome V2 fee,
/// which the caller supplies via `aerodrome_fee` (resolved by the outer loop's
/// on-chain `getFee(address, stable)` call — `None` only for non-Aerodrome V2
/// families, where this arg is ignored).
///
/// # Errors
///
/// Returns [`MapError::AerodromeFeeNeeded`] if the event is an Aerodrome V2
/// pool + `aerodrome_fee` is `None` (the caller forgot to resolve the fee).
pub fn map_pool_creation(
    spec: &ExchangeSpec,
    event: &DecodedPoolCreated,
    aerodrome_fee: Option<i64>,
) -> Result<PoolCreationToWrite, MapError> {
    match event {
        DecodedPoolCreated::V2(e) => {
            // Canonical V2: constant fee from the spec's `v2_fee_token`
            // (uniswap_v2: 3, pancakeswap_v2: 25, sushiswap_v2/swapbased_v2: 3).
            let fee = spec.v2_fee_token.unwrap_or(0);
            Ok(PoolCreationToWrite::V2 {
                exchange_id: spec.id,
                row: V2PoolRowInput {
                    address: e.pool_address,
                    token0_address: e.token0,
                    token1_address: e.token1,
                    fee_token0: fee,
                    fee_token1: fee,
                    stable: None,
                },
            })
        }
        DecodedPoolCreated::AerodromeV2(e) => {
            // Aerodrome V2: per-pool fee from on-chain `getFee(address, stable)`
            // (NOT a constant + NOT in the event). The outer loop resolves it.
            let fee = aerodrome_fee.ok_or(MapError::AerodromeFeeNeeded {
                pool_address: e.pool_address,
            })?;
            Ok(PoolCreationToWrite::V2 {
                exchange_id: spec.id,
                row: V2PoolRowInput {
                    address: e.pool_address,
                    token0_address: e.token0,
                    token1_address: e.token1,
                    fee_token0: fee,
                    fee_token1: fee,
                    stable: Some(e.stable),
                },
            })
        }
        DecodedPoolCreated::V3(e) => {
            // V3 fee + tick_spacing come from the event (applies to Uniswap V3,
            // PancakeSwap V3, SushiSwap V3, AND Aerodrome V3 — they all reuse
            // the V3 decode + carry fee/tick_spacing in the event).
            Ok(PoolCreationToWrite::V3 {
                exchange_id: spec.id,
                row: V3PoolRowInput {
                    address: e.pool_address,
                    token0_address: e.token0,
                    token1_address: e.token1,
                    fee: i64::from(e.fee),
                    tick_spacing: i64::from(e.tick_spacing),
                },
            })
        }
        DecodedPoolCreated::V4(e) => Ok(PoolCreationToWrite::V4 {
            exchange_id: spec.id,
            row: V4PoolRowInput {
                pool_hash: B256::from(e.pool_id).to_string(),
                hooks: e.hooks,
                currency0_address: e.currency0,
                currency1_address: e.currency1,
                fee: i64::from(e.fee),
                tick_spacing: i64::from(e.tick_spacing),
            },
        }),
    }
}

/// The testable, pure-synchronous inner core: write a chunk's worth of
/// decoded events (pool creations + V3/V4 liquidity) + stamp the in-scope
/// exchanges' `last_update_block` - ALL under the ONE borrowed
/// [`Connection`] (the chunk's `Transaction`).
///
/// This is the chunk-atomicity invariant's enforcement point: every write of
/// the chunk goes through here on ONE connection, + the caller's `Transaction`
/// commit/rollback is the single point of durability. Any `?` early-return
/// (a `UNIQUE` violation, a decode failure, ...) leaves the caller's
/// `Transaction` uncommitted → it drops → the whole chunk reverts →
/// `last_update_block` unchanged → restart re-processes (restart-invariant).
///
/// # The RPC and the map compute live OUTSIDE this fn
///
/// The per-pool map compute + the on-chain-truth verification RPC run
/// in the pre-transaction read pass ([`compute_preverified_liquidity`] plus
/// the driver's gate), NOT here: zero RPC round trips (and none of the
/// per-pool read SELECTs for planned pools) sit between the caller's
/// `transaction()` open and its commit/drop. Every planned pool persists the
/// plan's DELTA (only the event-touched keys — dirty survivors upserted,
/// drained keys deleted by key, or the drained-pool complement delete-all when
/// the verified map is empty): one derivation per pool, so a gated run
/// persists exactly what the gate verified. A chunk-new plan carries the
/// sentinel `pool_id` 0 and the apply keys it to the id its own upsert
/// assigned. The fused dirty compute survives ONLY as the no-plan fallback
/// seam (the atomicity contract's `run_chunk`), where the just-upserted empty
/// row makes the dirty map the whole map. The stamp is
/// OPTIMISTIC (the assumed marker rides the WHERE clause) so the restart
/// invariant is structural: a moved marker changes zero rows →
/// [`RunError::MarkerMoved`] → the caller drops the transaction and re-loops.
///
/// # The V3 liquidity in-scope filter
///
/// The whole-chain V3 scan returns pools across ALL V3 exchanges. The
/// in-scope set is `specs_to_update` (the chunk's laggard subset). The
/// read pass applies the SAME filter pre-transaction; pools it filtered
/// out (unknown or out-of-scope) are skipped here with no in-transaction
/// fetch - their scope fetch moved pre-`BEGIN` with the rest of the pass.
///
/// # Errors
///
/// Returns [`DbError`](degenbot_db::DbError) on any write/query failure - the
/// caller drops the `Transaction` (rollback) on `Err`. A stamp whose assumed
/// marker no longer matches surfaces as [`RunError::MarkerMoved`] (also a
/// rollback - the caller re-plans the chunk from refreshed specs).
#[expect(clippy::too_many_lines)]
pub fn apply_chunk_writes_on_conn(
    conn: &Connection,
    chain_id: i64,
    specs_to_update: &[ExchangeSpec],
    chunk_end: u64,
    inputs: &ChunkInputs,
    preverified: &PreVerifiedLiquidity,
) -> Result<ChunkWriteReport, RunError> {
    let fn_started = Instant::now();
    let mut report = ChunkWriteReport::default();

    // 1. Pool creations - group by (exchange_id, family) + upsert per batch.
    upsert_pool_creations_on_conn(
        conn,
        chain_id,
        specs_to_update,
        &inputs.pool_creations,
        &mut report,
    )?;

    // 2. V3 liquidity - in-scope filter per pool, then per-pool apply.
    //    The whole-chain scan grouped by emitter; the read pass already
    //    kept only in-scope pools (and computed their maps pre-transaction).
    //    Deterministic per-pool order: the map iteration is hashbrown-random,
    //    but each pool issues its own statements (the row-id read + persist on
    //    the planned path, the scope-filter fetch on the fallback path), so
    //    the chunk apply's statement sequence - and the row ids its upserts
    //    assign - must not depend on it (golden-capture replay, ADR-068
    //    D3/D6).
    let mut v3_pools: Vec<(Address, &Vec<LiquidityUpdateEvent>)> =
        inputs.v3_liquidity.iter().map(|(a, e)| (*a, e)).collect();
    v3_pools.sort_by_key(|(a, _)| *a);
    for (pool_address, events) in &v3_pools {
        // A planned pool persists its verified map's delta: the read pass's
        // compute SELECTs ran before `BEGIN`, not under the write lock, and
        // (gated) the map is the one the verify RPC checked.
        if let Some(plan) = preverified.v3.get(pool_address) {
            let keyed;
            let delta = if plan.is_new_pool {
                keyed =
                    plan.delta
                        .with_pool_id(chunk_new_v3_pool_id(conn, chain_id, *pool_address)?);
                &keyed
            } else {
                &plan.delta
            };
            DegenbotDb::persist_v3_liquidity_delta_on_conn(conn, delta)?;
            report.liquidity_apply_count += 1;
            continue;
        }
        if in_scope_v3_creation_tick_spacing(pool_address, specs_to_update, &inputs.pool_creations)
            .is_none()
        {
            // Not planned and not created by this chunk: the read pass already
            // scope-filtered this pool (its fetch ran pre-transaction) and
            // found it unknown or out-of-scope - skip with no in-transaction
            // fetch (the fetch moved pre-`BEGIN`).
            continue;
        }
        // The no-plan fallback seam (the atomicity contract's `run_chunk`):
        // the row exists - the upsert above created it - so the dirty compute
        // over the just-upserted empty row is the whole map.
        let pool = DegenbotDb::fetch_pool_by_address_on_conn(conn, *pool_address, chain_id)?;
        let in_scope = pool
            .as_ref()
            .and_then(|r| specs_to_update.iter().find(|s| s.id == r.exchange_id))
            .is_some();
        if !in_scope {
            continue; // pool belongs to an exchange not being updated this chunk
        }
        let compute_started = Instant::now();
        // The fallback seam's dirty compute — the base is the just-upserted
        // EMPTY row, so the dirty read (the touched keys' rows) is the whole
        // read and the delta is the whole write set. Byte-equal to the plan's
        // derivation (same empty base, same events, same spacing).
        let Some(delta) = DegenbotDb::compute_v3_liquidity_delta_on_conn(
            conn,
            chain_id,
            &pool_address.to_checksum(None),
            events,
        )?
        else {
            report.decode_compute_time += compute_started.elapsed();
            continue;
        };
        report.decode_compute_time += compute_started.elapsed();
        DegenbotDb::persist_v3_liquidity_delta_on_conn(conn, &delta)?;
        report.liquidity_apply_count += 1;
    }

    // 3. V4 liquidity - per-pool apply (the V4 fetch is per-PoolManager,
    //    already per-exchange-scoped). Same split as the V3 loop.
    //    Deterministic per-pool order (same reason as the V3 loop above).
    let mut v4_pools: Vec<(&String, &Vec<LiquidityUpdateEvent>)> =
        inputs.v4_liquidity.iter().collect();
    v4_pools.sort_by_key(|(h, _)| *h);
    for (pool_hash, events) in v4_pools {
        if let Some(plan) = preverified.v4.get(pool_hash) {
            let keyed;
            let delta = if plan.is_new_pool {
                keyed = plan.delta.with_pool_id(chunk_new_v4_pool_id(
                    conn,
                    pool_hash,
                    inputs.pool_manager_chain,
                )?);
                &keyed
            } else {
                &plan.delta
            };
            DegenbotDb::persist_v4_liquidity_delta_on_conn(conn, delta)?;
            report.liquidity_apply_count += 1;
            continue;
        }
        if in_scope_v4_creation_tick_spacing(pool_hash, specs_to_update, &inputs.pool_creations)
            .is_none()
        {
            // Not planned and not created by this chunk - the read pass
            // filtered it (its state fetch ran pre-transaction).
            continue;
        }
        let compute_started = Instant::now();
        // The fallback seam's dirty compute (the V4 mirror of the V3 arm
        // above).
        let Some(delta) = DegenbotDb::compute_v4_liquidity_delta_on_conn(
            conn,
            pool_hash,
            inputs.pool_manager_chain,
            events,
        )?
        else {
            report.decode_compute_time += compute_started.elapsed();
            continue;
        };
        report.decode_compute_time += compute_started.elapsed();
        DegenbotDb::persist_v4_liquidity_delta_on_conn(conn, &delta)?;
        report.liquidity_apply_count += 1;
    }

    // 4. Stamp `last_update_block` for each in-scope exchange - the LAST
    //    write in the transaction (the section-1 restart-invariant: on
    //    rollback the stamp does NOT advance, so a restart re-processes the
    //    chunk). The stamp is OPTIMISTIC - it carries the marker the
    //    read pass assumed, so a marker that moved under the chunk
    //    (impossible under the bot's single-writer discipline; the check
    //    makes the invariant structural rather than argued) changes ZERO rows
    //    and surfaces `RunError::MarkerMoved` - the caller drops the tx and
    //    re-plans the chunk.
    let chunk_end_i64 = i64::try_from(chunk_end).unwrap_or(i64::MAX);
    for spec in specs_to_update {
        let fired = DegenbotDb::set_exchange_last_update_block_if_unchanged_on_conn(
            conn,
            chain_id,
            spec.id,
            chunk_end_i64,
            spec.last_update_block,
        )?;
        if !fired {
            return Err(RunError::MarkerMoved {
                exchange_id: spec.id,
            });
        }
    }
    // Every stamp above fired (a zero-row stamp returned early), so the
    // committed marker value is exactly `chunk_end_i64` for each chunk spec
    // — carried out on the report so the caller never re-reads the DB for it
    // (Perf E).
    report.stamped_last_update_block = chunk_end_i64;

    // The remaining in-transaction wall time is the SQL apply (upserts, the
    // planned persists, the fused-path computes, the stamp) - the fn's span
    // minus the measured in-transaction compute spans (the read pass's
    // compute time is reported by the driver separately).
    report.apply_time = fn_started
        .elapsed()
        .saturating_sub(report.decode_compute_time);

    Ok(report)
}

/// Group [`PoolCreationToWrite`]s by (`exchange_id`, family) + upsert each batch
/// bound to the chunk's transaction. V2 + Aerodrome V2 share the V2 row shape;
/// V3 (incl. Aerodrome V3) + V4 are distinct. Pools for exchanges not in the
/// chunk's in-scope set are skipped (defensive — the outer loop filters too).
fn upsert_pool_creations_on_conn(
    conn: &Connection,
    chain_id: i64,
    specs_to_update: &[ExchangeSpec],
    pool_creations: &[PoolCreationToWrite],
    report: &mut ChunkWriteReport,
) -> Result<(), degenbot_db::DbError> {
    let mut v2_by_exchange: HashMap<i64, (String, i64, Vec<V2PoolRowInput>)> = HashMap::new();
    let mut v3_by_exchange: HashMap<i64, (String, i64, Vec<V3PoolRowInput>)> = HashMap::new();
    let mut v4_by_exchange: HashMap<i64, (String, i64, Vec<V4PoolRowInput>)> = HashMap::new();
    for creation in pool_creations {
        let Some(spec) = specs_to_update
            .iter()
            .find(|s| s.id == exchange_id_of(creation))
        else {
            continue;
        };
        match creation {
            PoolCreationToWrite::V2 { exchange_id, row } => {
                v2_by_exchange
                    .entry(*exchange_id)
                    .or_insert_with(|| (spec.name.clone(), spec.fee_denominator, Vec::new()))
                    .2
                    .push(row.clone());
            }
            PoolCreationToWrite::V3 { exchange_id, row } => {
                v3_by_exchange
                    .entry(*exchange_id)
                    .or_insert_with(|| (spec.name.clone(), spec.fee_denominator, Vec::new()))
                    .2
                    .push(row.clone());
            }
            PoolCreationToWrite::V4 { exchange_id, row } => {
                v4_by_exchange
                    .entry(*exchange_id)
                    .or_insert_with(|| (spec.name.clone(), spec.fee_denominator, Vec::new()))
                    .2
                    .push(row.clone());
            }
        }
    }
    for (exchange_id, (kind, fee_denominator, rows)) in v2_by_exchange {
        DegenbotDb::upsert_v2_pools_on_conn(
            conn,
            chain_id,
            &kind,
            exchange_id,
            fee_denominator,
            &rows,
        )?;
        report.pools_written += rows.len();
    }
    for (exchange_id, (kind, fee_denominator, rows)) in v3_by_exchange {
        DegenbotDb::upsert_v3_pools_on_conn(
            conn,
            chain_id,
            &kind,
            exchange_id,
            fee_denominator,
            &rows,
        )?;
        report.pools_written += rows.len();
    }
    for (exchange_id, (_, _, rows)) in v4_by_exchange {
        // V4 upserts `uniswap_v4_pools` keyed by the manager address (the
        // spec's `factory`), so the collected kind and denominator go unread:
        // V4 is `uniswap_v4` only, at the protocol's `1_000_000` denominator.
        DegenbotDb::upsert_v4_pools_on_conn(
            conn,
            chain_id,
            &specs_to_update
                .iter()
                .find(|s| s.id == exchange_id)
                .map(|s| s.factory.to_checksum(None))
                .unwrap_or_default(),
            1_000_000,
            &rows,
        )?;
        report.pools_written += rows.len();
    }
    Ok(())
}

/// Extract the `exchange_id` from a [`PoolCreationToWrite`] (a small helper
/// to keep the in-scope lookup in [`apply_chunk_writes_on_conn`] readable).
fn exchange_id_of(creation: &PoolCreationToWrite) -> i64 {
    match creation {
        PoolCreationToWrite::V2 { exchange_id, .. }
        | PoolCreationToWrite::V3 { exchange_id, .. }
        | PoolCreationToWrite::V4 { exchange_id, .. } => *exchange_id,
    }
}

// ── chunk in-scope / per-exchange fetch-range decision ────────────────────

/// Decide whether a spec is in-scope for the chunk `[working_start,
/// working_end]` given its stored `last_update_block`, and return the
/// per-exchange effective fetch start so a divergent-ahead spec does NOT
/// re-fetch its already-committed range.
///
/// Returns `None` when the spec is already past this chunk's end (nothing to
/// do); otherwise `Some(fetch_start)` where `fetch_start` is
/// `max(marker + 1, working_start)` — the spec's own unprocessed cursor,
/// clamped to the chunk's lower bound so a behind-spec still scans the whole
/// chunk.
///
/// This replaces a strict `marker + 1 == working_start` equality filter that
/// silently stranded any exchange whose marker diverged from the chunk grid
/// (the ahead-exchange still has unprocessed work in the chunk's tail, but the
/// equality test refused to admit it). See the V4 exclusion bug: V3 forks
/// stalled at `M` while V2/V4 advanced to `M + k`; once V3 rooted the cursor at
/// `M + 1`, V2/V4 (`marker + 1 = M + k + 1 ≠ M + 1`) were dropped from
/// `chunk_specs` for every subsequent chunk, so their `ModifyLiquidity` /
/// `PoolCreated` events in `[M + k + 1, tip]` were never fetched.
fn in_scope_fetch_start(
    spec_last_update: Option<i64>,
    working_start: u64,
    working_end: u64,
) -> Option<u64> {
    let marker = spec_last_update.map_or(0, |b| u64::try_from(b).unwrap_or(0));
    if marker >= working_end {
        return None;
    }
    // The spec has unprocessed work at or before `working_end`. Use its own
    // marker + 1 as the fetch start (so a divergent-ahead spec does NOT
    // re-fetch its committed range — re-applying its `ModifyLiquidity` /
    // `Mint`/`Burn` events would double-count liquidity in release builds,
    // where the per-pool event-order guard is a stripped `debug_assert!`).
    // Clamp to `working_start` so a behind-spec scans only the chunk's range.
    Some((marker + 1).max(working_start))
}

/// The tick spacing of an in-scope V3 `PoolCreated` row for `pool_address`,
/// when this chunk's creations upsert it (the chunk-new-pool classifier:
/// the read pass and the apply must agree on which pools the chunk creates -
/// a pool both planned and created would double-persist).
fn in_scope_v3_creation_tick_spacing(
    pool_address: &Address,
    specs_to_update: &[ExchangeSpec],
    pool_creations: &[PoolCreationToWrite],
) -> Option<i64> {
    pool_creations.iter().find_map(|c| match c {
        PoolCreationToWrite::V3 { exchange_id, row }
            if row.address == *pool_address
                && specs_to_update.iter().any(|s| s.id == *exchange_id) =>
        {
            Some(row.tick_spacing)
        }
        _ => None,
    })
}

/// The V4 twin of [`in_scope_v3_creation_tick_spacing`] (keyed by `pool_hash`).
fn in_scope_v4_creation_tick_spacing(
    pool_hash: &str,
    specs_to_update: &[ExchangeSpec],
    pool_creations: &[PoolCreationToWrite],
) -> Option<i64> {
    pool_creations.iter().find_map(|c| match c {
        PoolCreationToWrite::V4 { exchange_id, row }
            if row.pool_hash == pool_hash
                && specs_to_update.iter().any(|s| s.id == *exchange_id) =>
        {
            Some(row.tick_spacing)
        }
        _ => None,
    })
}

/// The `pools.id` this chunk's V3 pool upsert assigned — the key a chunk-new
/// plan's delta persists under (the plan itself carries the sentinel 0: no row
/// existed when it was computed).
fn chunk_new_v3_pool_id(
    conn: &Connection,
    chain_id: i64,
    pool_address: Address,
) -> Result<i64, RunError> {
    DegenbotDb::fetch_pool_by_address_on_conn(conn, pool_address, chain_id)?
        .map(|row| row.id)
        .ok_or_else(|| {
            RunError::Db(degenbot_db::DbError::Decode(format!(
                "chunk-new pool {pool_address} has no row after this chunk's upsert"
            )))
        })
}

/// The V4 twin of [`chunk_new_v3_pool_id`]: the `managed_pools.id` this
/// chunk's V4 upsert assigned.
fn chunk_new_v4_pool_id(
    conn: &Connection,
    pool_hash: &str,
    pool_manager_chain: i64,
) -> Result<i64, RunError> {
    DegenbotDb::fetch_v4_pool_update_state_on_conn(conn, pool_hash, pool_manager_chain)?
        .map(|state| state.pool_id)
        .ok_or_else(|| {
            RunError::Db(degenbot_db::DbError::Decode(format!(
                "chunk-new pool {pool_hash} has no managed-pool row after this chunk's upsert"
            )))
        })
}

/// The pre-transaction read pass: compute every in-scope
/// pool's post-chunk liquidity state BEFORE the chunk `Transaction` opens.
/// The per-pool map SELECTs (the baseline's in-lock compute) and the per-pool
/// scope fetches run on the plain guarded connection - a WAL reader takes no
/// write lock - and the plans come back owned by the caller, so the verify
/// RPC and the apply's persists run with the write lock free.
///
/// **The map read is the FULL map.** The per-chunk gate's contract is a
/// complete map vs chain, so every existing in-scope pool's read is the
/// full-map reconstitution + apply (`compute_v3/v4_liquidity_update_on_conn`)
/// and the delta is derived from it in memory (the projection is lossless:
/// the apply loop only ever mutates the event-touched keys).
///
/// Per V3 pool (address-sorted): a chunk-new pool (this chunk's creations
/// upsert it) has no committed base, so its planned map (empty base + events)
/// is what feeds the pre-transaction verify and what the apply persists (keyed
/// to the id its own upsert assigns).
/// An existing in-scope pool is scope-fetched + computed exactly as the
/// in-transaction path did - same SELECTs, moved out of the lock.
/// Unknown/out-of-scope pools are dropped here (the apply skips them without
/// re-fetching).
fn compute_preverified_liquidity(
    db: &DegenbotDb,
    chain_id: i64,
    chunk_specs: &[ExchangeSpec],
    inputs: &ChunkInputs,
) -> Result<PreVerifiedLiquidity, degenbot_db::DbError> {
    let mut preverified = PreVerifiedLiquidity::default();
    let guard = db.lock();
    let conn = &*guard;

    let mut v3_pools: Vec<(Address, &Vec<LiquidityUpdateEvent>)> =
        inputs.v3_liquidity.iter().map(|(a, e)| (*a, e)).collect();
    v3_pools.sort_by_key(|(a, _)| *a);
    for (pool_address, events) in &v3_pools {
        if let Some(tick_spacing) =
            in_scope_v3_creation_tick_spacing(pool_address, chunk_specs, &inputs.pool_creations)
        {
            // Chunk-new pool: no committed base to read. The planned map
            // (empty base + events, sentinel `pool_id`) is what the
            // pre-transaction verify RPC checks and what the apply persists,
            // keyed to the id its upsert assigns.
            // The creation row carries the event's tick spacing (i64); the
            // map math is i32-keyed. A spacing outside i32 is a decode-
            // level impossibility - fail loud, never clamp.
            let spacing = i32::try_from(tick_spacing).map_err(|e| {
                degenbot_db::DbError::Decode(format!(
                    "pool {pool_address} creation tick_spacing {tick_spacing} out of i32: {e}"
                ))
            })?;
            let computed = DegenbotDb::compute_v3_liquidity_update_for_new_pool(spacing, events);
            let delta = derive_liquidity_delta_from_computed(&computed, events);
            preverified.v3.insert(
                *pool_address,
                PlannedLiquidityMap {
                    computed,
                    delta,
                    is_new_pool: true,
                },
            );
            continue;
        }
        let Some(pool) = DegenbotDb::fetch_pool_by_address_on_conn(conn, *pool_address, chain_id)?
        else {
            continue; // unknown pool - the apply skips it without re-fetching
        };
        if !chunk_specs.iter().any(|s| s.id == pool.exchange_id) {
            continue; // out-of-scope - the apply skips it too
        }
        // The gate consumes the FULL map: the full-map read + compute, with
        // the delta derived from it in memory.
        let Some(c) = DegenbotDb::compute_v3_liquidity_update_on_conn(
            conn,
            chain_id,
            &pool_address.to_checksum(None),
            events,
        )?
        else {
            continue;
        };
        let delta = derive_liquidity_delta_from_computed(&c, events);
        preverified.v3.insert(
            *pool_address,
            PlannedLiquidityMap {
                computed: c,
                delta,
                is_new_pool: false,
            },
        );
    }

    let mut v4_pools: Vec<(&String, &Vec<LiquidityUpdateEvent>)> =
        inputs.v4_liquidity.iter().collect();
    v4_pools.sort_by_key(|(h, _)| *h);
    for (pool_hash, events) in v4_pools {
        if let Some(tick_spacing) =
            in_scope_v4_creation_tick_spacing(pool_hash, chunk_specs, &inputs.pool_creations)
        {
            let spacing = i32::try_from(tick_spacing).map_err(|e| {
                degenbot_db::DbError::Decode(format!(
                    "pool_hash {pool_hash} creation tick_spacing {tick_spacing} out of i32: {e}"
                ))
            })?;
            let computed = DegenbotDb::compute_v4_liquidity_update_for_new_pool(spacing, events);
            let delta = derive_liquidity_delta_from_computed(&computed, events);
            preverified.v4.insert(
                (*pool_hash).clone(),
                PlannedLiquidityMap {
                    computed,
                    delta,
                    is_new_pool: true,
                },
            );
            continue;
        }
        let Some(c) = DegenbotDb::compute_v4_liquidity_update_on_conn(
            conn,
            pool_hash,
            inputs.pool_manager_chain,
            events,
        )?
        else {
            continue; // unknown managed pool - the apply skips it too
        };
        let delta = derive_liquidity_delta_from_computed(&c, events);
        preverified.v4.insert(
            (*pool_hash).clone(),
            PlannedLiquidityMap {
                computed: c,
                delta,
                is_new_pool: false,
            },
        );
    }

    Ok(preverified)
}

/// The pre-transaction verify gate: compare each planned map against
/// on-chain truth at `block_number` with NO lock held (the read pass's guard
/// is long dropped by the time this runs). A non-empty divergence list
/// surfaces [`RunError::Verification`] - the chunk transaction never opens,
/// so the observable RED contract (error raised, zero rows written, stamp
/// unadvanced) holds with the RPC out of the lock. Per-pool order is the
/// read pass's sorted order (the in-transaction gate's relative order).
fn verify_precomputed_maps(
    provider: &AlloyProvider,
    rt: &'static tokio::runtime::Runtime,
    block_number: u64,
    preverified: &PreVerifiedLiquidity,
    inputs: &ChunkInputs,
) -> Result<(), RunError> {
    let mut v3_addrs: Vec<&Address> = preverified.v3.keys().collect();
    v3_addrs.sort();
    for addr in v3_addrs {
        let plan = &preverified.v3[addr];
        gate_liquidity_map_on_chain(
            rt,
            provider,
            &addr.to_checksum(None),
            LiquidityMapTarget::V3(*addr),
            &plan.computed,
            block_number,
        )?;
    }
    let mut v4_hashes: Vec<&String> = preverified.v4.keys().collect();
    v4_hashes.sort();
    for pool_hash in v4_hashes {
        let plan = &preverified.v4[pool_hash];
        let pool_manager_address = inputs
            .v4_manager_addresses
            .get(pool_hash)
            .copied()
            .ok_or_else(|| {
                RunError::Provider(ProviderError::DecodingError {
                    message: format!(
                        "v4 verify: no PoolManager address for pool_hash {pool_hash:?}"
                    ),
                })
            })?;
        let pool_id =
            B256::from_str(pool_hash.strip_prefix("0x").unwrap_or(pool_hash)).map_err(|e| {
                ProviderError::DecodingError {
                    message: format!("v4 verify: bad pool_hash {pool_hash:?}: {e}"),
                }
            })?;
        gate_liquidity_map_on_chain(
            rt,
            provider,
            pool_hash,
            LiquidityMapTarget::V4 {
                pool_manager: pool_manager_address,
                pool_id,
            },
            &plan.computed,
            block_number,
        )?;
    }
    Ok(())
}

// ── the outer chunk loop (RPC-bound; integration-tested) ─────────────────

/// The final report from a [`run_pool_update`] run.
#[derive(Debug, Default, Clone, Copy)]
pub struct UpdateReport {
    /// The chain advanced.
    pub chain_id: i64,
    /// The first block processed (inclusive).
    pub from_block: u64,
    /// The last block the run advanced every in-scope exchange to.
    pub to_block: u64,
    /// Total chunks committed.
    pub chunks_committed: usize,
    /// Total pools written across all chunks.
    pub total_pools_written: usize,
    /// Total per-pool liquidity applies across all chunks.
    pub total_liquidity_applies: usize,
}

/// An error from [`run_pool_update`].
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// A DB error (open / spec load / chunk write / commit).
    #[error("database error: {0}")]
    Db(#[from] degenbot_db::DbError),
    /// An RPC error (block tip / log fetch).
    #[error("rpc error: {0}")]
    Provider(#[from] ProviderError),
    /// A tokio runtime build failure.
    #[error("runtime error: {0}")]
    Runtime(#[from] std::io::Error),
    /// The run was cancelled via the `cancel` flag (cooperative, at a chunk
    /// boundary). The committed chunks are durable; the in-flight chunk (if
    /// any) was rolled back before returning.
    #[error("cancelled by cancel flag at chunk boundary")]
    Cancelled,
    /// The pre-commit on-chain-truth gate (Full per-pool verification) found
    /// divergences: the chunk's transaction was rolled back + the exchange's
    /// `last_update_block` was NOT advanced (the restart-invariant holds — the
    /// next run re-processes the same chunk). Carries the diverging pool's
    /// identifier + the verify-at block + the named divergence list (empty
    /// would be GREEN). Mirrors the aave `RunError::Verification`.
    #[error(
        "verification failed for pool {pool:?} at block {block_number} — see divergences field"
    )]
    Verification {
        /// The diverging pool's identifier — V3 checksummed address, V4 `pool_hash` hex.
        pool: String,
        /// The block number the on-chain truth was read at (= `chunk_end`).
        block_number: u64,
        /// The named, bisect-able divergences (empty = GREEN).
        divergences: Vec<LiquidityDivergence>,
    },
    /// The chunk's `last_update_block` marker moved between the
    /// pre-transaction read pass and the chunk transaction (the optimistic
    /// stamp matched zero rows). The chunk's staged writes were rolled back
    /// with its transaction; the driver refreshes the specs and re-plans the
    /// chunk. Unreachable under the bot's single-writer discipline - the
    /// check makes the restart invariant structural rather than argued.
    #[error(
        "exchange {exchange_id} marker moved under the chunk (optimistic stamp matched zero rows)"
    )]
    MarkerMoved {
        /// The exchange whose stored marker no longer matches the assumed one.
        exchange_id: i64,
    },
}

/// Run the pool-updater chunk loop for `chain_id`, advancing every active
/// exchange's `last_update_block` to `to_block` (or the chain tip if
/// `to_block` is `None`).
///
/// See the [module docs](self) for the chunk-atomicity invariants + the shared-runtime
/// constraint (D2: do NOT call from within any tokio runtime context).
///
/// # Arguments
///
/// - `database_path` — the writeable `DegenbotDb` path (must already be
///   migrated to the Rust-owned schema; the chunk loop does NOT migrate).
/// - `chain_id` — the chain to advance.
/// - `to_block` — `Some(n)` to advance to block `n`; `None` to advance to the
///   chain tip (resolved via `eth_blockNumber`).
/// - `chunk_size` — blocks per chunk (the batch the chunk's single
///   transaction covers).
/// - `max_block_range` — the widest block range a single `eth_getLogs`
///   request may span (the `MAX_BLOCKS_PER_REQUEST` clamp), independent
///   of `chunk_size`: a chunk wider than this is fetched as several
///   `max_block_range`-sized requests: the per-request clamp
///   (`fetch_logs_retrying`'s `max_blocks_per_request`),
///   so an operator can point the run at a public RPC with a tighter
///   `eth_getLogs` range limit without shrinking the commit chunk.
/// - `provider` — the already-built [`AlloyProvider`] the whole run is
///   driven over (ADR-068 D5: injection, not an internal build). The CLI and
///   Python shells construct the live transport from their `rpc_url` in a
///   thin wrapper; an offline replay constructs a cassette provider instead.
/// - `cancel` — set to `true` to cooperatively stop at the next chunk boundary.
/// - `progress` — the per-chunk progress sink (use [`NoProgress`] for silent).
///
/// Every run verifies: the pre-commit on-chain-truth gate runs over each
/// chunk's planned maps before its persist commits, and a divergence rolls
/// back the chunk + surfaces [`RunError::Verification`].
///
/// # Errors
///
/// Returns [`RunError`] on a DB/RPC failure (the in-flight chunk is rolled
/// back before returning — the committed chunks stay durable) or
/// [`RunError::Cancelled`] if the `cancel` flag was set.
#[expect(clippy::too_many_arguments)]
pub fn run_pool_update(
    database_path: &Path,
    chain_id: i64,
    to_block: Option<u64>,
    chunk_size: u64,
    max_block_range: u64,
    provider: AlloyProvider,
    cancel: Arc<AtomicBool>,
    progress: Arc<dyn ProgressSink>,
    // When `Some(n)`, run a pre-commit FULL (market-wide, all in-scope
    // pools) verification when a chunk crosses/lands-on a multiple of `n`
    // blocks. A divergence rolls back the chunk + does NOT advance
    // `last_update_block`. `None` = no interval gate. This is an ADDITIONAL
    // market-wide policy; the per-chunk gate always runs.
    verify_all_interval: Option<u64>,
    // When `true`, run a pre-commit FULL verification on the run's final
    // chunk (`working_end_block >= last_block`). A divergence rolls back
    // the chunk + does NOT advance `last_update_block`.
    verify_all_at_completion: bool,
) -> Result<UpdateReport, RunError> {
    if chunk_size == 0 || max_block_range == 0 {
        return Err(RunError::Provider(ProviderError::InvalidBlockRange {
            from: 1,
            to: 0,
        }));
    }

    // Open ONE writeable handle for the whole run — the chunk loop re-borrows
    // it per chunk (the `*_on_conn` variants take a `&Connection`, so the
    // chunk's `Transaction` borrows this handle's guarded connection).
    let (db, _schema_state) = DegenbotDb::open_for_writes(database_path)?;
    run_pool_update_on_db(
        &db,
        chain_id,
        to_block,
        chunk_size,
        max_block_range,
        provider,
        cancel,
        progress,
        verify_all_interval,
        verify_all_at_completion,
    )
}

/// The active specs still holding unprocessed blocks at or below `last_block`.
///
/// The driver re-reads this from the DB on a [`RunError::MarkerMoved`] retry:
/// the persisted stamps — not the dropped attempt's in-memory snapshot — are the
/// plan's authority, so the re-attempt's per-exchange fetch starts
/// ([`in_scope_fetch_start`]) come from the markers that actually moved.
fn unprocessed_specs(specs: &[ExchangeSpec], last_block: u64) -> Vec<ExchangeSpec> {
    specs
        .iter()
        .filter(|s| {
            s.last_update_block
                .is_none_or(|b| u64::try_from(b).unwrap_or(0) < last_block)
        })
        .cloned()
        .collect()
}

/// Pre-opened-handle variant of [`run_pool_update`] — behaviorally identical
/// (same chunk loop, same PRAGMA/open contract minus the open itself). The
/// statement-ledger golden machinery ([`degenbot_db::sql_ledger::LedgerDb`],
/// ADR-068 D3) needs to hand the run ITS traced connection, so the chunk loop
/// must accept a handle it did not open. [`run_pool_update`] opens then
/// delegates here.
///
/// # Errors
///
/// Same conditions as [`run_pool_update`].
#[expect(
    clippy::too_many_arguments,
    clippy::needless_pass_by_value,
    clippy::too_many_lines
)]
pub fn run_pool_update_on_db(
    db: &DegenbotDb,
    chain_id: i64,
    to_block: Option<u64>,
    chunk_size: u64,
    max_block_range: u64,
    provider: AlloyProvider,
    cancel: Arc<AtomicBool>,
    progress: Arc<dyn ProgressSink>,
    // When `Some(n)`, run a pre-commit FULL (market-wide, all in-scope
    // pools) verification when a chunk crosses/lands-on a multiple of `n`
    // blocks. A divergence rolls back the chunk + does NOT advance
    // `last_update_block`. `None` = no interval gate. This is an ADDITIONAL
    // market-wide policy; the per-chunk gate always runs.
    verify_all_interval: Option<u64>,
    // When `true`, run a pre-commit FULL verification on the run's final
    // chunk (`working_end_block >= last_block`). A divergence rolls back
    // the chunk + does NOT advance `last_update_block`.
    verify_all_at_completion: bool,
) -> Result<UpdateReport, RunError> {
    if chunk_size == 0 || max_block_range == 0 {
        return Err(RunError::Provider(ProviderError::InvalidBlockRange {
            from: 1,
            to: 0,
        }));
    }
    let specs = load_active_exchange_specs(db, chain_id)?;
    if specs.is_empty() {
        // Nothing to do — return a trivial report (no chunks, no advance).
        return Ok(UpdateReport {
            chain_id,
            from_block: 0,
            to_block: to_block.unwrap_or(0),
            ..Default::default()
        });
    }

    // The SHARED process runtime (D2 — degenbot_core::runtime::get_runtime(),
    // the `&'static` singleton). The fetches ride it; the DB writes are
    // synchronous. block_on here is legal on the bare `PyO3` fleet workers +
    // the CLI main thread (no ambient tokio context); it panics when called
    // from within any tokio runtime context.
    let rt = degenbot_core::runtime::get_runtime();
    // ONE transport for the whole chunk loop: the injected provider serves
    // the fetches AND both pre-commit verification gates (the per-chunk map
    // gate and `PreCommitFullVerifyCtx`), so its connection tasks live
    // exactly as long as the run. The core never builds a transport (ADR-068
    // D5) — the caller owns the construction (live from its rpc_url, or a
    // cassette replay transport for an offline run).
    let provider = Arc::new(provider);
    // The RPC request width is the `max_block_range` clamp, NOT the commit
    // chunk: a chunk wider than the clamp is fetched as several
    // `max_block_range`-sized `eth_getLogs` requests, while the commit stays
    // `chunk_size`-wide. The per-request clamp
    // (`fetch_logs_retrying`'s `max_blocks_per_request`) is decoupled from its
    // `--chunk` commit granularity.
    let fetcher = LogFetcher::new(provider.clone(), max_block_range);

    // Resolve the chain tip if `to_block` is None.
    let last_block = match to_block {
        Some(n) => n,
        None => rt.block_on(provider.get_block_number())?,
    };

    // `initial_start_block` = the minimum `last_update_block + 1` across the
    // active specs (the earliest block any exchange still needs). Exchanges
    // whose `last_update_block` is already at/`> to_block` are excluded.
    let mut specs_to_update = unprocessed_specs(&specs, last_block);
    if specs_to_update.is_empty() {
        return Ok(UpdateReport {
            chain_id,
            from_block: last_block,
            to_block: last_block,
            ..Default::default()
        });
    }
    let initial_start_block = specs_to_update
        .iter()
        .map(|s| {
            s.last_update_block
                .map_or(1, |b| u64::try_from(b).unwrap_or(0) + 1)
        })
        .min()
        .unwrap_or(1)
        .max(1);

    let mut report = UpdateReport {
        chain_id,
        from_block: initial_start_block,
        to_block: last_block,
        ..Default::default()
    };

    let mut working_start_block = initial_start_block;
    let mut last_progress_log = Instant::now();
    // The optimistic stamp's retry budget (the structural marker check).
    // Unreachable under the bot's single-writer discipline; a bound keeps a
    // pathological flip from spinning the loop. NOT reset on commit: four
    // marker moves per RUN is already far past any real contention shape.
    let mut marker_retries = 0usize;
    while working_start_block <= last_block {
        // Cooperative cancel at the chunk boundary (the most recent committed
        // chunk is durable; we haven't started the next chunk's writes yet).
        if cancel.load(Ordering::Acquire) {
            return Err(RunError::Cancelled);
        }

        let working_end_block = last_block.min(working_start_block + chunk_size - 1);

        // The chunk's trace ROOT (the `degenbot.epoch` shape): every stage
        // span below parents onto it through the thread-local span stack, so
        // one chunk = one Jaeger waterfall. PASSIVE telemetry: the guard only
        // carries span context — the loop body below is untouched, so the
        // statement ledger, the RPC request stream, and the stamp contract
        // (the three golden gates) cannot move.
        let chunk_span = tracing::info_span!(
            "degenbot.updater.pool.chunk",
            chain.id = chain_id,
            chunk.start = working_start_block,
            chunk.end = working_end_block,
        );
        let _chunk_guard = chunk_span.enter();
        // The JSON-RPC round trips this chunk's stages issue (the production
        // twin of the cassette gates' round-trip counters). The count is
        // process-wide (see `degenbot_rpc::provider::rpc_round_trips`); under
        // the single-writer one-shot updater processes this telemetry
        // targets, the counted transport IS this run's.
        let chunk_rpc_round_trips_start = degenbot_rpc::provider::rpc_round_trips();

        // The in-scope subset for THIS chunk + each spec's per-exchange fetch
        // start. A spec is in-scope iff it still has unprocessed work at or
        // before `working_end_block`; its fetch start is `max(marker + 1,
        // working_start_block)` — its own unprocessed cursor clamped to the
        // chunk's lower bound, so a divergent-ahead spec (advanced in earlier
        // committed chunks while a laggard rooted the cursor) does NOT re-fetch
        // its committed range (re-applying `ModifyLiquidity` / `Mint`/`Burn`
        // events would double-count liquidity in release builds, where the
        // per-pool event-order guard is a stripped `debug_assert!`).
        let chunk_specs_with_start: Vec<(ExchangeSpec, u64)> = specs_to_update
            .iter()
            .filter_map(|s| {
                in_scope_fetch_start(s.last_update_block, working_start_block, working_end_block)
                    .map(|start| (s.clone(), start))
            })
            .collect();
        let chunk_specs: Vec<ExchangeSpec> = chunk_specs_with_start
            .iter()
            .map(|(s, _)| s.clone())
            .collect();
        if chunk_specs.is_empty() {
            // No exchange has unprocessed work for this chunk — advance the
            // cursor (guards against a sparse `last_update_block` distribution).
            working_start_block = working_end_block + 1;
            continue;
        }

        // The replay-bench stage spans (plain `Instant`; the telemetry chunk lands
        // its own spans later). `chunk_lock_hold_time` is the write-lock
        // hold: `transaction()` open → commit/drop.
        let mut chunk_fetch_time = Duration::ZERO;
        let mut chunk_verify_time = Duration::ZERO;
        let mut chunk_full_verify_time = Duration::ZERO;
        // Assigned (then read) in every branch that reaches a report; left
        // uninitialized so the compiler proves that.
        let chunk_lock_hold_time;

        // RPC fetches (GIL-free, async) — pool creations per in-scope exchange +
        // the V3 whole-chain + V4 per-PoolManager liquidity scans for the range.
        // Perf E: the three phases run concurrently over the ONE shared
        // `LogFetcher` (tokio::join!) instead of three sequential `block_on`
        // spans. The request set is exactly the sequential shape's (same
        // filters, same per-phase chunking) — only the concurrency changes,
        // so the replay round-trip/byte counters must not move. Results are
        // consumed in the sequential order (creations, V3, V4) so the first
        // surfaced error keeps its precedence, loudly (no result is dropped
        // or defaulted when a sibling phase fails).
        // The fetch stage span wraps the SAME region the plain-`Instant`
        // `fetch_started` anchor times (the replay-driven baseline): the
        // three overlapped RPC phases ride ONE span (summing per-phase spans
        // would double-count wall time that runs concurrently — Perf E).
        let fetch_span = tracing::info_span!(
            "degenbot.updater.pool.fetch",
            rpc.round_trips = tracing::field::Empty,
        );
        let fetch_rt_start = degenbot_rpc::provider::rpc_round_trips();
        let fetch_guard = fetch_span.enter();
        let fetch_started = Instant::now();
        let (pool_creations, v3_liquidity, v4_fetched) = rt.block_on(async {
            tokio::join!(
                fetch_pool_creations(&fetcher, working_end_block, &chunk_specs_with_start),
                // V3 `Mint`/`Burn` events can't be efficiently RPC-filtered by
                // pool address, so this is a whole-chain scan over the chunk's
                // range. The apply step (in `apply_chunk_writes_on_conn`)
                // drops pools whose `exchange_id` is NOT in `chunk_specs`.
                // (Limitation: if a V3 fork ever diverges AHEAD of the grid,
                // this whole-chain range would re-fetch its committed events —
                // currently all V3 forks advance in lockstep at the laggard
                // marker, so the case does not arise.)
                fetch_v3_liquidity_logs_grouped(
                    &fetcher,
                    working_start_block,
                    working_end_block,
                    None, // whole-chain (V3 events can't be efficiently RPC-filtered)
                ),
                // V4 liquidity: fetch per PoolManager (the V4 exchange's
                // `factory` is the PoolManager address), scoped to each spec's
                // per-exchange fetch start. Merge across all V4 chunk-specs
                // into one grouped map (the apply is per-pool_hash,
                // manager-chain-scoped). The merge loop stays sequential —
                // the map order the apply sees must not depend on fetch
                // timing.
                async {
                    let mut v4_liquidity: HashMap<String, Vec<LiquidityUpdateEvent>> =
                        HashMap::new();
                    let mut v4_manager_addresses: HashMap<String, Address> = HashMap::new();
                    for (spec, fetch_start) in &chunk_specs_with_start {
                        if matches!(spec.family, crate::fetch::PoolFamily::V4) {
                            let per_manager = fetch_v4_liquidity_logs_grouped(
                                &fetcher,
                                *fetch_start,
                                working_end_block,
                                Some(spec.factory),
                            )
                            .await?;
                            for (hash, events) in per_manager {
                                v4_manager_addresses
                                    .entry(hash.clone())
                                    .or_insert(spec.factory);
                                v4_liquidity.entry(hash).or_default().extend(events);
                            }
                        }
                    }
                    // The error type is pinned explicitly: `RunError` carries
                    // several `From` impls, so inference cannot pick one.
                    Ok::<
                        (
                            HashMap<String, Vec<LiquidityUpdateEvent>>,
                            HashMap<String, Address>,
                        ),
                        ProviderError,
                    >((v4_liquidity, v4_manager_addresses))
                },
            )
        });
        // Consume the joined results in the sequential error precedence
        // (creations, V3, V4) — every failure surfaces, none is swallowed.
        let pool_creations = pool_creations?;
        let v3_liquidity = v3_liquidity?;
        let (v4_liquidity, v4_manager_addresses) = v4_fetched?;
        // One span for the overlapped phases: summing per-phase spans would
        // double-count wall time that now runs concurrently.
        chunk_fetch_time += fetch_started.elapsed();
        drop(fetch_guard);
        fetch_span.record(
            "rpc.round_trips",
            degenbot_rpc::provider::rpc_round_trips().saturating_sub(fetch_rt_start),
        );
        let inputs = ChunkInputs {
            pool_creations,
            v3_liquidity,
            v4_liquidity,
            v4_manager_addresses,
            pool_manager_chain: chain_id,
        };

        // The market-wide gate's chunk predicate (interval boundary crossing +
        // run completion), evaluated BEFORE the read pass: the pure block math
        // is the same value the gate block below consumes, and the read
        // pass needs it NOW — full maps are built iff any verification will
        // consume them, the dirty overlay + delta otherwise.
        let run_full = crate::verify::should_run_full_verify_at_interval(
            working_start_block,
            working_end_block,
            verify_all_interval,
        ) || (verify_all_at_completion && working_end_block >= last_block);

        // The read pass computes every in-scope pool's post-chunk
        // state BEFORE the transaction opens - the per-pool map SELECTs leave
        // the write lock (survey finding 1). The per-chunk gate consumes the
        // FULL maps, so the read pass always builds them (the gate's input
        // contract is exact — see `compute_preverified_liquidity`). The
        // returned plans own the maps + deltas; the guard they used drops
        // inside.
        // The compute stage span wraps the read pass — the same boundary
        // the `read_pass_time` anchor times. SQL-only: no RPC, no writes
        // outside the later tx.
        let compute_span = tracing::info_span!("degenbot.updater.pool.compute");
        let compute_guard = compute_span.enter();
        let read_pass_started = Instant::now();
        let preverified = compute_preverified_liquidity(db, chain_id, &chunk_specs, &inputs)?;
        let read_pass_time = read_pass_started.elapsed();
        drop(compute_guard);

        // The pre-commit on-chain-truth gate (Full per-pool verification),
        // outside the transaction: each planned map is
        // compared against on-chain truth at `working_end_block` with NO lock
        // held - a divergence means the transaction NEVER OPENS, so the
        // observable RED contract is unchanged (`RunError::Verification`, zero
        // rows written, the stamp unadvanced) while the RPC leaves the lock.
        {
            let verify_span = tracing::info_span!(
                "degenbot.updater.pool.verify",
                verify.kind = "chunk",
                rpc.round_trips = tracing::field::Empty,
            );
            let verify_rt_start = degenbot_rpc::provider::rpc_round_trips();
            let verify_guard = verify_span.enter();
            let verify_started = Instant::now();
            verify_precomputed_maps(&provider, rt, working_end_block, &preverified, &inputs)?;
            chunk_verify_time += verify_started.elapsed();
            drop(verify_guard);
            verify_span.record(
                "rpc.round_trips",
                degenbot_rpc::provider::rpc_round_trips().saturating_sub(verify_rt_start),
            );
        }

        // Full (market-wide) verification gate: interval boundary crossing +
        // run completion - hoisted the same way. The committed maps (read
        // pre-transaction) overlay the planned maps for this chunk's touched
        // pools, so the verified state is exactly what the in-transaction gate
        // would see post-apply; a divergence never opens the tx.
        // (`run_full` is computed above the read pass — the same value; the
        // read pass consumed it as its full-map requirement.)
        if run_full {
            let full_verify_span = tracing::info_span!(
                "degenbot.updater.pool.verify",
                verify.kind = "full",
                rpc.round_trips = tracing::field::Empty,
            );
            let full_verify_rt_start = degenbot_rpc::provider::rpc_round_trips();
            let full_verify_guard = full_verify_span.enter();
            let full_verify_started = Instant::now();
            crate::verify::verify_all_pools_pre_commit(&crate::verify::PreCommitFullVerifyCtx {
                db,
                chain_id,
                pool_manager_chain: inputs.pool_manager_chain,
                block_number: working_end_block,
                provider: &provider,
                rt,
                preverified: &preverified,
                v4_manager_addresses: &inputs.v4_manager_addresses,
            })?;
            chunk_full_verify_time += full_verify_started.elapsed();
            drop(full_verify_guard);
            full_verify_span.record(
                "rpc.round_trips",
                degenbot_rpc::provider::rpc_round_trips().saturating_sub(full_verify_rt_start),
            );
        }

        // The single-transaction chunk write (section-1 atomicity). On ANY
        // error the `Transaction` drops -> rollback -> `last_update_block`
        // unchanged -> restart re-processes clean (restart-invariant). The
        // only retryable error is `MarkerMoved` (the optimistic stamp's
        // structural check): drop, refresh the specs, re-plan the chunk.
        // The apply stage span wraps the single-transaction write block —
        // for the pool loop this IS the write-lock hold (the read pass + the
        // verify gates run outside the tx), so the span's Jaeger
        // duration and the `write_lock_hold_time` ChunkProgress number are
        // the same boundary.
        let apply_span = tracing::info_span!(
            "degenbot.updater.pool.apply",
            lock.hold_us = tracing::field::Empty,
            events.applied = tracing::field::Empty,
        );
        let apply_guard = apply_span.enter();
        let chunk_report = {
            let mut guard = db.lock();
            // The write-lock hold starts at the `transaction()` open and
            // ends at the commit/drop (measured in every branch below).
            let lock_hold_started = Instant::now();
            let tx = guard.transaction().map_err(degenbot_db::DbError::from)?;
            let result = apply_chunk_writes_on_conn(
                &tx,
                chain_id,
                &chunk_specs,
                working_end_block,
                &inputs,
                &preverified,
            );
            match result {
                Ok(r) => {
                    tx.commit().map_err(degenbot_db::DbError::from)?;
                    chunk_lock_hold_time = lock_hold_started.elapsed();
                    if let Some(t) = degenbot_core::updater_telemetry::updaters() {
                        t.observe_lock_hold(UpdaterKind::Pool, chunk_lock_hold_time.as_secs_f64());
                    }
                    r
                }
                Err(RunError::MarkerMoved { exchange_id }) => {
                    // The assumed marker moved under the chunk (structural:
                    // single-writer means this is unreachable in production;
                    // the check exists so the invariant is not merely argued).
                    // Drop the tx (rollback - nothing staged survives), re-read
                    // the specs, and re-run the SAME chunk: the re-attempt's
                    // per-exchange fetch starts come from the markers the DB
                    // holds, not from the dropped attempt's snapshot.
                    drop(tx);
                    chunk_lock_hold_time = lock_hold_started.elapsed();
                    if let Some(t) = degenbot_core::updater_telemetry::updaters() {
                        t.observe_lock_hold(UpdaterKind::Pool, chunk_lock_hold_time.as_secs_f64());
                    }
                    marker_retries += 1;
                    if marker_retries > MARKER_RETRY_CAP {
                        progress.report_chunk(&ChunkProgress {
                            chain_id,
                            chunk_start: working_start_block,
                            chunk_end: working_end_block,
                            pools_written: 0,
                            liquidity_apply_count: 0,
                            committed: false,
                            is_final: false,
                            fetch_time: chunk_fetch_time,
                            decode_compute_time: read_pass_time,
                            verify_time: chunk_verify_time + chunk_full_verify_time,
                            apply_time: Duration::ZERO,
                            write_lock_hold_time: chunk_lock_hold_time,
                        });
                        return Err(RunError::MarkerMoved { exchange_id });
                    }
                    // The refresh SELECT runs with the chunk's write lock
                    // released, and the cursor stays put: the loop re-plans
                    // from `working_start_block`.
                    drop(guard);
                    specs_to_update =
                        unprocessed_specs(&load_active_exchange_specs(db, chain_id)?, last_block);
                    continue;
                }
                Err(e) => {
                    // Drop `tx` (rollback) - the chunk's writes + the stamp
                    // advance are reverted; the committed prior chunks stand.
                    // `e` is already a `RunError` (Db, Provider, or
                    // Verification - all rollback the chunk).
                    drop(tx);
                    chunk_lock_hold_time = lock_hold_started.elapsed();
                    if let Some(t) = degenbot_core::updater_telemetry::updaters() {
                        t.observe_lock_hold(UpdaterKind::Pool, chunk_lock_hold_time.as_secs_f64());
                    }
                    progress.report_chunk(&ChunkProgress {
                        chain_id,
                        chunk_start: working_start_block,
                        chunk_end: working_end_block,
                        pools_written: 0,
                        liquidity_apply_count: 0,
                        committed: false,
                        is_final: false,
                        fetch_time: chunk_fetch_time,
                        decode_compute_time: read_pass_time,
                        verify_time: chunk_verify_time + chunk_full_verify_time,
                        apply_time: Duration::ZERO,
                        write_lock_hold_time: chunk_lock_hold_time,
                    });
                    return Err(e);
                }
            }
        };
        drop(apply_guard);
        apply_span.record(
            "lock.hold_us",
            u64::try_from(chunk_lock_hold_time.as_micros()).unwrap_or(u64::MAX),
        );
        apply_span.record(
            "events.applied",
            u64::try_from(chunk_report.liquidity_apply_count).unwrap_or(u64::MAX),
        );

        // Perf E: advance the in-memory markers straight from the write
        // report — the stamps the just-committed transaction wrote are
        // exactly `chunk_report.stamped_last_update_block` for every chunk
        // spec (each stamp's optimistic WHERE fired, or the chunk errored
        // before this line). The post-commit tail this replaces ran a
        // per-spec `fetch_exchange` whose result was thrown away (a dead
        // query — and a silent error swallow) plus a full
        // `load_active_exchange_specs` reload whose only live output was
        // the marker advance carried here; both cost real statements and
        // wall time on every chunk (survey finding 6). No other writer can
        // move a marker under the bot's single-writer discipline — a moved
        // marker surfaces loudly as `RunError::MarkerMoved` from the stamp
        // itself (the structural check this re-plan loop already handles).
        for spec in &mut specs_to_update {
            if chunk_specs.iter().any(|c| c.id == spec.id) {
                spec.last_update_block = Some(chunk_report.stamped_last_update_block);
            }
        }

        progress.report_chunk(&ChunkProgress {
            chain_id,
            chunk_start: working_start_block,
            chunk_end: working_end_block,
            pools_written: chunk_report.pools_written,
            liquidity_apply_count: chunk_report.liquidity_apply_count,
            committed: true,
            is_final: working_end_block >= last_block,
            fetch_time: chunk_fetch_time,
            // The read pass's compute (pre-transaction) + whatever fused-path
            // compute ran in-transaction (chunk-new pools).
            decode_compute_time: read_pass_time + chunk_report.decode_compute_time,
            verify_time: chunk_verify_time + chunk_full_verify_time,
            apply_time: chunk_report.apply_time,
            write_lock_hold_time: chunk_lock_hold_time,
        });
        // The stage metrics ride the SAME ChunkProgress boundaries the
        // replay bench reports (one observation per committed chunk; the
        // golden numbers stay the source of truth — this is the passive
        // production twin). On a rollback the stage costs don't sample (a
        // rolled-back chunk's numbers are not attributable); the lock hold
        // DOES — it was held (the three assignment sites above).
        if let Some(t) = degenbot_core::updater_telemetry::updaters() {
            t.observe_stage(
                UpdaterKind::Pool,
                UpdaterStage::Fetch,
                chunk_fetch_time.as_secs_f64(),
            );
            t.observe_stage(
                UpdaterKind::Pool,
                UpdaterStage::Compute,
                (read_pass_time + chunk_report.decode_compute_time).as_secs_f64(),
            );
            t.observe_stage(
                UpdaterKind::Pool,
                UpdaterStage::Verify,
                (chunk_verify_time + chunk_full_verify_time).as_secs_f64(),
            );
            t.observe_stage(
                UpdaterKind::Pool,
                UpdaterStage::Apply,
                chunk_report.apply_time.as_secs_f64(),
            );
            t.add_rpc_round_trips(
                UpdaterKind::Pool,
                degenbot_rpc::provider::rpc_round_trips()
                    .saturating_sub(chunk_rpc_round_trips_start),
            );
        }
        report.chunks_committed += 1;
        report.total_pools_written += chunk_report.pools_written;
        report.total_liquidity_applies += chunk_report.liquidity_apply_count;

        // Operator-facing progress (the Rust core owns CLI progress —
        // no per-chunk FFI hop). Time-throttled so a long backfill's console
        // stays readable; the run's final chunk always logs so completion is
        // observable even when the last chunks land inside one throttle window.
        if last_progress_log.elapsed() >= PROGRESS_LOG_INTERVAL || working_end_block >= last_block {
            op_info!(
                domain = ingest,
                chain_id,
                chunk_start = working_start_block,
                chunk_end = working_end_block,
                chunks_committed = report.chunks_committed,
                pools_written = chunk_report.pools_written,
                liquidity_apply_count = chunk_report.liquidity_apply_count,
                progress_pct = progress_percent(initial_start_block, working_end_block, last_block),
                "pool update: chunk committed"
            );
            last_progress_log = Instant::now();
        }

        working_start_block = working_end_block + 1;
    }

    Ok(report)
}

/// Fetch + map the chunk's pool-creation events across all in-scope exchange
/// specs (per-exchange `fetch_pool_created_logs_for_spec` +
/// [`map_pool_creation`]). Each spec carries its own per-exchange fetch start
/// (see [`in_scope_fetch_start`]) so a divergent-ahead spec does NOT re-fetch
/// its committed `PoolCreated`/`Initialize` events. Aerodrome V2 pools whose
/// per-pool `getFee` RPC hasn't been resolved are SKIPPED (the fee-RPC path
/// is a follow-up; the canonical V2/V3/V4 + Aerodrome V3 paths are NOT
/// affected).
async fn fetch_pool_creations(
    fetcher: &LogFetcher,
    to_block: u64,
    specs_with_start: &[(ExchangeSpec, u64)],
) -> Result<Vec<PoolCreationToWrite>, ProviderError> {
    let mut out = Vec::new();
    for (spec, from_block) in specs_with_start {
        let decoded =
            fetch_pool_created_logs_for_spec(fetcher, *from_block, to_block, spec).await?;
        for event in decoded {
            match map_pool_creation(spec, &event, /* aerodrome_fee */ None) {
                Ok(row) => out.push(row),
                Err(MapError::AerodromeFeeNeeded { pool_address }) => {
                    // Aerodrome V2 fee RPC not yet wired — skip this pool
                    // (a follow-up task resolves getFee(address, stable)).
                    // Logged via stderr (the chunk's progress sink doesn't carry
                    // per-pool warnings; a future sink revision might).
                    #[expect(clippy::print_stderr)] // deliberate per-pool stderr warning
                    {
                        eprintln!(
                            "degenbot-pool-updater: skipping Aerodrome V2 pool \
                             {pool_address} (exchange {}): per-pool getFee RPC not wired yet",
                            spec.name,
                        );
                    }
                }
            }
        }
    }
    Ok(out)
}

#[expect(clippy::panic)]
#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::verify::LiquidityDivergence;
    use alloy::primitives::{I256, U256 as U256P};
    use degenbot_db::sql_ledger::LedgerDb;
    use degenbot_db::DegenbotDb;
    use tempfile::TempDir;

    /// An in-memory writeable DB with the head schema (the chunk-writer's
    /// substrate). Mirrors the discovery tests' `write_db` helper.
    fn write_db() -> DegenbotDb {
        let (db, _state) = DegenbotDb::open_for_writes(Path::new(":memory:")).unwrap();
        db
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

    #[test]
    fn apply_chunk_writes_on_conn_commits_pools_and_stamp_together() {
        let db = write_db();
        let factory = Address::from([0x1f; 20]);
        let exchange = db.upsert_exchange(1, "uniswap_v3", factory, None).unwrap();
        let spec = ExchangeSpec {
            id: exchange.id,
            chain_id: 1,
            name: "uniswap_v3".to_string(),
            factory,
            family: crate::fetch::PoolFamily::V3,
            event_topic: B256::ZERO,
            fee_denominator: 1_000_000,
            v2_fee_token: None,
            rpc_fee_call: None,
            last_update_block: None,
        };
        let specs = [spec.clone()];
        let pool = V3PoolRowInput {
            address: Address::from([0x11; 20]),
            token0_address: Address::from([0x22; 20]),
            token1_address: Address::from([0x33; 20]),
            fee: 500,
            tick_spacing: 10,
        };
        let inputs = ChunkInputs {
            pool_creations: vec![PoolCreationToWrite::V3 {
                exchange_id: exchange.id,
                row: pool.clone(),
            }],
            ..Default::default()
        };

        // ONE transaction wraps the pool write + the stamp.
        let mut guard = db.lock();
        let tx = guard.transaction().unwrap();
        let report = apply_chunk_writes_on_conn(
            &tx,
            1,
            &specs,
            100,
            &inputs,
            &PreVerifiedLiquidity::default(),
        )
        .unwrap();
        tx.commit().unwrap();
        drop(guard);

        assert_eq!(report.pools_written, 1);
        // Both the pool + the stamp landed (atomicity).
        assert!(db.fetch_pool_by_address(pool.address, 1).unwrap().is_some());
        let after = db.fetch_exchange(exchange.id).unwrap().unwrap();
        assert_eq!(after.last_update_block, Some(100));
    }

    #[test]
    fn apply_chunk_writes_on_conn_rolls_back_on_injected_duplicate_failure() {
        let db = write_db();
        let factory = Address::from([0x1f; 20]);
        let exchange = db.upsert_exchange(1, "uniswap_v3", factory, None).unwrap();
        let spec = ExchangeSpec {
            id: exchange.id,
            chain_id: 1,
            name: "uniswap_v3".to_string(),
            factory,
            family: crate::fetch::PoolFamily::V3,
            event_topic: B256::ZERO,
            fee_denominator: 1_000_000,
            v2_fee_token: None,
            rpc_fee_call: None,
            last_update_block: None,
        };
        let specs = [spec.clone()];
        let pool = V3PoolRowInput {
            address: Address::from([0x11; 20]),
            token0_address: Address::from([0x22; 20]),
            token1_address: Address::from([0x33; 20]),
            fee: 500,
            tick_spacing: 10,
        };
        let inputs_ok = ChunkInputs {
            pool_creations: vec![PoolCreationToWrite::V3 {
                exchange_id: exchange.id,
                row: pool.clone(),
            }],
            ..Default::default()
        };
        let inputs_dup = ChunkInputs {
            pool_creations: vec![PoolCreationToWrite::V3 {
                exchange_id: exchange.id,
                row: pool.clone(),
            }],
            ..Default::default()
        };

        // First chunk: commit cleanly (pool + stamp=100).
        {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            apply_chunk_writes_on_conn(
                &tx,
                1,
                &specs,
                100,
                &inputs_ok,
                &PreVerifiedLiquidity::default(),
            )
            .unwrap();
            tx.commit().unwrap();
        }

        // Second chunk: write the SAME pool (duplicate) → UNIQUE violation →
        // the inner fn returns Err → tx drops → rollback. The stamp advance to
        // 200 must NOT be durable (the chunk-atomicity + restart invariants).
        let err = {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let result = apply_chunk_writes_on_conn(
                &tx,
                1,
                &specs,
                200,
                &inputs_dup,
                &PreVerifiedLiquidity::default(),
            );
            // The duplicate insert must surface as an error (UNIQUE constraint).
            assert!(result.is_err(), "duplicate pool insert must error");
            // Drop tx without commit → rollback (the inner fn's `?` already returned).
            drop(tx);
            result.err()
        };

        // (a) The stamp stayed at 100 (the 200 advance rolled back).
        let after = db.fetch_exchange(exchange.id).unwrap().unwrap();
        assert_eq!(
            after.last_update_block,
            Some(100),
            "rolled-back chunk's stamp advance must not be durable (restart-safe)",
        );
        // (b) The pool is still the single committed row (not re-inserted).
        let _ = err;
        assert!(db.fetch_pool_by_address(pool.address, 1).unwrap().is_some());
        // (c) The error propagated (the caller would surface it).
        // (already asserted `result.is_err()` above)
    }

    /// The V4 exclusion bug: when exchange markers diverge (V3 forks stalled
    /// at `M` while V2/V4 advanced to `M + k`), the strict equality filter
    /// `marker + 1 == working_start` stranded every ahead-exchange — its
    /// unprocessed work in the chunk's tail was never fetched. The fix admits
    /// any spec that still has unprocessed work at or before `working_end`,
    /// scoped to its own marker + 1 so it does NOT re-fetch committed blocks.
    #[test]
    fn in_scope_fetch_start_admits_divergent_ahead_spec() {
        // V3 laggard at M (roots the chunk cursor at M + 1).
        const M: u64 = 25_508_130;
        // V4 ahead by k blocks (advanced in earlier committed chunks while
        // V3 was stuck on a verify rollback).
        const K: u64 = 2_593;
        const TIP: u64 = 25_510_816;
        let working_start = M + 1; // the min marker + 1 (V3 roots the cursor)
        let working_end = TIP;

        // 1. V3 laggard at the grid point — unchanged behavior.
        let v3 = in_scope_fetch_start(Some(i64::try_from(M).unwrap()), working_start, working_end);
        assert_eq!(
            v3,
            Some(working_start),
            "laggard at the grid point fetches the whole chunk",
        );

        // 2. V4 ahead of the grid but still behind `working_end` — MUST be
        //    in-scope, scoped to its own marker + 1 (NOT working_start, which
        //    would re-fetch V4's already-committed [M+1, M+k] range and
        //    double-apply liquidity in release builds where the per-pool
        //    event guard is a stripped `debug_assert!`).
        let v4_marker = M + K; // 25_510_723
        let v4 = in_scope_fetch_start(
            Some(i64::try_from(v4_marker).unwrap()),
            working_start,
            working_end,
        );
        assert_eq!(
            v4,
            Some(v4_marker + 1),
            "divergent-ahead spec is in-scope, scoped to its own marker + 1 \
             (no re-fetch of committed blocks, no double-apply)",
        );

        // 3. A spec already past this chunk's end is skipped (unchanged).
        let past = in_scope_fetch_start(
            Some(i64::try_from(working_end).unwrap()),
            working_start,
            working_end,
        );
        assert_eq!(
            past, None,
            "spec already at the chunk's end is skipped (nothing to do)",
        );

        // 4. A spec with `last_update_block = None` (never stamped) scans the
        //    whole chunk (backfill / first run — unchanged).
        let fresh = in_scope_fetch_start(None, working_start, working_end);
        assert_eq!(
            fresh,
            Some(working_start),
            "never-stamped spec scans the whole chunk",
        );

        // 5. A spec BEHIND the grid (divergent laggard — rarer, but the same
        //    bug class): its own marker + 1 is the unprocessed cursor, but
        //    clamped to `working_start` so it re-scans only the chunk's range,
        //    not everything since its stale marker.
        let behind_marker = M - 5_000; // a spec way behind the grid
        let behind = in_scope_fetch_start(
            Some(i64::try_from(behind_marker).unwrap()),
            working_start,
            working_end,
        );
        assert_eq!(
            behind,
            Some(working_start),
            "behind-spec is in-scope but clamped to the chunk's lower bound",
        );
    }

    /// `RunError::Verification` carries the diverging pool id + the named
    /// divergences (the harness the pre-commit gate returns on a RED). This
    /// is the shape the chunk loop propagates (testing that the error chain
    /// conveys the divergence data the operator needs — no RPC).
    #[test]
    fn run_error_verification_carries_pool_and_divergences() {
        let divergences = vec![
            LiquidityDivergence::TickGross {
                tick: 12345,
                expected: alloy::primitives::U128::from(500u64),
                actual: alloy::primitives::U128::ZERO,
            },
            LiquidityDivergence::BitmapWord {
                word: 0,
                expected: U256P::from(1u64),
                actual: U256P::ZERO,
            },
        ];
        let err = RunError::Verification {
            pool: "0xdead..beef".to_string(),
            block_number: 42,
            divergences: divergences.clone(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("0xdead..beef"), "error names the pool: {msg}");
        assert!(msg.contains("42"), "error names the block: {msg}");
        match &err {
            RunError::Verification {
                pool: _,
                block_number: _,
                divergences: d,
            } => {
                assert_eq!(d, &divergences);
            }
            other => panic!("expected Verification, got {other:?}"),
        }
    }

    /// The ledger's capture session is ONE process-global slot: the two
    /// SQL-branch tests below take turns.
    static LEDGER_SESSION: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// One liquidity event; `liquidity_delta` is signed (Burn negative).
    fn liq_event(
        block_number: u64,
        log_index: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i64,
    ) -> LiquidityUpdateEvent {
        LiquidityUpdateEvent {
            block_number,
            log_index,
            tick_lower,
            tick_upper,
            liquidity_delta: I256::try_from(liquidity_delta).unwrap(),
        }
    }

    /// A traced DB with one `uniswap_v3` exchange, the chunk-new pool row, and
    /// the chunk inputs that create that pool + apply `events` to it.
    fn chunk_new_v3_fixture(
        path: &Path,
        events: &[LiquidityUpdateEvent],
    ) -> (LedgerDb, ExchangeSpec, ChunkInputs, V3PoolRowInput) {
        let (ledger, _state) = LedgerDb::open_for_writes(path).unwrap();
        let factory = Address::from([0x1f; 20]);
        let exchange = ledger
            .db()
            .upsert_exchange(1, "uniswap_v3", factory, None)
            .unwrap();
        let spec = ExchangeSpec {
            id: exchange.id,
            chain_id: 1,
            name: "uniswap_v3".to_string(),
            factory,
            family: crate::fetch::PoolFamily::V3,
            event_topic: B256::ZERO,
            fee_denominator: 1_000_000,
            v2_fee_token: None,
            rpc_fee_call: None,
            last_update_block: None,
        };
        let row = V3PoolRowInput {
            address: Address::from([0x11; 20]),
            token0_address: Address::from([0x22; 20]),
            token1_address: Address::from([0x33; 20]),
            fee: 500,
            tick_spacing: 10,
        };
        let inputs = ChunkInputs {
            pool_creations: vec![PoolCreationToWrite::V3 {
                exchange_id: exchange.id,
                row: row.clone(),
            }],
            v3_liquidity: HashMap::from([(row.address, events.to_vec())]),
            ..Default::default()
        };
        (ledger, spec, inputs, row)
    }

    /// The plan the read pass hands the apply for a chunk-new pool: empty base +
    /// the chunk's events (the row does not exist yet, so the delta keeps the
    /// sentinel `pool_id`).
    fn chunk_new_v3_plan(
        events: &[LiquidityUpdateEvent],
    ) -> (ComputedLiquidityUpdate, LiquidityDelta) {
        let computed = DegenbotDb::compute_v3_liquidity_update_for_new_pool(10, events);
        let delta = derive_liquidity_delta_from_computed(&computed, events);
        (computed, delta)
    }

    /// A chunk-new pool persists the map the gate verified: the plan's delta
    /// under the id the chunk's own upsert assigned, with no in-transaction
    /// re-derivation read.
    #[test]
    fn chunk_new_pool_persists_the_verified_plan_delta() {
        let _slot = LEDGER_SESSION.lock().unwrap();
        let dir = TempDir::new().unwrap();
        let events = vec![
            liq_event(100, 0, -10, 10, 1_000_000),
            liq_event(100, 1, 100, 110, 250_000),
            liq_event(101, 0, 100, 110, -250_000),
        ];
        let (ledger, spec, inputs, row) =
            chunk_new_v3_fixture(&dir.path().join("plan.db"), &events);
        let (computed, delta) = chunk_new_v3_plan(&events);
        assert_eq!(
            delta.pool_id, 0,
            "the plan keys nothing yet: sentinel pool_id"
        );
        let preverified = PreVerifiedLiquidity {
            v3: HashMap::from([(
                row.address,
                PlannedLiquidityMap {
                    computed: computed.clone(),
                    delta,
                    is_new_pool: true,
                },
            )]),
            ..Default::default()
        };

        let report = {
            let mut guard = ledger.db().lock();
            let tx = guard.transaction().unwrap();
            let report =
                apply_chunk_writes_on_conn(&tx, 1, &[spec], 200, &inputs, &preverified).unwrap();
            tx.commit().unwrap();
            report
        };
        assert_eq!(report.liquidity_apply_count, 1, "one liquidity apply");

        let records = ledger.records().unwrap();
        assert!(
            !records
                .iter()
                .any(|r| r.normalized_sql.starts_with("SELECT")
                    && r.normalized_sql.contains("FROM liquidity_positions")),
            "the apply must persist the verified plan, never re-read the map: {:?}",
            records
                .iter()
                .map(|r| r.normalized_sql.as_str())
                .collect::<Vec<_>>()
        );

        let conn = ledger.db().lock();
        let pool_id: i64 = conn
            .query_row(
                "SELECT id FROM pools WHERE address = ?1",
                rusqlite::params![row.address.to_checksum(None)],
                |r| r.get(0),
            )
            .unwrap();
        let mut expected: Vec<(i32, String, String)> = computed
            .tick_data
            .iter()
            .map(|(tick, at)| {
                (
                    *tick,
                    at.liquidity_net.to_string(),
                    at.liquidity_gross.to_string(),
                )
            })
            .collect();
        expected.sort_by_key(|(tick, _, _)| *tick);
        let mut stmt = conn
            .prepare(
                "SELECT tick, liquidity_net, liquidity_gross FROM liquidity_positions                  WHERE pool_id = ?1 ORDER BY tick",
            )
            .unwrap();
        let persisted: Vec<(i32, String, String)> = stmt
            .query_map(rusqlite::params![pool_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            persisted, expected,
            "the persisted map IS the verified plan's map"
        );
        let last = computed.last_event.unwrap();
        let marker: (i64, i64) = conn
            .query_row(
                "SELECT liquidity_update_block, liquidity_update_log_index                  FROM uniswap_v3_pools WHERE pool_id = ?1",
                rusqlite::params![pool_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            marker,
            (
                i64::try_from(last.block).unwrap(),
                i64::try_from(last.log_index).unwrap()
            ),
            "the marker is the verified stamp"
        );
    }

    /// A chunk-new pool drained inside its creating chunk takes the drained-pool
    /// complement delete-all — the derive truth — instead of per-key deletes.
    #[test]
    fn chunk_new_full_drain_takes_the_complement_delete() {
        let _slot = LEDGER_SESSION.lock().unwrap();
        let dir = TempDir::new().unwrap();
        let events = vec![
            liq_event(100, 0, -10, 10, 500_000),
            liq_event(100, 1, 100, 110, 250_000),
            liq_event(101, 0, -10, 10, -500_000),
            liq_event(101, 1, 100, 110, -250_000),
        ];
        let (ledger, spec, inputs, row) =
            chunk_new_v3_fixture(&dir.path().join("drain.db"), &events);
        let (computed, delta) = chunk_new_v3_plan(&events);
        assert!(delta.drained_pool, "the verified map is empty");
        let preverified = PreVerifiedLiquidity {
            v3: HashMap::from([(
                row.address,
                PlannedLiquidityMap {
                    computed,
                    delta,
                    is_new_pool: true,
                },
            )]),
            ..Default::default()
        };
        {
            let mut guard = ledger.db().lock();
            let tx = guard.transaction().unwrap();
            apply_chunk_writes_on_conn(&tx, 1, &[spec], 200, &inputs, &preverified).unwrap();
            tx.commit().unwrap();
        }

        let deletes: Vec<String> = ledger
            .records()
            .unwrap()
            .into_iter()
            .map(|r| r.normalized_sql)
            .filter(|sql| sql.starts_with("DELETE FROM liquidity_positions"))
            .collect();
        assert_eq!(deletes.len(), 1, "one delete statement: {deletes:?}");
        assert!(
            !deletes[0].contains(" IN ("),
            "the complement delete-all carries no per-key list: {}",
            deletes[0]
        );
        let conn = ledger.db().lock();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM liquidity_positions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0, "the fully-drained chunk-new pool holds no rows");
    }

    /// The chain these driver-level marker-move tests advance.
    const TEST_CHAIN: i64 = 84_532;

    /// Every `eth_getLogs` the stub answered: `(scoped, fromBlock)`, where
    /// `scoped` marks the per-exchange creation fetch (the request carrying a
    /// factory `address` filter) as opposed to the whole-chain V3 scan.
    type ServedLogs = Arc<std::sync::Mutex<Vec<(bool, u64)>>>;

    /// A JSON-RPC stub serving empty `eth_getLogs` answers, with `on_logs` run
    /// on every answered request — the hook between the driver's in-memory
    /// marker snapshot and its optimistic stamp, where a concurrent writer moves
    /// the marker.
    ///
    /// The provider's default caching layer answers a repeated identical filter
    /// locally, so only a RE-PLANNED range reaches the wire: a retry that keeps
    /// its stale specs issues no second creation fetch. The retry tests read
    /// exactly that.
    struct MarkerMoveNode {
        url: String,
        shutdown: Arc<AtomicBool>,
        served: ServedLogs,
    }

    impl MarkerMoveNode {
        fn spawn(on_logs: Box<dyn Fn(bool, u64) + Send + Sync>) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            listener.set_nonblocking(true).unwrap();
            let shutdown = Arc::new(AtomicBool::new(false));
            let served: ServedLogs = Arc::new(std::sync::Mutex::new(Vec::new()));
            let flag = Arc::clone(&shutdown);
            let served_thread = Arc::clone(&served);
            let on_logs = Arc::from(on_logs);
            std::thread::spawn(move || {
                while !flag.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _peer)) => {
                            let _ = answer_rpc(&mut stream, &served_thread, &*on_logs);
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                url,
                shutdown,
                served,
            }
        }

        fn served(&self) -> Vec<(bool, u64)> {
            self.served.lock().unwrap().clone()
        }

        fn creation_fetch_starts(&self) -> Vec<u64> {
            self.served()
                .into_iter()
                .filter(|(scoped, _)| *scoped)
                .map(|(_, from_block)| from_block)
                .collect()
        }
    }

    impl Drop for MarkerMoveNode {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Release);
        }
    }

    fn answer_rpc(
        stream: &mut std::net::TcpStream,
        served: &ServedLogs,
        on_logs: &(dyn Fn(bool, u64) + Send + Sync),
    ) -> std::io::Result<()> {
        use std::io::{Read, Write};
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            let n = stream.read(&mut chunk)?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let headers = String::from_utf8_lossy(&buf[..header_end]).into_owned();
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0);
        while buf.len() < header_end + content_length {
            let n = stream.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let request: serde_json::Value =
            serde_json::from_slice(&buf[header_end..]).unwrap_or(serde_json::Value::Null);
        let id = request["id"].clone();
        let response = match request["method"].as_str().unwrap_or_default() {
            "eth_getLogs" => {
                let filter = &request["params"][0];
                let scoped = filter.get("address").is_some();
                let from_block = filter
                    .get("fromBlock")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok())
                    .unwrap_or_default();
                served.lock().unwrap().push((scoped, from_block));
                on_logs(scoped, from_block);
                serde_json::json!({"jsonrpc": "2.0", "id": id, "result": []})
            }
            "eth_chainId" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": format!("0x{TEST_CHAIN:x}"),
            }),
            _ => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": "method not found"},
            }),
        };
        let body = response.to_string();
        let head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes())?;
        stream.write_all(body.as_bytes())?;
        stream.flush()
    }

    /// A temp DB with one ACTIVE `uniswap_v3` exchange, never stamped.
    fn seed_active_v3(path: &Path) {
        let (db, _state) = DegenbotDb::open_for_writes(path).unwrap();
        let exchange = db
            .upsert_exchange(TEST_CHAIN, "uniswap_v3", Address::repeat_byte(0xf1), None)
            .unwrap();
        db.set_exchange_active(exchange.id, true).unwrap();
    }

    /// The persisted marker — the value a refreshed spec reads.
    fn persisted_marker(path: &Path) -> Option<i64> {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.query_row(
            "SELECT last_update_block FROM exchanges WHERE chain_id = ?1 AND active = 1",
            [TEST_CHAIN],
            |row| row.get(0),
        )
        .unwrap()
    }

    /// Move the marker from OUTSIDE the driver's handle — the concurrent-writer
    /// shape the optimistic stamp's check exists for.
    fn move_marker(path: &Path, to: i64) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        conn.execute(
            "UPDATE exchanges SET last_update_block = ?1 WHERE chain_id = ?2 AND active = 1",
            rusqlite::params![to, TEST_CHAIN],
        )
        .unwrap();
    }

    /// Drive `run_pool_update` over the stub node's injected transport.
    fn drive(
        path: &Path,
        node: &MarkerMoveNode,
        to_block: u64,
        chunk_size: u64,
        progress: Arc<dyn ProgressSink>,
    ) -> Result<UpdateReport, RunError> {
        let provider = degenbot_core::runtime::get_runtime()
            .block_on(AlloyProvider::new(&node.url, 5))
            .unwrap();
        run_pool_update(
            path,
            TEST_CHAIN,
            Some(to_block),
            chunk_size,
            10_000,
            provider,
            Arc::new(AtomicBool::new(false)),
            progress,
            None,
            false,
        )
    }

    /// The chunks the run reported as committed, by boundary.
    #[derive(Default)]
    struct RecordedChunks(std::sync::Mutex<Vec<ChunkProgress>>);

    impl ProgressSink for RecordedChunks {
        fn report_chunk(&self, progress: &ChunkProgress) {
            self.0.lock().unwrap().push(*progress);
        }
    }

    impl RecordedChunks {
        fn committed(&self) -> Vec<(u64, u64)> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter(|p| p.committed)
                .map(|p| (p.chunk_start, p.chunk_end))
                .collect()
        }
    }

    /// A `MarkerMoved` retry plans from the DB's markers, not from the dropped
    /// attempt's snapshot: the re-attempt re-reads the exchange specs, so its
    /// per-exchange fetch start comes from the moved marker and the chunk
    /// commits instead of failing the same way until the run dies.
    #[test]
    fn marker_moved_retry_reloads_specs_before_replanning() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("marker_move_retry.db");
        seed_active_v3(&path);
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let node = MarkerMoveNode::spawn({
            let path = path.clone();
            Box::new(move |scoped, _from_block| {
                if scoped && attempts.fetch_add(1, Ordering::AcqRel) == 0 {
                    move_marker(&path, 5);
                }
            })
        });

        let report = drive(&path, &node, 10, 10, Arc::new(NoProgress)).unwrap();
        assert_eq!(
            report.chunks_committed, 1,
            "the retried attempt commits the chunk"
        );
        assert_eq!(
            persisted_marker(&path),
            Some(10),
            "the retried chunk stamps the chunk end"
        );
        assert_eq!(
            node.creation_fetch_starts(),
            vec![1, 6],
            "the re-attempt plans from the DB's marker (5 + 1 = 6), not the stale snapshot's 1",
        );
    }

    /// A marker that keeps moving exhausts the run-wide retry budget and
    /// surfaces the loud `MarkerMoved` backstop instead of spinning.
    #[test]
    fn marker_move_budget_exhaustion_surfaces_marker_moved() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("marker_move_budget.db");
        seed_active_v3(&path);
        let node = MarkerMoveNode::spawn({
            let path = path.clone();
            Box::new(move |scoped, _from_block| {
                if scoped {
                    // Every attempt refreshes to the marker its fetch then
                    // moves, so each retry re-plans a range the cache has not
                    // seen and the marker keeps slipping under the stamp.
                    let next = persisted_marker(&path).map_or(5, |marker| marker + 1);
                    move_marker(&path, next);
                }
            })
        });

        let err = drive(&path, &node, 10, 10, Arc::new(NoProgress)).unwrap_err();
        assert!(
            matches!(err, RunError::MarkerMoved { .. }),
            "a persistently moved marker surfaces the loud backstop, got: {err:?}",
        );
        assert_eq!(
            node.creation_fetch_starts(),
            vec![1, 6, 7, 8, 9],
            "one re-planned attempt per retry, then the budget (MARKER_RETRY_CAP) halts the run",
        );
        assert_ne!(
            persisted_marker(&path),
            Some(10),
            "the rolled-back chunk never advanced the stamp",
        );
    }

    /// The retried chunk keeps the cursor: a marker move re-runs the SAME block
    /// range, so no block is skipped and the stamp lands on the chunk end.
    #[test]
    fn marker_moved_retry_does_not_advance_the_chunk_cursor() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("marker_move_cursor.db");
        seed_active_v3(&path);
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let node = MarkerMoveNode::spawn({
            let path = path.clone();
            Box::new(move |scoped, _from_block| {
                if scoped && attempts.fetch_add(1, Ordering::AcqRel) == 0 {
                    move_marker(&path, 5);
                }
            })
        });
        let sink = Arc::new(RecordedChunks::default());

        let report = drive(&path, &node, 20, 10, sink.clone()).unwrap();
        assert_eq!(report.chunks_committed, 2);
        assert_eq!(
            sink.committed(),
            vec![(1, 10), (11, 20)],
            "the retried chunk re-runs 1..=10 before the cursor advances to 11",
        );
        assert_eq!(persisted_marker(&path), Some(20));
    }
}
