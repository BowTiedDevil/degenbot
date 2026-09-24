//! The header/logs watchdog windows (JIABO3 + SONJQA + the logs-silence
//! inverse watchdog).
//!
//! ADR-008: the FSM (`StageMachine`) OWNS the watchdog *decisions*
//! (`on_tick` emits `Recover` / `LogSilence`); this crate owns the watchdog
//! *windows* — the tunable timeouts + the per-episode alarm accounting — so
//! the transport's liveness policy lives with the transport while the
//! runtime's driver executes the FSM's decisions.

use std::time::Duration;

/// If no block header arrives within this window, poll `eth_blockNumber`
/// and backfill the gap — independent of log activity.
///
/// The `newHeads` WS subscription can die silently while `logs` keeps
/// flowing. A merged stream masks a dead block stream as long as logs arrive
/// every `< BACKFILL_TIMEOUT_SECS` (the combined-stream-silence backfill path
/// never fires). Because only headers advance the cursor, every result batch
/// would then be stamped with a frozen block while prices keep updating from
/// the live log applies — looking like the bot is running but making no
/// block progress. The header-staleness watchdog independently detects
/// header staleness (a capped `wait_timeout` wakes the loop by the deadline
/// even under dense log pressure) and runs the same catch-up the
/// no-activity path uses.
pub const HEADER_STALENESS_SECS: u64 = 30;

/// SONJQA: the waterfall `log_wait` child is force-closed (with an explicit
/// stall warning) once it ages past this horizon — the observed failure shape
/// (trace a1ad51bd, block 25913381) was a 12.7s all-quiet header gap leaving
/// `log_wait` open until the NEXT header while its parent `pump.block` span
/// exported at arm-exit (319us), corrupting the waterfall. Degenerate
/// relative to the 30s staleness watchdog, a fresh 5s bound keeps children
/// within a healthy block cadence.
pub const LOG_WAIT_MAX_AGE_SECS: u64 = 5;

/// Window for the logs-subscription liveness watchdog: if headers keep
/// flowing (`newHeads` fresh) but NO log arrived from the
/// `eth_subscribe "logs"` arm within this window, the logs subscription is
/// presumed dead/stalled and a warning is emitted. This is the INVERSE of
/// `header_staleness` (a dead `newHeads`): it catches a dead/stalled LOGS sub
/// while the blocks sub is alive. Runs for the whole pump lifetime, not only
/// at startup.
pub const LOG_SILENCE_SECS: u64 = 60;

/// The watchdog windows + per-episode alarm accounting the runtime's driver
/// consults. Default-constructed to the production constants; per-instance
/// overrides exist for tests (the pump's `set_*_for_test` setters delegate
/// to these fields, so tests stay immune to the global environment).
#[derive(Debug)]
pub struct Watchdog {
    /// Header-staleness window (dead-`newHeads` recovery).
    pub header_staleness: Duration,
    /// Max age of the waterfall `log_wait` child before force-close (SONJQA).
    pub log_wait_max_age: Duration,
    /// Logs-silence window (dead/stalled `logs` arm while headers flow).
    pub log_silence: Duration,
    /// Count of logs-silence alarms fired since the stream started.
    /// Incremented once per silence episode (re-armed when the next log
    /// resumes the sub) so the liveness watchdog is observable without
    /// depending on log-capture infrastructure.
    silence_alarms: u64,
}

impl Watchdog {
    /// Production windows ([`HEADER_STALENESS_SECS`], [`LOG_WAIT_MAX_AGE_SECS`],
    /// [`LOG_SILENCE_SECS`]).
    #[must_use]
    pub fn new() -> Self {
        Self {
            header_staleness: Duration::from_secs(HEADER_STALENESS_SECS),
            log_wait_max_age: Duration::from_secs(LOG_WAIT_MAX_AGE_SECS),
            log_silence: Duration::from_secs(LOG_SILENCE_SECS),
            silence_alarms: 0,
        }
    }

    /// Record one logs-silence alarm (once per episode; the driver re-arms
    /// when the next log resumes the sub). Returns the new total.
    pub fn record_silence_alarm(&mut self) -> u64 {
        self.silence_alarms = self.silence_alarms.saturating_add(1);
        self.silence_alarms
    }

    /// Count of logs-silence alarms fired so far (observability + tests).
    #[must_use]
    pub fn silence_alarm_count(&self) -> u64 {
        self.silence_alarms
    }
}

impl Default for Watchdog {
    fn default() -> Self {
        Self::new()
    }
}
