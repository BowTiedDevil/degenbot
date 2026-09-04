//! GIL-acquire-latency probe + main-loop stuck-watchdog.
//!
//! Purpose: measure and observe the rolling-start GIL-contention deadlock
//! (ergo 66H3KJ). The probe runs on a dedicated **`std::thread`** (NOT a `tokio`
//! worker, NOT needing the GIL to make progress), so it can keep sampling
//! even when the main `asyncio` thread is parked holding the GIL and every
//! `tokio` worker is blocked on `PyGILState_Ensure`.
//!
//! ## What the probe measures
//!
//! Every `interval_ms` (default 50 ms) the probe thread calls
//! `Python::attach(|_| ())` — the same `PyGILState_Ensure` acquire that
//! pyo3-log's per-record bridge and `future_into_py`'s result-setter
//! (`spawn_blocking(|| Python::attach(...))`) perform. The elapsed time of
//! each acquire is:
//!
//! - **~µs** in the steady state — the GIL is free between Python
//!   bytecode ticks (`CPython` releases it every `sys.getswitchinterval()`).
//! - **> `threshold_ms`** (default 100 ms) — a single OS thread is holding
//!   the GIL without yielding. The two known culprits:
//!   1. A synchronous `#[pyfunction]` / `#[pymethod]` on the `build_paths`
//!      path that does heavy work WITHOUT `py.allow_threads` — holds the
//!      GIL for the whole call body.
//!   2. The asyncio main thread parked in `_asyncio.so`'s `futex_wait`
//!      while awaiting a pyo3 `future_into_py` future whose completion
//!      needs the GIL — the permanent-deadlock signature observed at
//!      mainnet block 25647518.
//!
//! When `elapsed > threshold_ms`, the probe emits `[gil-probe] GIL held`
//! with the acquire duration — a live marker of WHO was holding the GIL
//! (correlate with the surrounding log lines).
//!
//! ## Stuck-watchdog
//!
//! A second thread tracks "last forward progress" via an atomic timestamp
//! updated from Python (the example's pump-dispatch loop calls
//! `mark_progress()` after each block). If no progress for `stuck_ms`
//! (default 30 s), it emits `[gil-probe] *** MAIN LOOP STUCK` repeatedly
//! and dumps the probe's last sample — confirming permanent deadlock
//! independently of the GIL (this thread never acquires the GIL).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pyo3::prelude::*;

/// Global "last forward progress" wall-clock ms (set from Python via
/// `mark_progress()`; read by the stuck-watchdog thread). `None`-ish (0)
/// until the first `mark_progress()` call.
static LAST_PROGRESS_MS: AtomicU64 = AtomicU64::new(0);

/// Wall-clock ms of the probe thread's LAST successful `Python::attach`
/// acquire (updated after every sample). A true permanent GIL deadlock
/// blocks the probe thread itself on `PyGILState_Ensure`, so this timestamp
/// stops advancing — distinguishing a real deadlock (probe blocked) from
/// the main loop merely being busy in `build_paths` (probe still sampling,
/// `LAST_PROGRESS_MS` alone goes stale because the consumer has no work
/// yet). Read by the watchdog; 0 until the first sample completes.
static LAST_PROBE_SAMPLE_MS: AtomicU64 = AtomicU64::new(0);

/// `true` once the probe threads are running (idempotent start guard).
static PROBE_RUNNING: AtomicBool = AtomicBool::new(false);

/// Wall-clock ms since UNIX epoch (best-effort; wraps only in 584 million
/// years). `0` is the sentinel "not yet set".
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Python-callable: stamp "forward progress happened" (call after each
/// block the pump delivers + dispatches). The stuck-watchdog reads this.
///
/// Cheap: one relaxed atomic store. Safe to call from any thread holding
/// the GIL (the example's main loop) — no allocation, no I/O.
/// The effective reaction for a failure bucket (ADR-040): the closed
/// failure_policy matrix resolved against boot-time `[failure_policy]`
/// overrides. Returns `observe` | `event` | `quarantine` | `exit`.
///
/// Single source of truth for the Python driver's reaction behavior (the
/// Rust core owns the matrix; Python must not duplicate the table).
#[pyfunction]
fn failure_action(kind: &str, reason: Option<&str>) -> &'static str {
    degenbot_bot::failure_policy::action(kind, reason).as_str()
}

#[pyfunction]
fn mark_progress() {
    LAST_PROGRESS_MS.store(now_ms(), Ordering::Relaxed);
}

/// Start the GIL-acquire-latency probe + main-loop stuck-watchdog.
///
/// Idempotent: a second call is a no-op (logs once that it's already
/// running). Spawns two detached `std::thread`s (daemon-equivalent — they
/// exit when the process exits).
///
/// # Arguments
/// * `interval_ms` — sampling period (default 50 ms).
/// * `threshold_ms` — log `[gil-probe] GIL held` when an acquire exceeds
///   this (default 100 ms).
/// * `stuck_ms` — log `[gil-probe] *** MAIN LOOP STUCK` when no
///   `mark_progress()` call for this long (default 30 000 ms).
///
/// # Errors
/// Returns `PyErr` only if thread spawning fails (extremely rare).
#[pyfunction]
#[pyo3(signature = (interval_ms=50, threshold_ms=100, stuck_ms=30_000))]
#[expect(clippy::too_many_lines)] // thread bodies are linear by design
fn start_gil_probe(interval_ms: u64, threshold_ms: u64, stuck_ms: u64) -> PyResult<()> {
    if PROBE_RUNNING.swap(true, Ordering::SeqCst) {
        tracing::warn!("[gil-probe] already running — start_gil_probe() call ignored (idempotent)");
        return Ok(());
    }
    LAST_PROGRESS_MS.store(now_ms(), Ordering::Relaxed);
    let interval = Duration::from_millis(interval_ms.max(1));
    let threshold = Duration::from_millis(threshold_ms.max(1));
    let stuck = Duration::from_millis(stuck_ms.max(1));

    // ── Probe thread: periodic GIL acquire + latency log. ──
    thread::Builder::new()
        .name("gil-probe".to_string())
        .spawn(move || {
            tracing::info!(
                interval = ?interval,
                threshold = ?threshold,
                "[gil-probe] sampling"
            );
            let mut last_sample_ms: u64 = now_ms();
            loop {
                let t0 = Instant::now();
                // `Python::attach` blocks on PyGILState_Ensure until the GIL
                // is available. The elapsed reflects how long some other
                // thread held the GIL without yielding.
                Python::attach(|_py| ());
                let elapsed = t0.elapsed();
                let now = now_ms();
                let gap = now.saturating_sub(last_sample_ms);
                last_sample_ms = now;
                // Publish the probe's own last-acquire timestamp so the
                // watchdog can distinguish a real GIL deadlock (this thread
                // blocks on ``PyGILState_Ensure`` -> this store never runs ->
                // ``LAST_PROBE_SAMPLE_MS`` goes stale) from the main loop
                // merely being busy (probe still sampling).
                LAST_PROBE_SAMPLE_MS.store(now, Ordering::Relaxed);
                if elapsed >= threshold {
                    tracing::warn!(
                        acquire_ms = %elapsed.as_millis(),
                        gap,
                        "[gil-probe] GIL held: acquire took ms — main thread holding GIL"
                    );
                } else {
                    tracing::debug!(
                        acquire_ms = %elapsed.as_millis(),
                        gap,
                        "[gil-probe] GIL acquire"
                    );
                }
                thread::sleep(interval);
            }
        })
        .map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!(
                "[gil-probe] failed to spawn probe thread: {e}"
            ))
        })?;

    // ── Watchdog thread: detect main-loop stuck (no `mark_progress()`). ──
    // Never acquires the GIL — runs even during a permanent GIL deadlock.
    //
    // Two signals, one verdict:
    //  - `LAST_PROGRESS_MS`        = Python consumer-loop heartbeat
    //    (`mark_progress()` per batch). Goes stale while the consumer has no
    //    work — a NORMAL rolling-start window (`build_paths` registers no
    //    paths yet, so no batches flow). Alone it is NOT a deadlock signal.
    //  - `LAST_PROBE_SAMPLE_MS`    = the probe thread's own last successful
    //    `Python::attach`. A permanent GIL deadlock blocks the probe thread too,
    //    so this stops advancing. THIS is the true deadlock signal.
    // The watchdog alarms `*** GIL DEADLOCK` only when BOTH are stale (the
    // probe itself stopped sampling) — a busy-but-alive main loop (probe still
    // sampling) emits a softer `main loop idle` note instead, so a normal
    // rolling start no longer false-alarms as a permanent deadlock.
    thread::Builder::new()
        .name("gil-probe-watchdog".to_string())
        .spawn(move || {
            tracing::info!(
                stuck = ?stuck,
                "[gil-probe] stuck-watchdog armed"
            );
            let stuck_ms = u64::try_from(stuck.as_millis()).unwrap_or(u64::MAX);
            let mut alarm_count: u32 = 0;
            let mut busy_dumped = false;
            loop {
                thread::sleep(Duration::from_secs(5));
                let progress = LAST_PROGRESS_MS.load(Ordering::Relaxed);
                let sample = LAST_PROBE_SAMPLE_MS.load(Ordering::Relaxed);
                let now = now_ms();
                match watchdog_verdict(progress, sample, now, stuck_ms) {
                    WatchdogVerdict::NotArmed => {}
                    WatchdogVerdict::Healthy => {
                        busy_dumped = false;
                    }
                    WatchdogVerdict::Busy {
                        since_progress,
                        since_sample,
                    } => {
                        tracing::info!(
                            since_progress,
                            since_sample,
                            "[gil-probe] main loop idle: no progress — busy, not a GIL deadlock \\
                             (if this persists for minutes while sampling stays fresh, suspect a \
                             non-GIL wedge: a Rust lock held across an await)"
                        );
                        if !busy_dumped && should_dump_busy(since_progress, BUSY_DUMP_AFTER_MS) {
                            busy_dumped = true;
                            if let Some(p) =
                                crate::diagnostics::thread_registry::dump_to_file()
                            {
                                tracing::error!(
                                    path = %p.display(),
                                    since_progress,
                                    "[gil-probe] long-Busy episode: thread-registry + futex table dumped (non-GIL wedge suspect)"
                                );
                            }
                        }
                    },
                    WatchdogVerdict::Deadlocked {
                        since_progress,
                        since_sample,
                    } => {
                        // TPMFLV: on the first confirmed alarm (and every 10
                        // alarms after) self-record the thread table. The
                        // watchdog never takes the GIL, so this runs even
                        // during a hard GIL deadlock; /proc reads + a file
                        // write are non-blocking.
                        alarm_count += 1;
                        if alarm_count == 1 || alarm_count.is_multiple_of(10) {
                            if let Some(p) = crate::diagnostics::thread_registry::dump_to_file() {
                                tracing::error!(
                                    path = %p.display(),
                                    "[gil-probe] thread-registry + futex table dumped"
                                );
                            }
                        }
                        tracing::error!(
                            since_progress,
                            since_sample,
                            stuck = ?stuck,
                            "[gil-probe] GIL DEADLOCK confirmed"
                        );
                    }
                }
            }
        })
        .map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!(
                "[gil-probe] failed to spawn watchdog thread: {e}"
            ))
        })?;

    Ok(())
}

/// Pure watchdog verdict — the dual-signal deadlock logic, factored out of
/// the spawned watchdog thread so it is unit-testable without GIL/threading.
///
/// Inputs are wall-clock-ms timestamps (0 = not yet set):
/// - `progress_ms` = `LAST_PROGRESS_MS` (Python consumer heartbeat)
/// - `sample_ms`   = `LAST_PROBE_SAMPLE_MS` (probe thread's own last acquire)
/// - `now_ms`      = current wall-clock ms
/// - `stuck_ms`    = the threshold
///
/// The verdict: a permanent GIL deadlock blocks the probe thread itself on
/// `PyGILState_Ensure`, so `sample_ms` stops advancing. Thus BOTH signals
/// must be stale for a true deadlock; a stale `progress_ms` alone (probe still
/// sampling) is just the main loop being busy (e.g. `build_paths` registering
/// no paths yet -> consumer has no batches -> heartbeat naturally idle).
#[must_use]
fn watchdog_verdict(
    progress_ms: u64,
    sample_ms: u64,
    now_ms: u64,
    stuck_ms: u64,
) -> WatchdogVerdict {
    if progress_ms == 0 || sample_ms == 0 {
        return WatchdogVerdict::NotArmed;
    }
    let since_progress = now_ms.saturating_sub(progress_ms);
    let since_sample = now_ms.saturating_sub(sample_ms);
    if since_progress >= stuck_ms && since_sample >= stuck_ms {
        WatchdogVerdict::Deadlocked {
            since_progress,
            since_sample,
        }
    } else if since_progress >= stuck_ms {
        WatchdogVerdict::Busy {
            since_progress,
            since_sample,
        }
    } else {
        WatchdogVerdict::Healthy
    }
}

/// Dump threshold for a long-Busy episode (MHE62T): after this much
/// heartbeat staleness with the probe still sampling, dump the registry once.
/// A LONG Busy episode is the signature of a NON-GIL wedge — a Rust lock held
/// across an await (incident 2026-08-21: the bot sat "busy" for 11 minutes
/// before a secondary GIL grab made the verdict flip to Deadlocked).
const BUSY_DUMP_AFTER_MS: u64 = 300_000;

/// Pure decision: has a Busy episode aged past the dump threshold?
/// Factored out of the watchdog thread for unit testing.
#[must_use]
fn should_dump_busy(since_progress_ms: u64, busy_dump_after_ms: u64) -> bool {
    since_progress_ms >= busy_dump_after_ms
}

/// The watchdog's verdict on a single tick. See [`watchdog_verdict`].
enum WatchdogVerdict {
    /// Probe hasn't completed its first sample yet (or `mark_progress` never
    /// called) — wait for more data before alarming.
    NotArmed,
    /// Both signals fresh — no alarm.
    Healthy,
    /// Heartbeat stale but probe still sampling — main loop busy, NOT a
    /// deadlock.
    Busy {
        since_progress: u64,
        since_sample: u64,
    },
    /// Both signals stale — probe thread blocked on `PyGILState_Ensure` ->
    /// a thread holds the GIL without yielding -> permanent deadlock.
    Deadlocked {
        since_progress: u64,
        since_sample: u64,
    },
}

/// Register the diagnostics pyfunctions on the module.
///
/// # Errors
/// Returns `PyErr` if a function fails to register.
pub fn add_diagnostics_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.diagnostics")?;
    submod.add_function(wrap_pyfunction!(start_gil_probe, &submod)?)?;
    submod.add_function(wrap_pyfunction!(mark_progress, &submod)?)?;
    submod.add_function(wrap_pyfunction!(failure_action, &submod)?)?;
    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.diagnostics", &submod)?;
    Ok(())
}

// Keep the unused-import linter quiet in builds without the probe wired.
fn _unused_imports_keep_arc() -> Arc<()> {
    Arc::new(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A normal rolling-start window: `mark_progress()` is stale (consumer
    /// has no batches yet during `build_paths`) BUT the probe thread is still
    /// sampling (GIL released). Must NOT alarm as a deadlock.
    #[test]
    fn busy_main_loop_with_fresh_probe_is_not_a_deadlock() {
        // 5-min rolling start; probe sampled 1s ago (fresh).
        let now = 5_000_000;
        let progress = 4_500_000; // 500s stale — alone looks stuck
        let sample = 4_999_000; // 1s ago — fresh (GIL released)
        let stuck = 30_000;
        assert!(
            matches!(
                watchdog_verdict(progress, sample, now, stuck),
                WatchdogVerdict::Busy { .. }
            ),
            "fresh probe sample must downgrade a stale heartbeat to Busy, not Deadlocked"
        );
    }

    /// A genuine permanent GIL deadlock: the probe thread itself blocks on
    /// `PyGILState_Ensure`, so BOTH `mark_progress` AND the probe's own sample
    /// stop advancing. Must alarm as Deadlocked.
    #[test]
    fn both_signals_stale_is_a_deadlock() {
        let now = 5_000_000;
        let progress = 4_000_000; // 1000s stale
        let sample = 4_000_000; // 1000s stale — probe blocked
        let stuck = 30_000;
        assert!(
            matches!(
                watchdog_verdict(progress, sample, now, stuck),
                WatchdogVerdict::Deadlocked { .. }
            ),
            "both signals stale must alarm as a permanent GIL deadlock"
        );
    }

    /// Before the first `mark_progress()` OR the probe's first sample, the
    /// watchdog must NOT alarm (insufficient data).
    #[test]
    fn not_armed_until_both_signals_seen() {
        let now = 1_000_000;
        let stuck = 30_000;
        assert!(matches!(
            watchdog_verdict(0, 1_000_000, now, stuck),
            WatchdogVerdict::NotArmed
        ));
        assert!(matches!(
            watchdog_verdict(1_000_000, 0, now, stuck),
            WatchdogVerdict::NotArmed
        ));
        assert!(matches!(
            watchdog_verdict(0, 0, now, stuck),
            WatchdogVerdict::NotArmed
        ));
    }

    /// MHE62T: a long-Busy episode crosses the dump threshold exactly once
    /// the age passes it (caller owns the once-per-episode flag).
    #[test]
    fn busy_dump_triggers_only_past_threshold() {
        assert!(!should_dump_busy(299_999, 300_000));
        assert!(should_dump_busy(300_000, 300_000));
        assert!(should_dump_busy(600_000, 300_000));
    }

    /// Both signals fresh — Healthy (no alarm at all).
    #[test]
    fn both_fresh_is_healthy() {
        let now = 1_000_000;
        assert!(matches!(
            watchdog_verdict(999_999, 999_990, now, 30_000),
            WatchdogVerdict::Healthy
        ));
    }
}
