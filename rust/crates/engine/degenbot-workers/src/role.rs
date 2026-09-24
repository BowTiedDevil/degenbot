//! `WorkerRole` — the sized role enum and its policy attributes (ADR-042
//! design doc §3.1).
//!
//! Mirrors `degenbot-bot`'s `stage_handlers::ALL_STAGES` idiom: a sized
//! `const` table plus a u8 index used by the conformance stub. Adding a
//! role variant without declaring its `ALL_ROLES` position is a compile
//! error, and the conformance harness indexes roles by position, so any
//! new role or re-ordering fails the harness loudly.

/// Class deciding what cordon does to a role's lease intake (design doc §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CordonClass {
    /// Never cordoned: pinned latency-critical work (`Solver`, `Merge`,
    /// the never-deferrable `Submitter`) and pooled resolve work.
    Never,
    /// I/O-dominant pooled sims: in-flight sims are never cancelled, but
    /// new intake is floored at half the slot cap while cordoned.
    SimPool,
    /// Declared background roles (`PoolStateUpdater`, `Registrar`,
    /// `Verifier`): no new leases while cordoned.
    Deferrable,
}

/// Work a fleet worker slot can be leased for (design doc §3.1). The first
/// four variants are v1-active; the remainder are **declared now, hosted
/// later** — adding them later must be an entry here plus a posture/census
/// row, not a redesign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WorkerRole {
    /// One persistent pin per LPT bin (RAYPAR T3). Pinned by bin key; warm
    /// L1/L2 + allocator arenas survive across cycles.
    Solver,
    /// Pipelined inline sims behind the slot pool. Absorbs `SimSlots`'
    /// drivers and the per-cycle `arb-sim-*` spawn (the first pooling
    /// candidate). I/O-dominant: cordon throttles intake, never cancels.
    SimDriver,
    /// Path resolution (the resolve chunks; the retired rayon partition
    /// pool's consumers now run on scoped std threads). I/O-adjacent CPU.
    Resolve,
    /// The detached merge sidecar: drains the result pipe so per-path sends
    /// land in a pipe somebody drinks from. Pinned (exactly one, T4).
    Merge,
    /// Pool-state update application (deferrable: sheds artifact-free).
    /// Hosted (PRG-3): the registration intake station — bounded per-role
    /// unit pool of keyed build units, behind Solver precedence.
    PoolStateUpdater,
    /// Registration verify-lifecycle driver (deferrable). Declared now.
    Registrar,
    /// Published-edge verification reads (deferrable). Declared now.
    Verifier,
    /// Settlement submission delivery (never deferrable: latency-critical).
    /// Declared now.
    Submitter,
}

/// The sized role table — position is the conformance stub's u8 script
/// encoding (v1-active roles first, then declared-not-active exactly as
/// in the ADR's decision table).
pub const ALL_ROLES: [WorkerRole; 8] = [
    WorkerRole::Solver,
    WorkerRole::SimDriver,
    WorkerRole::Resolve,
    WorkerRole::Merge,
    WorkerRole::PoolStateUpdater,
    WorkerRole::Registrar,
    WorkerRole::Verifier,
    WorkerRole::Submitter,
];

/// The v1-active prefix of [`ALL_ROLES`] (ADR-042 Q2: Solver, `SimDriver`,
/// Resolve, Merge; PRG-3 adds `PoolStateUpdater` — the registration
/// intake station. The rest are declared gating only).
pub const V1_ACTIVE_ROLES: [WorkerRole; 5] = [
    WorkerRole::Solver,
    WorkerRole::SimDriver,
    WorkerRole::Resolve,
    WorkerRole::Merge,
    WorkerRole::PoolStateUpdater,
];

impl WorkerRole {
    /// What cordon does to this role's lease intake (design doc §6):
    /// `Never` roles lease freely, `SimPool` intake is floored, and
    /// `Deferrable` intake is held entirely while cordoned.
    #[must_use]
    pub const fn cordon_class(self) -> CordonClass {
        match self {
            Self::SimDriver => CordonClass::SimPool,
            Self::PoolStateUpdater | Self::Registrar | Self::Verifier => CordonClass::Deferrable,
            Self::Solver | Self::Resolve | Self::Merge | Self::Submitter => CordonClass::Never,
        }
    }

    /// True for the hosted roles (ADR-042 Q2 + PRG-3); false for
    /// declared-not-active.
    #[must_use]
    pub const fn v1_active(self) -> bool {
        matches!(
            self,
            Self::Solver | Self::SimDriver | Self::Resolve | Self::Merge | Self::PoolStateUpdater
        )
    }

    /// Roles whose steady lease is a pin held across cycles (T3/T4).
    #[must_use]
    pub const fn is_pinnable(self) -> bool {
        matches!(self, Self::Solver | Self::Merge)
    }

    /// Worker-census `resource` id (small closed set; extends the ids
    /// documented in `degenbot_core::worker_census`).
    #[must_use]
    pub const fn census_resource(self) -> &'static str {
        match self {
            Self::Solver => "fleet_solver_slots",
            Self::SimDriver => "fleet_simdriver_slots",
            Self::Resolve => "fleet_resolve_slots",
            Self::Merge => "fleet_merge_slots",
            Self::PoolStateUpdater => "fleet_pool_state_updater_slots",
            Self::Registrar => "fleet_registrar_slots",
            Self::Verifier => "fleet_verifier_slots",
            Self::Submitter => "fleet_submitter_slots",
        }
    }

    /// Distinct, greppable thread-name pattern (GOQWCL rule: never share a
    /// thread-name pattern across resources).
    #[must_use]
    pub const fn thread_name(self) -> &'static str {
        match self {
            Self::Solver => "work-fleet-solver-{n}",
            Self::SimDriver => "work-fleet-sim-{n}",
            Self::Resolve => "work-fleet-resolve-{n}",
            Self::Merge => "work-fleet-merge-{n}",
            Self::PoolStateUpdater => "work-fleet-poolupd-{n}",
            Self::Registrar => "work-fleet-registrar-{n}",
            Self::Verifier => "work-fleet-verifier-{n}",
            Self::Submitter => "work-fleet-submit-{n}",
        }
    }

    /// Worker-census `kind` text.
    #[must_use]
    pub const fn census_kind(self) -> &'static str {
        match self {
            Self::Solver => "fleet worker slot: LPT-bin pin (RAYPAR T3)",
            Self::SimDriver => "fleet worker slot: pooled sim driver",
            Self::Resolve => "fleet worker slot: resolve chunk",
            Self::Merge => "fleet worker slot: merge sidecar pin",
            Self::PoolStateUpdater => "fleet worker slot: pool-state updater",
            Self::Registrar => "fleet worker slot: registrar",
            Self::Verifier => "fleet worker slot: verifier",
            Self::Submitter => "fleet worker slot: submitter",
        }
    }

    /// Worker-census `sizing` rule text (who derives the count).
    #[must_use]
    pub const fn census_sizing(self) -> &'static str {
        match self {
            Self::Solver => "pins = one seat per LPT bin (floor(Q) minus the solve headroom, the structural bin count; walk admission stays the Solver share)",
            Self::SimDriver => "slot cap = 4 (today's SimSlots cap), duty-counted; fractional-quota remainder spendable here",
            Self::Resolve => "fixed v1 (1 slot, 12.4 ms/cycle measured)",
            Self::Merge => "exactly one sidecar",
            Self::PoolStateUpdater => {
                "slot cap = fleet.pool_state_updater_slots (default 4), duty-counted; \
                 fractional-quota remainder spendable here; Deferrable cordon class"
            }
            Self::Registrar | Self::Verifier | Self::Submitter => "declared, unhosted in v1",
        }
    }

    /// Lowercase label for metrics/gauges (small closed label set).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Solver => "solver",
            Self::SimDriver => "sim_driver",
            Self::Resolve => "resolve",
            Self::Merge => "merge",
            Self::PoolStateUpdater => "pool_state_updater",
            Self::Registrar => "registrar",
            Self::Verifier => "verifier",
            Self::Submitter => "submitter",
        }
    }

    /// u8 index into [`ALL_ROLES`] (the conformance stub's script encoding).
    #[must_use]
    pub const fn index_in_all_roles(self) -> Option<u8> {
        match self {
            Self::Solver => Some(0),
            Self::SimDriver => Some(1),
            Self::Resolve => Some(2),
            Self::Merge => Some(3),
            Self::PoolStateUpdater => Some(4),
            Self::Registrar => Some(5),
            Self::Verifier => Some(6),
            Self::Submitter => Some(7),
        }
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn all_roles_is_the_sized_8_role_table() {
        assert_eq!(ALL_ROLES.len(), 8, "ADR-042 Q2: exactly 8 declared roles");
        assert_eq!(
            ALL_ROLES,
            [
                WorkerRole::Solver,
                WorkerRole::SimDriver,
                WorkerRole::Resolve,
                WorkerRole::Merge,
                WorkerRole::PoolStateUpdater,
                WorkerRole::Registrar,
                WorkerRole::Verifier,
                WorkerRole::Submitter,
            ],
            "ALL_ROLES must list every variant exactly once, positions are the conformance script"
        );
    }

    #[test]
    fn u8_index_round_trips_through_the_table() {
        for (i, role) in ALL_ROLES.iter().enumerate() {
            let idx = usize::from(role.index_in_all_roles().expect("all roles indexed"));
            assert_eq!(idx, i, "index_in_all_roles must match ALL_ROLES position");
            assert_eq!(ALL_ROLES[idx], *role, "round trip");
        }
    }

    #[test]
    fn v1_active_is_exactly_the_hosted_prefix() {
        // ADR-042 Q2 hosted the first four; PRG-3 hosts PoolStateUpdater
        // (the registration intake station) as the fifth.
        for (i, role) in ALL_ROLES.iter().enumerate() {
            assert_eq!(
                role.v1_active(),
                i < V1_ACTIVE_ROLES.len(),
                "v1-active must be exactly {V1_ACTIVE_ROLES:?}"
            );
        }
        assert_eq!(V1_ACTIVE_ROLES, ALL_ROLES[..V1_ACTIVE_ROLES.len()]);
    }

    #[test]
    fn cordon_classes_match_the_sign_off_table() {
        // design doc §6: (b) sim-slot intake throttled; (c) declared
        // background roles held; merge pin + ambient I/O never cordoned;
        // Submitter never deferrable; Solver/Resolve unregulaged v1 roles.
        let class = |r: WorkerRole| match r.cordon_class() {
            CordonClass::Never => "never",
            CordonClass::SimPool => "sim-pool",
            CordonClass::Deferrable => "deferrable",
        };
        assert_eq!(class(WorkerRole::SimDriver), "sim-pool");
        for r in [
            WorkerRole::PoolStateUpdater,
            WorkerRole::Registrar,
            WorkerRole::Verifier,
        ] {
            assert_eq!(class(r), "deferrable", "declared background set: {r:?}");
        }
        for r in [
            WorkerRole::Solver,
            WorkerRole::Resolve,
            WorkerRole::Merge,
            WorkerRole::Submitter,
        ] {
            assert_eq!(class(r), "never", "cordon-invariant role: {r:?}");
        }
    }

    #[test]
    fn only_solver_and_merge_are_pinnable() {
        for role in ALL_ROLES {
            assert_eq!(
                role.is_pinnable(),
                matches!(role, WorkerRole::Solver | WorkerRole::Merge)
            );
        }
    }

    #[test]
    fn census_and_thread_names_are_distinct_and_greppable() {
        let mut resources = Vec::new();
        let mut threads = Vec::new();
        for role in ALL_ROLES {
            resources.push(role.census_resource());
            threads.push(role.thread_name());
            assert!(role.thread_name().starts_with("work-fleet-"));
            assert!(role.census_kind().starts_with("fleet worker slot"));
            assert!(!role.census_sizing().is_empty());
        }
        let n = resources.len();
        resources.sort_unstable();
        threads.sort_unstable();
        resources.dedup();
        threads.dedup();
        assert_eq!(resources.len(), n, "census resource ids must be unique");
        assert_eq!(
            threads.len(),
            n,
            "thread-name patterns must be unique (GOQWCL)"
        );
    }

    #[test]
    fn gauge_labels_are_snake_case() {
        for role in ALL_ROLES {
            let label = role.label();
            assert!(!label.is_empty());
            assert!(label.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        }
    }
}
