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
use crate::arb_engine::session_end::SessionEndCause;

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
    driver.wait_session_end().await;
    fold_end(driver.is_stopped(), driver.session_end_cause())
}

/// The arm's terminal exit, from the session facts at the moment its block
/// clock ended.
///
/// A pump that ended after teardown began is the session's intended stop: the
/// consumer's close takes the session's teardown entry before it joins the
/// pump, and nothing else disarms a live block clock. A pump that ended any
/// other way — a dropped subscription, a panic, or the pump's own stall clock —
/// is the dead block clock a settlement session must not survive, and the
/// session's end cause becomes the tombstone's detail.
#[must_use]
fn fold_end(teardown_begun: bool, cause: SessionEndCause) -> DriverExit {
    if teardown_begun {
        DriverExit::Stopped
    } else {
        DriverExit::Halted(cause.name().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The intended stop: teardown began, so the arm stops cleanly whatever
    /// end fact the pump published while unwinding.
    #[test]
    fn an_intended_stop_folds_to_a_clean_exit() {
        for cause in [
            SessionEndCause::PumpFinished,
            SessionEndCause::StallWatchdogTripped,
        ] {
            assert_eq!(fold_end(true, cause), DriverExit::Stopped);
        }
    }

    /// A block clock that ended with no teardown behind it is a tombstone
    /// carrying the session's own end cause — never a clean stop.
    #[test]
    fn a_dead_block_clock_folds_to_a_tombstone_carrying_its_cause() {
        assert_eq!(
            fold_end(false, SessionEndCause::PumpFinished),
            DriverExit::Halted(String::from("PumpFinished"))
        );
        assert_eq!(
            fold_end(false, SessionEndCause::StallWatchdogTripped),
            DriverExit::Halted(String::from("StallWatchdogTripped"))
        );
    }
}
