//! The neutral identity pair is the key a post-target result is stored under
//! (ADR-098 D4).
//!
//! A solver path may be worthwhile at more than one execution deployment, so a
//! path id alone cannot key a candidate: these tests pin the two facts a result
//! map, a join, and a failure record rely on — same-path candidates at
//! different deployments are DISTINCT keys, and the composite orders
//! deterministically so a keyed structure's iteration is reproducible.

use std::collections::HashSet;

use degenbot_execution::{CandidateId, DeploymentId};

/// Two deployments simulating the same path are two identities, not one.
#[test]
fn same_path_at_two_deployments_is_two_identities() {
    let built_in = CandidateId::new(7, DeploymentId::new(1));
    let foreign = CandidateId::new(7, DeploymentId::new(2));

    assert_eq!(built_in.path_id(), 7);
    assert_eq!(foreign.path_id(), 7);
    assert_ne!(built_in, foreign, "the deployment half distinguishes them");

    // The map/set key claim: a path-only key would collapse these to one entry.
    let keys: HashSet<CandidateId> = [built_in, foreign, built_in].into_iter().collect();
    assert_eq!(keys.len(), 2);
    assert!(keys.contains(&built_in));
    assert!(keys.contains(&foreign));
}

/// The path id survives verbatim: operator-facing path numbering is unchanged
/// by the composite.
#[test]
fn the_candidate_id_preserves_the_solver_path_id() {
    let candidate = CandidateId::new(11_354, DeploymentId::new(3));

    assert_eq!(candidate.path_id(), 11_354);
    assert_eq!(candidate.deployment_id(), DeploymentId::new(3));
}

/// The composite orders by (path, deployment) — the tie-break a keyed result
/// store and a sorted outcome list need, so two `path 7` rows never compare
/// equal.
#[test]
fn candidate_ids_order_by_path_then_deployment() {
    let mut ids = vec![
        CandidateId::new(8, DeploymentId::new(1)),
        CandidateId::new(7, DeploymentId::new(2)),
        CandidateId::new(7, DeploymentId::new(1)),
    ];
    ids.sort_unstable();

    assert_eq!(
        ids,
        vec![
            CandidateId::new(7, DeploymentId::new(1)),
            CandidateId::new(7, DeploymentId::new(2)),
            CandidateId::new(8, DeploymentId::new(1)),
        ]
    );
}

/// The operator-facing string form names both halves, so a log line cannot be
/// read as a bare path id.
#[test]
fn the_rendered_identity_names_the_deployment() {
    let candidate = CandidateId::new(7, DeploymentId::new(2));

    assert_eq!(candidate.to_string(), "7@2");
    assert_eq!(DeploymentId::new(2).to_string(), "2");
    assert_eq!(DeploymentId::new(2).value(), 2);
}
