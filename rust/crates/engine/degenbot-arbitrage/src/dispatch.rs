//! The candidate-simulation fan-out + categorization.
//!
//! Owns the dispatch contract end to end: the per-block candidate fan-out over
//! the candidates the crate-internal `admission` pass admits, the categorization
//! into gas-profitable / gas-unprofitable / exception tallies, and the
//! suppression bookkeeping the next block's skip reads.
//!
//! This is the concurrency orchestration that owns the GIL release + the
//! tokio fan-out over the per-path [`simulate_path_on_evm`] leaf. Owning
//! the fan-out in Rust releases the GIL across the per-tx sim RPCs
//! (ADR-005 — "Python is the cockpit, Rust is the engine").
//!
//! # Ownership
//!
//! - **This leaf** — the fan-out + categorization: simulate the admitted
//!   candidates, then tally them pure-int into the three categories.
//! - **Consumed, not re-ported** — [`degenbot_submission::PathSuppression`]
//!   (`record_success`/`record_failure`/`is_suppressed`/
//!   `total_suppressed` + `PATH_SUPPRESS_THRESHOLD`).
//! - **`admission`** (crate-internal) — the pre-simulation policy: the stamping
//!   suppression read, divergence, fee-on-transfer, thin margin, staleness,
//!   the optional order-index reorder, and the cap. Its drop tally feeds this
//!   outcome's per-stage counters.
//! - **The driver's** — the `[sim] ... by reason: {breakdown}` summary log
//!   rendering (`format_failure_breakdown`). This leaf exposes the tally as a
//!   typed [`FailBuckets`] (re-exported from `simulator` at this crate's root)
//!   and the companion renders it.
//!
//! # The BAL delta storage opt-in
//!
//! A `DeltaDispatchConfig` on the session's warm cache turns on ONE alternative
//! storage representation for the fan-out: a BAL delta generation
//! (`DeltaHub`) instead of the per-anchor `StorageMemo`. The anchor site
//! resolves the choice once per fan-out
//! (`degenbot_simulation::sim::evm2::warm_code_cache::select_dispatch_storage`)
//! BEFORE any candidate runs.
//!
//! - OFF — the default, and the shipped posture — is the legacy lane
//!   unchanged: the number+hash read anchor, the ONE prefetch batch, and the
//!   same telemetry, with no BAL request anywhere.
//! - ON resolves the anchor's exact lineage `(number, hash)` in one header read
//!   and ASKS the session's one `BalHeadDriver` what that exact anchor answers.
//!   The selection queries; it never delivers, prepares, applies, or claims a
//!   preparation. That work belongs to the session's canonical-head pipeline,
//!   which runs on the pump's accepted-header edge — off every fan-out's path.
//!   A fan-out at the exact head whose preparation is in flight JOINS that
//!   existing preparation and waits for its terminal result, then re-reads the
//!   exact anchor. A head that settled READY answers candidate reads through
//!   its `DeltaHub` and pays NO prefetch batch; an abandoned preparation, a
//!   superseded, settle-cold (unreadable, lineage-refused), or never-delivered
//!   head leaves the fan-out on the legacy lane with the reason reported on
//!   `degenbot.dispatch.storage{mode,reason}` and the `fan-out storage
//!   selection` diag line, and serves no parent value.
//!
//! The first head after enabling has no installed parent, so it settles cold
//! (lineage-refused) once; every consecutive head after it carries, and a
//! repeated fan-out at one exact anchor shares the one settled generation and
//! repeats no BAL work. Nothing else moves: the fan-out order, the candidate
//! execution, and the verdict are the flag's invariant, and the RPC fallback
//! stays the authority for every word the selected view does not hold.
//!
//! # Two entries, one simulation body
//!
//! [`dispatch_profitable_results`] is the RAW standalone leaf: it runs the whole
//! admission pipeline (`stamping suppression -> divergence -> fee-on-transfer ->
//! thin margin -> staleness -> optional order-index reorder -> cap`) and then
//! simulates. [`dispatch_admitted_profitable_results`] is the PRE-ADMITTED leaf
//! the registry fan-out's pre-admitted entry reaches: the caller already ran
//! [`crate::project_early_admission`] — the early half of the same policy, the
//! pre-selection staleness gate included — and transported
//! [`AdmittedCandidate`]s, so the leaf re-reads the policy AUTHORITATIVELY
//! (`stamping suppression -> divergence -> fee-on-transfer -> staleness ->
//! optional order-index reorder -> same-cap truncation`) before simulating.
//!
//! Both leaves hand their survivors to the SAME private simulation body
//! (`simulate_leaf`) — the fan-out, the categorization, the divergence/FoT
//! feedback, the suppression recording, and the sorting have one
//! implementation, and `leaf_outcome` maps a pass' drop tally to
//! [`DispatchOutcome`] once. Only the pass in front of the simulation differs.
//!
//! # Attribution: one `fanout` identifier per fan-out
//!
//! [`dispatch_profitable_results`] mints one identifier per dispatch
//! (`degenbot_simulation::sim::sim_metrics::next_fanout_id`) and echoes it on
//! every row that fan-out emits: the `fan-out storage selection` row, the
//! `storage prefetch` row, the `[sim] cands=` delta summary, and each candidate's
//! `fanout-trace` row. The four row families fire at different seams, so the
//! identifier is what lets a capture join them — and because it is minted HERE,
//! the inline hook re-quote arm's equally shaped trace rows carry no
//! identifier at all and stay unattributed.

use degenbot_core::diag;
use degenbot_core::op_info;
// Solidity/EVM + Rust-ecosystem identifiers (tokio, JoinSet, SolverCalc,
// PathSuppression, etc.) are ubiquitous here.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use alloy::primitives::U256;
use degenbot_execution::{CandidateId, HopInfo, PathInfo};
use degenbot_submission::PathSuppression;
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::BotState;
use parking_lot::RwLock;

use crate::admission;
use crate::admission::AdmittedCandidate;
use crate::identity::{duplicate_candidate_id, DispatchRefusal};
use crate::simulate_path_on_evm_prepared;
use crate::{
    diverging_pool_keys, fot_suspected_token, hop_input_token, hop_output_token,
    is_solver_calc_failure, FailBuckets, FeeOnTransferRegistry, PoolDivergence, SimFailure,
    SimResult, SimulateContext, SimulatePath,
};

// The pre-simulation leaf helpers live in the crate-internal `admission` module
// but keep their published `degenbot_arbitrage::dispatch::*` paths (and, through
// them, the crate root's): the module boundary is an implementation fact, not a
// consumer-visible one.
pub use crate::admission::{
    candidate_is_stale, filter_thin_margin_results, BPS_DENOM, MAX_SIMULATE_CONCURRENT,
};

// ─────────────────────────────────────────────────────────────────────────
// Diagnostics
// ─────────────────────────────────────────────────────────────────────────

/// `the V2-calc probe` env-gated diagnostic: immediately before each
/// candidate path's sim, read every V2 hop's reserves word from the session's
/// settled view and log the decoded `reserve0`/`reserve1` plus the hop's token
/// orientation. `_v2_get_amount_out` (`V2_SWAP_CALC`, cmd 0x21) reads exactly
/// this word via `getReserves()`, so the logged values are the ground truth
/// for what the sim's V2 output is computed from. This closes the
/// observability gap for the path-11354 V3-V2-V3 1-wei under-delivery: a
/// reserves word that no on-chain block holds (synthetic / cached-intra-block /
/// polluted) surfaces here even though every DB layer below the `CacheDB`
/// forwards on-chain state.
///
/// The output is emitted at `debug` level, so it is gated behind the tracing
/// filter (`RUST_LOG=...=debug` / `=trace`) rather than appearing on stderr by
/// default — the `debug` level is the gate.
///
/// The probe reads through the engine's session (ADR-089 cutover: the evm2
/// handle is the only handle).
fn v2_calc_trace(
    handle: &mut degenbot_simulation::sim::evm2::handle::Evm2BlockSimHandle<'_>,
    sim_path: &SimulatePath,
) {
    let mut session = handle.session();
    v2_calc_trace_body(&mut session, sim_path);
}

/// The session verb the probe needs (both engines expose it over the same
/// slot table).
trait V2ReservesReader {
    /// The engine's read-error type.
    type Error: std::fmt::Display;
    /// Read + decode the pair's packed reserves word.
    fn read_v2_reserves(
        &mut self,
        pair: alloy::primitives::Address,
    ) -> Result<degenbot_pools::V2ReservesParts, Self::Error>;
}

impl V2ReservesReader for degenbot_simulation::sim::evm2::session::Evm2SimSession<'_, '_> {
    type Error = degenbot_simulation::sim::evm2::session::Evm2SimError;
    fn read_v2_reserves(
        &mut self,
        pair: alloy::primitives::Address,
    ) -> Result<degenbot_pools::V2ReservesParts, Self::Error> {
        Self::read_v2_reserves(self, pair)
    }
}

/// The probe body, generic over either engine's session (both expose
/// `read_v2_reserves` over the same slot table).
fn v2_calc_trace_body<S: V2ReservesReader>(session: &mut S, sim_path: &SimulatePath) {
    for hop in &sim_path.path_info.hops {
        if let HopInfo::V2(v2) = hop {
            let reserves = match session.read_v2_reserves(v2.pool_address) {
                Ok(reserves) => reserves,
                Err(e) => {
                    diag!(domain = sim, path_id = sim_path.candidate_id.path_id(),
                        deployment_id = sim_path.candidate_id.deployment_id().value(),
                        pair = ?v2.pool_address,
                        %e,
                        "pair reserves read failed"
                    );
                    break;
                }
            };
            diag!(domain = sim, path_id = sim_path.candidate_id.path_id(),
                deployment_id = sim_path.candidate_id.deployment_id().value(),
                pair = ?v2.pool_address,
                token0 = ?v2.token0_address,
                token1 = ?v2.token1_address,
                zfo = v2.zfo,
                fee = v2.fee,
                reserve0 = reserves.reserve0.to::<u128>(),
                reserve1 = reserves.reserve1.to::<u128>(),
                "pair reserves before path execute"
            );
            break;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Dispatch + pre-filter constants
// ─────────────────────────────────────────────────────────────────────────

/// The min net-profit threshold — a gross-profitable result is "gas-profitable"
/// iff `net_profit >= MIN_PROFIT_NET`.
///
/// One wei: `net_profit` already carries the gas cost, so this seam is not the
/// place for a second value judgement — every gross-profitable path reaches the
/// operator's inspect surface.
pub const MIN_PROFIT_NET: u128 = 1;

/// The gas-profitability predicate — the dispatch-floor half of the submit
/// decision, and the one home for it: the FFI batch fan-out (step 8 below) and
/// the inline-payload join both route through it. Keeping the comparison here
/// keeps it out of either adapter.
///
/// The floor alone does not make a result submittable: the deployment's own
/// assessment verdict must have passed too ([`SimResult::is_submittable`]).
/// A result below the floor is tallied gas-unprofitable.
#[must_use]
pub fn is_gas_profitable(net_profit: U256) -> bool {
    net_profit >= U256::from(MIN_PROFIT_NET)
}

// ─────────────────────────────────────────────────────────────────────────
// ─────────────────────────────────────────────────────────────────────────
// The candidate + outcome types (D1)
// ─────────────────────────────────────────────────────────────────────────

/// A pre-simulation candidate — the engine result + the resolved [`PathInfo`].
///
/// Carries the `EngineResult` tuple `(path_id, opt_input, profit, hop_outputs,
/// consumed_inputs, solve_block)`. The `PathInfo` arrives pre-resolved: the
/// registry lookup is the caller's concern (the `degenbot` umbrella's `Bot`
/// owns the engine registry), so this leaf never reaches into solver state.
/// One per-hop row of a candidate (a Cloudflare-style fewer-lists row):
/// the solver's output, the executable input, and the solve-time state
/// nonce, contiguous per hop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SolveStep {
    /// The solver's output after this hop (the former `hop_outputs[i]`).
    pub output: u128,
    /// The executable input fed into this hop's pool (the solver's concentrated-liquidity-hop
    /// clamp) — the former `consumed_inputs[i]`. For V3/V4 hops this may be
    /// less than `output` of the prior hop when the range boundary is hit;
    /// the encoder must feed the clamped forward, not the full prior output.
    pub consumed_input: u128,
    /// Per-hop state nonce captured at solve time (the staleness gate).
    pub state_nonce: u64,
}

#[derive(Debug, Clone)]
pub struct DispatchCandidate {
    /// The candidate identity — the solver path bound to the deployment that
    /// will encode and submit it. Two deployments on one path are two
    /// candidates.
    pub candidate_id: CandidateId,
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
    /// `path_info` — the resolved path hops the deployment's Encode part encodes.
    pub path_info: PathInfo,
    /// The Encode decision a target-selection walk already took for this
    /// candidate ([`crate::selection::PreparedEncoding`]).
    ///
    /// `Some` means the deployment's `encode` callback has ALREADY run on this
    /// candidate's path, so simulation reuses that exact transaction instead of
    /// composing a second time. `None` is the explicit all-target fan-out's
    /// shape: the simulation leaf composes for its own deployment.
    pub prepared: Option<crate::selection::PreparedEncoding>,
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
    pub(crate) fn to_simulate_path(&self) -> SimulatePath {
        SimulatePath {
            candidate_id: self.candidate_id,
            optimal_input: self.optimal_input,
            steps: self.steps.clone(),
            path_info: self.path_info.clone(),
            solve_block: self.solve_block,
        }
    }
}

/// The dispatch fan-out outcome — the categorization + tallies.
///
/// The `[sim] ... by reason: {breakdown}` RENDERING belongs to the driver; this
/// struct exposes the typed buckets it renders.
#[derive(Debug, Default)]
pub struct DispatchOutcome {
    /// Submittable results: the deployment's assessment passed AND the net
    /// clears [`MIN_PROFIT_NET`] ([`SimResult::is_submittable`]), sorted by net
    /// profit descending.
    pub gas_profitable: Vec<SimResult>,
    /// Returned but not submittable, sorted by net profit descending: the
    /// gross-positive rows that either fell below the gas floor or whose
    /// deployment assessment declined them. A failed assessment stays visible
    /// here — the same "every gross-profitable path stays visible" policy —
    /// instead of vanishing at the sim seam.
    pub gas_unprofitable: Vec<SimResult>,
    /// The number of candidates that raised an exception during simulation.
    pub exception_count: usize,
    /// The number of candidates that returned `None` from `simulate_path_on_evm`
    /// (revert / no-profit / int128-overflow / etc.): the candidate count less
    /// the simulated-ok and exception tallies.
    pub fail_count: usize,
    /// The number of candidates passed to simulation (after the pre-filters
    /// + the `MAX_SIMULATE_CONCURRENT` cap).
    pub candidate_count: usize,
    /// The number of candidates dropped by [`PathSuppression::is_suppressed`].
    pub suppressed_count: usize,
    /// The number of candidates dropped by [`filter_thin_margin_results`].
    pub thin_dropped: usize,
    /// The number of candidates dropped pre-sim because every surviving
    /// hop routed through a pool flagged `SolverCalc` within the decay window.
    /// Mirrors `suppressed_count` / `thin_dropped` — a per-call
    /// pre-filter drop count; the lifetime tally lives on
    /// [`PoolDivergence::total_divergent_dropped`].
    pub divergent_dropped: usize,
    /// The number of candidates dropped pre-sim because any hop's input token
    /// is FoT-confirmed. Mirrors `divergent_dropped` — a
    /// per-call pre-filter drop count; the lifetime tally lives on
    /// [`FeeOnTransferRegistry::total_fot_dropped`].
    pub fot_dropped: usize,
    /// candidates dropped because a pool's state nonce advanced past
    /// the snapshot captured at solve time (the solver computed against
    /// state the pump has since superseded — simulating would revert).
    pub stale_dropped: usize,
    /// The revert/no-profit/overflow bucket tally accumulated across the
    /// fan-out. The aggregation rendering (`format_failure_breakdown`) belongs
    /// to the driver; this exposes the typed buckets it renders.
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
    /// The total `sim_ok_count` — `len(gas_profitable) + len(gas_unprofitable)`.
    #[must_use]
    pub fn sim_ok_count(&self) -> usize {
        self.gas_profitable.len() + self.gas_unprofitable.len()
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The fan-out
// ─────────────────────────────────────────────────────────────────────────

/// Fan out [`simulate_path_on_evm`] over the candidates the admission policy
/// admitted, gather with exception tolerance, and categorize into
/// gas-profitable / gas-unprofitable / exception.
///
/// This is the RAW standalone entry: it applies the whole admission pipeline,
/// in the standalone order, to whatever it is handed. A caller that has already
/// applied its own admission projection transports its survivors to the
/// pre-admitted entry ([`crate::dispatch_admitted_across_deployments`]) instead,
/// whose leaf finalizes them and reaches this same simulation body.
///
/// Pipeline:
/// 1. **Admission** — `admission::admit` runs the whole
///    pre-simulation policy in one pass: the stamping suppression read (the
///    retry-interval logic is owned by [`PathSuppression`]), pool divergence,
///    fee-on-transfer, the thin-margin floor, and candidate staleness, then the
///    optional order-index reorder and the cap. Its typed outcome carries the
///    per-stage drop counters this outcome mirrors.
/// 2. **Fan-out** — the in-process evm2 path: build ONE per-block
///    `Evm2BlockSimHandle` + simulate each candidate SERIALLY on the shared
///    `&mut evm` (Tier 1). `bot_state` is `Option` so that
///    empty / pre-filtered-to-empty input (which short-circuits after
///    admission) need not construct an engine; the `None` arm is the
///    unreachable anti-pattern guard for non-empty input without a `BotState`
///    (ADR-019 D1 makes the in-process evm2 path the sole executor).
/// 3. **Gather** — collect the stream; a failed candidate is counted and
///    skipped, never propagated: one broken path must not sink a batch.
/// 4. **Feedback — pool divergence** — for each `SolverCalc`
///    failure, record divergence for the diverging hop's pool key (derived
///    from `path_info.hops[i]`); the NEXT block's admission drops paths
///    through it pre-sim.
/// 5. **Categorize** — gas-profitable (the deployment's assessment passed and
///    `net ≥ MIN_PROFIT_NET`) / gas-unprofitable (returned, gross > 0, but
///    below the floor or declined by the deployment's verdict) / exception.
///    Sort both categories by net profit descending.
/// 6. **Record suppression outcomes** — `record_success(pid)` for paths that
///    returned a result, `record_failure(pid)` for paths that didn't.
///
/// `ctx` is the shared [`SimulateContext`] (provider, addresses, warmup,
/// block context — see [`simulate_path_on_evm`]). `path_suppression` is
/// mutated in place for the admission stage + the outcome recording.
///
/// # Categorization contract (`docs/architecture/parity-gate.md`)
///
/// The categorization buckets, the pre-filter drops and the suppression
/// transitions are the dispatch contract: a candidate that returns a result and
/// one that reverts are tallied apart, and a path's suppression state moves only
/// on those outcomes. The `[sim] N candidates: X ok (Y profitable, Z below
/// threshold), W failed, V exceptions` summary is the driver's to render; this
/// leaf returns the typed [`DispatchOutcome`].
///
/// # Failure model
///
/// Every per-path failure (revert / no-profit / rpc-failed / int128-overflow)
/// is tallied into `outcome.fail_buckets` + recorded as a per-candidate
/// [`SimFailure`] — no SIMULATION verdict propagates as an `Err`. The
/// `exception_count` field captures `simulate_path_on_evm` `Err` returns (the
/// rare defensive `?` on the execute() ABI encode), counted not propagated.
///
/// # Errors
///
/// [`DispatchRefusal::DuplicateCandidateId`] for an input set that carries one
/// [`CandidateId`] twice. The identity is a ROW identity (see
/// [`crate::identity`]), so the check runs FIRST — before the census instrument
/// and before any admission stage — and a refusal spends no suppression retry
/// stamp, no divergence/fee-on-transfer tally, and no simulation. Silently
/// keeping one of the two rows is never the answer: the caller would never
/// learn which of its candidates ran.
///
/// # Panics
///
/// Panics if the `path_suppression`, `pool_divergence`, or `fot_registry` mutex
/// is poisoned (a peer task panicked while holding it). Each is locked ONLY at
/// a synchronous bookend — the admission pass (`admission::admit`) and
/// the outcome recording / divergence / FoT feedback below — and no `.await` is
/// ever held under a guard, so a poison indicates a bug in a sibling task (the
/// arcs are shared with the submission seam's accessors; never locked across an
/// `.await`).
#[expect(clippy::too_many_arguments)] // faithful dispatch seam
pub fn dispatch_profitable_results(
    candidates: Vec<DispatchCandidate>,
    ctx: &SimulateContext<'_>,
    path_suppression: &Arc<Mutex<PathSuppression>>,
    current_block: u64,
    min_profit_margin_bps: u64,
    // The per-pool solver-divergence memo. Parallels
    // `path_suppression` — a standalone `Arc<Mutex<PoolDivergence>>` (NOT
    // composed into the `Dispatcher`) so the sim seam locks it directly at
    // the skip bookend (step 2) + the feedback (step 7), never across the
    // fan-out `.await`s. The skip drops candidates routing through a pool
    // flagged `SolverCalc` within the decay window; the feedback records
    // divergence for `SolverCalc` failures' diverging hops so the NEXT
    // block's skip drops them.
    pool_divergence: &Arc<Mutex<PoolDivergence>>,
    // The per-token fee-on-transfer registry. Parallels
    // `pool_divergence` — a standalone `Arc<Mutex<FeeOnTransferRegistry>>`
    // (NOT composed into the `Dispatcher`) so the sim seam locks it directly
    // at the skip bookend (step 2) + the feedback (step 7) + the success
    // recording (step 8), never across the fan-out `.await`s. The skip drops
    // candidates whose any hop's input token is FoT-confirmed; the feedback
    // records suspicions for FoT-suspected failures; the success recording
    // feeds the 0-success disambiguator (a token that ever succeeds is not
    // FoT — the fee always shorts the input).
    fot_registry: &Arc<Mutex<FeeOnTransferRegistry>>,
    // The fan-out routes each candidate through the in-process evm2 sim
    // over the borrowed `&BotState` via a per-block shared `Evm2BlockSimHandle`
    // (Tier 1), the sole executor (ADR-019 D1). The `Arc<RwLock<BotState>>`
    // is the engine's shared state owner (ADR-003); a per-block read guard is
    // taken for the serial loop. The sync DB bridge's `block_on` uses
    // `tokio::task::block_in_place` under a multi-threaded runtime, so the
    // in-process sim's blocking RPC cold-miss path does not deadlock against
    // the pump's worker pool. `Option` (not required) so that empty /
    // pre-filtered-to-empty input — which never reaches this param at all
    // (step 4's `is_empty()` short-circuit above) — need not wire an engine;
    // the `None` arm below is the unreachable guard for non-empty input
    // without a `BotState`.
    bot_state: Option<Arc<StateLock<BotState>>>,
    // The cross-block persistent bytecode + account-existence cache
    // (`WarmCodeCacheInner`, the Option-A layer). Required by the
    // `Evm2BlockSimHandle` build (the `None` arm is unreachable — see `bot_state`).
    // The `Arc` clones cheaply into this async fn's future; the engine owner
    // (`PyArbitrageEngine` / standalone `Bot`) holds it for the engine's
    // life. If `Some(bot_state)` arrives with `None` warm_cache (a caller
    // wiring gap), a fresh `WarmCodeCacheInner::shared_default()` is
    // constructed per call — a safe degradation to Tier-1-only behavior (no
    // cross-block persistence, no panic). The FFI seam sets both from the
    // same `engine` arg, so the gap is unreachable in production.
    warm_cache: Option<
        Arc<RwLock<degenbot_simulation::sim::evm2::warm_code_cache::WarmCodeCacheInner>>,
    >,
) -> Result<DispatchOutcome, DispatchRefusal> {
    // 0. Identity — one row per `CandidateId`, refused before any other stage.
    refuse_duplicate_candidate_ids(&candidates)?;

    let pre_filter_count = candidates.len();
    // T3: candidates entering the fan-out (pre-filter sizes summed; the
    // per-stage drop counts live on `DispatchOutcome` for the logs).
    degenbot_bot::instruments::record(degenbot_bot::instruments::CensusEvent::CandidatesEntered {
        n: u64::try_from(pre_filter_count).unwrap_or(u64::MAX),
        arm: "fanout",
    });

    // 1-4. Pre-simulation admission — the ONE home of the policy sequence:
    //      the stamping suppression read, pool divergence, fee-on-transfer, the
    //      thin-margin floor, and candidate staleness, then the optional
    //      order-index reorder and the `MAX_SIMULATE_CONCURRENT` cap. The pass
    //      holds each registry lock in a synchronous bookend scope (never
    //      across the fan-out `.await`, so the future stays `Send`), and its
    //      typed outcome carries the per-stage drop counters below.
    let admitted = admission::admit(
        candidates,
        &admission::AdmissionPolicy {
            suppression: path_suppression,
            divergence: pool_divergence,
            fee_on_transfer: fot_registry,
            bot_state: bot_state.as_deref(),
            current_block,
            min_profit_margin_bps,
        },
    );
    let outcome = leaf_outcome(admitted.skips, admitted.admitted.len());
    Ok(simulate_leaf(
        admitted.admitted,
        outcome,
        LeafInputs {
            ctx,
            path_suppression,
            current_block,
            pool_divergence,
            fot_registry,
            bot_state,
            warm_cache,
        },
    ))
}

/// Finalize PRE-ADMITTED candidates against the live policy state, then
/// simulate them — the pre-admitted fan-out's leaf
/// ([`crate::dispatch_admitted_across_deployments`]).
///
/// The caller's assembly already ran [`crate::project_early_admission`] — the
/// non-stamping half of this policy (divergence, fee-on-transfer, thin margin,
/// its per-lane cap) and the pre-selection staleness gate — and transported
/// every survivor as an [`AdmittedCandidate`]. This leaf's finalization is the
/// AUTHORITATIVE half of the same policy, in the fixed pre-admitted order:
///
/// `stamping suppression -> divergence -> fee-on-transfer -> staleness ->
/// optional order-index reorder -> same-cap truncation -> simulation`
///
/// Nothing here trusts the caller's admission: a ledger, memo, or registry that
/// moved between admission and this call drops the candidate with the matching
/// per-stage counter, exactly as [`dispatch_profitable_results`] would, and a
/// retry-due candidate is stamped even when a later final stage drops it. The
/// thin-margin floor has no stage here (it belongs to the caller's early
/// admission), so `thin_dropped` is always `0` and no defensive second pass is
/// invented for it.
///
/// Simulation, categorization, the divergence/FoT feedback, the suppression
/// outcome recording, and the sorting are the SAME body
/// [`dispatch_profitable_results`] runs — one simulation implementation, two
/// entry orders.
///
/// # Errors
///
/// [`DispatchRefusal::DuplicateCandidateId`] for a transported set that carries
/// one [`CandidateId`] twice, checked before finalization touches a ledger (see
/// [`crate::identity`]). The registry entry
/// ([`crate::dispatch_admitted_across_deployments`]) checks the same rule before
/// the target-selection walk composes anything; the check here is the invariant
/// of the simulation body itself, so no future caller can reach it with a set
/// the lanes could not tell apart.
///
/// # Panics
///
/// Panics if a policy mutex is poisoned — the same loud failure the raw
/// standalone leaf has.
#[expect(clippy::too_many_arguments)] // the leaf's shared simulation inputs
pub(crate) fn dispatch_admitted_profitable_results(
    admitted: Vec<AdmittedCandidate>,
    ctx: &SimulateContext<'_>,
    path_suppression: &Arc<Mutex<PathSuppression>>,
    current_block: u64,
    // The per-pool solver-divergence memo: re-read here, so a pool flagged
    // after admission drops its candidates before simulation.
    pool_divergence: &Arc<Mutex<PoolDivergence>>,
    // The per-token fee-on-transfer registry, re-read for the same reason.
    fot_registry: &Arc<Mutex<FeeOnTransferRegistry>>,
    // The engine's shared state owner — the staleness gate's final read.
    bot_state: Option<Arc<StateLock<BotState>>>,
    // The cross-block bytecode cache the shared simulation body builds its
    // handle with (see [`dispatch_profitable_results`]).
    warm_cache: Option<
        Arc<RwLock<degenbot_simulation::sim::evm2::warm_code_cache::WarmCodeCacheInner>>,
    >,
) -> Result<DispatchOutcome, DispatchRefusal> {
    // 0. Identity — one row per `CandidateId`, before finalization reads or
    //    stamps any registry.
    refuse_duplicate_candidate_ids(admitted.iter().map(AdmittedCandidate::candidate))?;

    let entering = admitted.len();
    // T3: the same census edge the raw leaf emits — the candidates THIS leaf
    // considered, so a migrated caller's metric keeps its meaning.
    degenbot_bot::instruments::record(degenbot_bot::instruments::CensusEvent::CandidatesEntered {
        n: u64::try_from(entering).unwrap_or(u64::MAX),
        arm: "fanout",
    });

    // 1-4. Finalization — the authoritative half of the policy, in the
    //      pre-admitted order. Every lock is a synchronous bookend scope, so no
    //      guard crosses the simulation body below.
    let finalized = admission::finalize(
        admitted,
        &admission::FinalizationPolicy {
            suppression: path_suppression,
            divergence: pool_divergence,
            fee_on_transfer: fot_registry,
            bot_state: bot_state.as_deref(),
            current_block,
        },
    );
    let outcome = leaf_outcome(finalized.skips, finalized.admitted.len());
    Ok(simulate_leaf(
        finalized.admitted,
        outcome,
        LeafInputs {
            ctx,
            path_suppression,
            current_block,
            pool_divergence,
            fot_registry,
            bot_state,
            warm_cache,
        },
    ))
}

/// The raw leaf over a candidate set a TEST fixture built, unwrapped: the test
/// suites assert on an outcome, and the duplicate-identity refusal has its own
/// tests, so a fixture that accidentally repeats an identity fails loudly here
/// instead of skewing an unrelated assertion. Test-only, so the public entry
/// keeps its `Result`.
#[cfg(test)]
#[expect(
    clippy::too_many_arguments,
    clippy::expect_used,
    reason = "the leaf's own signature, forwarded; a fixture that repeats an identity must fail loudly"
)]
fn dispatch_test_leaf(
    candidates: Vec<DispatchCandidate>,
    ctx: &SimulateContext<'_>,
    path_suppression: &Arc<Mutex<PathSuppression>>,
    current_block: u64,
    min_profit_margin_bps: u64,
    pool_divergence: &Arc<Mutex<PoolDivergence>>,
    fot_registry: &Arc<Mutex<FeeOnTransferRegistry>>,
    bot_state: Option<Arc<StateLock<BotState>>>,
    warm_cache: Option<
        Arc<RwLock<degenbot_simulation::sim::evm2::warm_code_cache::WarmCodeCacheInner>>,
    >,
) -> DispatchOutcome {
    dispatch_profitable_results(
        candidates,
        ctx,
        path_suppression,
        current_block,
        min_profit_margin_bps,
        pool_divergence,
        fot_registry,
        bot_state,
        warm_cache,
    )
    .expect("the fixture carries one row per candidate identity")
}

/// Refuse a candidate set that carries one [`CandidateId`] twice — the input
/// check both leaves run before any other stage, because every structure below
/// (the ledger, the divergence/FoT feedback's path join, the suppression outcome
/// recording, and the submit row) is identity-keyed and cannot tell two rows of
/// one identity apart (see [`crate::identity`]).
fn refuse_duplicate_candidate_ids<'a>(
    candidates: impl IntoIterator<Item = &'a DispatchCandidate>,
) -> Result<(), DispatchRefusal> {
    match duplicate_candidate_id(candidates.into_iter().map(|c| c.candidate_id)) {
        Some((candidate_id, occurrences)) => Err(DispatchRefusal::DuplicateCandidateId {
            candidate_id,
            occurrences,
        }),
        None => Ok(()),
    }
}

/// The empty dispatch outcome carrying one pass's per-stage drop counters and
/// the count of candidates that reached simulation — the same mapping for both
/// entries, so the two can never report a drop differently.
fn leaf_outcome(skips: admission::AdmissionSkips, candidate_count: usize) -> DispatchOutcome {
    DispatchOutcome {
        suppressed_count: skips.suppressed,
        thin_dropped: skips.thin_margin,
        divergent_dropped: skips.divergent,
        fot_dropped: skips.fee_on_transfer,
        stale_dropped: skips.stale,
        candidate_count,
        ..DispatchOutcome::default()
    }
}

/// The policy state, engine handles, and block facts ONE leaf simulates with.
///
/// Both entries build one after their own pass (admission for the raw
/// standalone leaf, finalization for the pre-admitted one), so the simulation,
/// categorization, feedback, recording, and sorting below have exactly one
/// implementation.
struct LeafInputs<'a> {
    /// The block-wide facts and the deployment this lane simulates against.
    ctx: &'a SimulateContext<'a>,
    /// The retry-interval ledger — the outcome-recording bookend's target.
    path_suppression: &'a Arc<Mutex<PathSuppression>>,
    /// The block this fan-out runs at.
    current_block: u64,
    /// The per-pool solver-divergence memo — the feedback bookend's target.
    pool_divergence: &'a Arc<Mutex<PoolDivergence>>,
    /// The per-token fee-on-transfer registry — the feedback + success
    /// recording bookend's target.
    fot_registry: &'a Arc<Mutex<FeeOnTransferRegistry>>,
    /// The engine's shared state owner (see [`dispatch_profitable_results`]).
    bot_state: Option<Arc<StateLock<BotState>>>,
    /// The cross-block bytecode cache (see [`dispatch_profitable_results`]).
    warm_cache:
        Option<Arc<RwLock<degenbot_simulation::sim::evm2::warm_code_cache::WarmCodeCacheInner>>>,
}

/// Simulate `candidates` with `inputs`, then categorize, feed back, record, and
/// sort them into `outcome`.
#[expect(
    clippy::too_many_lines,
    clippy::expect_used,
    reason = "the fan-out, feedback, and recording read best as one ordered pass"
)]
fn simulate_leaf(
    candidates: Vec<DispatchCandidate>,
    mut outcome: DispatchOutcome,
    inputs: LeafInputs<'_>,
) -> DispatchOutcome {
    let LeafInputs {
        ctx,
        path_suppression,
        current_block,
        pool_divergence,
        fot_registry,
        bot_state,
        warm_cache,
    } = inputs;

    // The per-candidate `PathInfo` join map: the post-fan-out feedback
    // (divergence + FoT attribution) and the success recording re-derive a
    // hop's key from it — the V4 `CapturedSwap.emitter` is the shared
    // PoolManager, useless for per-pool attribution. Only admitted candidates
    // can be simulated, so only they need an entry. Its key is the solver
    // path id, and one identity per row (enforced by both leaves' entry check)
    // means two rows can never overwrite each other's entry.
    let path_info_by_id: HashMap<u64, PathInfo> = candidates
        .iter()
        .map(|c| (c.candidate_id.path_id(), c.path_info.clone()))
        .collect();

    // Empty-input short-circuit: with no candidates to simulate, the outcome
    // (all counters already set above) is complete. This avoids requiring a
    // `BotState` for the trivial empty case (suppressed-to-empty / genuinely
    // empty input) — the in-process evm2 path never builds a handle
    // for zero candidates.
    if candidates.is_empty() {
        return outcome;
    }

    // 2. Fan-out — build ONE per-block `Evm2BlockSimHandle` + simulate each
    //    candidate SERIALLY on the shared `&mut evm` (Tier 1). The cap was
    //    applied by the admission pass; the in-process serial path uses a
    //    plain `.map().collect()` over the borrowed `&mut evm`. Unlike
    //    `tokio::spawn`, neither borrows `'static` — the closure borrows
    //    `ctx` for its lifetime, and we collect within this fn (no task
    //    outlives the call).
    // One id per simulated row: the entry check refused a repeated identity, so
    // the outcome recording below moves each candidate's ledger state at most
    // once per pass — a duplicated id could otherwise record both a success and
    // a failure against it, or count one failure twice toward suppression.
    let candidate_ids: Vec<CandidateId> = candidates.iter().map(|c| c.candidate_id).collect();
    // SIMPIPE T1 lab baseline: snapshot BEFORE the fan-out so the exit log
    // carries this fan-out's cold-fetch/build/sim deltas.
    let lab_before = degenbot_simulation::sim::sim_metrics::snapshot();
    let lab_fanout_started = std::time::Instant::now();
    let fanout_candidate_count = candidates.len();
    // ONE identifier per fan-out, minted here and echoed on every row this
    // fan-out emits: the storage selection, the prefetch batch, the `[sim]`
    // delta summary, and each candidate's `fanout-trace` row. That is what lets
    // a capture join four row families that fire at different seams — and what
    // keeps the inline hook re-quote arm's equally shaped trace rows
    // unattributed instead of borrowing this fan-out's id.
    let fanout_id = degenbot_simulation::sim::sim_metrics::next_fanout_id();
    let sim_results: Vec<(CandidateId, FailBuckets, Result<Option<SimResult>, String>)> =
        match bot_state {
            // In-process evm2 path (Tier 1): build ONE per-block EVM
            // (`Evm2BlockSimHandle`) and simulate each candidate SERIALLY on the
            // shared `&mut evm`. The shared `CacheDB` captures the ~50× latency
            // win (benchmark `examples/rpc_cache_fanout.rs` config B vs A): the
            // trigger path pays the cold RPCs, the fan-out hits the warmed cache
            // at ~1 µs p50. Serial (not `buffer_unordered`) because a shared
            // `&mut evm` can't be held across `'static`+`Send` futures — the
            // per-path RPC cold-miss dwarfs the per-path EVM execution, so
            // serial-warm beats parallel-cold. Per-path isolation is the engine's
            // `finalize()` (execute() SSTOREs live in the per-path `State`, never
            // committed to the shared `CacheDB`). `parking_lot`'s read guard is
            // held for the serial loop's duration.
            Some(arc) => {
                // Incident 2026-08-20 #1 root fix: snapshot the sim
                // anchor under a SHORT read and drop the guard BEFORE any
                // provider I/O. Pre-fix this read guard was held across
                // `Evm2BlockSimHandle::build` + the whole serial sim loop — every
                // cold-miss fetch parked a `BotState` writer behind it for the
                // transport delay (minutes on a stalled RPC pre-F2). The
                // snapshot is O(pools) scalar words; see
                // `bot_core::SimAnchorState` for the audited surface.
                //
                // ADR-039: since T5 the snapshot is an ENUMERATED
                // per-family projection (`project_sim_anchor_scalars`), never an
                // arbitrary-key probe — this guard scope paid a ~2.7s hold per
                // block through the V3 tick-descent + V4 O(V^2) keccak paths.
                // DO NOT re-point the projection at the arbitrary-key probe, and
                // do not extend this guard across anything below it.
                let anchor = {
                    let guard = arc.read_at(degenbot_substrate::state_lock::LockSite::Sim);
                    degenbot_substrate::SimAnchorState::snapshot(&guard)
                };
                // The engine `Evm2BlockSimHandle` build takes the block-env primitives
                // + the override params projected from this strategy's
                // `SimulateContext` (ADR-019 D7, decision R — the engine stays
                // generic over strategy config; it never names `SimulateContext`).
                // The shared-EVM sim anchor: the pump pre-promotes
                // `active_block = max(drain_block, pool_state_head)` and threads it
                // as each candidate's `solve_block`, so every candidate in the
                // batch carries the SAME promoted block (the pool-state head). This
                // `max` over `solve_block` is therefore a defensive no-op that
                // returns that shared promoted block — kept as an invariant
                // assertion, not a re-anchor. Simulating at the lagging Python
                // clock fetched PRE-update state (state-ahead-of-clock desync) and
                // mismatched the solver's head math; one
                // head-anchored shared EVM reproduces every candidate exactly.
                let sim_block = candidates
                    .iter()
                    .map(|c| c.solve_block)
                    .max()
                    .unwrap_or(ctx.block.current_block);
                // The engine fan-out (ADR-089): `Evm2BlockSimHandle` + the
                // seam drive every candidate.
                // The warm-code cache arc; degrade to a fresh per-call cache if
                // the caller wired `bot_state` without one (safe — no
                // cross-block persistence, no panic).
                let fanout = {
                    // Bound before the match: a scrutinee temporary would
                    // otherwise extend the anchor borrow past the arm.
                    let evm2_warm_cache = warm_cache.unwrap_or_else(
                    degenbot_simulation::sim::evm2::warm_code_cache::WarmCodeCacheInner::shared_default,
                );
                    // SIMPIPE2 M2, cross-fan-out: the storage memo is scoped to the
                    // read anchor (`sim_block` — the block the fallback tags every
                    // RPC read with), not to this fan-out. Every dispatch at this
                    // anchor shares one memo, so a block's fan-outs pay the cold
                    // chain once; the session-owned slot retires it (and every
                    // value it cached) the moment the anchor moves, incl. a reorg
                    // rewind. The degrade path above mints a per-call cache, hence
                    // a fresh slot — the old per-fan-out scope, unchanged.
                    //
                    // The anchor is a BLOCK, not a height: its hash rides the key
                    // (`ReadAnchor`), so a same-height reorg — the anchor number
                    // unchanged, the canonical block replaced — mints a fresh memo
                    // instead of serving the abandoned block's words. The identity
                    // is resolved HERE, one `eth_getBlockByNumber` per fan-out
                    // against the cold storage chain the memo saves on this
                    // anchor's later fan-outs, and BEFORE any guard is taken
                    // (never across provider I/O). A failed read degrades to the
                    // number-only anchor — the pre-fence scope, never a wrong one.
                    // The storage representation this fan-out reads through:
                    // the legacy per-anchor memo (the default) or a ready BAL
                    // delta generation. The selection resolves the anchor either
                    // way — one identity read with the opt-in OFF, one whole
                    // (number, hash) header read with it ON — and, ON, only
                    // ASKS what this exact anchor answers: the session's
                    // canonical-head pipeline delivered, prepared and applied
                    // every accepted head off this path, so a fan-out generates
                    // no BAL work. A fan-out at the exact head whose
                    // preparation is in flight JOINS that preparation and
                    // waits for its terminal result; every real terminal cold
                    // outcome — an abandoned or unclaimed preparation
                    // included — reports Legacy with its reason, so the
                    // fallback is the existing memo + prefetch lane and no
                    // parent value is served on it.
                    // The selection is driven to completion here — the ONE
                    // future this fan-out awaits before the storage view is
                    // constructed. Only the exact current head with a live
                    // preparation waits inside it, on that head's own terminal
                    // completion; every other case resolves on the synchronous
                    // view.
                    let selection = degenbot_simulation::sim::prefetch::block_on_ambient(
                        degenbot_simulation::sim::evm2::warm_code_cache::select_dispatch_storage(
                            ctx.block.provider,
                            &evm2_warm_cache,
                            sim_block,
                        ),
                    )
                    .unwrap_or_else(|| {
                        // No ambient runtime could drive the selection: the
                        // same degrade the anchor read has — the number-only
                        // anchor, and the honest label.
                        degenbot_simulation::sim::evm2::warm_code_cache::DispatchStorageSelection::degraded(
                            evm2_warm_cache.read().delta_dispatch().is_none(),
                            sim_block,
                        )
                    });
                    diag!(
                        domain = sim,
                        current_block,
                        anchor = sim_block,
                        anchor_fenced = selection.anchor.hash.is_some(),
                        storage = selection.outcome.mode(),
                        reason = ?selection.outcome.reason(),
                        wait_us = u64::try_from(selection.wait.as_micros()).unwrap_or(u64::MAX),
                        fanout = fanout_id,
                        "fan-out storage selection"
                    );
                    degenbot_bot::instruments::record(
                        degenbot_bot::instruments::CensusEvent::DispatchStorage {
                            mode: selection.outcome.mode(),
                            reason: selection.outcome.reason(),
                        },
                    );
                    let read_anchor = selection.anchor;
                    // The per-anchor memo, the session's cross-block hot-slot
                    // store, and the durable per-hook slot-set store are all owned
                    // by the warm-cache owner; taken together under one short read
                    // guard so none of them rides it across provider I/O.
                    let (storage_memo, hot_slots, hook_slots) = {
                        let cache = evm2_warm_cache.read();
                        (
                            cache.storage_memo_for(read_anchor),
                            cache.hot_slots(),
                            cache.hook_slots(),
                        )
                    };
                    // Path-derived prefetch: the candidates name their pools before
                    // the sim runs, so the slots the frame is guaranteed to read
                    // (V2 token identities + reserves, V3 slot0/liquidity, V4 pool
                    // state words, the executor's fixed slots) are computable HERE
                    // and readable in ONE batch round trip at the fallback's own
                    // anchor. Seeded into the anchor's memo BEFORE the first
                    // candidate, the frame's serial cold chain finds the entries
                    // memoized instead of paying one round trip per slot.
                    //
                    // The lane never fails the fan-out: no ambient runtime or a
                    // failed batch leaves the memo as it was, and the frame runs the
                    // un-prefetched path (see `sim::prefetch`'s degrade contract).
                    // A warm memo — every derived entry already known — dispatches
                    // nothing, so the anchor's second fan-out pays no batch.
                    //
                    // The batch is the DERIVATION PLUS the capture-learned
                    // complement (`prewarm_plan`): the slots this session's earlier
                    // captures touched under the batch's own named addresses
                    // (arbitrary ERC20 `balanceOf` bases, per-pool drift words) ride
                    // the SAME batch — never a second round trip — and their values
                    // are read from the node at this anchor like every other member.
                    // The learning side is the handle's own: `session()` hands each
                    // candidate the store, so a completed capture's access list
                    // teaches the next anchor's prewarm.
                    //
                    // The learned HOOK sets ride the same batch: a hooked V4 pool's
                    // hook storage has no derivation (arbitrary bytecode owns it),
                    // so the black-box quoter learns each hook's touched-slot set
                    // once (`sim::evm2::hook_slots`) and the plan joins it at the
                    // hook's own address. A session without the store — or a hook no
                    // quote has learned yet — joins nothing, which is the cold start
                    // this lane pays down.
                    // A ready delta generation answers candidate storage through
                    // its own generation, so this fan-out pays NO memo prefetch.
                    let prefetch =
                        if selection.is_delta() {
                            None
                        } else {
                            Some(degenbot_simulation::sim::prefetch::prefetch_paths_into_memo(
                        ctx.block.provider,
                        sim_block,
                        candidates.iter().map(|c| &c.path_info),
                        &degenbot_simulation::sim::prefetch::FamilySlotLayout::PRODUCTION,
                        // The deployment declares its own settlement identities (the
                        // built-in executor's WETH + PoolManager); a foreign
                        // deployment declares none and derives only path words.
                        &degenbot_simulation::sim::prefetch::PrefetchFacts {
                            settlement: ctx.deployment.settlement(),
                        },
                        degenbot_simulation::sim::LearnedSets {
                            hot: &hot_slots,
                            hooks: hook_slots.as_deref(),
                        },
                        &storage_memo,
                        ))
                        };
                    if let Some(prefetch) = prefetch.as_ref() {
                        diag!(
                            domain = sim,
                            current_block,
                            anchor = sim_block,
                            anchor_fenced = read_anchor.hash.is_some(),
                            derived = prefetch.derived,
                            learned = prefetch.learned,
                            hook = prefetch.hook,
                            requested = prefetch.requested,
                            seeded = prefetch.seeded,
                            skipped = ?prefetch.skipped,
                            fanout = fanout_id,
                            "storage prefetch"
                        );
                        // The diag above is debug-level and invisible at the default
                        // log level; the same report lands on the metrics surface so a
                        // cold lane, a warm lane and a degraded lane are distinguishable
                        // from a scrape.
                        degenbot_bot::instruments::record(
                            degenbot_bot::instruments::CensusEvent::StoragePrefetch {
                                derived: u64::try_from(prefetch.derived).unwrap_or(u64::MAX),
                                learned: u64::try_from(prefetch.learned).unwrap_or(u64::MAX),
                                requested: u64::try_from(prefetch.requested).unwrap_or(u64::MAX),
                                seeded: u64::try_from(prefetch.seeded).unwrap_or(u64::MAX),
                                skipped: prefetch
                                    .skipped
                                    .map(degenbot_simulation::sim::prefetch::PrefetchSkip::label),
                            },
                        );
                    }
                    // The view the handle reads candidates through: the ready
                    // delta generation, or the per-anchor memo this lane seeded.
                    let storage_view: Option<
                        Arc<dyn degenbot_simulation::sim::evm2::storage_view::StorageView + '_>,
                    > = match selection.storage {
                        degenbot_simulation::sim::evm2::warm_code_cache::FanOutStorage::Delta(
                            hub,
                        ) => Some(hub),
                        degenbot_simulation::sim::evm2::warm_code_cache::FanOutStorage::Legacy => {
                            let view: Arc<
                                dyn degenbot_simulation::sim::evm2::storage_view::StorageView + '_,
                            > = storage_memo;
                            Some(view)
                        }
                    };
                    match degenbot_simulation::sim::evm2::handle::Evm2BlockSimHandle::build_with_view(
                        ctx.block.provider,
                        ctx.block.base_fee_next,
                        sim_block,
                        ctx.block.block_timestamp,
                        &ctx.deployment.override_params(),
                        &anchor,
                        &evm2_warm_cache,
                        storage_view,
                        false,
                        None,
                        degenbot_simulation::sim::evm2::jit::shared_runner(),
                    ) {
                        Some(mut handle) => candidates
                            .into_iter()
                            .map(|c| {
                                let cid = c.candidate_id;
                                let sim_path = c.to_simulate_path();
                                v2_calc_trace(&mut handle, &sim_path);
                                let mut buckets = FailBuckets::new();
                                let mut session = handle.session();
                                // A target the selection walk already composed
                                // hands its accepted transaction down, so the
                                // adapter's callback never runs twice.
                                let prepared = c.prepared.as_ref();
                                let result = simulate_path_on_evm_prepared(
                                    &mut session,
                                    ctx,
                                    &sim_path,
                                    prepared,
                                    &mut buckets,
                                    Some(fanout_id),
                                )
                                .map_err(|e| format!("{e}"));
                                (cid, buckets, result)
                            })
                            .collect(),
                        None => candidates
                            .into_iter()
                            .map(|c| {
                                let mut buckets = FailBuckets::new();
                                buckets.record(
                                    c.candidate_id,
                                    "rpc-failed",
                                    None,
                                    alloy::primitives::Bytes::new(),
                                    c.optimal_input,
                                    c.expected_outputs_vec(),
                                );
                                (c.candidate_id, buckets, Ok(None))
                            })
                            .collect(),
                    }
                };
                fanout
            }
            // ADR-019 D1 — the in-process evm2 path (the `Some` arm above) is
            // the sole executor. This arm is unreachable in production: the FFI
            // seam always sources a `BotState` from the engine, and empty /
            // pre-filtered-to-empty input short-circuits at step 4 above before
            // the `match` is reached. The `Option` (rather than a required arg)
            // is preserved so that empty-input callers (the seam's offline
            // empty-candidate tests) need not construct an engine.
            None => {
                unreachable!(
                "dispatch_profitable_results: non-empty input requires a BotState (ADR-019 D1 makes the in-process evm2 path the sole executor)"
            )
            }
        };
    // SIMPIPE T1 lab: this fan-out's delta — the per-dispatch cold-fetch and
    // handle-build profile that decides the M1 handle-pool default.
    {
        let lab = degenbot_simulation::sim::sim_metrics::delta(
            degenbot_simulation::sim::sim_metrics::snapshot(),
            lab_before,
        );
        op_info!(
            domain = sim,
            current_block,
            fanout_ms = u64::try_from(lab_fanout_started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "{}",
            degenbot_simulation::sim::sim_metrics::format_delta(
                &lab,
                fanout_candidate_count,
                fanout_id
            )
        );
    }

    // 3. Categorize — gas-profitable / gas-unprofitable / exception.
    //    `simulate_path_on_evm` returns `Ok(Some(result))` for gross-profitable
    //    paths; `Ok(None)` for revert/no-profit/overflow (tallied into
    //    `buckets`); `Err(_)` for an unrecoverable RPC failure (counted as an
    //    exception — one broken path must not sink a batch).
    let mut succeeded_candidate_ids: HashSet<CandidateId> = HashSet::new();
    for (cid, buckets, result) in sim_results {
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
                succeeded_candidate_ids.insert(cid);
                if r.is_submittable() {
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

    // `fail_count = len(candidates) - sim_ok_count - exception_count`.
    outcome.fail_count = outcome.candidate_count - outcome.sim_ok_count() - outcome.exception_count;

    // 4. Feedback — record divergence for `SolverCalc` failures' diverging
    //    hops. A pool flagged this block stays divergent for
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
            if let Some(path_info) = path_info_by_id.get(&f.candidate_id.path_id()) {
                for key in diverging_pool_keys(f, &path_info.hops) {
                    pd.record_divergence(key, current_block);
                }
            }
        }
    }

    // 4.5. FoT feedback — record suspicions for FoT-suspected failures.
    //      The `fot_suspected_token` leaf returns `(token, pool_key)` for
    //      failures whose `reverting_frame.label` is in
    //      `FOT_REVERT_LABELS` (`IIA`, `CurrencyNotSettled`, `UniswapV2: K`).
    //      The pool_key is the hop's `PoolDivergenceKey` (V2/V3 address, V4
    //      `poolId`); the registry tracks the distinct
    //      failing pool identities per token + the 0-success flag; the skip
    //      (step 2.5) drops paths whose any hop's input token is FoT-confirmed.
    //      Same standalone-arc discipline.
    {
        let mut fr = fot_registry.lock().expect("fot_registry mutex poisoned");
        for f in &outcome.failures {
            if let Some(path_info) = path_info_by_id.get(&f.candidate_id.path_id()) {
                if let Some((token, pool_key)) = fot_suspected_token(f, &path_info.hops) {
                    fr.record_suspicion(token, pool_key, current_block);
                }
            }
        }
    }

    // 5. Record suppression outcomes:
    //    `record_success(pid)` for succeeded, `record_failure(pid)` for the
    //    rest (revert/no-profit/overflow/exception). Scope-locked (same
    //    bookend-only discipline as step 1 — the guard drops before any
    //    `.await`; there is no `.await` in this synchronous loop anyway).
    {
        let mut s = path_suppression.lock().expect("suppression mutex poisoned");
        for &cid in &candidate_ids {
            if succeeded_candidate_ids.contains(&cid) {
                s.record_success(cid);
            } else {
                s.record_failure(cid);
            }
        }
    }

    // 5.5. FoT success recording. For every SUCCEEDED path,
    //      record `record_success(token)` for EVERY token on the path — each
    //      hop's input AND output (via `hop_input_token` + `hop_output_token`)
    //      the 0-success disambiguator. A true FoT token can never succeed
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
    if !succeeded_candidate_ids.is_empty() {
        let mut fr = fot_registry.lock().expect("fot_registry mutex poisoned");
        for &cid in &candidate_ids {
            if succeeded_candidate_ids.contains(&cid) {
                if let Some(path_info) = path_info_by_id.get(&cid.path_id()) {
                    for hop in &path_info.hops {
                        fr.record_success(hop_input_token(hop), current_block);
                        fr.record_success(hop_output_token(hop), current_block);
                    }
                }
            }
        }
    }

    // 6. Sort both categories by net profit descending.
    outcome
        .gas_profitable
        .sort_by_key(|r| std::cmp::Reverse(r.net_profit));
    outcome
        .gas_unprofitable
        .sort_by_key(|r| std::cmp::Reverse(r.net_profit));

    // Emit the env-gated `[sim-divergence] summary` for this block's dispatch
    // fan-out — the tally of engine-vs-RPC
    // tracked-slot comparisons run inside `BotStateDb::storage_ref` during
    // the sim fan-out above. No-op when the probe is off; when on, logs
    // `slots_compared/divergent_slots/divergent_pairs/divergent_pools`
    // alongside the driver's `[sim]` per-block line. The divergence lines
    // themselves (`[sim-divergence] pool=..`) emit per-mismatch during the
    // fan-out; this is the per-block tally rollup.
    degenbot_simulation::divergence_probe::dump_divergence_summary();

    outcome
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
    use degenbot_execution::{DeploymentId, HopInfo, PathInfo, V2HopInfo};
    use degenbot_executor::composers::EncodeOptions;
    use degenbot_rpc::provider::AlloyProvider;

    use crate::cmd_executor::CmdExecutorBinding;
    use crate::deployment::{SimulationBlock, SimulationDeployment};
    use std::sync::{Arc, Mutex};

    const OWNER: Address = address!("9c56a29c7231974c269e24f9fb3c29203039089e");
    const EXECUTOR: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    const PM: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");

    fn mock_provider(asserter: &Asserter) -> AlloyProvider {
        let client = ClientBuilder::default().transport(MockTransport::new(asserter.clone()), true);
        let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
        AlloyProvider::from_provider(
            Arc::new(dyn_provider) as Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>
        )
    }

    /// The built-in `cmd_executor` binding every fan-out test injects.
    fn binding() -> CmdExecutorBinding {
        CmdExecutorBinding::new(
            OWNER,
            EXECUTOR,
            WETH,
            PM,
            EncodeOptions {
                erc6909_profit: false,
                use_v4_batch: false,
                ..EncodeOptions::default()
            },
        )
    }

    /// The fan-out tests' one deployment identity.
    fn deployment_id() -> DeploymentId {
        DeploymentId::new(1)
    }

    /// The built-in deployment with `runtime` installed at the executor address
    /// (the tests' declared code override).
    #[expect(clippy::expect_used)] // the declared override names the executor
    pub(super) fn deployment(runtime: Bytes) -> SimulationDeployment {
        binding()
            .injected(deployment_id(), EXECUTOR, runtime)
            .expect("the declared override names the executor")
    }

    pub(super) fn ctx<'a>(
        provider: &'a AlloyProvider,
        deployment: &'a SimulationDeployment,
    ) -> SimulateContext<'a> {
        SimulateContext {
            block: SimulationBlock {
                provider,
                base_fee_next: 1_000_000_000u128, // 1 gwei
                current_block: 100,
                block_timestamp: 0,
                block_priority_fees: Some(BlockPriorityFees {
                    block: 100,
                    p10: U256::from(500_000_000u64),   // 0.5 gwei
                    p50: U256::from(2_000_000_000u64), // 2 gwei
                }),
            },
            deployment,
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
            candidate_id: CandidateId::new(path_id, deployment_id()),
            optimal_input: 1_000_000_000_000_000_000u128,
            engine_profit: 1_000,
            steps: Box::new([SolveStep {
                output: 1_100_000_000_000_000_000u128,
                consumed_input: 1_100_000_000_000_000_000u128,
                state_nonce: 0,
            }]),
            solve_block: 100,
            path_info: single_v2_hop(pool),
            prepared: None,
        }
    }

    pub(super) fn candidate(path_id: u64, opt_input: u128, profit: u128) -> DispatchCandidate {
        DispatchCandidate {
            candidate_id: CandidateId::new(path_id, deployment_id()),
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
            prepared: None,
        }
    }

    /// Build a 7-call simulated block where the balances yield a known gross.
    fn access_list_response() -> serde_json::Value {
        serde_json::json!({"accessList": [], "gasUsed": "0x0"})
    }

    /// Push the 2 canned responses (access-list + simulate) for a profitable
    /// path into the asserter.
    // ── D2: PathSuppression consumption (done-reference) ──────────────────

    // ── the production axis chain (kwarg → intake → config) ────────

    #[test]
    #[expect(clippy::expect_used)] // in-range axes; a fail-closed panic is the test contract
    fn erc6909_option_reaches_the_binding_and_mode2_config() {
        // The operator's `erc6909_profit` flag must survive the production
        // projection — the injected deployment binding's encode policy is the
        // ONE authority (a candidate carries no second copy) — resolving
        // through `resolve_axes` to the `Erc6909` capture axis and packing
        // `profit_mode=2` (the on-chain ERC6909 WETH floor the profit assert
        // runs under).
        let opts = EncodeOptions {
            erc6909_profit: true,
            ..EncodeOptions::default()
        };
        let adapter = CmdExecutorBinding::new(OWNER, EXECUTOR, WETH, PM, opts).adapter();
        assert!(
            adapter.binding().opts().erc6909_profit,
            "the binding carries the operator's erc6909 axis"
        );
        // → resolve_axes: the legacy bool forces the Erc6909 capture axis.
        let (_, capture, _) = degenbot_executor::composers::resolve_axes(opts);
        assert!(
            matches!(
                capture,
                degenbot_executor::grammar_ledger::ProfitCapture::Erc6909
            ),
            "erc6909_profit must resolve to the Erc6909 capture axis"
        );
        // → the axis-aware packed config: profit_mode=2 (Erc6909 WETH assert).
        let cfg = degenbot_executor::composers::config_for_options(opts, U256::ZERO)
            .expect("axes in range");
        assert_eq!(
            cfg & U256::from(255u64),
            U256::from(2u64),
            "profit_mode must be 2 for Erc6909 capture"
        );
    }

    #[test]
    #[expect(clippy::expect_used)] // in-range axes; a fail-closed panic is the test contract
    fn custody_defaults_to_mode1_assert() {
        // Control: the default (no operator toggle) runs the ACTIVE
        // WETH+ETH profit assert, not the old profit_mode=0 fast path.
        let opts = EncodeOptions::default();
        let cfg = degenbot_executor::composers::config_for_options(opts, U256::ZERO)
            .expect("axes in range");
        assert_eq!(
            cfg & U256::from(255u64),
            U256::from(1u64),
            "default capture must pack profit_mode=1 (active assert)"
        );
    }

    /// Per-candidate failure attribution keeps the deployment half: two
    /// deployments failing the same path are two `[sim-fail]` records, not one
    /// overwritten row.
    #[test]
    fn failure_records_keep_same_path_candidates_apart() {
        let mut buckets = FailBuckets::new();
        for deployment in [1_u64, 2] {
            buckets.record(
                CandidateId::new(7, DeploymentId::new(deployment)),
                "rpc-failed",
                None,
                alloy::primitives::Bytes::new(),
                1_000,
                vec![1_100],
            );
        }

        let failures = buckets.into_failures();
        assert_eq!(failures.len(), 2, "one record per candidate");
        let ids: HashSet<CandidateId> = failures.iter().map(|f| f.candidate_id).collect();
        assert_eq!(ids.len(), 2, "the records are distinct identities");
        assert!(ids.contains(&CandidateId::new(7, DeploymentId::new(1))));
        assert!(ids.contains(&CandidateId::new(7, DeploymentId::new(2))));
    }

    /// The dispatch floor has one home: `is_gas_profitable` is the single
    /// comparison both dispatch arms use, and it flips exactly at
    /// `MIN_PROFIT_NET`. Zero net is unprofitable; one wei (the constant) is
    /// profitable.
    #[test]
    fn gas_profitability_boundary_is_the_core_floor() {
        assert!(!is_gas_profitable(U256::ZERO));
        assert!(!is_gas_profitable(U256::from(MIN_PROFIT_NET - 1)));
        assert!(is_gas_profitable(U256::from(MIN_PROFIT_NET)));
        assert!(is_gas_profitable(U256::from(MIN_PROFIT_NET) + U256::ONE));
    }

    // ── Tier 1: in-process serial branch ─────────────────────

    // ── no BotState guard across provider I/O ──────────────────

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

    /// Incident 2026-08-20 #1 root fix: the fan-out must NOT hold
    /// the `BotState` read guard across provider I/O. Pre-fix,
    /// the per-block handle build + the serial sim loop borrowed the guard
    /// while every cold-miss fetch went over RPC — a writer parked for the
    /// full transport delay (minutes on a stalled RPC pre-F2). Post-fix the
    /// anchor is snapshotted under a SHORT read and the writer proceeds
    /// while the sim is mid-RPC.
    #[test]
    #[expect(clippy::expect_used)]
    fn fanout_does_not_hold_the_botstate_guard_across_provider_io() {
        use alloy::transports::Transport;
        // `BlockingAsyncDb::new` needs an ambient multi-threaded runtime; the
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
            let deployment = deployment(Bytes::from_static(&[0xfe]));
            rt.block_on(async {
                dispatch_test_leaf(
                    cands,
                    &ctx(&provider, &deployment),
                    &suppression,
                    100,
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
            let _guard = writer_state.write_at(degenbot_substrate::state_lock::LockSite::Sim); // parks behind the fan-out's read (pre-fix)
            tx.send(start.elapsed()).expect("report wait");
        });

        let waited = rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("writer completes");
        assert!(
            waited < std::time::Duration::from_millis(700),
            "BotState writer waited {waited:?} — the fan-out holds the read \
             guard across provider I/O"
        );

        let outcome = sim_thread.join().expect("fan-out thread");
        // The sim itself still fails cleanly (delayed-erroring transport):
        // every candidate tallies rpc-failed, no exceptions.
        assert_eq!(outcome.exception_count, 0);
        writer.join().expect("writer thread");
    }

    /// A method-keyed scripted transport serving empty accounts (zero
    /// balance, zero nonce, no code, zero storage) — enough for the
    /// INVALID-executor fixture, whose execute() reverts before any pool
    /// read. Serves either read pattern (an apply that pre-fetches WETH/PM
    /// accounts; the evm2 apply does not).
    #[expect(clippy::expect_used)] // test-only scripted transport (static values always encode)
    mod empty_transport {
        use super::*;
        use alloy::providers::RootProvider;
        use alloy::rpc::client::RpcClient;
        use alloy::rpc::json_rpc::{
            ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload,
            SerializedRequest,
        };
        use alloy::transports::{TransportError, TransportFut};
        use serde_json::value::{to_raw_value, RawValue};

        pub fn provider() -> AlloyProvider {
            let client = RpcClient::new(EmptyTransport, true);
            let root: RootProvider = RootProvider::new(client);
            AlloyProvider::from_provider(
                Arc::new(root) as Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>
            )
        }

        #[derive(Clone, Copy)]
        struct EmptyTransport;

        fn success(value: &str) -> ResponsePayload {
            let raw: Box<RawValue> =
                to_raw_value(&serde_json::Value::String(value.into())).expect("valid JSON string");
            ResponsePayload::Success(raw)
        }

        impl tower::Service<RequestPacket> for EmptyTransport {
            type Response = ResponsePacket;
            type Error = TransportError;
            type Future = TransportFut<'static>;

            fn poll_ready(
                &mut self,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Ready(Ok(()))
            }

            fn call(&mut self, req: RequestPacket) -> Self::Future {
                Box::pin(async move {
                    let respond = |req: &SerializedRequest| -> Response {
                        let payload = match req.method() {
                            "eth_getBalance" | "eth_getTransactionCount" => success("0x0"),
                            "eth_getCode" => success("0x"),
                            "eth_getStorageAt" => success(&format!("0x{:064x}", 0)),
                            other => {
                                return Response {
                                    id: req.id().clone(),
                                    payload: ResponsePayload::Failure(ErrorPayload {
                                        code: -32601,
                                        message: format!("method not found: {other}").into(),
                                        data: None,
                                    }),
                                };
                            }
                        };
                        Response {
                            id: req.id().clone(),
                            payload,
                        }
                    };
                    Ok(match req {
                        RequestPacket::Single(r) => ResponsePacket::Single(respond(&r)),
                        RequestPacket::Batch(rs) => {
                            ResponsePacket::Batch(rs.iter().map(respond).collect())
                        }
                    })
                })
            }
        }
    }

    /// A scripted transport that serves empty accounts/storage (like
    /// [`empty_transport`]) AND records the storage **batch** packets it is
    /// dispatched — the prefetch lane's observable wire signature. The sim's own
    /// slot reads arrive as single requests, so a batch of `eth_getStorageAt`
    /// members is the lane and nothing else.
    #[expect(clippy::expect_used)] // test-only scripted transport
    mod prefetch_transport {
        use super::*;
        use alloy::primitives::B256;
        use alloy::providers::RootProvider;
        use alloy::rpc::client::RpcClient;
        use alloy::rpc::json_rpc::{
            ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload,
            SerializedRequest,
        };
        use alloy::transports::{TransportError, TransportFut};
        use serde_json::value::{to_raw_value, RawValue};
        use std::sync::atomic::{AtomicU64, Ordering};

        /// The lane's wire signature: dispatched storage-batch packets and the
        /// total members they carried, plus the chain's current canonical block
        /// hash and the identity reads that observed it.
        #[derive(Clone, Default)]
        pub struct Recorder {
            pub storage_batches: Arc<AtomicU64>,
            pub storage_members: Arc<AtomicU64>,
            block_reads: Arc<AtomicU64>,
            head_hash: Arc<Mutex<B256>>,
        }

        impl Recorder {
            pub fn storage_batches(&self) -> u64 {
                self.storage_batches.load(Ordering::Relaxed)
            }

            pub fn storage_members(&self) -> u64 {
                self.storage_members.load(Ordering::Relaxed)
            }

            /// The anchor-identity reads the fan-outs dispatched
            /// (`eth_getBlockByNumber`).
            pub fn block_reads(&self) -> u64 {
                self.block_reads.load(Ordering::Relaxed)
            }

            /// Rewrite history at the current height: the same block number, a
            /// different canonical block.
            pub fn reorg_at_same_height(&self, hash: B256) {
                *self.head_hash.lock().expect("recorder mutex") = hash;
            }

            fn head_hash(&self) -> B256 {
                *self.head_hash.lock().expect("recorder mutex")
            }
        }

        #[derive(Clone)]
        struct RecordingTransport {
            recorder: Recorder,
        }

        pub fn provider(recorder: &Recorder) -> AlloyProvider {
            let client = RpcClient::new(
                RecordingTransport {
                    recorder: recorder.clone(),
                },
                true,
            );
            let root: RootProvider = RootProvider::new(client);
            AlloyProvider::from_provider(
                Arc::new(root) as Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>
            )
        }

        fn success(value: &str) -> ResponsePayload {
            let raw: Box<RawValue> =
                to_raw_value(&serde_json::Value::String(value.into())).expect("valid JSON string");
            ResponsePayload::Success(raw)
        }

        fn success_json(value: &serde_json::Value) -> ResponsePayload {
            let raw: Box<RawValue> = to_raw_value(value).expect("valid JSON value");
            ResponsePayload::Success(raw)
        }

        impl tower::Service<RequestPacket> for RecordingTransport {
            type Response = ResponsePacket;
            type Error = TransportError;
            type Future = TransportFut<'static>;

            fn poll_ready(
                &mut self,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Ready(Ok(()))
            }

            fn call(&mut self, req: RequestPacket) -> Self::Future {
                let recorder = self.recorder.clone();
                Box::pin(async move {
                    let respond = |req: &SerializedRequest| -> Response {
                        let payload = match req.method() {
                            "eth_getBalance" | "eth_getTransactionCount" => success("0x0"),
                            "eth_getCode" => success("0x"),
                            "eth_getStorageAt" => success(&format!("0x{:064x}", 0)),
                            // The anchor's IDENTITY: this chain's current
                            // canonical block hash (the reorg test switches it).
                            "eth_getBlockByNumber" => {
                                recorder.block_reads.fetch_add(1, Ordering::Relaxed);
                                success_json(&serde_json::json!({
                                    "hash": recorder.head_hash()
                                }))
                            }
                            other => {
                                return Response {
                                    id: req.id().clone(),
                                    payload: ResponsePayload::Failure(ErrorPayload {
                                        code: -32601,
                                        message: format!("method not found: {other}").into(),
                                        data: None,
                                    }),
                                };
                            }
                        };
                        Response {
                            id: req.id().clone(),
                            payload,
                        }
                    };
                    Ok(match req {
                        RequestPacket::Single(r) => ResponsePacket::Single(respond(&r)),
                        RequestPacket::Batch(rs) => {
                            if rs.iter().all(|r| r.method() == "eth_getStorageAt") {
                                recorder.storage_batches.fetch_add(1, Ordering::Relaxed);
                                recorder
                                    .storage_members
                                    .fetch_add(rs.len() as u64, Ordering::Relaxed);
                            }
                            ResponsePacket::Batch(rs.iter().map(respond).collect())
                        }
                    })
                })
            }
        }
    }

    /// The fan-out's prefetch lane, observed on the wire: ONE storage batch at
    /// the fan-out's anchor carrying the batch's derivable slot set, and no
    /// second batch when the next fan-out at the SAME anchor finds the memo
    /// warm. The two fan-outs' outcomes are identical — the lane changes the
    /// reads, never the verdict.
    #[expect(clippy::expect_used)] // test-only: runtime build + thread join
    #[expect(clippy::too_many_lines)] // the three fan-outs' wire observations read top-down
    #[test]
    fn the_fanout_prefetches_the_anchors_slot_set_once() {
        use prefetch_transport::Recorder;

        let recorder = Recorder::default();
        let provider = prefetch_transport::provider(&recorder);
        let suppression = Arc::new(Mutex::new(PathSuppression::new()));
        let pool_divergence = Arc::new(Mutex::new(crate::PoolDivergence::new()));
        let fot_registry = Arc::new(Mutex::new(crate::FeeOnTransferRegistry::new()));
        let bot_state = Arc::new(StateLock::new(BotState::new()));
        let warm = degenbot_simulation::WarmCodeCacheInner::shared_default();

        // Prime the session's capture-learned hot-slot store the way a
        // completed capture at the previous anchor would: a drift word the path
        // cannot derive, under a pool the batch's paths NAME. It must ride the
        // SAME batch as the derivation — never a second round trip — and a
        // learned word under an address no path names must stay out.
        let learned_pool: Address = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let drift_slot = U256::from(0x1234u64);
        let stranger = Address::new([0x5e; 20]);
        {
            let cache = warm.read();
            let hot = cache.hot_slots();
            hot.record(100, learned_pool, drift_slot);
            hot.record(100, stranger, U256::from(0x77u64));
        }

        let cands = vec![
            candidate(70, 1_000_000_000_000_000_000u128, 1_000),
            candidate(71, 1_000_000_000_000_000_000u128, 1_000),
        ];
        // The same candidates one block later: `sim_block` (the fan-out's read
        // anchor) moves with the batch's promoted solve block.
        let moved = cands
            .iter()
            .map(|candidate| {
                let mut moved = candidate.clone();
                moved.solve_block = 101;
                moved
            })
            .collect::<Vec<_>>();

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("multi-thread runtime");
        let (
            first,
            first_batches,
            first_members,
            second_batches,
            moved_run,
            moved_batches,
            moved_members,
        ) = std::thread::spawn(move || {
            rt.block_on(async {
                let deployment = deployment(Bytes::from_static(&[0xfe])); // INVALID
                let first = dispatch_test_leaf(
                    cands.clone(),
                    &ctx(&provider, &deployment),
                    &suppression,
                    100,
                    0,
                    &pool_divergence,
                    &fot_registry,
                    Some(Arc::clone(&bot_state)),
                    Some(Arc::clone(&warm)),
                );
                let first_batches = recorder.storage_batches();
                let first_members = recorder.storage_members();
                let _second = dispatch_test_leaf(
                    cands,
                    &ctx(&provider, &deployment),
                    &suppression,
                    100,
                    0,
                    &pool_divergence,
                    &fot_registry,
                    Some(Arc::clone(&bot_state)),
                    Some(Arc::clone(&warm)),
                );
                let second_batches = recorder.storage_batches();
                let moved_run = dispatch_test_leaf(
                    moved,
                    &ctx(&provider, &deployment),
                    &suppression,
                    101,
                    0,
                    &pool_divergence,
                    &fot_registry,
                    Some(bot_state),
                    Some(warm),
                );
                let moved_batches = recorder.storage_batches() - second_batches;
                let moved_members = recorder.storage_members() - first_members;
                (
                    first,
                    first_batches,
                    first_members,
                    second_batches,
                    moved_run,
                    moved_batches,
                    moved_members,
                )
            })
        })
        .join()
        .expect("fan-out thread");

        // The batch's two candidates both route the same two V2 pools: three
        // path words per pool plus the executor's three fixed slots, plus the
        // ONE learned word under a pool the paths name (the stranger's word
        // stays out of the batch).
        assert_eq!(
            first_batches, 1,
            "the fan-out pays exactly ONE storage batch before candidate #1"
        );
        assert_eq!(
            first_members, 12,
            "the batch carries 2 pools x 3 words + 2 pool WETH balances + 3 executor slots \
             + the ONE learned drift word — derivation and learned set in one packet"
        );
        assert_eq!(
            second_batches, first_batches,
            "a fan-out at the SAME anchor finds the memo warm and dispatches no batch"
        );

        // The anchor moves: the memo retires, and the learned set (still inside
        // its decay window) rides the moved anchor's ONE batch too.
        assert_eq!(
            moved_batches, 1,
            "the moved anchor dispatches ONE batch (never one per lane)"
        );
        assert_eq!(
            moved_members, 12,
            "the moved anchor's batch is the derivation plus the same learned hot set"
        );

        // Verdict identity at the dispatch seam: prefetch changes the reads,
        // never the outcome.
        assert_eq!(first.candidate_count, moved_run.candidate_count);
        assert_eq!(first.fail_count, moved_run.fail_count);
        assert_eq!(first.exception_count, moved_run.exception_count);
        assert_eq!(
            first.gas_profitable.len() + first.gas_unprofitable.len(),
            moved_run.gas_profitable.len() + moved_run.gas_unprofitable.len()
        );
        assert_eq!(
            first.fail_buckets.get("empty"),
            moved_run.fail_buckets.get("empty")
        );
        assert_eq!(first.fail_buckets.get("empty"), 2);
    }

    /// The reorg fence, observed on the wire: a same-height reorg — the anchor
    /// number unchanged, the block hash replaced — retires the anchor's memo,
    /// so the fan-out reading the replacement pays its own cold chain instead of
    /// being served the abandoned block's words. Between two fan-outs at one
    /// block (number AND hash) the memo still shares exactly as before.
    #[expect(clippy::expect_used)] // test-only: runtime build + thread join
    #[test]
    fn a_same_height_reorg_retires_the_anchors_storage_memo() {
        use alloy::primitives::B256;
        use prefetch_transport::Recorder;

        let recorder = Recorder::default();
        // The block every fan-out pins: one height, one canonical hash.
        recorder.reorg_at_same_height(B256::new([0xa1; 32]));
        let provider = prefetch_transport::provider(&recorder);
        let suppression = Arc::new(Mutex::new(PathSuppression::new()));
        let pool_divergence = Arc::new(Mutex::new(crate::PoolDivergence::new()));
        let fot_registry = Arc::new(Mutex::new(crate::FeeOnTransferRegistry::new()));
        let bot_state = Arc::new(StateLock::new(BotState::new()));
        let warm = degenbot_simulation::WarmCodeCacheInner::shared_default();

        let cands = vec![
            candidate(80, 1_000_000_000_000_000_000u128, 1_000),
            candidate(81, 1_000_000_000_000_000_000u128, 1_000),
        ];

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("multi-thread runtime");

        let (first_members, shared_batches, reorg_batches, reorg_members, reads, reorg_run) =
            std::thread::spawn(move || {
                rt.block_on(async {
                    let deployment = deployment(Bytes::from_static(&[0xfe])); // INVALID

                    let _first = dispatch_test_leaf(
                        cands.clone(),
                        &ctx(&provider, &deployment),
                        &suppression,
                        100,
                        0,
                        &pool_divergence,
                        &fot_registry,
                        Some(Arc::clone(&bot_state)),
                        Some(Arc::clone(&warm)),
                    );
                    let first_members = recorder.storage_members();
                    let first_batches = recorder.storage_batches();

                    // Same height, same block: the installed memo serves, so the
                    // fan-out dispatches no batch at all.
                    let _second = dispatch_test_leaf(
                        cands.clone(),
                        &ctx(&provider, &deployment),
                        &suppression,
                        100,
                        0,
                        &pool_divergence,
                        &fot_registry,
                        Some(Arc::clone(&bot_state)),
                        Some(Arc::clone(&warm)),
                    );
                    let shared_batches = recorder.storage_batches() - first_batches;

                    // Same height, DIFFERENT block: a reorg the anchor number
                    // alone cannot see.
                    recorder.reorg_at_same_height(B256::new([0xb2; 32]));
                    let reorg_run = dispatch_test_leaf(
                        cands,
                        &ctx(&provider, &deployment),
                        &suppression,
                        100,
                        0,
                        &pool_divergence,
                        &fot_registry,
                        Some(bot_state),
                        Some(warm),
                    );
                    let reorg_batches = recorder.storage_batches() - first_batches - shared_batches;
                    let reorg_members = recorder.storage_members() - first_members;

                    (
                        first_members,
                        shared_batches,
                        reorg_batches,
                        reorg_members,
                        recorder.block_reads(),
                        reorg_run,
                    )
                })
            })
            .join()
            .expect("fan-out thread");

        assert_eq!(
            shared_batches, 0,
            "same number + same hash shares the memo: the second fan-out dispatches no batch"
        );
        assert_eq!(
            reorg_batches, 1,
            "the replacement block is a different anchor — the memo retires and the \
             fan-out pays its own cold chain"
        );
        assert_eq!(
            reorg_members, first_members,
            "the replacement's batch carries the same derived + learned set the abandoned \
             block's did"
        );
        assert_eq!(reads, 3, "one anchor-identity read per fan-out, never more");

        // Verdict identity across the fence: the fence changes which memo serves
        // the reads, never the fan-out's outcome.
        assert_eq!(reorg_run.candidate_count, 2);
        assert_eq!(reorg_run.fail_buckets.get("empty"), 2);
    }

    /// The end-to-end engine parity run (ADR-089): the full dispatch fan-out
    /// over a scripted provider with the INVALID-executor fixture — the evm2
    /// fan-out is unconditional post-cutover; the fixed expectations ARE the
    /// parity gate.
    #[expect(clippy::expect_used)] // test-only: runtime build + thread join
    #[test]
    fn dispatch_end_to_end_revert_fixture_matches_fixed_expectations() {
        let provider = empty_transport::provider();
        let suppression = Arc::new(Mutex::new(PathSuppression::new()));
        let pool_divergence = Arc::new(Mutex::new(crate::PoolDivergence::new()));
        let fot_registry = Arc::new(Mutex::new(crate::FeeOnTransferRegistry::new()));
        let bot_state = Arc::new(StateLock::new(BotState::new()));

        let cands = vec![
            candidate(40, 1_000_000_000_000_000_000u128, 1_000),
            candidate(41, 1_000_000_000_000_000_000u128, 1_000),
        ];

        // The handle build needs a multi-thread runtime (the blocking
        // adapters); the sync dispatch runs inside `block_on` on a worker.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("multi-thread runtime");
        let sim_thread = std::thread::spawn(move || {
            let deployment = deployment(Bytes::from_static(&[0xfe])); // INVALID
            rt.block_on(async {
                dispatch_test_leaf(
                    cands,
                    &ctx(&provider, &deployment),
                    &suppression,
                    100,
                    0,
                    &pool_divergence,
                    &fot_registry,
                    Some(bot_state),
                    None,
                )
            })
        });
        let outcome = sim_thread.join().expect("fan-out thread");

        assert_eq!(outcome.candidate_count, 2);
        assert_eq!(outcome.fail_count, 2);
        assert_eq!(outcome.exception_count, 0);
        assert_eq!(
            outcome.gas_profitable.len() + outcome.gas_unprofitable.len(),
            0,
            "the INVALID executor yields no profitable path"
        );
        // Both candidates: execute() halts on 0xfe → the "empty" bucket with
        // deep frame attribution on whichever engine ran.
        assert_eq!(outcome.fail_buckets.get("empty"), 2);
        assert_eq!(outcome.failures.len(), 2);
        for failure in &outcome.failures {
            assert_eq!(failure.bucket, "empty");
            assert_eq!(
                failure.fail_index,
                Some(0),
                "the execute() call is the sequence's only step"
            );
            let frame = failure
                .reverting_frame
                .as_ref()
                .expect("deep frame attribution");
            assert_eq!(frame.depth, 1);
            assert_eq!(frame.target, EXECUTOR);
            assert_eq!(frame.label, "empty");
        }
    }

    /// Tier 1 parity: the in-process serial branch
    /// (`Some(bot_state)`) tallies `rpc-failed` for every candidate when the
    /// per-block EVM build fails. Under a `current_thread` tokio runtime,
    /// `BlockingAsyncDb::new` returns `None` (it requires a multi-threaded
    /// runtime — the pump's `pyo3_async_runtimes` default), so
    /// `Evm2BlockSimHandle::build` returns `None` and the dispatch fans a
    /// `rpc-failed` `SimFailure` out to every candidate — one per `path_id`.
    /// This is the RPC-free characterization of the serial branch's
    /// build-failure handling: under the no-runtime condition the build
    /// answers `Ok(None)` and one `rpc-failed` record lands per path.
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
        let deployment = deployment(Bytes::from_static(&[0xfe])); // INVALID
        let outcome = dispatch_test_leaf(
            cands,
            &ctx(&provider, &deployment),
            &suppression,
            100,
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
        let ids: Vec<u64> = outcome
            .failures
            .iter()
            .map(|f| f.candidate_id.path_id())
            .collect();
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

    // ── Pool divergence skip + feedback ────────────────

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
        let deployment = deployment(Bytes::from_static(&[0xfe])); // INVALID
        let outcome = dispatch_test_leaf(
            cands,
            &ctx(&provider, &deployment),
            &suppression,
            100,
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
        let simmed_ids: Vec<u64> = outcome
            .failures
            .iter()
            .map(|f| f.candidate_id.path_id())
            .collect();
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
        let deployment = deployment(Bytes::from_static(&[0xfe])); // INVALID
        let outcome = dispatch_test_leaf(
            cands,
            &ctx(&provider, &deployment),
            &suppression,
            100,
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
}

/// The scripted chain the dispatch-lane suites share with the dispatch-scaling
/// probe: one lazy module file, compiled only for this crate's test build and
/// included by path from the benchmark's own targets.
#[cfg(test)]
#[path = "../examples/shared/dispatch_probe.rs"]
#[expect(dead_code, clippy::expect_used)] // the scripted chain fails loudly on a broken fixture
mod probe;

/// The BAL delta storage opt-in at the dispatch seam: what the fan-out reads
/// candidates through, what it pays on the wire, and what it pays when the head
/// does not settle ready.
#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "the scripted chain and its assertions fail loudly on a broken fixture"
)]
mod delta_dispatch_tests {
    use super::*;

    use crate::deployment::SimulationDeployment;
    use alloy::primitives::{Bytes, B256};
    use degenbot_rpc::provider::AlloyProvider;
    use degenbot_simulation::sim::evm2::delta_snapshot::{
        BalHeadDriver, HeadOutcome, HeadReadiness,
    };
    use degenbot_simulation::sim::evm2::warm_code_cache::{
        select_dispatch_storage, DeltaDispatchConfig, DispatchStorageSelection, FanOutStorage,
        StorageSelectionOutcome, WarmCodeCacheInner,
    };
    use degenbot_simulation::sim::ReadAnchor;
    use std::future::Future;
    use std::task::{Context, Poll, Waker};
    use std::time::Duration;

    use super::probe::{provider, runtime, settle_head, BalMode, DeltaRecorder, BLOCK};

    fn empty_cache() -> Arc<RwLock<WarmCodeCacheInner>> {
        WarmCodeCacheInner::shared_default()
    }

    fn delta_cache() -> Arc<RwLock<WarmCodeCacheInner>> {
        WarmCodeCacheInner::shared_with_delta_dispatch(DeltaDispatchConfig::development())
    }

    /// The session's ONE head driver — the handle the canonical-head pipeline
    /// delivers through and the fan-out's selection queries. A fan-out never
    /// mints it and never delivers through it.
    fn session_driver(
        cache: &Arc<RwLock<WarmCodeCacheInner>>,
        provider: &AlloyProvider,
    ) -> Arc<BalHeadDriver> {
        cache
            .read()
            .delta_heads(provider)
            .expect("the opt-in is on, so the session owns one driver")
    }

    /// TEST_TIMEOUT bounds every wait these tests park on: a wait that never
    /// resolves fails the test instead of hanging it. It is a failure guard
    /// only — never a proof about how long a correct wait takes.
    const TEST_TIMEOUT: Duration = Duration::from_secs(5);

    /// Wait, bounded, until the gated BAL fetch is genuinely parked — from
    /// then on the preparation's claim is observably live.
    async fn wait_bal_in_flight(rec: &DeltaRecorder) {
        tokio::time::timeout(TEST_TIMEOUT, async {
            while rec.bal_in_flight() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the preparation's fetch parks on the gate");
    }

    /// Drive `selection` while the gate holds its head's fetch, asserting on
    /// every drive step that it has NOT resolved: the release-gated state
    /// transition that proves a live claim's selection waits for its terminal
    /// result. Returns once the selection's lineage read has landed and the
    /// selection is parked on that wait.
    async fn assert_waits<F>(rec: &DeltaRecorder, selection: &mut std::pin::Pin<Box<F>>)
    where
        F: Future<Output = DispatchStorageSelection>,
    {
        let head_reads_before = rec.head_reads();
        for _ in 0..64 {
            tokio::task::yield_now().await;
            assert!(
                selection
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending(),
                "a selection at a live claim waits for its terminal result, never resolves early"
            );
            if rec.head_reads() > head_reads_before {
                // The lineage read landed: one more drive parks the selection
                // on the live claim's completion, and it must still wait.
                tokio::task::yield_now().await;
                assert!(
                    selection
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop()))
                        .is_pending(),
                    "the selection waits on the live claim, never resolves before its terminal"
                );
                return;
            }
        }
        panic!("the selection's one lineage read never landed");
    }

    /// Drive `selection` to its answer while the gate still holds its head's
    /// fetch: resolving at all inside these drive steps is the release-gated
    /// proof that it did NOT wait on the in-flight preparation.
    async fn drive_to_resolution<F>(
        selection: &mut std::pin::Pin<Box<F>>,
    ) -> DispatchStorageSelection
    where
        F: Future<Output = DispatchStorageSelection>,
    {
        for _ in 0..64 {
            if let Poll::Ready(selection) = selection
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
            {
                return selection;
            }
            tokio::task::yield_now().await;
        }
        panic!("the selection parked instead of answering on a real outcome");
    }

    /// Join the fan-out's thread under the TEST bound: a selection that parks
    /// fails the test instead of hanging it. The bound is a failure guard
    /// only — never a claim about how long a correct wait takes.
    fn join_bounded<T: Send + 'static>(handle: std::thread::JoinHandle<T>) -> T {
        let deadline = std::time::Instant::now() + TEST_TIMEOUT;
        while !handle.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "the fan-out parked instead of resolving on a terminal outcome"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        handle.join().expect("fan-out thread")
    }

    /// The selection is a QUERY, never a producer: a head that never arrived on
    /// the canonical-head edge takes the legacy lane, and the fan-out's whole
    /// traffic is the ONE header read — no BAL request exists anywhere.
    #[test]
    fn dispatch_never_prepares_a_bal_and_a_never_delivered_head_takes_the_legacy_lane() {
        let recorder = DeltaRecorder::default();
        recorder.serve_head(BLOCK, B256::new([0xa1; 32]), B256::new([0xa0; 32]));
        let provider = provider(&recorder);
        let rt = runtime();
        let rec = recorder.clone();
        let (selection, wire) = std::thread::spawn(move || {
            rt.block_on(async {
                let selection = select_dispatch_storage(&provider, &delta_cache(), BLOCK).await;
                (selection, rec.wire())
            })
        })
        .join()
        .expect("selection thread");

        assert!(!selection.is_delta(), "an undelivered head serves nothing");
        assert_eq!(selection.outcome.reason(), Some("undelivered"));
        assert_eq!(recorder.bal_requests(), 0, "the selection prepares no BAL");
        assert_eq!(
            wire.iter()
                .filter(|request| request.starts_with("eth_getBlockByNumber"))
                .count(),
            1,
            "the exact-lineage read still happens: {wire:#?}"
        );
        assert_eq!(
            wire.len(),
            1,
            "the header read is the fan-out's only traffic: {wire:#?}"
        );
    }

    /// A fan-out at the exact in-flight head joins that head's EXISTING
    /// preparation and waits for its terminal settle — no timer, no fallback
    /// label — and the selection resolves DELTA once the gated fetch is
    /// released. The selection's `wait` wall rides along as a diagnostic only;
    /// the release-gated transition is the proof.
    #[test]
    fn a_same_block_in_flight_head_waits_for_the_settle_and_resolves_delta() {
        let recorder = DeltaRecorder::default();
        let parent = B256::new([0xa0; 32]);
        let root = B256::new([0xa1; 32]);
        let child = B256::new([0xb1; 32]);
        recorder.serve_head(BLOCK - 1, root, parent);
        recorder.serve_head(BLOCK, child, root);
        let provider = provider(&recorder);
        let rt = runtime();
        let rec = recorder.clone();
        let (selection, fetches) = join_bounded(std::thread::spawn(move || {
            rt.block_on(async {
                let cache = delta_cache();
                let driver = session_driver(&cache, &provider);
                settle_head(&driver, ReadAnchor::identified(BLOCK - 1, root), parent).await;

                // The head edge delivers and starts the preparation against a
                // GATED endpoint: the fetch is genuinely in flight and only
                // the test's release ends it.
                rec.defer_bal();
                let ticket = driver.deliver(ReadAnchor::identified(BLOCK, child), root);
                let head = Arc::clone(&driver);
                let prepared = tokio::spawn(async move { head.advance(ticket, &[]).await });
                wait_bal_in_flight(&rec).await;
                let fetches_before = rec.bal_requests();

                let mut selection = Box::pin(select_dispatch_storage(&provider, &cache, BLOCK));
                assert_waits(&rec, &mut selection).await;

                rec.release_bal();
                let selection = tokio::time::timeout(TEST_TIMEOUT, selection)
                    .await
                    .expect("the wait ends on the settle, never on a timer");
                prepared.await.expect("the head's preparation settles");
                (selection, rec.bal_requests() - fetches_before)
            })
        }));

        assert_eq!(selection.outcome, StorageSelectionOutcome::Ready);
        assert!(
            selection.is_delta(),
            "the settled head answers through its generation (waited {:?})",
            selection.wait
        );
        assert_eq!(
            fetches, 1,
            "the waiter joined the one background fetch instead of duplicating it"
        );
    }

    /// The same wait at the fan-out seam: the dispatch's ONE selection future
    /// joins the in-flight preparation before the storage view exists, so the
    /// fan-out reads DELTA and pays NO prefetch batch. The wait transition
    /// itself is pinned at the selection seam; here the fan-out's shape is
    /// what must hold — one shared fetch and no fallback batch.
    #[test]
    fn an_in_flight_head_resolves_delta_and_the_fan_out_pays_no_prefetch_batch() {
        let recorder = DeltaRecorder::default();
        let parent = B256::new([0xa0; 32]);
        let root = B256::new([0xa1; 32]);
        let child = B256::new([0xb1; 32]);
        recorder.serve_head(BLOCK - 1, root, parent);
        recorder.serve_head(BLOCK, child, root);
        let provider = provider(&recorder);
        let rt = runtime();
        let rec = recorder.clone();
        let (batches, fetches, outcome) = join_bounded(std::thread::spawn(move || {
            rt.block_on(async {
                let cache = delta_cache();
                let driver = session_driver(&cache, &provider);
                settle_head(&driver, ReadAnchor::identified(BLOCK - 1, root), parent).await;

                // The claim is live and its fetch parked on the gate; the
                // release runs beside the fan-out so the fetch stays the one
                // background work the fan-out joins.
                rec.defer_bal();
                let ticket = driver.deliver(ReadAnchor::identified(BLOCK, child), root);
                let head = Arc::clone(&driver);
                let prepared = tokio::spawn(async move { head.advance(ticket, &[]).await });
                wait_bal_in_flight(&rec).await;
                // The baselines precede the release: exactly one BAL answer —
                // the subject fetch's — happens inside the measured window.
                let fetches_before = rec.bal_requests();
                let batches_before = rec.storage_batches();
                let releaser = {
                    let rec = rec.clone();
                    tokio::spawn(async move {
                        rec.release_bal();
                    })
                };

                let deployment = super::tests::deployment(Bytes::from_static(&[0xfe]));
                let cands = vec![
                    super::tests::candidate(70, 1_000_000_000_000_000_000u128, 1_000),
                    super::tests::candidate(71, 1_000_000_000_000_000_000u128, 1_000),
                ];
                let suppression = Arc::new(Mutex::new(PathSuppression::new()));
                let divergence = Arc::new(Mutex::new(crate::PoolDivergence::new()));
                let fot = Arc::new(Mutex::new(crate::FeeOnTransferRegistry::new()));
                let bot_state = Arc::new(StateLock::new(BotState::new()));
                let outcome = dispatch_test_leaf(
                    cands,
                    &super::tests::ctx(&provider, &deployment),
                    &suppression,
                    BLOCK,
                    0,
                    &divergence,
                    &fot,
                    Some(bot_state),
                    Some(cache),
                );
                releaser.await.expect("the release task ends");
                prepared.await.expect("the head's preparation settles");
                (
                    rec.storage_batches() - batches_before,
                    rec.bal_requests() - fetches_before,
                    outcome,
                )
            })
        }));

        assert_eq!(
            batches, 0,
            "a ready generation answers: the fan-out pays no prefetch batch"
        );
        assert_eq!(
            fetches, 1,
            "one background fetch, joined by the fan-out and never repeated"
        );
        assert_eq!(outcome.candidate_count, 2, "the batch was simulated");
    }

    /// The previous anchor does NOT wait: while the current head's fetch is
    /// parked on the closed gate, a selection at the anchor the installed head
    /// replaced resolves from the generation that was installed there.
    #[test]
    fn the_previous_block_does_not_wait_while_the_current_head_is_in_flight() {
        let recorder = DeltaRecorder::default();
        let genesis = B256::new([0x90; 32]);
        let grandparent = B256::new([0xa0; 32]);
        let root = B256::new([0xa1; 32]);
        let child = B256::new([0xb1; 32]);
        recorder.serve_head(BLOCK - 2, grandparent, genesis);
        recorder.serve_head(BLOCK - 1, root, grandparent);
        recorder.serve_head(BLOCK, child, root);
        let provider = provider(&recorder);
        let rt = runtime();
        let rec = recorder.clone();
        let (selection,) = join_bounded(std::thread::spawn(move || {
            rt.block_on(async {
                let cache = delta_cache();
                let driver = session_driver(&cache, &provider);
                settle_head(
                    &driver,
                    ReadAnchor::identified(BLOCK - 2, grandparent),
                    genesis,
                )
                .await;
                settle_head(
                    &driver,
                    ReadAnchor::identified(BLOCK - 1, root),
                    grandparent,
                )
                .await;
                rec.defer_bal();
                let ticket = driver.deliver(ReadAnchor::identified(BLOCK, child), root);
                let head = Arc::clone(&driver);
                let prepared = tokio::spawn(async move { head.advance(ticket, &[]).await });
                wait_bal_in_flight(&rec).await;

                // The gate stays closed: resolving at all inside these drive
                // steps is the proof that the previous anchor never waits.
                let mut selection = Box::pin(select_dispatch_storage(&provider, &cache, BLOCK - 1));
                let selection = drive_to_resolution(&mut selection).await;
                rec.release_bal();
                prepared.await.expect("the head's preparation settles");
                (selection,)
            })
        }));

        assert_eq!(selection.outcome, StorageSelectionOutcome::Ready);
        assert!(selection.is_delta());
        assert_eq!(selection.anchor, ReadAnchor::identified(BLOCK - 1, root));
    }

    /// An explicit terminal failure waits ONCE and then falls back for real:
    /// the selection joins the in-flight preparation, resolves on its
    /// settle-cold result with the real reason, and the legacy memo + prefetch
    /// lane works — serving no parent value.
    #[test]
    fn an_explicit_failure_waits_once_then_the_legacy_prefetch_lane_works() {
        let recorder = DeltaRecorder::default();
        let parent = B256::new([0xa0; 32]);
        let root = B256::new([0xa1; 32]);
        let child = B256::new([0xb1; 32]);
        recorder.serve_head(BLOCK - 1, root, parent);
        recorder.serve_head(BLOCK, child, root);
        let provider = provider(&recorder);
        let rt = runtime();
        let rec = recorder.clone();
        let (selection, batches, fetches, candidate_count) =
            join_bounded(std::thread::spawn(move || {
                rt.block_on(async {
                    let cache = delta_cache();
                    let driver = session_driver(&cache, &provider);
                    settle_head(&driver, ReadAnchor::identified(BLOCK - 1, root), parent).await;

                    // The list this head is preparing cannot be read, and the
                    // fetch is gated: one wait, one real terminal, one
                    // fallback.
                    rec.defer_bal();
                    rec.set_bal_payload(vec![0xff, 0x00]);
                    let ticket = driver.deliver(ReadAnchor::identified(BLOCK, child), root);
                    let head = Arc::clone(&driver);
                    let prepared = tokio::spawn(async move { head.advance(ticket, &[]).await });
                    wait_bal_in_flight(&rec).await;

                    let mut selection = Box::pin(select_dispatch_storage(&provider, &cache, BLOCK));
                    assert_waits(&rec, &mut selection).await;

                    rec.release_bal();
                    let selection = tokio::time::timeout(TEST_TIMEOUT, selection)
                        .await
                        .expect("the settle-cold result is a real terminal outcome");
                    prepared.await.expect("the head's preparation settles");

                    // The fallback is the shipped lane: the legacy prefetch
                    // batch and the on-demand reads work exactly as OFF.
                    let deployment = super::tests::deployment(Bytes::from_static(&[0xfe]));
                    let cands = vec![
                        super::tests::candidate(70, 1_000_000_000_000_000_000u128, 1_000),
                        super::tests::candidate(71, 1_000_000_000_000_000_000u128, 1_000),
                    ];
                    let suppression = Arc::new(Mutex::new(PathSuppression::new()));
                    let divergence = Arc::new(Mutex::new(crate::PoolDivergence::new()));
                    let fot = Arc::new(Mutex::new(crate::FeeOnTransferRegistry::new()));
                    let bot_state = Arc::new(StateLock::new(BotState::new()));
                    let fetches_before = rec.bal_requests();
                    let batches_before = rec.storage_batches();
                    let outcome = dispatch_test_leaf(
                        cands,
                        &super::tests::ctx(&provider, &deployment),
                        &suppression,
                        BLOCK,
                        0,
                        &divergence,
                        &fot,
                        Some(bot_state),
                        Some(cache),
                    );
                    (
                        selection,
                        rec.storage_batches() - batches_before,
                        rec.bal_requests() - fetches_before,
                        outcome.candidate_count,
                    )
                })
            }));

        assert!(!selection.is_delta(), "a settle-cold head serves nothing");
        assert_eq!(selection.outcome.reason(), Some("bal-unreadable"));
        assert_eq!(batches, 1, "the legacy prefetch lane works on the fallback");
        assert_eq!(
            fetches, 0,
            "the settled-cold fallback requests no BAL of its own"
        );
        assert_eq!(candidate_count, 2);
    }

    /// A preparation dropped without settling is terminal at the dispatch
    /// seam: the selection resolves LEGACY with the `abandoned` reason — no
    /// retry, no stranded wait — and a selection arriving after the
    /// abandonment resolves immediately too.
    #[test]
    fn an_abandoned_preparation_resolves_the_selection_on_the_legacy_lane_with_the_abandoned_reason(
    ) {
        let recorder = DeltaRecorder::default();
        let parent = B256::new([0xa0; 32]);
        let root = B256::new([0xa1; 32]);
        let child = B256::new([0xb1; 32]);
        recorder.serve_head(BLOCK - 1, root, parent);
        recorder.serve_head(BLOCK, child, root);
        let provider = provider(&recorder);
        let rt = runtime();
        let rec = recorder.clone();
        join_bounded(std::thread::spawn(move || {
            rt.block_on(async {
                let cache = delta_cache();
                let driver = session_driver(&cache, &provider);
                settle_head(&driver, ReadAnchor::identified(BLOCK - 1, root), parent).await;
                let fetches_before = rec.bal_requests();

                // The head edge delivers and claims the preparation; the
                // fetch is gated so the claim is observably live, and the
                // cancel side owns its end.
                rec.defer_bal();
                let ticket = driver.deliver(ReadAnchor::identified(BLOCK, child), root);
                let head = Arc::clone(&driver);
                let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
                let prepared = tokio::spawn(async move {
                    let mut prepare = Box::pin(head.advance(ticket, &[]));
                    tokio::select! {
                        _ = &mut prepare => {}
                        _ = cancel_rx => {}
                    }
                    // Cancelling drops the preparation here: the claim is
                    // released and its abandonment published before this task
                    // ends.
                });
                wait_bal_in_flight(&rec).await;

                let mut selection = Box::pin(select_dispatch_storage(&provider, &cache, BLOCK));
                assert_waits(&rec, &mut selection).await;

                // The claim is dropped without settling anything.
                cancel_tx.send(()).expect("the test holds the cancel side");
                prepared.await.expect("the cancelled preparation task ends");
                let selection = tokio::time::timeout(TEST_TIMEOUT, selection)
                    .await
                    .expect("the abandonment is terminal: no waiter strands");

                assert!(
                    !selection.is_delta(),
                    "the fan-out falls back to the legacy lane (waited {:?})",
                    selection.wait
                );
                assert_eq!(selection.outcome.reason(), Some("abandoned"));

                // A selection arriving after the abandonment resolves
                // immediately: nothing is left to wait for.
                let mut late = Box::pin(select_dispatch_storage(&provider, &cache, BLOCK));
                let late = drive_to_resolution(&mut late).await;
                assert_eq!(late.outcome.reason(), Some("abandoned"));
                assert_eq!(
                    rec.bal_requests() - fetches_before,
                    0,
                    "the abandoned fetch never answered"
                );
            });
        }));
    }

    /// A delivered head nothing ever claimed is terminal too: the selection
    /// resolves `abandoned` — it never parks on a preparation that does not
    /// exist.
    #[test]
    fn a_delivered_head_nothing_prepared_resolves_the_selection_with_the_abandoned_reason() {
        let recorder = DeltaRecorder::default();
        let parent = B256::new([0xa0; 32]);
        let root = B256::new([0xa1; 32]);
        let child = B256::new([0xb1; 32]);
        recorder.serve_head(BLOCK - 1, root, parent);
        recorder.serve_head(BLOCK, child, root);
        let provider = provider(&recorder);
        let rt = runtime();
        let rec = recorder.clone();
        let (reason, is_delta, fetches) = join_bounded(std::thread::spawn(move || {
            rt.block_on(async {
                let cache = delta_cache();
                let driver = session_driver(&cache, &provider);
                settle_head(&driver, ReadAnchor::identified(BLOCK - 1, root), parent).await;
                let fetches_before = rec.bal_requests();
                let _ticket = driver.deliver(ReadAnchor::identified(BLOCK, child), root);

                let mut selection = Box::pin(select_dispatch_storage(&provider, &cache, BLOCK));
                let selection = drive_to_resolution(&mut selection).await;
                (
                    selection.outcome.reason().map(str::to_owned),
                    selection.is_delta(),
                    rec.bal_requests() - fetches_before,
                )
            })
        }));

        assert!(!is_delta, "a head nothing prepared serves nothing");
        assert_eq!(reason.as_deref(), Some("abandoned"));
        assert_eq!(fetches, 0, "no preparation ran, so no fetch happened");
    }

    /// The fan-out's own fallback on an abandoned preparation: the legacy
    /// prefetch lane answers with the `abandoned` label, exactly as the other
    /// terminal cold outcomes do.
    #[test]
    fn an_abandoned_preparation_takes_the_fan_out_to_the_legacy_lane_with_the_abandoned_reason() {
        let recorder = DeltaRecorder::default();
        let parent = B256::new([0xa0; 32]);
        let root = B256::new([0xa1; 32]);
        let child = B256::new([0xb1; 32]);
        recorder.serve_head(BLOCK - 1, root, parent);
        recorder.serve_head(BLOCK, child, root);
        let provider = provider(&recorder);
        let rt = runtime();
        let rec = recorder.clone();
        let (batches, fetches, candidate_count) = join_bounded(std::thread::spawn(move || {
            rt.block_on(async {
                let cache = delta_cache();
                let driver = session_driver(&cache, &provider);
                settle_head(&driver, ReadAnchor::identified(BLOCK - 1, root), parent).await;

                rec.defer_bal();
                let ticket = driver.deliver(ReadAnchor::identified(BLOCK, child), root);
                let head = Arc::clone(&driver);
                let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
                let prepared = tokio::spawn(async move {
                    let mut prepare = Box::pin(head.advance(ticket, &[]));
                    tokio::select! {
                        _ = &mut prepare => {}
                        _ = cancel_rx => {}
                    }
                });
                wait_bal_in_flight(&rec).await;
                cancel_tx.send(()).expect("the test holds the cancel side");
                prepared.await.expect("the cancelled preparation task ends");

                // The fallback is the shipped lane: the legacy prefetch batch
                // and the on-demand reads work exactly as OFF.
                let deployment = super::tests::deployment(Bytes::from_static(&[0xfe]));
                let cands = vec![
                    super::tests::candidate(70, 1_000_000_000_000_000_000u128, 1_000),
                    super::tests::candidate(71, 1_000_000_000_000_000_000u128, 1_000),
                ];
                let suppression = Arc::new(Mutex::new(PathSuppression::new()));
                let divergence = Arc::new(Mutex::new(crate::PoolDivergence::new()));
                let fot = Arc::new(Mutex::new(crate::FeeOnTransferRegistry::new()));
                let bot_state = Arc::new(StateLock::new(BotState::new()));
                let fetches_before = rec.bal_requests();
                let batches_before = rec.storage_batches();
                let outcome = dispatch_test_leaf(
                    cands,
                    &super::tests::ctx(&provider, &deployment),
                    &suppression,
                    BLOCK,
                    0,
                    &divergence,
                    &fot,
                    Some(bot_state),
                    Some(cache),
                );
                (
                    rec.storage_batches() - batches_before,
                    rec.bal_requests() - fetches_before,
                    outcome.candidate_count,
                )
            })
        }));

        assert_eq!(batches, 1, "the legacy prefetch lane works on the fallback");
        assert_eq!(
            fetches, 0,
            "the abandoned fallback requests no BAL of its own"
        );
        assert_eq!(candidate_count, 2);
    }

    /// Duplicate fan-outs at one in-flight head join ONE terminal completion and
    /// duplicate no RPC: two concurrent selections wait together and both
    /// resolve delta from the same settled generation.
    #[test]
    fn duplicate_fan_outs_join_one_completion_without_duplicate_bal() {
        let recorder = DeltaRecorder::default();
        let parent = B256::new([0xa0; 32]);
        let root = B256::new([0xa1; 32]);
        let child = B256::new([0xb1; 32]);
        recorder.serve_head(BLOCK - 1, root, parent);
        recorder.serve_head(BLOCK, child, root);
        let provider = provider(&recorder);
        let rt = runtime();
        let rec = recorder.clone();
        let (first, second, fetches, batches) = join_bounded(std::thread::spawn(move || {
            rt.block_on(async {
                let cache = delta_cache();
                let driver = session_driver(&cache, &provider);
                settle_head(&driver, ReadAnchor::identified(BLOCK - 1, root), parent).await;
                rec.defer_bal();
                let ticket = driver.deliver(ReadAnchor::identified(BLOCK, child), root);
                let head = Arc::clone(&driver);
                let prepared = tokio::spawn(async move { head.advance(ticket, &[]).await });
                wait_bal_in_flight(&rec).await;

                let fetches_before = rec.bal_requests();
                let batches_before = rec.storage_batches();
                let mut first = Box::pin(select_dispatch_storage(&provider, &cache, BLOCK));
                let mut second = Box::pin(select_dispatch_storage(&provider, &cache, BLOCK));
                assert_waits(&rec, &mut first).await;
                assert_waits(&rec, &mut second).await;

                rec.release_bal();
                let (first, second) =
                    tokio::time::timeout(TEST_TIMEOUT, async { tokio::join!(first, second) })
                        .await
                        .expect("both fan-outs wake on the one completion");
                prepared.await.expect("the head's preparation settles");
                (
                    first,
                    second,
                    rec.bal_requests() - fetches_before,
                    rec.storage_batches() - batches_before,
                )
            })
        }));

        assert_eq!(first.outcome, StorageSelectionOutcome::Ready);
        assert_eq!(second.outcome, StorageSelectionOutcome::Ready);
        let (FanOutStorage::Delta(one), FanOutStorage::Delta(two)) =
            (&first.storage, &second.storage)
        else {
            panic!("both fan-outs read through the delta generation");
        };
        assert!(
            Arc::ptr_eq(one, two),
            "both fan-outs share the ONE settled generation"
        );
        assert_eq!(
            fetches, 1,
            "two fan-outs, one fetch: the join duplicates no BAL"
        );
        assert_eq!(batches, 0, "and no prefetch batch on either");
    }

    /// OFF is the shipped posture: the legacy memo + prefetch lane unchanged,
    /// and NOT ONE BAL request — the session installs no head driver.
    #[test]
    fn the_opt_in_off_makes_no_bal_request_and_keeps_the_prefetch_lane() {
        let recorder = DeltaRecorder::default();
        recorder.serve_head(BLOCK, B256::new([0xa1; 32]), B256::new([0xa0; 32]));
        let provider = provider(&recorder);
        let rt = runtime();
        let recorder_thread = recorder.clone();
        let (selection,) = std::thread::spawn(move || {
            rt.block_on(async {
                let cache = empty_cache();
                let selection = select_dispatch_storage(&provider, &cache, BLOCK).await;
                (selection,)
            })
        })
        .join()
        .expect("selection thread");

        assert_eq!(selection.outcome, StorageSelectionOutcome::Disabled);
        assert!(!selection.is_delta(), "OFF reads through the legacy memo");
        assert_eq!(
            selection.anchor,
            ReadAnchor::identified(BLOCK, B256::new([0xa1; 32])),
            "the legacy anchor is still the number+hash identity read"
        );
        assert_eq!(recorder_thread.bal_requests(), 0, "no BAL while disabled");
        assert_eq!(recorder_thread.head_reads(), 1, "one identity read");
    }

    /// A head the pipeline delivered and settled READY answers the fan-out, and
    /// the selection's WHOLE traffic is the ONE header read: no BAL request and
    /// no prefetch batch rides the fan-out (a ready lane skips the batch, and the
    /// query never generates storage work).
    #[test]
    fn dispatch_selects_delta_when_the_head_settled_ready_and_pays_no_prefetch_batch() {
        let recorder = DeltaRecorder::default();
        let parent = B256::new([0xa0; 32]);
        let root = B256::new([0xa1; 32]);
        let child = B256::new([0xb1; 32]);
        recorder.serve_head(BLOCK - 1, root, parent);
        recorder.serve_head(BLOCK, child, root);
        let provider = provider(&recorder);
        let rt = runtime();
        let rec = recorder.clone();
        let (selection, hub_is_the_generation, bal_before, bal_after, wire) =
            std::thread::spawn(move || {
                rt.block_on(async {
                    let cache = delta_cache();
                    let driver = session_driver(&cache, &provider);
                    // The canonical-head pipeline, two consecutive heads.
                    settle_head(&driver, ReadAnchor::identified(BLOCK - 1, root), parent).await;
                    settle_head(&driver, ReadAnchor::identified(BLOCK, child), root).await;
                    let bal_before = rec.bal_requests();
                    rec.clear_wire();
                    let selection = select_dispatch_storage(&provider, &cache, BLOCK).await;
                    let wire = rec.wire();
                    let settled = driver.snapshot(ReadAnchor::identified(BLOCK, child));
                    let hub_is_the_generation = match &selection.storage {
                        FanOutStorage::Delta(hub) => Arc::ptr_eq(hub, &settled.hub),
                        FanOutStorage::Legacy => false,
                    };
                    (
                        selection,
                        hub_is_the_generation,
                        bal_before,
                        rec.bal_requests(),
                        wire,
                    )
                })
            })
            .join()
            .expect("selection thread");

        assert_eq!(selection.outcome, StorageSelectionOutcome::Ready);
        assert!(
            selection.is_delta(),
            "a settled head answers through its hub"
        );
        assert_eq!(
            selection.anchor,
            ReadAnchor::identified(BLOCK, child),
            "the head's own exact anchor"
        );
        assert!(
            hub_is_the_generation,
            "the fan-out reads the settled generation, never a copy of it"
        );
        assert_eq!(
            bal_after, bal_before,
            "the selection requests no BAL: the pipeline fetched it"
        );
        assert_eq!(
            wire.len(),
            1,
            "one header read, no prefetch batch and no BAL: {wire:#?}"
        );
        assert!(
            wire[0].starts_with("eth_getBlockByNumber"),
            "the ONE request is the exact-lineage read: {wire:#?}"
        );
    }

    /// A fan-out whose candidates were solved one block behind the head reads the
    /// exact PREVIOUS generation — the whole reason the driver keeps one previous
    /// anchor — while a two-block lag stays cold: the window is two consecutive
    /// anchors, never a retention policy.
    #[test]
    fn a_lagging_fan_out_anchor_reads_the_previous_generation_and_a_two_block_lag_does_not() {
        let recorder = DeltaRecorder::default();
        let genesis = B256::new([0xa0; 32]);
        let grandparent = B256::new([0xa1; 32]);
        let parent = B256::new([0xb1; 32]);
        let child = B256::new([0xc1; 32]);
        recorder.serve_head(BLOCK - 2, grandparent, genesis);
        recorder.serve_head(BLOCK - 1, parent, grandparent);
        recorder.serve_head(BLOCK, child, parent);
        let provider = provider(&recorder);
        let rt = runtime();
        let (lagging, two_below) = std::thread::spawn(move || {
            rt.block_on(async {
                let cache = delta_cache();
                let driver = session_driver(&cache, &provider);
                // Three consecutive heads: the first settles cold (no parent),
                // the second carries it, and the third carries the second.
                settle_head(
                    &driver,
                    ReadAnchor::identified(BLOCK - 2, grandparent),
                    genesis,
                )
                .await;
                settle_head(
                    &driver,
                    ReadAnchor::identified(BLOCK - 1, parent),
                    grandparent,
                )
                .await;
                settle_head(&driver, ReadAnchor::identified(BLOCK, child), parent).await;
                let lagging = select_dispatch_storage(&provider, &cache, BLOCK - 1).await;
                let two_below = select_dispatch_storage(&provider, &cache, BLOCK - 2).await;
                (lagging, two_below)
            })
        })
        .join()
        .expect("selection thread");

        assert_eq!(
            lagging.outcome,
            StorageSelectionOutcome::Ready,
            "the anchor the installed head replaced still answers"
        );
        assert!(lagging.is_delta());
        assert_eq!(lagging.anchor, ReadAnchor::identified(BLOCK - 1, parent));
        assert!(
            !two_below.is_delta(),
            "two blocks below the installed head is outside the window"
        );
        assert_eq!(two_below.outcome.reason(), Some("undelivered"));
    }

    /// A head that SETTLED cold because its lineage could not be carried answers
    /// the fan-out with the collapsed label — the detail behind it lives on the
    /// head's own telemetry row, never on the selection's closed label.
    #[test]
    fn a_lineage_refused_head_takes_the_legacy_path_with_its_reason() {
        let recorder = DeltaRecorder::default();
        recorder.serve_head(BLOCK, B256::new([0xa1; 32]), B256::new([0xa0; 32]));
        let provider = provider(&recorder);
        let rt = runtime();
        let selection = std::thread::spawn(move || {
            rt.block_on(async {
                let cache = delta_cache();
                let driver = session_driver(&cache, &provider);
                // The session's first head: no installed parent to carry.
                let outcome = settle_head(
                    &driver,
                    ReadAnchor::identified(BLOCK, B256::new([0xa1; 32])),
                    B256::new([0xa0; 32]),
                )
                .await;
                assert!(
                    matches!(outcome, HeadOutcome::Settled(_)),
                    "the head settled, cold"
                );
                select_dispatch_storage(&provider, &cache, BLOCK).await
            })
        })
        .join()
        .expect("selection thread");

        assert!(
            !selection.is_delta(),
            "a lineage-refused head serves nothing"
        );
        assert_eq!(selection.outcome.reason(), Some("lineage-refused"));
    }

    /// Repeated fan-outs at the same exact anchor share the ONE settled
    /// generation and repeat no BAL work — and they deliver nothing: the driver's
    /// repeat counter stays put, because the query has no delivery to repeat.
    #[test]
    fn a_ready_head_answers_the_fanout_and_repeats_without_new_bal_work() {
        let recorder = DeltaRecorder::default();
        let parent = B256::new([0xa0; 32]);
        let root = B256::new([0xa1; 32]);
        let child = B256::new([0xb1; 32]);
        recorder.serve_head(BLOCK - 1, root, parent);
        recorder.serve_head(BLOCK, child, root);
        let provider = provider(&recorder);
        let rt = runtime();
        let rec = recorder.clone();
        let (settled, repeated, bal_before, bal_after, deliveries, repeats) =
            std::thread::spawn(move || {
                rt.block_on(async {
                    let cache = delta_cache();
                    let driver = session_driver(&cache, &provider);
                    settle_head(&driver, ReadAnchor::identified(BLOCK - 1, root), parent).await;
                    settle_head(&driver, ReadAnchor::identified(BLOCK, child), root).await;
                    let stats_before = driver.stats();
                    let bal_before = rec.bal_requests();
                    let settled = select_dispatch_storage(&provider, &cache, BLOCK).await;
                    let repeated = select_dispatch_storage(&provider, &cache, BLOCK).await;
                    let stats_after = driver.stats();
                    (
                        settled,
                        repeated,
                        bal_before,
                        rec.bal_requests(),
                        stats_after.delivered - stats_before.delivered,
                        stats_after.repeated - stats_before.repeated,
                    )
                })
            })
            .join()
            .expect("selection thread");

        assert_eq!(settled.outcome, StorageSelectionOutcome::Ready);
        assert_eq!(
            settled.anchor,
            ReadAnchor::identified(BLOCK, child),
            "the head's own exact anchor"
        );
        assert_eq!(repeated.outcome, StorageSelectionOutcome::Ready);
        assert_eq!(
            bal_after, bal_before,
            "a repeated fan-out at the same exact anchor repeats no BAL work"
        );
        assert_eq!(
            (deliveries, repeats),
            (0, 0),
            "the query delivers nothing and repeats nothing: the head edge is the only producer"
        );
        match (&settled.storage, &repeated.storage) {
            (FanOutStorage::Delta(a), FanOutStorage::Delta(b)) => assert!(
                Arc::ptr_eq(a, b),
                "both fan-outs share the ONE settled generation"
            ),
            _ => panic!("both fan-outs must read through the delta generation"),
        }
    }

    /// A list the node cannot serve (the endpoint is unsupported) settles COLD
    /// on an otherwise consecutive head: the legacy lane answers and the reason
    /// is reported.
    #[test]
    fn an_unsupported_bal_endpoint_takes_the_legacy_path_with_its_reason() {
        let recorder = DeltaRecorder::default();
        let parent = B256::new([0xa0; 32]);
        let root = B256::new([0xa1; 32]);
        let child = B256::new([0xb1; 32]);
        recorder.serve_head(BLOCK - 1, root, parent);
        recorder.serve_head(BLOCK, child, root);
        let provider = provider(&recorder);
        let rt = runtime();
        let rec = recorder.clone();
        let (selection, bal, wire) = std::thread::spawn(move || {
            rt.block_on(async {
                let cache = delta_cache();
                let driver = session_driver(&cache, &provider);
                settle_head(&driver, ReadAnchor::identified(BLOCK - 1, root), parent).await;
                rec.set_bal_mode(BalMode::Unsupported);
                settle_head(&driver, ReadAnchor::identified(BLOCK, child), root).await;
                let selection = select_dispatch_storage(&provider, &cache, BLOCK).await;
                (selection, rec.bal_requests(), rec.wire())
            })
        })
        .join()
        .expect("selection thread");

        assert!(
            !selection.is_delta(),
            "an unreadable list serves no parent value: the legacy lane answers"
        );
        assert_eq!(selection.outcome.reason(), Some("bal-unreadable"));
        let bal_wire: Vec<&str> = wire
            .iter()
            .filter_map(|request| request.split(' ').next())
            .filter(|method| {
                *method == "debug_getRawBlockAccessList" || *method == "eth_getBlockAccessList"
            })
            .collect();
        assert_eq!(
            bal_wire,
            vec![
                "debug_getRawBlockAccessList",
                "debug_getRawBlockAccessList",
                "eth_getBlockAccessList",
            ],
            "the serving head fetched the raw list once; the unsupported head paid the \
             refused raw request and then the refused official JSON fallback it selected, \
             and the query fetched nothing: {wire:#?}"
        );
        assert_eq!(
            bal, 3,
            "BOTH official methods count as BAL fetches: one serving fetch, then the \
             refused raw request plus its JSON fallback, and none from the query"
        );
    }

    /// A same-height reorg is a DIFFERENT head: the replacement's own BAL is
    /// what a later fan-out reads against, never the abandoned branch's
    /// generation, and a candidate already holding the older generation keeps
    /// reading it.
    #[test]
    fn a_same_height_reorg_selects_the_replacement_branch() {
        let recorder = DeltaRecorder::default();
        let parent = B256::new([0xa0; 32]);
        let root = B256::new([0xa1; 32]);
        let child = B256::new([0xb1; 32]);
        let replacement = B256::new([0xb2; 32]);
        recorder.serve_head(BLOCK - 1, root, parent);
        recorder.serve_head(BLOCK, child, root);
        let provider = provider(&recorder);
        let rt = runtime();
        let rec = recorder.clone();
        let (held, after, bal) = std::thread::spawn(move || {
            rt.block_on(async {
                let cache = delta_cache();
                let driver = session_driver(&cache, &provider);
                settle_head(&driver, ReadAnchor::identified(BLOCK - 1, root), parent).await;
                settle_head(&driver, ReadAnchor::identified(BLOCK, child), root).await;
                let settled = select_dispatch_storage(&provider, &cache, BLOCK).await;
                let held = match &settled.storage {
                    FanOutStorage::Delta(hub) => Arc::clone(hub),
                    FanOutStorage::Legacy => panic!("the consecutive head settled ready"),
                };
                // Same height, different canonical block: the replacement head
                // arrives on the head edge and supersedes the settled one.
                rec.serve_head(BLOCK, replacement, root);
                settle_head(&driver, ReadAnchor::identified(BLOCK, replacement), root).await;
                let after = select_dispatch_storage(&provider, &cache, BLOCK).await;
                (held, after, rec.bal_requests())
            })
        })
        .join()
        .expect("selection thread");

        assert_eq!(
            held.anchor(),
            ReadAnchor::identified(BLOCK, child),
            "the generation a candidate already holds stays pinned at its own head"
        );
        assert_eq!(
            after.anchor,
            ReadAnchor::identified(BLOCK, replacement),
            "the replacement block's own anchor, never the abandoned one"
        );
        assert!(
            !after.is_delta(),
            "the abandoned branch's parent chain is never carried into the replacement"
        );
        assert_eq!(after.outcome.reason(), Some("lineage-refused"));
        assert_eq!(bal, 3, "the replacement pays its own attempt");
    }

    /// The fan-out's wire signature, OFF versus ON: OFF pays the legacy prefetch
    /// batch and no BAL; a head the canonical-head edge never delivered leaves the
    /// fan-out on the same batch and still pays no BAL (the pipeline owns the
    /// fetch); a READY head reads through its generation and pays NO prefetch
    /// batch. Every run's verdicts are identical — the selection changes the
    /// reads, never the outcome.
    #[test]
    fn the_storage_selection_changes_the_reads_and_never_the_verdict() {
        let recorder = DeltaRecorder::default();
        let parent = B256::new([0xa0; 32]);
        let root = B256::new([0xa1; 32]);
        let child = B256::new([0xb1; 32]);
        recorder.serve_head(BLOCK - 1, root, parent);
        recorder.serve_head(BLOCK, child, root);
        let provider = provider(&recorder);
        let rt = runtime();
        let rec = recorder.clone();

        let (off, cold, off_batches, cold_batches, ready_batches, ready, bal_before, bal_after) =
            std::thread::spawn(move || {
                rt.block_on(async {
                    let deployment = super::tests::deployment(Bytes::from_static(&[0xfe]));
                    let cands = vec![
                        super::tests::candidate(70, 1_000_000_000_000_000_000u128, 1_000),
                        super::tests::candidate(71, 1_000_000_000_000_000_000u128, 1_000),
                    ];
                    let suppression = Arc::new(Mutex::new(PathSuppression::new()));
                    let divergence = Arc::new(Mutex::new(crate::PoolDivergence::new()));
                    let fot = Arc::new(Mutex::new(crate::FeeOnTransferRegistry::new()));
                    let bot_state = Arc::new(StateLock::new(BotState::new()));
                    let ctx = super::tests::ctx(&provider, &deployment);
                    let run =
                        |cands: Vec<crate::DispatchCandidate>,
                         cache: Option<Arc<RwLock<WarmCodeCacheInner>>>| {
                            dispatch_test_leaf(
                                cands,
                                &ctx,
                                &suppression,
                                BLOCK,
                                0,
                                &divergence,
                                &fot,
                                Some(Arc::clone(&bot_state)),
                                cache,
                            )
                        };

                    // OFF: the shipped posture.
                    let off = run(cands.clone(), Some(WarmCodeCacheInner::shared_default()));
                    let off_batches = rec.storage_batches();

                    // ON with no head delivered: `undelivered`, the legacy batch,
                    // and no BAL — the fan-out generates nothing.
                    let warm = WarmCodeCacheInner::shared_with_delta_dispatch(
                        DeltaDispatchConfig::development(),
                    );
                    let cold = run(cands.clone(), Some(Arc::clone(&warm)));
                    let cold_batches = rec.storage_batches() - off_batches;

                    // The canonical-head pipeline settles the two consecutive
                    // heads off the fan-out's path; the child carries, so the
                    // ready generation answers and no prefetch batch is dispatched.
                    let driver = session_driver(&warm, &provider);
                    settle_head(&driver, ReadAnchor::identified(BLOCK - 1, root), parent).await;
                    settle_head(&driver, ReadAnchor::identified(BLOCK, child), root).await;
                    let bal_before = rec.bal_requests();
                    let ready = run(cands, Some(warm));
                    let bal_after = rec.bal_requests();
                    let ready_batches = rec.storage_batches() - off_batches - cold_batches;
                    (
                        off,
                        cold,
                        off_batches,
                        cold_batches,
                        ready_batches,
                        ready,
                        bal_before,
                        bal_after,
                    )
                })
            })
            .join()
            .expect("fan-out thread");

        assert_eq!(off_batches, 1, "OFF pays the legacy prefetch batch");
        assert_eq!(
            cold_batches, 1,
            "an undelivered head falls back to the SAME batch"
        );
        assert_eq!(
            ready_batches, 0,
            "a ready head reads through its generation and pays no prefetch"
        );
        assert_eq!(
            bal_after, bal_before,
            "the ready fan-out requested no BAL: the pipeline had already settled it"
        );
        assert_eq!(cold.candidate_count, off.candidate_count);

        // Verdict identity at the dispatch seam.
        assert_eq!(off.candidate_count, ready.candidate_count);
        assert_eq!(off.fail_count, ready.fail_count);
        assert_eq!(off.exception_count, ready.exception_count);
        assert_eq!(
            off.gas_profitable.len() + off.gas_unprofitable.len(),
            ready.gas_profitable.len() + ready.gas_unprofitable.len()
        );
        assert_eq!(
            off.fail_buckets.get("empty"),
            ready.fail_buckets.get("empty")
        );
        assert_eq!(off.fail_buckets.get("empty"), 2);
    }

    /// The scripted chain both parity pins run on: `BLOCK - 1` built on
    /// [`PARENT`], and `BLOCK` built on it.
    const PARENT: B256 = B256::new([0xa0; 32]);
    const ROOT: B256 = B256::new([0xa1; 32]);
    const CHILD: B256 = B256::new([0xb1; 32]);

    /// One fan-out's verdict surface, per candidate: the gas bucket it landed in
    /// or the failure bucket that claimed it, plus the priced figures that decide
    /// it. Two lanes agree only when every row matches.
    fn verdicts(outcome: &DispatchOutcome) -> Vec<(CandidateId, String)> {
        let mut rows: Vec<(CandidateId, String)> = outcome
            .gas_profitable
            .iter()
            .map(|result| {
                (
                    result.candidate_id,
                    format!(
                        "profitable gross={} net={} gas={} passed={}",
                        result.gross_profit, result.net_profit, result.gas_used, result.passed
                    ),
                )
            })
            .chain(outcome.gas_unprofitable.iter().map(|result| {
                (
                    result.candidate_id,
                    format!(
                        "unprofitable gross={} net={} gas={} passed={}",
                        result.gross_profit, result.net_profit, result.gas_used, result.passed
                    ),
                )
            }))
            .chain(
                outcome
                    .failures
                    .iter()
                    .map(|failure| (failure.candidate_id, failure.bucket.clone())),
            )
            .collect();
        rows.sort();
        rows
    }

    /// The candidates both lanes simulate: two different sizes, so a lane's cap
    /// or pre-filter cannot pass for parity by accident.
    fn parity_candidates() -> Vec<DispatchCandidate> {
        vec![
            super::tests::candidate(70, 1_000_000_000_000_000_000u128, 1_000),
            super::tests::candidate(71, 500_000_000_000_000_000u128, 25),
        ]
    }

    /// The session-scoped handles one fan-out mutates.
    struct FanOutFilters {
        suppression: Arc<Mutex<PathSuppression>>,
        divergence: Arc<Mutex<crate::PoolDivergence>>,
        fot: Arc<Mutex<crate::FeeOnTransferRegistry>>,
        bot_state: Arc<StateLock<BotState>>,
    }

    /// Fresh handles per lane, so one lane's suppression/divergence feedback
    /// cannot reach the other's run.
    fn fresh_filters() -> FanOutFilters {
        FanOutFilters {
            suppression: Arc::new(Mutex::new(PathSuppression::new())),
            divergence: Arc::new(Mutex::new(crate::PoolDivergence::new())),
            fot: Arc::new(Mutex::new(crate::FeeOnTransferRegistry::new())),
            bot_state: Arc::new(StateLock::new(BotState::new())),
        }
    }

    /// One fan-out over [`parity_candidates`] at [`BLOCK`], through `cache` —
    /// `None` being the shipped per-call degrade.
    fn parity_fan_out(
        provider: &AlloyProvider,
        deployment: &SimulationDeployment,
        cache: Option<Arc<RwLock<WarmCodeCacheInner>>>,
    ) -> DispatchOutcome {
        let filters = fresh_filters();
        dispatch_test_leaf(
            parity_candidates(),
            &super::tests::ctx(provider, deployment),
            &filters.suppression,
            BLOCK,
            0,
            &filters.divergence,
            &filters.fot,
            Some(filters.bot_state),
            cache,
        )
    }

    /// The session's armed pipeline, with the chain settled on the ONE driver the
    /// fan-out's selection reads.
    async fn settle_the_chain(
        provider: &AlloyProvider,
        warm: &Arc<RwLock<WarmCodeCacheInner>>,
    ) -> Arc<BalHeadDriver> {
        let pipeline = warm.read().bal_head_pipeline().expect("the opt-in is on");
        warm.read().arm_bal_heads(provider);
        assert!(pipeline.is_armed(), "the pipeline sees every accepted head");
        let driver = pipeline.driver().expect("arming installs the driver");
        assert!(
            Arc::ptr_eq(
                &driver,
                &warm
                    .read()
                    .delta_heads(provider)
                    .expect("the session has one driver")
            ),
            "the pipeline and the fan-out share the ONE session driver"
        );
        let root_ticket = driver.deliver(ReadAnchor::identified(BLOCK - 1, ROOT), PARENT);
        assert!(
            matches!(
                driver.advance(root_ticket, &[]).await,
                HeadOutcome::Settled(_)
            ),
            "the parent head settled"
        );
        let child_ticket = driver.deliver(ReadAnchor::identified(BLOCK, CHILD), ROOT);
        assert!(
            matches!(
                driver.advance(child_ticket, &[]).await,
                HeadOutcome::Settled(_)
            ),
            "the head the fan-out anchors at settled"
        );
        assert_eq!(
            driver
                .snapshot(ReadAnchor::identified(BLOCK, CHILD))
                .readiness,
            HeadReadiness::Ready,
            "and it answers ready before any candidate runs"
        );
        driver
    }

    /// What one ON-versus-OFF comparison observed: both outcomes, plus the lane
    /// evidence that says the ON fan-out read the settled generation.
    struct LaneEvidence {
        off: DispatchOutcome,
        on: DispatchOutcome,
        bal_before_fanout: u64,
        bal_after_fanout: u64,
        ready_views_before: u64,
        ready_views_after: u64,
        delivered: u64,
        repeated: u64,
    }

    /// Run the same candidate list OFF (the session cache without the opt-in),
    /// then ON (the same cache with the pipeline armed and its chain settled).
    async fn on_and_off_lanes(provider: &AlloyProvider, recorder: &DeltaRecorder) -> LaneEvidence {
        let deployment = super::tests::deployment(Bytes::from_static(&[0xfe]));
        let off = parity_fan_out(
            provider,
            &deployment,
            Some(WarmCodeCacheInner::shared_default()),
        );
        assert_eq!(recorder.bal_requests(), 0, "OFF requests no BAL, ever");

        let warm =
            WarmCodeCacheInner::shared_with_delta_dispatch(DeltaDispatchConfig::development());
        let driver = settle_the_chain(provider, &warm).await;
        let bal_before_fanout = recorder.bal_requests();
        let stats_before = driver.stats();
        let on = parity_fan_out(provider, &deployment, Some(Arc::clone(&warm)));
        let stats_after = driver.stats();
        LaneEvidence {
            off,
            on,
            bal_before_fanout,
            bal_after_fanout: recorder.bal_requests(),
            ready_views_before: stats_before.ready_views,
            ready_views_after: stats_after.ready_views,
            delivered: stats_after.delivered - stats_before.delivered,
            repeated: stats_after.repeated - stats_before.repeated,
        }
    }

    /// Candidate verdicts and failure buckets are identical with the pipeline ON
    /// and OFF: the session's canonical-head pipeline is armed and has settled the
    /// exact head before the fan-out, so the ON lane reads a delta generation
    /// instead of the legacy memo — and not one verdict, failure bucket, or
    /// exception moves.
    ///
    /// The selection is the only difference between the lanes; the pipeline owns
    /// the preparation the fan-out never pays.
    #[test]
    fn candidate_verdicts_and_failure_buckets_are_identical_with_the_pipeline_on_and_off() {
        let recorder = DeltaRecorder::default();
        recorder.serve_head(BLOCK - 1, ROOT, PARENT);
        recorder.serve_head(BLOCK, CHILD, ROOT);
        let provider = provider(&recorder);
        let rt = runtime();
        let rec = recorder.clone();
        let evidence = std::thread::spawn(move || rt.block_on(on_and_off_lanes(&provider, &rec)))
            .join()
            .expect("fan-out thread");
        let (off, on) = (&evidence.off, &evidence.on);

        assert_eq!(
            evidence.bal_before_fanout, 2,
            "the pipeline prepared and applied one list per head"
        );
        assert_eq!(
            evidence.bal_after_fanout, evidence.bal_before_fanout,
            "the ON fan-out resumed an already-settled head and requested no BAL"
        );
        assert_eq!(
            evidence.repeated, 0,
            "the fan-out delivers nothing: the head edge is the only producer"
        );
        assert_eq!(
            evidence.delivered, 0,
            "and it mints no delivery, so a fan-out can never manufacture a head"
        );
        assert!(
            evidence.ready_views_after > evidence.ready_views_before,
            "the fan-out read the settled generation ({} -> {} ready views)",
            evidence.ready_views_before,
            evidence.ready_views_after
        );

        assert_eq!(
            on.candidate_count, off.candidate_count,
            "the same candidates were simulated"
        );
        assert_eq!(on.fail_count, off.fail_count, "the same candidates failed");
        assert_eq!(
            on.exception_count, off.exception_count,
            "the same candidates raised"
        );
        assert_eq!(
            on.sim_ok_count(),
            off.sim_ok_count(),
            "the same candidates returned a result"
        );
        assert_eq!(on.suppressed_count, off.suppressed_count);
        assert_eq!(on.thin_dropped, off.thin_dropped);
        assert_eq!(on.divergent_dropped, off.divergent_dropped);
        assert_eq!(on.fot_dropped, off.fot_dropped);
        assert_eq!(on.stale_dropped, off.stale_dropped);
        assert_eq!(
            on.fail_buckets.buckets(),
            off.fail_buckets.buckets(),
            "the failure-bucket histogram is identical across the lanes"
        );
        assert_eq!(
            verdicts(on),
            verdicts(off),
            "every candidate's verdict — its bucket and its priced figures — is identical"
        );
        assert_eq!(
            verdicts(on).len(),
            off.candidate_count,
            "every simulated candidate produced exactly one verdict row"
        );
    }

    /// What one legacy-versus-OFF comparison observed: both outcomes and both
    /// wire streams, so the OFF lane's traffic can be compared byte for byte.
    struct OffLaneEvidence {
        legacy: DispatchOutcome,
        session: DispatchOutcome,
        legacy_wire: Vec<String>,
        session_wire: Vec<String>,
    }

    /// Run the same candidate list through the shipped legacy path (no session
    /// cache at all) and through an OFF session (a session cache with no opt-in).
    fn legacy_and_off_lanes(provider: &AlloyProvider, recorder: &DeltaRecorder) -> OffLaneEvidence {
        let deployment = super::tests::deployment(Bytes::from_static(&[0xfe]));

        recorder.clear_wire();
        let legacy = parity_fan_out(provider, &deployment, None);
        let legacy_wire = recorder.wire();

        let cache = WarmCodeCacheInner::shared_default();
        assert!(
            cache.read().bal_head_pipeline().is_none(),
            "an OFF session has no pipeline, so the stage surface installs no sink"
        );
        cache.read().arm_bal_heads(provider);
        assert!(
            cache.read().delta_heads(provider).is_none(),
            "and arming mints no driver: no BAL request could exist"
        );
        recorder.clear_wire();
        let session = parity_fan_out(provider, &deployment, Some(Arc::clone(&cache)));
        let session_wire = recorder.wire();
        assert!(
            cache.read().delta_heads(provider).is_none(),
            "the OFF lane minted no driver even after a fan-out"
        );
        assert!(
            cache.read().bal_head_pipeline().is_none(),
            "and still owns no pipeline"
        );
        OffLaneEvidence {
            legacy,
            session,
            legacy_wire,
            session_wire,
        }
    }

    /// The OFF lane is byte identical to the shipped legacy path: the same
    /// fan-out run with NO session cache at all (the pre-opt-in degrade to a
    /// fresh per-call cache) dispatches the same requests, in the same order,
    /// with the same params, and produces the same verdicts.
    ///
    /// The opt-in's machinery is therefore invisible when it is off: an OFF
    /// session installs no pipeline, mints no head driver, cannot be armed, and
    /// issues no BAL request — structurally, not behind a flag check.
    #[test]
    fn the_off_lane_is_byte_identical_to_the_shipped_legacy_path() {
        let recorder = DeltaRecorder::default();
        recorder.serve_head(BLOCK - 1, ROOT, PARENT);
        recorder.serve_head(BLOCK, CHILD, ROOT);
        let provider = provider(&recorder);
        let rt = runtime();
        let rec = recorder.clone();
        let evidence = std::thread::spawn(move || {
            rt.block_on(async { legacy_and_off_lanes(&provider, &rec) })
        })
        .join()
        .expect("fan-out thread");
        let (legacy, session) = (&evidence.legacy, &evidence.session);

        assert!(
            evidence
                .session_wire
                .iter()
                .any(|request| request.starts_with("eth_getBlockByNumber")),
            "the legacy lane pays its one identity read: {:#?}",
            evidence.session_wire
        );
        assert!(
            evidence
                .session_wire
                .iter()
                .any(|request| request.starts_with("eth_getStorageAt")),
            "and its prefetch batch, so the comparison has real traffic: {:#?}",
            evidence.session_wire
        );
        assert_eq!(
            evidence.session_wire, evidence.legacy_wire,
            "the OFF lane's wire stream is byte identical to the shipped legacy path's: \
             same requests, same order, same params"
        );
        assert!(
            !evidence
                .session_wire
                .iter()
                .any(|request| request.starts_with("debug_getRawBlockAccessList")),
            "the OFF lane issues no BAL request: {:#?}",
            evidence.session_wire
        );
        assert_eq!(
            session.candidate_count, legacy.candidate_count,
            "the same candidates were simulated"
        );
        assert_eq!(session.fail_count, legacy.fail_count);
        assert_eq!(session.exception_count, legacy.exception_count);
        assert_eq!(session.sim_ok_count(), legacy.sim_ok_count());
        assert_eq!(
            session.fail_buckets.buckets(),
            legacy.fail_buckets.buckets(),
            "the OFF lane's failure buckets are the legacy path's"
        );
        assert_eq!(
            verdicts(session),
            verdicts(legacy),
            "the OFF lane's verdicts are the legacy path's"
        );
    }

    /// Keeps a second [`tracing::Dispatch`] registered for the life of the test
    /// binary, without ever installing it as any thread's default.
    ///
    /// `tracing` caches every callsite's `Interest` in **process-global** state,
    /// and re-derives it from the *rebuilding thread's* default whenever exactly
    /// one dispatch is registered (`tracing_core`'s single-dispatch fast path
    /// reads `dispatcher::get_default`). A scoped `set_default` capture is
    /// thread-local: the cargo test harness's other threads own no subscriber, so
    /// the first hit of a fan-out callsite from one of them caches
    /// `Interest::never()` for the whole process — and the subscribed thread's own
    /// row is then dropped before dispatch. That is why the fan-out rows vanish
    /// only under the parallel test harness, and different row families vanish on
    /// different runs.
    ///
    /// A second live dispatch takes the interest cache off that single-dispatch
    /// path: the rebuild combines both opinions (`never` vs `always` →
    /// `sometimes`), so no callsite can be cached `never` and every event is
    /// decided by the emitting thread's own subscriber. This dispatch is inert —
    /// registered, never installed — so it swallows nothing and receives nothing.
    fn keep_a_second_dispatch_registered() {
        static SECOND_DISPATCH: std::sync::OnceLock<tracing::dispatcher::Dispatch> =
            std::sync::OnceLock::new();
        SECOND_DISPATCH.get_or_init(|| {
            tracing::dispatcher::Dispatch::new(tracing::subscriber::NoSubscriber::default())
        });
    }

    /// A log buffer the test subscriber writes into.
    #[derive(Clone, Default)]
    struct LogBuffer(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for LogBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log buffer").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    impl LogBuffer {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().expect("log buffer")).into_owned()
        }
    }

    /// The `fanout=<id>` a row of `marker`'s family carries, if it carries one.
    fn fanout_of(text: &str, marker: &str) -> Option<String> {
        let line = text.lines().find(|line| line.contains(marker))?;
        let rest = &line[line.find("fanout=")? + "fanout=".len()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        (!digits.is_empty()).then_some(digits)
    }

    /// ONE fan-out mints ONE identifier and every row it emits carries it: the
    /// storage selection, the prefetch batch, the `[sim]` delta summary, and each
    /// candidate's `fanout-trace` row all name the same fan-out. That join is what
    /// lets a capture attribute a candidate (and its lane) without guessing from a
    /// block number.
    #[test]
    fn the_fanout_id_joins_selection_prefetch_sim_and_trace_rows_for_one_fan_out() {
        let recorder = DeltaRecorder::default();
        recorder.serve_head(BLOCK, B256::new([0xa1; 32]), B256::new([0xa0; 32]));
        let provider = provider(&recorder);
        let rt = runtime();
        // The capture is thread-local, so this binary's other test threads must
        // not be able to cache these rows' callsites as uninteresting.
        keep_a_second_dispatch_registered();
        let buffer = LogBuffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(buffer.clone())
            .finish();
        let evidence = std::thread::spawn(move || {
            let _guard = tracing::subscriber::set_default(subscriber);
            rt.block_on(async {
                let deployment = super::tests::deployment(Bytes::from_static(&[0xfe]));
                let cands = vec![
                    super::tests::candidate(70, 1_000_000_000_000_000_000u128, 1_000),
                    super::tests::candidate(71, 1_000_000_000_000_000_000u128, 1_000),
                ];
                let suppression = Arc::new(Mutex::new(PathSuppression::new()));
                let divergence = Arc::new(Mutex::new(crate::PoolDivergence::new()));
                let fot = Arc::new(Mutex::new(crate::FeeOnTransferRegistry::new()));
                let bot_state = Arc::new(StateLock::new(BotState::new()));
                // The legacy lane: this fan-out pays the prefetch batch, so all
                // four row families exist in ONE fan-out's log.
                dispatch_test_leaf(
                    cands,
                    &super::tests::ctx(&provider, &deployment),
                    &suppression,
                    BLOCK,
                    0,
                    &divergence,
                    &fot,
                    Some(bot_state),
                    Some(WarmCodeCacheInner::shared_default()),
                )
            })
        })
        .join()
        .expect("fan-out thread");
        assert_eq!(evidence.candidate_count, 2);

        let text = buffer.text();
        let selection = fanout_of(&text, "fan-out storage selection").expect("a selection row");
        let prefetch = fanout_of(&text, "storage prefetch").expect("a prefetch row");
        let lab = fanout_of(&text, "cands=").expect("a `cands=` row");
        assert_eq!(
            (prefetch.as_str(), lab.as_str()),
            (selection.as_str(), selection.as_str()),
            "every dispatch-level row of one fan-out names the same fan-out:\n{text}"
        );
        let traces: Vec<&str> = text
            .lines()
            .filter(|line| line.contains("fanout-trace "))
            .collect();
        assert_eq!(traces.len(), 2, "one trace row per candidate:\n{text}");
        for row in traces {
            assert!(
                row.contains(&format!("\"fanout\":{selection}")),
                "a candidate's trace row names its fan-out: {row}"
            );
        }
    }

    /// The reason vocabulary the diagnostic line and the metric labels share is
    /// this closed set: the real terminal cold outcomes a fallback can name. A
    /// head still preparing is never one of them — a fan-out at one waits for
    /// the head's own terminal result instead of falling back on a label — and
    /// a rename has to come with the tests above.
    #[test]
    fn the_cold_reason_vocabulary_is_the_closed_set() {
        assert_eq!(
            [
                StorageSelectionOutcome::Cold("abandoned").reason(),
                StorageSelectionOutcome::Cold("superseded").reason(),
                StorageSelectionOutcome::Cold("undelivered").reason(),
                StorageSelectionOutcome::Cold("lineage-refused").reason(),
                StorageSelectionOutcome::Cold("bal-unreadable").reason(),
                StorageSelectionOutcome::Cold("settled").reason(),
                StorageSelectionOutcome::LineageUnreadable.reason(),
                StorageSelectionOutcome::Ready.reason(),
                StorageSelectionOutcome::Disabled.reason(),
            ],
            [
                Some("abandoned"),
                Some("superseded"),
                Some("undelivered"),
                Some("lineage-refused"),
                Some("bal-unreadable"),
                Some("settled"),
                Some("lineage-unreadable"),
                None,
                None
            ]
        );
        assert_eq!(StorageSelectionOutcome::Ready.mode(), "delta");
        assert_eq!(
            StorageSelectionOutcome::Cold("bal-unreadable").mode(),
            "legacy"
        );
    }
}
