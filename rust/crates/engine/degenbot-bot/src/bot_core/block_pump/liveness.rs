//! The pump loop's independent liveness clock and its shutdown phase space.
//!
//! Two clocks watch a running pump, and they answer different questions:
//!
//! - The driver's `staleness_tick` (with the FSM's `WatchdogPhase`) answers "is
//!   the CHAIN quiet?" — a header or logs silence it recovers from by
//!   backfilling. That interval is polled by the loop's own `select!`, so it can
//!   only fire while the loop is parked AT that select.
//! - This module answers "is the LOOP making progress at all?". Arrivals are
//!   beaten on the pump's own task ([`LoopLiveness::beat`]) and observed from a
//!   SEPARATE task, so a park BEFORE the select — a `PySubscriberAdapter` GIL
//!   re-entry inside `drive_publish`, engine-lock contention inside the stage
//!   hooks — is reported instead of silent. Nothing the loop owns has to run for
//!   the observation to land.
//!
//! The stall is closed cooperatively: the observer raises the pump's shutdown
//! flag, and the loop unwinds at its next arrival. A close that needed the
//! parked task to act first could not land while it is parked, which is why the
//! abort belongs to the teardown's last resort
//! ([`LivenessPhase::Aborted`]), not to liveness coverage.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use degenbot_core::op_warn;

use crate::arb_engine::{Heartbeat, SessionEndCause, SessionEndDetection, SessionEndPublisher};

/// The window a live pump loop may go without an arrival before the independent
/// clock reports it stalled.
///
/// The number answers "how long is a legitimate non-arriving stretch?", and that
/// stretch is one inline await, not a cadence: `handle_timeout_eager` awaits a
/// single recovery `eth_getLogs` for its whole block range, bounded by
/// `BACKFILL_TIMEOUT_SECS` (60s). This window clears that with margin.
///
/// Every other stretch arrives. Each stream event is an arrival, and a loop
/// parked at its select still arrives on its own 500ms cooperative tick — so
/// silence for this long means the loop is parked BEFORE the select, in engine
/// or Python work that is not returning.
pub const PUMP_STALL_SECS: u64 = 90;

/// The pump loop's liveness/shutdown phase space.
///
/// The four states are the closed set of answers to "why is the loop not making
/// progress?": a stall lands in exactly one of them, and each names the
/// mechanism that owns the response — so a teardown log or an exit line says
/// whether the loop was healthy, recovering, closing, or aborted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LivenessPhase {
    /// The loop arrived inside the stall window: it is making progress.
    Progressing,
    /// A watchdog observed staleness and the driver is running the catch-up (the
    /// FSM's `WatchdogPhase` is not `Healthy`): the loop still owns the
    /// response, so this is not a stall.
    Recovering,
    /// The independent clock passed a whole window with no arrival and raised
    /// the shutdown flag: the loop unwinds at its next arrival. A cooperative
    /// close, not an abort.
    Closing,
    /// The cooperative close did not land inside the teardown grace and the
    /// owning driver aborted the task (`PumpExit::Aborted`). The last resort.
    Aborted,
}

impl LivenessPhase {
    /// Fold one loop arrival.
    #[must_use]
    pub const fn arrived(self) -> Self {
        match self {
            // A close request is not cancelled by a late arrival: the loop
            // still unwinds, so the phase must never read healthy again.
            Self::Closing | Self::Aborted => self,
            Self::Progressing | Self::Recovering => Self::Progressing,
        }
    }

    /// Fold one staleness recovery the loop is running (its catch-up backfill).
    #[must_use]
    pub const fn recovering(self) -> Self {
        match self {
            Self::Progressing => Self::Recovering,
            Self::Recovering | Self::Closing | Self::Aborted => self,
        }
    }

    /// Fold the independent clock's stall observation: a whole window passed
    /// with no arrival, so the close request is the cooperative unwind.
    #[must_use]
    pub const fn stalled(self) -> Self {
        match self {
            Self::Aborted => Self::Aborted,
            Self::Progressing | Self::Recovering | Self::Closing => Self::Closing,
        }
    }

    /// The stable spelling the exit and teardown logs carry.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Progressing => "Progressing",
            Self::Recovering => "Recovering",
            Self::Closing => "Closing",
            Self::Aborted => "Aborted",
        }
    }
}

/// The pump loop's independent clock: the arrival heartbeat plus the observer
/// task that reports its silence.
///
/// Owned by the loop body, so the clock's lifetime IS the loop's: dropping it
/// drops the observation (`SessionEndDetection` aborts its observer), and a
/// clean loop end publishes no stall fact.
#[derive(Debug)]
pub(crate) struct LoopLiveness {
    heartbeat: Heartbeat,
    detection: SessionEndDetection,
}

impl LoopLiveness {
    /// Arm the clock. `on_stall` (raise the cooperative close, publish the fact)
    /// runs on the OBSERVER's task once a whole `window` passes with no
    /// [`Self::beat`].
    pub(crate) fn arm(
        shutdown: Arc<AtomicBool>,
        stall: &SessionEndPublisher,
        window: Duration,
    ) -> Self {
        let heartbeat = Heartbeat::new();
        let publisher = stall.clone();
        let stall_ms = u64::try_from(window.as_millis()).unwrap_or(u64::MAX);
        let detection = SessionEndDetection::stall_with(heartbeat.clone(), window, move || {
            op_warn!(
                domain = pump,
                stall_ms,
                "pump loop made no arrival inside the stall window — parked before its select \
                 (Python GIL re-entry or engine-lock contention); raising the cooperative close"
            );
            // The fact lands BEFORE the flag: the loop's exit is what wakes a
            // `wait_session_end` waiter, so a waiter that sees the end must
            // already see the reason it ended.
            publisher.publish(SessionEndCause::StallWatchdogTripped);
            shutdown.store(true, Ordering::Relaxed);
        });
        Self {
            heartbeat,
            detection,
        }
    }

    /// Record one loop arrival. Cheap by contract: the loop beats at each
    /// iteration head, before any work that can block.
    pub(crate) fn beat(&self) {
        self.heartbeat.beat();
    }

    /// Whether this clock published its stall fact — the close IT requested.
    pub(crate) fn stalled(&self) -> bool {
        self.detection.facts().current().is_some()
    }

    /// The phase a shutdown landing reports: `Closing` when this clock's stall is
    /// why the loop is unwinding, else the loop's own phase.
    pub(crate) fn landing(&self, phase: LivenessPhase) -> LivenessPhase {
        if self.stalled() {
            phase.stalled()
        } else {
            phase
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use degenbot_ingestion::watchdog::HEADER_STALENESS_SECS;
    use degenbot_ingestion::BACKFILL_TIMEOUT_SECS;

    const _: () = {
        assert!(
            PUMP_STALL_SECS > BACKFILL_TIMEOUT_SECS,
            "the window must clear one inline recovery backfill, or a recovering pump would \
             be closed as a stalled one"
        );
        assert!(
            PUMP_STALL_SECS > HEADER_STALENESS_SECS,
            "the window must clear the in-loop staleness interval — a loop parked at its \
             select arrives on that tick"
        );
    };

    #[test]
    fn an_arrival_returns_a_recovering_loop_to_progress() {
        assert_eq!(
            LivenessPhase::Progressing.recovering(),
            LivenessPhase::Recovering
        );
        assert_eq!(
            LivenessPhase::Recovering.arrived(),
            LivenessPhase::Progressing
        );
        assert_eq!(
            LivenessPhase::Progressing.arrived(),
            LivenessPhase::Progressing
        );
    }

    #[test]
    fn a_close_request_is_sticky_across_arrivals_and_recoveries() {
        let closing = LivenessPhase::Progressing.stalled();
        assert_eq!(closing, LivenessPhase::Closing);
        assert_eq!(closing.arrived(), LivenessPhase::Closing);
        assert_eq!(closing.recovering(), LivenessPhase::Closing);
        assert_eq!(closing.stalled(), LivenessPhase::Closing);
        assert_eq!(
            LivenessPhase::Recovering.stalled(),
            LivenessPhase::Closing,
            "a stall that lands mid-recovery still closes"
        );
        assert_eq!(
            LivenessPhase::Aborted.stalled().arrived().name(),
            "Aborted",
            "an aborted loop never reads closing or healthy again"
        );
    }

    #[test]
    fn the_phase_names_are_stable() {
        assert_eq!(LivenessPhase::Progressing.name(), "Progressing");
        assert_eq!(LivenessPhase::Recovering.name(), "Recovering");
        assert_eq!(LivenessPhase::Closing.name(), "Closing");
        assert_eq!(LivenessPhase::Aborted.name(), "Aborted");
    }
}
