//! The standalone pre-simulation admission policy.
//!
//! One home for the decision "which solved candidates reach simulation this
//! block", and for the order they reach it in. The pipeline is fixed:
//!
//! `stamping suppression -> pool divergence -> fee-on-transfer -> thin margin ->
//! candidate staleness -> optional order-index reorder -> cap`
//!
//! Every stage reads and (for suppression) writes mutable policy state that
//! outlives the call: the [`PathSuppression`] ledger, the [`PoolDivergence`]
//! memo, and the [`FeeOnTransferRegistry`]. Locks are taken synchronously in
//! short bookend scopes and never cross an `await`; a poisoned registry panics
//! loudly rather than degrading the policy.
//!
//! # Suppression stamps, it does not merely filter
//!
//! [`PathSuppression::is_suppressed`] has a side effect: a retry-due suppressed
//! candidate is admitted AND stamped with the current block, which consumes its
//! retry interval. Because the stamp happens at the first stage, a candidate
//! that a LATER stage drops (divergence, fee-on-transfer, thin margin,
//! staleness, cap) still spends its retry — that is the intended retry
//! cadence, not a leak. The read is sequential over the candidate list, so of
//! two rows sharing one candidate id only the first is admitted and stamped;
//! the second is suppressed by the stamp the first just wrote.
//!
//! # Cap and order
//!
//! The cap ([`MAX_SIMULATE_CONCURRENT`]) is applied to the list the reorder
//! produced: with the `order-index` feature the reorder is the net-profit
//! ranking, so the cap keeps the profit prefix; without it the caller's own
//! pre-sort is the contract and the cap keeps the input prefix. Dropping a
//! candidate at the cap is silent — no tally — and frees no slot for a dropped
//! sibling.
//!
//! # What this module does not own
//!
//! Input enumeration and raw-row validation (the caller's), simulation, and the
//! post-simulation outcome recording (`record_success`/`record_failure` for
//! candidates that actually ran, plus the divergence/FoT feedback) belong to
//! the dispatch fan-out.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use alloy::primitives::U256;
use degenbot_execution::HopInfo;
use degenbot_submission::PathSuppression;
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::BotState;

use crate::dispatch::DispatchCandidate;
use crate::{hop_input_token, hop_pool_key, FeeOnTransferRegistry, PoolDivergence};

// -------------------------------------------------------------------------
// Policy constants
// -------------------------------------------------------------------------

/// The concurrent-simulation cap — candidates past it are NOT simulated, so
/// the cap must sit above what a per-block candidate set realistically
/// reaches.
pub const MAX_SIMULATE_CONCURRENT: usize = 50;

/// The basis-points denominator.
pub const BPS_DENOM: u128 = 10_000;

// -------------------------------------------------------------------------
// The admission pass
// -------------------------------------------------------------------------

/// The mutable policy state and per-block facts one admission pass reads.
///
/// The three registries arrive as the shared arcs the dispatch fan-out already
/// holds, so the pass locks exactly the objects the post-simulation feedback
/// records into — one ledger, one memo, one suspicion registry per session.
pub(crate) struct AdmissionPolicy<'a> {
    /// The retry-interval ledger. Read (and stamped) once per candidate.
    pub(crate) suppression: &'a Arc<Mutex<PathSuppression>>,
    /// Pools flagged `SolverCalc` within the decay window.
    pub(crate) divergence: &'a Arc<Mutex<PoolDivergence>>,
    /// Tokens confirmed fee-on-transfer.
    pub(crate) fee_on_transfer: &'a Arc<Mutex<FeeOnTransferRegistry>>,
    /// The engine's shared state owner — the staleness gate's sample source.
    /// `None` skips the gate (the empty and pre-`BotState` callers).
    pub(crate) bot_state: Option<&'a StateLock<BotState>>,
    /// The block this pass admits for: the retry clock and the decay window.
    pub(crate) current_block: u64,
    /// The thin-margin floor in basis points of `optimal_input` (`0` disables).
    pub(crate) min_profit_margin_bps: u64,
}

/// What one admission pass dropped, per stage, in pipeline order.
///
/// Mirrors the per-call drop counters on the dispatch outcome; the lifetime
/// tallies live on the registries themselves.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AdmissionSkips {
    /// Candidates benched by the retry-interval ledger.
    pub(crate) suppressed: usize,
    /// Candidates routing through a pool flagged `SolverCalc` in the window.
    pub(crate) divergent: usize,
    /// Candidates whose any hop's input token is FoT-confirmed.
    pub(crate) fee_on_transfer: usize,
    /// Candidates dropped by the thin-margin floor.
    pub(crate) thin_margin: usize,
    /// Candidates whose solve-time hop nonce is no longer current.
    pub(crate) stale: usize,
}

impl AdmissionSkips {
    /// The sum of every stage's drops — the convenience the seam suite's
    /// "the stages partition what the pass dropped" assertions read, so it rides
    /// the test build only.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn total(&self) -> usize {
        self.suppressed + self.divergent + self.fee_on_transfer + self.thin_margin + self.stale
    }
}

/// One admission pass' result: the candidates simulation receives, in
/// simulation order, plus the per-stage drop tally.
#[derive(Debug)]
pub(crate) struct Admission {
    /// The surviving candidates, already reordered and capped.
    pub(crate) admitted: Vec<DispatchCandidate>,
    /// What each stage dropped on the way here.
    pub(crate) skips: AdmissionSkips,
}

/// Run the pre-simulation admission policy over `candidates` in their given
/// order.
///
/// The returned candidates are the ones to simulate, in the order to simulate
/// them.
///
/// # Panics
///
/// Panics if the suppression, divergence, or fee-on-transfer mutex is poisoned
/// (a peer task panicked while holding it) — the same loud failure the fan-out
/// bookends have always had, so a poisoned ledger is never silently read as
/// "nothing suppressed".
#[expect(clippy::expect_used)] // the poison guards are the policy's loud-failure contract
#[must_use]
pub(crate) fn admit(
    mut candidates: Vec<DispatchCandidate>,
    policy: &AdmissionPolicy<'_>,
) -> Admission {
    let mut skips = AdmissionSkips::default();

    // 1. Suppression — the STAMPING read (see the module docs).
    let before = candidates.len();
    {
        let mut ledger = policy
            .suppression
            .lock()
            .expect("suppression mutex poisoned");
        candidates.retain(|c| !ledger.is_suppressed(c.candidate_id, policy.current_block));
    }
    skips.suppressed = before - candidates.len();

    // 2. Pool divergence, then the lifetime tally for what it dropped.
    let before = candidates.len();
    {
        let divergence = policy
            .divergence
            .lock()
            .expect("pool_divergence mutex poisoned");
        candidates.retain(|c| {
            !c.path_info
                .hops
                .iter()
                .filter_map(hop_pool_key)
                .any(|k| divergence.is_divergent(k, policy.current_block))
        });
        skips.divergent = before - candidates.len();
    }
    if skips.divergent > 0 {
        let mut divergence = policy
            .divergence
            .lock()
            .expect("pool_divergence mutex poisoned");
        for _ in 0..skips.divergent {
            divergence.record_dropped();
        }
    }

    // 3. Fee-on-transfer, then the same lifetime tally.
    let before = candidates.len();
    {
        let registry = policy
            .fee_on_transfer
            .lock()
            .expect("fot_registry mutex poisoned");
        candidates.retain(|c| {
            !c.path_info
                .hops
                .iter()
                .any(|hop| registry.is_fot(hop_input_token(hop), policy.current_block))
        });
        skips.fee_on_transfer = before - candidates.len();
    }
    if skips.fee_on_transfer > 0 {
        let mut registry = policy
            .fee_on_transfer
            .lock()
            .expect("fot_registry mutex poisoned");
        for _ in 0..skips.fee_on_transfer {
            registry.record_dropped();
        }
    }

    // 4. Thin margin.
    let (kept, dropped) = filter_thin_margin_results(candidates, policy.min_profit_margin_bps);
    skips.thin_margin = dropped;
    candidates = kept;

    // 5. Staleness — the solve-time hop nonce against the live state. The read
    //    guard covers this retain only, never the caller's provider I/O.
    let before = candidates.len();
    if let Some(state) = policy.bot_state {
        let guard = state.read_at(degenbot_substrate::state_lock::LockSite::Sim);
        candidates.retain(|c| !candidate_is_stale(&guard, c));
    }
    skips.stale = before - candidates.len();

    // 6. Reorder, then cap what the reorder produced.
    #[cfg(feature = "order-index")]
    order_index_top_selection(&mut candidates);
    candidates.truncate(MAX_SIMULATE_CONCURRENT);

    Admission {
        admitted: candidates,
        skips,
    }
}

// -------------------------------------------------------------------------
// Thin-margin floor
// -------------------------------------------------------------------------

/// Drop solver results whose gross-profit margin is too low.
///
/// S1 found that the dominant IIA reverts in V3/V4-heavy perms are razor-thin
/// arb (gross profit ~0.001 USD on ~0.06-1.30 USD input = sub-0.2 bps margin)
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
/// # Parity-gate note
///
/// Ports the reference from `examples/eth_backrun_helpers.py`. The leaf is
/// standalone-usable (pure int + zero allocation beyond the kept vec).
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
        // `profit * BPS_DENOM >= opt_input * min_profit_margin_bps` is "kept".
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

// -------------------------------------------------------------------------
// Staleness gate
// -------------------------------------------------------------------------

/// Resolve a hop's `pool_id` on `BotState` and check whether its current
/// `state_nonce` has advanced past the snapshot `candidate.steps[hop]`.
/// Returns `true` (stale) if ANY hop's nonce has advanced. A hop whose
/// `pool_id` cannot be resolved is treated as fresh (the path-validity guard
/// at resolve time already dropped unresolvable paths; a transient miss here
/// is the registry's responsibility, not a staleness signal).
///
/// The executor chains the solver's predicted hop outputs as exact-in
/// amounts, so a stale hop's over-prediction becomes an IIA underpayment
/// revert. Dropping the candidate pre-sim avoids the revert.
#[must_use]
pub fn candidate_is_stale(core: &BotState, candidate: &DispatchCandidate) -> bool {
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
                    core.pool_id_by_key(h.pool_manager_address, &pid)
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

// -------------------------------------------------------------------------
// Order-index reorder
// -------------------------------------------------------------------------

/// Deterministically reorder `candidates` into the order index's net-profit
/// ranking (profit descending, id ascending) before the
/// [`MAX_SIMULATE_CONCURRENT`] cap — the `order-index` feature's substitute for
/// the "candidates arrive pre-sorted" contract (see ADR-024). Today the solver
/// emits no per-path gas, so `gas = 0` and `net == gross == engine_profit`; the
/// seam becomes net-aware as soon as per-path gas is available.
#[cfg(feature = "order-index")]
fn order_index_top_selection(candidates: &mut [DispatchCandidate]) {
    use degenbot_execution::CandidateId;
    use degenbot_order_index::{EnvelopeIndex, OrderIndex};
    let mut idx = EnvelopeIndex::<CandidateId>::new();
    for c in candidates.iter() {
        idx.insert(c.candidate_id, U256::ZERO, U256::from(c.engine_profit));
    }
    let ranked = idx.top_k(U256::ZERO, candidates.len());
    // `ranked` is every id in (net desc, id asc) order. Safe-guard: only enforce
    // when it covers every candidate (candidate ids are unique by construction).
    if ranked.len() != candidates.len() {
        return;
    }
    let pos: HashMap<CandidateId, usize> = ranked
        .into_iter()
        .enumerate()
        .map(|(i, id)| (id, i))
        .collect();
    candidates.sort_by_key(|c| pos.get(&c.candidate_id).copied().unwrap_or(usize::MAX));
}

#[expect(clippy::unwrap_used)]
#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, aliases::U112, Address, B256};
    use degenbot_core::fee::FeeRatio;
    use degenbot_execution::{CandidateId, DeploymentId, PathInfo, V2HopInfo};
    use degenbot_pools::{DexVariant, RegisterV2PoolParams};
    use degenbot_submission::{PATH_SUPPRESS_RETRY_INTERVAL, PATH_SUPPRESS_THRESHOLD};

    use crate::dispatch::SolveStep;
    use crate::{PoolDivergenceKey, POOL_DIVERGENCE_DECAY_BLOCKS};

    const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    const OTHER: Address = address!("1111111111111111111111111111111111111111");
    /// The fixture pools — one flaggable, one clean.
    const POOL_A: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    const POOL_B: Address = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

    /// The retry clock the stamping tests run at: past the retry interval, so a
    /// benched candidate is due.
    const DUE_BLOCK: u64 = 1_000;

    fn deployment_id() -> DeploymentId {
        DeploymentId::new(1)
    }

    fn candidate_id(path_id: u64) -> CandidateId {
        CandidateId::new(path_id, deployment_id())
    }

    fn step(state_nonce: u64) -> SolveStep {
        SolveStep {
            output: 1_100_000_000_000_000_000u128,
            consumed_input: 1_000_000_000_000_000_000u128,
            state_nonce,
        }
    }

    fn single_v2_hop(pool: Address) -> PathInfo {
        PathInfo::new(vec![HopInfo::V2(V2HopInfo {
            pool_address: pool,
            token0_address: WETH,
            token1_address: OTHER,
            fee: 30,
            zfo: true,
        })])
    }

    fn two_v2_hops() -> PathInfo {
        PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: POOL_A,
                token0_address: WETH,
                token1_address: OTHER,
                fee: 30,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: POOL_B,
                token0_address: OTHER,
                token1_address: WETH,
                fee: 30,
                zfo: true,
            }),
        ])
    }

    /// A two-hop candidate through both fixture pools.
    fn candidate(path_id: u64, opt_input: u128, profit: u128) -> DispatchCandidate {
        DispatchCandidate {
            candidate_id: candidate_id(path_id),
            optimal_input: opt_input,
            engine_profit: profit,
            steps: Box::new([step(0), step(0)]),
            solve_block: 100,
            path_info: two_v2_hops(),
            prepared: None,
        }
    }

    /// Many two-hop candidates, ids `1..=count`, profit from `profit_of`.
    fn candidates(count: u64, profit_of: impl Fn(u64) -> u128) -> Vec<DispatchCandidate> {
        (1..=count)
            .map(|path_id| candidate(path_id, 1_000_000, profit_of(path_id)))
            .collect()
    }

    /// A one-hop candidate routing through `pool`, carrying `state_nonce` on
    /// its single step (the staleness gate's snapshot).
    fn candidate_through_pool(path_id: u64, pool: Address, state_nonce: u64) -> DispatchCandidate {
        DispatchCandidate {
            candidate_id: candidate_id(path_id),
            optimal_input: 1_000_000_000_000_000_000u128,
            engine_profit: 1_000,
            steps: Box::new([step(state_nonce)]),
            solve_block: 100,
            path_info: single_v2_hop(pool),
            prepared: None,
        }
    }

    /// The three policy registries, fresh per test.
    struct Registries {
        suppression: Arc<Mutex<PathSuppression>>,
        divergence: Arc<Mutex<PoolDivergence>>,
        fee_on_transfer: Arc<Mutex<FeeOnTransferRegistry>>,
    }

    fn registries() -> Registries {
        Registries {
            suppression: Arc::new(Mutex::new(PathSuppression::new())),
            divergence: Arc::new(Mutex::new(PoolDivergence::new())),
            fee_on_transfer: Arc::new(Mutex::new(FeeOnTransferRegistry::new())),
        }
    }

    /// Bench `path_id` in the suppression ledger: `PATH_SUPPRESS_THRESHOLD`
    /// consecutive failures.
    fn bench(ledger: &Registries, path_id: u64) {
        let mut suppression = ledger.suppression.lock().unwrap();
        for _ in 0..PATH_SUPPRESS_THRESHOLD {
            suppression.record_failure(candidate_id(path_id));
        }
        assert!(
            suppression.is_in_suppressed_set(candidate_id(path_id)),
            "fixture benches the candidate"
        );
    }

    /// Would a suppression read at `block` skip this candidate — i.e. does the
    /// ledger hold a retry stamp at (or inside the interval of) that block?
    fn suppressed_at(ledger: &Registries, path_id: u64, block: u64) -> bool {
        ledger
            .suppression
            .lock()
            .unwrap()
            .is_suppressed(candidate_id(path_id), block)
    }

    fn run(
        ledger: &Registries,
        state: Option<&StateLock<BotState>>,
        block: u64,
        min_profit_margin_bps: u64,
        candidates: Vec<DispatchCandidate>,
    ) -> Admission {
        admit(
            candidates,
            &AdmissionPolicy {
                suppression: &ledger.suppression,
                divergence: &ledger.divergence,
                fee_on_transfer: &ledger.fee_on_transfer,
                bot_state: state,
                current_block: block,
                min_profit_margin_bps,
            },
        )
    }

    /// A `BotState` with each pool in `pools` registered as a V2 pair.
    fn bot_state_with(pools: &[Address]) -> BotState {
        let mut core = BotState::new();
        for pool in pools {
            core.register_v2_pool(&RegisterV2PoolParams {
                address: *pool,
                token0: WETH,
                token1: OTHER,
                reserve0: U112::from(1_000_000u64),
                reserve1: U112::from(2_000_000u64),
                fee_token0: FeeRatio::new(997, 1_000).unwrap(),
                fee_token1: FeeRatio::new(997, 1_000).unwrap(),
                factory: Address::ZERO,
                deployer: Address::ZERO,
                init_hash: B256::ZERO,
                update_block: 1,
                variant: DexVariant::UniswapV2,
                stable_swap: false,
            })
            .unwrap();
        }
        core
    }

    /// The live `state_nonce` of `pool` — the value a fresh candidate snapshots.
    fn live_nonce(core: &BotState, pool: Address) -> u64 {
        let pid = core.pool_id_by_address(&pool).unwrap();
        core.pool_state_nonce(pid)
    }

    fn path_ids(admission: &Admission) -> Vec<u64> {
        admission
            .admitted
            .iter()
            .map(|c| c.candidate_id.path_id())
            .collect()
    }

    // -- the pass itself -------------------------------------------------

    #[test]
    fn a_clean_candidate_is_admitted_unchanged() {
        let ledger = registries();
        let admission = run(
            &ledger,
            None,
            DUE_BLOCK,
            0,
            vec![candidate(7, 1_000_000, 100_000)],
        );
        assert_eq!(path_ids(&admission), vec![7]);
        assert_eq!(admission.skips, AdmissionSkips::default());
    }

    #[test]
    fn an_empty_input_admits_nothing_and_tallies_nothing() {
        let ledger = registries();
        let admission = run(&ledger, None, DUE_BLOCK, 50, Vec::new());
        assert!(admission.admitted.is_empty());
        assert_eq!(admission.skips, AdmissionSkips::default());
        assert_eq!(admission.skips.total(), 0);
    }

    #[test]
    fn a_not_due_suppressed_candidate_is_dropped_and_stamps_nothing() {
        let ledger = registries();
        bench(&ledger, 3);
        // Well inside the retry interval: `last_retry` is still the sentinel 0,
        // so the retry clock has NOT reached the interval.
        let before_due = PATH_SUPPRESS_RETRY_INTERVAL - 1;
        let admission = run(
            &ledger,
            None,
            before_due,
            0,
            vec![candidate(3, 1_000_000, 100_000)],
        );
        assert!(admission.admitted.is_empty());
        assert_eq!(admission.skips.suppressed, 1);
        // Still skipped at the block it comes due at: the dropped read stamped
        // nothing.
        assert!(suppressed_at(&ledger, 3, before_due));
    }

    // -- the retry stamp survives every later drop -----------------------

    #[test]
    fn a_retry_due_candidate_stays_stamped_when_divergence_drops_it() {
        let ledger = registries();
        bench(&ledger, 5);
        ledger
            .divergence
            .lock()
            .unwrap()
            .record_divergence(PoolDivergenceKey::V2(POOL_A), 100);
        let admission = run(
            &ledger,
            None,
            100,
            0,
            vec![candidate(5, 1_000_000, 100_000)],
        );
        assert!(admission.admitted.is_empty(), "divergence dropped the row");
        assert_eq!(admission.skips.divergent, 1);
        assert_eq!(admission.skips.suppressed, 0, "the retry was admitted");
        assert!(
            suppressed_at(&ledger, 5, 100),
            "the retry due at 100 is spent even though divergence dropped it"
        );
    }

    #[test]
    fn a_retry_due_candidate_stays_stamped_when_fee_on_transfer_drops_it() {
        let ledger = registries();
        bench(&ledger, 5);
        {
            let mut registry = ledger.fee_on_transfer.lock().unwrap();
            registry.record_suspicion(WETH, PoolDivergenceKey::V2(POOL_A), 100);
            registry.record_suspicion(WETH, PoolDivergenceKey::V2(POOL_B), 100);
        }
        let admission = run(
            &ledger,
            None,
            100,
            0,
            vec![candidate(5, 1_000_000, 100_000)],
        );
        assert!(admission.admitted.is_empty(), "FoT dropped the row");
        assert_eq!(admission.skips.fee_on_transfer, 1);
        assert_eq!(admission.skips.suppressed, 0, "the retry was admitted");
        assert!(
            suppressed_at(&ledger, 5, 100),
            "the retry due at 100 is spent even though FoT dropped it"
        );
    }

    #[test]
    fn a_retry_due_candidate_stays_stamped_when_thin_margin_drops_it() {
        let ledger = registries();
        bench(&ledger, 5);
        let admission = run(&ledger, None, 100, 50, vec![candidate(5, 1_000_000, 1)]);
        assert!(admission.admitted.is_empty(), "thin margin dropped the row");
        assert_eq!(admission.skips.thin_margin, 1);
        assert_eq!(admission.skips.suppressed, 0, "the retry was admitted");
        assert!(
            suppressed_at(&ledger, 5, 100),
            "the retry due at 100 is spent even though thin margin dropped it"
        );
    }

    #[test]
    fn a_retry_due_candidate_stays_stamped_when_staleness_drops_it() {
        let ledger = registries();
        let core = bot_state_with(&[POOL_A]);
        let stale_nonce = live_nonce(&core, POOL_A) + 1;
        let state = StateLock::new(core);
        bench(&ledger, 5);
        let admission = run(
            &ledger,
            Some(&state),
            100,
            0,
            vec![candidate_through_pool(5, POOL_A, stale_nonce)],
        );
        assert!(admission.admitted.is_empty(), "staleness dropped the row");
        assert_eq!(admission.skips.stale, 1);
        assert_eq!(admission.skips.suppressed, 0, "the retry was admitted");
        assert!(
            suppressed_at(&ledger, 5, 100),
            "the retry due at 100 is spent even though staleness dropped it"
        );
    }

    #[test]
    fn a_retry_due_candidate_stays_stamped_when_the_cap_drops_it() {
        let ledger = registries();
        bench(&ledger, 999);
        // The benched row carries the lowest profit, so the reorder (feature
        // on) also leaves it last and the cap drops it in both feature lanes.
        let cap = u64::try_from(MAX_SIMULATE_CONCURRENT).unwrap();
        let mut rows = candidates(cap, |path_id| 100_000 + u128::from(path_id));
        rows.push(candidate(999, 1_000_000, 1));
        let admission = run(&ledger, None, 100, 0, rows);
        assert_eq!(admission.admitted.len(), MAX_SIMULATE_CONCURRENT);
        assert!(
            !path_ids(&admission).contains(&999),
            "the cap dropped the benched row"
        );
        assert_eq!(
            admission.skips,
            AdmissionSkips::default(),
            "the cap is a silent drop — no per-stage tally"
        );
        assert!(
            suppressed_at(&ledger, 999, 100),
            "the retry due at 100 is spent even though the cap dropped it"
        );
    }

    // -- sequential duplicates -------------------------------------------

    #[test]
    fn the_first_retry_due_duplicate_is_stamped_and_admitted_and_the_second_is_suppressed() {
        let ledger = registries();
        bench(&ledger, 11);
        let first = candidate(11, 1_000_000, 100_000);
        let second = candidate(11, 1_000_000, 200_000);
        assert_eq!(first.candidate_id, second.candidate_id, "one identity");

        let admission = run(&ledger, None, DUE_BLOCK, 0, vec![first, second]);
        assert_eq!(
            admission.skips.suppressed, 1,
            "the stamp suppresses the second row"
        );
        assert_eq!(admission.skips.total(), 1);
        assert_eq!(admission.admitted.len(), 1);
        assert_eq!(
            admission.admitted[0].engine_profit, 100_000,
            "the FIRST row in candidate order is the admitted one"
        );

        // The stamp holds for the rest of the interval: a second pass in the
        // same block suppresses both rows.
        let again = run(
            &ledger,
            None,
            DUE_BLOCK,
            0,
            vec![
                candidate(11, 1_000_000, 100_000),
                candidate(11, 1_000_000, 200_000),
            ],
        );
        assert_eq!(again.skips.suppressed, 2);
        assert!(again.admitted.is_empty());
    }

    #[test]
    #[should_panic(expected = "suppression mutex poisoned")]
    fn a_poisoned_suppression_registry_panics_at_the_admission_read() {
        let ledger = registries();
        poison_under_guard(&ledger.suppression);
        let _ = run(
            &ledger,
            None,
            DUE_BLOCK,
            0,
            vec![candidate(1, 1_000_000, 100_000)],
        );
    }

    /// The divergence memo's poison guard: the pass reads it at its own stage,
    /// with the message the fan-out bookend has always panicked with.
    #[test]
    #[should_panic(expected = "pool_divergence mutex poisoned")]
    fn a_poisoned_divergence_registry_panics_at_its_admission_stage() {
        let ledger = registries();
        poison_under_guard(&ledger.divergence);
        let _ = run(
            &ledger,
            None,
            DUE_BLOCK,
            0,
            vec![candidate(1, 1_000_000, 100_000)],
        );
    }

    /// The FoT registry's poison guard, same contract one stage later.
    #[test]
    #[should_panic(expected = "fot_registry mutex poisoned")]
    fn a_poisoned_fee_on_transfer_registry_panics_at_its_admission_stage() {
        let ledger = registries();
        poison_under_guard(&ledger.fee_on_transfer);
        let _ = run(
            &ledger,
            None,
            DUE_BLOCK,
            0,
            vec![candidate(1, 1_000_000, 100_000)],
        );
    }

    fn poison_under_guard<T>(ledger: &Arc<Mutex<T>>) {
        let handle = Arc::clone(ledger);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = handle.lock().unwrap();
            // A panic while the guard is live is the only way a std `Mutex`
            // takes poison; `catch_unwind` keeps the unwind out of the test.
            // The panic source is a genuinely fallible parse: an unwrapped
            // `None` literal is the `unnecessary_literal_unwrap` lint, and the
            // workspace denies `panic!`.
            let _: u8 = "not a number".parse().unwrap();
        }));
        assert!(ledger.is_poisoned(), "fixture poisons the ledger");
    }

    // -- divergence: threshold and scope ---------------------------------

    #[test]
    fn divergence_drops_only_candidates_through_a_pool_flagged_inside_the_decay_window() {
        let ledger = registries();
        ledger
            .divergence
            .lock()
            .unwrap()
            .record_divergence(PoolDivergenceKey::V2(POOL_A), 100);

        // Inside the window: only the candidate through the flagged pool.
        let inside = run(
            &ledger,
            None,
            100 + POOL_DIVERGENCE_DECAY_BLOCKS - 1,
            0,
            vec![
                candidate_through_pool(1, POOL_A, 0),
                candidate_through_pool(2, POOL_B, 0),
            ],
        );
        assert_eq!(path_ids(&inside), vec![2], "scope is the flagged pool");
        assert_eq!(inside.skips.divergent, 1);
        assert_eq!(
            ledger.divergence.lock().unwrap().total_divergent_dropped(),
            1,
            "the lifetime tally is fed once per dropped candidate"
        );

        // The window closes exactly at the decay bound.
        let outside = run(
            &ledger,
            None,
            100 + POOL_DIVERGENCE_DECAY_BLOCKS,
            0,
            vec![candidate_through_pool(1, POOL_A, 0)],
        );
        assert_eq!(path_ids(&outside), vec![1], "clean history clears the flag");
        assert_eq!(outside.skips, AdmissionSkips::default());
    }

    // -- fee-on-transfer: the two-pool threshold -------------------------

    #[test]
    fn fee_on_transfer_needs_two_distinct_failing_pools_for_the_input_token() {
        let ledger = registries();
        // One suspicion is not confirmation.
        ledger.fee_on_transfer.lock().unwrap().record_suspicion(
            WETH,
            PoolDivergenceKey::V2(POOL_A),
            100,
        );
        let one = run(
            &ledger,
            None,
            100,
            0,
            vec![candidate_through_pool(1, POOL_A, 0)],
        );
        assert_eq!(path_ids(&one), vec![1], "one failing pool is not FoT");
        assert_eq!(one.skips, AdmissionSkips::default());

        // The second distinct pool confirms the token.
        ledger.fee_on_transfer.lock().unwrap().record_suspicion(
            WETH,
            PoolDivergenceKey::V2(POOL_B),
            100,
        );
        let confirmed = run(
            &ledger,
            None,
            100,
            0,
            vec![
                candidate_through_pool(1, POOL_A, 0),
                candidate_through_pool(2, POOL_B, 0),
            ],
        );
        assert!(confirmed.admitted.is_empty());
        assert_eq!(confirmed.skips.fee_on_transfer, 2);
        assert_eq!(
            ledger.fee_on_transfer.lock().unwrap().total_fot_dropped(),
            2
        );
    }

    // -- thin margin: per-row attribution --------------------------------

    #[test]
    fn thin_margin_drops_only_the_marginal_row_of_a_duplicate_id() {
        let ledger = registries();
        let thin = candidate(7, 1_000_000, 1);
        let fat = candidate(7, 1_000_000, 500_000);
        let admission = run(&ledger, None, 100, 50, vec![thin, fat]);
        assert_eq!(admission.skips.thin_margin, 1);
        assert_eq!(admission.skips.total(), 1);
        assert_eq!(admission.admitted.len(), 1);
        assert_eq!(
            admission.admitted[0].engine_profit, 500_000,
            "the row with margin survives its thin twin"
        );
    }

    // -- staleness: per-hop nonce ----------------------------------------

    #[test]
    fn staleness_drops_a_candidate_whose_hop_nonce_advanced() {
        let ledger = registries();
        let core = bot_state_with(&[POOL_A, POOL_B]);
        let fresh_a = live_nonce(&core, POOL_A);
        let fresh_b = live_nonce(&core, POOL_B);
        let state = StateLock::new(core);
        let admission = run(
            &ledger,
            Some(&state),
            100,
            0,
            vec![
                candidate_through_pool(1, POOL_A, fresh_a + 1), // advanced
                candidate_through_pool(2, POOL_B, fresh_b),     // still current
            ],
        );
        assert_eq!(path_ids(&admission), vec![2]);
        assert_eq!(admission.skips.stale, 1);
        assert_eq!(admission.skips.total(), 1);
    }

    // -- the cap and the reorder -----------------------------------------

    #[test]
    fn the_cap_truncates_the_survivors_and_keeps_no_tally() {
        let ledger = registries();
        let cap = u64::try_from(MAX_SIMULATE_CONCURRENT).unwrap();
        let admission = run(&ledger, None, 100, 0, candidates(cap + 10, |_| 100_000));
        assert_eq!(admission.admitted.len(), MAX_SIMULATE_CONCURRENT);
        assert_eq!(admission.skips, AdmissionSkips::default());
    }

    /// Feature off: the caller's pre-sort is the contract, so the cap keeps the
    /// INPUT prefix.
    #[cfg(not(feature = "order-index"))]
    #[test]
    fn the_feature_off_cap_keeps_the_input_prefix() {
        let ledger = registries();
        let cap = u64::try_from(MAX_SIMULATE_CONCURRENT).unwrap();
        let admission = run(
            &ledger,
            None,
            100,
            0,
            candidates(cap + 10, |path_id| 100_000 + u128::from(path_id)),
        );
        let expected: Vec<u64> = (1..=cap).collect();
        assert_eq!(path_ids(&admission), expected, "input prefix survives");
    }

    /// Feature on: the reorder runs before the cap, so the cap keeps the
    /// profit-reordered prefix.
    #[cfg(feature = "order-index")]
    #[test]
    fn the_feature_on_cap_keeps_the_profit_reordered_prefix() {
        let ledger = registries();
        let cap = u64::try_from(MAX_SIMULATE_CONCURRENT).unwrap();
        let over_cap = cap + 10;
        let admission = run(
            &ledger,
            None,
            100,
            0,
            candidates(over_cap, |path_id| 100_000 + u128::from(path_id)),
        );
        let expected: Vec<u64> = (over_cap - cap + 1..=over_cap).rev().collect();
        assert_eq!(
            path_ids(&admission),
            expected,
            "the profit-descending prefix survives"
        );
    }

    // -- the thin-margin floor -------------------------------------------

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
        // candidate A: opt=1_000_000, profit=4_999 -> 4_999*10000=49_990_000;
        //   opt*50=50_000_000 -> 49_990_000 < 50_000_000 -> DROPPED (< 0.5%).
        // candidate B: opt=1_000_000, profit=5_000 -> 50_000_000 >= 50_000_000 -> KEPT.
        // candidate C: opt=0, profit=any -> kept (no basis).
        let cands = vec![
            candidate(1, 1_000_000, 4_999),
            candidate(2, 1_000_000, 5_000),
            candidate(3, 0, 100),
        ];
        let (kept, dropped) = filter_thin_margin_results(cands, 50);
        assert_eq!(kept.len(), 2);
        assert_eq!(dropped, 1);
        // The dropped one is the sub-0.5% path 1.
        let kept_ids: Vec<u64> = kept.iter().map(|c| c.candidate_id.path_id()).collect();
        assert!(!kept_ids.contains(&1));
        assert!(kept_ids.contains(&2));
        assert!(kept_ids.contains(&3));
    }

    /// The SAME solver path twice — once per deployment — is two candidates.
    ///
    /// The pre-sim filters key on the candidate identity, so a filter pass can
    /// never fold two deployments' candidates at one path into one row (the
    /// collapse a path-only key produces).
    #[test]
    fn same_path_candidates_at_two_deployments_both_survive_the_pre_filters() {
        let at = |deployment: u64| DispatchCandidate {
            candidate_id: CandidateId::new(7, DeploymentId::new(deployment)),
            optimal_input: 1_000_000,
            engine_profit: 50_000,
            steps: Box::new([]),
            solve_block: 100,
            path_info: two_v2_hops(),
            prepared: None,
        };

        // A 0-bps floor keeps everything: two rows in, two rows out.
        let (kept, dropped) = filter_thin_margin_results(vec![at(1), at(2)], 0);
        assert_eq!(dropped, 0);
        assert_eq!(kept.len(), 2, "both deployments' candidates survive");
        let ids: Vec<CandidateId> = kept.iter().map(|c| c.candidate_id).collect();
        assert_eq!(
            ids,
            vec![
                CandidateId::new(7, DeploymentId::new(1)),
                CandidateId::new(7, DeploymentId::new(2)),
            ]
        );

        // A floor that drops the marginal row drops BOTH of its deployments'
        // rows (the same economics), never one because the other was seen
        // first.
        let (kept, dropped) = filter_thin_margin_results(vec![at(1), at(2)], 1_000);
        assert_eq!(dropped, 2);
        assert!(kept.is_empty());
    }

    #[test]
    fn thin_margin_empty_input_keeps() {
        let (kept, dropped) = filter_thin_margin_results(Vec::new(), 50);
        assert!(kept.is_empty());
        assert_eq!(dropped, 0);
    }

    #[test]
    fn thin_margin_uses_exact_integer_arithmetic() {
        // 99 bps. profit=990 on opt=1_000_000 -> 990*10000=9_900_000;
        //   1_000_000*99=99_000_000 -> DROPPED.
        // profit=9_900 -> 99_000_000 >= 99_000_000 -> KEPT (exactly at threshold).
        let cands = vec![candidate(1, 1_000_000, 990), candidate(2, 1_000_000, 9_900)];
        let (kept, dropped) = filter_thin_margin_results(cands, 99);
        assert_eq!(kept.len(), 1);
        assert_eq!(dropped, 1);
        assert_eq!(kept[0].candidate_id.path_id(), 2);
    }

    /// `order-index` feature: the reorder is deterministic (profit desc,
    /// id-asc tie-break) regardless of the caller's order — parity with a
    /// brute-force profit sort (ADR-024).
    #[cfg(feature = "order-index")]
    #[test]
    fn order_index_top_selection_is_deterministic() {
        let path = two_v2_hops();
        let mk = |id: u64, profit: u128| DispatchCandidate {
            candidate_id: candidate_id(id),
            optimal_input: 0,
            engine_profit: profit,
            steps: Box::new([]),
            solve_block: 100,
            path_info: path.clone(),
            prepared: None,
        };
        // Deliberately passed in a non-sorted order, with a profit tie (100).
        let mut cands = vec![mk(10, 100), mk(5, 500), mk(9, 100), mk(20, 300)];
        order_index_top_selection(&mut cands);
        let order: Vec<(u64, u128)> = cands
            .iter()
            .map(|c| (c.candidate_id.path_id(), c.engine_profit))
            .collect();
        // profit descending, then id ascending on the 100-tie.
        assert_eq!(order, vec![(5, 500), (20, 300), (9, 100), (10, 100)]);
    }
}
