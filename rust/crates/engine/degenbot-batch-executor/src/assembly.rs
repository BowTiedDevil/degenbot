//! Stage 1 — batch assembly: raw rows + payload rows → typed verdicts +
//! sim-ready candidates.
//!
//! One home for the per-row raw-row checks both consumers applied driftedly:
//! the `PyO3` seam skipped empty-hops + payload-served silently and raised on a
//! resolve miss; the settlement bot's `plan_batch` folded the resolve miss
//! into `SkipEmptyHops` and pre-applied suppression + thin margin on top of
//! the fan-out's own re-application (a double `is_suppressed` read whose
//! retry-stamp side effect could re-suppress a path due for its interval
//! retry). Here the raw checks run once, the pre-sim policy is projected early
//! by its own owner (below) and re-checked authoritatively at simulation time,
//! and every skip emits a typed [`AssemblyVerdict`] record.
//!
//! Policy inputs stay DISTINCT typed values (task decision b): the
//! batch-local payload-served set and the cross-block `PathSuppression`
//! registry are different decisions with different feedback loops, injected
//! separately — never merged into one knob.
//!
//! # One identity, one row
//!
//! Stage 0 refuses a batch whose rows would carry one [`CandidateId`] twice, in
//! BOTH of the batch's row kinds: the rows that would be SIMULATED (raw solver
//! rows, whose identity is the row's path id bound to
//! [`AssemblyInputs::deployment_id`]) and the rows that would be SUBMITTED
//! (payload rows above the shared profit floor). Both scans run before the raw
//! checks, the path resolve, any policy-registry read or stamp, and any record
//! ([`AssemblyError::DuplicateCandidateId`]) — a refused batch produces no
//! partial side effect and can never reach the fan-out.
//!
//! Records are not rows of execution: several PROVENANCE rows for one candidate
//! (an inline sim's failure beside its profitable retry) stay legal, because a
//! record is evidence ABOUT a candidate rather than a slot in execution. The
//! rule itself, and why refusal rather than deduplication is the outcome, live
//! in `degenbot_arbitrage::identity`.
//!
//! # This stage PROJECTS the early half of the admission policy
//!
//! The batch's order is
//!
//! `raw checks -> non-stamping suppression projection -> divergence ->
//! fee-on-transfer -> thin margin -> input-order per-lane cap -> pre-selection
//! staleness -> shared target selection -> leaf finalization`
//!
//! The stages through the pre-selection staleness gate are not implemented here:
//! they are [`degenbot_arbitrage::project_early_admission`], the same
//! predicates, floor, cap, and staleness gate the authoritative passes run,
//! applied early and REPORTING each drop (candidate + stage) instead of only
//! counting it. This stage supplies what is genuinely the batch's — the resolved
//! rows, the caller's cap, the state owner, and the [`AssemblyVerdict`]/record
//! join per reported drop — and nothing else.
//!
//! Everything an early drop reports is an EARLY drop: it stays unstamped (the
//! projection is the non-stamping read), it never reaches the fan-out, and only
//! the stages a record fold carries earn their typed [`AssemblyVerdict`] — a cap
//! drop and a pre-selection stale drop stay drain-invisible. What survives is
//! handed over as [`degenbot_arbitrage::AdmittedCandidate`] — the pre-admitted
//! transport — to
//! `degenbot_arbitrage::dispatch_admitted_across_deployments`, whose leaf
//! finalizes it authoritatively at simulation time (`stamping suppression ->
//! divergence -> fee-on-transfer -> staleness -> optional reorder -> same-cap
//! truncation`).
//!
//! A projection is evidence about the block it read, never a promise: the leaf
//! re-reads every mutable registry and the live nonces rather than trusting it,
//! so a policy fact that moved between here and simulation drops the candidate
//! THERE, and the retry stamp is the leaf's to write. A candidate a stage before
//! the cap dropped FREES its slot — a clean sibling takes it — while a candidate
//! the cap itself dropped is simply gone (the drain-invisible drop), and a
//! candidate the staleness gate dropped after it keeps the slot it already
//! occupied.
//!
//! # Migrating a consumer of this stage
//!
//! [`AssembledBatch::candidates`] is that transport
//! ([`degenbot_arbitrage::AdmittedCandidate`]) rather than a plain
//! [`DispatchCandidate`] list:
//!
//! - a consumer that only drains [`AssembledBatch::outcomes`] is unaffected —
//!   the record vocabulary and the per-row verdicts keep their shapes;
//! - a consumer that read the candidate list reads `candidate()` off each entry
//!   and hands the list to
//!   `degenbot_arbitrage::dispatch_admitted_across_deployments`, never to the
//!   RAW entry (which would re-apply the whole admission policy to rows this
//!   stage already projected).
//!
//! [`AssemblyInputs`] gains one field, `bot_state`: pass the engine's shared
//! state owner to have the projection run its PRE-SELECTION staleness gate in
//! front of target selection, or `None` to skip the gate and own it yourself (the
//! pre-admitted leaf's finalization skips the same `None`).
//!
//! Everything else this module constructs keeps its shape: the
//! [`AssemblyVerdict`] set, the payload-arm helpers ([`merge_payload_results`],
//! [`join_sim_result`], [`derive_path_pools`]), and the [`AssembledBatch`]
//! record/join fields.
//!
use std::collections::{HashMap, HashSet};

use crate::record::PathInfoView;

use degenbot_arbitrage::{
    AdmittedCandidate, DispatchCandidate, FeeOnTransferRegistry, PoolDivergence,
};
use degenbot_execution::{CandidateId, DeploymentId, PathInfo};
use degenbot_submission::{PoolKey, SubmitCandidate};
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::BotState;

use crate::record::{AssemblyVerdict, BatchOutcome, SimReceipt, SimulateVerdict};
use crate::row::{PayloadFailure, PayloadRow, RawResult};

/// Resolves a path id to its registered typed hops (the engine registry on
/// the Python side; a session map on the Rust side).
pub trait PathResolver: Send + Sync {
    /// The registered path, or `None` when the id is not registered.
    fn resolve(&self, path_id: u64) -> Option<PathInfo>;
}

/// Why the assembly stage refused to produce a batch — the loud-abort rule (see
/// the resolve-miss note on [`AssemblyVerdict`]): both arms refuse the WHOLE
/// batch rather than folding the problem into a per-row skip, because neither is
/// a property of one row's economics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssemblyError {
    /// A row's per-hop lengths disagree with its resolved path's hop count, a
    /// payload belongs to another deployment, or a payload's path is not
    /// registered: the batch and the registry have diverged.
    Corrupt {
        /// The offending row's path id.
        path_id: u64,
        /// What diverged.
        detail: String,
    },
    /// Two rows for one [`CandidateId`] would both reach EXECUTION — either two
    /// raw solver rows the batch would resolve and simulate, or two payload rows
    /// that would both join a live submit row. The batch's enforcement of the
    /// identity contract ([`degenbot_arbitrage::identity`]: one identity names
    /// one live row), checked at the top of the stage for both row kinds.
    ///
    /// Records keep per-row provenance (an inline sim's failure row and its
    /// profitable retry are different evidence about ONE candidate), so this is
    /// about the rows that would execute or reach the submit lane, never about
    /// the batch's records.
    DuplicateCandidateId {
        /// The identity the rows share.
        candidate_id: CandidateId,
        /// How many rows carried it.
        occurrences: usize,
    },
}

impl AssemblyError {
    /// The path id the refusal names — the row-level handle a driver logs.
    #[must_use]
    pub fn path_id(&self) -> u64 {
        match self {
            Self::Corrupt { path_id, .. } => *path_id,
            Self::DuplicateCandidateId { candidate_id, .. } => candidate_id.path_id(),
        }
    }

    /// A batch-structure corruption at `path_id`.
    fn corrupt(path_id: u64, detail: String) -> Self {
        Self::Corrupt { path_id, detail }
    }
}

impl std::fmt::Display for AssemblyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Corrupt { path_id, detail } => {
                write!(f, "batch assembly corrupt at path {path_id}: {detail}")
            }
            Self::DuplicateCandidateId {
                candidate_id,
                occurrences,
            } => write!(
                f,
                "candidate {candidate_id} arrived on {occurrences} rows that would all execute: \
                 one identity is one live row"
            ),
        }
    }
}

impl std::error::Error for AssemblyError {}

/// The identity disagreement between an engine-born payload row and the
/// deployment whose submit lane would receive it — one home for the message
/// both payload arms raise (stage-1 assembly and the standalone merge).
fn payload_deployment_mismatch(candidate_id: CandidateId, expected: DeploymentId) -> AssemblyError {
    AssemblyError::corrupt(
        candidate_id.path_id(),
        format!(
            "payload row carries deployment {}, but this batch executes deployment {expected}",
            candidate_id.deployment_id()
        ),
    )
}

/// The identities the payload arm would join to a LIVE submit row: no failure
/// detail, and a net profit at or above the shared floor. Rows outside that set
/// (failures, below-floor sims) keep their own records and claim no submit slot,
/// so several of them for one candidate are legal provenance.
fn submit_row_identities(payloads: &[PayloadRow]) -> impl Iterator<Item = CandidateId> + '_ {
    payloads
        .iter()
        .filter(|p| p.failure.is_none() && degenbot_arbitrage::is_gas_profitable(p.net_profit))
        .map(|p| p.candidate_id)
}

/// Refuse a row set that repeats one [`CandidateId`] — the batch's enforcement
/// of the dispatch identity contract ([`degenbot_arbitrage::identity`]), shared
/// by the batch's two row kinds so the rule has one implementation.
fn refuse_duplicate_candidate_id<I>(ids: I) -> Result<(), AssemblyError>
where
    I: IntoIterator<Item = CandidateId>,
{
    match degenbot_arbitrage::duplicate_candidate_id(ids) {
        Some((candidate_id, occurrences)) => Err(AssemblyError::DuplicateCandidateId {
            candidate_id,
            occurrences,
        }),
        None => Ok(()),
    }
}

/// Refuse a raw row set that repeats one [`CandidateId`]. A raw solver row is
/// path-keyed (the solver has no deployment in view), so its identity is the
/// row's path id bound to the assembly's deployment — the same derivation
/// [`classify_raw_row`] and [`build_raw_candidate`] apply.
fn refuse_duplicate_raw_rows(
    rows: &[RawResult],
    deployment_id: DeploymentId,
) -> Result<(), AssemblyError> {
    refuse_duplicate_candidate_id(
        rows.iter()
            .map(|row| CandidateId::new(row.path_id, deployment_id)),
    )
}

/// Refuse a payload set that would attach two live submit rows to one
/// [`CandidateId`] — run BEFORE any record or submit row is emitted.
fn refuse_duplicate_submit_rows(payloads: &[PayloadRow]) -> Result<(), AssemblyError> {
    refuse_duplicate_candidate_id(submit_row_identities(payloads))
}

/// The typed verdict this stage attributes to one early-projection drop.
///
/// `None` for the two drops the record fold has never carried, both of which are
/// drain-invisible (the batch's `candidate_count` excludes them):
/// [`degenbot_arbitrage::EarlyDropReason::OverCap`] (a row the caller's cap left
/// no room for) and [`degenbot_arbitrage::EarlyDropReason::Stale`] (a row the
/// projection's PRE-SELECTION staleness gate dropped: a stale row never earned a
/// record, so nothing is emitted for it here).
fn early_drop_verdict(reason: degenbot_arbitrage::EarlyDropReason) -> Option<AssemblyVerdict> {
    use degenbot_arbitrage::EarlyDropReason;
    match reason {
        EarlyDropReason::Suppressed => Some(AssemblyVerdict::SkipSuppressed),
        EarlyDropReason::DivergentPool => Some(AssemblyVerdict::SkipDivergentPool),
        EarlyDropReason::FeeOnTransfer => Some(AssemblyVerdict::SkipFeeOnTransfer),
        EarlyDropReason::ThinMargin => Some(AssemblyVerdict::SkipThinMargin),
        EarlyDropReason::OverCap | EarlyDropReason::Stale => None,
    }
}

/// The stage-1 inputs — every policy value, injected.
pub struct AssemblyInputs<'a> {
    /// The raw solver rows.
    pub rows: &'a [RawResult],
    /// The inline-sim payload rows.
    pub payloads: &'a [PayloadRow],
    /// The path resolver.
    pub resolver: &'a dyn PathResolver,
    /// The encode options stamped onto every candidate.
    /// The batch-local payload-served set (decision b: THIS batch's already-
    /// simulated paths — an idempotency/dedup rule with no state of its own).
    pub payload_served: &'a HashSet<CandidateId>,
    /// The cross-block suppression registry (decision b: persistent feedback
    /// state). The early projection reads it WITHOUT the retry stamp, so the
    /// one stamping read stays the leaf's at simulation time.
    pub suppression: &'a mut degenbot_submission::PathSuppression,
    /// The per-pool solver-divergence memo (skip + lifetime-tally read). A
    /// poisoned mutex is recovered here; the leaf keeps panicking.
    pub divergence: &'a std::sync::Mutex<PoolDivergence>,
    /// The per-token fee-on-transfer registry (same skip + tally read, same
    /// poison recovery).
    pub fot: &'a std::sync::Mutex<FeeOnTransferRegistry>,
    /// The engine's shared state owner — the projection's PRE-SELECTION staleness
    /// gate reads it (one retain, at `LockSite::Sim`) in front of the target
    /// selection this stage's survivors reach. `None` skips the gate, and the
    /// pre-admitted leaf's own finalization skips it for the same `None`.
    pub bot_state: Option<&'a StateLock<BotState>>,
    /// The block the batch is consumed at.
    pub current_block: u64,
    /// The thin-margin floor in bps (0 disables).
    pub min_profit_margin_bps: u64,
    /// The per-batch sim cap — the plain count the driver injects. The
    /// projection clamps it to the fan-out's own `MAX_SIMULATE_CONCURRENT` and
    /// bounds each lane in INPUT order, before the leaf's optional
    /// simulation-order reorder, so the leaf's same-cap truncation only ever
    /// preserves what this cap kept.
    pub max_candidates: usize,
    /// This lane's execution target — the deployment's own `to`, which the join
    /// stamps on every payload row the lane produces. It is a DEPLOYMENT fact:
    /// one session-global executor stamp could not route a heterogeneous
    /// fan-out, so nothing above this field may assume a single target.
    pub target: alloy::primitives::Address,
    /// The session deployment's operator-assigned identity. A raw solver row is
    /// path-keyed, so this is the deployment half the assembly binds it to when
    /// the row becomes a candidate.
    pub deployment_id: DeploymentId,
}

/// The stage-1 product: the batch's records so far (assembly verdicts +
/// payload-arm simulate verdicts), the sim-ready candidates, and the
/// payload-joined submit rows.
#[derive(Debug, Default)]
pub struct AssembledBatch {
    /// The resolved `PathInfo` per sim-ready candidate id — the join map the
    /// sim leaf needs post-fan-out (the `SimResult` carries only a hop count).
    pub path_info_by_id: HashMap<u64, PathInfo>,
    /// One record per processed row, in input order (raw rows, then payload
    /// rows). Candidates that reach the sim stage get their record AFTER the
    /// fan-out (the sim leaf fills the verdicts); cap-dropped rows and
    /// pre-selection stale rows produce no record — both are drain-invisible.
    pub outcomes: Vec<BatchOutcome>,
    /// The sim-ready candidates as the PRE-ADMITTED transport, in input order,
    /// post cap and post staleness gate: each one owns the whole
    /// [`DispatchCandidate`] (identity, per-hop rows, resolved path, economics),
    /// so the fan-out's finalization never recovers a candidate from an ID-keyed
    /// side table.
    ///
    /// Every candidate this stage dropped is one it does NOT transport — and
    /// every candidate it transports may still be dropped by the leaf, which
    /// re-reads the same policy state authoritatively.
    pub candidates: Vec<AdmittedCandidate>,
    /// The payload arm's joined submit rows (gas-profitable inline sims).
    pub payload_submits: Vec<SubmitCandidate>,
}

/// Assemble one batch (stage 1). See the module docs for the policy order and
/// the resolve-miss/shape-corruption split.
///
/// # Errors
///
/// [`AssemblyError::Corrupt`] when a row's hop lengths disagree with its
/// resolved path's hop count, or a payload names a foreign deployment or an
/// unregistered path (batch corruption — the loud-abort rule), and
/// [`AssemblyError::DuplicateCandidateId`] when the batch would simulate two raw
/// rows for one candidate id, or attach two LIVE submit rows to one candidate
/// id.
///
/// # Panics
///
/// When a resolved candidate's path info is missing from the stage's join
/// map — an internal invariant (the map holds every resolved candidate).
#[expect(
    clippy::too_many_lines,
    reason = "the stage reads best as one ordered policy pass"
)]
#[expect(
    clippy::expect_used,
    reason = "the join map holds every resolved candidate — internal invariant"
)]
pub fn assemble_batch(inputs: &AssemblyInputs<'_>) -> Result<AssembledBatch, AssemblyError> {
    // 0. The identity checks run FIRST, before a raw row is classified or
    //    resolved, before a record is emitted, and before a policy registry is
    //    read or stamped, so a refused batch leaves nothing behind (see
    //    [`degenbot_arbitrage::identity`]). The rows that would be SIMULATED
    //    come first: a duplicate there would reach the fan-out as two rows one
    //    identity-keyed result map cannot tell apart.
    refuse_duplicate_raw_rows(inputs.rows, inputs.deployment_id)?;
    // Then the rows that would be SUBMITTED: one identity, one live submit row.
    refuse_duplicate_submit_rows(inputs.payloads)?;

    let mut batch = AssembledBatch::default();
    // Every raw row this session assembles targets the session deployment.
    let candidate_id = |path_id: u64| CandidateId::new(path_id, inputs.deployment_id);
    let mut path_info_by_id: HashMap<u64, PathInfo> = HashMap::new();
    // The resolved raw candidates the policy stages below project. They stay
    // plain [`DispatchCandidate`]s until the cap, which is where this stage's
    // product becomes the pre-admitted transport.
    let mut candidates: Vec<DispatchCandidate> = Vec::new();
    // The solver request row per path — the figures the inline sim was issued
    // from. A payload row carries only its verdict, so a payload FAILURE's dump
    // reads `optimal_input`/`hop_outputs` off this map: the batch's own request
    // rows, beside the payload, never the engine's result store (a retried or
    // expired candidate has already left it, which is exactly how every dump
    // came to render `optimal_input=0 hop_outputs=[]`).
    let request_by_path: HashMap<u64, &RawResult> =
        inputs.rows.iter().map(|row| (row.path_id, row)).collect();

    // ── Raw rows ─────────────────────────────────────────────────────────
    // Order: empty-hops, payload-served, resolve, shape — the PyO3 seam's
    // documented order (the skip predicates before the resolve), with the
    // resolve miss now typed instead of raised.
    for row in inputs.rows {
        // The pre-resolve skip predicates — one home shared with the PyO3
        // assembly delegate ([`classify_raw_row`]).
        match classify_raw_row(row, inputs.payload_served, inputs.deployment_id) {
            RawRowClass::Skip(verdict) => {
                batch.outcomes.push(BatchOutcome {
                    candidate_id: candidate_id(row.path_id),
                    block: inputs.current_block,
                    assembly: verdict,
                    simulate: None,
                    submit: None,
                    path_info: PathInfoView::empty(),
                });
                continue;
            }
            RawRowClass::Build => {}
        }
        let Some(path_info) = inputs.resolver.resolve(row.path_id) else {
            batch.outcomes.push(BatchOutcome {
                candidate_id: candidate_id(row.path_id),
                block: inputs.current_block,
                assembly: AssemblyVerdict::SkipResolveMiss,
                simulate: None,
                submit: None,
                path_info: PathInfoView::empty(),
            });
            continue;
        };
        // The loud-abort shape guard + the candidate construction — one home
        // shared with the PyO3 assembly delegate ([`build_raw_candidate`]).
        let candidate = build_raw_candidate(row, &path_info, inputs.deployment_id)?;
        path_info_by_id.insert(row.path_id, path_info.clone());
        candidates.push(candidate);
    }

    // ── The early admission projection ───────────────────────────────────
    // The non-stamping half of the pre-simulation policy — suppression without
    // the retry stamp, pool divergence, fee-on-transfer, thin margin, and this
    // batch's input-order cap — is ONE implementation, in
    // `degenbot_arbitrage::project_early_admission`. This stage hands it the
    // resolved rows and turns its reported drops into typed records; what
    // survives is the pre-admitted transport, and the fan-out's leaf finalizes
    // it authoritatively at simulation time.
    let projection = degenbot_arbitrage::project_early_admission(
        candidates,
        &degenbot_arbitrage::EarlyAdmissionPolicy {
            suppression: inputs.suppression,
            divergence: inputs.divergence,
            fee_on_transfer: inputs.fot,
            bot_state: inputs.bot_state,
            current_block: inputs.current_block,
            min_profit_margin_bps: inputs.min_profit_margin_bps,
            max_candidates: inputs.max_candidates,
        },
    );
    for dropped in projection.drops() {
        // The cap and the pre-selection staleness gate are the two early drops
        // the record fold never sees: the candidate_count the driver reports
        // excludes them.
        let Some(assembly) = early_drop_verdict(dropped.reason()) else {
            continue;
        };
        let candidate_id = dropped.candidate().candidate_id;
        batch.outcomes.push(BatchOutcome {
            candidate_id,
            block: inputs.current_block,
            assembly,
            simulate: None,
            submit: None,
            path_info: PathInfoView::from(
                path_info_by_id
                    .get(&candidate_id.path_id())
                    .expect("resolved above"),
            ),
        });
    }
    // The survivors ARE the pre-admitted transport: the fan-out finalizes them
    // at simulation time (re-reading the same policy state), so what travels is
    // the complete candidate.
    batch.candidates = projection.into_admitted();

    // ── Payload rows ─────────────────────────────────────────────────────
    // The already-simulated arm: join each payload to the submit-lane shape
    // and emit its record with the categorized simulate verdict. A resolve
    // miss here is decision (a)'s LOUD-ABORT arm (see
    // [`AssemblyVerdict::SkipResolveMiss`]): a payload row is engine-born —
    // the engine simulated the path inline — so a miss evidences
    // batch/registry divergence, and the pool keys could never be derived.
    for p in inputs.payloads {
        // A payload row is engine-born, so its identity is the engine's: a
        // deployment other than this batch's would carry another deployment's
        // calldata into this session's submit lane. Refuse rather than
        // re-stamp — an identity disagreement is a wiring bug, not data.
        if p.candidate_id.deployment_id() != inputs.deployment_id {
            return Err(payload_deployment_mismatch(
                p.candidate_id,
                inputs.deployment_id,
            ));
        }
        let Some(path_info) = inputs.resolver.resolve(p.candidate_id.path_id()) else {
            return Err(AssemblyError::corrupt(
                p.candidate_id.path_id(),
                format!(
                    "path_id {} is not registered in this engine; \
                     the payload pool keys cannot be derived",
                    p.candidate_id.path_id()
                ),
            ));
        };
        let view = PathInfoView::from(&path_info);
        if let Some(detail) =
            p.failure_detail(request_by_path.get(&p.candidate_id.path_id()).copied())
        {
            batch.outcomes.push(BatchOutcome {
                candidate_id: p.candidate_id,
                block: inputs.current_block,
                assembly: AssemblyVerdict::Assembled,
                simulate: Some(SimulateVerdict::Failed(Box::new(detail))),
                submit: None,
                path_info: view,
            });
            continue;
        }
        let receipt = SimReceipt {
            gross_profit: p.gross_profit,
            net_profit: p.net_profit,
            gas_used: p.gas_used,
            priority_fee: p.priority_fee,
        };
        if degenbot_arbitrage::is_gas_profitable(p.net_profit) {
            batch.payload_submits.push(join_sim_result(
                &p.sim_result(),
                Some(&path_info),
                inputs.target,
            ));
            batch.outcomes.push(BatchOutcome {
                candidate_id: p.candidate_id,
                block: inputs.current_block,
                assembly: AssemblyVerdict::Assembled,
                simulate: Some(SimulateVerdict::Profitable(receipt)),
                submit: None,
                path_info: view,
            });
        } else {
            batch.outcomes.push(BatchOutcome {
                candidate_id: p.candidate_id,
                block: inputs.current_block,
                assembly: AssemblyVerdict::Assembled,
                simulate: Some(SimulateVerdict::GasUnprofitable(receipt)),
                submit: None,
                path_info: view,
            });
        }
    }

    batch.path_info_by_id = path_info_by_id;
    Ok(batch)
}

/// Join a sim result + its originating path hops → a submit candidate (the
/// mutual-exclusion pool keys derive from the hops: V4 → the salted
/// `pool_id_hex`, V2/V3 → the EIP-55 Display form — the one keying rule both
/// entry arms share; the core owns the join and the `PyO3` seam drives it).
/// `path_info` is `None` only for a path id the fan-out could not recover
/// (an empty mutual-exclusion set — see the body).
#[must_use]
pub fn join_sim_result(
    r: &degenbot_arbitrage::SimResult,
    path_info: Option<&PathInfo>,
    target: alloy::primitives::Address,
) -> SubmitCandidate {
    // `None` = the originating path could not be recovered (the core only
    // re-emits path ids it was handed) — a degenerate but safe fall-through:
    // isolation falls back to no mutual-exclusion for that one path.
    let path_pools: HashSet<PoolKey> = path_info
        .map(|p| derive_path_pools(&p.hops))
        .unwrap_or_default();
    SubmitCandidate {
        candidate_id: r.candidate_id,
        gross_profit: r.gross_profit,
        net_profit: r.net_profit,
        gas_used: r.gas_used, // UN-inflated (1.5× applied at submit time).
        priority_fee: r.priority_fee,
        base_fee_next: r.base_fee_next,
        execute_calldata: r.execute_calldata.clone(),
        // The encoded transaction's native value travels with the calldata.
        value: r.value,
        execution_target: target,
        access_list: r.access_list.clone(),
        path_pools,
    }
}

/// Derive the mutual-exclusion pool-key set from a path's hops (V4 → the
/// `pool_id_hex` string; V2/V3 → the checksummed Display form).
#[must_use]
pub fn derive_path_pools(hops: &[degenbot_execution::HopInfo]) -> HashSet<PoolKey> {
    hops.iter()
        .map(|h| match h {
            degenbot_execution::HopInfo::V4(v4) => PoolKey::new(v4.pool_id_hex.clone()),
            degenbot_execution::HopInfo::V2(v2) => PoolKey::new(format!("{}", v2.pool_address)),
            degenbot_execution::HopInfo::V3(v3) => PoolKey::new(format!("{}", v3.pool_address)),
        })
        .collect()
}

/// The per-row classification of one raw solver row — the pre-resolve skip
/// predicates of stage 1 (one home shared by [`assemble_batch`] and the
/// `PyO3` assembly delegate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawRowClass {
    /// The row passed every pre-resolve predicate: resolve + build it.
    Build,
    /// Skip the row with this typed verdict (empty hops or payload-served).
    Skip(AssemblyVerdict),
}

/// Classify one raw solver row against the batch-local payload-served set —
/// the skip predicates that run BEFORE the path resolve.
#[must_use]
pub fn classify_raw_row<S: std::hash::BuildHasher>(
    row: &RawResult,
    payload_served: &HashSet<CandidateId, S>,
    deployment_id: DeploymentId,
) -> RawRowClass {
    if !row.has_hops() {
        return RawRowClass::Skip(AssemblyVerdict::SkipEmptyHops);
    }
    // Already simulated inline by the engine; its record comes from the
    // payload arm, not the sim batch.
    if payload_served.contains(&CandidateId::new(row.path_id, deployment_id)) {
        return RawRowClass::Skip(AssemblyVerdict::SkipPayloadServed);
    }
    RawRowClass::Build
}

/// The loud-abort shape guard + candidate construction for one RESOLVED raw
/// row — the corruption arm of decision (a): per-hop lengths that disagree
/// with the resolved path's hop count prove the batch and the registry
/// diverged, and abort the whole batch in the caller's frame. The outputs/
/// inputs messages keep the historical wording both consumers' logs pinned.
///
/// # Errors
///
/// [`AssemblyError`] when any per-hop length mismatches the resolved hop
/// count.
pub fn build_raw_candidate(
    row: &RawResult,
    path_info: &PathInfo,
    deployment_id: DeploymentId,
) -> Result<DispatchCandidate, AssemblyError> {
    let hops = path_info.hops.len();
    if row.hop_outputs.len() != hops {
        return Err(AssemblyError::corrupt(
            row.path_id,
            format!(
                "hop_outputs length ({}) != path {} hops ({hops})",
                row.hop_outputs.len(),
                row.path_id
            ),
        ));
    }
    if row.consumed_inputs.len() != hops {
        return Err(AssemblyError::corrupt(
            row.path_id,
            format!(
                "consumed_inputs length ({}) != path {} hops ({hops})",
                row.consumed_inputs.len(),
                row.path_id
            ),
        ));
    }
    if row.state_nonces.len() != hops {
        return Err(AssemblyError::corrupt(
            row.path_id,
            format!(
                "state_nonces length ({}) != path {} hops ({hops})",
                row.state_nonces.len(),
                row.path_id
            ),
        ));
    }
    Ok(row.to_candidate(path_info.clone(), deployment_id))
}

/// The payload arm's per-entry categorization verdict (the driver seam
/// renders `"submit"` / `"unprofitable"` from it and reads nothing else —
/// the threshold comparison ran crate-side, once, for both entry arms).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadArm {
    /// Net profit reached the floor — the row joined to the submit lane.
    Submit,
    /// Onchain-valid but below the net threshold.
    Unprofitable,
}

/// The payload arm's merged record set: joined submit rows + per-entry
/// verdicts + failure rows + the resolved path infos (first-appearance order
/// over DISTINCT path ids — the render source). A pure-Rust projection with
/// no Python types: the `PyO3` seam wraps it, the pure-Rust bot reads it.
#[derive(Debug, Default)]
pub struct MergedPayloadOutcome {
    /// The joined submit rows (gas-profitable inline sims), in entry order.
    pub submits: Vec<SubmitCandidate>,
    /// Valid sims below the net threshold.
    pub unprofitable_count: usize,
    /// The failure rows (candidate id + the crate failure record).
    pub failures: Vec<(CandidateId, PayloadFailure)>,
    /// Per-entry verdicts (non-failure entries, in entry order).
    pub verdicts: Vec<(CandidateId, PayloadArm)>,
    /// The resolved path infos, first-appearance over distinct path ids.
    pub path_infos: Vec<(u64, PathInfo)>,
}

/// Merge the inline-sim payload rows (stage 1's payload arm, standalone):
/// resolve each row's path, join it to the submit-lane shape through
/// [`join_sim_result`], and categorize with the core's `is_gas_profitable`
/// predicate — the ONE rule both entry arms share.
///
/// `deployment_id` is the deployment whose submit lane receives these rows,
/// and the join stamps that deployment's `target` onto every row it produces: a
/// row minted by another deployment would carry that deployment's calldata into
/// this executor's lane. The identity is therefore checked BEFORE the resolve,
/// so a foreign row is refused rather than re-stamped — the same rule
/// [`assemble_batch`] applies to its own payload arm.
///
/// Decision (a), payload arm: a resolve miss here is the LOUD-ABORT arm, not
/// the raw-row typed skip — a payload row is engine-born (the engine
/// simulated the path inline), so a miss evidences batch/registry divergence,
/// and the pool keys could never be derived.
///
/// # Errors
///
/// [`AssemblyError::Corrupt`] when a payload's `deployment_id` differs from
/// `deployment_id`, or its `path_id` is not registered (the corruption arm
/// above), and [`AssemblyError::DuplicateCandidateId`] when two payload rows
/// would attach two LIVE submit rows to one candidate id — the join never
/// silently deduplicates and never leans on the submit lane's mutual exclusion.
pub fn merge_payload_results(
    payloads: &[PayloadRow],
    resolver: &dyn PathResolver,
    target: alloy::primitives::Address,
    deployment_id: DeploymentId,
) -> Result<MergedPayloadOutcome, AssemblyError> {
    // One `CandidateId`, one live submit row — checked before the join emits
    // anything (see [`degenbot_arbitrage::identity`]).
    refuse_duplicate_submit_rows(payloads)?;

    let mut merged = MergedPayloadOutcome::default();
    let mut seen: HashSet<u64> = HashSet::with_capacity(payloads.len());
    for p in payloads {
        if p.candidate_id.deployment_id() != deployment_id {
            return Err(payload_deployment_mismatch(p.candidate_id, deployment_id));
        }
        let Some(path_info) = resolver.resolve(p.candidate_id.path_id()) else {
            return Err(AssemblyError::corrupt(
                p.candidate_id.path_id(),
                format!(
                    "path_id {} is not registered in this engine; \
                     the payload pool keys cannot be derived",
                    p.candidate_id.path_id()
                ),
            ));
        };
        // One resolve per DISTINCT path id (multiple entries may share a
        // path), in first-appearance order.
        if seen.insert(p.candidate_id.path_id()) {
            merged
                .path_infos
                .push((p.candidate_id.path_id(), path_info.clone()));
        }
        if let Some(failure) = &p.failure {
            merged.failures.push((p.candidate_id, failure.clone()));
            continue;
        }
        let joined = join_sim_result(&p.sim_result(), Some(&path_info), target);
        if degenbot_arbitrage::is_gas_profitable(p.net_profit) {
            merged.submits.push(joined);
            merged.verdicts.push((p.candidate_id, PayloadArm::Submit));
        } else {
            merged.unprofitable_count += 1;
            merged
                .verdicts
                .push((p.candidate_id, PayloadArm::Unprofitable));
        }
    }
    Ok(merged)
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-valid inputs; parse_address fixtures are valid"
)]
mod tests {
    use super::*;
    use crate::row::{PayloadFailure, PayloadRow};
    use alloy::primitives::Address;
    use degenbot_arbitrage::{
        FeeOnTransferRegistry, PoolDivergence, PoolDivergenceKey, MAX_SIMULATE_CONCURRENT,
    };
    use degenbot_core::address_utils::parse_address;
    use degenbot_execution::{HopInfo, V2HopInfo};
    use degenbot_submission::PathSuppression;
    use std::sync::{Arc, Mutex};

    const EXECUTOR: Address =
        alloy::primitives::address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    /// The fixture pools: one flaggable, one clean, and one that confirms a
    /// second fee-on-transfer suspicion for the same input token.
    const POOL_A: Address = alloy::primitives::address!("a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1");
    const POOL_B: Address = alloy::primitives::address!("b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2");
    const POOL_C: Address = alloy::primitives::address!("c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3");

    /// The fixture hop's input/output tokens. `zfo` is on, so the INPUT token —
    /// the one the fee-on-transfer registry keys a suspicion on — is `token0`.
    const TOKEN_IN: Address =
        alloy::primitives::address!("2222222222222222222222222222222222222222");
    const TOKEN_OUT: Address =
        alloy::primitives::address!("3333333333333333333333333333333333333333");

    /// A second pair's tokens — the clean sibling beside a fee-on-transfer
    /// candidate, so a confirmed token drops one row and not both.
    const OTHER_IN: Address =
        alloy::primitives::address!("4444444444444444444444444444444444444444");
    const OTHER_OUT: Address =
        alloy::primitives::address!("5555555555555555555555555555555555555555");

    /// One V2 hop through `pool` between the named tokens.
    fn v2_path_between(pool: Address, token_in: Address, token_out: Address) -> PathInfo {
        PathInfo::new(vec![HopInfo::V2(V2HopInfo {
            pool_address: pool,
            token0_address: token_in,
            token1_address: token_out,
            fee: 30,
            zfo: true,
        })])
    }

    /// One V2 hop through `pool` with the fixture's default tokens.
    fn v2_path_through(pool: Address) -> PathInfo {
        v2_path_between(pool, TOKEN_IN, TOKEN_OUT)
    }

    /// The assembly tests' default path: one hop through the fixture pool.
    fn v2_path() -> PathInfo {
        v2_path_through(parse_address("0x1111111111111111111111111111111111111111").unwrap())
    }

    struct MapResolver(std::collections::HashMap<u64, PathInfo>);
    impl PathResolver for MapResolver {
        fn resolve(&self, path_id: u64) -> Option<PathInfo> {
            self.0.get(&path_id).cloned()
        }
    }

    /// A map resolver that counts its own calls — the lever proving a refusal
    /// happened BEFORE the path resolve.
    struct CountingResolver {
        paths: std::collections::HashMap<u64, PathInfo>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl PathResolver for CountingResolver {
        fn resolve(&self, path_id: u64) -> Option<PathInfo> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.paths.get(&path_id).cloned()
        }
    }

    /// The assembly tests' one deployment identity.
    fn deployment_id() -> DeploymentId {
        DeploymentId::new(1)
    }

    fn resolver(paths: &[u64]) -> MapResolver {
        MapResolver(paths.iter().map(|&p| (p, v2_path())).collect())
    }

    /// A resolver mapping each path id to its own path fixture (one path per
    /// pool/token pair).
    fn resolver_paths(paths: &[(u64, PathInfo)]) -> MapResolver {
        MapResolver(paths.iter().cloned().collect())
    }

    /// The identities the assembly's transport carries, in order.
    fn ids(batch: &AssembledBatch) -> Vec<CandidateId> {
        batch
            .candidates
            .iter()
            .map(|admitted| admitted.candidate().candidate_id)
            .collect()
    }

    /// The stage-1 verdicts the batch emitted, in record order.
    fn verdicts(batch: &AssembledBatch) -> Vec<AssemblyVerdict> {
        batch.outcomes.iter().map(|o| o.assembly).collect()
    }

    /// Poison `ledger` the way a panicking peer task does (a guard held across
    /// an unwind is the only way a std `Mutex` takes poison).
    fn poison_under_guard<T>(ledger: &std::sync::Mutex<T>) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = ledger.lock().unwrap();
            // The panic source is a genuinely fallible parse: an unwrapped
            // `None` literal is the `unnecessary_literal_unwrap` lint.
            let _: u8 = "not a number".parse().unwrap();
        }));
        assert!(ledger.is_poisoned(), "fixture poisons the ledger");
    }

    fn row(path_id: u64, optimal_input: u128, profit: u128) -> RawResult {
        // One hop — matching the single-hop `v2_path` fixture (the shape
        // guard compares row lengths against the RESOLVED hop count).
        RawResult {
            path_id,
            optimal_input,
            profit,
            hop_outputs: vec![profit],
            consumed_inputs: vec![optimal_input],
            solve_block: 100,
            state_nonces: vec![1],
        }
    }

    struct Policy {
        suppression: Arc<Mutex<PathSuppression>>,
        divergence: Arc<Mutex<PoolDivergence>>,
        fot: Arc<Mutex<FeeOnTransferRegistry>>,
    }

    impl Policy {
        fn fresh() -> Self {
            Self {
                suppression: Arc::new(Mutex::new(PathSuppression::new())),
                divergence: Arc::new(Mutex::new(PoolDivergence::new())),
                fot: Arc::new(Mutex::new(FeeOnTransferRegistry::new())),
            }
        }
    }

    /// Assemble one batch at block 100 with no thin-margin floor and the
    /// driver's default cap — the fixture's neutral shape.
    fn assemble(
        rows: &[RawResult],
        payloads: &[PayloadRow],
        res: &dyn PathResolver,
        p: &Policy,
    ) -> Result<AssembledBatch, AssemblyError> {
        assemble_with(rows, payloads, res, p, 100, 0, 50)
    }

    /// Assemble one batch under an explicit block, floor, and cap: only the
    /// suppression ledger is pre-locked here (the stage locks the divergence and
    /// fee-on-transfer registries itself, so pre-locking them would deadlock).
    fn assemble_with(
        rows: &[RawResult],
        payloads: &[PayloadRow],
        res: &dyn PathResolver,
        p: &Policy,
        current_block: u64,
        min_profit_margin_bps: u64,
        max_candidates: usize,
    ) -> Result<AssembledBatch, AssemblyError> {
        let served: HashSet<CandidateId> = payloads.iter().map(|x| x.candidate_id).collect();
        let mut suppression = p.suppression.lock().unwrap();
        assemble_batch(&AssemblyInputs {
            rows,
            payloads,
            resolver: res,
            payload_served: &served,
            suppression: &mut suppression,
            divergence: &p.divergence,
            fot: &p.fot,
            bot_state: None,
            current_block,
            min_profit_margin_bps,
            max_candidates,
            target: EXECUTOR,
            deployment_id: deployment_id(),
        })
    }

    /// [`assemble_with`] plus the engine's state owner, for the fixtures that
    /// exercise the projection's PRE-SELECTION staleness gate.
    fn assemble_with_state(
        rows: &[RawResult],
        res: &dyn PathResolver,
        p: &Policy,
        state: &StateLock<BotState>,
    ) -> Result<AssembledBatch, AssemblyError> {
        let mut suppression = p.suppression.lock().unwrap();
        assemble_batch(&AssemblyInputs {
            rows,
            payloads: &[],
            resolver: res,
            payload_served: &HashSet::new(),
            suppression: &mut suppression,
            divergence: &p.divergence,
            fot: &p.fot,
            bot_state: Some(state),
            current_block: 100,
            min_profit_margin_bps: 0,
            max_candidates: 50,
            target: EXECUTOR,
            deployment_id: deployment_id(),
        })
    }

    /// A `BotState` holding the fixture path's pool as a V2 pair, plus that
    /// pool's live `state_nonce` (the value a fresh row snapshots).
    fn state_with_fixture_pool() -> (StateLock<BotState>, u64) {
        let pool = parse_address("0x1111111111111111111111111111111111111111").unwrap();
        let mut core = BotState::new();
        core.register_v2_pool(&degenbot_pools::RegisterV2PoolParams {
            address: pool,
            token0: TOKEN_IN,
            token1: TOKEN_OUT,
            reserve0: alloy::primitives::aliases::U112::from(1_000_000u64),
            reserve1: alloy::primitives::aliases::U112::from(2_000_000u64),
            fee_token0: degenbot_core::fee::FeeRatio::new(997, 1_000).unwrap(),
            fee_token1: degenbot_core::fee::FeeRatio::new(997, 1_000).unwrap(),
            factory: Address::ZERO,
            deployer: Address::ZERO,
            init_hash: alloy::primitives::B256::ZERO,
            update_block: 1,
            variant: degenbot_pools::DexVariant::UniswapV2,
            stable_swap: false,
        })
        .unwrap();
        let pid = core.pool_id_by_address(&pool).unwrap();
        let live = core.pool_state_nonce(pid);
        (StateLock::new(core), live)
    }

    /// Decision (a): a resolve miss is a typed skip + counted — the unification
    /// of the `PyO3` seam's `ValueError` and the bot's `SkipEmptyHops` fold.
    #[test]
    fn resolve_miss_is_a_typed_skip_not_an_abort() {
        let res = resolver(&[]);
        let p = Policy::fresh();
        let rows = [row(7, 1_000, 100)];
        let batch = assemble(&rows, &[], &res, &p).unwrap();
        assert_eq!(batch.outcomes.len(), 1);
        assert_eq!(batch.outcomes[0].assembly, AssemblyVerdict::SkipResolveMiss);
        assert!(batch.candidates.is_empty());
    }

    /// Decision (a), corruption arm: a row whose hop lengths disagree with the
    /// resolved path's hop count proves the batch and registry diverged —
    /// loud abort, re-raised in the driver's frame.
    #[test]
    fn row_shape_mismatch_is_a_loud_abort() {
        let res = resolver(&[7]);
        let p = Policy::fresh();
        let mut bad = row(7, 1_000, 100);
        bad.hop_outputs.push(999); // 3 outputs vs 1 resolved hop
        let err = assemble(&[bad], &[], &res, &p).unwrap_err();
        assert_eq!(err.path_id(), 7);
    }

    #[test]
    fn empty_hops_skip_before_any_policy() {
        let res = resolver(&[7]);
        let p = Policy::fresh();
        let mut r = row(7, 1_000, 100);
        r.hop_outputs.clear();
        r.consumed_inputs.clear();
        r.state_nonces.clear();
        let batch = assemble(&[r], &[], &res, &p).unwrap();
        assert_eq!(batch.outcomes[0].assembly, AssemblyVerdict::SkipEmptyHops);
    }

    #[test]
    fn payload_served_row_is_skipped_and_the_payload_row_carries_the_record() {
        let res = resolver(&[7]);
        let p = Policy::fresh();
        let payload = PayloadRow {
            candidate_id: CandidateId::new(7, deployment_id()),
            gross_profit: alloy::primitives::U256::from(600_u64),
            net_profit: alloy::primitives::U256::from(500_u64),
            gas_used: 300_000,
            priority_fee: 2,
            base_fee_next: 30,
            execute_calldata: alloy::primitives::Bytes::from_static(&[0xab]),
            access_list: None,
            failure: None,
            captured_swaps: Vec::new(),
        };
        let batch = assemble(&[row(7, 1_000, 100)], &[payload], &res, &p).unwrap();
        let verdicts: Vec<AssemblyVerdict> = batch.outcomes.iter().map(|o| o.assembly).collect();
        assert!(verdicts.contains(&AssemblyVerdict::SkipPayloadServed));
        // The payload arm produced the joined submit row.
        assert_eq!(batch.payload_submits.len(), 1);
        assert_eq!(batch.payload_submits[0].candidate_id.path_id(), 7);
        // Its pool keys derive from the resolved hops.
        assert!(!batch.payload_submits[0].path_pools.is_empty());
        // And its record carries the profitable receipt.
        let payload_record = batch
            .outcomes
            .iter()
            .find(|o| o.assembly == AssemblyVerdict::Assembled)
            .expect("payload record");
        assert!(matches!(
            payload_record.simulate,
            Some(SimulateVerdict::Profitable(_))
        ));
    }

    #[test]
    fn suppressed_path_skips_and_the_retry_read_happens_once() {
        let res = resolver(&[5]);
        let p = Policy::fresh();
        for _ in 0..degenbot_submission::PATH_SUPPRESS_THRESHOLD {
            p.suppression
                .lock()
                .unwrap()
                .record_failure(CandidateId::new(5, deployment_id()));
        }
        // Due for a retry at the interval boundary: the single read stamps the
        // retry and ADMITS the candidate (Python retry semantics) — a second
        // read (as the pre-filter + fan-out double-read would do) would
        // re-suppress it. The assembly must read once.
        let batch = assemble(&[row(5, 1_000, 10_000)], &[], &res, &p).unwrap();
        assert_eq!(batch.candidates.len(), 1);
        assert!(batch
            .outcomes
            .iter()
            .all(|o| o.assembly != AssemblyVerdict::SkipSuppressed));

        // A NOT-due suppressed path (inside the retry interval, block 50 <
        // last_retry 0 + interval) skips — and because the projection is
        // no-stamp, a LATER batch at the due block still admits the retry.
        // Only the suppression lock is held here (assemble_batch locks the
        // divergence/FoT registries itself — pre-locking them deadlocks).
        let served: HashSet<CandidateId> = HashSet::new();
        {
            let mut suppression = p.suppression.lock().unwrap();
            let batch = assemble_batch(&AssemblyInputs {
                rows: &[row(5, 1_000, 10_000)],
                payloads: &[],
                resolver: &res,
                payload_served: &served,
                suppression: &mut suppression,
                divergence: &p.divergence,
                fot: &p.fot,
                bot_state: None,
                current_block: 50,
                min_profit_margin_bps: 0,
                max_candidates: 50,
                target: EXECUTOR,
                deployment_id: deployment_id(),
            })
            .unwrap();
            assert_eq!(batch.outcomes[0].assembly, AssemblyVerdict::SkipSuppressed);
            assert!(batch.candidates.is_empty());
        }
        // The guard is scoped: the retry-due read at the later block admits.
        let batch = assemble(&[row(5, 1_000, 10_000)], &[], &res, &p).unwrap();
        assert_eq!(batch.candidates.len(), 1);
    }

    #[test]
    fn thin_margin_rows_skip() {
        let res = resolver(&[1, 2]);
        let p = Policy::fresh();
        // 100/1_000_000 = 1 bps → dropped at a 50 bps floor; 10_000/1_000_000
        // = 100 bps → kept.
        let rows = [row(1, 1_000_000, 100), row(2, 1_000_000, 10_000)];
        let served: HashSet<CandidateId> = HashSet::new();
        let mut suppression = p.suppression.lock().unwrap();
        let batch = assemble_batch(&AssemblyInputs {
            rows: &rows,
            payloads: &[],
            resolver: &res,
            payload_served: &served,
            suppression: &mut suppression,
            divergence: &p.divergence,
            fot: &p.fot,
            bot_state: None,
            current_block: 100,
            min_profit_margin_bps: 50,
            max_candidates: 50,
            target: EXECUTOR,
            deployment_id: deployment_id(),
        })
        .unwrap();
        let by_id: std::collections::HashMap<u64, AssemblyVerdict> = batch
            .outcomes
            .iter()
            .map(|o| (o.candidate_id.path_id(), o.assembly))
            .collect();
        assert_eq!(by_id[&1], AssemblyVerdict::SkipThinMargin);
        // The KEPT row emits no record at stage 1 (its simulate/submit
        // verdicts arrive only after the fan-out) — drain-invisible here.
        assert!(!by_id.contains_key(&2));
        assert_eq!(batch.candidates.len(), 1);
        assert_eq!(batch.candidates[0].candidate().candidate_id.path_id(), 2);
    }

    #[test]
    fn cap_truncates_silently_matching_today_s_drain_invisible_drop() {
        let res = resolver(&[1, 2, 3]);
        let p = Policy::fresh();
        let rows = [
            row(1, 1_000_000, 10_000),
            row(2, 1_000_000, 10_000),
            row(3, 1_000_000, 10_000),
        ];
        let served: HashSet<CandidateId> = HashSet::new();
        let mut suppression = p.suppression.lock().unwrap();
        let batch = assemble_batch(&AssemblyInputs {
            rows: &rows,
            payloads: &[],
            resolver: &res,
            payload_served: &served,
            suppression: &mut suppression,
            divergence: &p.divergence,
            fot: &p.fot,
            bot_state: None,
            current_block: 100,
            min_profit_margin_bps: 0,
            max_candidates: 2,
            target: EXECUTOR,
            deployment_id: deployment_id(),
        })
        .unwrap();
        assert_eq!(batch.candidates.len(), 2);
        // Rows the cap dropped produce NO record — the same drain-invisible
        // drop today's truncate applies (candidate_count excludes them).
        assert!(batch.outcomes.is_empty());
    }

    // -- the pre-admitted transport --------------------------------------

    /// An empty batch transports nothing, records nothing, and resolves nothing
    /// — the empty-input short-circuit the fan-out then sees as zero candidates.
    #[test]
    fn an_empty_batch_transports_nothing_and_records_nothing() {
        let res = resolver(&[]);
        let p = Policy::fresh();
        let batch = assemble(&[], &[], &res, &p).unwrap();

        assert!(batch.candidates.is_empty());
        assert!(batch.outcomes.is_empty());
        assert!(batch.payload_submits.is_empty());
        assert!(batch.path_info_by_id.is_empty());
    }

    /// The survivors ARE the transport: each rides an `AdmittedCandidate` that
    /// owns the complete row (identity, economics, per-hop rows, resolved path),
    /// so the fan-out's finalization re-reads the policy without recovering
    /// anything from an ID-keyed side table.
    #[test]
    fn survivors_ride_the_pre_admitted_transport_with_full_provenance() {
        let res = resolver(&[7]);
        let p = Policy::fresh();
        let rows = [row(7, 1_234, 5_678)];
        let batch = assemble(&rows, &[], &res, &p).unwrap();

        assert_eq!(batch.candidates.len(), 1);
        let carried = batch.candidates[0].candidate();
        assert_eq!(carried.candidate_id, CandidateId::new(7, deployment_id()));
        assert_eq!(carried.optimal_input, 1_234);
        assert_eq!(carried.engine_profit, 5_678);
        assert_eq!(carried.solve_block, 100);
        assert_eq!(carried.steps.len(), 1);
        assert_eq!(carried.steps[0].state_nonce, 1);
        let view = PathInfoView::from(&carried.path_info);
        assert_eq!(view.path_type, "V2");
        assert_eq!(view.hops.len(), 1);
        assert!(
            matches!(
                &carried.path_info.hops[0],
                degenbot_execution::HopInfo::V2(hop)
                    if hop.pool_address
                        == parse_address("0x1111111111111111111111111111111111111111").unwrap()
            ),
            "the carried path is the resolved fixture path"
        );
        assert!(
            carried.prepared.is_none(),
            "the target-selection walk attaches the encoding, not the assembly"
        );
    }

    /// The batch's EXECUTION half of the identity contract: two raw solver rows
    /// carrying one [`CandidateId`] are refused at stage 0 — before the raw
    /// checks, the path resolve, the projection's policy-registry reads, and any
    /// record — so a duplicate row can never be simulated, never collapse in an
    /// identity-keyed result map, and never reach the fan-out as two rows.
    ///
    /// This also retires the thin-margin sibling quirk this stage could exhibit
    /// over a duplicate set (a thin row whose identity a fat twin also carried
    /// earned no record): no duplicate row reaches the projection, so the quirk
    /// is not an input this boundary can be handed. The projection's own
    /// sequential contract over a HAND-BUILT duplicate set is pinned in
    /// `degenbot-arbitrage`'s admission tests.
    #[test]
    fn raw_duplicate_rows_are_refused_before_resolution_and_registries() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let res = CountingResolver {
            paths: [(7, v2_path_through(POOL_A))].into_iter().collect(),
            calls: Arc::clone(&calls),
        };
        let p = Policy::fresh();
        // A divergent pool the projection would drop AND tally if it ran at all.
        p.divergence
            .lock()
            .unwrap()
            .record_divergence(PoolDivergenceKey::V2(POOL_A), 100);
        // One thin row (1 bps) and one fat (100 bps) at ONE identity, under a
        // 50 bps floor: had both reached the projection, the fat twin would have
        // survived carrying the thin twin's identity.
        let rows = [row(7, 1_000_000, 1), row(7, 1_000_000, 10_000)];

        let err = assemble_with(&rows, &[], &res, &p, 100, 50, 50).unwrap_err();
        assert_eq!(
            err,
            AssemblyError::DuplicateCandidateId {
                candidate_id: CandidateId::new(7, deployment_id()),
                occurrences: 2,
            }
        );
        assert_eq!(err.path_id(), 7);
        assert!(
            err.to_string().contains("one identity is one live row"),
            "the refusal states the rule: {err}"
        );
        // Nothing ran and nothing was written: no path was resolved, no
        // divergence or fee-on-transfer tally was recorded, no suppression was
        // read — and the stage produced no batch at all, so no record was
        // emitted and no candidate could reach the fan-out.
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the refusal precedes the resolve"
        );
        assert_eq!(p.divergence.lock().unwrap().total_divergent_dropped(), 0);
        assert_eq!(p.fot.lock().unwrap().total_fot_dropped(), 0);
        assert_eq!(p.suppression.lock().unwrap().total_suppressed(), 0);
    }

    /// The identity check is an INPUT judgement, never a policy verdict: a
    /// retry-due benched identity arriving twice is refused exactly as a healthy
    /// pair is, and the refusal spends nothing — the stamping read never ran, so
    /// the retry interval is still due. ONE row for that same identity still
    /// assembles and is admitted by the projection's single non-stamping read.
    #[test]
    fn a_retry_due_duplicate_is_refused_and_a_single_row_still_admits() {
        let res = resolver(&[5]);
        let p = Policy::fresh();
        let candidate_id = CandidateId::new(5, deployment_id());
        let due = degenbot_submission::PATH_SUPPRESS_RETRY_INTERVAL;
        for _ in 0..degenbot_submission::PATH_SUPPRESS_THRESHOLD {
            p.suppression.lock().unwrap().record_failure(candidate_id);
        }

        let err = assemble_with(
            &[row(5, 1_000, 10_000), row(5, 1_000, 10_000)],
            &[],
            &res,
            &p,
            due,
            0,
            50,
        )
        .unwrap_err();
        assert_eq!(
            err,
            AssemblyError::DuplicateCandidateId {
                candidate_id,
                occurrences: 2,
            }
        );
        assert!(
            !p.suppression
                .lock()
                .unwrap()
                .is_suppressed_without_stamp(candidate_id, due),
            "the refusal precedes the suppression read, so the retry is unspent"
        );

        let batch = assemble_with(&[row(5, 1_000, 10_000)], &[], &res, &p, due, 0, 50).unwrap();
        assert_eq!(ids(&batch), vec![candidate_id]);
        assert!(batch
            .outcomes
            .iter()
            .all(|o| o.assembly != AssemblyVerdict::SkipSuppressed));
    }

    /// The PRE-SELECTION staleness gate is the projection's, so this stage runs
    /// it: a row whose hop nonce turned over is not transported and earns NO
    /// record (the drain-invisible stale drop), while its fresh sibling rides.
    #[test]
    fn a_stale_row_is_not_transported_and_earns_no_record() {
        let res = resolver(&[7, 8]);
        let p = Policy::fresh();
        let (owner, live) = state_with_fixture_pool();
        let advanced = RawResult {
            state_nonces: vec![live + 1],
            ..row(7, 1_000, 10_000)
        };
        let current = RawResult {
            state_nonces: vec![live],
            ..row(8, 1_000, 10_000)
        };

        let batch = assemble_with_state(&[advanced, current], &res, &p, &owner).unwrap();

        assert_eq!(
            ids(&batch),
            vec![CandidateId::new(8, deployment_id())],
            "only the fresh row rides"
        );
        assert!(
            batch.outcomes.is_empty(),
            "a stale row is drain-invisible — no record"
        );
    }

    /// Cap 1, the fan-out's order: the floor runs BEFORE the cap, so the thin
    /// row's early drop FREES the one slot — the fat sibling takes it. Were the
    /// cap first, the prefix would keep the thin row and starve the fat one.
    #[test]
    fn the_floor_runs_before_the_cap_so_a_thin_row_frees_its_slot() {
        let res = resolver(&[1, 2]);
        let p = Policy::fresh();
        let rows = [row(1, 1_000_000, 1), row(2, 1_000_000, 10_000)];
        let batch = assemble_with(&rows, &[], &res, &p, 100, 50, 1).unwrap();

        assert_eq!(ids(&batch), vec![CandidateId::new(2, deployment_id())]);
        assert_eq!(verdicts(&batch), vec![AssemblyVerdict::SkipThinMargin]);
    }

    /// Cap 1: divergence runs before the cap, so the divergent row's early drop
    /// FREES the slot its clean sibling takes, and the drop both emits its typed
    /// record and feeds the memo's lifetime tally.
    #[test]
    fn the_cap_runs_after_divergence_so_a_divergent_row_frees_its_slot() {
        let res = resolver_paths(&[(1, v2_path_through(POOL_A)), (2, v2_path_through(POOL_B))]);
        let p = Policy::fresh();
        p.divergence
            .lock()
            .unwrap()
            .record_divergence(PoolDivergenceKey::V2(POOL_A), 100);
        let rows = [row(1, 1_000_000, 10_000), row(2, 1_000_000, 10_000)];
        let batch = assemble_with(&rows, &[], &res, &p, 100, 0, 1).unwrap();

        assert_eq!(ids(&batch), vec![CandidateId::new(2, deployment_id())]);
        assert_eq!(verdicts(&batch), vec![AssemblyVerdict::SkipDivergentPool]);
        assert_eq!(
            p.divergence.lock().unwrap().total_divergent_dropped(),
            1,
            "the assembly stage feeds the same lifetime tally the leaf does"
        );
    }

    /// Cap 1: fee-on-transfer runs before the cap too, so the confirmed row's
    /// early drop FREES the slot its clean sibling takes. The sibling routes
    /// through another input token, so only the confirmed token's row drops.
    #[test]
    fn the_cap_runs_after_fee_on_transfer_so_a_confirmed_token_frees_its_slot() {
        let res = resolver_paths(&[
            (1, v2_path_between(POOL_A, TOKEN_IN, TOKEN_OUT)),
            (2, v2_path_between(POOL_B, OTHER_IN, OTHER_OUT)),
        ]);
        let p = Policy::fresh();
        {
            // One suspicion is not confirmation; the second distinct pool is.
            let mut fot = p.fot.lock().unwrap();
            fot.record_suspicion(TOKEN_IN, PoolDivergenceKey::V2(POOL_A), 100);
            fot.record_suspicion(TOKEN_IN, PoolDivergenceKey::V2(POOL_C), 100);
        }
        let rows = [row(1, 1_000_000, 10_000), row(2, 1_000_000, 10_000)];
        let batch = assemble_with(&rows, &[], &res, &p, 100, 0, 1).unwrap();

        assert_eq!(ids(&batch), vec![CandidateId::new(2, deployment_id())]);
        assert_eq!(verdicts(&batch), vec![AssemblyVerdict::SkipFeeOnTransfer]);
        assert_eq!(
            p.fot.lock().unwrap().total_fot_dropped(),
            1,
            "the assembly stage feeds the same lifetime tally the leaf does"
        );
    }

    /// The driver's count is clamped to the fan-out ceiling, and the cap is
    /// INPUT ORDER: the prefix survives whatever the profits are. With the
    /// `order-index` feature on the leaf's reorder only moves the simulation
    /// order — its same-cap truncation keeps every transported survivor.
    #[test]
    fn over_cap_rows_are_dropped_in_input_order_at_the_fan_out_ceiling() {
        let ceiling = u64::try_from(MAX_SIMULATE_CONCURRENT).unwrap();
        let over = ceiling + 10;
        let path_ids: Vec<u64> = (1..=over).collect();
        let res = resolver(&path_ids);
        let p = Policy::fresh();
        // Profit FALLS with the input position: a profit ranking would invert
        // the surviving prefix.
        let rows: Vec<RawResult> = path_ids
            .iter()
            .map(|&id| row(id, 1_000_000, u128::from(over - id + 1) * 1_000))
            .collect();
        let batch =
            assemble_with(&rows, &[], &res, &p, 100, 0, MAX_SIMULATE_CONCURRENT + 100).unwrap();

        let expected: Vec<CandidateId> = (1..=ceiling)
            .map(|id| CandidateId::new(id, deployment_id()))
            .collect();
        assert_eq!(ids(&batch), expected, "the input prefix survives");
        assert!(batch.outcomes.is_empty(), "cap-dropped rows hold no record");
    }

    /// Batch assembly RECOVERS a poisoned policy registry (the driver's frame
    /// keeps running); the fan-out leaves keep panicking loudly, so a poisoned
    /// ledger is never silently read as "nothing flagged".
    #[test]
    fn assembly_recovers_a_poisoned_divergence_registry() {
        let res = resolver_paths(&[(1, v2_path_through(POOL_A)), (2, v2_path_through(POOL_B))]);
        let p = Policy::fresh();
        p.divergence
            .lock()
            .unwrap()
            .record_divergence(PoolDivergenceKey::V2(POOL_A), 100);
        poison_under_guard(&p.divergence);

        let rows = [row(1, 1_000_000, 10_000), row(2, 1_000_000, 10_000)];
        let batch = assemble(&rows, &[], &res, &p).unwrap();

        assert_eq!(ids(&batch), vec![CandidateId::new(2, deployment_id())]);
        assert_eq!(verdicts(&batch), vec![AssemblyVerdict::SkipDivergentPool]);
    }

    #[test]
    fn assembly_recovers_a_poisoned_fee_on_transfer_registry() {
        let res = resolver_paths(&[
            (1, v2_path_between(POOL_A, TOKEN_IN, TOKEN_OUT)),
            (2, v2_path_between(POOL_B, OTHER_IN, OTHER_OUT)),
        ]);
        let p = Policy::fresh();
        {
            let mut fot = p.fot.lock().unwrap();
            fot.record_suspicion(TOKEN_IN, PoolDivergenceKey::V2(POOL_A), 100);
            fot.record_suspicion(TOKEN_IN, PoolDivergenceKey::V2(POOL_C), 100);
        }
        poison_under_guard(&p.fot);

        let rows = [row(1, 1_000_000, 10_000), row(2, 1_000_000, 10_000)];
        let batch = assemble(&rows, &[], &res, &p).unwrap();

        assert_eq!(ids(&batch), vec![CandidateId::new(2, deployment_id())]);
        assert_eq!(verdicts(&batch), vec![AssemblyVerdict::SkipFeeOnTransfer]);
    }

    /// A payload row is ENGINE-BORN: it carries the identity of the deployment
    /// that simulated it. A row whose deployment is not this batch's would send
    /// another deployment's calldata down this session's submit lane, so the
    /// assembly refuses it instead of re-stamping the identity.
    #[test]
    fn a_payload_from_another_deployment_is_refused() {
        let res = resolver(&[7]);
        let p = Policy::fresh();
        let payload = PayloadRow {
            candidate_id: CandidateId::new(7, DeploymentId::new(2)),
            gross_profit: alloy::primitives::U256::from(600_u64),
            net_profit: alloy::primitives::U256::from(500_u64),
            gas_used: 300_000,
            priority_fee: 2,
            base_fee_next: 30,
            execute_calldata: alloy::primitives::Bytes::from_static(&[0xab]),
            access_list: None,
            failure: None,
            captured_swaps: Vec::new(),
        };

        let err = assemble(&[], &[payload], &res, &p).unwrap_err();
        assert_eq!(err.path_id(), 7);
        let detail = err.to_string();
        assert!(
            detail.contains("deployment 2") && detail.contains("deployment 1"),
            "the refusal names both deployments: {detail}"
        );
    }

    /// Two payload rows for the SAME path at the same deployment stay two
    /// rows: identity keys the payload arm, so neither dedup nor attribution
    /// folds them.
    #[test]
    fn same_path_payload_rows_keep_their_composite_identity() {
        let res = resolver(&[7]);
        let p = Policy::fresh();
        let row = |net: u64| PayloadRow {
            candidate_id: CandidateId::new(7, deployment_id()),
            gross_profit: alloy::primitives::U256::from(net + 100),
            net_profit: alloy::primitives::U256::from(net),
            gas_used: 300_000,
            priority_fee: 2,
            base_fee_next: 30,
            execute_calldata: alloy::primitives::Bytes::from_static(&[0xab]),
            access_list: None,
            failure: None,
            captured_swaps: Vec::new(),
        };

        let batch = assemble(&[], &[row(500), row(0)], &res, &p).unwrap();
        assert_eq!(batch.outcomes.len(), 2);
        for outcome in &batch.outcomes {
            assert_eq!(
                outcome.candidate_id,
                CandidateId::new(7, deployment_id()),
                "the record exposes the composite identity"
            );
        }
        // One profitable, one below the floor — the identity rides each row.
        assert!(matches!(
            batch.outcomes[0].simulate,
            Some(SimulateVerdict::Profitable(_))
        ));
        assert!(matches!(
            batch.outcomes[1].simulate,
            Some(SimulateVerdict::GasUnprofitable(_))
        ));
    }

    /// One identity, one LIVE submit row: two payload rows that would both
    /// submit are refused — the identity contract
    /// (`degenbot_arbitrage::identity`) — before the stage emits a record or a
    /// submit row. The batch never leans on the submit lane's mutual exclusion
    /// to hide the second row, and it never silently deduplicates.
    #[test]
    fn two_live_submit_rows_for_one_identity_are_refused() {
        let res = resolver(&[7]);
        let p = Policy::fresh();
        let row = |net: u64| PayloadRow {
            candidate_id: CandidateId::new(7, deployment_id()),
            gross_profit: alloy::primitives::U256::from(net + 100),
            net_profit: alloy::primitives::U256::from(net),
            gas_used: 300_000,
            priority_fee: 2,
            base_fee_next: 30,
            execute_calldata: alloy::primitives::Bytes::from_static(&[0xab]),
            access_list: None,
            failure: None,
            captured_swaps: Vec::new(),
        };

        let err = assemble(&[], &[row(500), row(700)], &res, &p).unwrap_err();
        assert_eq!(
            err,
            AssemblyError::DuplicateCandidateId {
                candidate_id: CandidateId::new(7, deployment_id()),
                occurrences: 2,
            }
        );
        assert_eq!(err.path_id(), 7);
        assert!(
            err.to_string().contains("one identity is one live row"),
            "the refusal states the rule: {err}"
        );

        // No side effect: neither policy registry was written.
        assert_eq!(p.divergence.lock().unwrap().total_divergent_dropped(), 0);
        assert_eq!(p.fot.lock().unwrap().total_fot_dropped(), 0);
        assert_eq!(p.suppression.lock().unwrap().total_suppressed(), 0);
    }

    /// Records keep PER-ROW provenance: one profitable row, one below the floor,
    /// and one failure for the same identity are three records and exactly ONE
    /// live submit row — the batch shape the inline arm produces.
    #[test]
    fn one_live_submit_row_per_identity_keeps_every_record() {
        let res = resolver(&[7]);
        let p = Policy::fresh();
        let payload = |net: u64, failure: Option<PayloadFailure>| PayloadRow {
            candidate_id: CandidateId::new(7, deployment_id()),
            gross_profit: alloy::primitives::U256::from(net + 100),
            net_profit: alloy::primitives::U256::from(net),
            gas_used: 300_000,
            priority_fee: 2,
            base_fee_next: 30,
            execute_calldata: alloy::primitives::Bytes::from_static(&[0xab]),
            access_list: None,
            failure,
            captured_swaps: Vec::new(),
        };

        let batch = assemble(
            &[],
            &[
                payload(500, None),
                payload(0, None),
                payload(
                    0,
                    Some(PayloadFailure {
                        bucket: "inline-fail".to_string(),
                        fail_index: None,
                        revert_data: alloy::primitives::Bytes::new(),
                    }),
                ),
            ],
            &res,
            &p,
        )
        .unwrap();

        assert_eq!(batch.outcomes.len(), 3, "one record per payload row");
        assert_eq!(
            batch.payload_submits.len(),
            1,
            "exactly one of them joins a live submit row"
        );
        assert_eq!(
            batch.payload_submits[0].candidate_id,
            CandidateId::new(7, deployment_id())
        );
    }

    #[test]
    fn payload_failure_row_yields_a_failed_record() {
        let res = resolver(&[9]);
        let p = Policy::fresh();
        let payload = PayloadRow {
            candidate_id: CandidateId::new(9, deployment_id()),
            gross_profit: alloy::primitives::U256::ZERO,
            net_profit: alloy::primitives::U256::ZERO,
            gas_used: 0,
            priority_fee: 0,
            base_fee_next: 0,
            execute_calldata: alloy::primitives::Bytes::new(),
            access_list: None,
            failure: Some(PayloadFailure {
                bucket: "inline-fail".to_string(),
                fail_index: None,
                revert_data: alloy::primitives::Bytes::new(),
            }),
            captured_swaps: Vec::new(),
        };
        let batch = assemble(&[], &[payload], &res, &p).unwrap();
        assert_eq!(batch.outcomes.len(), 1);
        assert!(matches!(
            batch.outcomes[0].simulate,
            Some(SimulateVerdict::Failed(_))
        ));
    }

    /// A payload FAILURE's detail carries the batch request's real figures
    /// (`optimal_input` + expected `hop_outputs`) and the payload's own captured
    /// swaps. Without the join the dump renders `optimal_input=0
    /// hop_outputs=[] captured[]` for every inline failure — an artifact, not
    /// evidence.
    #[test]
    fn payload_failure_detail_threads_the_request_and_the_capture() {
        use degenbot_arbitrage::CapturedSwap;
        use degenbot_simulation::SwapFamily;

        let res = resolver(&[9]);
        let p = Policy::fresh();
        let captured = CapturedSwap {
            emitter: degenbot_core::address_utils::parse_address(
                "0x1111111111111111111111111111111111111111",
            )
            .unwrap(),
            family: SwapFamily::V2,
            amount0: alloy::primitives::I256::try_from(-1_000_i128).unwrap(),
            amount1: alloy::primitives::I256::try_from(990_i128).unwrap(),
            sqrt_price_x96: alloy::primitives::U256::ZERO,
            liquidity: alloy::primitives::U256::ZERO,
            tick: 0,
        };
        let payload = PayloadRow {
            candidate_id: CandidateId::new(9, deployment_id()),
            gross_profit: alloy::primitives::U256::ZERO,
            net_profit: alloy::primitives::U256::ZERO,
            gas_used: 0,
            priority_fee: 0,
            base_fee_next: 0,
            execute_calldata: alloy::primitives::Bytes::new(),
            access_list: None,
            failure: Some(PayloadFailure {
                bucket: "no-profit".to_string(),
                fail_index: None,
                revert_data: alloy::primitives::Bytes::new(),
            }),
            captured_swaps: vec![captured.clone()],
        };
        // The batch's own solver row for the path — the request the inline sim
        // ran on (the same batch carries it; a payload-served row is skipped
        // from the sim lane, not from the record).
        let request = row(9, 1_234, 5_678);
        let batch = assemble(&[request], &[payload], &res, &p).unwrap();
        let failed = batch
            .outcomes
            .iter()
            .find(|o| matches!(o.simulate, Some(SimulateVerdict::Failed(_))))
            .expect("the payload arm must emit a failed record");
        let Some(SimulateVerdict::Failed(detail)) = failed.simulate.as_ref() else {
            unreachable!("selected by the Failed predicate above");
        };
        assert_eq!(detail.bucket, "no-profit");
        assert_eq!(detail.optimal_input, 1_234, "the request's input");
        assert_eq!(detail.hop_outputs, vec![5_678], "the request's outputs");
        assert_eq!(detail.captured_swaps, vec![captured]);
    }

    /// With no request row in the batch beside the payload (the standalone
    /// payload-only arm), the join is ABSENT rather than wrong: the figures stay
    /// `0`/`[]` and the captured swaps still ride the record.
    #[test]
    fn payload_failure_without_a_request_row_still_carries_the_capture() {
        use degenbot_arbitrage::CapturedSwap;
        use degenbot_simulation::SwapFamily;

        let captured = CapturedSwap {
            emitter: degenbot_core::address_utils::parse_address(
                "0x1111111111111111111111111111111111111111",
            )
            .unwrap(),
            family: SwapFamily::V3,
            amount0: alloy::primitives::I256::try_from(-7_i128).unwrap(),
            amount1: alloy::primitives::I256::try_from(6_i128).unwrap(),
            sqrt_price_x96: alloy::primitives::U256::ZERO,
            liquidity: alloy::primitives::U256::ZERO,
            tick: 0,
        };
        let payload = PayloadRow {
            candidate_id: CandidateId::new(9, deployment_id()),
            gross_profit: alloy::primitives::U256::ZERO,
            net_profit: alloy::primitives::U256::ZERO,
            gas_used: 0,
            priority_fee: 0,
            base_fee_next: 0,
            execute_calldata: alloy::primitives::Bytes::new(),
            access_list: None,
            failure: Some(PayloadFailure {
                bucket: "unknown:0xcafebabe".to_string(),
                fail_index: Some(3),
                revert_data: alloy::primitives::Bytes::from_static(&[0xca, 0xfe]),
            }),
            captured_swaps: vec![captured.clone()],
        };
        let detail = payload.failure_detail(None).expect("failure detail");
        assert_eq!(detail.optimal_input, 0);
        assert!(detail.hop_outputs.is_empty());
        assert_eq!(detail.captured_swaps, vec![captured]);
        assert_eq!(detail.revert_data.as_ref(), &[0xca, 0xfe]);
    }

    // ── Moved from the PyO3 seam (the join's one home is here now) ────────

    fn v2_hop(addr: &str) -> degenbot_execution::HopInfo {
        degenbot_execution::HopInfo::V2(V2HopInfo {
            pool_address: addr.parse().unwrap(),
            token0_address: addr.parse().unwrap(),
            token1_address: addr.parse().unwrap(),
            fee: 30,
            zfo: true,
        })
    }

    fn v3_hop(addr: &str) -> degenbot_execution::HopInfo {
        degenbot_execution::HopInfo::V3(degenbot_execution::V3HopInfo {
            pool_address: addr.parse().unwrap(),
            token0_address: addr.parse().unwrap(),
            token1_address: addr.parse().unwrap(),
            fee: 3000,
            zfo: true,
        })
    }

    fn v4_hop(pool_id_hex: &str) -> degenbot_execution::HopInfo {
        degenbot_execution::HopInfo::V4(degenbot_execution::V4HopInfo {
            pool_manager_address: "0x000000000004444c5dc75cb358380d2e3de08a90"
                .parse()
                .unwrap(),
            pool_id_hex: pool_id_hex.to_string(),
            currency0_address: "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
                .parse()
                .unwrap(),
            currency1_address: "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
                .parse()
                .unwrap(),
            fee: 500,
            tick_spacing: 10,
            hook_address: alloy::primitives::Address::ZERO,
            zfo: true,
        })
    }

    /// Byte-identity of the keying rule: the V4 key is the `pool_id_hex`
    /// string and the V2/V3 key the EIP-55 (checksummed) Display form —
    /// the exact sets the old Python set-comprehension mirrored. Mixed-family
    /// paths key the same on both entry arms (both call THIS walk).
    #[test]
    fn derive_path_pools_mixed_families_byte_identity() {
        let checksum_addr = alloy::primitives::address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
        assert_eq!(
            format!("{checksum_addr}"),
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
        );
        let v4_id = "0x4f88f7c99022eace4740c6898f59ce6a2e798a1e64ce54589720b7153eb224a7";

        let hops_v4v2 = vec![
            v4_hop(v4_id),
            v2_hop("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"),
        ];
        let hops_v3 = vec![
            v3_hop("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"),
            v2_hop("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
        ];

        let set_v4v2 = derive_path_pools(&hops_v4v2);
        let set_v3 = derive_path_pools(&hops_v3);
        assert!(set_v4v2.contains(&PoolKey::new(v4_id)));
        assert!(set_v4v2.contains(&PoolKey::new("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")));
        assert!(set_v3.contains(&PoolKey::new("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")));
        assert!(set_v3.contains(&PoolKey::new("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")));
        assert_eq!(set_v4v2.len(), 2);
        assert_eq!(set_v3.len(), 2);
        let only_v4: std::collections::HashSet<_> = set_v4v2
            .difference(&set_v3)
            .map(PoolKey::to_string)
            .collect();
        assert_eq!(only_v4, std::iter::once(v4_id.to_string()).collect());
    }

    /// Join entry-point parity: a payload `SimResult` reassembled row and an
    /// FFI survivor for the same path id pass through the SAME
    /// [`join_sim_result`] — the `path_pools` sets are equal by construction.
    #[test]
    fn join_sim_result_sets_are_entry_independent() {
        let v4_id = "0x4f88f7c99022eace4740c6898f59ce6a2e798a1e64ce54589720b7153eb224a7";
        let path_info = PathInfo::new(vec![
            v4_hop(v4_id),
            v2_hop("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"),
        ]);
        let sim = degenbot_arbitrage::SimResult {
            candidate_id: CandidateId::new(7, deployment_id()),
            gross_profit: alloy::primitives::U256::from(600_000_000_000_u64),
            net_profit: alloy::primitives::U256::from(500_000_000_000_u64),
            gas_used: 300_000,
            priority_fee: 2,
            base_fee_next: 30,
            execute_calldata: alloy::primitives::Bytes::from(vec![0xab, 0x58, 0x98, 0xe8, 0x01]),
            value: alloy::primitives::U256::ZERO,
            access_list: None,
            captured_swaps: Vec::new(),
            passed: true,
            hop_count: 2,
        };
        let executor = "0x690b9a9e9aa1c9db991c7721a92d351db4fac990"
            .parse()
            .unwrap();
        // `None` (the unrecoverable-path fall-through) yields an empty set;
        // `Some` yields the derived set — both arms call this one fn.
        let degenerate = join_sim_result(&sim, None, executor);
        assert!(degenerate.path_pools.is_empty());
        let row = join_sim_result(&sim, Some(&path_info), executor);
        assert!(row.path_pools.contains(&PoolKey::new(v4_id)));
        assert!(row
            .path_pools
            .contains(&PoolKey::new("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")));
        assert_eq!(row.path_pools.len(), 2);
        assert_eq!(row.gross_profit, sim.gross_profit);
        assert_eq!(row.net_profit, sim.net_profit);
        assert_eq!(row.gas_used, sim.gas_used);
        assert_eq!(row.priority_fee, sim.priority_fee);
        assert_eq!(row.base_fee_next, sim.base_fee_next);
        assert_eq!(row.access_list, None);
        assert_eq!(row.execute_calldata, sim.execute_calldata);
    }

    /// The standalone payload merge: submit + unprofitable categorization and
    /// the failure rows, one rule for both entry arms.
    #[test]
    fn merge_payload_results_categorizes_and_joins() {
        // Every payload path must resolve — including the failure row's
        // (engine-born rows; the merge's loud-abort arm fires otherwise).
        let res = resolver(&[7, 8, 9]);
        let p = Policy::fresh();
        let _ = p;
        let payloads = [
            PayloadRow {
                candidate_id: CandidateId::new(7, deployment_id()),
                gross_profit: alloy::primitives::U256::from(600_u64),
                net_profit: alloy::primitives::U256::from(500_u64),
                gas_used: 300_000,
                priority_fee: 2,
                base_fee_next: 30,
                execute_calldata: alloy::primitives::Bytes::from_static(&[0xab]),
                access_list: None,
                failure: None,
                captured_swaps: Vec::new(),
            },
            PayloadRow {
                candidate_id: CandidateId::new(8, deployment_id()),
                gross_profit: alloy::primitives::U256::from(600_u64),
                net_profit: alloy::primitives::U256::ZERO,
                gas_used: 300_000,
                priority_fee: 2,
                base_fee_next: 30,
                execute_calldata: alloy::primitives::Bytes::from_static(&[0xab]),
                access_list: None,
                failure: None,
                captured_swaps: Vec::new(),
            },
            PayloadRow {
                candidate_id: CandidateId::new(9, deployment_id()),
                gross_profit: alloy::primitives::U256::ZERO,
                net_profit: alloy::primitives::U256::ZERO,
                gas_used: 0,
                priority_fee: 0,
                base_fee_next: 0,
                execute_calldata: alloy::primitives::Bytes::new(),
                access_list: None,
                failure: Some(PayloadFailure {
                    bucket: "inline-fail".to_string(),
                    fail_index: None,
                    revert_data: alloy::primitives::Bytes::new(),
                }),
                captured_swaps: Vec::new(),
            },
        ];
        let merged = merge_payload_results(&payloads, &res, EXECUTOR, deployment_id()).unwrap();
        // The profitable row joined (pool keys derived from the resolved hops).
        assert_eq!(merged.submits.len(), 1);
        assert_eq!(merged.submits[0].candidate_id.path_id(), 7);
        assert!(!merged.submits[0].path_pools.is_empty());
        // The zero-profit row counted unprofitable; the failure row surfaced.
        assert_eq!(merged.unprofitable_count, 1);
        assert_eq!(merged.verdicts.len(), 2);
        assert_eq!(
            merged.verdicts[0],
            (CandidateId::new(7, deployment_id()), PayloadArm::Submit)
        );
        assert_eq!(
            merged.verdicts[1],
            (
                CandidateId::new(8, deployment_id()),
                PayloadArm::Unprofitable
            )
        );
        assert_eq!(merged.failures.len(), 1);
        assert_eq!(merged.failures[0].0.path_id(), 9);
        // Path infos in first-appearance order over DISTINCT ids (including
        // the failure row's — the render source covers it).
        let ids: Vec<u64> = merged.path_infos.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![7, 8, 9]);
    }

    /// Decision (a), payload arm: a payload row is engine-born, so a resolve
    /// miss evidences batch/registry divergence — the LOUD-ABORT arm (the
    /// raw-row arm above stays the typed skip).
    #[test]
    fn merge_payload_resolve_miss_is_a_loud_abort() {
        let res = resolver(&[]);
        let payloads = [PayloadRow {
            candidate_id: CandidateId::new(11, deployment_id()),
            gross_profit: alloy::primitives::U256::from(600_u64),
            net_profit: alloy::primitives::U256::from(500_u64),
            gas_used: 300_000,
            priority_fee: 2,
            base_fee_next: 30,
            execute_calldata: alloy::primitives::Bytes::from_static(&[0xab]),
            access_list: None,
            failure: None,
            captured_swaps: Vec::new(),
        }];
        let err = merge_payload_results(&payloads, &res, EXECUTOR, deployment_id()).unwrap_err();
        assert_eq!(err.path_id(), 11);
        assert!(err.to_string().contains("not registered in this engine"));
    }

    /// The standalone merge refuses a row minted by another deployment BEFORE
    /// the resolve: the join would otherwise stamp this executor onto foreign
    /// calldata and hand it to this deployment's submit lane.
    #[test]
    fn merge_payload_refuses_a_foreign_deployment_before_the_join() {
        // The path is deliberately unregistered too: the identity check must
        // fire FIRST, so the error names the mismatch, not a resolve miss.
        let res = resolver(&[]);
        let payloads = [PayloadRow {
            candidate_id: CandidateId::new(7, DeploymentId::new(2)),
            gross_profit: alloy::primitives::U256::from(600_u64),
            net_profit: alloy::primitives::U256::from(500_u64),
            gas_used: 300_000,
            priority_fee: 2,
            base_fee_next: 30,
            execute_calldata: alloy::primitives::Bytes::from_static(&[0xab]),
            access_list: None,
            failure: None,
            captured_swaps: Vec::new(),
        }];
        let err = merge_payload_results(&payloads, &res, EXECUTOR, deployment_id()).unwrap_err();
        // The path id survives, and both identities are named.
        assert_eq!(err.path_id(), 7);
        let detail = err.to_string();
        assert!(
            detail.contains("carries deployment 2"),
            "detail must name the actual deployment: {detail}"
        );
        assert!(
            detail.contains("deployment 1"),
            "detail must name the expected deployment: {detail}"
        );
    }
}
