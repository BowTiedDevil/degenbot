//! Test-only helper for the GIL/BotState concurrency contract (incident
//! 2026-08-20: a sync FFI pymethod took the BotState WRITE while holding the
//! GIL and parked behind the dispatch fan-out's long-held READ guard; the
//! reader then wanted the GIL -> permanent inversion; every other thread
//! froze on the GIL futex = the observed 'GIL deadlock').
//!
//! Rust-callable only (not exported to Python); gated on `auto-initialize`
//! like the integration tests that use it.
use super::PyBot;
use pyo3::prelude::*;
use std::thread;
use std::time::Duration;

/// Arm the incident cycle around the REAL `unregister_pool` pymethod and
/// call it from a GIL-held thread.
///
/// Ordering (matches the incident): the READER (fan-out shape) holds the
/// BotState read end FIRST; the writer (the pymethod, GIL-held by the
/// caller) then parks behind it. While parked, the reader wants the GIL
/// (`Python::attach`).
///
/// - Pre-fix (write taken GIL-held): the writer parks behind the read while
///   still holding the GIL; the reader's `attach` parks on that GIL ->
///   permanent inversion; the test's outer deadline fires (RED).
/// - Fixed (write acquired via `py.detach`): the writer parks GIL-released,
///   the reader's attach proceeds, the reader drops the read, the write
///   completes (GREEN).
///
/// Returns the holder's `JoinHandle` - join it OUTSIDE any GIL scope (the
/// holder's final `Python::attach` needs the GIL the caller still holds).
pub fn state_write_park_cycle(
    py: Python<'_>,
    bot: &PyBot,
    address: &str,
    read_hold_ms: u64,
) -> PyResult<std::thread::JoinHandle<()>> {
    let arc = bot.bot.state_arc();
    let holder = thread::spawn(move || {
        let guard = arc.read();
        thread::sleep(Duration::from_millis(read_hold_ms));
        // The reader's next GIL touch (a Python-backed data source). Under
        // the pre-fix shape this is where the cycle closes forever.
        Python::attach(|_p| ());
        drop(guard);
    });
    // Let the reader settle on the read end before the writer arrives
    // (incident ordering: fan-out mid-sim, writer arrives late).
    thread::sleep(Duration::from_millis(150));
    let _ = bot.unregister_pool(py, address, None)?;
    Ok(holder)
}

/// Incident 2026-08-20 #2: the same cycle around `update_v3_pool` (the T2
/// re-audit found the build_/register_/update_ pymethods carried the same
/// GIL-held `state_arc().write()` shape; the frozen V3 cold-build was
/// parked in build_v3_pool's registration write).
/// Pre-fix: the write parks behind the reader while holding the GIL (RED);
/// fixed (write inside `py.detach`): parks GIL-released (GREEN).
pub fn update_v3_park_cycle(
    py: Python<'_>,
    bot: &PyBot,
    address: &str,
    read_hold_ms: u64,
) -> PyResult<std::thread::JoinHandle<()>> {
    let arc = bot.bot.state_arc();
    let holder = thread::spawn(move || {
        let guard = arc.read();
        thread::sleep(Duration::from_millis(read_hold_ms));
        Python::attach(|_p| ());
        drop(guard);
    });
    thread::sleep(Duration::from_millis(150));
    let builtins = py.import("builtins")?;
    let int = builtins.getattr("int")?;
    let spx: Bound<'_, PyAny> = int.call1((1u64,))?;
    let liq: Bound<'_, PyAny> = int.call1((0u64,))?;
    let _ = bot.update_v3_pool(py, address, &spx, &liq, 0, 0)?;
    Ok(holder)
}
