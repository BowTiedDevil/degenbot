//! `pump_session` — the pump session's lifecycle, as one machine.
//!
//! The session used to be split across two owners with no table: a bare phase
//! byte the engine stored, and five independently-mutated fields on the driver
//! (`pump_handle`, `pump_finished_tx`, `subscribe_state`, `stopped`,
//! `result_receiver_taken`). Legality was decided from the driver's fields and
//! asserted against the engine's byte, which had to be kept in sync by hand.
//!
//! The phase could not be an ordering because the session is not a ladder:
//! `subscribe` is legal from `Detached` **or** from a phase that already loaded
//! a snapshot, because the snapshot may be loaded before or after the WS
//! subscribe. A comparison cannot say that, so the old code carried four
//! legality functions and a raw setter whose doc said "the caller validates".
//!
//! Here the states are named, and [`transition`] is the whole legality table:
//! one total function over (pose, event) that answers every pair. Anything not
//! allowed is a typed [`PumpDecline`], never a silent no-op.

use std::fmt;

/// Where a pump session is. A `Copy` projection: readers see this, never a
/// handle into the session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PumpPhase {
    /// No WS connection and nothing pending.
    #[default]
    Detached,
    /// The subscribe handshake is in flight.
    Connecting,
    /// The first complete block is observed and the stream is open; `resume`
    /// is legal.
    Subscribed,
    /// The `S+1..W` backfill is in flight.
    Resuming,
    /// The live loop is spawned and running.
    Running,
    /// The live loop returned without a `Stop` — stream end, panic, or abort.
    Ended,
    /// A resume did not commit — cancelled mid-flight, or its `S+1..W` backfill
    /// failed. Either way the WS session is gone and no live loop runs; the
    /// only way forward is a fresh `subscribe`.
    Abandoned,
    /// `Stop` latched; teardown is in progress.
    Stopping,
    /// Terminal.
    Stopped,
}

impl PumpPhase {
    /// The lower-case name, for the FFI's name translator and for error text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Detached => "detached",
            Self::Connecting => "connecting",
            Self::Subscribed => "subscribed",
            Self::Resuming => "resuming",
            Self::Running => "running",
            Self::Ended => "ended",
            Self::Abandoned => "abandoned",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
        }
    }

    /// Whether teardown has begun or finished.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Stopping | Self::Stopped)
    }

    /// Whether a live loop exists or is being started — the fact teardown
    /// needs to decide whether anything must be joined.
    #[must_use]
    pub const fn live_loop_armed(self) -> bool {
        matches!(self, Self::Resuming | Self::Running)
    }

    /// Whether the session holds a WS subscription (directly, or through the
    /// live loop that owns the pump).
    #[must_use]
    pub const fn holds_subscription(self) -> bool {
        matches!(
            self,
            Self::Connecting | Self::Subscribed | Self::Resuming | Self::Running
        )
    }
}

impl fmt::Display for PumpPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Display for PumpEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What happened. Events carry no payload: the payload moves are the caller's
/// (a guard hands the pump and the ticket over), while this table owns
/// legality.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PumpEvent {
    /// Open the WS subscribe handshake.
    SubscribeRequested,
    /// The handshake completed and produced a resumable session.
    Subscribed,
    /// The handshake failed.
    SubscribeFailed,
    /// The future driving the handshake was dropped before it committed.
    SubscribeAbandoned,
    /// Begin the backfill and start the live loop.
    ResumeRequested,
    /// The live loop was spawned.
    ResumeSpawned,
    /// The resume did not commit: the future driving it was dropped mid-flight,
    /// or its `S+1..W` backfill returned `Err`.
    ResumeAbandoned,
    /// The live loop returned on its own.
    PumpExited,
    /// Tear the session down.
    Stop,
    /// Teardown finished.
    StopCompleted,
}

impl PumpEvent {
    /// The lower-case name, for error text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SubscribeRequested => "subscribe",
            Self::Subscribed => "subscribed",
            Self::SubscribeFailed => "subscribe_failed",
            Self::SubscribeAbandoned => "subscribe_abandoned",
            Self::ResumeRequested => "resume",
            Self::ResumeSpawned => "resume_spawned",
            Self::ResumeAbandoned => "resume_abandoned",
            Self::PumpExited => "pump_exited",
            Self::Stop => "stop",
            Self::StopCompleted => "stop_completed",
        }
    }
}

/// Why a session move was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PumpDecline {
    /// `subscribe` is legal only from [`PumpPhase::Detached`], or from
    /// [`PumpPhase::Abandoned`] where it is the recovery edge.
    #[error("cannot subscribe from {current}; subscribe requires detached")]
    SubscribeRequiresDetached {
        /// The pose the request arrived in.
        current: PumpPhase,
    },
    /// `resume` is legal only from [`PumpPhase::Subscribed`].
    #[error("cannot resume from {current}; resume requires subscribed")]
    ResumeRequiresSubscribed {
        /// The pose the request arrived in.
        current: PumpPhase,
    },
    /// A resume was abandoned and the WS session is gone.
    #[error("the previous resume was abandoned; subscribe again before resuming")]
    ResumeAfterAbandonment,
    /// The event does not apply in the current pose.
    #[error("cannot apply {event} from {current}")]
    EventOutOfOrder {
        /// The pose the event arrived in.
        current: PumpPhase,
        /// The event that does not apply.
        event: PumpEvent,
    },
    /// The session is terminal.
    #[error("cannot apply {event}: the session is {current}")]
    Stopped {
        /// The terminal pose.
        current: PumpPhase,
        /// The event that was refused.
        event: PumpEvent,
    },
}

/// The whole legality table: (pose, event) -> next pose, or a typed decline.
///
/// Total over every pair — no combination is undefined and none is a silent
/// no-op. `Stop` is the only event that never declines, because teardown is
/// legal from every pose.
///
/// # Errors
///
/// [`PumpDecline::SubscribeRequiresDetached`] when `subscribe` arrives at a
/// session that is already past `Detached` and not recovering from
/// [`PumpPhase::Abandoned`]; [`PumpDecline::ResumeRequiresSubscribed`] when
/// `resume` arrives without a resumable session;
/// [`PumpDecline::ResumeAfterAbandonment`] when a resume is attempted after a
/// cancelled one; [`PumpDecline::EventOutOfOrder`] for an event the pose does
/// not expect; and [`PumpDecline::Stopped`] against a session that is tearing
/// down or already stopped.
pub fn transition(pose: PumpPhase, event: PumpEvent) -> Result<PumpPhase, PumpDecline> {
    use PumpEvent as E;
    use PumpPhase as P;

    // Teardown is total. It declines nothing and never leaves a terminal pose.
    if matches!(event, E::Stop) {
        return Ok(match pose {
            P::Detached | P::Ended | P::Abandoned | P::Stopped => P::Stopped,
            P::Connecting | P::Subscribed | P::Resuming | P::Running | P::Stopping => P::Stopping,
        });
    }

    // `Stopped` is absorbing: it refuses everything else, loudly and by name.
    // `Stopping` is not absorbing — it still owes one `StopCompleted`.
    if pose == P::Stopped {
        return Err(PumpDecline::Stopped {
            current: pose,
            event,
        });
    }

    match (pose, event) {
        // subscribe: the one door in. `Abandoned` is the recovery edge — the
        // only way forward after a cancelled resume, since its WS session is
        // gone.
        (P::Detached | P::Abandoned, E::SubscribeRequested) => Ok(P::Connecting),

        // handshake outcome
        (P::Connecting, E::Subscribed) => Ok(P::Subscribed),
        (P::Connecting, E::SubscribeFailed | E::SubscribeAbandoned) => Ok(P::Detached),

        // resume
        (P::Subscribed, E::ResumeRequested) => Ok(P::Resuming),
        (P::Resuming, E::ResumeSpawned) => Ok(P::Running),
        // Cancelled or backfill-failed: both leave the pump and its WS session
        // gone with nothing to resume, so `Abandoned` is the honest landing.
        // A failure's typed error travels beside the pose (the caller's
        // `Err`), never through it — the table carries no payloads.
        (P::Resuming, E::ResumeAbandoned) => Ok(P::Abandoned),

        // the live loop returned without a stop
        (P::Running, E::PumpExited) => Ok(P::Ended),

        // teardown completion
        (P::Stopping, E::StopCompleted) => Ok(P::Stopped),

        // A tearing-down session owes one `StopCompleted` and nothing else.
        (P::Stopping, _) => Err(PumpDecline::Stopped {
            current: pose,
            event,
        }),

        // A cancelled resume cannot be retried: the pump and its WS session
        // were dropped with the future.
        (P::Abandoned, E::ResumeRequested) => Err(PumpDecline::ResumeAfterAbandonment),

        // `subscribe` from a live session is not a recovery, it is a mistake.
        (_, E::SubscribeRequested) => Err(PumpDecline::SubscribeRequiresDetached { current: pose }),

        // `resume` anywhere but `Subscribed` has nothing to resume.
        (_, E::ResumeRequested) => Err(PumpDecline::ResumeRequiresSubscribed { current: pose }),

        // Everything else is an event arriving out of order.
        _ => Err(PumpDecline::EventOutOfOrder {
            current: pose,
            event,
        }),
    }
}

/// One published transition: the pose the session held and the pose the table
/// landed it on.
///
/// [`transition`] answers legality; this is the answer of a projection that has
/// already written the landing, so a caller can tell "I began this move" from
/// "the session was already there" (teardown idempotence reads the pair that
/// way).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PhaseAdvance {
    /// The pose the event arrived in.
    pub(crate) from: PumpPhase,
    /// The pose the table landed on.
    pub(crate) to: PumpPhase,
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;
    use PumpEvent as E;
    use PumpPhase as P;

    const ALL_POSES: [PumpPhase; 9] = [
        P::Detached,
        P::Connecting,
        P::Subscribed,
        P::Resuming,
        P::Running,
        P::Ended,
        P::Abandoned,
        P::Stopping,
        P::Stopped,
    ];

    const ALL_EVENTS: [PumpEvent; 10] = [
        E::SubscribeRequested,
        E::Subscribed,
        E::SubscribeFailed,
        E::SubscribeAbandoned,
        E::ResumeRequested,
        E::ResumeSpawned,
        E::ResumeAbandoned,
        E::PumpExited,
        E::Stop,
        E::StopCompleted,
    ];

    #[test]
    fn every_state_answers_every_event() {
        // Totality over the full 9x10 grid: no pair is undefined and no pair is
        // a silent no-op. The count pins the table's SHAPE, so an edge added or
        // removed in one place has to be acknowledged here.
        let mut allowed = 0;
        for pose in ALL_POSES {
            for event in ALL_EVENTS {
                if transition(pose, event).is_ok() {
                    allowed += 1;
                }
            }
        }
        // 9 poses admit `Stop`, plus the ten named edges of the graph.
        assert_eq!(allowed, 19, "the graph's allowed edge count changed");
    }

    #[test]
    fn subscribe_is_legal_from_detached_and_from_abandoned_only() {
        for pose in ALL_POSES {
            let allowed = matches!(pose, P::Detached | P::Abandoned);
            assert_eq!(
                transition(pose, E::SubscribeRequested).ok(),
                allowed.then_some(P::Connecting),
                "subscribe from {pose}"
            );
        }
    }

    #[test]
    fn resume_is_legal_exactly_when_subscribed() {
        for pose in ALL_POSES {
            let verdict = transition(pose, E::ResumeRequested);
            let expected = match pose {
                P::Subscribed => Ok(P::Resuming),
                P::Abandoned => Err(PumpDecline::ResumeAfterAbandonment),
                P::Stopping | P::Stopped => Err(PumpDecline::Stopped {
                    current: pose,
                    event: E::ResumeRequested,
                }),
                _ => Err(PumpDecline::ResumeRequiresSubscribed { current: pose }),
            };
            assert_eq!(verdict, expected, "resume from {pose}");
        }
    }

    #[test]
    fn stop_is_total_and_never_declines() {
        for pose in ALL_POSES {
            let verdict = transition(pose, E::Stop).expect("stop never declines");
            assert!(
                matches!(verdict, P::Stopping | P::Stopped),
                "stop from {pose} landed on {verdict}"
            );
        }
    }

    #[test]
    fn stopped_is_absorbing() {
        for event in ALL_EVENTS {
            if event == E::Stop {
                assert_eq!(transition(P::Stopped, event), Ok(P::Stopped));
                continue;
            }
            assert_eq!(
                transition(P::Stopped, event),
                Err(PumpDecline::Stopped {
                    current: P::Stopped,
                    event
                }),
                "{event} against a stopped session"
            );
        }
    }

    #[test]
    fn a_live_loop_that_ends_on_its_own_reaches_ended() {
        assert_eq!(transition(P::Running, E::PumpExited), Ok(P::Ended));
        // And `Ended` is not a live loop any more, so a later stop is immediate.
        assert_eq!(transition(P::Ended, E::Stop), Ok(P::Stopped));
    }

    #[test]
    fn abandoned_is_reachable_only_by_a_cancelled_resume() {
        // Its one entrance.
        assert_eq!(
            transition(P::Resuming, E::ResumeAbandoned),
            Ok(P::Abandoned)
        );
        // And nothing else lands there.
        for pose in ALL_POSES {
            if pose == P::Resuming {
                continue;
            }
            for event in ALL_EVENTS {
                assert_ne!(
                    transition(pose, event),
                    Ok(P::Abandoned),
                    "({pose}, {event}) reached abandoned"
                );
            }
        }
    }

    #[test]
    fn abandoned_has_exactly_two_exits() {
        let exits: Vec<PumpPhase> = ALL_EVENTS
            .iter()
            .filter_map(|e| transition(P::Abandoned, *e).ok())
            .collect();
        assert_eq!(exits, vec![P::Connecting, P::Stopped]);
    }

    #[test]
    fn a_cancelled_subscribe_restores_detached() {
        // Cancel-safe by restoration: nothing was created, so the session goes
        // back where it started.
        assert_eq!(
            transition(P::Connecting, E::SubscribeFailed),
            Ok(P::Detached)
        );
        assert_eq!(
            transition(P::Connecting, E::SubscribeAbandoned),
            Ok(P::Detached)
        );
    }

    #[test]
    fn a_cancelled_resume_is_not_restorable() {
        // Not cancel-safe by restoration: the pump and its WS session were
        // dropped with the future, so the honest landing is `Abandoned`.
        assert_eq!(
            transition(P::Resuming, E::ResumeAbandoned),
            Ok(P::Abandoned)
        );
        assert_eq!(
            transition(P::Abandoned, E::ResumeSpawned),
            Err(PumpDecline::EventOutOfOrder {
                current: P::Abandoned,
                event: E::ResumeSpawned
            })
        );
    }

    #[test]
    fn stop_decides_between_immediate_and_joined_teardown() {
        // Nothing to tear down: land stopped at once.
        for pose in [P::Detached, P::Ended, P::Abandoned, P::Stopped] {
            assert_eq!(
                transition(pose, E::Stop),
                Ok(P::Stopped),
                "stop from {pose}"
            );
        }
        // Something to tear down: stop first, then complete.
        for pose in [P::Connecting, P::Subscribed, P::Resuming, P::Running] {
            assert_eq!(
                transition(pose, E::Stop),
                Ok(P::Stopping),
                "stop from {pose}"
            );
        }
        assert_eq!(transition(P::Stopping, E::Stop), Ok(P::Stopping));
        assert_eq!(transition(P::Stopping, E::StopCompleted), Ok(P::Stopped));
    }

    #[test]
    fn stop_completed_is_only_meaningful_while_stopping() {
        for pose in ALL_POSES {
            let verdict = transition(pose, E::StopCompleted);
            if pose == P::Stopping {
                assert_eq!(verdict, Ok(P::Stopped));
            } else {
                assert!(verdict.is_err(), "stop_completed from {pose} was allowed");
            }
        }
    }

    #[test]
    fn the_armed_projections_name_the_teardown_facts() {
        for pose in ALL_POSES {
            assert_eq!(
                pose.live_loop_armed(),
                matches!(pose, P::Resuming | P::Running),
                "live_loop_armed({pose})"
            );
            assert_eq!(
                pose.holds_subscription(),
                matches!(
                    pose,
                    P::Connecting | P::Subscribed | P::Resuming | P::Running
                ),
                "holds_subscription({pose})"
            );
            assert_eq!(pose.is_terminal(), matches!(pose, P::Stopping | P::Stopped));
        }
    }

    #[test]
    fn pose_and_event_names_are_stable() {
        // The FFI translates names rather than authoring legality, so the
        // strings are the boundary contract.
        assert_eq!(P::Detached.as_str(), "detached");
        assert_eq!(P::Abandoned.as_str(), "abandoned");
        assert_eq!(E::ResumeRequested.as_str(), "resume");
        assert_eq!(E::StopCompleted.as_str(), "stop_completed");
    }
}
