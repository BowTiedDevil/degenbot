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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use degenbot_execution::{CandidateId, DeploymentId};
use degenbot_submission::PathSuppression;
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::BotState;
use parking_lot::RwLock;

use crate::deployment::{DeploymentRegistry, SimulateContext, SimulationBlock};
use crate::dispatch::{dispatch_profitable_results, DispatchCandidate, DispatchOutcome};
use crate::selection::{select_first_compatible, TargetSelection};
use crate::{FeeOnTransferRegistry, PoolDivergence};

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

/// Why the registry fan-out refused to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeploymentFanOutError {
    /// A candidate named a deployment the session never registered: its
    /// [`CandidateId`] cannot be simulated (there is no adapter, target, or
    /// owner for it) and cannot be joined to a submit lane.
    UnregisteredDeployment {
        /// The offending candidate.
        candidate_id: CandidateId,
        /// The identities the registry actually holds.
        registered: Vec<DeploymentId>,
    },
}

impl std::fmt::Display for DeploymentFanOutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnregisteredDeployment {
                candidate_id,
                registered,
            } => {
                write!(
                    f,
                    "candidate {} targets deployment {}, which is not registered (registered: ",
                    candidate_id,
                    candidate_id.deployment_id()
                )?;
                for (index, id) in registered.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{id}")?;
                }
                f.write_str(")")
            }
        }
    }
}

impl std::error::Error for DeploymentFanOutError {}

/// Fan every candidate out to the deployment its identity names, one lane per
/// registered deployment, in registry order.
///
/// `target_selection` decides which deployments each path is offered to
/// ([`TargetSelection`]); the lanes are the same either way, and the policy
/// registries (`path_suppression`, `pool_divergence`, `fot_registry`) and the
/// engine handles are SHARED across them — one simulated block, one set of
/// policy state — while each lane reads and stamps them per candidate, so a
/// suppression/failure entry keyed `(path, deployment)` never bleeds into
/// another target.
///
/// # Errors
///
/// [`DeploymentFanOutError::UnregisteredDeployment`] when a candidate names an
/// identity the registry does not hold (checked before any selection or lane
/// runs, under either policy).
#[expect(
    clippy::too_many_arguments,
    reason = "the shared fan-out inputs are the one-deployment leaf's, plus the registry and the selection policy"
)]
pub fn dispatch_across_deployments(
    candidates: Vec<DispatchCandidate>,
    block: &SimulationBlock<'_>,
    deployments: &DeploymentRegistry,
    target_selection: TargetSelection,
    path_suppression: &Arc<Mutex<PathSuppression>>,
    current_block: u64,
    min_profit_margin_bps: u64,
    pool_divergence: &Arc<Mutex<PoolDivergence>>,
    fot_registry: &Arc<Mutex<FeeOnTransferRegistry>>,
    bot_state: Option<&Arc<StateLock<BotState>>>,
    warm_cache: Option<
        &Arc<RwLock<degenbot_simulation::sim::evm2::warm_code_cache::WarmCodeCacheInner>>,
    >,
) -> Result<DeploymentFanOutOutcome, DeploymentFanOutError> {
    // Every candidate the caller minted names a deployment this registry holds;
    // checked ONCE, before either policy touches a candidate or an adapter.
    for candidate in &candidates {
        if deployments
            .get(candidate.candidate_id.deployment_id())
            .is_none()
        {
            return Err(DeploymentFanOutError::UnregisteredDeployment {
                candidate_id: candidate.candidate_id,
                registered: deployments.ids().collect(),
            });
        }
    }

    // Target selection. `AllTargets` keeps the caller's candidate set verbatim;
    // `FirstCompatible` narrows it to the candidates its ordered walk examined
    // (each carrying the deployment's own Encode decision, so a selected target
    // is never composed twice) and reports the rest as retired.
    let (examined, retired) = match target_selection {
        TargetSelection::AllTargets => (candidates, Vec::new()),
        TargetSelection::FirstCompatible => {
            let selection = select_first_compatible(candidates, deployments);
            (selection.examined, selection.retired)
        }
    };

    // Partition by target. The order inside a lane is the caller's order (the
    // assembly's cap-ordered candidate list), so a lane's survivors keep the
    // best-first contract the leaf's cap expects.
    let mut lanes: HashMap<DeploymentId, Vec<DispatchCandidate>> =
        HashMap::with_capacity(deployments.len());
    for candidate in examined {
        lanes
            .entry(candidate.candidate_id.deployment_id())
            .or_default()
            .push(candidate);
    }

    let mut outcomes = Vec::with_capacity(deployments.len());
    for deployment in deployments.entries() {
        let lane_candidates = lanes
            .remove(&deployment.deployment_id())
            .unwrap_or_default();
        // ONE Jaeger span per deployment's simulate fan-out (ADR-043), entered
        // only across the synchronous leaf call.
        let outcome = {
            let span = degenbot_bot::telemetry::simulate_dispatch_span(
                current_block,
                lane_candidates.len(),
            );
            let _guard = span.enter();
            let ctx = SimulateContext {
                block: block.clone(),
                deployment,
            };
            dispatch_profitable_results(
                lane_candidates,
                &ctx,
                path_suppression,
                current_block,
                min_profit_margin_bps,
                pool_divergence,
                fot_registry,
                bot_state.cloned(),
                warm_cache.cloned(),
            )
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
