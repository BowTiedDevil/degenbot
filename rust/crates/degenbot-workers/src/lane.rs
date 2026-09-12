//! Seat context (LW-T2, Seam B): a lane unit takes ALL external capability
//! through a passed `LaneCtx` — never through ambient state, and seats run
//! with NO ambient tokio runtime (the `Handle::try_current() == Err` wedge
//! test pins this structurally). Pyo3-free by construction (design doc §8).
//!
//! Seam C (LW-T3): escalation is an injected, typed, BOUNDED port — the
//! cold-miss budget is the capacity ceiling and is visible at the port's
//! gauge surface; escalated work drives on the port's own capability lane
//! (CPU cannot starve I/O) and propagates the caller's span context.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tracing::warn;

use crate::dispatcher::ArenaToken;
use crate::slot::PinKey;

/// One escalated work item: driven on the port's own capability lane, never
/// on the caller's CPU seat.
pub type EscalationWork = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Typed escalation failure (LW-T3, Seam C). Fail-fast over fail-slow: a
/// port NEVER hangs a lane unit waiting for out-of-band capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationError {
    /// The cold-miss budget (the escalation capacity ceiling,
    /// incident-2026-08-20) is exhausted: refuse synchronously.
    ColdMissBudgetExceeded,
    /// No escalation port is installed / the port lane is closed.
    PortClosed,
}

/// The port's gauge surface: escalation traffic is VISIBLE — monotone
/// refusal/completion counters plus the live in-flight gauge (mirrors the
/// `gauges.rs` read-out convention).
#[derive(Debug, Default)]
pub struct EscalationCounters {
    /// Escalations admitted and completed successfully.
    pub completed: AtomicU64,
    /// Escalations refused for exceeding the cold-miss budget.
    pub budget_exceeded: AtomicU64,
    /// Escalations admitted and still running on the port lane.
    pub in_flight: AtomicU64,
}

/// One read-out of [`EscalationCounters`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EscalationCountersSnapshot {
    /// Live escalations on the port lane.
    pub in_flight: u64,
    /// Completed escalations.
    pub completed: u64,
    /// Refused escalations (budget exceeded).
    pub budget_exceeded: u64,
}

impl EscalationCounters {
    /// One read-out of all three counters.
    #[must_use]
    pub fn snapshot(&self) -> EscalationCountersSnapshot {
        EscalationCountersSnapshot {
            in_flight: self.in_flight.load(Ordering::Relaxed),
            completed: self.completed.load(Ordering::Relaxed),
            budget_exceeded: self.budget_exceeded.load(Ordering::Relaxed),
        }
    }
}

/// The cold-miss budget gate — SHARED fail-fast admission control for every
/// port impl (the budget IS the escalation capacity ceiling,
/// incident-2026-08-20; exceeding it refuses synchronously, never a hang).
pub struct EscalationGate {
    budget: usize,
    counters: EscalationCounters,
}

/// The admission permit for ONE escalation: retires the in-flight slot ON
/// DROP — completion, panic and cancel all decrement IDENTICALLY (reth's
/// `IncCounterOnDrop` model: tie the counter to the admission's lifetime,
/// never to control flow; a leaked slot would be a budget leak into
/// permanent hard-refusal — the raddest fail-fast failure mode).
pub struct FinishOnDrop {
    gate: Arc<EscalationGate>,
    completed: bool,
}

impl FinishOnDrop {
    /// Mark the escalated work COMPLETED — EXACTLY the drop arm (taking
    /// `self` by value ends its lifetime here, so `Drop::drop` runs once;
    /// hand-writing a second decrement here would double-fire the in-flight
    /// gauge into permanent hard-refusal).
    pub fn retire(mut self) {
        self.completed = true;
    }
}

impl std::fmt::Debug for FinishOnDrop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FinishOnDrop").finish_non_exhaustive()
    }
}

impl Drop for FinishOnDrop {
    fn drop(&mut self) {
        // THE reclaim (LW-T3, reth `IncCounterOnDrop`): completion, panic
        // and cancel all land HERE, exactly once per admission — the
        // in-flight slot can never leak, and only completed work counts as
        // completed (a CANCELLED escalation is reclaimed, never tallied).
        self.gate.finish(self.completed);
    }
}

impl EscalationGate {
    /// Gate over `cold_miss_budget` concurrent escalations.
    #[must_use]
    pub fn new(cold_miss_budget: usize) -> Arc<Self> {
        Arc::new(Self {
            budget: cold_miss_budget,
            counters: EscalationCounters::default(),
        })
    }

    /// Admit one escalation: fail-fast beyond the cold-miss budget.
    ///
    /// # Errors
    /// [`EscalationError::ColdMissBudgetExceeded`] when the budget is out.
    pub fn begin(self: &Arc<Self>) -> Result<FinishOnDrop, EscalationError> {
        // The cold-miss budget IS the capacity ceiling (incident-2026-08-20):
        // a refusal is SYNCHRONOUS and counted — never a hang, never a drop.
        if self.counters.in_flight.load(Ordering::Relaxed)
            >= u64::try_from(self.budget).unwrap_or(u64::MAX)
        {
            self.counters
                .budget_exceeded
                .fetch_add(1, Ordering::Relaxed);
            return Err(EscalationError::ColdMissBudgetExceeded);
        }
        self.counters.in_flight.fetch_add(1, Ordering::Relaxed);
        Ok(FinishOnDrop {
            gate: Arc::clone(self),
            completed: false,
        })
    }

    /// Retire one admitted escalation: the in-flight slot re-frees; only
    /// COMPLETED work tallies into `completed` (a cancelled admission is
    /// reclaimed without ever counting as work done).
    fn finish(&self, completed: bool) {
        self.counters.in_flight.fetch_sub(1, Ordering::Relaxed);
        if completed {
            self.counters.completed.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The gauge surface: counts visible without serialising the port.
    #[must_use]
    pub fn counters(&self) -> EscalationCountersSnapshot {
        self.counters.snapshot()
    }
}

/// The injected escalation port (Seam C): typed, bounded, SELF-CONTAINED —
/// escalated work drives on the port's own capability lane (the default
/// impl: the degenbot-inline-sim runtime, registered at hook install where
/// pyo3 is fine) and NEVER occupies the caller's CPU seat (reth research
/// §7: CPU cannot starve I/O).
pub trait EscalationPort: Send + Sync + 'static {
    /// Submit ONE escalated work item. Fail-fast beyond the cold-miss
    /// budget: [`EscalationError::ColdMissBudgetExceeded`] returns
    /// synchronously — never a hang.
    ///
    /// # Errors
    /// [`EscalationError::ColdMissBudgetExceeded`] / [`EscalationError::PortClosed`].
    fn escalate(&self, work: EscalationWork) -> Result<(), EscalationError>;

    /// The live gauge surface.
    fn counters(&self) -> EscalationCountersSnapshot;
}

/// The pooled-seat stub: pooled lanes have no escalation capability —
/// asking for it is REFUSED with the typed error, never silently dropped.
#[derive(Debug, Default)]
pub struct NoEscalationPort;

impl EscalationPort for NoEscalationPort {
    fn escalate(&self, _work: EscalationWork) -> Result<(), EscalationError> {
        Err(EscalationError::PortClosed)
    }

    fn counters(&self) -> EscalationCountersSnapshot {
        EscalationCountersSnapshot::default()
    }
}

/// The installed DEFAULT escalation port (the degenbot-inline-sim runtime,
/// registered at `install_inline_simulator` where pyo3 is fine — ADR-042
/// §8: no worker crate ever depends on pyo3).
static DEFAULT_ESCALATION_PORT: Mutex<Option<Arc<dyn EscalationPort>>> = Mutex::new(None);

/// Register the default escalation port. Replacing an installed port is
/// loud (the constructor-once discipline: first install wins unless the
/// installer explicitly re-registers).
pub fn install_default_escalation_port(port: Arc<dyn EscalationPort>) {
    let mut guard = DEFAULT_ESCALATION_PORT
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if guard.is_some() {
        warn!(
            target: "degenbot::fleet",
            "default escalation port re-installed — the previous port is replaced"
        );
    }
    *guard = Some(port);
}

/// The installed default escalation port, if any.
#[must_use]
pub fn default_escalation_port() -> Option<Arc<dyn EscalationPort>> {
    DEFAULT_ESCALATION_PORT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// The no-port stub as a handle: for lanes that must never escalate (the
/// `escalate` call returns [`EscalationError::PortClosed`], typed).
#[must_use]
pub fn no_escalation_port() -> Arc<dyn EscalationPort> {
    Arc::new(NoEscalationPort)
}

/// Cooperative quit signal for a lane (stub; later seams refine).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuitSig;

/// The per-seat unit context, handed to the unit body by the seat — never
/// constructed ambiently. The `arena` identity is warm across cycles while
/// the pin lives and freshly minted after a T9 role switch (an arena is
/// never live across a role switch, design doc §3.4).
#[derive(Clone)]
pub struct LaneCtx {
    // Debug is manual (the dyn port is not Debug)
    /// The lane's pinned bin key: the pin IS the key, so the ctx names the
    /// bin the unit is running for.
    pub pin: PinKey,
    /// The seat's warm arena token (`DETACHED` for pooled seats, which
    /// carry no warm arena).
    pub arena: ArenaToken,
    /// The injected escalation port (LW-T3, Seam C): typed, bounded,
    /// self-contained; never the caller's CPU seat.
    pub escalation: Arc<dyn EscalationPort>,
    /// Cooperative quit signal (stub).
    pub quit: QuitSig,
}

impl std::fmt::Debug for LaneCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaneCtx")
            .field("pin", &self.pin)
            .field("arena", &self.arena)
            .field("escalation", &"dyn EscalationPort")
            .field("quit", &self.quit)
            .finish()
    }
}

impl LaneCtx {
    /// The stub context for pooled seats (no warm arena, no pin, no
    /// escalation capability): the pinned solve lanes hand the real
    /// pin-bound ctx at the T2 grant seam (LW-T8 landed: the executors
    /// submit through the ONE Executor seam).
    #[must_use]
    pub fn detached() -> Self {
        Self {
            pin: 0,
            arena: ArenaToken::DETACHED,
            escalation: no_escalation_port(),
            quit: QuitSig,
        }
    }

    /// Escalate one work item through the injected port — fail-fast beyond
    /// the cold-miss budget, typed error (never a hang, never a panic).
    ///
    /// # Errors
    /// [`EscalationError`] — see [`EscalationPort::escalate`].
    pub fn escalate(&self, work: EscalationWork) -> Result<(), EscalationError> {
        self.escalation.escalate(work)
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod lane_tests {
    use super::*;

    /// LW-T3 (Seam C): the cold-miss budget IS the escalation capacity
    /// ceiling — beyond it the port refuses SYNCHRONOUSLY (typed, fail-fast,
    /// never a hang) and the refusal is visible on the gauge surface;
    /// completing escalations re-free capacity (deterministic).
    #[test]
    fn escalation_cold_miss_budget_is_the_synchronous_fail_fast_ceiling() {
        let gate = EscalationGate::new(2);
        let first = gate.begin().expect("first escalation within budget");
        let second = gate.begin().expect("second escalation within budget");
        let refused = gate
            .begin()
            .expect_err("the budget ceiling must refuse SYNCHRONOUSLY, not hang");
        assert_eq!(refused, EscalationError::ColdMissBudgetExceeded);
        assert_eq!(
            gate.counters().budget_exceeded,
            1,
            "the refusal must be visible on the gauge surface"
        );
        first.retire();
        drop(second);
        gate.begin()
            .expect("completed escalations re-free capacity");
    }

    /// LW-T3 green arm (reth `IncCounterOnDrop`): in-flight slots are
    /// reclaimed ON DROP — cancel-before-completion reclaims identically
    /// to completion; a leaked slot would be a budget leak into permanent
    /// hard-refusal (the raddest fail-fast failure mode).
    #[test]
    fn escalation_in_flight_is_reclaimed_on_drop_and_cancel() {
        let gate = EscalationGate::new(2);
        {
            let permit = gate.begin().expect("admitted");
            drop(permit); // cancel before any completion signal
            assert_eq!(
                gate.counters().in_flight,
                0,
                "cancel reclaims the slot (finish-on-drop)"
            );
        }
        let live = gate
            .begin()
            .expect("cancel must not leak into permanent hard-refusal");
        assert_eq!(
            gate.counters().in_flight,
            1,
            "the re-admitted escalation holds its slot while live"
        );
        assert_eq!(
            gate.counters().completed,
            0,
            "a CANCELLED escalation is never counted completed"
        );
        live.retire();
        assert_eq!(gate.counters().in_flight, 0, "retire frees the last slot");
    }

    /// LW-T3 (Seam C): a lane without an installed port (or a pooled seat's
    /// detached stub) refuses escalation with the TYPED error — never a
    /// panic, never a silent drop.
    #[test]
    fn a_lane_without_an_installed_port_refuses_escalation_with_a_typed_error() {
        let ctx = LaneCtx::detached();
        let refused = ctx
            .escalate(Box::pin(std::future::ready(())))
            .expect_err("no port installed — must refuse typed");
        assert_eq!(refused, EscalationError::PortClosed);
        // And the no-port handle agrees: lanes that must never escalate are
        // refused identically.
        let refused = no_escalation_port()
            .escalate(Box::pin(std::future::ready(())))
            .expect_err("the no-port stub refuses typed");
        assert_eq!(refused, EscalationError::PortClosed);
    }
}
