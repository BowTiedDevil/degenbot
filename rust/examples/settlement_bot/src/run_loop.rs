//! The live arm's run-until-shutdown phase — parity-ledger rows 16/19
//! wiring (RSP-10, ergo `SGCAJ5`).
//!
//! Mirrors `src/degenbot/runner/bot_runner.py::BotRunner.run`'s final step:
//! once registration (the Python `build_paths` step) has attached, the session
//! stays open until the runner is told to stop. The Python loop awaits the
//! session watch indefinitely (Ctrl-C ends it); a supervisor/CI observation run
//! needs a *bounded* window instead, so this phase ends on either:
//!
//! - a shutdown signal (SIGINT — the `BotRunner.__aexit__` Ctrl-C path), or
//! - the env-gated `DEGENBOT_SMOKE_MAX_SECS` window (observation/CI runs).
//!
//! While the session is open the loop emits a bounded per-block heartbeat
//! (`[session] heartbeat ...`) so a long observation run is visibly alive.
//!
//! Teardown ordering is ADR-050 D6 and lives in [`teardown_session`]:
//! stop the pump FIRST, then close the operator channel, then join the
//! consumer/watch. The order is centralized here so it cannot drift.

use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;

use crate::consume::SessionProgress;
use crate::session_watch::Heartbeat;

/// One run-loop heartbeat observation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunLoopHeartbeat {
    /// Heartbeat ticks emitted so far (bounded by the window / interval).
    pub ticks: u64,
    /// Result batches consumed so far (the consumer's block clock advances
    /// once per batch — batches and blocks track 1:1 on the result stream).
    pub blocks_seen: u64,
    /// The consumer's current block.
    pub current_block: u64,
}

/// How the run-until-shutdown phase ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunLoopEnd {
    /// The bounded observation window elapsed.
    WindowExpired,
    /// A shutdown signal arrived.
    Interrupted,
}

/// Run-loop configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunLoopConfig {
    /// The bounded observation window; `None` runs until a shutdown signal.
    pub max_secs: Option<u64>,
    /// The heartbeat emission interval.
    pub heartbeat_interval: Duration,
}

impl RunLoopConfig {
    /// A config with a 1-second heartbeat interval.
    #[must_use]
    pub fn new(max_secs: Option<u64>) -> Self {
        Self {
            max_secs,
            heartbeat_interval: Duration::from_secs(1),
        }
    }
}

/// Keep the session open until `shutdown` resolves or the bounded window
/// elapses, calling `on_heartbeat` once per interval tick.
///
/// This is the Rust analogue of `await self._session_watch.wait()` in the
/// Python main loop: it does not own the consumer/watch tasks (their verdict
/// is read after teardown) — it only holds the process/session open and
/// reports liveness. It never touches RPC directly.
///
/// Returning does NOT tear the session down; the caller runs
/// [`teardown_session`] next so the ADR-050 D6 ordering is preserved.
#[must_use]
pub async fn run_session_loop<F, G>(
    config: &RunLoopConfig,
    heartbeat: &Heartbeat,
    progress: &SessionProgress,
    shutdown: F,
    mut on_heartbeat: G,
) -> RunLoopEnd
where
    F: Future<Output = ()>,
    G: FnMut(&RunLoopHeartbeat),
{
    tokio::pin!(shutdown);
    let deadline = config
        .max_secs
        .map(|secs| Instant::now() + Duration::from_secs(secs));
    let mut interval = tokio::time::interval(config.heartbeat_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut ticks = 0_u64;
    loop {
        let tick = interval.tick();
        tokio::pin!(tick);
        let end = if let Some(deadline) = deadline {
            tokio::select! {
                () = &mut shutdown => Some(RunLoopEnd::Interrupted),
                () = async { tokio::time::sleep_until(deadline).await; } => {
                    Some(RunLoopEnd::WindowExpired)
                }
                _ = &mut tick => None,
            }
        } else {
            tokio::select! {
                () = &mut shutdown => Some(RunLoopEnd::Interrupted),
                _ = &mut tick => None,
            }
        };
        let Some(end) = end else {
            ticks += 1;
            on_heartbeat(&RunLoopHeartbeat {
                ticks,
                blocks_seen: heartbeat.seq(),
                current_block: progress.current_block(),
            });
            continue;
        };
        return end;
    }
}

/// The ADR-050 D6 teardown, in the one order that is correct:
/// stop the pump → close the operator channel → join the consumer/watch.
///
/// Centralized so the ordering is a single testable function rather than
/// three loose calls at the tail of `main`. Each step is a closure so the
/// live arm can supply the runtime-blocking `block_on` wrappers while the
/// offline tests supply recorders.
///
/// # Errors
///
/// Propagates the first failing step; later steps do not run (a failed pump
/// stop must be loud, not swallowed by an operator/join error).
pub fn teardown_session<T>(
    stop_pump: impl FnOnce() -> Result<(), String>,
    close_operator: impl FnOnce() -> Result<(), String>,
    join_watch: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    stop_pump()?;
    close_operator()?;
    join_watch()
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests assert on known-valid inputs")]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn bounded_window_ends_the_loop_and_bounds_heartbeat_lines() {
        let config = RunLoopConfig {
            max_secs: Some(3),
            heartbeat_interval: Duration::from_millis(400),
        };
        let heartbeat = Heartbeat::new();
        let progress = SessionProgress::new();
        let lines = RefCell::new(0_u64);
        let end = run_session_loop(
            &config,
            &heartbeat,
            &progress,
            std::future::pending(),
            |_| *lines.borrow_mut() += 1,
        )
        .await;
        assert_eq!(end, RunLoopEnd::WindowExpired);
        // 3s at 400ms + the immediate first tick → a bounded, finite run.
        let emitted = *lines.borrow();
        assert!(
            (7..=9).contains(&emitted),
            "heartbeat lines must be bounded by the window, got {emitted}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_signal_ends_the_window_early() {
        let config = RunLoopConfig {
            max_secs: Some(60),
            heartbeat_interval: Duration::from_secs(1),
        };
        let heartbeat = Heartbeat::new();
        let progress = SessionProgress::new();
        let end = run_session_loop(
            &config,
            &heartbeat,
            &progress,
            async {
                tokio::time::sleep(Duration::from_millis(1_500)).await;
            },
            |_| {},
        )
        .await;
        assert_eq!(end, RunLoopEnd::Interrupted);
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_reports_shared_consumer_progress() {
        let config = RunLoopConfig {
            max_secs: Some(1),
            heartbeat_interval: Duration::from_millis(300),
        };
        let heartbeat = Heartbeat::new();
        heartbeat.beat();
        heartbeat.beat();
        let progress = SessionProgress::new();
        let clock = crate::consume::BlockClock {
            current_block: 21_000_042,
            ..crate::consume::BlockClock::default()
        };
        progress.note(&clock);
        let seen = RefCell::new(Vec::new());
        let end = run_session_loop(
            &config,
            &heartbeat,
            &progress,
            std::future::pending(),
            |hb| seen.borrow_mut().push(*hb),
        )
        .await;
        assert_eq!(end, RunLoopEnd::WindowExpired);
        let first = *seen.borrow().first().unwrap();
        assert_eq!(first.blocks_seen, 2);
        assert_eq!(first.current_block, 21_000_042);
    }

    #[test]
    fn teardown_runs_stop_then_operator_then_join() {
        let order = RefCell::new(Vec::new());
        let output = teardown_session(
            || {
                order.borrow_mut().push("stop");
                Ok(())
            },
            || {
                order.borrow_mut().push("operator");
                Ok(())
            },
            || {
                order.borrow_mut().push("join");
                Ok(42_u64)
            },
        )
        .unwrap();
        assert_eq!(output, 42);
        assert_eq!(*order.borrow(), vec!["stop", "operator", "join"]);
    }

    #[test]
    fn teardown_stops_at_the_first_failure() {
        let order = RefCell::new(Vec::new());
        let error = teardown_session(
            || {
                order.borrow_mut().push("stop");
                Err::<(), String>("pump stop failed".to_string())
            },
            || {
                order.borrow_mut().push("operator");
                Ok(())
            },
            || {
                order.borrow_mut().push("join");
                Ok(0_u64)
            },
        )
        .unwrap_err();
        assert_eq!(error, "pump stop failed");
        assert_eq!(*order.borrow(), vec!["stop"]);
    }
}
