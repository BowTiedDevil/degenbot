//! Neutral deployment and candidate identity (ADR-098 D4).
//!
//! A bot may hold several heterogeneous execution deployments, and the SAME
//! solved path can be worthwhile at more than one of them. Path identity alone
//! therefore cannot key a post-target result: two candidates that share a
//! solver `path_id` but target different deployments are independent rows, and
//! a map, join, or suppression key that stores only the path collapses them.
//!
//! The split is a boundary, not a naming convention:
//!
//! - [`DeploymentId`] is the **operator-assigned** identity of one execution
//!   deployment. It is opaque and neutral: it carries no release id, protocol
//!   capability, contract address, or target-selection policy — those belong
//!   to the adapter that owns the deployment (ADR-098 D4/D5).
//! - [`CandidateId`] is the **post-target** identity: one solver path
//!   evaluated at one deployment. It is the authoritative key for a
//!   candidate's result map entry, simulation record, batch outcome, and
//!   submission record.
//!
//! Everything BEFORE target selection stays path-keyed: the solver's result,
//! the path registry/resolver, and the discovery surface name a path, not a
//! deployment. `CandidateId` is minted where a path is bound to the deployment
//! that will encode and submit it, and it is never re-derived from a subset of
//! its own fields.

use std::fmt;

/// The operator-assigned identity of one execution deployment.
///
/// Opaque and `Copy`: deployments are named once at the composition boundary
/// and the name travels by value through every result key. Nothing else about
/// the deployment (address, owner, protocol version, release, capabilities) is
/// part of this identity — an adapter owns those facts (ADR-098 D4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeploymentId(u64);

impl DeploymentId {
    /// Name a deployment. The operator owns the numbering; degenbot never
    /// interprets it.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The operator-facing number this identity was minted with.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

impl fmt::Display for DeploymentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The post-target identity of one candidate: a solver path at a deployment.
///
/// Both halves are carried together because they are one fact. The path id is
/// preserved verbatim, so operator-facing path numbering reads the same as the
/// solver's: a candidate is `path 7 at deployment 1`, never a renumbered
/// substitute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CandidateId {
    path_id: u64,
    deployment_id: DeploymentId,
}

impl CandidateId {
    /// Bind a solver path to the deployment that will execute it.
    #[must_use]
    pub const fn new(path_id: u64, deployment_id: DeploymentId) -> Self {
        Self {
            path_id,
            deployment_id,
        }
    }

    /// The solver path this candidate came from (the pre-target identity,
    /// kept verbatim).
    #[must_use]
    pub const fn path_id(&self) -> u64 {
        self.path_id
    }

    /// The deployment this candidate targets.
    #[must_use]
    pub const fn deployment_id(&self) -> DeploymentId {
        self.deployment_id
    }
}

impl fmt::Display for CandidateId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.path_id, self.deployment_id)
    }
}
