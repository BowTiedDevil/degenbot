//! The role-switching worker fleet core .
//!
//! One generalized, bounded worker fleet whose workers **switch roles** —
//! hosting every execution resource the bot needs — replacing the per-era
//! pile of independently-sized parallelism mechanisms. This crate is the
//! F1+F2 migration steps plus the posture skeleton:
//!
//! - [`role`] — the sized `WorkerRole` enum (`ALL_ROLES`), its cordon classes
//!   and pin classes (ADR-042 §3.1, design doc §3.1).
//! - [`slot`] — worker-slot states and the T1–T9 legal-transition table;
//!   every off-table move is a loud, typed rejection (design doc §3.2–3.3).
//! - [`budget`] — `FleetBudget`, the single budget authority bounding the
//!   SUM of every consumer's declared peak share against the cgroup quota
//!   (design doc §5), with floor-allocation of the fractional quota.
//! - [`quota`] — the fleet-local fractional (pre-ceil) cgroup-quota
//!   detector: `floor`-allocation's input `Q` (see `budget`).
//! - [`posture`] — the `Nominal ⇄ Cordoned` posture FSM consuming
//!   `cpu_budget::cgroup_throttle_delta` deltas; typed, tunable thresholds
//!   (sign-off amendment 2026-09-09) (design doc §6).
//! - [`dispatcher`] — per-role bounded queues, sim-before-solve lease
//!   precedence, and the slot host that grants leases only along the
//!   T-table (design doc §3–§4). No production callers yet: the
//!   conformance harness (`NoopStubFleetHost` mirror) is the consumer.
//! - [`gauges`] — per-role busy/idle gauge surface for the activation
//!   dashboard (the `instruments.rs` mirror seam), plus worker-census
//!   self-registration of fleet slots (design doc §7).
//!
//! Engine-agnostic by construction: it depends on `degenbot-core`
//! (`cpu_budget` + `worker_census`) and `degenbot-config` ONLY. The engine
//! plugs roles in as closures/tasks in later migration steps (F3–F5);
//! there is deliberately no pyo3 dependency — simulation never round-trips
//! Python and the FFI is crossed only for runtime/startup concerns
//! (design doc §8).

pub mod budget;
pub mod dispatcher;
pub mod gauges;
pub mod lane;
pub mod plan;
pub mod posture;
pub mod quota;
pub mod role;
pub mod slot;
