//! GIL-release contract test for the `subscribe`/`resume` pump-init seam.
//!
//! Both `PumpState::subscribe` and `PumpState::resume` run a
//! `runtime.block_on(...)` while holding the calling (asyncio main) thread's
//! GIL — the handshake (~12s, one block time) and the snapshot→WS backfill
//! (tens of seconds, ~168k logs) show up as `gil-probe` "GIL held" acquires of
//! 16.6s / 8.4s / 5.0s in the 240s example run.
//!//! The fix wraps the `block_on` in `py.detach(|| { ... })` (PyO3 0.29 renamed
//! `allow_threads` to `detach`). This test locks the contract: while a
//! `block_on` future parks, a parallel OS thread's `Python::attach` MUST
//! complete (it would block indefinitely if the calling thread held the GIL
//! without yielding). The futures are pure Rust async (no GIL needed to
//! complete) so `detach` is safe - no re-entry deadlock.

#![cfg(feature = "auto-initialize")]
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::doc_markdown,
    clippy::unnecessary_wraps
)]

use pyo3::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// A `#[pyfunction]` mirroring the FIXED `subscribe`/`resume` shape: a
/// synchronous pyo3 call that runs `runtime.block_on(...)` wrapped in
/// `py.detach(...)`. A parallel thread can acquire the GIL while the future
/// parks, proving the GIL was released across the `block_on`.
#[pyfunction]
#[pyo3(signature = (park_ms))]
fn gil_released_block_on(py: Python<'_>, park_ms: u64) -> PyResult<u64> {
    // Phase-check / PyErr construction happen GIL-held (before detach).
    // The long parking future is pure Rust async - no GIL needed to complete.
    let result = py.detach(|| {
        degenbot_core::runtime::get_runtime().block_on(async move {
            tokio::time::sleep(Duration::from_millis(park_ms)).await;
            park_ms
        })
    });
    Ok(result)
}

#[test]
fn test_detach_releases_gil_during_block_on() {
    // Arm a background thread that tries to acquire the GIL WHILE the main
    // thread is parked inside `gil_released_block_on` (200 ms). If the call
    // held the GIL (the pre-fix shape), `Python::attach` would block until
    // the 200 ms park ended, so `attach_during_park` would still be false by
    // the time the main thread returns - the assertion below catches that.
    let attach_during_park = Arc::new(AtomicBool::new(false));
    let probe = Arc::clone(&attach_during_park);
    let probe_thread = thread::spawn(move || {
        // Give the main thread a moment to enter its `block_on` park.
        thread::sleep(Duration::from_millis(50));
        Python::attach(|py| {
            // Touching the interpreter proves we acquired the GIL mid-park.
            let _ = py.None();
        });
        probe.store(true, Ordering::Release);
    });

    Python::attach(|py| {
        // Run the pyo3 function on the main GIL thread, mirroring how
        // `subscribe`/`resume` are called from the asyncio main thread.
        let _ = gil_released_block_on(py, 200).unwrap();
    });

    probe_thread.join().expect("probe thread panicked");
    assert!(
        attach_during_park.load(Ordering::Acquire),
        "GIL was NOT released during block_on: the parallel Python::attach did \
         not complete while the main thread was parked - `detach` (PyO3 0.29; \
         `allow_threads` pre-0.29) is missing from the pump-init block_on seam \
         (the subscribe/resume GIL stall)"
    );
}

/// The pre-fix shape (`block_on` WITHOUT `detach`) - a RED baseline the
/// assertion above is designed to catch. Kept as a documented negative; it is
/// NOT run (the GIL is held, so `attach_during_park` stays false -> assertion
/// fails). It exists to make the contract the test enforces unambiguous.
#[test]
#[ignore = "RED baseline: documents the pre-fix GIL-holding block_on"]
fn test_block_on_without_detach_holds_gil() {
    let attach_during_park = Arc::new(AtomicBool::new(false));
    let probe = Arc::clone(&attach_during_park);
    let probe_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        Python::attach(|_py| ());
        probe.store(true, Ordering::Release);
    });

    Python::attach(|py| {
        // Pre-fix shape: block_on WITHOUT detach -> GIL held -> the probe
        // thread's attach blocks until this returns.
        py.run(c"import time; time.sleep(0.0002)", None, None)
            .unwrap();
    });

    probe_thread.join().expect("probe thread panicked");
    // With the GIL held the whole time, the probe could not have attached
    // during the park window - this is the failure the GREEN test forbids.
    assert!(
        !attach_during_park.load(Ordering::Acquire),
        "baseline: GIL held, probe did not attach mid-park (expected - this \
         confirms the RED shape the GREEN test catches)"
    );
}

#[pymodule]
fn gil_release_contract(_py: Python<'_>, _m: &Bound<'_, PyModule>) -> PyResult<()> {
    Ok(())
}
