//! The settlement arm's hosted driver: the host's handle on the engine pump
//! session's lifetime.
//!
//! Settlement owns no loop for the host to start. Its block clock IS the
//! engine's block pump, and the session consumer arms that pump; a second loop
//! beside it would be a second block clock on the one hub head source. What the
//! host can own — and now does — is the arm's *lifetime*: [`settlement_spawn`]
//! closes over the session's one [`EngineDriver`] and returns the factory the
//! host boots at
//! [`StrategyHost::start_driving`](crate::strategy_host::StrategyHost::start_driving),
//! so the settlement record moves `Enabled -> Running` at the one start
//! boundary and folds its terminal exit into the same `Stopped` / `Halted`
//! vocabulary every other driver uses.
//!
//! The factory adds no pump wrapper: the future it returns awaits the session
//! the engine already runs, and reads the session's own end facts.

use std::sync::Arc;

use degenbot_substrate::driver_spawn::{DriverExit, DriverSpawnFactory};
use tracing::Instrument;

use crate::arb_engine::driver::EngineDriver;
use crate::arb_engine::pump_session::PumpPhase;
use crate::arb_engine::session_end::{SessionEnd, SessionEndCause};

/// The settlement arm's spawn factory: the pump session's lifetime in the
/// host's driver vocabulary.
///
/// The host hands the factory the arm's lane namespace at the driving edge, and
/// that namespace is the identity the boot filed the arm's submission lane
/// under (`LaneNamespace::under(state_root, settlement)`): one declaration names
/// both the artifact scope and the lane's owner, so this factory takes no
/// artifact root of its own. The pump keeps the process-global run-artifact root
/// it has always written — the lane namespace scopes the nonce lane, not the
/// pump's journal.
#[must_use]
pub fn settlement_spawn(driver: Arc<EngineDriver>) -> DriverSpawnFactory {
    Box::new(move |lane| {
        let label = lane
            .as_ref()
            .map(|namespace| namespace.root().display().to_string());
        let span = tracing::info_span!("degenbot.settlement.drive", lane = ?label);
        Box::pin(drive(driver).instrument(span))
    })
}

/// The settlement driver loop: gate on an armed block clock, then await the
/// session's end.
async fn drive(driver: Arc<EngineDriver>) -> DriverExit {
    let phase = driver.current_phase();
    if phase != PumpPhase::Running {
        // The host boots this driver at its start boundary, which a session
        // that intends to settle crosses only after its consumer resumed the
        // pump. A record claiming `Running` over an unarmed clock is exactly
        // the silently-never-started hole this seam closes, so an unarmed
        // clock is a tombstone.
        return DriverExit::Halted(format!(
            "the block clock is not armed (session phase `{}`): settlement settles sealed blocks from the engine pump session",
            phase.as_str()
        ));
    }
    // The session's ONE end fact, read once the pump-completion edge resolves.
    fold_end(driver.wait_session_end().await)
}

/// The arm's terminal exit, from the session's ONE end fact.
///
/// The owner's intended stop is the only clean exit: its teardown entry writes
/// that fact before it joins the pump, so a stall or a losing loop landing that
/// unfolds while the session unwinds cannot re-author it. Every other fact is the
/// tombstone a settlement session must not survive, and it carries its own
/// spelling — a pump that finished on its own, stalled, or was aborted is never
/// reported as a clean stop.
#[must_use]
fn fold_end(end: SessionEnd) -> DriverExit {
    match end {
        SessionEnd::Ended(SessionEndCause::IntendedStop) => DriverExit::Stopped,
        // `Running` is unreachable behind the completion edge; a tombstone is the
        // safe reading if it ever is.
        other => DriverExit::Halted(other.name().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The intended stop is the ONE clean exit, whatever detection fact the pump
    /// published while unwinding: the fact is already written when the teardown
    /// entry took it, so a later stall cannot re-author it.
    #[test]
    fn an_intended_stop_folds_to_a_clean_exit() {
        assert_eq!(
            fold_end(SessionEnd::Ended(SessionEndCause::IntendedStop)),
            DriverExit::Stopped
        );
    }

    /// A block clock that ended with no intended stop behind it is a tombstone
    /// carrying the session's own end cause — never a clean stop.
    #[test]
    fn a_dead_block_clock_folds_to_a_tombstone_carrying_its_cause() {
        for cause in [
            SessionEndCause::PumpFinished,
            SessionEndCause::StallWatchdogTripped,
        ] {
            assert_eq!(
                fold_end(SessionEnd::Ended(cause)),
                DriverExit::Halted(cause.name().to_string())
            );
        }
    }

    /// A loop that never landed — a panic or an abort — is never the clean pump
    /// finish, and never the clean stop.
    #[test]
    fn a_lost_loop_folds_to_a_tombstone_never_a_clean_finish() {
        assert_eq!(
            fold_end(SessionEnd::pump_lost()),
            DriverExit::Halted(String::from("Aborted"))
        );
        assert_eq!(
            fold_end(SessionEnd::Ended(SessionEndCause::Aborted)),
            DriverExit::Halted(String::from("Aborted"))
        );
    }

    /// The total remainder: a fact that is still `Running` behind the completion
    /// edge is a tombstone too, and it names itself.
    #[test]
    fn the_running_fact_still_totals_to_a_tombstone() {
        assert_eq!(
            fold_end(SessionEnd::Running),
            DriverExit::Halted(String::from("Running"))
        );
    }
}
