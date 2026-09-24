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
//! The listener is ONE dedicated std thread running a current-thread runtime.
//! A one-shot console command must never boot the shared bot runtimes
//! (`degenbot_core::runtime`) for a single parked signal task, so this surface
//! is fully self-owned (asserted by `tests/sigint_runtime_isolation.rs`). The
//! command itself stays synchronous: the listener parks on the signal stream
//! and never blocks the calling thread.

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
    /// Aborting the task parks the single listener thread at its next
    /// `.await`, which returns from `block_on`, drops the runtime, and ends
    /// the thread.
    listener: Option<tokio::task::AbortHandle>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        // The abort is synchronous + non-blocking (safe from any thread): the
        // task cancels at its next `.await` park.
        if let Some(listener) = self.listener.take() {
            listener.abort();
        }
    }
}

/// The census entry for the dedicated listener thread. The registry is
/// documentation-enforced for every spawn site (see `worker_census` module
/// docs): an unregistered thread is invisible to the gauge and the boot dump.
#[cfg(unix)]
const fn census_entry() -> degenbot_core::worker_census::WorkerCensusEntry {
    degenbot_core::worker_census::WorkerCensusEntry {
        resource: "cli_sigint_listener",
        kind: "std signal-listener thread (one current-thread runtime; SIGINT -> cooperative CancelHandle)",
        count: 1,
        thread_name: "degenbot-cli-sigint",
        sizing: "exactly one per run (fixed; owned by the run guard)",
        binding: "pinned",
    }
}

/// Install the SIGINT policy for `cancel`.
///
/// Spawns the listener on its OWN current-thread runtime — never the
/// process-wide shared runtime (`degenbot_core::runtime::get_runtime`), which
/// a console invocation must not pay for. A runtime- or thread-spawn failure
/// is handled at this site (a warning is logged): cooperative cancel is then
/// unavailable, but the run still proceeds.
#[cfg(unix)]
pub fn install(cancel: CancelHandle) -> Guard {
    degenbot_core::worker_census::register(census_entry());

    // Built on the calling thread (a Runtime is Send), then handed to the
    // one listener thread. A current_thread runtime suffices: the listener is
    // a single task parking on the signal stream.
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        degenbot_core::op_warn!(
            domain = pump,
            "could not build the SIGINT listener runtime; cooperative cancel is unavailable"
        );
        return Guard { listener: None };
    };

    // The abort handle crosses back to the guard; the listen task stays with
    // the runtime that spawned it.
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("degenbot-cli-sigint".to_owned())
        .spawn(move || {
            let task = runtime.spawn(listen(cancel));
            let _ = tx.send(task.abort_handle());
            let _ = runtime.block_on(task);
        });

    if let Err(error) = spawned {
        degenbot_core::op_warn!(
            domain = pump,
            error = %error,
            "could not spawn the SIGINT listener thread; cooperative cancel is unavailable"
        );
        return Guard { listener: None };
    }
    let listener = rx
        .recv()
        .map_err(|_| {
            degenbot_core::op_warn!(
                domain = pump,
                "SIGINT listener thread died before installing; cooperative cancel is unavailable"
            );
        })
        .ok();
    Guard { listener }
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
