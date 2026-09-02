//! The dedicated solve executor (epic BXUSGL T1): a private, CPU-bound tokio
//! runtime running on named, lower-OS-priority threads.
//!
//! ## Why a dedicated runtime (not a shared-pool job)
//!
//! The I/O side of the bot (block clock, RPC, the Python result bridge) runs
//! on the process's main tokio runtime. The solve fan-out is pure CPU
//! (per-path solves, seconds at the heavy end). Running CPU jobs on the
//! I/O runtime — or letting an unprioritized CPU pool contend with it for
//! cores — is exactly the "same runtime for I/O and CPU" hazard the Tokio
//! docs warn about. The `InfluxDB IOx` `DedicatedExecutor` pattern (alamb's
//! thenewstack.io article + gist; the same technique behind tpchgen PR
//! `#34`'s bounded-parallelism choice over Rayon: "I couldn't find any way
//! [with Rayon] to limit the number of things that were buffered at once")
//! hosts CPU tasks on a SEPARATE multi-thread runtime whose worker threads
//! run at lower OS priority, so latency-critical I/O tasks always win the
//! cores.
//!
//! ## Bounded in-flight parallelism
//!
//! The caller spawns exactly `n_threads` jobs — one per LPT bin, each
//! pinning one persistent worker for the whole bin (no splitting, no
//! stealing: RAYPAR T3 semantics, warm L1/L2 and allocator arenas). Results
//! stream back over a channel as each PATH completes (per-path sends, not
//! per-bin), so the drain merges fast paths while slow bins still run.

use std::sync::mpsc;

/// A pre-composed per-bin solve closure. The sync CPU work inside the async
/// block is BY DESIGN: the "never block a runtime worker without .await"
/// rule applies to the shared I/O runtime, and this is deliberately not it
/// (see module docs). Workers never yield mid-bin.
type Job = Box<dyn FnOnce() + Send + 'static>;

/// Loud, unrecoverable executor failure (mirror of the ADR-021 tripwire's
/// abort discipline): a dead executor would deadlock the first solve — its
/// per-path sends would land in a pipe nobody drains — so swallowing the
/// error is never an option.
fn abort_executor(context: &str, err: &str) -> ! {
    tracing::error!(context = %context, error = %err, "[solve-executor] unrecoverable - aborting");
    std::process::abort();
}

pub(crate) struct SolveExecutor {
    tx: mpsc::Sender<Job>,
}

impl SolveExecutor {
    /// Build the executor: one host thread installs a private multi-thread
    /// runtime and pulls jobs off the request channel onto it.
    pub(crate) fn new(thread_name: &'static str, worker_threads: usize) -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        let thread_name = thread_name.to_string();
        let spawned = std::thread::Builder::new()
            .name(format!("{thread_name}-host"))
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .thread_name(thread_name)
                    .worker_threads(worker_threads.max(1))
                    .on_thread_start(lower_priority)
                    .build();
                let runtime = match runtime {
                    Ok(rt) => rt,
                    Err(err) => abort_executor("runtime build", &err.to_string()),
                };
                runtime.block_on(async move {
                    while let Ok(job) = rx.recv() {
                        tokio::spawn(async move { job() });
                    }
                    // Channel closed (host sends dropped): shut down. Jobs in
                    // flight finish; spawned tasks are awaited by the runtime
                    // on its normal shutdown path.
                });
            });
        if let Err(err) = spawned {
            abort_executor("host thread spawn", &format!("{err:?}"));
        }
        Self { tx }
    }

    /// Submit one job (one LPT bin). Never blocks: the channel is unbounded
    /// and bounded-ness comes from the caller spawning exactly n bins.
    pub(crate) fn spawn(&self, job: impl FnOnce() + Send + 'static) {
        // Send errors only when the host is gone (process exiting mid-solve):
        // nothing can drain results then anyway.
        let _ = self.tx.send(Box::new(job));
    }
}

/// Lower this thread's OS priority so the latency-critical I/O runtime's
/// tasks (block clock, RPC, the Python bridge) always win the cores. `IOx`'s
/// `DedicatedExecutor` does exactly this. Failure is non-fatal (`nice()` may
/// be denied under restricted rlimits) — the executor just runs at default
/// priority, as today.
fn lower_priority() {
    #[cfg(unix)]
    {
        // SAFETY: a plain setpriority(2) call on the calling thread (0 = our
        // pid), touching no aliased memory. A denied value is non-fatal.
        let result = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 10) };
        if result != 0 {
            tracing::debug!("[solve-executor] setpriority denied - running at default priority");
        }
    }
}

static SOLVE_EXECUTOR: std::sync::OnceLock<SolveExecutor> = std::sync::OnceLock::new();

/// The process-wide solve executor, built lazily on the first tokio-stance
/// solve and persisting for the process lifetime (mirroring the rayon global
/// pool's construction-once contract: persistent workers keep warm L1/L2 +
/// allocator arenas across drains).
pub(crate) fn global_solve_executor() -> &'static SolveExecutor {
    SOLVE_EXECUTOR.get_or_init(|| {
        // Match the rayon pool width the engine sizes its LPT bins against.
        // Both default to the same value (available parallelism); the bins
        // are computed from rayon::current_num_threads() at dispatch time, so
        // one runtime can never host fewer workers than there are bins.
        SolveExecutor::new("degenbot-solve-tokio", rayon::current_num_threads())
    })
}
