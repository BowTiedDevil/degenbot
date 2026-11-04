//! Rust-level `GIL` contention stress test.
//!
//! Proves that `Python::attach()` cannot deadlock when concurrent threads hold
//! the `GIL`. `CPython`'s thread scheduler always yields the `GIL` (check interval,
//! I/O poll), so circular wait is impossible. This test verifies that property
//! under concurrent load.

#![cfg(feature = "auto-initialize")]
#![expect(clippy::unwrap_used, clippy::expect_used)]

use pyo3::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Arc;
use std::thread;

/// The holder raises this from INSIDE its `Python::attach`, so the attacher can
/// order its first attach after the GIL has demonstrably been taken.
static GIL_HELD: AtomicBool = AtomicBool::new(false);
static GIL_HOLDER_DONE: AtomicBool = AtomicBool::new(false);

#[test]
fn test_attach_completes_under_gil_contention() {
    let attach_count = Arc::new(AtomicUsize::new(0));
    let attach_count_clone = Arc::clone(&attach_count);

    // Thread A: hold GIL in a tight Python loop, then release
    let holder = thread::spawn(|| {
        Python::attach(|py| {
            GIL_HELD.store(true, std::sync::atomic::Ordering::Release);
            // Run a tight Python loop that holds the GIL continuously
            py.run(c"for _ in range(10000): pass", None, None).unwrap();
        });
        GIL_HOLDER_DONE.store(true, std::sync::atomic::Ordering::Release);
    });

    // Thread B: repeatedly attach, starting once Thread A holds the GIL.
    //
    // The overlap is ORDERED, not timed: `GIL_HELD` is raised by the holder
    // from inside its attach, so the first attach below is issued after the
    // GIL has been taken. Which thread then wins each switch is deliberately
    // not asserted — the claim under test is that attach never deadlocks
    // while another thread holds the GIL, and the counters below are its
    // non-vacuity.
    let attacher = thread::spawn(move || {
        while !GIL_HELD.load(std::sync::atomic::Ordering::Acquire) {
            thread::yield_now();
        }
        for _ in 0..100 {
            Python::attach(|py| {
                let _ = py.None();
            });
            attach_count_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    });

    holder.join().expect("GIL holder thread panicked");
    attacher.join().expect("Attacher thread panicked");

    assert_eq!(
        attach_count.load(std::sync::atomic::Ordering::Relaxed),
        100,
        "All 100 attach calls must complete"
    );
    assert!(
        GIL_HOLDER_DONE.load(std::sync::atomic::Ordering::Relaxed),
        "GIL holder must have released"
    );
}
