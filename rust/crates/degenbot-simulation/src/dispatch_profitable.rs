//! The `dispatch_profitable_results` fan-out + categorization (D-row).
//!
//! Ports `examples/eth_backrun_v2_v3_v4_rust.py::dispatch_profitable_results`
//! (the fan-out L2450–L2517 + categorization + summary L2519–L2535) + the
//! thin-margin pre-filter (`filter_thin_margin_results` from
//! `examples/eth_backrun_helpers.py::filter_thin_margin_results`, L407–L446,
//! the SYI3PG cross-epic reference).
//!
//! This is the concurrency orchestration that owns the GIL release + the
//! tokio fan-out over the per-path [`simulate_one`] leaf (sibling C-child).
//! Owning the fan-out in Rust releases the GIL across the per-tx sim RPCs
//! (ADR-005 §3 — "Rust is the engine").
//!
//! # Dispositions (per the `4JGPDW` scope rubric)
//!
//! - **D1 `port-now`** — the fan-out + categorization (this leaf). Pure
//!   concurrency orchestration: a `buffer_unordered(MAX_SIMULATE_CONCURRENT)`
//!   stream (capped by `truncate` pre-fan-out); pure-int categorization.
//! - **D2 `done`-reference** — [`degenbot_submission::PathSuppression`] (the
//!   M756BN leaf — `record_success`/`record_failure`/`is_suppressed`/
//!   `total_suppressed` + `PATH_SUPPRESS_THRESHOLD`). CONSUMED, not re-ported.
//! - **D3 `done`-reference** — [`filter_thin_margin_results`] (the SYI3PG
//!   cross-epic reference). The leaf was Python-only at the time of this
//!   port; it's small (20 lines, pure int) + has a clean standalone signature,
//!   so it is ported HERE (with a note it should later move to a shared
//!   crate). Re-porting it inline is the standalone-Rust-core constraint — a
//!   standalone consumer (`cargo add degenbot`) must NOT have to call into
//!   Python for the thin-margin pre-filter.
//! - **D4 `stays-python`** — the `[sim] ... by reason: {breakdown}` summary
//!   log rendering (`format_failure_breakdown`) stays in the Python driver;
//!   this leaf exposes the tally as a typed [`crate::simulate_one::FailBuckets`]
//!   the companion renders.

// Solidity/EVM + Rust-ecosystem identifiers (tokio, JoinSet, bps, PathSuppression,
// MAX_SIMULATE_CONCURRENT, etc.) are ubiquitous here.
#![allow(clippy::doc_markdown)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use alloy::primitives::U256;
use degenbot_bot::bot_core::BotState;
use degenbot_executor::composers::{EncodeOptions, PathInfo};
use degenbot_submission::PathSuppression;
use futures::stream::{self, StreamExt};
use parking_lot::RwLock;

use crate::{
    simulate_one, BlockSimHandle, FailBuckets, SimFailure, SimResult, SimulateContext, SimulatePath,
};

// ─────────────────────────────────────────────────────────────────────────
// Constants (ports the Python oracle's module-level literals)
// ─────────────────────────────────────────────────────────────────────────

/// The concurrent-simulation cap (`MAX_SIMULATE_CONCURRENT = 50`, L147) —
/// the `truncate` pre-fan-out cap + the `buffer_unordered` bound for the
/// legacy RPC path. Candidates beyond this are NOT simulated (the Python
/// oracle slices `results[:MAX_SIMULATE_CONCURRENT]`).
pub const MAX_SIMULATE_CONCURRENT: usize = 50;

/// The min net-profit threshold (`MIN_PROFIT_NET = 1`, L137) — a gross-
/// profitable result is "gas-profitable" iff `net_profit >= MIN_PROFIT_NET`.
/// Was `5 * 10**9` (5 gwei); relaxed to `1` wei to keep all gross-profitable
/// paths for the operator to inspect.
pub const MIN_PROFIT_NET: u128 = 1;

/// The basis-points denominator (`BPS_DENOM = 10_000`,
/// `examples/eth_backrun_helpers.py::BPS_DENOM`, L400).
pub const BPS_DENOM: u128 = 10_000;

// ─────────────────────────────────────────────────────────────────────────
// The thin-margin pre-filter (D3 — SYI3PG reference, ported inline)
// ─────────────────────────────────────────────────────────────────────────

/// Drop solver results whose gross-profit margin is too low (ports
/// `filter_thin_margin_results`, `eth_backrun_helpers.py` L407–L446).
///
/// S1 found that the dominant IIA reverts in V3/V4-heavy perms are razor-thin
/// arb (gross profit ≈ $0.001 on ≈ $0.06–$1.30 input = sub-0.2 bps margin)
/// that cannot survive 1-block drift — the chain has already arbitraged these
/// away by the time the sim runs. Filtering them pre-sim saves an RPC + a
/// revert per attempt + keeps them out of the TSV `Reverts` column.
///
/// `min_profit_margin_bps` is in basis points of `optimal_input` (e.g. `50` =
/// 0.5% — a result with profit < 0.5% of its input is dropped). `0` disables
/// the filter (keeps all — backwards-compatible default).
///
/// Returns `(kept, dropped_count)` — pure int arithmetic (no float):
/// a result is kept iff `profit * BPS_DENOM >= optimal_input *
/// min_profit_margin_bps`, OR `optimal_input == 0` (no input basis to ratio
/// against).
///
/// # §4.2 note
///
/// Ports the reference from `examples/eth_backrun_helpers.py`. The leaf is
/// standalone-usable (pure int + zero allocation beyond the kept vec); it is
/// defined here so a standalone Rust consumer (`cargo add degenbot`) doesn't
/// need Python for the pre-filter. It may later move to a shared crate if a
/// second consumer emerges.
#[must_use]
pub fn filter_thin_margin_results(
    results: Vec<DispatchCandidate>,
    min_profit_margin_bps: u64,
) -> (Vec<DispatchCandidate>, usize) {
    if min_profit_margin_bps == 0 || results.is_empty() {
        return (results, 0);
    }
    let threshold_num = U256::from(min_profit_margin_bps);
    let denom = U256::from(BPS_DENOM);
    let mut kept = Vec::with_capacity(results.len());
    let mut dropped = 0usize;
    for row in results {
        let opt_input = U256::from(row.optimal_input);
        let profit = U256::from(row.engine_profit);
        // `profit * BPS_DENOM >= opt_input * min_profit_margin_bps` ⟺ kept.
        // (integer math — no float rounding.) A zero `opt_input` is kept (no
        // basis to ratio against — ports the `opt_input == 0` branch).
        let is_enough_margin = !opt_input.is_zero() && profit * denom >= opt_input * threshold_num;
        if opt_input.is_zero() || is_enough_margin {
            kept.push(row);
        } else {
            dropped += 1;
        }
    }
    (kept, dropped)
}

// ─────────────────────────────────────────────────────────────────────────
// The candidate + outcome types (D1)
// ─────────────────────────────────────────────────────────────────────────

/// A pre-simulation candidate — the engine result + the resolved [`PathInfo`].
///
/// Ports the `EngineResult` tuple `(path_id, opt_input, profit, hop_outputs,
/// consumed_inputs, solve_block)` (L1672 + L2486). The Python oracle resolves
/// the `PathInfo` from `engine_registry.paths.get(path_id)`; this leaf takes
/// it pre-resolved (the registry lookup is the caller's concern — the
/// `degenbot` umbrella's `Bot` owns the engine registry).
#[derive(Debug, Clone)]
pub struct DispatchCandidate {
    /// `path_id` — the unique arb path identifier.
    pub path_id: u64,
    /// `optimal_input` — the solver's optimal swap input.
    pub optimal_input: u128,
    /// `engine_profit` — the solver's expected gross profit (used for sorting
    /// and the thin-margin filter; NOT the on-chain gross — that's
    /// [`SimResult::gross_profit`]).
    pub engine_profit: u128,
    /// `hop_outputs` — the per-hop solver outputs.
    pub hop_outputs: Vec<u128>,
    /// `solve_block` — the block the solver produced the result on.
    pub solve_block: u64,
    /// `path_info` — the resolved path hops (consumed by `encode_cmd_stream`).
    pub path_info: PathInfo,
    /// `opts` — the encode options (`erc6909_profit` / `use_v4_batch`).
    pub opts: EncodeOptions,
}

impl DispatchCandidate {
    /// Build the [`SimulatePath`] the fan-out hands to [`simulate_one`].
    #[must_use]
    fn to_simulate_path(&self) -> SimulatePath {
        SimulatePath {
            path_id: self.path_id,
            optimal_input: self.optimal_input,
            hop_outputs: self.hop_outputs.clone(),
            path_info: self.path_info.clone(),
            solve_block: self.solve_block,
            opts: self.opts,
        }
    }
}

/// The dispatch fan-out outcome — the categorization + tallies (D1).
///
/// Ports the closure locals `gas_profitable` / `gas_unprofitable` /
/// `exception_count` / `_fail_buckets` (L2519–L2535 + the summary log).
/// The `[sim] ... by reason: {breakdown}` RENDERING stays Python (D4); this
/// struct exposes the typed buckets the companion renders.
#[derive(Debug, Default)]
pub struct DispatchOutcome {
    /// Gas-profitable results (net ≥ [`MIN_PROFIT_NET`]), sorted by net profit
    /// descending (L2561).
    pub gas_profitable: Vec<SimResult>,
    /// Onchain-valid but gas-unprofitable (gross > 0, net below threshold),
    /// sorted by net profit descending (L2563).
    pub gas_unprofitable: Vec<SimResult>,
    /// The number of candidates that raised an exception during simulation
    /// (ports `exception_count`, L2536).
    pub exception_count: usize,
    /// The number of candidates that returned `None` from `simulate_one`
    /// (revert / no-profit / int128-overflow / etc.) — `sim_fail_count`
    /// (L2537): `len(candidates) - sim_ok_count - exception_count`.
    pub fail_count: usize,
    /// The number of candidates passed to simulation (after the pre-filters
    /// + the `MAX_SIMULATE_CONCURRENT` cap). Ports `len(candidates)` (L2491).
    pub candidate_count: usize,
    /// The number of candidates dropped by [`PathSuppression::is_suppressed`]
    /// (ports `suppressed_count`, L2489).
    pub suppressed_count: usize,
    /// The number of candidates dropped by [`filter_thin_margin_results`]
    /// (ports `thin_dropped`, L2499).
    pub thin_dropped: usize,
    /// The `_fail_buckets` tally — the revert/no-profit/overflow buckets
    /// accumulated across the fan-out (ports `_fail_buckets`, L1769). The
    /// aggregation rendering (`format_failure_breakdown`) stays Python (D4);
    /// this exposes the typed buckets the companion renders.
    pub fail_buckets: FailBuckets,
    /// The per-path `SimFailure` records (one per `tally`/`record` site across
    /// the fan-out), in fan-out completion order — surfaced across the FFI so
    /// the Python driver can render a per-candidate `[sim-fail]` line carrying
    /// `path_id` + `fail_index` + the raw revert bytes. The aggregation
    /// (`fail_buckets`) collapses these into a count; this preserves the
    /// per-candidate attribution the operator needs to identify WHICH path
    /// reverted against WHICH pools.
    pub failures: Vec<SimFailure>,
}

impl DispatchOutcome {
    /// The total `sim_ok_count` — `len(gas_profitable) + len(gas_unprofitable)`
    /// (ports L2536).
    #[must_use]
    pub fn sim_ok_count(&self) -> usize {
        self.gas_profitable.len() + self.gas_unprofitable.len()
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The fan-out (D1)
// ─────────────────────────────────────────────────────────────────────────

/// Fan out [`simulate_one`] across a `buffer_unordered(MAX_SIMULATE_CONCURRENT)`
/// stream (capped by `truncate` pre-fan-out), gather with exception tolerance,
/// and categorize into gas-profitable / gas-unprofitable / exception (ports the
/// fan-out L2450–L2517 + categorization L2519–L2535).
///
/// Pipeline:
/// 1. **Pre-filter — suppression** — drop paths currently suppressed by
///    `path_suppression.is_suppressed(pid, current_block)` (the retry-interval
///    logic is owned by [`PathSuppression`]).
/// 2. **Pre-filter — thin-margin** — drop razor-thin arb via
///    [`filter_thin_margin_results`] (`min_profit_margin_bps`; `0` disables).
/// 3. **Cap** — take the first [`MAX_SIMULATE_CONCURRENT`] candidates (the
///    Python oracle slices `results[:MAX_SIMULATE_CONCURRENT]`; the candidates
///    are expected pre-sorted by engine profit descending — the caller does
///    the sort, ports L1684).
/// 4. **Fan-out** — two branches on `bot_state`. When `Some`, the in-process
///    revm path builds ONE per-block `BlockSimHandle` + simulates each
///    candidate SERIALLY on the shared `&mut evm` (Tier 1, `V5HCR5`). When
///    `None`, the legacy RPC path drives `simulate_one` per candidate through
///    `buffer_unordered(MAX_SIMULATE_CONCURRENT)` (the bound mirrors the
///    Python `asyncio.gather(*sim_tasks)` concurrency — the L2491 `truncate`
///    already caps the count, the `buffer_unordered` bound is belt +
///    suspenders for a caller that hands more candidates).
/// 5. **Gather** — collect the stream; exceptions are tolerated (counted, not
///    propagated — the Python oracle uses `return_exceptions=True`,
///    L2504).
/// 6. **Categorize** — gas-profitable (`net ≥ min_profit_net`) /
///    gas-unprofitable (`None`-filtered, gross > 0, net below threshold) /
///    exception. Sort both categories by net profit descending (L2561/L2563).
/// 7. **Record suppression outcomes** — `record_success(pid)` for paths that
///    returned a result, `record_failure(pid)` for paths that didn't
///    (L2573–L2577).
///
/// `ctx` is the shared [`SimulateContext`] (provider, addresses, warmup,
/// block context — see [`simulate_one`]). `path_suppression` is mutated in
/// place for the pre-filter + the outcome recording.
///
/// # §4.2 parity
///
/// The categorization buckets, the pre-filter drops, and the suppression
/// transitions match the Python oracle's `dispatch_profitable_results`
/// behavior (the `[sim] N candidates: X ok (Y profitable, Z below threshold),
/// W failed, V exceptions` summary L2538–L2542). The summary RENDERING stays
/// Python (D4); this leaf returns the typed [`DispatchOutcome`].
///
/// # Failure model
///
/// Infallible — returns `DispatchOutcome` directly (not a `Result`). Every
/// per-path failure (revert / no-profit / rpc-failed / int128-overflow) is
/// tallied into `outcome.fail_buckets` + recorded as a per-candidate
/// [`SimFailure`] (ports the Python oracle's `return_exceptions=True`
/// tolerance, L2504 — no failure propagates as an `Err`). The
/// `exception_count` field captures `simulate_one` `Err` returns (the rare
/// defensive `?` on the execute() ABI encode), counted not propagated.
///
/// # Panics
///
/// Panics if the `path_suppression` mutex is poisoned (a peer task panicked
/// while holding it). The mutex is locked ONLY at the bookends (step 1
/// pre-filter + step 6 outcome record) — both synchronous spans, no `.await`
/// held under the guard — so a poison indicates a bug in a sibling task
/// (the suppression arc is shared with the submission seam's accessors;
/// never locked across an `.await`).
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub async fn dispatch_profitable_results(
    mut candidates: Vec<DispatchCandidate>,
    ctx: &SimulateContext<'_>,
    path_suppression: &Arc<Mutex<PathSuppression>>,
    current_block: u64,
    min_profit_net: u128,
    min_profit_margin_bps: u64,
    // When `Some`, the fan-out routes each candidate through the in-process
    // revm sim over the borrowed `&BotState` via a per-block shared
    // `BlockSimHandle` (Tier 1, `V5HCR5` — retired the per-path
    // `simulate_in_process` fresh-`CacheDB`-per-call build), replacing the
    // `eth_simulateV1` RPC path (`simulate_one`) for the dispatch. When
    // `None`, the fan-out uses `simulate_one` (the legacy concurrent RPC path
    // — the default). The `Arc<RwLock<BotState>>` is the engine's shared
    // state owner (ADR-003); a per-block read guard is taken for the serial
    // loop. revm's `WrapDatabaseAsync::block_on` uses
    // `tokio::task::block_in_place` under a multi-threaded runtime, so the
    // in-process sim's blocking RPC cold-miss path does not deadlock against
    // the pump's worker pool.
    bot_state: Option<Arc<RwLock<BotState>>>,
    // The cross-block persistent bytecode + account-existence cache
    // (`WarmCodeCacheInner`, the `HDEG7H` Option-A layer). Required when
    // `bot_state` is `Some` (the `BlockSimHandle` path always inserts the
    // `WarmCodeCache` layer); ignored when `bot_state` is `None` (the legacy
    // `simulate_one` RPC path doesn't go through `BlockSimHandle`). The
    // `Arc` clones cheaply into this async fn's future; the engine owner
    // (`PyArbitrageEngine` / standalone `Bot`) holds it for the engine's
    // life. If `Some(bot_state)` arrives with `None` warm_cache (a caller
    // wiring gap), a fresh `WarmCodeCacheInner::shared_default()` is
    // constructed per call — a safe degradation to Tier-1-only behavior (no
    // cross-block persistence, no panic). The FFI seam sets both from the
    // same `engine` arg, so the gap is unreachable in production.
    warm_cache: Option<Arc<RwLock<degenbot_evm::WarmCodeCacheInner>>>,
) -> DispatchOutcome {
    let mut outcome = DispatchOutcome::default();
    let pre_filter_count = candidates.len();

    // 1. Pre-filter — suppression (L2486–L2490). Lock the suppression arc
    //    ONLY for this synchronous retain (the guard is dropped before the
    //    fan-out `.await` so the future stays `Send` — a `std::sync::MutexGuard`
    //    is not `Send`). A3 (`LITQFF`) extracted `PathSuppression` onto its own
    //    arc precisely so this bookend scope never contends with the
    //    `Dispatcher` arc the monitor tasks lock.
    {
        let mut s = path_suppression.lock().expect("suppression mutex poisoned");
        candidates.retain(|c| !s.is_suppressed(c.path_id, current_block));
    }
    outcome.suppressed_count = pre_filter_count - candidates.len();

    // 2. Pre-filter — thin-margin (L2497–L2499).
    let (kept, thin_dropped) = filter_thin_margin_results(candidates, min_profit_margin_bps);
    outcome.thin_dropped = thin_dropped;
    candidates = kept;

    // 3. Cap (L2491). The candidates are expected pre-sorted by engine profit
    //    descending (the caller's responsibility — ports L1684's sort).
    candidates.truncate(MAX_SIMULATE_CONCURRENT);
    outcome.candidate_count = candidates.len();

    // 4. Fan-out — two branches on `bot_state`. When `Some`, the in-process
    //    revm path builds ONE per-block `BlockSimHandle` + simulates each
    //    candidate SERIALLY on the shared `&mut evm` (Tier 1, `V5HCR5`). When
    //    `None`, the legacy RPC path spawns one `simulate_one` per candidate
    //    with a bounded-concurrency stream. `buffer_unordered(MAX_SIMULATE_CONCURRENT)` is
    //    the Rust idiom for the Python `asyncio.gather(*sim_tasks)` + the
    //    `results[:MAX_SIMULATE_CONCURRENT]` cap (the cap is double-asserted:
    //    the L2491 truncate + the buffer bound — belt + suspenders).
    //    Unlike `tokio::spawn`, `buffer_unordered` does NOT require `'static` —
    //    the stream borrows `ctx` for its lifetime, and we collect within this
    //    fn (no task outlives the call).
    let candidate_path_ids: Vec<u64> = candidates.iter().map(|c| c.path_id).collect();
    let sim_results: Vec<(u64, FailBuckets, Result<Option<SimResult>, String>)> = match bot_state {
        // In-process revm path (Tier 1, `V5HCR5`): build ONE per-block EVM
        // (`BlockSimHandle`) and simulate each candidate SERIALLY on the
        // shared `&mut evm`. The shared `CacheDB` captures the ~50× latency
        // win (benchmark `examples/rpc_cache_fanout.rs` config B vs A): the
        // trigger path pays the cold RPCs, the fan-out hits the warmed cache
        // at ~1 µs p50. Serial (not `buffer_unordered`) because a shared
        // `&mut evm` can't be held across `'static`+`Send` futures — the
        // per-path RPC cold-miss dwarfs the per-path EVM execution, so
        // serial-warm beats parallel-cold. Per-path isolation is revm's
        // `finalize()` (execute() SSTOREs live in the per-path `State`, never
        // committed to the shared `CacheDB`). `parking_lot`'s read guard is
        // held for the serial loop's duration.
        Some(arc) => {
            let guard = arc.read();
            // The warm-code cache arc; degrade to a fresh per-call cache if
            // the caller wired `bot_state` without one (safe — no
            // cross-block persistence, no panic).
            let warm_cache = warm_cache
                .clone()
                .unwrap_or_else(degenbot_evm::WarmCodeCacheInner::shared_default);
            match BlockSimHandle::build(ctx, &guard, &warm_cache) {
                Some(mut handle) => candidates
                    .into_iter()
                    .map(|c| {
                        let pid = c.path_id;
                        let mut buckets = FailBuckets::new();
                        let result = handle
                            .simulate_path(ctx, c.to_simulate_path(), &mut buckets)
                            .map_err(|e| format!("{e}"));
                        (pid, buckets, result)
                    })
                    .collect(),
                // Build failure — no ambient runtime or an override-
                // application error. The whole block's sim is dead: tally
                // `rpc-failed` for every candidate (mirrors the retired
                // per-path build-failure tally).
                None => candidates
                    .into_iter()
                    .map(|c| {
                        let mut buckets = FailBuckets::new();
                        buckets.record(
                            c.path_id,
                            "rpc-failed",
                            None,
                            alloy::primitives::Bytes::new(),
                        );
                        (c.path_id, buckets, Ok(None))
                    })
                    .collect(),
            }
        }
        // Legacy RPC path (`eth_simulateV1`) — concurrent
        // `buffer_unordered` + `simulate_one`. The default when no `bot_state`
        // is supplied.
        None => {
            stream::iter(
                candidates
                    .into_iter()
                    .map(|c| (c.path_id, c.to_simulate_path())),
            )
            .map(|(pid, path)| async move {
                let mut buckets = FailBuckets::new();
                let result: Result<Option<SimResult>, String> =
                    simulate_one(ctx, path, &mut buckets)
                        .await
                        .map_err(|e| format!("{e}"));
                match result {
                    Ok(opt) => (pid, buckets, Ok(opt)),
                    Err(e) => (pid, buckets, Err(e)),
                }
            })
            .buffer_unordered(MAX_SIMULATE_CONCURRENT)
            .collect()
            .await
        }
    };

    // 5. Categorize — gas-profitable / gas-unprofitable / exception (ports
    //    L2519–L2557). `simulate_one` returns `Ok(Some(result))` for gross-
    //    profitable paths; `Ok(None)` for revert/no-profit/overflow (tallied
    //    into `buckets`); `Err(_)` for an unrecoverable RPC failure (counted
    //    as an exception — ports `return_exceptions=True`, L2504).
    let mut succeeded_path_ids: HashSet<u64> = HashSet::new();
    for (pid, buckets, result) in sim_results {
        // Merge the per-path buckets into the outcome tally first.
        for (bucket, count) in buckets.buckets() {
            let entry = outcome
                .fail_buckets
                .buckets_mut()
                .entry(bucket.clone())
                .or_insert(0);
            *entry += count;
        }
        // Surface the per-candidate failure detail (`path_id` + `fail_index` +
        // `revert_data`) across the FFI so the Python driver can render a
        // `[sim-fail]` line. `fail_buckets.tally` was paired with `record`
        // (pushing a `SimFailure`) at every site that should be surfaced;
        // `into_failures()` moves them out of the dropped `buckets` instance
        // into the outcome without a clone.
        outcome.failures.extend(buckets.into_failures());
        match result {
            Ok(Some(r)) => {
                succeeded_path_ids.insert(pid);
                if r.net_profit >= U256::from(min_profit_net) {
                    outcome.gas_profitable.push(r);
                } else {
                    outcome.gas_unprofitable.push(r);
                }
            }
            Ok(None) => {
                // Already tallied into `buckets` above.
            }
            Err(_msg) => {
                outcome.exception_count += 1;
            }
        }
    }

    // `fail_count = len(candidates) - sim_ok_count - exception_count` (L2537).
    outcome.fail_count = outcome.candidate_count - outcome.sim_ok_count() - outcome.exception_count;

    // 6. Record suppression outcomes (L2573–L2577). Ports
    //    `record_success(pid)` for succeeded, `record_failure(pid)` for the
    //    rest (revert/no-profit/overflow/exception). Scope-locked (same
    //    bookend-only discipline as step 1 — the guard drops before any
    //    `.await`; there is no `.await` in this synchronous loop anyway).
    {
        let mut s = path_suppression.lock().expect("suppression mutex poisoned");
        for &pid in &candidate_path_ids {
            if succeeded_path_ids.contains(&pid) {
                s.record_success(pid);
            } else {
                s.record_failure(pid);
            }
        }
    }

    // 7. Sort both categories by net profit descending (L2561/L2563).
    outcome
        .gas_profitable
        .sort_by_key(|r| std::cmp::Reverse(r.net_profit));
    outcome
        .gas_unprofitable
        .sort_by_key(|r| std::cmp::Reverse(r.net_profit));

    outcome
}

#[cfg(test)]
mod tests {
    #![allow(clippy::too_many_lines)]

    use super::*;
    use crate::BlockPriorityFees;
    use alloy::primitives::{address, Address, Bytes, U256};
    use alloy::providers::{Provider, ProviderBuilder};
    use alloy::rpc::client::ClientBuilder;
    use alloy::rpc::types::eth::simulate::{SimCallResult, SimulatedBlock};
    use alloy::transports::mock::{Asserter, MockTransport};
    use degenbot_executor::composers::{HopInfo, PathInfo, V2HopInfo};
    use degenbot_executor::{compute_simulation_warmup_slots, WarmupSlots};
    use degenbot_rpc::provider::{AlloyProvider, EthBlock};
    use std::sync::{Arc, Mutex};

    const OWNER: Address = address!("9c56a29c7231974c269e24f9fb3c29203039089e");
    const EXECUTOR: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    const PM: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");
    const MULTICALL3: Address = address!("c411372f0b8ae58585e33b78aea9e0596da9a6f1");

    fn mock_provider(asserter: &Asserter) -> AlloyProvider {
        let client = ClientBuilder::default().transport(MockTransport::new(asserter.clone()), true);
        let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
        AlloyProvider::from_provider(
            Arc::new(dyn_provider) as Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>
        )
    }

    fn warmup() -> WarmupSlots {
        compute_simulation_warmup_slots(EXECUTOR, WETH, PM)
    }

    fn ctx(provider: &AlloyProvider) -> SimulateContext<'_> {
        SimulateContext {
            provider,
            executor_owner: OWNER,
            executor_address: EXECUTOR,
            weth_address: WETH,
            pool_manager_address: PM,
            multicall3_address: MULTICALL3,
            inject_code: true,
            injected_address: Some(EXECUTOR),
            runtime_bytecode: Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]),
            warmup: warmup(),
            base_fee_next: 1_000_000_000u128, // 1 gwei
            current_block: 100,
            block_timestamp: 0,
            block_priority_fees: Some(BlockPriorityFees {
                block: 100,
                p10: U256::from(500_000_000u64),   // 0.5 gwei
                p50: U256::from(2_000_000_000u64), // 2 gwei
            }),
        }
    }

    fn two_v2_hops() -> PathInfo {
        PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                token0_address: WETH,
                token1_address: address!("1111111111111111111111111111111111111111"),
                fee: 30,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("cccccccccccccccccccccccccccccccccccccccc"),
                token0_address: address!("1111111111111111111111111111111111111111"),
                token1_address: WETH,
                fee: 30,
                zfo: true,
            }),
        ])
    }

    fn candidate(path_id: u64, opt_input: u128, profit: u128) -> DispatchCandidate {
        DispatchCandidate {
            path_id,
            optimal_input: opt_input,
            engine_profit: profit,
            hop_outputs: vec![opt_input * 11 / 10, opt_input * 121 / 100],
            solve_block: 100,
            path_info: two_v2_hops(),
            opts: EncodeOptions {
                erc6909_profit: false,
                use_v4_batch: false,
            },
        }
    }

    fn ok_call(balance: U256) -> SimCallResult {
        SimCallResult {
            return_data: Bytes::from(balance.to_be_bytes::<32>().to_vec()),
            logs: Vec::new(),
            gas_used: 30_000,
            max_used_gas: None,
            status: true,
            error: None,
        }
    }

    fn ok_gas(gas: u64) -> SimCallResult {
        SimCallResult {
            return_data: Bytes::new(),
            logs: Vec::new(),
            gas_used: gas,
            max_used_gas: None,
            status: true,
            error: None,
        }
    }

    /// Build a 7-call simulated block where the balances yield a known gross.
    fn profit_block(gross: U256, gas_used: u64) -> Vec<SimulatedBlock<EthBlock>> {
        let weth_before = U256::from(10_000_000_000_000_000_000u128);
        let eth_before = U256::from(5_000_000_000_000_000_000u128);
        let erc_before = U256::ZERO;
        let weth_after = weth_before + gross;
        let eth_after = eth_before;
        let erc_after = erc_before;
        let calls = vec![
            ok_call(weth_before),
            ok_call(eth_before),
            ok_call(erc_before),
            ok_gas(gas_used),
            ok_call(weth_after),
            ok_call(eth_after),
            ok_call(erc_after),
        ];
        vec![SimulatedBlock {
            inner: EthBlock::default(),
            calls,
        }]
    }

    fn access_list_response() -> serde_json::Value {
        serde_json::json!({"accessList": [], "gasUsed": "0x0"})
    }

    /// Push the 2 canned responses (access-list + simulate) for a profitable
    /// path into the asserter.
    fn push_profitable(asserter: &Asserter, gross: U256, gas: u64) {
        asserter.push_success(&access_list_response());
        asserter.push_success(&profit_block(gross, gas));
    }

    // ── D2: PathSuppression consumption (done-reference) ──────────────────

    #[tokio::test]
    async fn dispatch_records_failure_for_reverted_paths() {
        // One profitable candidate + one no-profit candidate → the no-profit
        // path records a failure; the profitable path records a success.
        let asserter = Asserter::new();
        // Candidate 1: gross 2 ETH (profitable).
        push_profitable(
            &asserter,
            U256::from(2_000_000_000_000_000_000u128),
            200_000,
        );
        // Candidate 2: gross 0 (no-profit → None → record_failure).
        asserter.push_success(&access_list_response());
        asserter.push_success(&profit_block(U256::ZERO, 200_000));
        let provider = mock_provider(&asserter);
        let suppression = Arc::new(Mutex::new(PathSuppression::new()));

        let cands = vec![
            candidate(
                1,
                1_000_000_000_000_000_000u128,
                2_000_000_000_000_000_000u128,
            ),
            candidate(2, 1_000_000_000_000_000_000u128, 0),
        ];
        let outcome = dispatch_profitable_results(
            cands,
            &ctx(&provider),
            &suppression,
            100,
            MIN_PROFIT_NET,
            0,
            None,
            None,
        )
        .await;

        // Categorization: 1 profitable, 1 no-profit (→ fail_count=1).
        assert_eq!(outcome.gas_profitable.len(), 1);
        assert_eq!(outcome.gas_unprofitable.len(), 0);
        assert_eq!(outcome.fail_count, 1);
        assert_eq!(outcome.exception_count, 0);
        assert_eq!(outcome.candidate_count, 2);
        // Suppression: path 2 has 1 failure (not yet suppressed — threshold 10).
        assert_eq!(
            suppression
                .lock()
                .expect("suppression mutex poisoned")
                .total_suppressed(),
            0
        );
    }

    #[tokio::test]
    async fn dispatch_suppresses_path_after_threshold_failures() {
        // The threshold is 10 (PATH_SUPPRESS_THRESHOLD). Failing the same path
        // 10 times should put it in the suppressed set.
        let asserter = Asserter::new();
        for _ in 0..10 {
            asserter.push_success(&access_list_response());
            asserter.push_success(&profit_block(U256::ZERO, 100_000)); // no-profit
        }
        let provider = mock_provider(&asserter);
        let suppression = Arc::new(Mutex::new(PathSuppression::new()));

        for _ in 0..10 {
            let cands = vec![candidate(7, 1_000_000_000_000_000_000u128, 0)];
            let _ = dispatch_profitable_results(
                cands,
                &ctx(&provider),
                &suppression,
                100,
                MIN_PROFIT_NET,
                0,
                None,
                None,
            )
            .await;
        }
        // After 10 failures, path 7 is suppressed.
        assert!(suppression
            .lock()
            .expect("suppression mutex poisoned")
            .is_in_suppressed_set(7));
        assert_eq!(
            suppression
                .lock()
                .expect("suppression mutex poisoned")
                .total_suppressed(),
            1
        );

        // The NEXT dispatch with the same path at block 100 → suppressed_count=1
        // (is_suppressed returns true; no RPC dispatched — the asserter queue
        // is untouched).
        let asserter2 = Asserter::new();
        asserter2.push_success(&access_list_response()); // sentinel (should stay unconsumed)
        let provider2 = mock_provider(&asserter2);
        let outcome = dispatch_profitable_results(
            vec![candidate(7, 1_000_000_000_000_000_000u128, 0)],
            &ctx(&provider2),
            &suppression,
            50, // < retry interval (100) → still suppressed (last_retry stays 0)
            MIN_PROFIT_NET,
            0,
            None,
            None,
        )
        .await;
        assert_eq!(outcome.suppressed_count, 1);
        assert_eq!(outcome.candidate_count, 0);
        // The sentinel response is still unconsumed.
        assert!(!asserter2.read_q().is_empty());
    }

    #[tokio::test]
    async fn dispatch_predilters_suppressed_paths() {
        // A suppressed path is dropped pre-sim — no RPC dispatched.
        let asserter = Asserter::new();
        asserter.push_success(&access_list_response()); // sentinel
        let provider = mock_provider(&asserter);
        let suppression = Arc::new(Mutex::new(PathSuppression::new()));
        // Manually suppress path 99.
        for _ in 0..10 {
            suppression
                .lock()
                .expect("suppression mutex poisoned")
                .record_failure(99);
        }
        assert!(suppression
            .lock()
            .expect("suppression mutex poisoned")
            .is_in_suppressed_set(99));

        let outcome = dispatch_profitable_results(
            vec![candidate(99, 1_000_000_000_000_000_000u128, 1_000)],
            &ctx(&provider),
            &suppression,
            50, // < retry interval (100) → still suppressed
            MIN_PROFIT_NET,
            0,
            None,
            None,
        )
        .await;
        assert_eq!(outcome.suppressed_count, 1);
        assert_eq!(outcome.candidate_count, 0);
        // No RPC dispatched (sentinel still in queue).
        assert!(!asserter.read_q().is_empty());
    }

    // ── D3: filter_thin_margin_results ────────────────────────────────────

    #[test]
    fn thin_margin_zero_bps_keeps_all() {
        let cands = vec![
            candidate(1, 1_000, 1),     // 0.1 bps — tiny
            candidate(2, 1_000, 1_000), // 100 bps — huge
        ];
        let (kept, dropped) = filter_thin_margin_results(cands, 0);
        assert_eq!(kept.len(), 2);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn thin_margin_50_bps_drops_low_profit() {
        // 50 bps = 0.5%. profit * 10000 >= opt_input * 50.
        // candidate A: opt=1_000_000, profit=4_999 → 4_999*10000=49_990_000;
        //   opt*50=50_000_000 → 49_990_000 < 50_000_000 → DROPPED (< 0.5%).
        // candidate B: opt=1_000_000, profit=5_000 → 50_000_000 >= 50_000_000 → KEPT.
        // candidate C: opt=0, profit=any → kept (no basis).
        let cands = vec![
            candidate(1, 1_000_000, 4_999),
            candidate(2, 1_000_000, 5_000),
            candidate(3, 0, 100),
        ];
        let (kept, dropped) = filter_thin_margin_results(cands, 50);
        assert_eq!(kept.len(), 2);
        assert_eq!(dropped, 1);
        // The dropped one is the sub-0.5% path 1.
        let kept_ids: Vec<u64> = kept.iter().map(|c| c.path_id).collect();
        assert!(!kept_ids.contains(&1));
        assert!(kept_ids.contains(&2));
        assert!(kept_ids.contains(&3));
    }

    #[test]
    fn thin_margin_empty_input_keeps() {
        let (kept, dropped) = filter_thin_margin_results(Vec::new(), 50);
        assert!(kept.is_empty());
        assert_eq!(dropped, 0);
    }

    #[test]
    fn thin_margin_uses_exact_integer_arithmetic() {
        // 99 bps. profit=990 on opt=1_000_000 → 990*10000=9_900_000;
        //   1_000_000*99=99_000_000 → DROPPED.
        // profit=9_900 → 99_000_000 >= 99_000_000 → KEPT (exactly at threshold).
        let cands = vec![candidate(1, 1_000_000, 990), candidate(2, 1_000_000, 9_900)];
        let (kept, dropped) = filter_thin_margin_results(cands, 99);
        assert_eq!(kept.len(), 1);
        assert_eq!(dropped, 1);
        assert_eq!(kept[0].path_id, 2);
    }

    // ── D1: categorization buckets ────────────────────────────────────────

    #[tokio::test]
    async fn dispatch_categorizes_gas_profitable_and_unprofitable() {
        // Two profitable paths: one with a huge net (gas-profitable), one
        // with a tiny net (gas-unprofitable, below MIN_PROFIT_NET).
        // Path A: gross 2 ETH, gas 200_000 → net = 2e18 - 200_000*(1e9+2e9+1).
        //   = 2e18 - 200_000 * 3_000_000_001 = 2e18 - 600_000_000_200_000 ≈ 1.9994e18 → profitable.
        // Path B: gross 1 gwei (1e9), gas 1_000_000 → net = 1e9 - 1e6*(1e9+2e9+1) < 0 → but
        //   simulate_one returns Some only if gross > 0; net may be < MIN_PROFIT_NET (1) → unprofitable.
        //   But wait: net = gross - gas_fee. If net < 0, saturating_sub → 0. 0 < 1 → unprofitable.
        let asserter = Asserter::new();
        push_profitable(
            &asserter,
            U256::from(2_000_000_000_000_000_000u128),
            200_000,
        );
        // Path B: gross = 1e9 wei, but the balance decode requires gross > 0; use 1e9.
        // gas huge → net clamps to ~0 (unprofitable).
        push_profitable(&asserter, U256::from(1_000_000_000u128), 50_000_000);
        let provider = mock_provider(&asserter);
        let suppression = Arc::new(Mutex::new(PathSuppression::new()));

        let cands = vec![
            candidate(
                10,
                1_000_000_000_000_000_000u128,
                2_000_000_000_000_000_000u128,
            ),
            candidate(11, 1_000_000_000_000u128, 1_000_000_000u128), // small path
        ];
        let outcome = dispatch_profitable_results(
            cands,
            &ctx(&provider),
            &suppression,
            100,
            MIN_PROFIT_NET,
            0,
            None,
            None,
        )
        .await;

        assert_eq!(outcome.candidate_count, 2);
        assert_eq!(
            outcome.gas_profitable.len(),
            1,
            "buckets: {:?}",
            outcome.fail_buckets.buckets()
        );
        assert_eq!(outcome.gas_unprofitable.len(), 1);
        assert_eq!(outcome.exception_count, 0);
        assert_eq!(outcome.fail_count, 0);
        // Sorted by net descending: the profitable path (big net) is first.
        assert!(outcome.gas_profitable[0].net_profit > outcome.gas_unprofitable[0].net_profit);
        // Both paths recorded success (both returned Some).
        assert_eq!(
            suppression
                .lock()
                .expect("suppression mutex poisoned")
                .total_suppressed(),
            0
        );
    }

    /// A6 golden-fixture parity: pin the EXACT gross/net/gas/priority-fee
    /// figures on a gas-profitable survivor. The categorizes test (above)
    /// asserts the profitable SUBSET + a net comparison; this test pins the
    /// absolute ints so a drift in the fee arithmetic
    /// (`compute_priority_fee` / `gas_fee` / `net`) is caught as a figure
    /// change, not just a category flip. The fixture mirrors the deleted
    /// Python `_compute_priority_fee` + net arithmetic (the §4.2 parity: the
    /// Rust port reproduces the Python oracle's `int(...)` truncation of the
    /// f64 priority-fee path — pins the exact `int` the Python would have
    /// produced for this gross/gas/fee block).
    #[tokio::test]
    async fn dispatch_pins_exact_profitable_figures() {
        // Path: gross 2 ETH, gas 200_000, base_fee_next 1 gwei, p10=0.5 gwei,
        // p50=2 gwei, age 0 (solve_block == current_block == 100).
        let asserter = Asserter::new();
        push_profitable(
            &asserter,
            U256::from(2_000_000_000_000_000_000u128),
            200_000,
        );
        let provider = mock_provider(&asserter);
        let suppression = Arc::new(Mutex::new(PathSuppression::new()));

        let cands = vec![candidate(
            10,
            1_000_000_000_000_000_000u128,
            2_000_000_000_000_000_000u128,
        )];
        let outcome = dispatch_profitable_results(
            cands,
            &ctx(&provider),
            &suppression,
            100,
            MIN_PROFIT_NET,
            0,
            None,
            None,
        )
        .await;

        assert_eq!(
            outcome.gas_profitable.len(),
            1,
            "buckets: {:?}",
            outcome.fail_buckets.buckets()
        );
        let r = &outcome.gas_profitable[0];
        assert_eq!(r.path_id, 10);
        assert_eq!(r.gross_profit, U256::from(2_000_000_000_000_000_000u128));
        assert_eq!(r.gas_used, 200_000);
        assert_eq!(r.base_fee_next, 1_000_000_000);
        // priority_fee: target = (gross/1.25 - gas*base)/gas is huge (~8e15);
        // age 0 keeps target unchanged (age_factor=1.0). Market ceiling is
        // max = p50+1 = 2_000_000_001, so the final clamp pins the priority fee
        // directly to that ceiling. The §4.2 parity is on the `int(...)`
        // truncation semantics — the f64 path is lossy by design.
        assert_eq!(r.priority_fee, 2_000_000_001);
        // net = gross - gas*(base + priority)
        //     = 2e18 - 200_000*(1e9 + 2_000_000_001)
        //     = 2e18 - 600_000_000_200_000
        //     = 1_999_399_999_999_800_000.
        assert_eq!(r.net_profit, U256::from(1_999_399_999_999_800_000u128));
    }

    #[tokio::test]
    async fn dispatch_tolerates_sim_failures_as_fail_count_not_exception() {
        // `simulate_one` swallows ALL sim failures (revert/no-profit/rpc-failed/
        // overflow) into `Ok(None)` + a tally bucket — it never returns `Err`
        // for ordinary sim outcomes (only the defensive `?` on the execute()
        // ABI encode can Err, and valid inputs never hit that). So a simulate_v1
        // RPC failure tallies `rpc-failed` + `Ok(None)` → `fail_count`, NOT
        // `exception_count`. This ports the `return_exceptions=True` tolerance
        // (L2504) faithfully: the dispatch never propagates a sim failure as
        // an `Err`.
        let asserter = Asserter::new();
        asserter.push_success(&access_list_response());
        asserter.push_failure_msg("simulate_v1 RPC failed");
        let provider = mock_provider(&asserter);
        let suppression = Arc::new(Mutex::new(PathSuppression::new()));

        let outcome = dispatch_profitable_results(
            vec![candidate(
                21,
                1_000_000_000_000_000_000u128,
                2_000_000_000_000_000_000u128,
            )],
            &ctx(&provider),
            &suppression,
            100,
            MIN_PROFIT_NET,
            0,
            None,
            None,
        )
        .await;
        // The simulate_v1 failure → simulate_one `Ok(None)` + `rpc-failed` →
        // fail_count=1, exception_count=0.
        assert_eq!(outcome.exception_count, 0);
        assert_eq!(outcome.fail_count, 1);
        assert_eq!(outcome.candidate_count, 1);
        assert_eq!(
            outcome.gas_profitable.len() + outcome.gas_unprofitable.len(),
            0
        );
        assert_eq!(outcome.fail_buckets.get("rpc-failed"), 1);
        // Path 21 records a failure.
        assert_eq!(
            suppression
                .lock()
                .expect("suppression mutex poisoned")
                .total_suppressed(),
            0
        );
    }

    // ── D1: the cap ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn dispatch_caps_at_max_simulate_concurrent() {
        // MAX_SIMULATE_CONCURRENT = 50. Hand 60 candidates (all suppressed at
        // the block-context retry interval → no RPC). The candidate_count
        // should be capped at 50.
        let suppression = Arc::new(Mutex::new(PathSuppression::new()));
        // Suppress paths 0..60.
        for pid in 0..60u64 {
            for _ in 0..10 {
                suppression
                    .lock()
                    .expect("suppression mutex poisoned")
                    .record_failure(pid);
            }
        }
        let asserter = Asserter::new();
        // sentinel — every candidate is suppressed at block 50 (< retry
        // interval 100) → no RPC, sentinel stays.
        asserter.push_success(&access_list_response());
        let provider = mock_provider(&asserter);

        let cands: Vec<DispatchCandidate> = (0..60u64)
            .map(|pid| {
                candidate(
                    pid,
                    1_000_000_000_000_000_000u128,
                    1_000_000_000_000_000_000u128,
                )
            })
            .collect();
        let outcome = dispatch_profitable_results(
            cands,
            &ctx(&provider),
            &suppression,
            50, // < retry interval → all suppressed
            MIN_PROFIT_NET,
            0,
            None,
            None,
        )
        .await;
        // All 60 suppressed pre-sim.
        assert_eq!(outcome.suppressed_count, 60);
        assert_eq!(outcome.candidate_count, 0);
        // No RPC dispatched (sentinel unconsumed).
        assert!(!asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn dispatch_returns_empty_outcome_for_empty_input() {
        let asserter = Asserter::new();
        let provider = mock_provider(&asserter);
        let suppression = Arc::new(Mutex::new(PathSuppression::new()));

        let outcome = dispatch_profitable_results(
            Vec::new(),
            &ctx(&provider),
            &suppression,
            100,
            MIN_PROFIT_NET,
            0,
            None,
            None,
        )
        .await;
        assert_eq!(outcome.candidate_count, 0);
        assert_eq!(outcome.gas_profitable.len(), 0);
        assert_eq!(outcome.suppressed_count, 0);
        assert_eq!(outcome.thin_dropped, 0);
    }

    // ── D1: tally merging ─────────────────────────────────────────────────

    #[tokio::test]
    async fn dispatch_merges_per_path_fail_buckets() {
        // Two no-profit candidates → two "no-profit" buckets merged into the
        // outcome tally (count=2).
        let asserter = Asserter::new();
        push_profitable(&asserter, U256::ZERO, 100_000); // path 1 no-profit
        push_profitable(&asserter, U256::ZERO, 100_000); // path 2 no-profit
        let provider = mock_provider(&asserter);
        let suppression = Arc::new(Mutex::new(PathSuppression::new()));

        let cands = vec![
            candidate(30, 1_000_000_000_000_000_000u128, 1_000),
            candidate(31, 1_000_000_000_000_000_000u128, 1_000),
        ];
        let outcome = dispatch_profitable_results(
            cands,
            &ctx(&provider),
            &suppression,
            100,
            MIN_PROFIT_NET,
            0,
            None,
            None,
        )
        .await;
        assert_eq!(outcome.fail_count, 2);
        assert_eq!(outcome.fail_buckets.get("no-profit"), 2);
    }

    // ── D1: per-path failure detail surfacing ────────────────────────────

    #[tokio::test]
    async fn dispatch_surfaces_per_path_failures_with_no_fail_index() {
        // Two no-profit candidates → two `SimFailure` records in
        // `outcome.failures`, each carrying its own `path_id`. No call
        // reverted (no-profit = all 7 succeeded), so `fail_index` is `None`.
        let asserter = Asserter::new();
        push_profitable(&asserter, U256::ZERO, 100_000);
        push_profitable(&asserter, U256::ZERO, 100_000);
        let provider = mock_provider(&asserter);
        let suppression = Arc::new(Mutex::new(PathSuppression::new()));

        let cands = vec![
            candidate(30, 1_000_000_000_000_000_000u128, 1_000),
            candidate(31, 1_000_000_000_000_000_000u128, 1_000),
        ];
        let outcome = dispatch_profitable_results(
            cands,
            &ctx(&provider),
            &suppression,
            100,
            MIN_PROFIT_NET,
            0,
            None,
            None,
        )
        .await;

        assert_eq!(outcome.failures.len(), 2, "expected two failure records");
        let ids: Vec<u64> = outcome.failures.iter().map(|f| f.path_id).collect();
        assert!(ids.contains(&30), "path 30 must be surfaced, got {ids:?}");
        assert!(ids.contains(&31), "path 31 must be surfaced, got {ids:?}");
        for f in &outcome.failures {
            assert_eq!(f.bucket, "no-profit");
            assert!(f.fail_index.is_none(), "no-profit has no failing call idx");
            assert!(f.revert_data.is_empty(), "no revert data on no-profit");
        }
    }

    #[tokio::test]
    async fn dispatch_surfaces_rpc_failed_failure_with_fail_index_none() {
        // A simulate_v1 RPC failure tallies `rpc-failed` + records a
        // `SimFailure` with `fail_index: None` + empty `revert_data` (no
        // simulate response was ever returned to inspect).
        let asserter = Asserter::new();
        asserter.push_success(&access_list_response());
        asserter.push_failure_msg("simulate_v1 RPC failed");
        let provider = mock_provider(&asserter);
        let suppression = Arc::new(Mutex::new(PathSuppression::new()));

        let outcome = dispatch_profitable_results(
            vec![candidate(
                21,
                1_000_000_000_000_000_000u128,
                2_000_000_000_000_000_000u128,
            )],
            &ctx(&provider),
            &suppression,
            100,
            MIN_PROFIT_NET,
            0,
            None,
            None,
        )
        .await;

        assert_eq!(outcome.failures.len(), 1);
        let f = &outcome.failures[0];
        assert_eq!(f.path_id, 21);
        assert_eq!(f.bucket, "rpc-failed");
        assert!(f.fail_index.is_none(), "rpc-failed has no sim call idx");
        assert!(f.revert_data.is_empty());
    }

    // ── Tier 1 (V5HCR5): in-process serial branch ─────────────────────

    /// Tier 1 (`V5HCR5`) parity: the in-process serial branch
    /// (`Some(bot_state)`) tallies `rpc-failed` for every candidate when the
    /// per-block EVM build fails. Under a `current_thread` tokio runtime,
    /// `WrapDatabaseAsync::new` returns `None` (it requires a multi-threaded
    /// runtime — the pump's `pyo3_async_runtimes` default), so
    /// `BlockSimHandle::build` returns `None` and the dispatch fans a
    /// `rpc-failed` `SimFailure` out to every candidate — one per `path_id`.
    /// This is the RPC-free characterization of the serial branch's
    /// build-failure handling, mirroring the retired per-path
    /// `simulate_in_process` build-failure tally (behavior-preserving — the
    /// old per-path build also returned `Ok(None)` + a `rpc-failed` record per
    /// path under the same no-runtime condition).
    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_in_process_tallies_rpc_failed_for_all_when_build_fails() {
        let asserter = Asserter::new();
        // sentinel — never consumed (build fails before any sim runs).
        asserter.push_success(&access_list_response());
        let provider = mock_provider(&asserter);
        let suppression = Arc::new(Mutex::new(PathSuppression::new()));
        let bot_state = Arc::new(RwLock::new(BotState::new()));

        let cands = vec![
            candidate(40, 1_000_000_000_000_000_000u128, 1_000),
            candidate(41, 1_000_000_000_000_000_000u128, 1_000),
        ];
        let outcome = dispatch_profitable_results(
            cands,
            &ctx(&provider),
            &suppression,
            100,
            MIN_PROFIT_NET,
            0,
            Some(bot_state),
            Some(degenbot_evm::WarmCodeCacheInner::shared_default()),
        )
        .await;

        // Build failed under current_thread runtime → every candidate
        // tallied rpc-failed (Ok(None)), no exceptions.
        assert_eq!(outcome.candidate_count, 2);
        assert_eq!(outcome.fail_count, 2);
        assert_eq!(outcome.exception_count, 0);
        assert_eq!(
            outcome.gas_profitable.len() + outcome.gas_unprofitable.len(),
            0
        );
        assert_eq!(outcome.fail_buckets.get("rpc-failed"), 2);
        // One SimFailure per candidate, each carrying its own path_id.
        assert_eq!(outcome.failures.len(), 2);
        let ids: Vec<u64> = outcome.failures.iter().map(|f| f.path_id).collect();
        assert!(ids.contains(&40), "path 40 must be surfaced, got {ids:?}");
        assert!(ids.contains(&41), "path 41 must be surfaced, got {ids:?}");
        for f in &outcome.failures {
            assert_eq!(f.bucket, "rpc-failed");
            assert!(f.fail_index.is_none());
            assert!(f.revert_data.is_empty());
        }
        // No RPC dispatched (build failed before any transact; sentinel
        // untouched).
        assert!(!asserter.read_q().is_empty());
    }
}
