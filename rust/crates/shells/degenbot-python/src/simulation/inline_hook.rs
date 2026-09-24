//! The production `InlineSimulator` implementation (SIMPIPE2 T4 prerequisite) —
//! the degenbot-python closure over the session sim config.
//!
//! The engine's `degenbot_bot` seam stays strategy-agnostic (ADR-019 D7):
//! this crate installs the concrete simulator. One per-path sim drives the
//! SAME core the FFI fan-out drives — `BlockSimHandle` (the layered DB:
//! `AlloyDB` → `WrapDatabaseAsync` → `BotStateDb` → `WarmCodeCache` →
//! `CacheDB`, the state overrides applied once) → `simulate_path_on_evm` —
//! so the two arms' verdicts are comparable by construction (the T4
//! soak's premise). The deliberate differences:
//!
//! - ZERO FFI: the sim runs entirely in the solve worker's thread context.
//! - The per-path handle build (the `M1` amortization target) still happens
//!   per candidate — the soak MEASURES that (a per-block handle reuse
//!   inside the worker is a follow-up if the tail demands it).
//! - `block_priority_fees` is `None` (the dispatcher's fee ring is
//!   Python-owned): the priority fee falls to the core's self-targeting
//!   formula without the percentile clamp. The soak compares LATENCY
//!   primarily; the formula (target/decay) dominates the clamp on mainnet.
//!
//! Runtime caveat (the T4 body note, updated by LW-T9):
//! `WrapDatabaseAsync::new` captures `Handle::try_current()` at build time,
//! and its DB calls escalate to `block_in_place` on a multi-threaded
//! worker. The fleet worker seats are `std` threads — NO ambient runtime
//! (the LW-T2 wedge test pins this structurally; the old
//! sync-inside-async BY DESIGN comment was that invariant's only
//! enforcement before the cutover) — so the sim body spawns onto this
//! hook's DEDICATED multi-thread runtime and the SYNC sim
//! drive runs on that runtime's BLOCKING pool (`tokio::task::spawn_blocking`):
//! a blocking-pool thread still carries the runtime handle (the
//! build-time capture succeeds), and revm's `WrapDatabaseAsync::block_on`
//! bridging (`block_in_place` under a multi-thread handle) collapses to a
//! plain call there — a DB wait never converts a runtime worker, and the
//! runtime never spawns replacement workers mid-block. The worker only
//! awaits the blocking section; the plain caller thread blocks on the
//! join. Out-of-band work
//! (affect-cache misses, escalations) rides the injected `EscalationPort`
//! — never an ambient `Handle`.

use degenbot_core::op_info;
use std::sync::Arc;

use alloy::primitives::{Bytes, U256};
use degenbot_arbitrage::{
    simulate_path_on_evm_in_span, FailBuckets, SimResult, SimulateContext, SimulatePath, SolveStep,
};
use degenbot_bot::arb_engine::inline_sim::{
    AccessListRow, CapturedSwapRow, InlineSimFailure, InlineSimRequest, InlineSimulator,
    InlineSwapFamily, SimulatedPathResult,
};
use degenbot_bot::bot_core::state_lock::StateLock;
use degenbot_bot::bot_core::{BotState, SimAnchorState};
use degenbot_executor::composers::EncodeOptions;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm::inspectors::SwapFamily;
use degenbot_simulation::WarmCodeCacheInner;
use parking_lot::RwLock;
use std::future::Future;

/// The session-static sim config + the shared state arc-set the closure
/// closes over. Built once (at `install_inline_simulator` time) and shared
/// by every per-path worker call for the engine's lifetime.
pub(crate) struct InlineSimHook {
    /// The `sim_bounded` provider (fail-fast cold-miss budget — the dispatch
    /// seam's incident-2026-08-20 discipline).
    provider: Arc<AlloyProvider>,
    executor_owner: alloy::primitives::Address,
    executor_address: alloy::primitives::Address,
    weth_address: alloy::primitives::Address,
    pool_manager_address: alloy::primitives::Address,
    multicall3_address: alloy::primitives::Address,
    inject_code: bool,
    injected_address: Option<alloy::primitives::Address>,
    runtime_bytecode: Bytes,
    warmup: degenbot_executor::WarmupSlots,
    erc6909_profit: bool,
    /// The shared core (the sim anchor's snapshot source) — the SAME short
    /// read discipline as the FFI path (ULUWNI: snapshot under a short read,
    /// drop the guard BEFORE any provider I/O).
    bot_state: Arc<StateLock<BotState>>,
    warm_cache: Arc<RwLock<WarmCodeCacheInner>>,
    /// The dedicated multi-thread sim runtime (the T4 body note — see the
    /// module doc). Built once, shared for the hook's lifetime.
    sim_runtime: Arc<tokio::runtime::Runtime>,
    /// SIMPIPE2 M2: per-block storage memo shared by every payload sim of
    /// the same sim height (recreated on block advance). Collapses the
    /// ~20-cold-storage-RPC-per-sim into ~per-pool-unique per cycle.
    storage_memo: std::sync::Mutex<(u64, Arc<degenbot_simulation::StorageMemo>)>,
    /// VERIFY2 T2: paths whose LAST sim failed - their next sim re-verifies
    /// with the divergence probe armed (engine-vs-RPC comparison on the same
    /// storage reads). Cleared after one armed sim (verify once per failure).
    reverify_armed: std::sync::Mutex<std::collections::HashSet<u64>>,
    /// VERIFY2 T2: the random spot-check arm counter; 0 = the env spot-check
    /// is off.
    spotcheck_n: std::sync::atomic::AtomicU64,
}

impl InlineSimHook {
    /// The escalation port bound to the hook's sim runtime (LW-T3 Seam C):
    /// handed to `install_default_escalation_port` at hook install — the
    /// default impl's capability lane IS the inline-sim runtime; the
    /// cold-miss budget ceiling is the runtime's drive capacity.
    pub(crate) fn escalation_port(
        &self,
    ) -> std::sync::Arc<dyn degenbot_workers::lane::EscalationPort> {
        std::sync::Arc::new(SimRuntimeEscalationPort::new(Arc::clone(&self.sim_runtime)))
    }
}

fn outputs_vec(req: &InlineSimRequest) -> Vec<u128> {
    req.hop_outputs
        .iter()
        .map(|v| u128::try_from(*v).unwrap_or(u128::MAX))
        .collect()
}

impl InlineSimHook {
    /// Assemble the hook from the installed `PyO3` context (see
    /// `PyArbEngine::install_inline_simulator`).
    #[expect(clippy::too_many_arguments)]
    pub(crate) fn new(
        provider: Arc<AlloyProvider>,
        executor_owner: alloy::primitives::Address,
        executor_address: alloy::primitives::Address,
        weth_address: alloy::primitives::Address,
        pool_manager_address: alloy::primitives::Address,
        multicall3_address: alloy::primitives::Address,
        inject_code: bool,
        injected_address: Option<alloy::primitives::Address>,
        runtime_bytecode: Bytes,
        warmup: degenbot_executor::WarmupSlots,
        erc6909_profit: bool,
        bot_state: Arc<StateLock<BotState>>,
        warm_cache: Arc<RwLock<WarmCodeCacheInner>>,
    ) -> Self {
        Self {
            provider,
            executor_owner,
            executor_address,
            weth_address,
            pool_manager_address,
            multicall3_address,
            inject_code,
            injected_address,
            runtime_bytecode,
            warmup,
            erc6909_profit,
            bot_state,
            warm_cache,
            storage_memo: std::sync::Mutex::new((
                0,
                Arc::new(degenbot_simulation::StorageMemo::new()),
            )),
            reverify_armed: std::sync::Mutex::new(std::collections::HashSet::new()),
            spotcheck_n: std::sync::atomic::AtomicU64::new(0),
            sim_runtime: Arc::new(build_inline_sim_runtime()),
        }
    }

    /// The EIP-1559 `next_base_fee` (the `calculations/evm_math.py` port —
    /// the worker-side twin of the driver's pre-sim computation).
    fn next_base_fee(req: &InlineSimRequest) -> u128 {
        if req.parent_base_fee == 0 {
            return 0; // pre-EIP-1559 / unknown parent
        }
        let parent_base_fee = u128::from(req.parent_base_fee);
        let last_gas_target = (req.parent_gas_limit / 2).max(1);
        if req.parent_gas_used == last_gas_target {
            return parent_base_fee;
        }
        if req.parent_gas_used > last_gas_target {
            let gas_used_delta = req.parent_gas_used - last_gas_target;
            let base_fee_delta =
                (parent_base_fee * u128::from(gas_used_delta) / u128::from(last_gas_target) / 8)
                    .max(1);
            parent_base_fee + base_fee_delta
        } else {
            let gas_used_delta = last_gas_target - req.parent_gas_used;
            let base_fee_delta =
                parent_base_fee * u128::from(gas_used_delta) / u128::from(last_gas_target) / 8;
            parent_base_fee.saturating_sub(base_fee_delta)
        }
    }

    /// Convert a core `SimResult` into the primitive payload (the T1 field
    /// table).
    fn payload_from_sim(path_id: u64, sim: &SimResult) -> SimulatedPathResult {
        SimulatedPathResult {
            path_id,
            gross_profit: sim.gross_profit,
            net_profit: sim.net_profit,
            gas_used: sim.gas_used,
            priority_fee: sim.priority_fee,
            base_fee_next: sim.base_fee_next,
            execute_calldata: sim.execute_calldata.to_vec(),
            access_list: sim.access_list.as_ref().map(|lst| {
                lst.0
                    .iter()
                    .map(|row| AccessListRow {
                        address: row.address,
                        storage_keys: row
                            .storage_keys
                            .iter()
                            .map(|k| U256::from_be_bytes(k.0))
                            .collect(),
                    })
                    .collect()
            }),
            captured_swaps: sim
                .captured_swaps
                .iter()
                .map(|s| CapturedSwapRow {
                    emitter: s.emitter,
                    family: match s.family {
                        SwapFamily::V2 => InlineSwapFamily::V2,
                        SwapFamily::V3 => InlineSwapFamily::V3,
                        SwapFamily::V4 => InlineSwapFamily::V4,
                    },
                    amount0: s.amount0,
                    amount1: s.amount1,
                    sqrt_price_x96: s.sqrt_price_x96,
                    liquidity: s.liquidity,
                    tick: s.tick,
                })
                .collect(),
            hop_count: sim.hop_count,
            failure: None,
        }
    }
}

// SIMPIPE2 T1 continuation: the spawned sim task must re-enter the calling
// span context. tokio::spawn clones tokio task context but NOT the tracing
// span context, so any span created inside the task without this re-entry
// becomes an independent Jaeger trace ROOT (observed 635 orphan roots/60s -
// the in-memory Jaeger ring at ~100k traces shrank
// to a 45-min window because ~30 pct of roots were these 2-span orphans).
/// Spawn `fut` on a runtime, re-entering `parent` so traced work inside the
/// task joins the caller's trace. Awaitable from any thread (plain thread or
/// runtime worker); the caller's current span is captured by value.
pub(crate) async fn spawn_sim_task<T, Fut>(
    parent: tracing::Span,
    fut: Fut,
) -> Result<T, tokio::task::JoinError>
where
    Fut: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    tokio::spawn(async move {
        // THE FIX: re-enter the caller's span. The tracing thread-local on a
        // fresh runtime worker is empty, so without this the first span
        // created inside the task becomes an independent trace root that
        // never joins the block's solve trace. `parent.enter()` installs the
        // caller's tracing+OTel context for the task body (guard held till
        // the future completes; harmless if `parent` is a no-op root).
        let _guard = parent.enter();
        fut.await
    })
    .await
}

/// LW-T3 (Seam C): the DEFAULT escalation port impl — the inline-sim
/// runtime IS the port's own capability lane (pyo3 is fine here; the trait
/// lives pyo3-free in degenbot-workers, ADR-042 §8) and escalated work
/// NEVER occupies the submitting seat (CPU cannot starve I/O). The
/// cold-miss budget ceiling is the runtime's drive capacity (worker count)
/// and is visible at [`EscalationPort::counters`].
///
/// RED scaffold: the budget is NOT enforced and the caller's span context
/// is NOT propagated (the orphan-root pattern) — the red tests
/// fail exactly there.
pub(crate) struct SimRuntimeEscalationPort {
    runtime: Arc<tokio::runtime::Runtime>,
    gate: Arc<degenbot_workers::lane::EscalationGate>,
}

impl SimRuntimeEscalationPort {
    /// Build the port over the sim runtime; the cold-miss budget is the
    /// runtime's worker count (its drive capacity).
    /// RED: the cap is WORKER-PARITY BY DESIGN (each escalation busies a
    /// runtime worker, as blocking sims do) — should escalations become
    /// io-await-heavy enough to sit purely on I/O resources, re-derive this
    /// cap and document the NEW model here; do not let the number drift by
    /// accident.
    pub(crate) fn new(runtime: Arc<tokio::runtime::Runtime>) -> Self {
        let budget = runtime.metrics().num_workers();
        Self {
            runtime,
            gate: degenbot_workers::lane::EscalationGate::new(budget.max(1)),
        }
    }
}

impl degenbot_workers::lane::EscalationPort for SimRuntimeEscalationPort {
    fn escalate(
        &self,
        work: degenbot_workers::lane::EscalationWork,
    ) -> Result<(), degenbot_workers::lane::EscalationError> {
        // Fail-fast: the cold-miss budget is the escalation capacity
        // ceiling — a refusal is typed and synchronous, never a hang.
        let permit = self.gate.begin()?;
        // Span re-entry: the escalation RE-ENTERS the caller's span —
        // Jaeger continuity survives the port (no orphan roots).
        let parent = tracing::Span::current();
        // FinishOnDrop moves INTO the task: completion, panic and cancel
        // all reclaim the in-flight slot identically (no budget leak), and
        // only work that RUNS TO COMPLETION tallies into `completed`.
        self.runtime.spawn(async move {
            let _guard = parent.enter();
            work.await;
            permit.retire();
        });
        Ok(())
    }

    fn counters(&self) -> degenbot_workers::lane::EscalationCountersSnapshot {
        self.gate.counters()
    }
}

/// Join a sim-task future from a worker seat: the dedicated sim runtime is
/// driven directly. The ambient-runtime arm the old treatment carried
/// (`block_in_place` + ambient-handle drive) is DELETED with the tokio
/// solve stance (LW-T9): every caller is a fleet worker seat — plain OS
/// thread, NO ambient runtime, pinned structurally by the LW-T2 wedge test
/// in `degenbot_bot::arb_engine::fleet_solve_executor` — and sim
/// escalations drive on the injected `EscalationPort`, never an ambient
/// `Handle`. A future caller that DOES hold an ambient runtime now fails
/// loudly (`block_on` inside a runtime panics — the 2026-09-05 soak freeze
/// #2 anti-pattern the old arm re-purchased) instead of silently
/// `block_in_place`-driving on a CPU seat.
fn join_sim_task<F>(sim_runtime: &tokio::runtime::Runtime, fut: F) -> F::Output
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    sim_runtime.block_on(fut)
}

/// GOQWCL: the inline-sim runtime previously kept the default thread name
/// (`tokio-runtime-worker`) — indistinguishable in thread dumps from every
/// other defaulting pool. The census declares the distinct
/// `degenbot-inline-sim-{n}` pattern; the seq closure mirrors tokio 1.53's
/// per-thread `thread_name_fn` invocation (same mechanism as
/// `degenbot_core::runtime`).
static INLINE_SIM_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn inline_sim_thread_name() -> String {
    format!(
        "degenbot-inline-sim-{}",
        INLINE_SIM_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// The private multi-thread runtime hosting the payload sims ).
/// Sized by [`inline_sim_worker_count`], named distinctly, census-registered
fn build_inline_sim_runtime() -> tokio::runtime::Runtime {
    let workers = inline_sim_worker_count();
    degenbot_core::worker_census::register(degenbot_core::worker_census::WorkerCensusEntry {
        resource: "inline_sim_runtime_workers",
        kind: "tokio multi-thread runtime (inline-sim hook — payload sims; fleet-hosted SimDriver target, ADR-042)",
        count: workers,
        thread_name: "degenbot-inline-sim-{n}",
        sizing: "leftover_worker_budget; override `solve.inline_sim_workers` (env DEGENBOT_INLINE_SIM_WORKERS), clamp 1..=32",
        binding: "shared",
    });
    #[expect(clippy::expect_used)]
    // unreachable in production: multi-thread Builder only fails on allocator OOM or invalid config (worker count is clamped 1..=32)
    tokio::runtime::Builder::new_multi_thread()
        // M2 soak sizing (2026-09-05): with the hard-coded 2
        // workers the per-cycle wall was 72ms + 4.74ms/path
        // (R^2 0.89, 228 steady cycles) - the payload sims queued
        // on the 2-thread runtime while ~50 bins/cycle arrived
        // concurrently (sims p50 12ms). Sizing to the core count
        // lets the bins' sims actually overlap. Env-tunable for
        // constrained hosts. Workers exist for SIM
        // PARALLELISM, NOT block_on compensation — the sync sim drive runs
        // on this runtime's BLOCKING pool (`spawn_blocking`, see the sim
        // body), so DB waits never convert a worker and the blocking adds
        // no sizing pressure on this count.
        .worker_threads(workers)
        .thread_name_fn(inline_sim_thread_name)
        .enable_all()
        .build()
        .expect("inline-sim runtime build")
}

/// The inline-sim runtime's worker count (M2 soak sizing): core-count by
/// default (the payload sims are the per-path marginal cost of every solve
/// cycle - see the M1/M2 soak records), overridable via the typed
/// `solve.inline_sim_workers` key (env `DEGENBOT_INLINE_SIM_WORKERS`).
/// Value parsing/validation is the loader's job (fail-closed at boot,
/// KAHU5W); this layer just clamps to 1..=32.
fn inline_sim_worker_count() -> usize {
    // Two-runtime sizing: the sim runtime follows the LEFTOVER
    // of the CPU budget after the solve bins (not raw available
    // parallelism), so sim runtime workers + solve bins never exceed
    // the quota.
    let default = degenbot_core::cpu_budget::leftover_worker_budget();
    // typed schema key `solve.inline_sim_workers`
    // (`DEGENBOT_INLINE_SIM_WORKERS`); the loader owns the env read.
    match ::degenbot_config::holder::config().solve.inline_sim_workers {
        Some(n) => n.clamp(1, 32),
        None => default,
    }
}

impl InlineSimulator for InlineSimHook {
    #[expect(clippy::too_many_lines)]
    fn simulate_path(&self, req: InlineSimRequest) -> Option<SimulatedPathResult> {
        // 1. PathInfo STRAIGHT OFF THE CORE — engine-Lock-FREE. The calling
        //    cycle holds the engine `Mutex` for its whole duration (the busy
        //    loop that owns this worker), so re-entering the engine lock from
        //    the worker would self-deadlock (cycle-waits-on-bin,
        //    bin-waits-on-cycle — the 2026-09-05 soak freeze). The projection
        //    (build_path_info) needs only a short core read, same class as
        //    the T2 clamp's read. Unknown/unresolvable hops degrade to `None`
        //    (the batch entry stays payload-less -> the legacy FFI path).
        let path_info = {
            let core = self
                .bot_state
                .read_at(degenbot_bot::bot_core::state_lock::LockSite::Python);
            match degenbot_bot::arb_engine::build_path_info(&core, &req.hops) {
                Ok(pi) => pi,
                Err(_) => return None,
            }
        };

        // 2. The sim anchor — SHORT core read, dropped before any provider
        //    I/O (ULUWNI discipline).
        let anchor = {
            let guard = self
                .bot_state
                .read_at(degenbot_bot::bot_core::state_lock::LockSite::Python);
            SimAnchorState::snapshot(&guard)
        };

        // 3. The sim path from the clamp-committed solve outputs (u128 steps;
        //    >u128 values are the int-overflow class anyway).
        let to_u128 = |v: &U256| u128::try_from(*v).ok();
        let optimal_input = to_u128(&req.optimal_input)?;
        let consumed: Vec<Option<u128>> = req.consumed_inputs.iter().map(to_u128).collect();
        let outputs: Vec<Option<u128>> = req.hop_outputs.iter().map(to_u128).collect();
        if consumed.iter().any(Option::is_none) || outputs.iter().any(Option::is_none) {
            return None;
        }
        let steps: Vec<SolveStep> = consumed
            .into_iter()
            .zip(outputs)
            .enumerate()
            .map(|(i, (consumed_input, output))| SolveStep {
                output: output.unwrap_or(0),
                consumed_input: consumed_input.unwrap_or(0),
                state_nonce: req.state_nonces.get(i).copied().unwrap_or(0),
            })
            .collect();
        let sim_path = SimulatePath {
            path_id: req.path_id,
            optimal_input,
            steps: steps.into_boxed_slice(),
            path_info,
            solve_block: req.sim_block,
            opts: EncodeOptions {
                erc6909_profit: self.erc6909_profit,
                ..EncodeOptions::default()
            },
        };

        let path_id = req.path_id;
        let hop_count = req.hop_outputs.len();
        let base_fee_next = Self::next_base_fee(&req);
        let provider = Arc::clone(&self.provider);
        let warm_cache = Arc::clone(&self.warm_cache);
        // M2: the cycle-scoped storage memo - one per sim block; sims at a
        // new block recreate it (pre-state differs across heights).
        // VERIFY2 T2: on-demand verification arming. A path whose last sim
        // failed re-simulates with the divergence probe armed; fresh paths
        // may sample into a spot-check at DEGENBOT_VERIFY_SPOTCHECK_PERMYRIAD
        // per-ten-thousand (default 0 = off). Armed probes cost no extra RPC
        // - the engine-vs-RPC comparison rides the same storage reads - and
        // only log on a real tracked-field mismatch.
        let reverify = self
            .reverify_armed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&req.path_id);
        let spotcheck = {
            static PERMYRIAD: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
            // typed schema key `verify.verify_spotcheck_permyriad`.
            let permyriad = *PERMYRIAD.get_or_init(|| {
                ::degenbot_config::holder::config()
                    .verify
                    .verify_spotcheck_permyriad
            });
            permyriad > 0
                && self
                    .spotcheck_n
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    .is_multiple_of(10_000 / permyriad.min(10_000))
        };
        let verify_divergence = reverify || spotcheck;
        if verify_divergence {
            op_info!(
                domain = sim,
                path_id = req.path_id,
                reason = if reverify { "fail-retry" } else { "spot-check" },
                "divergence probe armed (on-demand verification)"
            );
        }
        let storage_memo = {
            // A poisoned memo lock is recoverable: the memo is block-scoped
            // and purely advisory (the fallback path re-fetches on a miss).
            let mut guard = match self.storage_memo.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            if guard.0 != req.sim_block {
                // M2 visibility: the retiring block's memo tally - the hit
                // share is the RPC-trip compression the memo buys. The
                // degenbot::diag target stays off the Python log cap (console
                // + OTel only).
                let (hits, misses) = guard.1.stats();
                if hits + misses > 0 {
                    op_info!(
                        domain = sim,
                        block_number = guard.0,
                        memo.hits = hits,
                        memo.misses = misses,
                        "storage memo stats (block retired)"
                    );
                }
                *guard = (
                    req.sim_block,
                    Arc::new(degenbot_simulation::StorageMemo::new()),
                );
            }
            Arc::clone(&guard.1)
        };
        let executor_owner = self.executor_owner;
        let executor_address = self.executor_address;
        let weth_address = self.weth_address;
        let pool_manager_address = self.pool_manager_address;
        let multicall3_address = self.multicall3_address;
        let inject_code = self.inject_code;
        let injected_address = self.injected_address;
        let runtime_bytecode = self.runtime_bytecode.clone();
        let warmup = self.warmup;

        // 4. The sim body runs as a task on the dedicated runtime (see the
        //    module doc for the thread/runtime matrix). The request is cloned
        //    into the task (the outer conversions read the original after).
        let (result, buckets): (Result<Option<SimResult>, String>, FailBuckets) = {
            let req_task = req.clone();
            // capture the caller's span (the worker's entered
            // `degenbot.bundle.simulate`) BEFORE the runtime hop; the helper
            // re-enters it inside the spawned task so `degenbot.simulate.inline`
            // joins the block trace instead of forking an orphan root.
            // SIMSPANDUP: the same capture is ALSO the seam span the sim body
            // reuses (see `simulate_path_on_evm_in_span`) — the seam no longer
            // opens a second same-named span under `degenbot.simulate.inline`.
            let sim_task_parent = tracing::Span::current();
            let seam_span = tracing::Span::current();
            let sim_future = async move {
                spawn_sim_task(sim_task_parent, async move {
                    // SIMPIPE2 M1 span parity: the engine-side sim stays
                    // Jaeger-visible via `degenbot.simulate.inline`, nested
                    // under the spawning solve worker's span context (the
                    // T1 helper forwards the caller's span across the
                    // tokio::spawn). The `degenbot.bundle.simulate` span the
                    // worker already holds is REUSED by the seam body
                    // (SIMSPANDUP) - one inline sim = two spans, no duplicate.
                    let req = req_task;
                    let inline_span = tracing::info_span!(
                        target: "degenbot::solver",
                        "degenbot.simulate.inline",
                        path_id = req.path_id,
                        sim_block = req.sim_block,
                        hops = req.hops.len(),
                        sim_ok = false,
                    );
                    // The sync sim drive BLOCKS on its cold-miss
                    // path — revm's `WrapDatabaseAsync` bridges the async
                    // provider via `block_in_place` + `handle.block_on`
                    // under this multi-thread runtime. That drive runs on
                    // the runtime's BLOCKING pool (`spawn_blocking`), NOT
                    // on the worker: on a blocking-pool thread the internal
                    // `block_in_place` setup collapses to a plain call
                    // (blocking is what that pool exists for), so a DB wait
                    // never converts a worker and the runtime never spawns
                    // mid-block replacement workers. The blocking-pool
                    // thread carries the ambient handle (the pool `enter`s
                    // its runtime), so both the build-time capture and the
                    // drive succeed there; the worker below only awaits.
                    let eval = tokio::task::spawn_blocking(move || {
                        let _sim_span = inline_span.clone().entered();
                        let ctx = SimulateContext {
                            provider: &provider,
                            executor_owner,
                            executor_address,
                            weth_address,
                            pool_manager_address,
                            multicall3_address,
                            inject_code,
                            injected_address,
                            runtime_bytecode,
                            warmup,
                            base_fee_next,
                            current_block: req.sim_block,
                            block_timestamp: req.block_timestamp,
                            block_priority_fees: None,
                        };
                        if let Some(mut handle) = degenbot_simulation::BlockSimHandle::build(
                            &provider,
                            base_fee_next,
                            req.sim_block,
                            req.block_timestamp,
                            &ctx.override_params(),
                            &anchor,
                            &warm_cache,
                            Some(&storage_memo),
                            verify_divergence,
                        ) {
                            let mut buckets = FailBuckets::new();
                            // SIMSPANDUP: the sim body rides the CALLER-HELD
                            // `degenbot.bundle.simulate` span (captured before the
                            // runtime hop below) — the seam must not open a
                            // same-named duplicate nested under this span.
                            let result = simulate_path_on_evm_in_span(
                                handle.evm_mut(),
                                &ctx,
                                &sim_path,
                                &mut buckets,
                                &seam_span,
                            )
                            .map_err(|e| format!("{e}"));
                            inline_span.record(
                                "sim_ok",
                                result.as_ref().ok().and_then(|o| o.as_ref()).is_some(),
                            );
                            (result, buckets)
                        } else {
                            // No ambient runtime at build (a blocking-pool thread
                            // always carries the handle — see the module docs) / an
                            // override error:
                            // tally `rpc-failed` (mirrors the FFI build-failure arm).
                            let mut buckets = FailBuckets::new();
                            buckets.record(
                                req.path_id,
                                "rpc-failed",
                                None,
                                Bytes::new(),
                                optimal_input,
                                outputs_vec(&req),
                            );
                            (Ok(None), buckets)
                        }
                    })
                    .await;
                    match eval {
                        Ok(eval) => eval,
                        Err(e) => {
                            // The blocking sim section PANICKED — re-raise the
                            // payload through the outer task so the join
                            // conversion below records `exception` exactly as
                            // a synchronous sim panic would (same JoinError →
                            // same bucket + Err string).
                            std::panic::resume_unwind(e.into_panic())
                        }
                    }
                })
                .await
                .unwrap_or_else(|e| {
                    // The sim task panicked — the FFI fan-out's exception class.
                    let mut buckets = FailBuckets::new();
                    buckets.record(
                        req.path_id,
                        "exception",
                        None,
                        Bytes::new(),
                        optimal_input,
                        outputs_vec(&req),
                    );
                    (Err(format!("{e}")), buckets)
                })
            };
            join_sim_task(&self.sim_runtime, sim_future)
        };

        // 5. Convert: Ok(Some) → success payload; Ok(None)/Err → failure
        //    payload through the buckets (one record — single path).
        if let Ok(Some(sim)) = result {
            Some(Self::payload_from_sim(path_id, &sim))
        } else {
            {
                // VERIFY2 T2: this sim failed - arm the path so its NEXT sim
                // re-verifies with the divergence probe (the failure itself
                // already carries its failure data in the payload). The set
                // is flood-guarded (cleared beyond 1024 arms).
                {
                    let mut arm = self
                        .reverify_armed
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if arm.len() >= 1024 {
                        arm.clear();
                    }
                    arm.insert(path_id);
                }
                let mut failures = buckets.into_failures();
                let f = failures.pop()?;
                Some(SimulatedPathResult {
                    path_id,
                    gross_profit: U256::ZERO,
                    net_profit: U256::ZERO,
                    gas_used: 0,
                    priority_fee: 0,
                    base_fee_next: 0,
                    execute_calldata: Vec::new(),
                    access_list: None,
                    captured_swaps: Vec::new(),
                    hop_count,
                    failure: Some(InlineSimFailure {
                        fail_index: f.fail_index,
                        revert_data: f.revert_data.to_vec(),
                        bucket: f.bucket,
                    }),
                })
            }
        }
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)] // census/name assertions follow the repo's loud-assert test style
mod tests {
    use std::sync::Arc;

    // The trait resolves the port methods (escalate/counters) in tests.
    use degenbot_workers::lane::EscalationPort as _;

    // The override parsing matrix moved to degenbot-config's precedence
    // tests (KAHU5W: the loader owns the env read). This pins the production
    // default only: with no override, the count follows the leftover CPU
    // budget, NOT raw available_parallelism.
    #[test]
    fn inline_sim_worker_count_defaults_to_leftover_budget() {
        let default = degenbot_core::cpu_budget::leftover_worker_budget();
        assert_eq!(super::inline_sim_worker_count(), default);
    }

    /// LW-T3 (Seam C): the DEFAULT escalation port enforces the cold-miss
    /// budget fail-fast (typed `ColdMissBudgetExceeded`, visible on the
    /// gauge surface) and retired escalations re-free capacity.
    #[test]
    fn inline_sim_escalation_port_enforces_the_cold_miss_budget_fail_fast() {
        let port =
            super::SimRuntimeEscalationPort::new(Arc::new(super::build_inline_sim_runtime()));
        // Park `budget` escalations on the lane; they hold the whole
        // capacity (the worker count).
        let (release1, parked1) = tokio::sync::oneshot::channel::<()>();
        let (release2, parked2) = tokio::sync::oneshot::channel::<()>();
        port.escalate(Box::pin(async move {
            let _ = parked1.await;
        }))
        .expect("first escalation within budget");
        port.escalate(Box::pin(async move {
            let _ = parked2.await;
        }))
        .expect("second escalation within budget");
        let refused = port
            .escalate(Box::pin(std::future::ready(())))
            .expect_err("the cold-miss budget must refuse SYNCHRONOUSLY, not hang");
        assert_eq!(
            refused,
            degenbot_workers::lane::EscalationError::ColdMissBudgetExceeded
        );
        assert_eq!(
            port.counters().budget_exceeded,
            1,
            "the refusal must be visible on the gauge surface"
        );
        drop((release1, release2)); // retire the parked escalations
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while port.counters().completed < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "parked escalations did not retire in time"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        port.escalate(Box::pin(std::future::ready(())))
            .expect("completed escalations re-free capacity");
    }

    /// LW-T3 (Seam C, reth `IncCounterOnDrop`): an escalated future that
    /// PANICS must NOT leak its admission slot — finish-on-drop covers
    /// completion, panic and cancel identically; a leaked slot is a budget
    /// leak into permanent hard-refusal (the raddest fail-fast failure
    /// mode). Cancel-before-completion is pinned at the same invariant (a
    /// dropped permit reclaims; a cancelled escalation is never counted
    /// completed).
    #[test]
    fn a_panicking_escalation_does_not_leak_the_in_flight_slot() {
        let port =
            super::SimRuntimeEscalationPort::new(Arc::new(super::build_inline_sim_runtime()));
        port.escalate(Box::pin(async {
            // The panic IS the fixture's signal (the slot must reclaim on
            // panic exactly as on completion).
            #[expect(
                clippy::panic,
                reason = "panicking escalated work is the reclaimed-slot contract under test"
            )]
            {
                panic!("cold-miss escalation panics mid-work (LW-T3)");
            }
        }))
        .expect("escalation admitted");
        // The admission must be RECLAIMED whether the work panicked,
        // completed or was cancelled: in_flight returns to 0 with no other
        // signal, and a subsequent escalation succeeds.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while port.counters().in_flight != 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the panicking escalation leaked the in-flight slot"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        port.escalate(Box::pin(std::future::ready(())))
            .expect("a leaked slot must NOT hard-refuse subsequent escalations");
    }

    /// The inline-sim runtime's workers must carry the
    /// DISTINCT census thread name — never the shared
    /// `tokio-runtime-worker` default that made thread dumps
    /// unattributable when both runtimes overlapped.
    #[test]
    fn inline_sim_runtime_workers_carry_the_census_thread_name() {
        let rt = super::build_inline_sim_runtime();
        let name = rt.block_on(async {
            tokio::spawn(async move { std::thread::current().name().map(str::to_owned) })
                .await
                .expect("worker spawn")
        });
        let name = name.expect("worker thread name");
        assert!(
            name.starts_with("degenbot-inline-sim-"),
            "inline-sim worker must be census-named, got {name}"
        );
        // And the self-registration agrees with the built runtime.
        let row = degenbot_core::worker_census::snapshot()
            .into_iter()
            .find(|e| e.resource == "inline_sim_runtime_workers")
            .expect("inline-sim runtime must self-register in the census");
        assert_eq!(row.count, rt.metrics().num_workers());
        assert_eq!(row.thread_name, "degenbot-inline-sim-{n}");
    }
}

// the spawned sim task must JOIN the caller's trace, not fork a
// new root. Pinned against the in-memory exporter seam pattern): the
// `degenbot.simulate.inline` span created inside `spawn_sim_task` must carry
// the calling span's trace/parent - the exact relationship Jaeger lost when
// 635 orphan roots/60s fragmented the block traces.
#[cfg(all(test, feature = "otel", not(target_arch = "wasm32")))]
#[expect(clippy::expect_used)] // otel tests assert loudly per telemetry.rs otel_tests
mod spawn_span_parent_tests {
    use std::sync::Arc;

    use degenbot_bot::otel;
    use degenbot_workers::lane::EscalationPort as _;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use tracing_subscriber::layer::SubscriberExt;

    /// Install the process-wide subscriber, or report that another test owns
    /// the slot. `set_global_default` is once-per-process and `cargo test` runs
    /// these tests in parallel (the `python_log_layer` capture test wants the
    /// same slot), so the loser must skip rather than panic.
    fn install_global_or_skip(
        subscriber: impl tracing::Subscriber + Send + Sync + 'static,
    ) -> bool {
        tracing::subscriber::set_global_default(subscriber).is_ok()
    }

    #[test]
    fn sim_task_span_parents_under_the_caller_span() {
        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("test sim runtime");

        // Cross-thread spans need the GLOBAL slot (repo convention, see the
        // block_pump header-span test: with_default is thread-local and the
        // spawned task runs on a runtime worker thread). This crate's test
        // binary installs it at most once.
        if !install_global_or_skip(subscriber) {
            // Another test in this binary holds the process-wide global
            // subscriber slot, so this test capture was never installed and
            // any assertion below would read another test buffer. Each test
            // is verified in isolation: `cargo test -p degenbot_rs --lib
            // simulation::inline_hook`.
            return;
        }

        // Scope the solve span so it ENDS before the flush (an exporter only
        // receives closed spans).
        {
            let solve = tracing::info_span!("degenbot.arb.solve", block.number = 9u64);
            let _guard = solve.enter();
            let parent = tracing::Span::current();

            // The spawned task creates a span exactly like the inline-hook sim
            // body does; with the fix it must JOIN the solve trace.
            let verdict = rt.block_on(super::spawn_sim_task(parent, async {
                let sim = tracing::info_span!("degenbot.simulate.inline", path_id = 5u64);
                let _enter = sim.enter();
                42u8
            }));
            assert_eq!(verdict.expect("join"), 42);
        }; // solve span ends here (guard drop) - before the flush

        provider.force_flush().expect("flush");
        let spans = exporter.get_finished_spans().expect("spans");
        let solve = spans
            .iter()
            .find(|sp| sp.name.as_ref() == "degenbot.arb.solve")
            .expect("caller span must be exported");
        let inline = spans
            .iter()
            .find(|sp| sp.name.as_ref() == "degenbot.simulate.inline")
            .expect("sim span must be exported");
        assert_eq!(
            inline.span_context.trace_id(),
            solve.span_context.trace_id(),
            "sim span must JOIN the caller's trace (no orphan root)"
        );
        assert_eq!(
            inline.parent_span_id,
            solve.span_context.span_id(),
            "sim span must parent under the calling span"
        );
    }

    /// LW-T3 (Seam C): escalations RE-ENTER the caller's span through the
    /// port — Jaeger continuity survives the port (no orphan roots).
    /// RED scaffold: the port does not propagate the span yet.
    #[test]
    fn escalations_reenter_the_caller_span_through_the_port() {
        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
        let port = super::SimRuntimeEscalationPort::new(Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("test escalation runtime"),
        ));
        let done: Arc<std::sync::atomic::AtomicU64> = Arc::default();

        if !install_global_or_skip(subscriber) {
            // Another test in this binary holds the process-wide global
            // subscriber slot, so this test capture was never installed and
            // any assertion below would read another test buffer. Each test
            // is verified in isolation: `cargo test -p degenbot_rs --lib
            // simulation::inline_hook`.
            return;
        }

        {
            let solve = tracing::info_span!("degenbot.arb.solve", block.number = 3u64);
            let _guard = solve.enter();
            let done = Arc::clone(&done);
            port.escalate(Box::pin(async move {
                let child = tracing::info_span!("degenbot.simulate.escalation", path_id = 7u64);
                let _enter = child.enter();
                done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }))
            .expect("escalation within budget");
        } // solve span ends before the flush

        // Wait for the escalated work to run (its span closed) then flush.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while done.load(std::sync::atomic::Ordering::Relaxed) < 1 {
            assert!(
                std::time::Instant::now() < deadline,
                "escalated work never ran in time"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        provider.force_flush().expect("flush");
        let spans = exporter.get_finished_spans().expect("spans");
        let solve = spans
            .iter()
            .find(|sp| sp.name.as_ref() == "degenbot.arb.solve")
            .expect("caller span must be exported");
        let child = spans
            .iter()
            .find(|sp| sp.name.as_ref() == "degenbot.simulate.escalation")
            .expect("escalated span must be exported");
        assert_eq!(
            child.span_context.trace_id(),
            solve.span_context.trace_id(),
            "the escalated span must JOIN the caller's trace (no orphan root)"
        );
        assert_eq!(
            child.parent_span_id,
            solve.span_context.span_id(),
            "the escalated span must parent under the calling span"
        );
    }
}
