//! The seat host's pure admission verdicts (ADR-079 D1): the total
//! admission predicate, the intake knobs, the no-progress guard, and the
//! grant-kind contract — functions that return results and read nothing
//! beyond their inputs, directly testable with no live host.

use degenbot_workers::dispatcher::GrantKind;
use std::time::Duration;
/// The complete, constructible input tuple of the admission predicate
/// (adversarial-review requirement 5): [`admission`] reads NOTHING else —
/// no shared map, no channel depth, no host borrow. Property tests
/// construct these tuples directly with no live host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AdmissionInputs {
    /// The bounded role queue's current occupancy (`FleetHost::queue_len`).
    pub(crate) queue_len: usize,
    /// The role's queue cap (`FleetHost::queue_cap`).
    pub(crate) queue_cap: usize,
    /// The live posture consult (`FleetHost::posture_admits_role`).
    pub(crate) posture_admits: bool,
}
/// The admission predicate's total output. Saturated and posture-held are
/// NOT progress states — they are the two independent WAIT reasons here,
/// and they can co-occur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admission {
    /// The backlog head may move to the role queue.
    Admit,
    /// Blocked by the cap alone.
    WaitCap,
    /// Blocked by the posture hold alone.
    WaitPosture,
    /// Blocked by both (the terms co-occur).
    WaitBoth,
}
impl Admission {
    /// Whether the unit may leave the backlog for the role queue.
    pub(crate) fn admits(self) -> bool {
        matches!(self, Admission::Admit)
    }
}
/// THE pure admission `predicate::` a
/// total function of [`AdmissionInputs`] ALONE. It is the ONLY gate on the
/// backlog → role-queue move; `try_enqueue`'s `PostureHeld` hand-back
/// stays the TOCTOU backstop for a cordon onset between the consult and the
/// enqueue (hold, never drop, never abort).
pub(crate) fn admission(inputs: AdmissionInputs) -> Admission {
    let at_cap = inputs.queue_len >= inputs.queue_cap;
    match (at_cap, !inputs.posture_admits) {
        (false, false) => Admission::Admit,
        (true, false) => Admission::WaitCap,
        (false, true) => Admission::WaitPosture,
        (true, true) => Admission::WaitBoth,
    }
}
/// The host intake backstop interval (T2): the recv-timeout armed iff a
/// backlog exists. Read from the installed typed config (schema defaults
/// when none installed); clamped to >= 1 ms so a garbage value can never
/// collapse into a busy-spin.
pub(crate) fn intake_backstop() -> Duration {
    Duration::from_millis(
        degenbot_substrate::stance::config()
            .fleet
            .intake_backstop_ms
            .max(1),
    )
}
/// The no-progress trip threshold K (T4): read from the installed typed
/// config, clamped to >= 1 so a garbage value cannot trip on the first
/// pass.
pub(crate) fn intake_no_progress_ticks() -> usize {
    degenbot_substrate::stance::config()
        .fleet
        .intake_no_progress_ticks
        .max(1)
}
/// The T4 no-progress guard: K CONSECUTIVE admitted-but-no-progress pump
/// passes trip the loud fail. A pass is "admitted" iff the backlog is
/// non-empty AND the pure admission predicate admits (so a legitimate
/// `WaitCap`/`WaitPosture` hold never accrues), and "progressed" iff the
/// backlog shrank or a grant was delivered. Only real progress (or an
/// un-admitted pass) resets the counter — an untrusted `PostureEdge` hint
/// runs no different code path, so it can never mask a livelock.
#[derive(Debug)]
pub(crate) struct NoProgressGuard {
    consecutive: usize,
    limit: usize,
}
/// One guard step's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoProgressStep {
    /// Progress was made, or the pass was legitimately un-admitted.
    Progressed,
    /// No progress this pass; `consecutive` of `limit` accrued.
    Ticking { consecutive: usize },
    /// K consecutive no-progress passes — the caller fails loudly.
    Tipping { consecutive: usize },
}
impl NoProgressGuard {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            consecutive: 0,
            limit: limit.max(1),
        }
    }
    pub(crate) fn limit(&self) -> usize {
        self.limit
    }
    #[cfg(test)]
    pub(crate) fn consecutive(&self) -> usize {
        self.consecutive
    }
    /// Fold one pump pass into the guard.
    pub(crate) fn step(&mut self, admitted: bool, progressed: bool) -> NoProgressStep {
        if !admitted || progressed {
            self.consecutive = 0;
            return NoProgressStep::Progressed;
        }
        self.consecutive += 1;
        if self.consecutive >= self.limit {
            NoProgressStep::Tipping {
                consecutive: self.consecutive,
            }
        } else {
            NoProgressStep::Ticking {
                consecutive: self.consecutive,
            }
        }
    }
}
/// Which dispatch grant kinds a host's `HostPump` is contracted to serve
/// (the row-#6 contract check, loud on all three hosts): a
/// grant outside this set is a broken host contract, never a seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrantContract {
    /// A pooled host serves exactly ONE grant kind — the descriptor's
    /// (`GrantKind::Sim` / `GrantKind::PoolStateUpdate`).
    Single(GrantKind),
    /// The solve host serves the Solver pair: a new pin claim (T1 keyed
    /// lease) or the SAME pin's continuation (T6) — both route to the
    /// grant slot's keyed seat.
    SolverPins,
}
impl GrantContract {
    /// The pure contract predicate (the test-declared tripwire's seam —
    /// the abort itself is a process abort, untestable in-process).
    #[must_use]
    pub(crate) fn admits(self, kind: GrantKind) -> bool {
        match self {
            Self::Single(expected) => kind == expected,
            Self::SolverPins => {
                matches!(kind, GrantKind::NewPinClaim | GrantKind::PinContinuation)
            }
        }
    }
}
