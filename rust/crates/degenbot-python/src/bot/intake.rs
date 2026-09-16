//! The fleet registration intake's `PyO3` surface (PRG-3): the Python
//! driver submits its pool-build callables as fleet units to the
//! `PoolStateUpdater` intake executor and joins each unit's receipt. The
//! legacy stance never sees this module — the `c_api` register site gates
//! it on the installed fleet boot (construction-time stance like the
//! executor field, never read per call).
//!
//! GIL cadence (mirrors the incumbent worker threads): the seat attaches
//! once to invoke the callable; the callable's Rust-builder sections
//! release the GIL through the existing `py.detach` seams, so pooled
//! seats run concurrently through the Rust core — the identical runtime
//! the legacy `ThreadPoolExecutor` provided (parity gate).

use degenbot_core::op_warn;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pyo3::prelude::*;

use crate::bot::engine::intake_faulted;
use degenbot_bot::fleet_intake::IntakeFaultWatch;

/// The seat→waiter outcome (`Send` because `Py<T>` and `PyErr` are).
type IntakeOutcome = Result<Py<PyAny>, PyErr>;

/// One submitted intake unit's completion handle. `done()` probes
/// completion cheaply, `result()` delivers the callable's return value
/// or re-raises its exception (repeatable — the outcome is stored, not
/// consumed), `wait()` is the blocking GIL-detached join, and
/// `wait_async()` is the asyncio-native awaitable on the shared runtime.
#[pyclass(name = "IntakeReceipt", module = "degenbot._ffi")]
pub struct PyIntakeReceipt {
    /// The stored outcome (one unit, one delivery — repeatable reads).
    outcome: Arc<Mutex<Option<IntakeOutcome>>>,
    /// The completion signal: one `()` delivery after the outcome lands
    /// (`!Sync` receiver gated behind a mutex; `Arc` so `wait_async` can
    /// move a clone into the blocking task).
    signal_rx: Arc<Mutex<Receiver<()>>>,
    /// Set by the seat right after the outcome lands — a cheap probe for
    /// the driver (no parked waiter thread per unit).
    done: Arc<AtomicBool>,
    /// TB4QGX T6 (spike S2): this executor's fault watch. When the sticky
    /// lane-death latch faults the intake, `wait`/`result` resolve terminally
    /// instead of parking forever.
    fault: Option<Arc<IntakeFaultWatch>>,
}

/// Block until the unit completes, the intake faults, or `timeout` elapses.
/// The fault is polled in short slices because `std::sync::mpsc::Receiver`
/// is not selectable (the fault is rare, so the 20 ms slice is negligible).
fn join_signals(
    signal_rx: &std::sync::Mutex<Receiver<()>>,
    done: &AtomicBool,
    fault: Option<&IntakeFaultWatch>,
    timeout: Option<Duration>,
) -> Result<(), PyErr> {
    use std::sync::mpsc::RecvTimeoutError;
    let timeout_secs = timeout.map(|d| d.as_secs_f64());
    let deadline = timeout.map(|d| std::time::Instant::now() + d);
    loop {
        if done.load(Ordering::Relaxed) {
            return Ok(());
        }
        if let Some(fault) = fault.and_then(IntakeFaultWatch::snapshot) {
            return Err(intake_faulted(fault));
        }
        let slice = match deadline {
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    return Err(pyo3::exceptions::PyTimeoutError::new_err(format!(
                        "intake unit did not complete in {}s",
                        timeout_secs.unwrap_or_default()
                    )));
                }
                remaining.min(Duration::from_millis(20))
            }
            None => Duration::from_millis(20),
        };
        let guard = signal_rx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match guard.recv_timeout(slice) {
            Ok(()) => return Ok(()),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(
                    "intake executor dropped the receipt channel",
                ));
            }
        }
    }
}

impl PyIntakeReceipt {
    fn lock_outcome(&self) -> std::sync::MutexGuard<'_, Option<IntakeOutcome>> {
        self.outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[pymethods]
impl PyIntakeReceipt {
    /// Non-blocking probe: has the unit completed? The driver's asyncio
    /// join polls this (one relaxed atomic load) instead of parking a
    /// waiter thread per in-flight build.
    #[must_use]
    pub fn done(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }

    /// The unit's result (call once [`Self::done`] turns true). Raises the
    /// callable's exception if the build failed.
    ///
    /// # Errors
    /// `RuntimeError` when the unit has not completed yet; the callable's
    /// own exception on build failure.
    pub fn result(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let guard = self.lock_outcome();
        match &*guard {
            Some(Ok(value)) => Ok(value.clone_ref(py)),
            Some(Err(err)) => Err(err.clone_ref(py)),
            None => match self.fault.as_ref().and_then(|watch| watch.snapshot()) {
                Some(fault) => Err(intake_faulted(fault)),
                None => Err(pyo3::exceptions::PyRuntimeError::new_err(
                    "intake unit has not completed yet (poll done() first)",
                )),
            },
        }
    }

    /// Blocking join (legacy-future parity). The parked recv runs
    /// GIL-DETACHED — a waiter holding the GIL would deadlock its own
    /// unit (the seat's `Python::attach` could never acquire it).
    ///
    /// # Errors
    /// `TimeoutError` when the unit did not complete in time; the
    /// callable's own exception on build failure.
    #[pyo3(signature = (timeout=None))]
    fn wait(&self, py: Python<'_>, timeout: Option<f64>) -> PyResult<Py<PyAny>> {
        let dur = timeout.map(|secs| {
            Duration::try_from_secs_f64(secs.max(0.0)).unwrap_or(Duration::from_secs(1))
        });
        let outcome =
            py.detach(|| join_signals(&self.signal_rx, &self.done, self.fault.as_deref(), dur));
        outcome?;
        self.result(py)
    }

    /// The asyncio-native join: an awaitable that resolves on the shared
    /// tokio runtime when the unit completes (no parked waiter thread, no
    /// poll loop — the driver awaits it directly).
    ///
    /// # Errors
    /// `RuntimeError` when the signal channel dropped without a delivery
    /// (executor died); the callable's own exception on build failure.
    fn wait_async<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let signal_rx = Arc::clone(&self.signal_rx);
        let done = Arc::clone(&self.done);
        let fault = self.fault.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            tokio::task::spawn_blocking(move || {
                join_signals(&signal_rx, &done, fault.as_deref(), None)
            })
            .await
            .map_err(|join_err| {
                pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "intake receipt join failed: {join_err}"
                ))
            })?
        })
    }

    #[expect(
        clippy::unused_self,
        reason = "__repr__ is a pyo3 protocol method — the receiver is part of the protocol shape"
    )]
    fn __repr__(&self) -> &'static str {
        "IntakeReceipt()"
    }
}

/// Submit one pool-build callable to the fleet intake executor (PRG-3).
/// The callable runs on a pooled `work-fleet-poolupd-{n}` seat; its slot
/// grant is bounded by the budget's `pool_state_updater_slots` and its
/// admission rides the Deferrable cordon class. Never drops: a full
/// per-role queue spills to the executor's FIFO backlog.
///
/// FF-T1: a refused fleet boot raises the TYPED `BootRefused`
/// exception (detected budget + floor + one operator hint) BEFORE any
/// unit is built or enqueued — the sticky materializer re-surfaces the
/// same refusal on every submit, and the host process survives.
///
/// # Errors
/// `BootRefused` when the fleet host refused to boot (the typed, sticky
/// boot-refusal family; the library never aborts the host process).
pub fn submit(fn_work: Py<PyAny>) -> PyResult<PyIntakeReceipt> {
    let (sig_tx, sig_rx) = std::sync::mpsc::channel::<()>();
    let receipt = PyIntakeReceipt {
        outcome: Arc::new(Mutex::new(None)),
        signal_rx: Arc::new(Mutex::new(sig_rx)),
        done: Arc::new(AtomicBool::new(false)),
        fault: degenbot_bot::fleet_intake::registration_fault_watch(),
    };
    let done = Arc::clone(&receipt.done);
    let outcome_slot = Arc::clone(&receipt.outcome);
    // FF-T1: submit checks the boot state FIRST — a refused boot
    // raises the typed BootRefused before any unit, channel, or receipt is
    // created (never an enqueue into a pipe that will not be drained).
    let intake = degenbot_bot::fleet_intake::registration_intake()
        .map_err(crate::bot::engine::boot_refused)?;
    intake.spawn(Box::new(move || {
        let outcome = Python::attach(|py| {
            // GOQWCL: propagate the seat's Rust thread name into Python —
            // an anonymous C thread registers as `Dummy-N`, hiding which
            // fleet seat executed the unit (py-spy/operator
            // greppability). The per-call rename is idempotent.
            let seat = std::thread::current();
            if let Some(name) = seat.name() {
                let _ = py
                    .import("threading")
                    .and_then(|m| m.call_method0("current_thread"))
                    .and_then(|t| t.call_method1("setName", (name,)));
            }
            fn_work.call0(py)
        });
        *outcome_slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(outcome);
        done.store(true, Ordering::Relaxed);
        // A waiting driver may be gone (cancelled task): log it, never
        // crash a warm seat.
        if sig_tx.send(()).is_err() {
            op_warn!(
                domain = ingest,
                "intake unit completed with no waiting consumer"
            );
        }
    }));
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use degenbot_bot::fleet_intake::{IntakeFault, IntakeFaultWatch};

    /// TB4QGX T7 (carried from the T6 acceptance): a lane death latches the
    /// sticky watch; a waiting receipt's `join_signals` resolves to the TYPED
    /// `FleetIntakeFaultedError` instead of parking forever — the S2 seam's
    /// Python-observable half.
    ///
    /// FALSIFICATION: a bare `PyRuntimeError` (or a timeout) instead of the
    /// typed class, or a parked waiter (the test would hang, not fail).
    #[test]
    #[expect(clippy::expect_used)] // the fault is asserted to surface, not just not-panic
    fn join_signals_resolves_a_lane_death_to_the_typed_fault_error() {
        let (_tx, rx) = std::sync::mpsc::channel::<()>();
        let watch = IntakeFaultWatch::new();
        watch.set(IntakeFault {
            cause: "lane-death",
            held: 3,
        });
        let done = AtomicBool::new(false);
        let err = join_signals(&Mutex::new(rx), &done, Some(&watch), None)
            .expect_err("a latched fault must not park");
        Python::attach(|py| {
            assert!(
                err.is_instance_of::<crate::bot::engine::FleetIntakeFaultedError>(py),
                "typed FleetIntakeFaultedError expected, got {err:?}"
            );
        });
    }

    /// The fault message names the cause and the held-unit count (the
    /// operator greppability contract) — the Python-facing half of
    /// `intake_faulted`.
    #[test]
    fn intake_faulted_message_names_the_cause_and_held_count() {
        let err = intake_faulted(IntakeFault {
            cause: "lane-death",
            held: 7,
        });
        let text = err.to_string();
        assert!(text.contains("lane-death"), "cause missing: {text}");
        assert!(
            text.contains("7 held unit(s)"),
            "held count missing: {text}"
        );
        Python::attach(|py| {
            assert!(
                err.is_instance_of::<crate::bot::engine::FleetIntakeFaultedError>(py),
                "typed class expected"
            );
        });
    }
}
