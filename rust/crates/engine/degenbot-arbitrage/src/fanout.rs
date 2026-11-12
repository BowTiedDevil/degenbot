//! Explicit all-target candidate fan-out over an ordered deployment registry
//! (ADR-098 D4).
//!
//! [`dispatch_profitable_results`] is the ONE-deployment leaf: every candidate
//! it is handed is simulated against `ctx.deployment`. This module is the
//! registry-aware orchestration above it: one lane per registered deployment, in
//! registry order, with a candidate only ever simulated against the deployment
//! its [`degenbot_execution::CandidateId`] names.
//!
//! # What the fan-out guarantees
//!
//! - **One lane per registered deployment, in registry order.** [`DeploymentFanOutOutcome::lanes`]
//!   is that order, so the caller's merged record set is deterministic.
//! - **A candidate never crosses lanes.** The partition is by
//!   `candidate_id.deployment_id()`, so an adapter only ever encodes its own
//!   candidates; a candidate naming an unregistered deployment is a wiring bug
//!   and is refused before any lane runs.
//! - **One identity, one row.** A set carrying one [`CandidateId`] twice is
//!   refused ([`crate::DispatchRefusal::DuplicateCandidateId`]) before the
//!   selection walk composes anything, because composition, the ledger stamp,
//!   the lane's result map, and the submit lane all key on that identity and
//!   cannot tell its two rows apart (see [`crate::identity`]).
//! - **Isolation.** A lane's refusals (an adapter that will not encode, a
//!   revert, a failed assessment) stay that lane's rows. The other lanes'
//!   candidates for the same path are simulated, categorized, and submitted
//!   independently: no lane's verdict suppresses another's.
//!
//! # Which targets a path is offered to
//!
//! Which deployments a path is offered to is the session's
//! [`TargetSelection`], and it is the only difference between the two policies:
//!
//! - [`TargetSelection::AllTargets`] — every registered deployment is offered
//!   every candidate (the fan-out described above, with independent outcomes).
//! - [`TargetSelection::FirstCompatible`] (the default) — each path is offered
//!   the registry in order and executes at the first deployment whose
//!   composition accepts it ([`crate::selection`]). Later targets are never
//!   composed or simulated, and the candidates the walk never reached are
//!   [`DeploymentFanOutOutcome::retired`].
//!
//! Selection decides WHO runs; the lanes below still decide whether the chosen
//! target's candidate survives the pre-sim policy, and a lane's refusals stay
//! that lane's rows either way.
//!
//! # Call shape
//!
//! Both entries take one named [`FanOutInputs`] — registry, block, block number,
//! selection policy, the three policy registries, `BotState`, and the warm cache
//! — so no argument order can transpose two inputs of the same shape, and a new
//! shared input is one field rather than a signature-wide rewrite. The
//! CANDIDATES stay outside it, because the entries take different kinds of row.
//!
//! The raw entry takes [`RawFanOutInputs`]: those same named inputs plus the
//! thin-margin floor its own admission pass applies. The pre-admitted entry
//! takes [`FanOutInputs`], so a pre-admitted caller cannot hand a thin floor to
//! a leaf that has no thin stage — the floor belongs to that caller's early
//! projection ([`crate::project_early_admission`]).
//!
//! ```text
//! let inputs = FanOutInputs {
//!     deployments: &registry,
//!     block: &block,
//!     current_block: BLOCK,
//!     target_selection: TargetSelection::AllTargets,
//!     path_suppression: &suppression,
//!     pool_divergence: &divergence,
//!     fot_registry: &fot,
//!     bot_state: Some(&bot_state),
//!     warm_cache: Some(&warm_cache),
//! };
//!
//! // RAW: whole policy in the leaf, thin floor included.
//! let raw = dispatch_across_deployments(candidates, inputs.with_thin_margin(50))?;
//!
//! // PRE-ADMITTED: same configuration, no thin floor; the leaf re-reads.
//! let admitted = dispatch_admitted_across_deployments(transported, inputs)?;
//! ```
//!
//! Both calls audit NOTHING in the other entry's shape: the raw call's floor is
//! a field of [`RawFanOutInputs`] alone, and the pre-admitted call's rows are
//! [`AdmittedCandidate`]s only [`crate::project_early_admission`] can mint.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use degenbot_execution::{CandidateId, DeploymentId};
use degenbot_submission::PathSuppression;
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::BotState;
use parking_lot::RwLock;

use crate::admission::AdmittedCandidate;
use crate::deployment::{
    DeploymentRegistry, SimulateContext, SimulationBlock, SimulationDeployment,
};
use crate::dispatch::{
    dispatch_admitted_profitable_results, dispatch_profitable_results, DispatchCandidate,
    DispatchOutcome,
};
use crate::identity::{duplicate_candidate_id, DispatchRefusal};
use crate::selection::{select_first_compatible, FanOutEntry, TargetSelection};
use crate::{FeeOnTransferRegistry, PoolDivergence};

// The entries' side of the walk's interface ([`crate::selection::FanOutEntry`]):
// the choreography above the leaves — the unregistered-deployment check, target
// selection, the deployment partition, the registry lane order, and the
// per-lane span — is written ONCE over it, so the raw standalone entry and the
// pre-admitted entry cannot drift apart.
impl FanOutEntry for DispatchCandidate {
    fn candidate(&self) -> &Self {
        self
    }

    fn candidate_mut(&mut self) -> &mut Self {
        self
    }
}

impl FanOutEntry for AdmittedCandidate {
    // The inherent accessors, not these trait methods: the transport owns the
    // candidate, so the walk reads and annotates it without a side table.
    fn candidate(&self) -> &DispatchCandidate {
        AdmittedCandidate::candidate(self)
    }

    fn candidate_mut(&mut self) -> &mut DispatchCandidate {
        AdmittedCandidate::candidate_mut(self)
    }
}

/// One deployment's slice of the fan-out.
#[derive(Debug)]
pub struct DeploymentLane {
    /// The deployment this lane simulated against (its half of every
    /// [`CandidateId`] in [`Self::outcome`]).
    pub deployment_id: DeploymentId,
    /// The lane's own categorization — tallies, survivors, and failures name
    /// only this deployment's candidates.
    pub outcome: DispatchOutcome,
}

/// The registry fan-out's product: one lane per registered deployment, in
/// registry order.
#[derive(Debug, Default)]
pub struct DeploymentFanOutOutcome {
    /// The per-deployment lanes, in registry order.
    pub lanes: Vec<DeploymentLane>,
    /// The candidates the target-selection policy retired WITHOUT examining
    /// them: under [`TargetSelection::FirstCompatible`], the deployments a path
    /// stopped being offered to once an earlier target accepted its composition
    /// (or once a hard encode error ended the walk).
    ///
    /// They were neither composed nor simulated, so they hold no lane row and
    /// deserve no record: the caller's record set names exactly the candidates
    /// that entered execution. Empty under [`TargetSelection::AllTargets`], where
    /// every candidate is examined.
    pub retired: Vec<CandidateId>,
}

impl DeploymentFanOutOutcome {
    /// The lane for `deployment_id`, if the registry held it.
    #[must_use]
    pub fn lane(&self, deployment_id: DeploymentId) -> Option<&DispatchOutcome> {
        self.lanes
            .iter()
            .find(|lane| lane.deployment_id == deployment_id)
            .map(|lane| &lane.outcome)
    }
}

/// The shared configuration ONE registry fan-out runs with: the ordered
/// registry, the simulated block and its number, the target-selection policy,
/// the three policy registries, and the engine handles.
///
/// Every field is NAMED at the call site. The two positional hazards the old
/// signatures carried are gone: the simulated block [`Self::block`] and the
/// block number [`Self::current_block`] can no longer be transposed, and the
/// suppression ledger, divergence memo, and fee-on-transfer registry cannot be
/// passed in each other's slot. Adding a shared input adds a field.
///
/// The CANDIDATES are deliberately absent: they are the entry's own subject,
/// and the two entries take different kinds of row ([`DispatchCandidate`] for
/// the raw entry, [`AdmittedCandidate`] for the pre-admitted one). The raw
/// entry's thin-margin floor is absent for the mirror-image reason — it is
/// [`RawFanOutInputs`], which the pre-admitted entry does not take.
///
/// # Construction
///
/// A literal, one field per input:
///
/// ```text
/// let inputs = FanOutInputs {
///     deployments: &registry,
///     block: &block,
///     current_block: BLOCK,
///     target_selection: TargetSelection::AllTargets,
///     path_suppression: &suppression,
///     pool_divergence: &divergence,
///     fot_registry: &fot,
///     bot_state: Some(&bot_state),
///     warm_cache: Some(&warm_cache),
/// };
/// ```
///
/// The policy registries are borrowed as `&Arc<Mutex<_>>` and the engine
/// handles as `Option<&Arc<_>>`: the fan-out borrows them for its own duration
/// and clones the `Arc`s into each lane's leaf, so one configuration serves
/// every lane and the caller keeps ownership. [`Clone`] and [`Copy`] are
/// derived, so one built configuration serves EITHER entry.
#[derive(Clone, Copy)]
pub struct FanOutInputs<'a> {
    /// The ordered deployment registry: one lane per entry, in this order.
    pub deployments: &'a DeploymentRegistry,
    /// The simulated block's facts; every lane clones them into its
    /// [`SimulateContext`].
    pub block: &'a SimulationBlock<'a>,
    /// The block this fan-out runs at — the policy's clock (suppression retry,
    /// divergence decay, staleness) and the per-lane span's block.
    ///
    /// A session normally passes the block [`Self::block`] was built for, but
    /// the two are separate inputs: the policy clock is not read off the block
    /// facts.
    pub current_block: u64,
    /// Which deployments each path is offered to ([`TargetSelection`]).
    pub target_selection: TargetSelection,
    /// The retry-interval ledger: the admission/finalization read stamps it and
    /// the outcome recording writes it.
    pub path_suppression: &'a Arc<Mutex<PathSuppression>>,
    /// The per-pool solver-divergence memo: read at the pre-sim skip and written
    /// by the `SolverCalc` feedback.
    pub pool_divergence: &'a Arc<Mutex<PoolDivergence>>,
    /// The per-token fee-on-transfer registry: read at the pre-sim skip and
    /// written by the failure/success feedback.
    pub fot_registry: &'a Arc<Mutex<FeeOnTransferRegistry>>,
    /// The engine's shared state owner (ADR-003), the staleness gate's final
    /// read. `None` skips the gate; non-empty input without it hits the leaf's
    /// unreachable guard.
    pub bot_state: Option<&'a Arc<StateLock<BotState>>>,
    /// The cross-block persistent bytecode cache the lane's sim handle is built
    /// with. `None` degrades to Tier-1-only behavior (no cross-block
    /// persistence, no panic).
    pub warm_cache: Option<
        &'a Arc<RwLock<degenbot_simulation::sim::evm2::warm_code_cache::WarmCodeCacheInner>>,
    >,
}

impl<'a> FanOutInputs<'a> {
    /// The RAW entry's inputs: this shared configuration plus the thin-margin
    /// floor the raw leaf applies inside its admission pass.
    ///
    /// `min_profit_margin_bps` is in basis points of each candidate's
    /// `optimal_input` — the same floor
    /// [`crate::EarlyAdmissionPolicy::min_profit_margin_bps`] takes. The
    /// pre-admitted entry has no counterpart: moving a caller onto it means
    /// passing the floor to [`crate::project_early_admission`], where the drops
    /// become the caller's own records and
    /// [`crate::DispatchOutcome::thin_dropped`] stays `0`.
    ///
    /// ```text
    /// dispatch_across_deployments(candidates, inputs.with_thin_margin(50))
    /// ```
    #[must_use]
    pub fn with_thin_margin(self, min_profit_margin_bps: u64) -> RawFanOutInputs<'a> {
        RawFanOutInputs {
            shared: self,
            min_profit_margin_bps,
        }
    }
}

/// The RAW entry's inputs: the shared [`FanOutInputs`] plus the thin-margin
/// floor, built by [`FanOutInputs::with_thin_margin`].
///
/// A distinct type on purpose: [`dispatch_admitted_across_deployments`] takes
/// [`FanOutInputs`] and so cannot accept a thin floor at all, accidentally or
/// otherwise.
#[derive(Clone, Copy)]
pub struct RawFanOutInputs<'a> {
    /// The configuration every fan-out shares.
    shared: FanOutInputs<'a>,
    /// The thin floor, in basis points of each candidate's `optimal_input`.
    min_profit_margin_bps: u64,
}

/// The registry choreography both entries share: validate the identities, run
/// the target-selection policy, partition by deployment, then walk the registry
/// in order and hand each lane's entries to `leaf`.
///
/// ONE implementation, whatever the entries are: the raw standalone candidates
/// and the pre-admitted transport entries are partitioned, ordered, and spanned
/// identically, and the only per-entry detail the kernel reads is the carried
/// [`DispatchCandidate`]'s identity (and the encoding the selection walk
/// attaches to it).
///
/// Both identity checks are INPUT refusals and run before anything else: the
/// [`CandidateId`] row contract (a repeated identity, so no adapter composes and
/// no ledger stamp or tally is spent on a set the lanes could not keep apart)
/// and then the registry membership. Only then does the selection walk reach an
/// adapter.
fn fan_out_over<Entry: FanOutEntry>(
    entries: Vec<Entry>,
    deployments: &DeploymentRegistry,
    target_selection: TargetSelection,
    current_block: u64,
    mut leaf: impl FnMut(
        &Arc<SimulationDeployment>,
        Vec<Entry>,
    ) -> Result<DispatchOutcome, DispatchRefusal>,
) -> Result<DeploymentFanOutOutcome, DispatchRefusal> {
    // One identity, one row (see `crate::identity`): the composition the walk
    // below performs and the map every lane joins its results through are
    // identity-keyed, so a repeated identity is refused before either runs.
    if let Some((candidate_id, occurrences)) =
        duplicate_candidate_id(entries.iter().map(|entry| entry.candidate().candidate_id))
    {
        return Err(DispatchRefusal::DuplicateCandidateId {
            candidate_id,
            occurrences,
        });
    }

    // Every candidate the caller minted names a deployment this registry holds;
    // checked ONCE, before either policy touches a candidate or an adapter.
    for entry in &entries {
        let candidate_id = entry.candidate().candidate_id;
        if deployments.get(candidate_id.deployment_id()).is_none() {
            return Err(DispatchRefusal::UnregisteredDeployment {
                candidate_id,
                registered: deployments.ids().collect(),
            });
        }
    }

    // Target selection. `AllTargets` keeps the caller's candidate set verbatim;
    // `FirstCompatible` narrows it to the candidates its ordered walk examined
    // (each carrying the deployment's own Encode decision, so a selected target
    // is never composed twice) and reports the rest as retired.
    let (examined, retired) = match target_selection {
        TargetSelection::AllTargets => (entries, Vec::new()),
        TargetSelection::FirstCompatible => {
            let selection = select_first_compatible(entries, deployments);
            (selection.examined, selection.retired)
        }
    };

    // Partition by target. The order inside a lane is the caller's order (the
    // assembly's cap-ordered candidate list), so a lane's survivors keep the
    // best-first contract the leaf's cap expects.
    let mut lanes: HashMap<DeploymentId, Vec<Entry>> = HashMap::with_capacity(deployments.len());
    for entry in examined {
        lanes
            .entry(entry.candidate().candidate_id.deployment_id())
            .or_default()
            .push(entry);
    }

    let mut outcomes = Vec::with_capacity(deployments.len());
    for deployment in deployments.entries() {
        let lane_entries = lanes
            .remove(&deployment.deployment_id())
            .unwrap_or_default();
        // ONE Jaeger span per deployment's simulate fan-out (ADR-043), entered
        // only across the synchronous leaf call.
        let outcome = {
            let span =
                degenbot_bot::telemetry::simulate_dispatch_span(current_block, lane_entries.len());
            let _guard = span.enter();
            leaf(deployment, lane_entries)?
        };
        outcomes.push(DeploymentLane {
            deployment_id: deployment.deployment_id(),
            outcome,
        });
    }

    Ok(DeploymentFanOutOutcome {
        lanes: outcomes,
        retired,
    })
}

/// Fan every RAW standalone candidate out to the deployment its identity names,
/// one lane per registered deployment, in registry order.
///
/// The shared configuration arrives as [`RawFanOutInputs`] — one named
/// [`FanOutInputs`] field per input, plus the thin-margin floor
/// [`FanOutInputs::with_thin_margin`] attaches — so a caller cannot transpose
/// the block for the block number, or the suppression ledger for the divergence
/// memo. [`FanOutInputs::target_selection`] decides which deployments each path
/// is offered to ([`TargetSelection`]); the lanes are the same either way, and
/// the policy registries ([`FanOutInputs::path_suppression`],
/// [`FanOutInputs::pool_divergence`], [`FanOutInputs::fot_registry`]) and the
/// engine handles ([`FanOutInputs::bot_state`], [`FanOutInputs::warm_cache`])
/// are SHARED across them — one simulated block, one set of policy state —
/// while each lane reads and stamps them per candidate, so a
/// suppression/failure entry keyed `(path, deployment)` never bleeds into
/// another target.
///
/// Each lane runs the RAW standalone leaf
/// ([`dispatch_profitable_results`]): the whole admission pipeline, in the
/// standalone order, over that lane's candidates — no pre-selection staleness
/// gate, so an examined stale candidate still retires a fresh sibling. A caller
/// that projected its rows early ([`crate::project_early_admission`]) transports
/// its survivors to [`dispatch_admitted_across_deployments`] instead.
///
/// # Errors
///
/// [`DispatchRefusal::DuplicateCandidateId`] when two rows carry one
/// [`CandidateId`] — the identity contract (see [`crate::identity`]) — and
/// [`DispatchRefusal::UnregisteredDeployment`] when a candidate names an
/// identity the registry does not hold. Both are checked before any selection,
/// composition, or lane runs, under either policy.
pub fn dispatch_across_deployments(
    candidates: Vec<DispatchCandidate>,
    inputs: RawFanOutInputs<'_>,
) -> Result<DeploymentFanOutOutcome, DispatchRefusal> {
    let RawFanOutInputs {
        shared,
        min_profit_margin_bps,
    } = inputs;
    fan_out_over(
        candidates,
        shared.deployments,
        shared.target_selection,
        shared.current_block,
        |deployment, lane| {
            let ctx = SimulateContext {
                block: shared.block.clone(),
                deployment,
            };
            dispatch_profitable_results(
                lane,
                &ctx,
                shared.path_suppression,
                shared.current_block,
                min_profit_margin_bps,
                shared.pool_divergence,
                shared.fot_registry,
                shared.bot_state.cloned(),
                shared.warm_cache.cloned(),
            )
        },
    )
}

/// Fan every PRE-ADMITTED candidate out to the deployment its identity names,
/// one lane per registered deployment, in registry order.
///
/// The pre-admitted entry: the caller obtained its survivors from
/// [`crate::project_early_admission`] — the shared early projection, whose
/// stages are the non-stamping suppression read, divergence, fee-on-transfer,
/// the caller's thin floor, the caller's input-order cap, and the
/// PRE-SELECTION staleness gate — and transports them as
/// [`AdmittedCandidate`]s. "Transported" therefore means early-projected and
/// pre-selection-staleness-gated, never guaranteed admitted at finalization.
/// Everything the registry choreography owns is the SAME implementation the
/// raw entry uses —
/// one target-selection walk, one deployment partition, one lane order, one
/// span per lane — and only the leaf differs: each lane runs the pre-admitted
/// leaf, whose finalization re-reads the policy state authoritatively
/// (`stamping suppression -> divergence -> fee-on-transfer -> staleness ->
/// optional order-index reorder -> same-cap truncation`) before the shared
/// simulation body runs.
///
/// There is no thin-margin field on [`FanOutInputs`] on purpose: the floor is
/// early admission's verdict, so the leaf has no stage for it and
/// `DispatchOutcome::thin_dropped` stays `0`. The raw entry's floor rides
/// [`RawFanOutInputs`], a type this entry does not take — a pre-admitted caller
/// cannot express the floor here by accident.
///
/// # Migrating a direct Rust caller (raw -> pre-admitted)
///
/// The raw call already builds the shared configuration — it passes
/// [`RawFanOutInputs`], i.e. the same [`FanOutInputs`] plus its thin floor — and
/// that configuration carries over unchanged (it is [`Copy`], so one literal
/// serves either entry). What moves is the floor and the rows: the floor becomes
/// the projection's input, the candidates pass through the projection, and the
/// survivors are transported.
///
/// 0. **Drop the floor from the call.** `inputs.with_thin_margin(bps)` becomes
///    plain `inputs`, and `bps` becomes the projection's
///    `min_profit_margin_bps` in step 1. There is no floor to pass here — the
///    type does not have one — and `DispatchOutcome::thin_dropped` is `0` from
///    then on.
/// 1. **Project early.** Call [`crate::project_early_admission`] with your
///    resolved rows and a [`crate::EarlyAdmissionPolicy`] — the non-stamping
///    suppression read, divergence, fee-on-transfer, your thin margin, your own
///    per-lane cap, and the PRE-SELECTION staleness gate (pass your state owner;
///    `None` skips the gate). It sits in front of target selection on purpose:
///    an examined stale candidate is composed and accepted, which retires its
///    fresh sibling at a later target ([`crate::selection`]). Every drop comes
///    back beside the row it dropped, which is yours to record; the leaf never
///    sees it.
/// 2. **Transport the survivors.** [`crate::project_early_admission`] is the ONE
///    construction: [`AdmittedCandidate`]'s unchecked constructor is
///    crate-private, so a caller cannot wrap an arbitrary row in it and
///    re-implement these stages. The transport owns the complete candidate, so
///    no ID-keyed side table is needed — and one candidate id may appear only
///    ONCE in the transported set: this entry refuses a repeated [`CandidateId`]
///    ([`DispatchRefusal::DuplicateCandidateId`]) before the walk composes
///    anything, because the ledger, the lane's result map, and the submit row
///    are all identity-keyed (see [`crate::identity`]).
/// 3. **Expect finalization to re-read.** The leaf stamps the suppression
///    ledger, then re-reads divergence, fee-on-transfer, and the live state
///    nonces at its own block. A candidate whose evidence moved since your
///    projection is dropped there — `suppressed_count` / `divergent_dropped` /
///    `fot_dropped` / `stale_dropped` on the lane's [`DispatchOutcome`] account
///    for it — and a retry-due candidate is stamped even when a later final
///    stage drops it. Admission is never a promise of later admission.
///
/// `DispatchOutcome::thin_dropped` is `0` on this entry because there is no thin
/// stage; report your assembly's own thin drops beside it.
///
/// # Cap and order
///
/// The leaf truncates at the same [`crate::MAX_SIMULATE_CONCURRENT`] cap the raw
/// leaf applies, AFTER its reorder. Your assembly already capped each lane at
/// that cap, so the truncation preserves the survivors you transported: with
/// the `order-index` feature on they are reordered into the net-profit ranking
/// and ALL of them reach simulation in that order; with it off, your transported
/// order IS the simulation order.
///
/// # Errors
///
/// [`DispatchRefusal::DuplicateCandidateId`] when two transported rows carry one
/// [`CandidateId`], and [`DispatchRefusal::UnregisteredDeployment`] when a
/// transported candidate names an identity the registry does not hold — both
/// checked before any selection, composition, or lane runs, under either policy.
pub fn dispatch_admitted_across_deployments(
    admitted: Vec<AdmittedCandidate>,
    inputs: FanOutInputs<'_>,
) -> Result<DeploymentFanOutOutcome, DispatchRefusal> {
    fan_out_over(
        admitted,
        inputs.deployments,
        inputs.target_selection,
        inputs.current_block,
        |deployment, lane| {
            let ctx = SimulateContext {
                block: inputs.block.clone(),
                deployment,
            };
            dispatch_admitted_profitable_results(
                lane,
                &ctx,
                inputs.path_suppression,
                inputs.current_block,
                inputs.pool_divergence,
                inputs.fot_registry,
                inputs.bot_state.cloned(),
                inputs.warm_cache.cloned(),
            )
        },
    )
}
