//! Rust-level `GIL` contention stress test.
//!
//! Proves that `Python::attach()` cannot deadlock when concurrent threads hold
//! the `GIL`. `CPython`'s thread scheduler always yields the `GIL` (check interval,
//! I/O poll), so circular wait is impossible. This test verifies that property
//! under concurrent load.

#![cfg(feature = "auto-initialize")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use pyo3::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn test_attach_completes_under_gil_contention() {
    static GIL_HOLDER_DONE: AtomicBool = AtomicBool::new(false);
    let attach_count = Arc::new(AtomicUsize::new(0));
    let attach_count_clone = Arc::clone(&attach_count);

    // Thread A: hold GIL in a tight Python loop, then release
    let holder = thread::spawn(|| {
        Python::attach(|py| {
            // Run a tight Python loop that holds the GIL continuously
            py.run(c"for _ in range(10000): pass", None, None).unwrap();
        });
        GIL_HOLDER_DONE.store(true, std::sync::atomic::Ordering::Release);
    });

    // Thread B: repeatedly attach while Thread A may hold the GIL
    let attacher = thread::spawn(move || {
        // Wait briefly so Thread A has a chance to grab the GIL first
        thread::sleep(Duration::from_micros(100));
        for _ in 0..100 {
            Python::attach(|py| {
                let _ = py.None();
            });
            attach_count_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    });

    // Both threads must complete within 10 seconds (no deadlock)
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
