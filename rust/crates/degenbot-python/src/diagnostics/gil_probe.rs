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
#[allow(clippy::too_many_arguments)]
fn start_gil_probe(interval_ms: u64, threshold_ms: u64, stuck_ms: u64) -> PyResult<()> {
    if PROBE_RUNNING.swap(true, Ordering::SeqCst) {
        log::warn!("[gil-probe] already running — start_gil_probe() call ignored (idempotent)");
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
            log::info!(
                "[gil-probe] sampling every {interval:?} (warn threshold {threshold:?})"
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
                if elapsed >= threshold {
                    log::warn!(
                        "[gil-probe] GIL held: acquire took {elapsed:?} (gap since last sample {gap}ms) — \
                         main thread is holding the GIL without yielding (sync pyo3 call or _asyncio futex park)"
                    );
                } else {
                    log::debug!(
                        "[gil-probe] GIL acquire {elapsed:?} (gap {gap}ms)"
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
    thread::Builder::new()
        .name("gil-probe-watchdog".to_string())
        .spawn(move || {
            log::info!(
                "[gil-probe] stuck-watchdog armed (stuck threshold {stuck:?})"
            );
            loop {
                thread::sleep(Duration::from_secs(5));
                let last = LAST_PROGRESS_MS.load(Ordering::Relaxed);
                let now = now_ms();
                if last == 0 {
                    continue;
                }
                let since = now.saturating_sub(last);
                if since >= u64::try_from(stuck.as_millis()).unwrap_or(u64::MAX) {
                    log::error!(
                        "[gil-probe] *** MAIN LOOP STUCK: no mark_progress() for {since}ms (threshold {stuck:?}) — permanent GIL deadlock suspected"
                    );
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

/// Register the diagnostics pyfunctions on the module.
///
/// # Errors
/// Returns `PyErr` if a function fails to register.
pub fn add_diagnostics_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.diagnostics")?;
    submod.add_function(wrap_pyfunction!(start_gil_probe, &submod)?)?;
    submod.add_function(wrap_pyfunction!(mark_progress, &submod)?)?;
    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.diagnostics", &submod)?;
    Ok(())
}

// Keep the unused-import linter quiet in builds without the probe wired.
#[allow(dead_code)]
fn _unused_imports_keep_arc() -> Arc<()> {
    Arc::new(())
}
