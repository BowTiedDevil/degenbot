//! SIGINT ownership (ADR-051 D7).
//!
//! The console owns the interrupt policy on BOTH entry paths. cli-core only
//! carries the cooperative [`CancelHandle`] the updater loops poll at chunk
//! boundaries (never mid-chunk, so chunk atomicity is preserved); this module
//! turns Ctrl+C into that handle:
//!
//! - **first Ctrl+C** -> [`Action::Cancel`]: set the handle. The in-flight
//!   chunk completes (commit OR rollback), the run returns a friendly
//!   `cancelled` report, and committed chunks stay durable.
//! - **second Ctrl+C** -> [`Action::Abort`]: abort the process.
//!
//! The listener is one spawned task on the process-wide shared runtime
//! (`degenbot_core::runtime::get_runtime()`) — no ad-hoc runtime. The command
//! itself stays synchronous: the listener parks on the shared runtime's IO
//! driver and never blocks the calling thread, and cli-core's arms `block_on`
//! that same runtime from a clean (non-nested) context.

use degenbot_cli_core::CancelHandle;

/// What a received interrupt means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Action {
    /// Feed the cooperative cancel flag.
    Cancel,
    /// Restore the default disposition and abort.
    Abort,
}

/// The policy: the first interrupt cancels, the second aborts.
pub const fn action(already_cancelled: bool) -> Action {
    if already_cancelled {
        Action::Abort
    } else {
        Action::Cancel
    }
}

/// Keeps the SIGINT listener alive for the lifetime of the run; dropping the
/// guard aborts the listener task, which drops its signal stream and restores
/// the default SIGINT disposition (the policy is run-scoped).
#[derive(Debug)]
#[must_use = "the guard must outlive the command run"]
pub struct Guard {
    listener: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Aborting the task drops its signal `Stream` -> the SIGINT handler
        // de-registers. The abort is synchronous + non-blocking (safe from any
        // thread, inside a runtime context included): the task cancels at its
        // next `.await` — it parks on `interrupts.recv().await`.
        if let Some(listener) = self.listener.take() {
            listener.abort();
        }
    }
}

/// Install the SIGINT policy for `cancel`.
///
/// Spawns the listener on the process-wide shared runtime
/// (`degenbot_core::runtime::get_runtime()`); no per-listener runtime is
/// built. A signal-stream build failure is handled inside [`listen`] (a
/// warning is logged): cooperative cancel is then unavailable, but the run
/// still proceeds.
#[cfg(unix)]
pub fn install(cancel: CancelHandle) -> Guard {
    Guard {
        listener: Some(degenbot_core::runtime::get_runtime().spawn(listen(cancel))),
    }
}

/// Non-Unix builds have no SIGINT policy to install.
#[cfg(not(unix))]
pub fn install(_cancel: CancelHandle) -> Guard {
    degenbot_core::op_warn!(
        domain = pump,
        "SIGINT handling is Unix-only; cooperative cancel is unavailable"
    );
    Guard { listener: None }
}

#[cfg(unix)]
async fn listen(cancel: CancelHandle) {
    let Ok(mut interrupts) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
    else {
        degenbot_core::op_warn!(
            domain = pump,
            "could not install the SIGINT handler; cooperative cancel is unavailable"
        );
        return;
    };
    loop {
        if interrupts.recv().await.is_none() {
            return;
        }
        match action(cancel.is_cancelled()) {
            Action::Cancel => {
                cancel.cancel();
                degenbot_core::op_warn!(
                    domain = pump,
                    "interrupt received: finishing the in-flight chunk, then stopping (press Ctrl+C again to abort)"
                );
            }
            Action::Abort => {
                // Abort is the outcome D7 names; there is no subsequent
                // disposition to restore because the process dies here. The
                // default backtrace-less abort skips destructors, which is the
                // point: the run is already unwinding under a cooperative stop.
                degenbot_core::op_warn!(domain = pump, "second interrupt received: aborting");
                std::process::abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{action, Action};

    #[test]
    fn first_interrupt_cancels_and_second_aborts() {
        assert_eq!(action(false), Action::Cancel);
        assert_eq!(action(true), Action::Abort);
    }
}
