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
//! # Three entries over one policy
//!
//! Both passes run the same stage implementations; only the sequence and the
//! thin-margin presence differ (thin margin reads nothing but the solver's own
//! row, so its verdict cannot move between admission and simulation, and the
//! pre-admitted leaf never sees the floor).
//!
//! Finalization never treats the caller's admission as a promise of later
//! admission: it re-reads every mutable projection at its own block, so a
//! ledger, memo, or registry that moved between admission and finalization
//! drops the candidate exactly as the raw pass would — and a retry-due
//! candidate is stamped before any final drop can happen.
//!
//! # The early projection
//!
//! [`project_early_admission`] is the CALLER-OWNED early half of the same
//! policy: the batch's assembly hands it its resolved rows and turns the drops
//! it reports into typed records, then transports the survivors. It differs
//! from [`admit`] in exactly two ways the caller owns:
//!
//! - suppression is the NON-STAMPING read ([`PathSuppression::is_suppressed_without_stamp`])
//!   over a ledger the caller holds locked, because the retry stamp is the
//!   leaf's to write at finalization time;
//! - the cap is the caller's own count, clamped to
//!   [`MAX_SIMULATE_CONCURRENT`] and applied in INPUT order, because the
//!   caller's per-lane budget is the one that survives the fan-out's reorder
//!   (see "Cap and order" below).
//!
//! It also REPORTS what it dropped (candidate + stage) instead of only
//! counting, which is what lets a caller emit one record per drop. It is not a
//! policy fork: the predicates, the partition loop, the thin-margin floor, and
//! the cap are the same functions the two authoritative passes run.
//!
//! # Cap and order
//!
//! On the authoritative passes the cap ([`MAX_SIMULATE_CONCURRENT`]) is applied
//! to the list the reorder produced: with the `order-index` feature the reorder
//! is the net-profit ranking, so the cap keeps the profit prefix; without it
//! the caller's own pre-sort is the contract and the cap keeps the input
//! prefix. Dropping a candidate at the cap is silent — no tally — and frees no
//! slot for a dropped sibling, because the cap is the LAST stage.
//!
//! The caller's early cap is the first of those two facts applied early: it
//! keeps its input prefix at its own (clamped) count, so the leaf's same-cap
//! truncation over what it transports drops nobody. Because the early cap sits
//! after divergence, fee-on-transfer, and thin margin, a candidate those
//! stages dropped FREES its slot — its clean sibling takes it. Only a candidate
//! the cap itself drops is lost to the prefix, and that drop is the one the
//! batch's record fold never sees.
//!
//! # What this module does not own
//!
//! Input enumeration and raw-row validation (the caller's), simulation, and the
//! post-simulation outcome recording (`record_success`/`record_failure` for
//! candidates that actually ran, plus the divergence/FoT feedback) belong to
//! the dispatch fan-out.

#[cfg(feature = "order-index")]
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use alloy::primitives::U256;
use degenbot_execution::{CandidateId, HopInfo};
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

/// The mutable policy state and per-block facts one FINALIZATION pass reads —
/// the pre-admitted entry's authoritative re-read.
///
/// The same three registries as [`AdmissionPolicy`], minus the thin-margin
/// floor: the floor belongs to the caller's early admission.
pub(crate) struct FinalizationPolicy<'a> {
    /// The retry-interval ledger. Read (and stamped) once per candidate.
    pub(crate) suppression: &'a Arc<Mutex<PathSuppression>>,
    /// Pools flagged `SolverCalc` within the decay window.
    pub(crate) divergence: &'a Arc<Mutex<PoolDivergence>>,
    /// Tokens confirmed fee-on-transfer.
    pub(crate) fee_on_transfer: &'a Arc<Mutex<FeeOnTransferRegistry>>,
    /// The engine's shared state owner — the staleness gate's sample source.
    /// `None` skips the gate (the empty and pre-`BotState` callers).
    pub(crate) bot_state: Option<&'a StateLock<BotState>>,
    /// The block this pass finalizes for: the retry clock and the decay window.
    pub(crate) current_block: u64,
}

impl AdmissionPolicy<'_> {
    /// The finalization half of this pass: the same registries and block,
    /// without the thin-margin floor.
    fn finalization(&self) -> FinalizationPolicy<'_> {
        FinalizationPolicy {
            suppression: self.suppression,
            divergence: self.divergence,
            fee_on_transfer: self.fee_on_transfer,
            bot_state: self.bot_state,
            current_block: self.current_block,
        }
    }
}

/// What one pass dropped, per stage, in pipeline order.
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

/// One pass' result: the candidates simulation receives, in simulation order,
/// plus the per-stage drop tally. `finalize` reports through this same type, so
/// the raw and pre-admitted entries share one outcome vocabulary and one
/// mapping onto [`crate::DispatchOutcome`].
#[derive(Debug)]
pub(crate) struct Admission {
    /// The surviving candidates, already reordered and capped.
    pub(crate) admitted: Vec<DispatchCandidate>,
    /// What each stage dropped on the way here.
    pub(crate) skips: AdmissionSkips,
}

/// A candidate a caller has ALREADY projected through its own pre-simulation
/// admission, transported to the pre-admitted fan-out entry
/// ([`crate::dispatch_admitted_across_deployments`]) for authoritative
/// finalization at simulation time.
///
/// Construction ([`Self::new`]) is the caller's claim that it applied its own
/// non-stamping admission projection; what travels is the COMPLETE
/// [`DispatchCandidate`] — identity, solve block, per-hop rows, resolved path,
/// economics, and any encoding a target-selection walk attached — so
/// finalization never needs an ID-keyed side table to piece a candidate back
/// together, and two candidates sharing one identity stay two rows in their
/// carried order.
///
/// # Admission is not a promise of later admission
///
/// Finalization re-reads every mutable projection (the suppression ledger, the
/// divergence memo, the fee-on-transfer registry, the live state nonces) at its
/// own block, so a policy registry that moved after this candidate was admitted
/// drops it exactly as the raw pass would. That is why the transport carries no
/// admission-time verdict to trust: a projection is evidence about the block it
/// read, never a licence to skip the final read.
///
/// # Building one
///
/// A direct Rust consumer that owns its own pre-simulation projection asks
/// [`project_early_admission`] for the list — it reports the drops and returns
/// the survivors already wrapped — rather than reproducing the stages. A caller
/// with a projection of its own constructs these directly, and every candidate
/// it does NOT transport is one it dropped:
///
/// ```
/// use degenbot_arbitrage::{AdmittedCandidate, DispatchCandidate};
/// # use degenbot_execution::{CandidateId, DeploymentId, PathInfo};
/// let candidate = DispatchCandidate {
///     candidate_id: CandidateId::new(7, DeploymentId::new(1)),
///     optimal_input: 1_000,
///     engine_profit: 50,
///     steps: Box::new([]),
///     solve_block: 100,
///     path_info: PathInfo::new(Vec::new()),
///     prepared: None,
/// };
/// let admitted = AdmittedCandidate::new(candidate);
/// assert_eq!(admitted.candidate().optimal_input, 1_000);
/// ```
#[derive(Debug, Clone)]
pub struct AdmittedCandidate {
    /// The complete candidate the transport carries.
    candidate: DispatchCandidate,
}

impl AdmittedCandidate {
    /// Transport `candidate` as pre-admitted: the caller has already applied
    /// its own pre-simulation projection and this candidate is ready for the
    /// authoritative final read at simulation time.
    #[must_use]
    pub fn new(candidate: DispatchCandidate) -> Self {
        Self { candidate }
    }

    /// The candidate this transport carries.
    #[must_use]
    pub fn candidate(&self) -> &DispatchCandidate {
        &self.candidate
    }

    /// The carried candidate, for the target-selection walk: an examined
    /// candidate keeps the encoding that walk composed.
    pub(crate) fn candidate_mut(&mut self) -> &mut DispatchCandidate {
        &mut self.candidate
    }

    /// Unwrap the carried candidate — finalization's input.
    pub(crate) fn into_candidate(self) -> DispatchCandidate {
        self.candidate
    }
}

/// Run the pre-simulation admission policy over `candidates` in their given
/// order — the RAW standalone pass.
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
#[must_use]
pub(crate) fn admit(
    mut candidates: Vec<DispatchCandidate>,
    policy: &AdmissionPolicy<'_>,
) -> Admission {
    let reads = policy.finalization();

    // 1. Suppression — the STAMPING read (see the module docs).
    let suppressed = take_suppressed(&mut candidates, &reads).len();

    // 2. Pool divergence.
    let divergent = take_divergent(&mut candidates, &reads).len();

    // 3. Fee-on-transfer.
    let fee_on_transfer = take_fee_on_transfer(&mut candidates, &reads).len();

    // 4. Thin margin.
    let (kept, thin) = partition_thin_margin(candidates, policy.min_profit_margin_bps);
    candidates = kept;

    // 5. Staleness.
    let stale = take_stale(&mut candidates, &reads).len();

    // 6. Reorder, then cap what the reorder produced.
    reorder_and_cap(&mut candidates);

    Admission {
        admitted: candidates,
        skips: AdmissionSkips {
            suppressed,
            divergent,
            fee_on_transfer,
            thin_margin: thin.len(),
            stale,
        },
    }
}

/// Finalize PRE-ADMITTED candidates in the order their caller transported them.
///
/// The pre-admitted pass: the caller's assembly already applied its own
/// non-stamping projection and the pre-selection staleness gate, so this pass
/// re-runs only the stages whose verdict must be authoritative at simulation
/// time, in a fixed order —
///
/// `stamping suppression -> pool divergence -> fee-on-transfer -> candidate
/// staleness -> optional order-index reorder -> same-cap truncation`
///
/// It stops short of simulation: the admitted list is what the leaf simulates.
/// `skips.thin_margin` is always `0` here — the thin floor is the caller's
/// early admission, and no defensive second pass is invented for it.
///
/// Every stage consumes the list SEQUENTIALLY in transported order, so of two
/// rows sharing one candidate id the first is stamped (and admitted, or dropped
/// by a later stage) exactly as the raw pass does, and the second is suppressed
/// by the stamp the first wrote. A retry-due candidate is stamped before any
/// final drop, so a candidate this pass drops has still spent its retry
/// interval.
///
/// # Panics
///
/// Panics if a policy mutex is poisoned — the same loud failure the raw pass
/// has.
#[must_use]
pub(crate) fn finalize(
    admitted: Vec<AdmittedCandidate>,
    policy: &FinalizationPolicy<'_>,
) -> Admission {
    let mut candidates: Vec<DispatchCandidate> = admitted
        .into_iter()
        .map(AdmittedCandidate::into_candidate)
        .collect();

    // 1. Suppression — the STAMPING read, authoritative at finalization time.
    let suppressed = take_suppressed(&mut candidates, policy).len();

    // 2. Pool divergence — a pool flagged between admission and now drops here.
    let divergent = take_divergent(&mut candidates, policy).len();

    // 3. Fee-on-transfer — same authority for the token registry.
    let fee_on_transfer = take_fee_on_transfer(&mut candidates, policy).len();

    // 4. Staleness — the live nonce is the final word, whatever admission saw.
    let stale = take_stale(&mut candidates, policy).len();

    // 5. Reorder, then the SAME cap over the order it produced. The caller's
    //    assembly already capped its lanes at this cap, so with the `order-index`
    //    feature on the truncation preserves the survivors it was handed and
    //    only the simulation order moves.
    reorder_and_cap(&mut candidates);

    Admission {
        admitted: candidates,
        // `thin_margin` is structurally zero: the pre-admitted pass has no thin
        // stage (see the `finalize` docs).
        skips: AdmissionSkips {
            suppressed,
            divergent,
            fee_on_transfer,
            thin_margin: 0,
            stale,
        },
    }
}

// -------------------------------------------------------------------------
// The caller's early projection — the same policy, applied early
// -------------------------------------------------------------------------

/// Why the early projection dropped a candidate.
///
/// The variants are the batch's [`AssemblyVerdict`] sources: one typed drop
/// reason per stage, so a caller maps reasons to its own verdict vocabulary
/// instead of re-deriving the stage from a second predicate.
///
/// [`AssemblyVerdict`]: https://docs.rs/degenbot-batch-executor
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EarlyDropReason {
    /// The retry-interval ledger holds this candidate and it is not due.
    Suppressed,
    /// A hop routes through a pool flagged `SolverCalc` inside the decay window.
    DivergentPool,
    /// A hop's input token is fee-on-transfer confirmed.
    FeeOnTransfer,
    /// The candidate's own margin is below the caller's floor.
    ThinMargin,
    /// The caller's per-lane cap left no room for this candidate.
    OverCap,
}

/// One candidate the early projection dropped: the COMPLETE candidate plus the
/// stage that dropped it, in the projection's stage-then-input order.
///
/// The candidate rides whole (identity, economics, per-hop rows, resolved path)
/// so a caller attributes the drop without an ID-keyed side table, and two rows
/// that share one identity stay two reported drops.
#[derive(Debug, Clone)]
pub struct EarlyDrop {
    candidate: DispatchCandidate,
    reason: EarlyDropReason,
}

impl EarlyDrop {
    fn at(candidate: DispatchCandidate, reason: EarlyDropReason) -> Self {
        Self { candidate, reason }
    }

    /// The dropped candidate — full provenance, exactly as it was projected.
    #[must_use]
    pub fn candidate(&self) -> &DispatchCandidate {
        &self.candidate
    }

    /// The stage that dropped it.
    #[must_use]
    pub fn reason(&self) -> EarlyDropReason {
        self.reason
    }
}

/// The policy state and per-block facts ONE early projection reads.
///
/// This is the caller-owned half of [`AdmissionPolicy`]: the same three policy
/// objects, minus the staleness gate (the caller applies that at its own
/// pre-selection position, where a stale candidate cannot retire a fresh
/// sibling) and with the retry stamp left to finalization.
///
/// [`AdmissionPolicy`] is crate-private on purpose — an external caller states
/// its early inputs here and lets this module own the pass.
pub struct EarlyAdmissionPolicy<'a> {
    /// The retry-interval ledger, already locked by the caller: the projection
    /// takes a shared reference so it can only perform the non-stamping read.
    pub suppression: &'a PathSuppression,
    /// Pools flagged `SolverCalc` within the decay window. A poisoned mutex is
    /// RECOVERED here (the caller's frame keeps running); the authoritative
    /// passes panic instead.
    pub divergence: &'a Mutex<PoolDivergence>,
    /// Tokens confirmed fee-on-transfer — same poison policy as `divergence`.
    pub fee_on_transfer: &'a Mutex<FeeOnTransferRegistry>,
    /// The block the projection reads for: the retry clock and the decay window.
    pub current_block: u64,
    /// The thin-margin floor in basis points of `optimal_input` (`0` disables).
    pub min_profit_margin_bps: u64,
    /// The caller's per-lane cap, clamped to [`MAX_SIMULATE_CONCURRENT`] and
    /// applied in INPUT order (see the projection docs).
    pub max_candidates: usize,
}

/// What one early projection produced: the pre-admitted survivors, and every
/// drop it reported.
#[derive(Debug)]
pub struct EarlyProjection {
    admitted: Vec<AdmittedCandidate>,
    drops: Vec<EarlyDrop>,
}

impl EarlyProjection {
    /// The survivors, in input order, as the pre-admitted transport.
    #[must_use]
    pub fn admitted(&self) -> &[AdmittedCandidate] {
        &self.admitted
    }

    /// The survivors, consumed — what a caller hands the pre-admitted fan-out.
    #[must_use]
    pub fn into_admitted(self) -> Vec<AdmittedCandidate> {
        self.admitted
    }

    /// Every reported drop, in stage-then-input order: suppression, divergence,
    /// fee-on-transfer, thin margin, cap.
    #[must_use]
    pub fn drops(&self) -> &[EarlyDrop] {
        &self.drops
    }
}

/// Project `candidates` through the EARLY half of the admission policy — the
/// caller-owned pass a batch's assembly runs before it transports survivors.
///
/// The order is fixed and is the batch's record order:
///
/// `non-stamping suppression -> pool divergence -> fee-on-transfer ->
/// thin margin -> caller cap`
///
/// `candidates` are expected in the caller's input order and the survivors come
/// back in that same order; the cap is applied LAST and in INPUT order, so the
/// survivors are a sub-sequence of the input, never a ranking.
///
/// # What the projection does NOT decide
///
/// Two stages stay the caller's, at the caller's position, because they are the
/// caller's facts and not this policy's:
///
/// - the PRE-SELECTION staleness gate (drop a candidate whose live hop nonce
///   advanced BEFORE target selection — an examined stale candidate is composed
///   and accepted, which retires a fresh sibling);
/// - what to do with each report (the caller's own per-drop record type).
///
/// Nothing here is a promise of later admission: the pre-admitted leaf
/// finalizes every survivor with the stamping suppression read, a fresh
/// divergence/fee-on-transfer read, and the live nonces, so a ledger, memo, or
/// registry that moved between this projection and simulation drops the
/// candidate there.
///
/// # Reported drops are records, not tallies
///
/// Every drop comes back as an [`EarlyDrop`] carrying the whole candidate and
/// the stage, which is what lets a caller emit one record per drop. Two
/// exceptions are deliberate and preserve the batch's established record fold:
///
/// - the CAP reports its drops as [`EarlyDropReason::OverCap`], and callers that
///   fold drops into records have always left cap drops out of that fold (a
///   capped candidate is drain-invisible);
/// - a thin row whose identity a SURVIVING row also carries
///   ([`EarlyDropReason::ThinMargin`]) is dropped but NOT reported: the floor
///   judges each row on its own margin, and the identity-keyed attribution that
///   produced those records never attributed a drop whose identity survived. Two
///   thin rows sharing one identity are two reported drops.
///
/// # Poisoned registries are recovered
///
/// The divergence and fee-on-transfer locks are taken with
/// `PoisonError::into_inner`, so a registry poisoned by a panicking peer task is
/// still read (the caller's frame keeps simulating). The authoritative passes
/// keep panicking loudly — an early projection is best-effort, finalization is
/// not.
///
/// Both registries are taken as ONE pair — divergence then fee-on-transfer,
/// before the suppression read and held across the whole early sequence — so no
/// concurrent writer can land between the early divergence read and the early
/// fee-on-transfer read. Only the suppression ledger may be pre-locked by the
/// caller; pre-locking either registry deadlocks.
///
/// # Migrating a caller onto this seam
///
/// A caller that used to hand raw rows to the RAW fan-out entry
/// (`crate::dispatch_across_deployments`) and let the leaf apply the whole
/// policy moves the early half here and transports the survivors to
/// `crate::dispatch_admitted_across_deployments`:
///
/// ```
/// # use std::sync::Mutex;
/// # use degenbot_arbitrage::{
/// #     project_early_admission, AdmittedCandidate, DispatchCandidate,
/// #     EarlyAdmissionPolicy, EarlyDropReason, FeeOnTransferRegistry, PoolDivergence,
/// # };
/// # use degenbot_submission::PathSuppression;
/// # fn project(
/// #     candidates: Vec<DispatchCandidate>,
/// #     suppression: &PathSuppression,
/// #     divergence: &Mutex<PoolDivergence>,
/// #     fee_on_transfer: &Mutex<FeeOnTransferRegistry>,
/// # ) -> (Vec<AdmittedCandidate>, Vec<EarlyDropReason>) {
/// let projection = project_early_admission(
///     candidates,
///     &EarlyAdmissionPolicy {
///         suppression,
///         divergence,
///         fee_on_transfer,
///         current_block: 100,
///         min_profit_margin_bps: 50,
///         max_candidates: 50,
///     },
/// );
/// let reasons = projection.drops().iter().map(|drop| drop.reason()).collect();
/// (projection.into_admitted(), reasons)
/// # }
/// ```
///
/// The passed ledger must already be locked by the caller (this function takes
/// `&PathSuppression` and can only read it), and the thin-margin floor travels
/// here — the pre-admitted leaf has no thin stage, so a caller that stops
/// passing it anywhere silently disables it.
#[must_use]
pub fn project_early_admission(
    mut candidates: Vec<DispatchCandidate>,
    policy: &EarlyAdmissionPolicy<'_>,
) -> EarlyProjection {
    let mut drops: Vec<EarlyDrop> = Vec::new();

    // Both registries are taken BEFORE the suppression read and held across the
    // whole early sequence (suppression -> divergence -> fee-on-transfer): a
    // concurrent writer must never land between the early divergence read and
    // the early fee-on-transfer read, so the pair is one critical section in
    // this fixed order, as the fan-out's own bookend pairs them.
    let mut divergence = policy
        .divergence
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut fee_on_transfer = policy
        .fee_on_transfer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // 1. Suppression — the NON-STAMPING read over the caller's locked ledger:
    //    a not-due benched candidate is dropped here AND at finalization, while
    //    a retry-due one is admitted by both and stamped only by the leaf.
    drops.extend(
        retain_dropped(&mut candidates, |c| {
            policy
                .suppression
                .is_suppressed_without_stamp(c.candidate_id, policy.current_block)
        })
        .into_iter()
        .map(|c| EarlyDrop::at(c, EarlyDropReason::Suppressed)),
    );

    // 2. Pool divergence — a poisoned memo is recovered (see the docs).
    let divergent = retain_dropped(&mut candidates, |c| {
        candidate_is_divergent(c, &divergence, policy.current_block)
    });
    for _ in &divergent {
        divergence.record_dropped();
    }
    drops.extend(
        divergent
            .into_iter()
            .map(|c| EarlyDrop::at(c, EarlyDropReason::DivergentPool)),
    );

    // 3. Fee-on-transfer — same lifetime tally, same poison recovery.
    let fot = retain_dropped(&mut candidates, |c| {
        candidate_is_fee_on_transfer(c, &fee_on_transfer, policy.current_block)
    });
    for _ in &fot {
        fee_on_transfer.record_dropped();
    }
    drops.extend(
        fot.into_iter()
            .map(|c| EarlyDrop::at(c, EarlyDropReason::FeeOnTransfer)),
    );

    // 4. Thin margin — the shared floor, attributed by IDENTITY: a thin row
    //    whose identity a survivor carries earns no report (see the docs).
    let (kept, thin) = partition_thin_margin(candidates, policy.min_profit_margin_bps);
    let kept_ids: HashSet<CandidateId> = kept.iter().map(|c| c.candidate_id).collect();
    drops.extend(
        thin.into_iter()
            .filter(|c| !kept_ids.contains(&c.candidate_id))
            .map(|c| EarlyDrop::at(c, EarlyDropReason::ThinMargin)),
    );
    candidates = kept;

    // 5. Cap — the caller's count clamped to the fan-out ceiling, in INPUT
    //    order, so the leaf's same-cap truncation over these survivors is a
    //    no-op.
    let cap = policy.max_candidates.min(MAX_SIMULATE_CONCURRENT);
    drops.extend(
        cap_tail(&mut candidates, cap)
            .into_iter()
            .map(|c| EarlyDrop::at(c, EarlyDropReason::OverCap)),
    );

    EarlyProjection {
        admitted: candidates.into_iter().map(AdmittedCandidate::new).collect(),
        drops,
    }
}

// -------------------------------------------------------------------------
// The stages — ONE implementation each, whatever order the pass runs them in
// -------------------------------------------------------------------------

/// Partition `candidates` by `drop`, leaving the survivors in place and
/// returning the dropped rows WHOLE, in input order.
///
/// The one traversal every stage of every entry runs: a pass that only counts,
/// a pass that feeds the lifetime tallies, and the early projection that
/// reports one record per drop all read the same partition, so no stage can
/// drift into a second copy of its predicate.
fn retain_dropped(
    candidates: &mut Vec<DispatchCandidate>,
    mut drop: impl FnMut(&DispatchCandidate) -> bool,
) -> Vec<DispatchCandidate> {
    let mut kept = Vec::with_capacity(candidates.len());
    let mut dropped = Vec::new();
    for candidate in candidates.drain(..) {
        if drop(&candidate) {
            dropped.push(candidate);
        } else {
            kept.push(candidate);
        }
    }
    *candidates = kept;
    dropped
}

/// Does this candidate route through a pool flagged `SolverCalc` inside the
/// decay window?
fn candidate_is_divergent(
    candidate: &DispatchCandidate,
    divergence: &PoolDivergence,
    current_block: u64,
) -> bool {
    candidate
        .path_info
        .hops
        .iter()
        .filter_map(hop_pool_key)
        .any(|key| divergence.is_divergent(key, current_block))
}

/// Is any hop's input token fee-on-transfer confirmed?
fn candidate_is_fee_on_transfer(
    candidate: &DispatchCandidate,
    registry: &FeeOnTransferRegistry,
    current_block: u64,
) -> bool {
    candidate
        .path_info
        .hops
        .iter()
        .any(|hop| registry.is_fot(hop_input_token(hop), current_block))
}

/// Take the candidates the cap leaves no room for, in input order.
///
/// The cap is a PREFIX rule: the tail is dropped, so this is `split_off` and
/// never a sort. Both the authoritative passes (over the reorder's output) and
/// the caller's early projection (over its input order) truncate here.
fn cap_tail(candidates: &mut Vec<DispatchCandidate>, cap: usize) -> Vec<DispatchCandidate> {
    if candidates.len() <= cap {
        Vec::new()
    } else {
        candidates.split_off(cap)
    }
}

/// The STAMPING suppression read (see the module docs): a retry-due benched
/// candidate is admitted AND stamped with `policy.current_block`.
#[expect(clippy::expect_used)] // the poison guard is the policy's loud-failure contract
fn take_suppressed(
    candidates: &mut Vec<DispatchCandidate>,
    policy: &FinalizationPolicy<'_>,
) -> Vec<DispatchCandidate> {
    let mut ledger = policy
        .suppression
        .lock()
        .expect("suppression mutex poisoned");
    retain_dropped(candidates, |c| {
        ledger.is_suppressed(c.candidate_id, policy.current_block)
    })
}

/// Pool divergence, plus the lifetime tally for what it dropped.
#[expect(clippy::expect_used)]
fn take_divergent(
    candidates: &mut Vec<DispatchCandidate>,
    policy: &FinalizationPolicy<'_>,
) -> Vec<DispatchCandidate> {
    // TWO sequential scopes: the partition reads under the shared guard, the
    // tally takes the mutable one afterwards, so a concurrent writer keeps its
    // own window between the read and the recording instead of being blocked
    // out of it by one long-held lock.
    let dropped = {
        let divergence = policy
            .divergence
            .lock()
            .expect("pool_divergence mutex poisoned");
        retain_dropped(candidates, |c| {
            candidate_is_divergent(c, &divergence, policy.current_block)
        })
    };
    if !dropped.is_empty() {
        let mut divergence = policy
            .divergence
            .lock()
            .expect("pool_divergence mutex poisoned");
        for _ in &dropped {
            divergence.record_dropped();
        }
    }
    dropped
}

/// Fee-on-transfer, plus the same lifetime tally.
#[expect(clippy::expect_used)]
fn take_fee_on_transfer(
    candidates: &mut Vec<DispatchCandidate>,
    policy: &FinalizationPolicy<'_>,
) -> Vec<DispatchCandidate> {
    // Same two sequential scopes as `take_divergent`.
    let dropped = {
        let registry = policy
            .fee_on_transfer
            .lock()
            .expect("fot_registry mutex poisoned");
        retain_dropped(candidates, |c| {
            candidate_is_fee_on_transfer(c, &registry, policy.current_block)
        })
    };
    if !dropped.is_empty() {
        let mut registry = policy
            .fee_on_transfer
            .lock()
            .expect("fot_registry mutex poisoned");
        for _ in &dropped {
            registry.record_dropped();
        }
    }
    dropped
}

/// Candidate staleness — the solve-time hop nonce against the live state. The
/// read guard covers this retain only, never the caller's provider I/O.
fn take_stale(
    candidates: &mut Vec<DispatchCandidate>,
    policy: &FinalizationPolicy<'_>,
) -> Vec<DispatchCandidate> {
    let Some(state) = policy.bot_state else {
        return Vec::new();
    };
    let guard = state.read_at(degenbot_substrate::state_lock::LockSite::Sim);
    retain_dropped(candidates, |c| candidate_is_stale(&guard, c))
}

/// The optional order-index reorder, then the [`MAX_SIMULATE_CONCURRENT`] cap
/// over the order it produced.
fn reorder_and_cap(candidates: &mut Vec<DispatchCandidate>) {
    #[cfg(feature = "order-index")]
    order_index_top_selection(candidates);
    // The authoritative passes report their cap drops through the caller's
    // `candidate_count` rather than per row, so the tail is discarded here.
    drop(cap_tail(candidates, MAX_SIMULATE_CONCURRENT));
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
    let (kept, dropped) = partition_thin_margin(results, min_profit_margin_bps);
    (kept, dropped.len())
}

/// [`filter_thin_margin_results`] keeping the dropped rows themselves: the
/// caller's early projection reports one record per drop, so it needs the rows
/// the count-only entry discards.
///
/// The rows come back in input order. A caller attributing them by IDENTITY
/// (the batch's record fold) must remember that two rows can share one
/// candidate id: the floor judges each row on its own margin, so a fat twin
/// leaves its thin twin's identity "kept" even though the thin row itself is
/// gone.
fn partition_thin_margin(
    results: Vec<DispatchCandidate>,
    min_profit_margin_bps: u64,
) -> (Vec<DispatchCandidate>, Vec<DispatchCandidate>) {
    if min_profit_margin_bps == 0 {
        return (results, Vec::new());
    }
    let threshold_num = U256::from(min_profit_margin_bps);
    let denom = U256::from(BPS_DENOM);
    let mut kept = Vec::with_capacity(results.len());
    let mut dropped: Vec<DispatchCandidate> = Vec::new();
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
            dropped.push(row);
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

    /// Transport raw rows the way a caller's assembly would: one
    /// `AdmittedCandidate` per row it decided to carry.
    fn transport(candidates: Vec<DispatchCandidate>) -> Vec<AdmittedCandidate> {
        candidates.into_iter().map(AdmittedCandidate::new).collect()
    }

    /// One pre-admitted finalization pass over the transported rows.
    fn run_finalize(
        ledger: &Registries,
        state: Option<&StateLock<BotState>>,
        block: u64,
        admitted: Vec<AdmittedCandidate>,
    ) -> Admission {
        finalize(
            admitted,
            &FinalizationPolicy {
                suppression: &ledger.suppression,
                divergence: &ledger.divergence,
                fee_on_transfer: &ledger.fee_on_transfer,
                bot_state: state,
                current_block: block,
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

    // -- the pre-admitted finalization pass ------------------------------

    /// A clean transported candidate reaches simulation in its transported
    /// order, with no stage claiming a drop.
    #[test]
    fn finalization_admits_a_clean_transported_candidate_unchanged() {
        let ledger = registries();
        let pass = run_finalize(
            &ledger,
            None,
            DUE_BLOCK,
            transport(vec![candidate(7, 1_000_000, 100_000)]),
        );
        assert_eq!(path_ids(&pass), vec![7]);
        assert_eq!(pass.skips, AdmissionSkips::default());
    }

    #[test]
    fn finalization_of_no_transported_candidates_tallies_nothing() {
        let ledger = registries();
        let pass = run_finalize(&ledger, None, DUE_BLOCK, Vec::new());
        assert!(pass.admitted.is_empty());
        assert_eq!(pass.skips, AdmissionSkips::default());
    }

    /// The thin-margin floor is EARLY admission's verdict: finalization has no
    /// stage for it, so a row the caller transported reaches simulation even
    /// when it would fail a 50 bps floor.
    #[test]
    fn finalization_applies_no_thin_margin_stage() {
        let ledger = registries();
        let pass = run_finalize(
            &ledger,
            None,
            100,
            transport(vec![candidate(7, 1_000_000, 1)]),
        );
        assert_eq!(path_ids(&pass), vec![7]);
        assert_eq!(pass.skips.thin_margin, 0, "the leaf has no thin stage");
    }

    /// The retry stamp lands BEFORE the final divergence read, so a candidate
    /// the final read drops has still spent its retry interval.
    #[test]
    fn finalization_stamps_a_retry_due_candidate_before_divergence_drops_it() {
        let ledger = registries();
        bench(&ledger, 5);
        ledger
            .divergence
            .lock()
            .unwrap()
            .record_divergence(PoolDivergenceKey::V2(POOL_A), 100);
        let pass = run_finalize(
            &ledger,
            None,
            100,
            transport(vec![candidate(5, 1_000_000, 100_000)]),
        );
        assert!(pass.admitted.is_empty(), "the final read dropped the row");
        assert_eq!(pass.skips.divergent, 1);
        assert_eq!(pass.skips.suppressed, 0, "the retry was admitted");
        assert_eq!(
            ledger.divergence.lock().unwrap().total_divergent_dropped(),
            1,
            "the final read feeds the same lifetime tally"
        );
        assert!(
            suppressed_at(&ledger, 5, 100),
            "the retry due at 100 is spent even though the final read dropped it"
        );
    }

    /// The same stamp-then-drop, one stage later: a token the registry
    /// confirmed between admission and finalization.
    #[test]
    fn finalization_stamps_a_retry_due_candidate_before_fee_on_transfer_drops_it() {
        let ledger = registries();
        bench(&ledger, 5);
        {
            let mut registry = ledger.fee_on_transfer.lock().unwrap();
            registry.record_suspicion(WETH, PoolDivergenceKey::V2(POOL_A), 100);
            registry.record_suspicion(WETH, PoolDivergenceKey::V2(POOL_B), 100);
        }
        let pass = run_finalize(
            &ledger,
            None,
            100,
            transport(vec![candidate(5, 1_000_000, 100_000)]),
        );
        assert!(
            pass.admitted.is_empty(),
            "the final FoT read dropped the row"
        );
        assert_eq!(pass.skips.fee_on_transfer, 1);
        assert_eq!(pass.skips.suppressed, 0, "the retry was admitted");
        assert!(
            suppressed_at(&ledger, 5, 100),
            "the retry due at 100 is spent even though the final FoT read dropped it"
        );
    }

    /// And last: the live nonce is the final word, so a state write after
    /// admission drops the candidate AFTER it has been stamped.
    #[test]
    fn finalization_stamps_a_retry_due_candidate_before_staleness_drops_it() {
        let ledger = registries();
        let core = bot_state_with(&[POOL_A]);
        let stale_nonce = live_nonce(&core, POOL_A) + 1;
        let state = StateLock::new(core);
        bench(&ledger, 5);
        let pass = run_finalize(
            &ledger,
            Some(&state),
            100,
            transport(vec![candidate_through_pool(5, POOL_A, stale_nonce)]),
        );
        assert!(
            pass.admitted.is_empty(),
            "the final staleness read dropped it"
        );
        assert_eq!(pass.skips.stale, 1);
        assert_eq!(pass.skips.suppressed, 0, "the retry was admitted");
        assert!(
            suppressed_at(&ledger, 5, 100),
            "the retry due at 100 is spent even though the final staleness read dropped it"
        );
    }

    /// A registry write BETWEEN admission and finalization is authoritative: the
    /// transported candidate was clean when it was admitted and is divergent
    /// when it is finalized.
    #[test]
    fn a_registry_write_between_admission_and_finalization_drops_the_transported_candidate() {
        let ledger = registries();
        let admitted = transport(vec![candidate(5, 1_000_000, 100_000)]);
        ledger
            .divergence
            .lock()
            .unwrap()
            .record_divergence(PoolDivergenceKey::V2(POOL_A), 100);
        let pass = run_finalize(&ledger, None, 100, admitted);
        assert!(pass.admitted.is_empty());
        assert_eq!(pass.skips.divergent, 1);
    }

    /// Two transported rows sharing one identity keep the sequential read: the
    /// first is stamped and admitted, the second is suppressed by that stamp.
    #[test]
    fn finalization_preserves_the_sequential_duplicate_read() {
        let ledger = registries();
        bench(&ledger, 11);
        let first = candidate(11, 1_000_000, 100_000);
        let second = candidate(11, 1_000_000, 200_000);
        let pass = run_finalize(&ledger, None, DUE_BLOCK, transport(vec![first, second]));
        assert_eq!(pass.skips.suppressed, 1);
        assert_eq!(pass.admitted.len(), 1);
        assert_eq!(
            pass.admitted[0].engine_profit, 100_000,
            "the FIRST transported row is the admitted one"
        );
    }

    /// The stamp is written before a later stage can drop the row, so the
    /// duplicate behind it is suppressed even when the stamped first row is
    /// dropped afterwards.
    #[test]
    fn finalization_stamps_and_suppresses_duplicates_before_a_later_stage_drops_the_first() {
        let ledger = registries();
        bench(&ledger, 11);
        ledger
            .divergence
            .lock()
            .unwrap()
            .record_divergence(PoolDivergenceKey::V2(POOL_A), 100);
        let pass = run_finalize(
            &ledger,
            None,
            100,
            transport(vec![
                candidate(11, 1_000_000, 100_000),
                candidate(11, 1_000_000, 200_000),
            ]),
        );
        assert_eq!(pass.skips.suppressed, 1, "the first row's stamp wins");
        assert_eq!(pass.skips.divergent, 1, "then the stamped row is dropped");
        assert!(pass.admitted.is_empty());
        assert!(suppressed_at(&ledger, 11, 100));
    }

    /// Feature off: finalization is a simulation-order no-op — the transported
    /// order IS the simulation order.
    #[cfg(not(feature = "order-index"))]
    #[test]
    fn finalization_keeps_the_transported_order_when_the_reorder_is_off() {
        let ledger = registries();
        let pass = run_finalize(
            &ledger,
            None,
            100,
            transport(vec![
                candidate(1, 1_000_000, 100),
                candidate(2, 1_000_000, 300),
                candidate(3, 1_000_000, 200),
            ]),
        );
        assert_eq!(path_ids(&pass), vec![1, 2, 3]);
    }

    /// Feature on: the survivors the caller already capped are ALL preserved —
    /// the same-cap truncation is a no-op — while the simulation order moves to
    /// the profit ranking.
    #[cfg(feature = "order-index")]
    #[test]
    fn finalization_reorders_the_already_capped_survivors_without_dropping_any() {
        let ledger = registries();
        let cap = u64::try_from(MAX_SIMULATE_CONCURRENT).unwrap();
        let pass = run_finalize(
            &ledger,
            None,
            100,
            transport(candidates(cap, |path_id| 100_000 + u128::from(path_id))),
        );
        assert_eq!(
            pass.admitted.len(),
            MAX_SIMULATE_CONCURRENT,
            "the caller's cap already bounded the transport"
        );
        let expected: Vec<u64> = (1..=cap).rev().collect();
        assert_eq!(
            path_ids(&pass),
            expected,
            "profit-descending simulation order"
        );
    }

    /// Feature on: an over-cap transport truncates AFTER the reorder, exactly
    /// like the raw pass, so the profit prefix survives.
    #[cfg(feature = "order-index")]
    #[test]
    fn finalization_truncates_an_over_cap_transport_to_the_profit_prefix() {
        let ledger = registries();
        let cap = u64::try_from(MAX_SIMULATE_CONCURRENT).unwrap();
        let over = cap + 10;
        let pass = run_finalize(
            &ledger,
            None,
            100,
            transport(candidates(over, |path_id| 100_000 + u128::from(path_id))),
        );
        let expected: Vec<u64> = (over - cap + 1..=over).rev().collect();
        assert_eq!(path_ids(&pass), expected);
    }

    #[cfg(not(feature = "order-index"))]
    #[test]
    fn finalization_truncates_an_over_cap_transport_to_the_input_prefix() {
        let ledger = registries();
        let cap = u64::try_from(MAX_SIMULATE_CONCURRENT).unwrap();
        let pass = run_finalize(
            &ledger,
            None,
            100,
            transport(candidates(cap + 10, |path_id| {
                100_000 + u128::from(path_id)
            })),
        );
        let expected: Vec<u64> = (1..=cap).collect();
        assert_eq!(path_ids(&pass), expected);
    }

    #[test]
    #[should_panic(expected = "suppression mutex poisoned")]
    fn a_poisoned_suppression_registry_panics_at_finalization() {
        let ledger = registries();
        poison_under_guard(&ledger.suppression);
        let _ = run_finalize(
            &ledger,
            None,
            DUE_BLOCK,
            transport(vec![candidate(1, 1_000_000, 100_000)]),
        );
    }

    // ── The caller's early projection ────────────────────────────────────

    /// One V2 hop from `token_in` to `token_out` through `pool` — the
    /// projection fixtures vary the pool and the input token independently, so a
    /// stage's drop can be attributed to that stage alone.
    fn single_v2_hop_over(pool: Address, token_in: Address, token_out: Address) -> PathInfo {
        PathInfo::new(vec![HopInfo::V2(V2HopInfo {
            pool_address: pool,
            token0_address: token_in,
            token1_address: token_out,
            fee: 30,
            zfo: true,
        })])
    }

    /// One one-hop candidate through `pool` from `token_in`, with its own margin
    /// (`optimal_input` = 1_000_000, so `profit` reads as basis points).
    fn projected_candidate(
        path_id: u64,
        pool: Address,
        token_in: Address,
        profit: u128,
    ) -> DispatchCandidate {
        DispatchCandidate {
            candidate_id: candidate_id(path_id),
            optimal_input: 1_000_000,
            engine_profit: profit,
            steps: Box::new([step(0)]),
            solve_block: 100,
            path_info: single_v2_hop_over(pool, token_in, OTHER),
            prepared: None,
        }
    }

    /// One early projection, by the caller's rules: the suppression ledger is
    /// locked by the CALLER (the projection can only read it) and the cap is the
    /// caller's own count.
    fn project(
        ledger: &Registries,
        block: u64,
        min_profit_margin_bps: u64,
        max_candidates: usize,
        candidates: Vec<DispatchCandidate>,
    ) -> EarlyProjection {
        let suppression = ledger.suppression.lock().unwrap();
        project_early_admission(
            candidates,
            &EarlyAdmissionPolicy {
                suppression: &suppression,
                divergence: &ledger.divergence,
                fee_on_transfer: &ledger.fee_on_transfer,
                current_block: block,
                min_profit_margin_bps,
                max_candidates,
            },
        )
    }

    /// The projection's survivor path ids, in transported order.
    fn projected_ids(projection: &EarlyProjection) -> Vec<u64> {
        projection
            .admitted()
            .iter()
            .map(|admitted| admitted.candidate().candidate_id.path_id())
            .collect()
    }

    /// The projection's reported reasons, in report order.
    fn projected_reasons(projection: &EarlyProjection) -> Vec<EarlyDropReason> {
        projection.drops().iter().map(EarlyDrop::reason).collect()
    }

    /// The projection's reported drop path ids, in report order.
    fn dropped_ids(projection: &EarlyProjection) -> Vec<u64> {
        projection
            .drops()
            .iter()
            .map(|drop| drop.candidate().candidate_id.path_id())
            .collect()
    }

    /// Stage order and drop provenance: one row per stage drops against its OWN
    /// row, in pipeline order, and only the clean row is transported.
    #[test]
    fn the_projection_reports_each_stage_in_pipeline_order() {
        let ledger = registries();
        bench(&ledger, 1);
        ledger
            .divergence
            .lock()
            .unwrap()
            .record_divergence(PoolDivergenceKey::V2(POOL_A), 100);
        {
            let mut registry = ledger.fee_on_transfer.lock().unwrap();
            registry.record_suspicion(WETH, PoolDivergenceKey::V2(POOL_A), 100);
            registry.record_suspicion(WETH, PoolDivergenceKey::V2(POOL_B), 100);
        }
        let rows = vec![
            projected_candidate(1, POOL_B, OTHER, 100_000), // benched, not due
            projected_candidate(2, POOL_A, OTHER, 100_000), // flagged pool
            projected_candidate(3, POOL_B, WETH, 100_000),  // confirmed token
            projected_candidate(4, POOL_B, OTHER, 100),     // 1 bps at a 50 bps floor
            projected_candidate(5, POOL_B, OTHER, 100_000), // clean
        ];
        // One block inside the retry interval (the ledger's sentinel retry is 0).
        let projection = project(&ledger, PATH_SUPPRESS_RETRY_INTERVAL - 1, 50, 50, rows);

        assert_eq!(
            projected_reasons(&projection),
            vec![
                EarlyDropReason::Suppressed,
                EarlyDropReason::DivergentPool,
                EarlyDropReason::FeeOnTransfer,
                EarlyDropReason::ThinMargin,
            ],
            "suppression, divergence, fee-on-transfer, thin margin — in that order"
        );
        assert_eq!(
            dropped_ids(&projection),
            vec![1, 2, 3, 4],
            "each report carries the row its own stage dropped"
        );
        assert_eq!(projected_ids(&projection), vec![5]);
        assert_eq!(
            ledger.divergence.lock().unwrap().total_divergent_dropped(),
            1,
            "the early projection feeds the same lifetime tally the leaf does"
        );
        assert_eq!(
            ledger.fee_on_transfer.lock().unwrap().total_fot_dropped(),
            1
        );
    }

    /// The suppression read is the NON-STAMPING projection: a retry-due
    /// candidate is admitted here and its retry stays unspent for the leaf.
    #[test]
    fn the_projection_admits_a_retry_due_candidate_without_spending_its_retry() {
        let ledger = registries();
        bench(&ledger, 9);

        let projection = project(
            &ledger,
            DUE_BLOCK,
            0,
            50,
            vec![projected_candidate(9, POOL_B, OTHER, 100_000)],
        );
        assert_eq!(projected_ids(&projection), vec![9]);
        assert!(projection.drops().is_empty());

        // One block later the projection of the ledger (no stamp, no retry
        // spent) still admits: the projection wrote no `last_retry_block`.
        let mut ledger_guard = ledger.suppression.lock().unwrap();
        assert!(
            !ledger_guard.is_suppressed_without_stamp(candidate_id(9), DUE_BLOCK + 1),
            "the early read spent no retry"
        );
        // Contrast: the STAMPING read does spend it, leaving the same block
        // suppressed one block later.
        assert!(!ledger_guard.is_suppressed(candidate_id(9), DUE_BLOCK));
        assert!(
            ledger_guard.is_suppressed_without_stamp(candidate_id(9), DUE_BLOCK + 1),
            "the leaf's stamp is what consumes the interval"
        );
    }

    /// The thin-margin duplicate quirk, preserved exactly: a thin row whose
    /// identity a SURVIVING twin carries is dropped but earns no report, because
    /// the identity-keyed attribution that emits those records never attributed a
    /// drop whose identity survived.
    #[test]
    fn a_thin_row_whose_twin_survives_is_dropped_without_a_report() {
        let ledger = registries();
        let projection = project(
            &ledger,
            100,
            50,
            50,
            vec![
                projected_candidate(7, POOL_B, OTHER, 1),      // 0 bps
                projected_candidate(7, POOL_B, OTHER, 10_000), // 100 bps
            ],
        );

        assert_eq!(projected_ids(&projection), vec![7]);
        assert_eq!(
            projection.admitted()[0].candidate().engine_profit,
            10_000,
            "the fat twin is what rides"
        );
        assert!(
            projection.drops().is_empty(),
            "the thin twin shares a kept identity, so it earns no report"
        );
    }

    /// Two thin rows on ONE identity are two reports — each row is judged on its
    /// own margin, so no survivor hides either drop.
    #[test]
    fn two_thin_duplicates_of_one_identity_are_two_reports() {
        let ledger = registries();
        let projection = project(
            &ledger,
            100,
            50,
            50,
            vec![
                projected_candidate(7, POOL_B, OTHER, 1),
                projected_candidate(7, POOL_B, OTHER, 2),
            ],
        );

        assert!(projection.admitted().is_empty());
        assert_eq!(
            projected_reasons(&projection),
            vec![EarlyDropReason::ThinMargin, EarlyDropReason::ThinMargin]
        );
        assert_eq!(dropped_ids(&projection), vec![7, 7]);
    }

    /// The cap is the caller's count clamped to the fan-out ceiling and applied
    /// LAST, in INPUT order, reporting the tail it dropped.
    #[test]
    fn the_projection_caps_in_input_order_and_reports_the_tail() {
        let ledger = registries();
        let ceiling = u64::try_from(MAX_SIMULATE_CONCURRENT).unwrap();
        let over = ceiling + 10;
        let rows: Vec<DispatchCandidate> = (1..=over)
            .map(|path_id| projected_candidate(path_id, POOL_B, OTHER, 100_000))
            .collect();

        // A caller count ABOVE the ceiling still clamps to it.
        let projection = project(&ledger, 100, 0, MAX_SIMULATE_CONCURRENT + 100, rows);
        assert_eq!(
            projected_ids(&projection),
            (1..=ceiling).collect::<Vec<u64>>(),
            "the cap keeps the input prefix"
        );
        assert_eq!(
            projected_reasons(&projection),
            vec![EarlyDropReason::OverCap; 10]
        );
        assert_eq!(
            dropped_ids(&projection),
            (ceiling + 1..=over).collect::<Vec<u64>>(),
            "the reported tail is the input-order tail"
        );
        assert_eq!(
            ledger.divergence.lock().unwrap().total_divergent_dropped(),
            0,
            "a capped drop is not a divergence drop"
        );
    }

    /// The survivors ARE the pre-admitted transport: the complete candidate
    /// rides, so the leaf needs no ID-keyed side table.
    #[test]
    fn the_projection_transports_the_complete_candidate() {
        let ledger = registries();
        let projection = project(&ledger, DUE_BLOCK, 0, 50, vec![candidate(7, 1_234, 5_678)]);

        assert_eq!(projection.admitted().len(), 1);
        let carried = projection.admitted()[0].candidate();
        assert_eq!(carried.candidate_id, candidate_id(7));
        assert_eq!(carried.optimal_input, 1_234);
        assert_eq!(carried.engine_profit, 5_678);
        assert_eq!(carried.solve_block, 100);
        assert_eq!(carried.steps.len(), 2);
        assert_eq!(carried.path_info.hops.len(), 2);
        assert!(
            carried.prepared.is_none(),
            "the target-selection walk attaches the encoding, not the projection"
        );
    }

    /// The projection RECOVERS a poisoned registry (the caller's frame keeps
    /// simulating); the authoritative passes keep their loud failure on the same
    /// state, so a poisoned ledger is never silently read as "nothing flagged".
    #[test]
    fn the_projection_recovers_poisoned_registries_the_authoritative_passes_panic_on() {
        let ledger = registries();
        ledger
            .divergence
            .lock()
            .unwrap()
            .record_divergence(PoolDivergenceKey::V2(POOL_A), 100);
        {
            let mut registry = ledger.fee_on_transfer.lock().unwrap();
            registry.record_suspicion(WETH, PoolDivergenceKey::V2(POOL_A), 100);
            registry.record_suspicion(WETH, PoolDivergenceKey::V2(POOL_B), 100);
        }
        poison_under_guard(&ledger.divergence);
        poison_under_guard(&ledger.fee_on_transfer);

        let projection = project(
            &ledger,
            100,
            0,
            50,
            vec![
                projected_candidate(1, POOL_A, OTHER, 100_000),
                projected_candidate(2, POOL_B, WETH, 100_000),
                projected_candidate(3, POOL_B, OTHER, 100_000),
            ],
        );
        assert_eq!(
            projected_reasons(&projection),
            vec![
                EarlyDropReason::DivergentPool,
                EarlyDropReason::FeeOnTransfer
            ],
            "the poisoned memos are still READ, not treated as empty"
        );
        assert_eq!(projected_ids(&projection), vec![3]);

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = run(
                &ledger,
                None,
                100,
                0,
                vec![projected_candidate(3, POOL_B, OTHER, 100_000)],
            );
        }));
        assert!(
            panicked.is_err(),
            "the raw pass keeps panicking on a poisoned memo"
        );
    }

    /// Feature off: the early projection and the RAW pass run the same policy,
    /// so they admit the same rows in the same order — the projection only
    /// REPORTS what the raw pass counts.
    #[cfg(not(feature = "order-index"))]
    #[test]
    fn the_projection_and_the_raw_pass_admit_the_same_rows() {
        let ledger = registries();
        bench(&ledger, 1);
        ledger
            .divergence
            .lock()
            .unwrap()
            .record_divergence(PoolDivergenceKey::V2(POOL_A), 100);
        {
            let mut registry = ledger.fee_on_transfer.lock().unwrap();
            registry.record_suspicion(WETH, PoolDivergenceKey::V2(POOL_A), 100);
            registry.record_suspicion(WETH, PoolDivergenceKey::V2(POOL_B), 100);
        }
        let rows = || {
            vec![
                projected_candidate(1, POOL_B, OTHER, 100_000),
                projected_candidate(2, POOL_A, OTHER, 100_000),
                projected_candidate(3, POOL_B, WETH, 100_000),
                projected_candidate(4, POOL_B, OTHER, 100),
                projected_candidate(5, POOL_B, OTHER, 100_000),
            ]
        };
        let block = PATH_SUPPRESS_RETRY_INTERVAL - 1;
        let projection = project(&ledger, block, 50, 50, rows());
        let raw = run(&ledger, None, block, 50, rows());

        assert_eq!(projected_ids(&projection), path_ids(&raw));
        assert_eq!(
            projection.drops().len(),
            raw.skips.total(),
            "the projection reports exactly the drops the raw pass counts"
        );
    }
}
