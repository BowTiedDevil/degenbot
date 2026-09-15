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
//! The listener runs on its own one-worker `tokio` runtime so the command
//! itself stays synchronous (cli-core's arms own their runtimes and must not be
//! nested inside one).

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

/// Keeps the SIGINT listener alive for the lifetime of the run; dropping it
/// stops the runtime and the listener task.
#[derive(Debug)]
#[must_use = "the guard must outlive the command run"]
pub struct Guard {
    _runtime: tokio::runtime::Runtime,
}

/// Install the SIGINT policy for `cancel`.
///
/// Returns `None` (and logs a warning) when the listener runtime or the signal
/// stream cannot be built: cooperative cancel is then unavailable, but the run
/// still proceeds.
#[cfg(unix)]
#[must_use]
pub fn install(cancel: CancelHandle) -> Option<Guard> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .ok()?;
    runtime.spawn(listen(cancel));
    Some(Guard { _runtime: runtime })
}

/// Non-Unix builds have no SIGINT policy to install.
#[cfg(not(unix))]
#[must_use]
pub fn install(_cancel: CancelHandle) -> Option<Guard> {
    degenbot_core::op_warn!(
        domain = pump,
        "SIGINT handling is Unix-only; cooperative cancel is unavailable"
    );
    None
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
