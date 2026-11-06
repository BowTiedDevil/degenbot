//! `PyO3` bindings for the subscription module.
//!
//! Exposes `AlloySubscription` as a Python class implementing the async iterator
//! protocol (`__aiter__` / `__anext__`) plus a `drain()` method for bulk
//! consumption and a `started()` method that awaits subscription confirmation.
//!
//! # This wrapper translates; it does not decide
//!
//! Every lifecycle fact lives in the Rust core ([`SubscriptionHandle`]): the
//! current phase, the latched startup result, and the terminal a reader observes.
//! The wrapper owns no `ended`/`disconnected` pair and no representation state —
//! it converts raw items to Python objects and translates a
//! [`TerminalNotice`] into the one Python exception that notice means, so every
//! read path (`__anext__`, `drain`, `started`, `__repr__`) reads the same
//! authority and cannot disagree with another.
//!
//! # GIL Safety
//!
//! `Python::attach()` is used in async contexts (Tokio worker threads) to
//! construct Python objects from subscription data. These may block briefly
//! while the Python event loop or another Python thread holds the GIL. This
//! cannot deadlock — see `rpc::async_provider.rs` module-level comment for the
//! full reasoning.

use crate::ambient_runtime::future_into_py;
use crate::prelude::*;
use crate::subscription::{
    DrainNotice, RawSubItem, ReadClaim, ReadOutcome, StartupResult, SubscriptionHandle,
    TerminalNotice,
};
use pyo3::exceptions::{PyRuntimeError, PyStopAsyncIteration};
use std::sync::Arc;

// =============================================================================
// GIL-bound conversion (the binding-side half of the drain)
// =============================================================================
//
// [`SubscriptionHandle::drain_raw`], [`RawDrain`], [`DrainNotice`], and
// [`TerminalNotice`] live in the `degenbot-rpc` crate (pure Rust, no `pyo3`). The
// GIL-bound conversion layer stays here because it constructs Python objects via
// `conversion::rpc_types`.

use crate::conversion::rpc_types::{block_to_py_dict, header_to_py_dict, log_to_py_dict};

/// Convert a single raw item to the Python object it represents.
fn convert_item(py: Python<'_>, item: RawSubItem) -> PyResult<Py<PyAny>> {
    match item {
        RawSubItem::Header(header) => {
            let d = header_to_py_dict(py, &header)?;
            Ok(d.into_any().unbind())
        }
        RawSubItem::Block(block) => {
            let d = block_to_py_dict(py, &block)?;
            Ok(d.into_any().unbind())
        }
        RawSubItem::PendingTxHash(hash) => {
            let hex_str = hash.to_string();
            Ok(pyo3::types::PyString::new(py, &hex_str).into_any().unbind())
        }
        RawSubItem::FullPendingTx(json_guard) => {
            let json_val = json_guard.lock().take().ok_or_else(|| {
                pyo3::exceptions::PyRuntimeError::new_err("Transaction data already consumed")
            })?;
            // A pending-transaction subscription carries transaction objects,
            // so it decodes with the transaction shape's field kinds.
            let obj = crate::conversion::rpc_types::json_to_py_with_hexbytes(
                py,
                json_val,
                crate::conversion::rpc_types::FieldSchema::TRANSACTION,
            )?;
            Ok(obj.unbind())
        }
        RawSubItem::Log(log) => {
            let d = log_to_py_dict(py, &log)?;
            Ok(d.into_any().unbind())
        }
    }
}

/// Convert a raw batch to Python objects. Called with the GIL held.
fn convert_all(py: Python<'_>, items: Vec<RawSubItem>) -> PyResult<Vec<Py<PyAny>>> {
    let mut converted = Vec::with_capacity(items.len());
    for item in items {
        converted.push(convert_item(py, item)?);
    }
    Ok(converted)
}

/// Drain the handle and convert everything to Python objects.
///
/// The typed notice crosses unchanged: this layer never decides whether the
/// subscription is over — the core's latched phase does.
fn drain_buffer(
    handle: &SubscriptionHandle,
    py: Python<'_>,
) -> PyResult<(Vec<Py<PyAny>>, Option<DrainNotice>)> {
    let raw = handle.drain_raw();
    Ok((convert_all(py, raw.items)?, raw.notice))
}

// =============================================================================
// Terminal translation: the wrapper's whole lifecycle vocabulary
// =============================================================================

/// The Python exception a terminal notice raises.
///
/// A clean end and an unsubscribe both close iteration (`StopAsyncIteration`);
/// a failure is the terminal error. This is the only place a terminal notice
/// becomes a Python outcome, so `__anext__` and `drain` cannot disagree.
fn terminal_to_pyerr(notice: TerminalNotice) -> PyErr {
    match notice {
        TerminalNotice::Ended | TerminalNotice::Unsubscribed => {
            PyStopAsyncIteration::new_err("Subscription closed")
        }
        TerminalNotice::Failed { message } => {
            PyRuntimeError::new_err(format!("Subscription failed: {message}"))
        }
    }
}

/// The Python result of a resolved startup.
fn startup_result_to_py(result: StartupResult) -> PyResult<()> {
    match result {
        StartupResult::Started => Ok(()),
        StartupResult::Failed { message } => Err(PyRuntimeError::new_err(message)),
        StartupResult::Closed { phase } => Err(PyRuntimeError::new_err(format!(
            "Subscription closed before it started ({})",
            phase.name()
        ))),
    }
}

/// Python wrapper for a subscription.
///
/// Implements the async iterator protocol so users can write:
///
/// ```python
/// async for header in subscription:
///     print(header)
/// ```
///
/// Also provides `drain()` for bulk consumption:
///
/// ```python
/// items = subscription.drain()  # Returns list[dict]
/// ```
///
/// And `started()` to await subscription confirmation:
///
/// ```python
/// await subscription.started()  # Raises on failure
/// ```
///
/// A reconnect (`phase == "reconnecting"`) is transient: iteration keeps awaiting
/// data and does not raise. Only a terminal phase ends it — `StopAsyncIteration`
/// for a clean end or an unsubscribe, `RuntimeError` for a failure.
#[pyclass(name = "AlloySubscription", module = "degenbot._ffi.provider")]
pub struct PyAlloySubscription {
    /// The shared subscription handle — the sole lifecycle authority.
    handle: Arc<SubscriptionHandle>,
}

#[pymethods]
impl PyAlloySubscription {
    /// Return self as the async iterator.
    const fn __aiter__(this: PyClassGuard<'_, Self>) -> PyClassGuard<'_, Self> {
        this
    }

    /// Return the next item from the subscription as an awaitable.
    ///
    /// Always returns a coroutine (via `future_into_py`) so that
    /// `await sub.__anext__()` and `async for item in sub` work
    /// correctly. The core's read loop resolves it as soon as an item is
    /// buffered.
    ///
    /// Raises `RuntimeError` when another read of this subscription is in
    /// progress — a concurrent `__anext__` is a driver bug, and it must not
    /// surface as a spurious `StopAsyncIteration`.
    fn __anext__<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        // Claim the read side synchronously, so a concurrent reader gets the
        // typed refusal at call time instead of racing the buffer.
        let claim = self.claim_read()?;
        let handle = Arc::clone(&self.handle);

        future_into_py(py, async move {
            // The claim lives until this future resolves: the read side is never
            // "checked out" in a way a second reader can mistake for closure.
            let _claim = claim;
            let outcome = handle.read_next().await;
            Python::attach(|py| match outcome {
                ReadOutcome::Item(item) => convert_item(py, item),
                ReadOutcome::Terminal(terminal) => Err(terminal_to_pyerr(terminal)),
            })
        })
    }

    /// Drain accumulated items from the subscription.
    ///
    /// Bulk-converts every buffered item, in arrival order, to Python objects.
    /// The list may be empty if no items have arrived since the last drain.
    ///
    /// Pre-terminal items are always returned before the terminal is reported:
    /// the terminal state is latched in the core, so a later call reports it.
    /// Raises `StopAsyncIteration` once the subscription is over (a clean end or
    /// an unsubscribe) and `RuntimeError` when it failed. A reconnect is
    /// transient and is not reported here.
    ///
    /// Raises `RuntimeError` when another read of this subscription is in
    /// progress.
    fn drain(&mut self) -> PyResult<Vec<Py<PyAny>>> {
        let _claim = self.claim_read()?;
        let (items, notice) = Python::attach(|py| drain_buffer(&self.handle, py))?;

        match notice {
            Some(DrainNotice::Terminal(terminal)) if items.is_empty() => {
                Err(terminal_to_pyerr(terminal))
            }
            Some(DrainNotice::Terminal(_) | DrainNotice::Reconnecting { .. }) | None => Ok(items),
        }
    }

    /// Wait for the subscription to be confirmed by the node.
    ///
    /// Resolves when the WS subscription is active. Raises if the subscription
    /// failed to establish, or was torn down before it started. Every waiter —
    /// early, late, or repeated — observes the same latched outcome.
    fn started<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let handle = Arc::clone(&self.handle);
        future_into_py(py, async move {
            startup_result_to_py(handle.await_startup().await)
        })
    }

    /// The current lifecycle phase, for observability.
    ///
    /// One of `starting`, `streaming`, `reconnecting`, `ended`, `failed`,
    /// `unsubscribed`. `reconnecting` is transient — the subscription is still
    /// live and iteration continues.
    #[getter]
    fn phase(&self) -> String {
        self.handle.phase().name().to_string()
    }

    /// Unsubscribe from the event stream.
    ///
    /// Aborts the background pump task and latches the terminal `unsubscribed`
    /// phase. Idempotent: a second call changes nothing, and a late pump event
    /// cannot reopen the subscription. After calling this, `__anext__()` raises
    /// `StopAsyncIteration`.
    fn unsubscribe(&self) {
        self.handle.unsubscribe();
    }

    fn __repr__(&self) -> String {
        let lifecycle = self.handle.lifecycle();
        let phase = lifecycle.phase();
        match phase.message() {
            Some(message) => {
                format!(
                    "AlloySubscription(state={}, message={message:?})",
                    phase.name()
                )
            }
            None => format!("AlloySubscription(state={})", phase.name()),
        }
    }
}

impl PyAlloySubscription {
    /// Claim the read side, translating the core's typed refusal.
    fn claim_read(&self) -> PyResult<ReadClaim> {
        self.handle
            .claim_read()
            .map_err(|refusal| PyRuntimeError::new_err(refusal.to_string()))
    }

    /// Create a new Python subscription wrapper from a Rust subscription handle.
    pub fn from_handle(handle: Arc<SubscriptionHandle>) -> Self {
        Self { handle }
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test assertions fail loudly")]

    use super::{startup_result_to_py, terminal_to_pyerr, PyAlloySubscription};
    use crate::prelude::*;
    use crate::subscription::{
        LifecycleEvent, RawSubItem, StartupResult, SubscriptionHandle, SubscriptionPhase,
        TerminalNotice,
    };
    use std::sync::Arc;

    fn handle() -> Arc<SubscriptionHandle> {
        Arc::new(SubscriptionHandle::new())
    }

    fn streaming() -> Arc<SubscriptionHandle> {
        let handle = handle();
        handle.apply(LifecycleEvent::Started).unwrap();
        handle
    }

    /// The Python exception's message, without the type prefix `Display` adds.
    fn message(error: &PyErr) -> String {
        pyo3::Python::attach(|py| error.value(py).to_string())
    }

    /// `__repr__` reports the core phase — it infers no lifecycle of its own.
    #[test]
    fn repr_reports_the_core_phase() {
        let subscription = PyAlloySubscription::from_handle(handle());
        assert_eq!(subscription.__repr__(), "AlloySubscription(state=starting)");

        let subscription = PyAlloySubscription::from_handle(streaming());
        assert_eq!(
            subscription.__repr__(),
            "AlloySubscription(state=streaming)"
        );
        assert_eq!(subscription.phase(), "streaming");

        let reconnecting = streaming();
        reconnecting
            .apply(LifecycleEvent::Stall {
                message: "no header within watchdog window".to_string(),
            })
            .unwrap();
        let subscription = PyAlloySubscription::from_handle(reconnecting);
        assert_eq!(
            subscription.__repr__(),
            "AlloySubscription(state=reconnecting, message=\"no header within watchdog window\")"
        );
        assert_eq!(
            subscription.phase(),
            "reconnecting",
            "a transient reconnect is observable but not terminal"
        );
    }

    /// `drain()` returns the pre-terminal items in arrival order; the terminal the
    /// core latched is reported by the *later* call.
    #[test]
    fn drain_returns_items_then_reports_the_latched_terminal() {
        let handle = streaming();
        handle.push_item(RawSubItem::Log(alloy::rpc::types::Log::default()));
        handle.apply(LifecycleEvent::End).unwrap();

        let mut subscription = PyAlloySubscription::from_handle(Arc::clone(&handle));
        assert_eq!(subscription.drain().unwrap().len(), 1, "the log is drained");

        let terminal = subscription.drain().unwrap_err();
        pyo3::Python::attach(|py| {
            assert!(terminal.is_instance_of::<pyo3::exceptions::PyStopAsyncIteration>(py));
        });
        assert_eq!(subscription.phase(), "ended");
    }

    /// The wrapper owns no terminal fields: repeated drains re-read the core's
    /// latched terminal instead of remembering one locally.
    #[test]
    fn the_terminal_state_is_latched_in_the_core_not_the_wrapper() {
        let handle = streaming();
        handle.apply(LifecycleEvent::End).unwrap();
        let mut subscription = PyAlloySubscription::from_handle(Arc::clone(&handle));

        for _ in 0..2 {
            let terminal = subscription.drain().unwrap_err();
            pyo3::Python::attach(|py| {
                assert!(terminal.is_instance_of::<pyo3::exceptions::PyStopAsyncIteration>(py));
            });
        }
        assert_eq!(subscription.__repr__(), "AlloySubscription(state=ended)");
    }

    /// A failure is the terminal error, not a silent close.
    #[test]
    fn a_failed_subscription_raises_the_terminal_error() {
        let handle = streaming();
        handle
            .apply(LifecycleEvent::Fail {
                message: "transport died".to_string(),
            })
            .unwrap();
        let mut subscription = PyAlloySubscription::from_handle(handle);

        let failure = subscription.drain().unwrap_err();
        pyo3::Python::attach(|py| {
            assert!(failure.is_instance_of::<pyo3::exceptions::PyRuntimeError>(py));
        });
        assert_eq!(message(&failure), "Subscription failed: transport died");
    }

    /// A concurrent read gets the typed refusal, never a fake closure.
    #[test]
    fn a_concurrent_read_is_refused_while_another_read_holds_the_side() {
        let handle = streaming();
        let mut subscription = PyAlloySubscription::from_handle(Arc::clone(&handle));

        let claim = handle.claim_read().unwrap();
        let refusal = subscription.drain().unwrap_err();
        pyo3::Python::attach(|py| {
            assert!(refusal.is_instance_of::<pyo3::exceptions::PyRuntimeError>(py));
            assert!(
                !refusal.is_instance_of::<pyo3::exceptions::PyStopAsyncIteration>(py),
                "a checked-out reader must not look like a closed subscription"
            );
        });
        assert_eq!(
            message(&refusal),
            "another read of this subscription is in progress"
        );

        drop(claim);
        assert!(
            subscription.drain().is_ok(),
            "the read side is released with the claim"
        );
    }

    /// Unsubscribing is idempotent and terminal.
    #[test]
    fn unsubscribe_is_idempotent_and_terminal() {
        let handle = streaming();
        let mut subscription = PyAlloySubscription::from_handle(handle);

        subscription.unsubscribe();
        subscription.unsubscribe();
        assert_eq!(subscription.phase(), "unsubscribed");
        assert_eq!(
            subscription.__repr__(),
            "AlloySubscription(state=unsubscribed)"
        );

        let terminal = subscription.drain().unwrap_err();
        pyo3::Python::attach(|py| {
            assert!(terminal.is_instance_of::<pyo3::exceptions::PyStopAsyncIteration>(py));
        });
    }

    /// The startup translation table: success is silent, a startup failure is its
    /// message, and a handle torn down before startup says so by phase.
    #[test]
    fn the_startup_translation_table_matches_the_terminal_vocabulary() {
        pyo3::Python::attach(|py| {
            assert!(startup_result_to_py(StartupResult::Started).is_ok());

            let failed = startup_result_to_py(StartupResult::Failed {
                message: "no endpoint".to_string(),
            })
            .unwrap_err();
            assert_eq!(message(&failed), "no endpoint");
            assert!(failed.is_instance_of::<pyo3::exceptions::PyRuntimeError>(py));

            let closed = startup_result_to_py(StartupResult::Closed {
                phase: SubscriptionPhase::Unsubscribed,
            })
            .unwrap_err();
            assert!(
                message(&closed).contains("Subscription closed before it started (unsubscribed)")
            );
        });
    }

    /// The terminal translation table: a clean end and an unsubscribe close
    /// iteration, a failure raises.
    #[test]
    fn the_terminal_translation_table_is_exhaustive() {
        pyo3::Python::attach(|py| {
            for terminal in [TerminalNotice::Ended, TerminalNotice::Unsubscribed] {
                let error = terminal_to_pyerr(terminal);
                assert!(error.is_instance_of::<pyo3::exceptions::PyStopAsyncIteration>(py));
            }
            let error = terminal_to_pyerr(TerminalNotice::Failed {
                message: "transport died".to_string(),
            });
            assert!(error.is_instance_of::<pyo3::exceptions::PyRuntimeError>(py));
            assert_eq!(message(&error), "Subscription failed: transport died");
        });
    }
}
