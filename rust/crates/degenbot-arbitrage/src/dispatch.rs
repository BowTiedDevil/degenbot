//! The `dispatch_profitable_results` fan-out + categorization (D-row).
//!
//! Ports `examples/eth_backrun_v2_v3_v4_rust.py::dispatch_profitable_results`
//! (the fan-out L2450–L2517 + categorization + summary L2519–L2535) + the
//! thin-margin pre-filter (`filter_thin_margin_results` from
//! `examples/eth_backrun_helpers.py::filter_thin_margin_results`, L407–L446,
//! the SYI3PG cross-epic reference).
//!
//! This is the concurrency orchestration that owns the GIL release + the
//! tokio fan-out over the per-path [`simulate_path_on_evm`] leaf. Owning
//! the fan-out in Rust releases the GIL across the per-tx sim RPCs
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
//!   this leaf exposes the tally as a typed [`FailBuckets`] (re-exported
//!   from `simulator` at this crate's root) the companion renders.
//!   the companion renders.

// Solidity/EVM + Rust-ecosystem identifiers (tokio, JoinSet, bps, PathSuppression,
// MAX_SIMULATE_CONCURRENT, etc.) are ubiquitous here.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use alloy::primitives::U256;
use degenbot_bot::bot_core::state_lock::StateLock;
use degenbot_bot::bot_core::BotState;
use degenbot_executor::composers::{EncodeOptions, HopInfo, PathInfo};
use degenbot_simulation::BlockSimHandle;
use degenbot_submission::PathSuppression;
use parking_lot::RwLock;
use revm::database_interface::DatabaseRef;

use crate::{
    diverging_pool_keys, fot_suspected_token, hop_input_token, hop_output_token, hop_pool_key,
    is_solver_calc_failure, simulate_path_on_evm, FailBuckets, FeeOnTransferRegistry,
    PoolDivergence, SimFailure, SimResult, SimulateContext, SimulatePath,
};

// ─────────────────────────────────────────────────────────────────────────
// Diagnostics
// ─────────────────────────────────────────────────────────────────────────

/// `DEGENBOT_V2_CALC_TRACE` env-gated diagnostic: immediately before each
/// candidate path's sim, read every V2 hop's reserves slot (slot 8) from the
/// SHARED per-block `CacheDB` and log the decoded `reserve0`/`reserve1` plus
/// the hop's token orientation. `_v2_get_amount_out` (`V2_SWAP_CALC`, cmd
/// 0x21) reads exactly this word via `getReserves()`, so the logged values are
/// the ground truth for what the sim's V2 output is computed from. This closes
/// the observability gap for the path-11354 V3-V2-V3 1-wei under-delivery: a
/// slot8 that no on-chain block holds (synthetic / cached-intra-block /
/// polluted) surfaces here even though every DB layer below the `CacheDB`
/// forwards on-chain state.
///
/// The output is emitted at `debug` level, so it is gated behind the tracing
/// filter (`RUST_LOG=...=debug` / `=trace`) rather than appearing on stderr by
/// default. `DEGENBOT_V2_CALC_TRACE` (conservative default ON via
/// `flag_default_on`) additionally gates the slot-8 read itself: set it to a
/// falsey value to skip the read work entirely. The `debug` level is the
/// primary noise gate — the env var only controls whether the (cheap) reads
/// run.
fn v2_calc_trace(handle: &mut BlockSimHandle<'_>, sim_path: &SimulatePath) {
    if !crate::simulator::flag_default_on("DEGENBOT_V2_CALC_TRACE") {
        return;
    }
    for hop in &sim_path.path_info.hops {
        if let HopInfo::V2(v2) = hop {
            let cache_db = &mut handle.evm_mut().ctx.journaled_state.database;
            let word = match cache_db.storage_ref(v2.pool_address, U256::from(8u64)) {
                Ok(w) => w,
                Err(e) => {
                    tracing::debug!(
                        path_id = sim_path.path_id,
                        pair = ?v2.pool_address,
                        %e,
                        "[v2-calc-trace] slot8 read failed"
                    );
                    break;
                }
            };
            let mask112 = (U256::from(1_u128) << U256::from(112)) - U256::from(1_u128);
            let reserve0 = (word & mask112).to::<u128>(); // low 112 = token0 reserve
            let reserve1 = ((word >> U256::from(112)) & mask112).to::<u128>(); // next 112
            tracing::debug!(
                path_id = sim_path.path_id,
                pair = ?v2.pool_address,
                token0 = ?v2.token0_address,
                token1 = ?v2.token1_address,
                zfo = v2.zfo,
                fee = v2.fee,
                reserve0 = reserve0,
                reserve1 = reserve1,
                "[v2-calc-trace] pair reserves slot8 before path execute"
            );
            break;
        }
    }
}

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
/// One per-hop row of a candidate (Cloudflare fewer-lists pattern — HTPKLX
/// 4JLQNS continuation): the solver's output, the executable input, and the
/// solve-time state nonce, contiguous per hop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SolveStep {
    /// The solver's output after this hop (the former `hop_outputs[i]`).
    pub output: u128,
    /// The executable input fed into this hop's pool (the solver's CL-hop
    /// clamp) — the former `consumed_inputs[i]`. For V3/V4 hops this may be
    /// less than `output` of the prior hop when the range boundary is hit;
    /// the encoder must feed the clamped forward, not the full prior output.
    pub consumed_input: u128,
    /// Per-hop state nonce captured at solve time (AV42C7 staleness gate).
    pub state_nonce: u64,
}

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
    /// Per-hop rows (one allocation instead of the former three parallel
    /// `Vec`s).
    pub steps: Box<[SolveStep]>,
    /// `solve_block` — the block the solver produced the result on.
    pub solve_block: u64,
    /// `path_info` — the resolved path hops (consumed by `encode_cmd_stream`).
    pub path_info: PathInfo,
    /// `opts` — the encode options (`erc6909_profit` / `use_v4_batch`).
    pub opts: EncodeOptions,
}

impl DispatchCandidate {
    /// The solver's per-hop expected outputs as a flat `Vec` for the
    /// failure-telemetry intake (`FailBuckets::record` takes `Vec<u128>`);
    /// a failure-path-only view of the merged rows.
    #[must_use]
    pub fn expected_outputs_vec(&self) -> Vec<u128> {
        self.steps.iter().map(|s| s.output).collect()
    }

    /// Build the [`SimulatePath`] the fan-out hands to [`simulate_path_on_evm`].
    #[must_use]
    fn to_simulate_path(&self) -> SimulatePath {
        SimulatePath {
            path_id: self.path_id,
            optimal_input: self.optimal_input,
            steps: self.steps.clone(),
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
    /// The number of candidates that returned `None` from `simulate_path_on_evm`
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
    /// The number of candidates dropped pre-sim because every surviving
    /// hop routed through a pool flagged `SolverCalc` within the decay window
    /// (ergo `GMWYIU`). Mirrors `suppressed_count` / `thin_dropped` — a per-call
    /// pre-filter drop count; the lifetime tally lives on
    /// [`PoolDivergence::total_divergent_dropped`].
    pub divergent_dropped: usize,
    /// The number of candidates dropped pre-sim because any hop's input token
    /// is FoT-confirmed (ergo `3O535Q`). Mirrors `divergent_dropped` — a
    /// per-call pre-filter drop count; the lifetime tally lives on
    /// [`FeeOnTransferRegistry::total_fot_dropped`].
    pub fot_dropped: usize,
    /// AV42C7: candidates dropped because a pool's state nonce advanced past
    /// the snapshot captured at solve time (the solver computed against
    /// state the pump has since superseded — simulating would revert).
    pub stale_dropped: usize,
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

/// Fan out [`simulate_path_on_evm`] across a `buffer_unordered(MAX_SIMULATE_CONCURRENT)`
/// stream (capped by `truncate` pre-fan-out), gather with exception tolerance,
/// and categorize into gas-profitable / gas-unprofitable / exception (ports the
/// fan-out L2450–L2517 + categorization L2519–L2535).
///
/// Pipeline:
/// 1. **Pre-filter — suppression** — drop paths currently suppressed by
///    `path_suppression.is_suppressed(pid, current_block)` (the retry-interval
///    logic is owned by [`PathSuppression`]).
/// 2. **Pre-filter — pool divergence** (ergo `GMWYIU`) — drop candidates
///    routing through a pool flagged `SolverCalc` within the decay window
///    (counted in `divergent_dropped`). The memo is keyed by chain identity
///    (V2/V3 address, V4 `poolId`) derivable from each hop's `HopInfo`.
/// 3. **Pre-filter — thin-margin** — drop razor-thin arb via
///    [`filter_thin_margin_results`] (`min_profit_margin_bps`; `0` disables).
/// 4. **Cap** — take the first [`MAX_SIMULATE_CONCURRENT`] candidates (the
///    Python oracle slices `results[:MAX_SIMULATE_CONCURRENT]`; the candidates
///    are expected pre-sorted by engine profit descending — the caller does
///    the sort, ports L1684).
/// 5. **Fan-out** — the in-process revm path: build ONE per-block
///    `BlockSimHandle` + simulate each candidate SERIALLY on the shared
///    `&mut evm` (Tier 1, `V5HCR5`). `bot_state` is `Option` so that
///    empty / pre-filtered-to-empty input (which short-circuits at step 5)
///    need not construct an engine; the `None` arm is the unreachable
///    anti-pattern guard for non-empty input without a `BotState` (the
///    retired `eth_simulateV1` RPC executor — ADR-019 D1).
/// 6. **Gather** — collect the stream; exceptions are tolerated (counted, not
///    propagated — the Python oracle uses `return_exceptions=True`,
///    L2504).
/// 7. **Feedback — pool divergence** (ergo `GMWYIU`) — for each `SolverCalc`
///    failure, record divergence for the diverging hop's pool key (derived
///    from `path_info.hops[i]`); the NEXT block's skip (step 2) drops paths
///    through it pre-sim.
/// 8. **Categorize** — gas-profitable (`net ≥ min_profit_net`) /
///    gas-unprofitable (`None`-filtered, gross > 0, net below threshold) /
///    exception. Sort both categories by net profit descending (L2561/L2563).
/// 9. **Record suppression outcomes** — `record_success(pid)` for paths that
///    returned a result, `record_failure(pid)` for paths that didn't
///    (L2573–L2577).
///
/// `ctx` is the shared [`SimulateContext`] (provider, addresses, warmup,
/// block context — see [`simulate_path_on_evm`]). `path_suppression` is
/// mutated in place for the pre-filter + the outcome recording.
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
/// `exception_count` field captures `simulate_path_on_evm` `Err` returns (the
/// rare defensive `?` on the execute() ABI encode), counted not propagated.
///
/// # Panics
///
/// Panics if the `path_suppression` OR `pool_divergence` mutex is poisoned (a
/// peer task panicked while holding it). Both mutexes are locked ONLY at the
/// bookends (the suppression + divergence pre-filters in steps 1/2 + the
/// outcome recording + divergence feedback in steps 7/8) — both synchronous
/// spans, no `.await` held under either guard — so a poison indicates a bug in
/// a sibling task (the arcs are shared with the submission seam's accessors;
/// never locked across an `.await`).
/// Deterministically reorder `candidates` into the order index's net-profit
/// ranking (profit descending, id ascending) before the `MAX_SIMULATE_CONCURRENT`
/// cap — the `order-index` feature's substitute for the "candidates arrive
/// pre-sorted" contract (see ADR-024, 34VCC2). Today the solver emits no
/// per-path gas, so `gas = 0` and `net == gross == engine_profit`; the seam
/// becomes net-aware as soon as per-path gas is available.
#[cfg(feature = "order-index")]
fn order_index_top_selection(candidates: &mut [DispatchCandidate]) {
    use degenbot_order_index::{EnvelopeIndex, OrderIndex};
    let mut idx = EnvelopeIndex::<u64>::new();
    for c in candidates.iter() {
        idx.insert(c.path_id, U256::ZERO, U256::from(c.engine_profit));
    }
    let ranked = idx.top_k(U256::ZERO, candidates.len());
    // `ranked` is every id in (net desc, id asc) order. Safe-guard: only enforce
    // when it covers every candidate (candidate path_ids are expected unique).
    if ranked.len() != candidates.len() {
        return;
    }
    let pos: HashMap<u64, usize> = ranked
        .into_iter()
        .enumerate()
        .map(|(i, id)| (id, i))
        .collect();
    candidates.sort_by_key(|c| pos.get(&c.path_id).copied().unwrap_or(usize::MAX));
}

#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::missing_panics_doc,
    clippy::expect_used
)] // faithful dispatch seam; mutex-poison guards panic loudly
pub fn dispatch_profitable_results(
    mut candidates: Vec<DispatchCandidate>,
    ctx: &SimulateContext<'_>,
    path_suppression: &Arc<Mutex<PathSuppression>>,
    current_block: u64,
    min_profit_net: u128,
    min_profit_margin_bps: u64,
    // The per-pool solver-divergence memo (ergo `GMWYIU`). Parallels
    // `path_suppression` — a standalone `Arc<Mutex<PoolDivergence>>` (NOT
    // composed into the `Dispatcher`) so the sim seam locks it directly at
    // the skip bookend (step 2) + the feedback (step 7), never across the
    // fan-out `.await`s. The skip drops candidates routing through a pool
    // flagged `SolverCalc` within the decay window; the feedback records
    // divergence for `SolverCalc` failures' diverging hops so the NEXT
    // block's skip drops them.
    pool_divergence: &Arc<Mutex<PoolDivergence>>,
    // The per-token fee-on-transfer registry (ergo `3O535Q`). Parallels
    // `pool_divergence` — a standalone `Arc<Mutex<FeeOnTransferRegistry>>`
    // (NOT composed into the `Dispatcher`) so the sim seam locks it directly
    // at the skip bookend (step 2) + the feedback (step 7) + the success
    // recording (step 8), never across the fan-out `.await`s. The skip drops
    // candidates whose any hop's input token is FoT-confirmed; the feedback
    // records suspicions for FoT-suspected failures; the success recording
    // feeds the 0-success disambiguator (a token that ever succeeds is not
    // FoT — the fee always shorts the input).
    fot_registry: &Arc<Mutex<FeeOnTransferRegistry>>,
    // The fan-out routes each candidate through the in-process revm sim
    // over the borrowed `&BotState` via a per-block shared `BlockSimHandle`
    // (Tier 1, `V5HCR5` — retired the per-path `simulate_in_process`
    // fresh-`CacheDB`-per-call build), the sole executor since the
    // `eth_simulateV1` RPC path retired (ADR-019 D1). The `Arc<RwLock<BotState>>`
    // is the engine's shared state owner (ADR-003); a per-block read guard is
    // taken for the serial loop. revm's `WrapDatabaseAsync::block_on` uses
    // `tokio::task::block_in_place` under a multi-threaded runtime, so the
    // in-process sim's blocking RPC cold-miss path does not deadlock against
    // the pump's worker pool. `Option` (not required) so that empty /
    // pre-filtered-to-empty input — which never reaches this param at all
    // (step 4's `is_empty()` short-circuit above) — need not wire an engine;
    // the `None` arm below is the unreachable guard for non-empty input
    // without a `BotState`.
    bot_state: Option<Arc<StateLock<BotState>>>,
    // The cross-block persistent bytecode + account-existence cache
    // (`WarmCodeCacheInner`, the `HDEG7H` Option-A layer). Required by the
    // `BlockSimHandle` build (the `None` arm is unreachable — see `bot_state`).
    // The `Arc` clones cheaply into this async fn's future; the engine owner
    // (`PyArbitrageEngine` / standalone `Bot`) holds it for the engine's
    // life. If `Some(bot_state)` arrives with `None` warm_cache (a caller
    // wiring gap), a fresh `WarmCodeCacheInner::shared_default()` is
    // constructed per call — a safe degradation to Tier-1-only behavior (no
    // cross-block persistence, no panic). The FFI seam sets both from the
    // same `engine` arg, so the gap is unreachable in production.
    warm_cache: Option<Arc<RwLock<degenbot_simulation::WarmCodeCacheInner>>>,
) -> DispatchOutcome {
    let mut outcome = DispatchOutcome::default();
    let pre_filter_count = candidates.len();
    // T3: candidates entering the fan-out (pre-filter sizes summed; the
    // per-stage drop counts live on `DispatchOutcome` for the logs).
    if let Some(p) = degenbot_bot::instruments::pipeline() {
        p.count_candidates_found(u64::try_from(pre_filter_count).unwrap_or(u64::MAX));
    }

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

    // 2. Pre-filter — pool divergence (ergo `GMWYIU`). Drop candidates
    //    routing through a pool flagged `SolverCalc` within the decay window
    //    (counted in `divergent_dropped`). The memo is keyed by chain
    //    identity — V2/V3 pool address, V4 `poolId` bytes32 — derivable from
    //    each hop's `HopInfo` with no engine lookup, so the skip is
    //    standalone. Snapshot each candidate's `PathInfo` into a join map
    //    first (the post-fan-out feedback path re-derives the diverging
    //    hop's key from `hops[i]` — the V4 `CapturedSwap.emitter` is the
    //    shared PoolManager, useless for per-pool attribution).
    let path_info_by_id: HashMap<u64, PathInfo> = candidates
        .iter()
        .map(|c| (c.path_id, c.path_info.clone()))
        .collect();
    let divergent_before = candidates.len();
    {
        let pd = pool_divergence
            .lock()
            .expect("pool_divergence mutex poisoned");
        candidates.retain(|c| {
            !c.path_info
                .hops
                .iter()
                .filter_map(hop_pool_key)
                .any(|k| pd.is_divergent(k, current_block))
        });
        outcome.divergent_dropped = divergent_before - candidates.len();
    }
    // Bump the lifetime tally (mirrors `PathSuppression::total_suppressed`).
    if outcome.divergent_dropped > 0 {
        let mut pd = pool_divergence
            .lock()
            .expect("pool_divergence mutex poisoned");
        for _ in 0..outcome.divergent_dropped {
            pd.record_dropped();
        }
    }

    // 2.5. Pre-filter — fee-on-transfer tokens (ergo `3O535Q`). Drop
    //      candidates whose any hop's input token is FoT-confirmed (counted
    //      in `fot_dropped`). The registry is keyed by token `Address`;
    //      the input token is derived from each hop's `HopInfo` via
    //      `hop_input_token` (selected by `zfo`). Same standalone-arc
    //      discipline as the divergence skip — locked only at this bookend,
    //      never across the fan-out `.await`s.
    let fot_before = candidates.len();
    {
        let fr = fot_registry.lock().expect("fot_registry mutex poisoned");
        candidates.retain(|c| {
            !c.path_info
                .hops
                .iter()
                .any(|hop| fr.is_fot(hop_input_token(hop), current_block))
        });
        outcome.fot_dropped = fot_before - candidates.len();
    }
    if outcome.fot_dropped > 0 {
        let mut fr = fot_registry.lock().expect("fot_registry mutex poisoned");
        for _ in 0..outcome.fot_dropped {
            fr.record_dropped();
        }
    }

    // 3. Pre-filter — thin-margin (L2497–L2499).
    let (kept, thin_dropped) = filter_thin_margin_results(candidates, min_profit_margin_bps);
    outcome.thin_dropped = thin_dropped;
    candidates = kept;

    // 3.5. Pre-filter — stale solve results (AV42C7). The solver computed
    //      each candidate's hop_outputs against pool state captured at
    //      resolve time; the pump may have advanced a pool's state since (a
    //      user swap landed between the solve and the sim). The executor
    //      chains the solver's PREDICTED hop outputs as exact-in amounts for
    //      downstream hops, so any per-hop over-prediction becomes an IIA
    //      underpayment revert. Drop candidates whose any hop's current
    //      state nonce has advanced past the snapshot captured at solve
    //      time — the result is stale and would revert on-chain.
    let stale_before = candidates.len();
    if let Some(ref arc) = bot_state {
        let guard = arc.read();
        candidates.retain(|c| !candidate_is_stale(&guard, c));
    }
    outcome.stale_dropped = stale_before - candidates.len();

    // 4. Cap (L2491). With the `order-index` feature, the selection is made
    //    deterministic by profit via the net-profit order index (see ADR-024)
    //    instead of trusting the caller's pre-sort; otherwise the candidates
    //    are expected pre-sorted by engine profit descending (the caller's
    //    responsibility — ports L1684's sort).
    #[cfg(feature = "order-index")]
    order_index_top_selection(&mut candidates);
    candidates.truncate(MAX_SIMULATE_CONCURRENT);
    outcome.candidate_count = candidates.len();

    // Empty-input short-circuit: with no candidates to simulate, the outcome
    // (all counters already set above) is complete. This avoids requiring a
    // `BotState` for the trivial empty case (suppressed-to-empty / genuinely
    // empty input) — the in-process revm path never builds a `BlockSimHandle`
    // for zero candidates.
    if candidates.is_empty() {
        return outcome;
    }

    // 5. Fan-out — build ONE per-block `BlockSimHandle` + simulate each
    //    candidate SERIALLY on the shared `&mut evm` (Tier 1, `V5HCR5`).
    //    `buffer_unordered(MAX_SIMULATE_CONCURRENT)` was the Rust idiom for
    //    the Python `asyncio.gather(*sim_tasks)` + the
    //    `results[:MAX_SIMULATE_CONCURRENT]` cap under the retired RPC path
    //    (the cap is double-asserted: the L2491 truncate + the buffer bound
    //    — belt + suspenders); the in-process serial path uses a plain
    //    `.map().collect()` over the borrowed `&mut evm`. Unlike
    //    `tokio::spawn`, neither borrows `'static` — the closure borrows
    //    `ctx` for its lifetime, and we collect within this fn (no task
    //    outlives the call).
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
            // ULUWNI (incident 2026-08-20 #1 root fix): snapshot the sim
            // anchor under a SHORT read and drop the guard BEFORE any
            // provider I/O. Pre-fix this read guard was held across
            // `BlockSimHandle::build` + the whole serial sim loop — every
            // cold-miss fetch parked a `BotState` writer behind it for the
            // transport delay (minutes on a stalled RPC pre-F2). The
            // snapshot is O(pools) scalar words; see
            // `bot_core::SimAnchorState` for the audited surface.
            //
            // ADR-039 (K4ETHF): since T5 the snapshot is an ENUMERATED
            // per-family projection (`project_sim_anchor_scalars`), never an
            // arbitrary-key probe — this guard scope paid a ~2.7s hold per
            // block through the V3 tick-descent + V4 O(V^2) keccak paths.
            // DO NOT re-point the projection at the arbitrary-key probe, and
            // do not extend this guard across anything below it.
            let anchor = {
                let guard = arc.read();
                degenbot_bot::bot_core::SimAnchorState::snapshot(&guard)
            };
            // The warm-code cache arc; degrade to a fresh per-call cache if
            // the caller wired `bot_state` without one (safe — no
            // cross-block persistence, no panic).
            let warm_cache =
                warm_cache.unwrap_or_else(degenbot_simulation::WarmCodeCacheInner::shared_default);
            // The engine `BlockSimHandle` build takes the block-env primitives
            // + the override params projected from this strategy's
            // `SimulateContext` (ADR-019 D7, decision R — the engine stays
            // generic over strategy config; it never names `SimulateContext`).
            // The shared-EVM sim anchor: since BO5FBS the pump pre-promotes
            // `active_block = max(drain_block, pool_state_head)` and threads it
            // as each candidate's `solve_block`, so every candidate in the
            // batch carries the SAME promoted block (the pool-state head). This
            // `max` over `solve_block` is therefore a defensive no-op that
            // returns that shared promoted block — kept as an invariant
            // assertion, not a re-anchor. Simulating at the lagging Python
            // clock fetched PRE-update state (state-ahead-of-clock desync) and
            // mismatched the solver's head math — the MQIZ5M IIA; one
            // head-anchored shared EVM reproduces every candidate exactly.
            let sim_block = candidates
                .iter()
                .map(|c| c.solve_block)
                .max()
                .unwrap_or(ctx.current_block);
            match BlockSimHandle::build(
                ctx.provider,
                ctx.base_fee_next,
                sim_block,
                ctx.block_timestamp,
                &ctx.override_params(),
                &anchor,
                &warm_cache,
            ) {
                Some(mut handle) => candidates
                    .into_iter()
                    .map(|c| {
                        let pid = c.path_id;
                        let sim_path = c.to_simulate_path();
                        // `DEGENBOT_V2_CALC_TRACE` — env-gated diagnostic that
                        // reads every V2 hop's reserves slot (slot 8) straight
                        // from the SHARED per-block CacheDB immediately before
                        // this candidate's `simulate_path_on_evm` run. This is
                        // exactly what the executor's Vyper `_v2_get_amount_out`
                        // (`V2_SWAP_CALC`, cmd 0x21) SLOADs via `getReserves()`,
                        // so the logged reserve word is the ground truth for
                        // what the sim's V2 output is computed from — it reveals
                        // any cached/polluted slot8 that no on-chain read can
                        // reproduce (path-11354 V3-V2-V3 1-wei under-delivery).
                        v2_calc_trace(&mut handle, &sim_path);
                        let mut buckets = FailBuckets::new();
                        let result =
                            simulate_path_on_evm(handle.evm_mut(), ctx, &sim_path, &mut buckets)
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
                            c.optimal_input,
                            c.expected_outputs_vec(),
                        );
                        (c.path_id, buckets, Ok(None))
                    })
                    .collect(),
            }
        }
        // ADR-019 D1 — the legacy RPC `eth_simulateV1` path retired; the
        // in-process revm path (the `Some` arm above) is the sole executor.
        // This arm is unreachable in production: the FFI seam always sources a
        // `BotState` from the engine, and empty / pre-filtered-to-empty input
        // short-circuits at step 4 above before the `match` is reached. The
        // `Option` (rather than a required arg) is preserved so that
        // empty-input callers (the seam's offline empty-candidate tests) need
        // not construct an engine.
        None => {
            unreachable!(
                "dispatch_profitable_results: the legacy RPC sim path retired (ADR-019 D1); supply a BotState"
            )
        }
    };

    // 6. Categorize — gas-profitable / gas-unprofitable / exception (ports
    //    L2519–L2557). `simulate_path_on_evm` returns `Ok(Some(result))` for gross-
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

    // 7. Feedback — record divergence for `SolverCalc` failures' diverging
    //    hops (ergo `GMWYIU`). A pool flagged this block stays divergent for
    //    `POOL_DIVERGENCE_DECAY_BLOCKS`; the NEXT block's skip drops paths
    //    routing through it pre-sim. The diverging hop's key is derived from
    //    `path_info_by_id[pid].hops[i]` (index correspondence is the
    //    `is_solver_calc_failure` count-guard contract — a count mismatch
    //    short-circuits to non-`SolverCalc`); the V4 `poolId` comes from the
    //    hop, NOT the captured swap's emitter (which is the shared
    //    PoolManager). Scope-locked once for the whole feedback loop.
    {
        let mut pd = pool_divergence
            .lock()
            .expect("pool_divergence mutex poisoned");
        for f in &outcome.failures {
            if !is_solver_calc_failure(f) {
                continue;
            }
            // A path dropped by a PRE-sim filter never reaches sim, so its
            // failures aren't in `outcome.failures`; the lookup always hits
            // for a SolverCalc-classified failure (it was simulated).
            if let Some(path_info) = path_info_by_id.get(&f.path_id) {
                for key in diverging_pool_keys(f, &path_info.hops) {
                    pd.record_divergence(key, current_block);
                }
            }
        }
    }

    // 7.5. FoT feedback — record suspicions for FoT-suspected failures (ergo
    //      `3O535Q`). The `fot_suspected_token` leaf returns `(token,
    //      pool_key)` for failures whose `reverting_frame.label` is in
    //      `FOT_REVERT_LABELS` (`IIA`, `CurrencyNotSettled`, `UniswapV2: K`).
    //      The pool_key is the hop's `PoolDivergenceKey` (V2/V3 address, V4
    //      `poolId` — see ergo `DLSKD7`); the registry tracks the distinct
    //      failing pool identities per token + the 0-success flag; the skip
    //      (step 2.5) drops paths whose any hop's input token is FoT-confirmed.
    //      Same standalone-arc discipline.
    {
        let mut fr = fot_registry.lock().expect("fot_registry mutex poisoned");
        for f in &outcome.failures {
            if let Some(path_info) = path_info_by_id.get(&f.path_id) {
                if let Some((token, pool_key)) = fot_suspected_token(f, &path_info.hops) {
                    fr.record_suspicion(token, pool_key, current_block);
                }
            }
        }
    }

    // 8. Record suppression outcomes (L2573–L2577). Ports
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

    // 8.5. FoT success recording (ergo `3O535Q`). For every SUCCEEDED path,
    //      record `record_success(token)` for EVERY token on the path — each
    //      hop's input AND output (via `hop_input_token` + `hop_output_token`)
    //      — the 0-success disambiguator. A true FoT token can never succeed
    //      (the fee always shorts the input), so a single success clears the
    //      token. Recording BOTH sides (not just hop inputs) is important: a
    //      committed swap proves EVERY token that crossed a leg transferred
    //      without shorting the fee (any short would have reverted), so a
    //      token appearing only as a hop OUTPUT (e.g. WBTC on the final leg)
    //      is also cleared. Both `gas_profitable` + `gas_unprofitable` are
    //      "execute() succeeded" — the FoT check is "did the swap commit",
    //      not "was it profitable". Failed paths are NOT recorded here (a FoT
    //      token's failures are recorded as suspicions in step 7.5; a
    //      stale-state failure is neither a success nor a FoT suspicion).
    if !succeeded_path_ids.is_empty() {
        let mut fr = fot_registry.lock().expect("fot_registry mutex poisoned");
        for &pid in &candidate_path_ids {
            if succeeded_path_ids.contains(&pid) {
                if let Some(path_info) = path_info_by_id.get(&pid) {
                    for hop in &path_info.hops {
                        fr.record_success(hop_input_token(hop), current_block);
                        fr.record_success(hop_output_token(hop), current_block);
                    }
                }
            }
        }
    }

    // 9. Sort both categories by net profit descending (L2561/L2563).
    outcome
        .gas_profitable
        .sort_by_key(|r| std::cmp::Reverse(r.net_profit));
    outcome
        .gas_unprofitable
        .sort_by_key(|r| std::cmp::Reverse(r.net_profit));

    // Emit the env-gated `[sim-divergence] summary` for this block's dispatch
    // fan-out (ergo task 4C33DP / epic TR6GWT) — the tally of engine-vs-RPC
    // tracked-slot comparisons run inside `BotStateDb::storage_ref` during
    // the sim fan-out above. No-op when the probe is off; when on, logs
    // `slots_compared/divergent_slots/divergent_pairs/divergent_pools`
    // alongside the driver's `[sim]` per-block line. The divergence lines
    // themselves (`[sim-divergence] pool=..`) emit per-mismatch during the
    // fan-out; this is the per-block tally rollup.
    degenbot_simulation::divergence_probe::dump_divergence_summary();

    outcome
}

/// Resolve a hop's `pool_id` on `BotState` and check whether its current
/// `state_nonce` has advanced past the snapshot `candidate.state_nonces[hop]`.
/// Returns `true` (stale) if ANY hop's nonce has advanced. A hop whose
/// `pool_id` cannot be resolved is treated as fresh (the path-validity guard
/// at resolve time already dropped unresolvable paths; a transient miss here
/// is the registry's responsibility, not a staleness signal).
///
/// AV42C7: the executor chains the solver's predicted hop outputs as exact-in
/// amounts, so a stale hop's over-prediction becomes an IIA underpayment
/// revert. Dropping the candidate pre-sim avoids the revert.
fn candidate_is_stale(core: &BotState, candidate: &DispatchCandidate) -> bool {
    use degenbot_executor::composers::HopInfo;
    for (i, hop) in candidate.path_info.hops.iter().enumerate() {
        let Some(step) = candidate.steps.get(i) else {
            return false; // Mismatched lengths — let the sim path handle it.
        };
        let snapshot_nonce = step.state_nonce;
        let current_nonce = match hop {
            HopInfo::V2(h) => core
                .pool_id_by_address(&h.pool_address)
                .map_or(0, |pid| core.pool_state_nonce(pid)),
            HopInfo::V3(h) => core
                .pool_id_by_address(&h.pool_address)
                .map_or(0, |pid| core.pool_state_nonce(pid)),
            HopInfo::V4(h) => match alloy::hex::decode(h.pool_id_hex.trim_start_matches("0x")) {
                Ok(bytes) if bytes.len() == 32 => {
                    let mut pid = [0u8; 32];
                    pid.copy_from_slice(&bytes);
                    core.v4_pool_id_by_key(h.pool_manager_address, &pid)
                        .map_or(0, |p| core.pool_state_nonce(p))
                }
                _ => 0,
            },
        };
        if current_nonce != snapshot_nonce {
            return true;
        }
    }
    false
}

#[expect(clippy::unwrap_used)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlockPriorityFees;
    use alloy::primitives::{address, Address, Bytes, U256};
    use alloy::providers::{Provider, ProviderBuilder};
    use alloy::rpc::client::ClientBuilder;
    use alloy::transports::mock::{Asserter, MockTransport};
    use degenbot_executor::composers::{HopInfo, PathInfo, V2HopInfo};
    use degenbot_executor::{compute_simulation_warmup_slots, WarmupSlots};
    use degenbot_rpc::provider::AlloyProvider;
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
        compute_simulation_warmup_slots(EXECUTOR, WETH)
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

    /// A single-hop V2 candidate routing through `pool` — for the divergence
    /// skip test, which pre-flags one pool + asserts the candidate through it
    /// is dropped while a candidate through a clean pool survives.
    fn single_v2_hop(pool: Address) -> PathInfo {
        PathInfo::new(vec![HopInfo::V2(V2HopInfo {
            pool_address: pool,
            token0_address: WETH,
            token1_address: address!("1111111111111111111111111111111111111111"),
            fee: 30,
            zfo: true,
        })])
    }

    fn candidate_through_pool(path_id: u64, pool: Address) -> DispatchCandidate {
        DispatchCandidate {
            path_id,
            optimal_input: 1_000_000_000_000_000_000u128,
            engine_profit: 1_000,
            steps: Box::new([SolveStep {
                output: 1_100_000_000_000_000_000u128,
                consumed_input: 1_100_000_000_000_000_000u128,
                state_nonce: 0,
            }]),
            solve_block: 100,
            path_info: single_v2_hop(pool),
            opts: EncodeOptions {
                erc6909_profit: false,
                use_v4_batch: false,
                ..Default::default()
            },
        }
    }

    fn candidate(path_id: u64, opt_input: u128, profit: u128) -> DispatchCandidate {
        DispatchCandidate {
            path_id,
            optimal_input: opt_input,
            engine_profit: profit,
            steps: Box::new([
                SolveStep {
                    output: opt_input * 11 / 10,
                    consumed_input: opt_input,
                    state_nonce: 0,
                },
                SolveStep {
                    output: opt_input * 121 / 100,
                    consumed_input: opt_input * 11 / 10,
                    state_nonce: 0,
                },
            ]),
            solve_block: 100,
            path_info: two_v2_hops(),
            opts: EncodeOptions {
                erc6909_profit: false,
                use_v4_batch: false,
                ..Default::default()
            },
        }
    }

    /// Build a 7-call simulated block where the balances yield a known gross.
    fn access_list_response() -> serde_json::Value {
        serde_json::json!({"accessList": [], "gasUsed": "0x0"})
    }

    /// Push the 2 canned responses (access-list + simulate) for a profitable
    /// path into the asserter.
    // ── D2: PathSuppression consumption (done-reference) ──────────────────

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

    // ── SMOZG3: the production axis chain (kwarg → intake → config) ────────

    #[test]
    #[expect(clippy::expect_used)] // in-range axes; a fail-closed panic is the test contract
    fn erc6909_candidate_option_reaches_intake_and_mode2_config() {
        // The operator's `erc6909_profit` flag must survive the full
        // production projection — DispatchCandidate → `to_simulate_path` →
        // `encode_request` (the ADR-033 intake) — resolving through
        // `resolve_axes` to the `Erc6909` capture axis and packing
        // `check_mode=2` (the on-chain ERC6909 WETH floor the profit assert
        // runs under). This pins the chain end-to-end so the ADR-033
        // reshape cannot silently drop the knob.
        let cand = DispatchCandidate {
            path_id: 42,
            optimal_input: 1_000_000_000_000_000_000u128,
            engine_profit: 1_000,
            steps: Box::new([
                SolveStep {
                    output: 1_100_000_000_000_000_000u128,
                    consumed_input: 1_000_000_000_000_000_000u128,
                    state_nonce: 0,
                },
                SolveStep {
                    output: 1_300_000_000_000_000_000u128,
                    consumed_input: 1_100_000_000_000_000_000u128,
                    state_nonce: 0,
                },
            ]),
            solve_block: 100,
            path_info: two_v2_hops(),
            opts: EncodeOptions {
                erc6909_profit: true,
                ..Default::default()
            },
        };
        let path = cand.to_simulate_path();
        // kwarg → SimulatePath.opts (unchanged across the ADR-033 reshape):
        assert!(
            path.opts.erc6909_profit,
            "candidate opts must reach SimulatePath"
        );
        let req = path.encode_request();
        // → resolve_axes: the legacy bool forces the Erc6909 capture axis.
        let (_, capture, _) = degenbot_executor::composers::resolve_axes(req.opts);
        assert!(
            matches!(
                capture,
                degenbot_executor::grammar_ledger::ProfitCapture::Erc6909
            ),
            "erc6909_profit must resolve to the Erc6909 capture axis"
        );
        // → the axis-aware packed config: check_mode=2 (Erc6909 WETH assert).
        let cfg = degenbot_executor::composers::config_for_options(req.opts, U256::ZERO)
            .expect("axes in range");
        assert_eq!(
            cfg & U256::from(255u64),
            U256::from(2u64),
            "check_mode must be 2 for Erc6909 capture"
        );
    }

    #[test]
    #[expect(clippy::expect_used)] // in-range axes; a fail-closed panic is the test contract
    fn custody_candidate_defaults_to_mode1_assert() {
        // Control: the default (no operator toggle) runs the ACTIVE
        // WETH+ETH profit assert (U3WVLL), not the old check_mode=0 fast path.
        let cand = candidate(43, 1_000, 100);
        let path = cand.to_simulate_path();
        let req = path.encode_request();
        let cfg = degenbot_executor::composers::config_for_options(req.opts, U256::ZERO)
            .expect("axes in range");
        assert_eq!(
            cfg & U256::from(255u64),
            U256::from(1u64),
            "default capture must pack check_mode=1 (active assert)"
        );
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

    // ── Tier 1 (V5HCR5): in-process serial branch ─────────────────────

    // ── ULUWNI: no BotState guard across provider I/O ──────────────────

    /// A transport wrapper that delays every request before delegating —
    /// makes the fan-out's provider I/O observably slow so a guard held
    /// across it is measurable. The wrapped queue can be empty: the delay
    /// happens BEFORE the (then-failing) delegation, which is all the
    /// timing assertion needs.
    #[derive(Debug, Clone)]
    struct DelayedTransport {
        inner: alloy::transports::BoxTransport,
        delay: std::time::Duration,
    }

    impl tower::Service<alloy_json_rpc::RequestPacket> for DelayedTransport {
        type Response = alloy_json_rpc::ResponsePacket;
        type Error = alloy::transports::TransportError;
        type Future = alloy::transports::TransportFut<'static>;

        fn poll_ready(
            &mut self,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            tower::Service::<alloy_json_rpc::RequestPacket>::poll_ready(&mut self.inner, cx)
        }

        fn call(&mut self, req: alloy_json_rpc::RequestPacket) -> Self::Future {
            let mut inner = self.inner.clone();
            let delay = self.delay;
            Box::pin(async move {
                tokio::time::sleep(delay).await;
                inner.call(req).await
            })
        }
    }

    /// ULUWNI (incident 2026-08-20 #1 root fix): the fan-out must NOT hold
    /// the `BotState` read guard across provider I/O. Pre-fix,
    /// `BlockSimHandle::build` + the serial sim loop borrowed the guard
    /// while every cold-miss fetch went over RPC — a writer parked for the
    /// full transport delay (minutes on a stalled RPC pre-F2). Post-fix the
    /// anchor is snapshotted under a SHORT read and the writer proceeds
    /// while the sim is mid-RPC.
    #[test]
    #[expect(clippy::expect_used)]
    fn fanout_does_not_hold_the_botstate_guard_across_provider_io() {
        use alloy::transports::Transport;
        // WrapDatabaseAsync needs an ambient multi-threaded runtime; the
        // delaying transport needs its reactor for the sleep.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("multi-thread runtime");

        // Empty Asserter queue: every RPC errors — AFTER the delay. The
        // timing assertion only needs the delay, not successful sims.
        let asserter = Asserter::new();
        let client = alloy::rpc::client::ClientBuilder::default().transport(
            DelayedTransport {
                inner: MockTransport::new(asserter).boxed(),
                delay: std::time::Duration::from_millis(1_500),
            },
            true,
        );
        let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
        let provider =
            AlloyProvider::from_provider(Arc::new(dyn_provider)
                as Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>);

        let suppression = Arc::new(Mutex::new(PathSuppression::new()));
        let pool_divergence = Arc::new(Mutex::new(crate::PoolDivergence::new()));
        let fot_registry = Arc::new(Mutex::new(crate::FeeOnTransferRegistry::new()));
        let warm = degenbot_simulation::WarmCodeCacheInner::shared_default();
        let bot_state = Arc::new(StateLock::new(BotState::new()));
        let writer_state = Arc::clone(&bot_state);

        // The fan-out runs on its own thread inside the runtime.
        let sim_thread = std::thread::spawn(move || {
            let cands = vec![
                candidate(40, 1_000_000_000_000_000_000u128, 1_000),
                candidate(41, 1_000_000_000_000_000_000u128, 1_000),
            ];
            rt.block_on(async {
                dispatch_profitable_results(
                    cands,
                    &ctx(&provider),
                    &suppression,
                    100,
                    MIN_PROFIT_NET,
                    0,
                    &pool_divergence,
                    &fot_registry,
                    Some(bot_state),
                    Some(warm),
                )
            })
        });

        // Once the fan-out is mid-build (guard held, first delayed RPC in
        // flight), queue a BotState writer and measure how long it parks.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let (tx, rx) = std::sync::mpsc::channel::<std::time::Duration>();
        let writer = std::thread::spawn(move || {
            let start = std::time::Instant::now();
            let _guard = writer_state.write(); // parks behind the fan-out's read (pre-fix)
            tx.send(start.elapsed()).expect("report wait");
        });

        let waited = rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("writer completes");
        assert!(
            waited < std::time::Duration::from_millis(700),
            "BotState writer waited {waited:?} — the fan-out holds the read \
             guard across provider I/O (ULUWNI)"
        );

        let outcome = sim_thread.join().expect("fan-out thread");
        // The sim itself still fails cleanly (delayed-erroring transport):
        // every candidate tallies rpc-failed, no exceptions.
        assert_eq!(outcome.exception_count, 0);
        writer.join().expect("writer thread");
    }

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
    #[test]
    fn dispatch_in_process_tallies_rpc_failed_for_all_when_build_fails() {
        let asserter = Asserter::new();
        // sentinel — never consumed (build fails before any sim runs).
        asserter.push_success(&access_list_response());
        let provider = mock_provider(&asserter);
        let suppression = Arc::new(Mutex::new(PathSuppression::new()));
        let pool_divergence = Arc::new(Mutex::new(crate::PoolDivergence::new()));
        let fot_registry = Arc::new(Mutex::new(crate::FeeOnTransferRegistry::new()));
        let bot_state = Arc::new(StateLock::new(BotState::new()));

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
            &pool_divergence,
            &fot_registry,
            Some(bot_state),
            Some(degenbot_simulation::WarmCodeCacheInner::shared_default()),
        );

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

    // ── Pool divergence skip + feedback (ergo GMWYIU) ────────────────

    /// The skip (step 2) drops a candidate routing through a pool flagged
    /// `SolverCalc` within the decay window, while a candidate through a
    /// clean pool survives to sim. `divergent_dropped` counts the drop +
    /// `PoolDivergence::total_divergent_dropped` bumps the lifetime tally.
    /// Uses the build-fails sim path (no real sim needed — the skip is
    /// pre-sim, so the dropped candidate never reaches the fan-out).
    #[test]
    fn dispatch_skips_candidates_through_divergent_pools() {
        // Pre-flag pool A as divergent this block.
        const POOL_A: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        const POOL_B: Address = address!("dddddddddddddddddddddddddddddddddddddddd");
        let asserter = Asserter::new();
        asserter.push_success(&access_list_response());
        let provider = mock_provider(&asserter);
        let suppression = Arc::new(Mutex::new(PathSuppression::new()));
        let pool_divergence = Arc::new(Mutex::new(crate::PoolDivergence::new()));
        let fot_registry = Arc::new(Mutex::new(crate::FeeOnTransferRegistry::new()));
        let bot_state = Arc::new(StateLock::new(BotState::new()));

        pool_divergence
            .lock()
            .unwrap()
            .record_divergence(crate::PoolDivergenceKey::V2(POOL_A), 100);

        let cands = vec![
            candidate_through_pool(50, POOL_A), // divergent → dropped pre-sim
            candidate_through_pool(51, POOL_B), // clean → reaches sim
        ];
        let outcome = dispatch_profitable_results(
            cands,
            &ctx(&provider),
            &suppression,
            100,
            MIN_PROFIT_NET,
            0,
            &pool_divergence,
            &fot_registry,
            Some(bot_state),
            Some(degenbot_simulation::WarmCodeCacheInner::shared_default()),
        );

        // The divergent candidate dropped pre-sim; the clean one reached sim
        // (build fails → rpc-failed).
        assert_eq!(outcome.divergent_dropped, 1, "divergent candidate dropped");
        assert_eq!(
            outcome.candidate_count, 1,
            "only the clean candidate simmed"
        );
        assert_eq!(outcome.fail_buckets.get("rpc-failed"), 1);
        // The lifetime tally bumped once.
        assert_eq!(
            pool_divergence.lock().unwrap().total_divergent_dropped(),
            1,
            "record_dropped bumped the lifetime tally"
        );
        // The dropped candidate's path_id is NOT in the failures (it never
        // simulated).
        let simmed_ids: Vec<u64> = outcome.failures.iter().map(|f| f.path_id).collect();
        assert!(simmed_ids.contains(&51), "clean candidate simmed");
        assert!(
            !simmed_ids.contains(&50),
            "divergent candidate never simmed"
        );
    }

    /// The feedback (step 7) does NOT record divergence for non-`SolverCalc`
    /// failures. The build-fails path tallies `rpc-failed` (empty
    /// `captured_swaps` → `is_solver_calc_failure` is false), so the memo stays
    /// empty after the fan-out — the next block's skip is unaffected.
    #[test]
    fn dispatch_feedback_skips_non_solvercalc_failures() {
        let asserter = Asserter::new();
        asserter.push_success(&access_list_response());
        let provider = mock_provider(&asserter);
        let suppression = Arc::new(Mutex::new(PathSuppression::new()));
        let pool_divergence = Arc::new(Mutex::new(crate::PoolDivergence::new()));
        let fot_registry = Arc::new(Mutex::new(crate::FeeOnTransferRegistry::new()));
        let bot_state = Arc::new(StateLock::new(BotState::new()));

        let cands = vec![candidate_through_pool(
            60,
            address!("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"),
        )];
        let outcome = dispatch_profitable_results(
            cands,
            &ctx(&provider),
            &suppression,
            100,
            MIN_PROFIT_NET,
            0,
            &pool_divergence,
            &fot_registry,
            Some(bot_state),
            Some(degenbot_simulation::WarmCodeCacheInner::shared_default()),
        );

        // rpc-failed (empty captured_swaps) → not SolverCalc → no feedback.
        assert_eq!(outcome.fail_buckets.get("rpc-failed"), 1);
        assert_eq!(outcome.fail_count, 1);
        // The memo is unchanged — no pools flagged.
        assert!(
            pool_divergence
                .lock()
                .unwrap()
                .divergent_pools(100)
                .is_empty(),
            "non-SolverCalc failures must not record divergence"
        );
    }

    /// `order-index` feature: the pre-sim selection is deterministic (profit
    /// desc, id-asc tie-break) regardless of the caller's order — parity with a
    /// brute-force profit sort (ADR-024 / 34VCC2).
    #[cfg(feature = "order-index")]
    #[test]
    fn order_index_top_selection_is_deterministic() {
        let path = two_v2_hops();
        let mk = |id: u64, profit: u128| DispatchCandidate {
            path_id: id,
            optimal_input: 0,
            engine_profit: profit,
            steps: Box::new([]),
            solve_block: 100,
            path_info: path.clone(),
            opts: EncodeOptions {
                erc6909_profit: false,
                use_v4_batch: false,
                ..Default::default()
            },
        };
        // Deliberately passed in a non-sorted order, with a profit tie (100).
        let mut cands = vec![mk(10, 100), mk(5, 500), mk(9, 100), mk(20, 300)];
        order_index_top_selection(&mut cands);
        let order: Vec<(u64, u128)> = cands.iter().map(|c| (c.path_id, c.engine_profit)).collect();
        // profit descending, then id ascending on the 100-tie.
        assert_eq!(order, vec![(5, 500), (20, 300), (9, 100), (10, 100)]);
    }
}
