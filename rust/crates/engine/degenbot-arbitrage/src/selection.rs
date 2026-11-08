//! Target-selection policy above the deployment registry (ADR-098 D4).
//!
//! A session may hold several heterogeneous execution deployments, and the
//! operator decides HOW a solved path chooses one. This module owns that choice
//! and the ordered walk it implies. It encodes nothing: every step of the walk
//! is the deployment's own [`degenbot_execution::ExecutionAdapter::encode`]
//! answer, so selection never grows a capability opinion beside the adapter's.
//!
//! # The two policies
//!
//! [`TargetSelection::FirstCompatible`] (the default) examines the registry in
//! order and binds a path to the FIRST deployment that accepts composition. A
//! deployment that declines for want of capability
//! ([`degenbot_execution::ComposeError::Unsupported`]) is the routine case: the
//! walk advances to the next target without failing or suppressing anything.
//! Every target declining is that path having no selected deployment, not an
//! error: each examined candidate keeps its own refusal.
//!
//! [`TargetSelection::AllTargets`] is the explicit fan-out: every registered
//! deployment is offered every path and each runs its own independent
//! candidate with its own outcome.
//!
//! # Composition decides, and it happens once
//!
//! The accepted [`degenbot_execution::TransactionRequest`] travels to
//! simulation on the candidate itself ([`PreparedEncoding`]), so an adapter's
//! `encode` callback runs exactly once per examined target — never once for
//! selection and again for simulation. A target the walk never reaches is never
//! composed at all.
//!
//! # What is NOT a capability decline
//!
//! Only [`degenbot_execution::ComposeError::Unsupported`] advances the walk. A
//! [`degenbot_execution::ComposeError::Encode`] failure is a primitive encoding
//! error: it is unrecoverable for that candidate, so it ends the walk instead of
//! being reclassified as "this target cannot" — another target would not make
//! the failure disappear, it would only hide it. A transaction aimed away from
//! the deployment's declared target is likewise that candidate's own refusal (the
//! simulation leaf records `target-mismatch`), and a revert, an unprofitable
//! result, or a failed assessment is a simulation verdict: the walk is over
//! before any of them ran, so none of them can select a later target.

use std::collections::HashMap;

use degenbot_execution::{CandidateId, ComposeError, DeploymentId, TransactionRequest};

use crate::deployment::DeploymentRegistry;
use crate::dispatch::DispatchCandidate;
use crate::simulator::compose_transaction;

/// How a solved path chooses the deployment that executes it.
///
/// The policy is an operator decision about ORDER and SELECTION, never about
/// encoding: which targets a path is offered to, and whether the first refusal
/// passes the path on. A session naming one deployment is unaffected either way
/// — the order holds one entry and both policies examine it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum TargetSelection {
    /// Ordered first-compatible: examine the registry in order and execute at
    /// the first deployment that accepts composition. The default.
    #[default]
    FirstCompatible,
    /// Explicit fan-out: every registered deployment is offered every path,
    /// each running an independent candidate and outcome.
    AllTargets,
}

impl TargetSelection {
    /// Every accepted spelling, in the order a refusal lists them.
    pub const NAMES: [&'static str; 2] = ["first_compatible", "all_targets"];

    /// This policy's operator-facing spelling (the configuration name).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::FirstCompatible => Self::NAMES[0],
            Self::AllTargets => Self::NAMES[1],
        }
    }

    /// Parse a policy from its spelling, or `None` when the name names no
    /// policy (a caller reports that loudly rather than defaulting).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "first_compatible" => Some(Self::FirstCompatible),
            "all_targets" => Some(Self::AllTargets),
            _ => None,
        }
    }
}

/// The Encode decision a candidate already carries, so a target the selection
/// walk examined is never composed a second time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreparedEncoding {
    /// The deployment accepted composition: this is the exact transaction to
    /// simulate and, if it clears the policy, to submit.
    Accepted(TransactionRequest),
    /// The deployment declined for want of capability
    /// ([`degenbot_execution::ComposeError::Unsupported`]) — a routine refusal
    /// the walk advanced past.
    Declined(String),
    /// A primitive encoding step failed ([`degenbot_execution::ComposeError::Encode`]):
    /// this candidate's own exception, and the end of the walk.
    Failed(String),
}

impl PreparedEncoding {
    /// Take the encoding a selection compose produced, keeping its outcome.
    #[must_use]
    fn from_compose_result(result: Result<TransactionRequest, ComposeError>) -> Self {
        match result {
            Ok(transaction) => Self::Accepted(transaction),
            Err(ComposeError::Unsupported(detail)) => Self::Declined(detail),
            Err(ComposeError::Encode(detail)) => Self::Failed(detail),
        }
    }

    /// Whether this encoding ends the walk: an accepted transaction selects the
    /// target, a hard error is that candidate's own unrecoverable verdict, and
    /// only a capability decline leaves the path looking for another target.
    #[must_use]
    const fn stops_walk(&self) -> bool {
        !matches!(self, Self::Declined(_))
    }
}

/// What one first-compatible walk produced.
#[derive(Debug, Default)]
pub struct SelectedCandidates {
    /// The examined candidates, each carrying its [`PreparedEncoding`], in path
    /// order and registry order within a path.
    pub examined: Vec<DispatchCandidate>,
    /// The candidates the walk stopped short of: an earlier target accepted, or
    /// a hard encode error ended the walk. Never composed, never simulated, and
    /// they produce no record.
    pub retired: Vec<CandidateId>,
}

/// Walk one batch's candidates in registry order, per path, and select the
/// first deployment each path's composition accepts.
///
/// `candidates` must name registered deployments only
/// ([`crate::dispatch_across_deployments`] validates that before selecting);
/// a candidate that names an unregistered one is retired unexamined rather than
/// simulated against an identity the registry cannot describe.
///
/// A path is only ever examined at deployments the caller minted a candidate
/// for: an offer the assembly already dropped (a cap truncation, a payload-served
/// skip) is not re-derived here, because composition needs the solver's rows,
/// not just the path.
pub(crate) fn select_first_compatible(
    candidates: Vec<DispatchCandidate>,
    deployments: &DeploymentRegistry,
) -> SelectedCandidates {
    // Registry position is the walk's order, so a candidate list minted
    // deployment-major or path-major selects identically.
    let positions: HashMap<DeploymentId, usize> = deployments
        .ids()
        .enumerate()
        .map(|(index, deployment_id)| (deployment_id, index))
        .collect();

    // Group by path, keeping first-appearance order (the solver's order): the
    // walk is independent per path and paths never influence each other.
    let mut order: Vec<u64> = Vec::new();
    let mut by_path: HashMap<u64, Vec<DispatchCandidate>> = HashMap::new();
    for candidate in candidates {
        let path_id = candidate.candidate_id.path_id();
        by_path.entry(path_id).or_insert_with(|| {
            order.push(path_id);
            Vec::new()
        });
        if let Some(group) = by_path.get_mut(&path_id) {
            group.push(candidate);
        }
    }

    let mut selected = SelectedCandidates::default();
    for path_id in order {
        let Some(mut group) = by_path.remove(&path_id) else {
            continue;
        };
        // Registry order first; a path offered more than once at one
        // deployment keeps the caller's order, so the first accepting offer is
        // the one the path executes.
        group.sort_by_key(|candidate| {
            positions
                .get(&candidate.candidate_id.deployment_id())
                .copied()
                .unwrap_or(usize::MAX)
        });

        let mut walk = group.into_iter();
        let mut decided = false;
        for candidate in walk.by_ref() {
            let Some(deployment) = deployments.get(candidate.candidate_id.deployment_id()) else {
                selected.retired.push(candidate.candidate_id);
                continue;
            };
            let mut candidate = candidate;
            let prepared = PreparedEncoding::from_compose_result(compose_transaction(
                deployment,
                &candidate.to_simulate_path(),
            ));
            decided = prepared.stops_walk();
            candidate.prepared = Some(prepared);
            selected.examined.push(candidate);
            if decided {
                break;
            }
        }
        // Whatever the walk did not reach is retired: an accepted target ends
        // the path's walk, and so does a hard encode error.
        if decided {
            selected
                .retired
                .extend(walk.map(|candidate| candidate.candidate_id));
        }
    }

    selected
}
