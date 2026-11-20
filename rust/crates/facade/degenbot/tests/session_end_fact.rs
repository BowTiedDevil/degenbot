//! Compile-time contract for the umbrella's session-end fact surface.
//!
//! `cargo add degenbot` must be able to NAME the session's one end fact and its
//! write-once slot from the umbrella alone: `EngineDriver::session_end()` returns
//! the fact, so a consumer that cannot reach the type cannot read the answer. This
//! pins the re-export paths; the fold's behavior is covered at its
//! `degenbot-bot` seam.

use degenbot::session_end::{
    rank, RaiseSlot, SessionEnd, SessionEndCause, SessionEndObservation, SessionEndSlot,
    SessionEndVerdict, SessionEndWrite, ALL_SESSION_END_CAUSES,
};

#[test]
fn the_umbrella_names_the_session_end_fact_and_its_slot() {
    // The fact is total: `Running` is a value with no cause.
    assert_eq!(SessionEnd::default(), SessionEnd::Running);
    assert_eq!(SessionEnd::Running.cause(), None);
    assert_eq!(ALL_SESSION_END_CAUSES.len(), 4);

    // The slot's duplicate policy is typed, and the first terminal observation
    // stands.
    let slot = SessionEndSlot::new();
    let stopped = SessionEnd::Ended(SessionEndCause::IntendedStop);
    assert_eq!(slot.record(stopped), SessionEndWrite::Recorded);
    assert_eq!(
        slot.record(SessionEnd::Ended(SessionEndCause::Aborted)),
        SessionEndWrite::Retained(stopped)
    );
    assert_eq!(slot.current(), stopped);
}

#[test]
fn the_umbrella_ranks_the_owner_stop_as_the_intended_end() {
    let ranking = rank::<()>(SessionEndObservation {
        cause: Some(SessionEndCause::IntendedStop),
        consumer: None,
        registration: None,
        output: None,
    });
    assert_eq!(ranking.verdict, SessionEndVerdict::PumpEnded);
    assert_eq!(ranking.slot, RaiseSlot::Nothing);
}

#[test]
fn the_umbrella_names_a_lost_loop_as_the_unexpected_end() {
    // A panic or an abort is never the clean pump finish, through the umbrella
    // exactly as through the engine seam.
    assert_eq!(
        SessionEnd::pump_lost(),
        SessionEnd::Ended(SessionEndCause::Aborted)
    );
    let ranking = rank::<()>(SessionEndObservation {
        cause: Some(SessionEndCause::Aborted),
        consumer: None,
        registration: None,
        output: None,
    });
    assert_eq!(ranking.verdict, SessionEndVerdict::WatchdogTripped);
}
