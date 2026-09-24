//! Logging parity test: verify that Rust `log::` and `tracing::` events
//! reach Python `logging` under the correct logger names with the correct
//! levels and messages.
//!
//! This tests the O7CE2A subscriber swap: `pyo3_log::init()` replaced by
//! a `tracing_subscriber` registry with a batched Python-forwarding layer.
//! The contract (parity with `src/degenbot/logging.py`) is:
//!
//! - A `log::info!("probe {n}")` arrives at Python logger
//!   `rust_log_probe` (the Rust target with `::` → `.`) with level INFO
//!   and message body `"probe <n>"`.
//! - A `tracing::info!(n = 42, "probe")` arrives at Python logger
//!   `tracing_log_probe` with level INFO and message body `"probe"` (the
//!   structured field `n` is not part of the Python message body; it is
//!   appended as ` {n=...}` by the layer's formatting).
//!
//! Both must arrive within a bounded flush window (the drainer flushes
//! every 50 ms; we wait up to 2 s).

#![cfg(feature = "auto-initialize")]
#![expect(clippy::unwrap_used)]

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use degenbot_rs::python_log_layer;
use pyo3::prelude::*;

static INIT_LOGGING: OnceLock<()> = OnceLock::new();

fn ensure_logging_setup() {
    INIT_LOGGING.get_or_init(|| {
        // Initialize the Rust tracing subscriber (Python-forwarding layer).
        // This is normally called from `#[pymodule]`, but in a standalone
        // test we must call it explicitly. Idempotent.
        python_log_layer::init_logging_subscriber();

        // Python logging must be configured before Rust emits. We configure
        // a Python `logging` handler that captures records into a list,
        // attached to the `rust_log_probe` and `tracing_log_probe` loggers.
        Python::attach(|py| {
            let _logging = py.import("logging").unwrap();

            // Create a handler that appends records to a list on the Python
            // module itself.
            py.run(
                c"
import logging

# A handler that stores records in a list for assertions.
class CaptureHandler(logging.Handler):
    def __init__(self):
        super().__init__()
        self.records = []

    def emit(self, record):
        self.records.append({
            'name': record.name,
            'level': record.levelname,
            'msg': record.getMessage(),
        })

# Create the handler and attach to the test loggers.
_log_handler = CaptureHandler()

_rust_logger = logging.getLogger('rust_log_probe')
_rust_logger.setLevel(logging.INFO)
_rust_logger.addHandler(_log_handler)
_rust_logger.propagate = False

_tracing_logger = logging.getLogger('tracing_log_probe')
_tracing_logger.setLevel(logging.INFO)
_tracing_logger.addHandler(_log_handler)
_tracing_logger.propagate = False

# Wait for at least one flush cycle to ensure the Rust subscriber is up.
import time
time.sleep(0.1)
",
                None,
                None,
            )
            .unwrap();
        });
    });
}

#[track_caller]
fn rust_log_probe(n: i32) {
    log::info!(target: "rust_log_probe", "probe {n}");
}

#[track_caller]
fn tracing_log_probe() {
    tracing::info!(target: "tracing_log_probe", n = 42, "probe");
}

#[test]
fn test_log_reaches_python() {
    ensure_logging_setup();

    // Clear any previous records by creating a fresh CaptureHandler.
    Python::attach(|py| {
        py.run(
            c"
import logging
_log_handler.records.clear()
",
            None,
            None,
        )
        .unwrap();
    });

    // Emit a Rust log::info! call.
    rust_log_probe(99);

    // Emit a Rust tracing::info! call.
    tracing_log_probe();

    // Wait for the drainer to flush (up to 2 seconds).
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut log_found = false;
    let mut tracing_found = false;

    let log_msg = format!("probe {i}", i = 99);
    // The tracing message: "probe {n=42}"
    let tracing_msg = "probe {n=42}";

    while Instant::now() < deadline {
        Python::attach(|py| {
            let records: Vec<(String, String, String)> = py
                .eval(
                    c"[(r['name'], r['level'], r['msg']) for r in _log_handler.records]",
                    None,
                    None,
                )
                .unwrap()
                .extract()
                .unwrap();

            for (name, level, msg) in &records {
                if name == "rust_log_probe" && level == "INFO" && msg == &log_msg {
                    log_found = true;
                }
                if name == "tracing_log_probe" && level == "INFO" && msg == tracing_msg {
                    tracing_found = true;
                }
            }
        });

        if log_found && tracing_found {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(
        log_found,
        "log::info!(\"probe {{n}}\", n=99) did not reach Python logging \
         within the flush window: expected logger=rust_log_probe, \
         level=INFO, msg={log_msg:?}"
    );
    assert!(
        tracing_found,
        "tracing::info!(n = 42, \"probe\") did not reach Python logging \
         within the flush window: expected logger=tracing_log_probe, \
         level=INFO, msg={tracing_msg:?}"
    );
}
