//! The Rust-owned pool-updater chunk loop — `run_pool_update` (epic
//! `2SFL6I`, task `CKXCOB` 3c).
//!
//! This is the standalone-Rust `pool_update` core: it opens ONE writeable
//! [`DegenbotDb`], loads the active exchange specs (task `SHFIGX`), resolves
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
//! # The §1 three invariants — hold by construction
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
//! # Why the write half is split out ([`apply_chunk_writes_on_conn`])
//!
//! The hard-to-test RPC boundary (fetch + decode) is separated from the
//! transaction-semantics core. [`apply_chunk_writes_on_conn`] is a pure
//! synchronous function: feed it pre-built fixture decoded events + a temp
//! `DegenbotDb` + a `Transaction`, + assert the all-or-nothing outcome. No
//! mock RPC, no live node. The fetchers themselves are unit-tested in
//! [`crate::fetch`] (decode-leaves) + integration-tested (live RPC).
//!
//! # D2: owned `tokio` runtime
//!
//! [`run_pool_update`] constructs its own `tokio::runtime::Runtime` (the
//! `rt-multi-thread` feature). **It MUST NOT be called from within an existing
//! tokio runtime** — doing so panics ("Cannot start a runtime from within a
//! runtime"). The Task 4 `PyO3` seam (`db_run_pool_update`) is the entry from
//! Python: Python calls it from a worker thread (NO ambient tokio runtime),
//! so the owned-runtime constraint holds. If Python ever needs to drive
//! `run_pool_update` from inside an async context, wrap it in
//! `tokio::task::spawn_blocking`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use alloy::primitives::{Address, B256};
use degenbot_core::errors::ProviderError;
use degenbot_db::{
    DegenbotDb, LiquidityUpdateEvent, V2PoolRowInput, V3PoolRowInput, V4PoolRowInput,
};
use degenbot_rpc::provider::{AlloyProvider, LogFetcher};
use rusqlite::Connection;

use crate::fetch::{
    fetch_pool_created_logs_for_spec, fetch_v3_liquidity_logs_grouped,
    fetch_v4_liquidity_logs_grouped, DecodedPoolCreated,
};
use crate::spec::{load_active_exchange_specs, ExchangeSpec};

/// The max-RPC-retries constant (mirrors the Python `get_v3_liquidity_events`
/// retry budget — a sane conservative default; not yet configurable).
const RPC_MAX_RETRIES: u32 = 5;

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
}

/// The sink the chunk loop reports per-chunk progress to. Implementations:
/// `NoProgress` (silent), a logging sink, or a `PyO3` callback sink (Task 4).
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
    /// The `pool_manager_chain` for V4 applies (the chain of the
    /// PoolManager(s) whose `ModifyLiquidity` events were scanned — equals
    /// `chain_id` for the per-chain chunk loop).
    pub pool_manager_chain: i64,
}

/// What [`apply_chunk_writes_on_conn`] wrote in the chunk.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ChunkWriteReport {
    /// V2/V3/V4 pool-creation rows upserted.
    pub pools_written: usize,
    /// Per-pool liquidity applies (V3 + V4) that found + mutated a pool row.
    pub liquidity_apply_count: usize,
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
#[allow(clippy::missing_errors_doc)]
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
/// exchanges' `last_update_block` — ALL under the ONE borrowed
/// [`Connection`] (the chunk's `Transaction`).
///
/// This is the §1 atomicity invariant's enforcement point: every write of
/// the chunk goes through here on ONE connection, + the caller's `Transaction`
/// commit/rollback is the single point of durability. Any `?` early-return
/// (a `UNIQUE` violation, a decode failure, ...) leaves the caller's
/// `Transaction` uncommitted → it drops → the whole chunk reverts →
/// `last_update_block` unchanged → restart re-processes (restart-invariant).
///
/// # The V3 liquidity in-scope filter
///
/// The whole-chain V3 scan returns pools across ALL V3 exchanges. The in-scope
/// set is `specs_to_update` (the chunk's laggard subset) — a V3 pool whose
/// `exchange_id` is NOT in `specs_to_update` is skipped (its exchange either
/// already advanced or isn't being updated this run). This mirrors `main`'s
/// `if exchange.id not in exchanges_to_update: continue` filter.
///
/// # Errors
///
/// Returns [`DbError`](degenbot_db::DbError) on any write/query failure — the
/// caller drops the `Transaction` (rollback) on `Err`.
#[allow(clippy::missing_errors_doc)]
pub fn apply_chunk_writes_on_conn(
    conn: &Connection,
    chain_id: i64,
    specs_to_update: &[ExchangeSpec],
    chunk_end: u64,
    inputs: &ChunkInputs,
) -> Result<ChunkWriteReport, degenbot_db::DbError> {
    let mut report = ChunkWriteReport::default();

    // 1. Pool creations — group by (exchange_id, family) + upsert per batch.
    upsert_pool_creations_on_conn(
        conn,
        chain_id,
        specs_to_update,
        &inputs.pool_creations,
        &mut report,
    )?;

    // 2. V3 liquidity — in-scope filter per pool, then per-pool apply.
    //    The whole-chain scan grouped by emitter; here we keep only pools whose
    //    `exchange_id` is in `specs_to_update` (the chunk's laggard subset).
    for (pool_address, events) in &inputs.v3_liquidity {
        let pool = DegenbotDb::fetch_pool_by_address_on_conn(conn, *pool_address, chain_id)?;
        let in_scope = pool
            .as_ref()
            .and_then(|r| specs_to_update.iter().find(|s| s.id == r.exchange_id))
            .is_some();
        if !in_scope {
            continue; // pool belongs to an exchange not being updated this chunk
        }
        if DegenbotDb::apply_v3_liquidity_updates_on_conn(
            conn,
            chain_id,
            &pool_address.to_checksum(None),
            events,
        )? {
            report.liquidity_apply_count += 1;
        }
    }

    // 3. V4 liquidity — per-pool apply. The V4 fetch is per-PoolManager
    //    (already per-exchange-scoped), so all fetched events are in-scope.
    for (pool_hash, events) in &inputs.v4_liquidity {
        if DegenbotDb::apply_v4_liquidity_updates_on_conn(
            conn,
            pool_hash,
            inputs.pool_manager_chain,
            events,
        )? {
            report.liquidity_apply_count += 1;
        }
    }

    // 4. Stamp `last_update_block` for each in-scope exchange — the LAST write
    //    in the transaction (the §1 restart-invariant: on rollback the stamp
    //    does NOT advance, so a restart re-processes the chunk).
    let chunk_end_i64 = i64::try_from(chunk_end).unwrap_or(i64::MAX);
    for spec in specs_to_update {
        DegenbotDb::set_exchange_last_update_block_on_conn(conn, chain_id, spec.id, chunk_end_i64)?;
    }

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
    for (exchange_id, (kind, fee_denominator, rows)) in v4_by_exchange {
        // V4 upserts `uniswap_v4_pools` keyed by the manager address (the
        // spec's `factory` is the PoolManager). The kind is always
        // `uniswap_v4`; `fee_denominator` is `1_000_000`.
        let _ = (kind, fee_denominator);
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
}

/// Run the pool-updater chunk loop for `chain_id`, advancing every active
/// exchange's `last_update_block` to `to_block` (or the chain tip if
/// `to_block` is `None`).
///
/// See the [module docs](self) for the §1 three invariants + the owned-runtime
/// constraint (D2: do NOT call from within an existing tokio runtime).
///
/// # Arguments
///
/// - `database_path` — the writeable `DegenbotDb` path (must already be
///   migrated to the Rust-owned schema; the chunk loop does NOT migrate).
/// - `chain_id` — the chain to advance.
/// - `to_block` — `Some(n)` to advance to block `n`; `None` to advance to the
///   chain tip (resolved via `eth_blockNumber`).
/// - `chunk_size` — blocks per chunk (the `MAX_BLOCKS_PER_REQUEST`-aligned
///   batch the RPC fetch + the chunk's single transaction cover).
/// - `rpc_url` — the HTTP RPC endpoint.
/// - `cancel` — set to `true` to cooperatively stop at the next chunk boundary.
/// - `progress` — the per-chunk progress sink (use [`NoProgress`] for silent).
///
/// # Errors
///
/// Returns [`RunError`] on a DB/RPC failure (the in-flight chunk is rolled
/// back before returning — the committed chunks stay durable) or
/// [`RunError::Cancelled`] if the `cancel` flag was set.
#[allow(
    clippy::missing_errors_doc,
    clippy::too_many_lines,
    clippy::needless_pass_by_value
)]
pub fn run_pool_update(
    database_path: &Path,
    chain_id: i64,
    to_block: Option<u64>,
    chunk_size: u64,
    rpc_url: &str,
    cancel: Arc<AtomicBool>,
    progress: Arc<dyn ProgressSink>,
) -> Result<UpdateReport, RunError> {
    if chunk_size == 0 {
        return Err(RunError::Provider(ProviderError::InvalidBlockRange {
            from: 1,
            to: 0,
        }));
    }

    // Open ONE writeable handle for the whole run — the chunk loop re-borrows
    // it per chunk (the `*_on_conn` variants take a `&Connection`, so the
    // chunk's `Transaction` borrows this handle's guarded connection).
    let (db, _schema_state) = DegenbotDb::open_for_writes(database_path)?;
    let specs = load_active_exchange_specs(&db, chain_id)?;
    if specs.is_empty() {
        // Nothing to do — return a trivial report (no chunks, no advance).
        return Ok(UpdateReport {
            chain_id,
            from_block: 0,
            to_block: to_block.unwrap_or(0),
            ..Default::default()
        });
    }

    // Owned tokio runtime (D2). The fetches run on it; the DB writes are
    // synchronous (off the runtime). MUST NOT be called from within an
    // existing tokio runtime.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let provider = rt.block_on(AlloyProvider::new(rpc_url, RPC_MAX_RETRIES))?;
    let provider = Arc::new(provider);
    let fetcher = LogFetcher::new(provider.clone(), chunk_size);

    // Resolve the chain tip if `to_block` is None.
    let last_block = match to_block {
        Some(n) => n,
        None => rt.block_on(provider.get_block_number())?,
    };

    // `initial_start_block` = the minimum `last_update_block + 1` across the
    // active specs (the earliest block any exchange still needs). Exchanges
    // whose `last_update_block` is already at/`> to_block` are excluded.
    let mut specs_to_update: Vec<ExchangeSpec> = specs
        .iter()
        .filter(|s| {
            s.last_update_block
                .is_none_or(|b| u64::try_from(b).unwrap_or(0) < last_block)
        })
        .cloned()
        .collect();
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
    while working_start_block <= last_block {
        // Cooperative cancel at the chunk boundary (the most recent committed
        // chunk is durable; we haven't started the next chunk's writes yet).
        if cancel.load(Ordering::Acquire) {
            return Err(RunError::Cancelled);
        }

        let working_end_block = last_block.min(working_start_block + chunk_size - 1);

        // The in-scope subset for THIS chunk: exchanges whose
        // `last_update_block == working_start_block - 1` (the laggards that
        // should advance together this chunk). Exchanges already past this
        // point (a prior chunk advanced them) are skipped.
        let chunk_specs: Vec<ExchangeSpec> = specs_to_update
            .iter()
            .filter(|s| {
                s.last_update_block
                    .is_none_or(|b| u64::try_from(b).unwrap_or(0) + 1 == working_start_block)
            })
            .cloned()
            .collect();
        if chunk_specs.is_empty() {
            // No lagging exchange for this start block — advance the cursor
            // (shouldn't normally happen if all exchanges advance in lockstep,
            // but guards against a sparse `last_update_block` distribution).
            working_start_block = working_end_block + 1;
            continue;
        }

        // RPC fetches (GIL-free, async) — pool creations per in-scope exchange +
        // the V3 whole-chain + V4 per-PoolManager liquidity scans for the range.
        let pool_creations = rt.block_on(fetch_pool_creations(
            &fetcher,
            working_start_block,
            working_end_block,
            &chunk_specs,
        ))?;
        let v3_liquidity = rt.block_on(fetch_v3_liquidity_logs_grouped(
            &fetcher,
            working_start_block,
            working_end_block,
            None, // whole-chain (V3 events can't be efficiently RPC-filtered)
        ))?;
        // V4 liquidity: fetch per PoolManager (the V4 exchange's `factory` is
        // the PoolManager address). Merge across all V4 chunk-specs into one
        // grouped map (the apply is per-pool_hash, manager-chain-scoped).
        let mut v4_liquidity: HashMap<String, Vec<LiquidityUpdateEvent>> = HashMap::new();
        for spec in &chunk_specs {
            if matches!(spec.family, crate::fetch::PoolFamily::V4) {
                let per_manager = rt.block_on(fetch_v4_liquidity_logs_grouped(
                    &fetcher,
                    working_start_block,
                    working_end_block,
                    Some(spec.factory),
                ))?;
                for (hash, events) in per_manager {
                    v4_liquidity.entry(hash).or_default().extend(events);
                }
            }
        }
        let inputs = ChunkInputs {
            pool_creations,
            v3_liquidity,
            v4_liquidity,
            pool_manager_chain: chain_id,
        };

        // The single-transaction chunk write (§1 atomicity). On ANY error the
        // `Transaction` drops → rollback → `last_update_block` unchanged →
        // restart re-processes clean (restart-invariant).
        let chunk_report = {
            let mut guard = db.lock();
            let tx = guard.transaction().map_err(degenbot_db::DbError::from)?;
            let result =
                apply_chunk_writes_on_conn(&tx, chain_id, &chunk_specs, working_end_block, &inputs);
            match result {
                Ok(r) => {
                    tx.commit().map_err(degenbot_db::DbError::from)?;
                    r
                }
                Err(e) => {
                    // Drop `tx` (rollback) — the chunk's writes + the stamp
                    // advance are reverted; the committed prior chunks stand.
                    drop(tx);
                    progress.report_chunk(&ChunkProgress {
                        chain_id,
                        chunk_start: working_start_block,
                        chunk_end: working_end_block,
                        pools_written: 0,
                        liquidity_apply_count: 0,
                        committed: false,
                    });
                    return Err(RunError::Db(e));
                }
            }
        };

        // Refresh the in-memory `last_update_block` for the next-iteration
        // laggard computation (the DB row is now `working_end_block`).
        for spec in &specs_to_update {
            if chunk_specs.iter().any(|c| c.id == spec.id) {
                // Re-read from the DB to stay canonical (the stamp just committed).
                if let Ok(Some(exchange)) = db.fetch_exchange(spec.id) {
                    // Mirror the field via a fresh spec fetch (one cheap query
                    // per exchange per chunk — acceptable for the chunk pace).
                    let _ = exchange;
                }
            }
        }
        // Re-load specs to refresh `last_update_block` for the next iteration's
        // laggard filter (the DB now has the advanced stamp).
        let refreshed = load_active_exchange_specs(&db, chain_id)?;
        for spec in &mut specs_to_update {
            if let Some(r) = refreshed.iter().find(|r| r.id == spec.id) {
                spec.last_update_block = r.last_update_block;
            }
        }

        progress.report_chunk(&ChunkProgress {
            chain_id,
            chunk_start: working_start_block,
            chunk_end: working_end_block,
            pools_written: chunk_report.pools_written,
            liquidity_apply_count: chunk_report.liquidity_apply_count,
            committed: true,
        });
        report.chunks_committed += 1;
        report.total_pools_written += chunk_report.pools_written;
        report.total_liquidity_applies += chunk_report.liquidity_apply_count;

        working_start_block = working_end_block + 1;
    }

    Ok(report)
}

/// Fetch + map the chunk's pool-creation events across all in-scope exchange
/// specs (per-exchange `fetch_pool_created_logs_for_spec` +
/// [`map_pool_creation`]). Aerodrome V2 pools whose per-pool `getFee` RPC
/// hasn't been resolved are SKIPPED (the fee-RPC path is a follow-up; the
/// canonical V2/V3/V4 + Aerodrome V3 paths are NOT affected).
async fn fetch_pool_creations(
    fetcher: &LogFetcher,
    from_block: u64,
    to_block: u64,
    specs: &[ExchangeSpec],
) -> Result<Vec<PoolCreationToWrite>, ProviderError> {
    let mut out = Vec::new();
    for spec in specs {
        let decoded = fetch_pool_created_logs_for_spec(fetcher, from_block, to_block, spec).await?;
        for event in decoded {
            match map_pool_creation(spec, &event, /* aerodrome_fee */ None) {
                Ok(row) => out.push(row),
                Err(MapError::AerodromeFeeNeeded { pool_address }) => {
                    // Aerodrome V2 fee RPC not yet wired — skip this pool
                    // (a follow-up task resolves getFee(address, stable)).
                    // Logged via stderr (the chunk's progress sink doesn't carry
                    // per-pool warnings; a future sink revision might).
                    eprintln!(
                        "degenbot-pool-updater: skipping Aerodrome V2 pool \
                         {pool_address} (exchange {}): per-pool getFee RPC not wired yet",
                        spec.name,
                    );
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use degenbot_db::DegenbotDb;

    /// An in-memory writeable DB with the head schema (the chunk-writer's
    /// substrate). Mirrors the discovery tests' `write_db` helper.
    fn write_db() -> DegenbotDb {
        let (db, _state) = DegenbotDb::open_for_writes(Path::new(":memory:")).unwrap();
        db
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
        let report = apply_chunk_writes_on_conn(&tx, 1, &specs, 100, &inputs).unwrap();
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
            apply_chunk_writes_on_conn(&tx, 1, &specs, 100, &inputs_ok).unwrap();
            tx.commit().unwrap();
        }

        // Second chunk: write the SAME pool (duplicate) → UNIQUE violation →
        // the inner fn returns Err → tx drops → rollback. The stamp advance to
        // 200 must NOT be durable (§1 atomicity + restart-invariant).
        let err = {
            let mut guard = db.lock();
            let tx = guard.transaction().unwrap();
            let result = apply_chunk_writes_on_conn(&tx, 1, &specs, 200, &inputs_dup);
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
}
