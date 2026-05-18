//! `PyO3` bindings for the subscription module.
//!
//! Exposes `AlloySubscription` as a Python class implementing the async iterator
//! protocol (`__aiter__` / `__anext__`) plus a `drain()` method for bulk
//! consumption and a `started()` method that awaits WS subscription confirmation.

use crate::subscription::{drain_buffer, DrainResult, SubscriptionHandle};
use pyo3::exceptions::{PyRuntimeError, PyStopAsyncIteration};
use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;
use std::sync::atomic::Ordering;
use std::sync::Arc;

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
#[pyclass(name = "AlloySubscription")]
pub struct PyAlloySubscription {
    /// The shared subscription handle.
    handle: Arc<SubscriptionHandle>,
    /// Local batch: items drained from Rust but not yet yielded by __anext__.
    /// Shared between sync `drain()` and async `__anext__()` future.
    local_batch: Arc<parking_lot::Mutex<Vec<Py<PyAny>>>>,
    /// Whether the subscription has ended (End marker received).
    ended: bool,
    /// Whether the subscription has disconnected.
    disconnected: Option<String>,
}

#[pymethods]
impl PyAlloySubscription {
    /// Return self as the async iterator.
    const fn __aiter__(this: PyRef<'_, Self>) -> PyRef<'_, Self> {
        this
    }

    /// Return the next item from the subscription as an awaitable.
    ///
    /// Always returns a coroutine (via `future_into_py`) so that
    /// `await sub.__anext__()` and `async for item in sub` work
    /// correctly. The fast path (items already in local batch or
    /// Rust buffer) resolves the future immediately.
    fn __anext__<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        // Check terminal states
        if self.ended {
            return Err(PyStopAsyncIteration::new_err("Subscription ended"));
        }
        if let Some(ref msg) = self.disconnected {
            return Err(PyRuntimeError::new_err(format!(
                "Subscription disconnected: {msg}"
            )));
        }

        // Fast path: return from local batch or non-blocking drain
        if let Some(result) = self.try_fast_drain(py)? {
            return Ok(result);
        }

        // Slow path: wait for pump notification, then drain
        let handle = Arc::clone(&self.handle);
        let local_batch = Arc::clone(&self.local_batch);

        future_into_py(py, async move {
            loop {
                // Take the notify_rx, await, put it back
                let mut rx = handle
                    .notify_rx
                    .lock()
                    .take()
                    .ok_or_else(|| PyStopAsyncIteration::new_err("Subscription closed"))?;

                let recv_result = rx.recv().await;
                *handle.notify_rx.lock() = Some(rx);

                match recv_result {
                    Some(()) => {}
                    None => return Err(PyStopAsyncIteration::new_err("Subscription closed")),
                }

                // Drain the buffer with GIL
                let drain_result = Python::attach(|py| drain_buffer(&handle, py))?;

                match drain_result {
                    DrainResult::Items(items) if items.is_empty() => {
                        // Spurious wake — no items yet, loop and wait again
                    }
                    DrainResult::Items(mut items) => {
                        let first = items.remove(0);
                        if !items.is_empty() {
                            local_batch.lock().extend(items);
                        }
                        return Ok(first);
                    }
                    DrainResult::Ended(mut items) => {
                        if let Some(first) = items.pop() {
                            if !items.is_empty() {
                                local_batch.lock().extend(items);
                            }
                            return Ok(first);
                        }
                        return Err(PyStopAsyncIteration::new_err("Subscription ended"));
                    }
                    DrainResult::Disconnected {
                        mut items,
                        message,
                    } => {
                        if let Some(first) = items.pop() {
                            if !items.is_empty() {
                                local_batch.lock().extend(items);
                            }
                            return Ok(first);
                        }
                        return Err(PyRuntimeError::new_err(format!(
                            "Subscription disconnected: {message}"
                        )));
                    }
                }
            }
        })
    }

    /// Drain accumulated items from the subscription.
    ///
    /// Swaps the internal double-buffer and bulk-converts all accumulated
    /// items to Python dicts. Returns a list of items. The list may be
    /// empty if no items have arrived since the last drain.
    ///
    /// Raises `StopAsyncIteration` if the subscription has ended.
    /// Raises `RuntimeError` if the subscription has disconnected.
    fn drain(&mut self) -> PyResult<Vec<Py<PyAny>>> {
        if self.ended {
            return Err(PyStopAsyncIteration::new_err("Subscription ended"));
        }
        if let Some(ref msg) = self.disconnected {
            return Err(PyRuntimeError::new_err(format!(
                "Subscription disconnected: {msg}"
            )));
        }

        // First, yield any items in the local batch
        let mut result = Vec::new();
        let mut batch = self.local_batch.lock();
        result.append(&mut *batch);
        drop(batch);

        // Then drain the Rust buffer
        let drain_result = Python::attach(|py| drain_buffer(&self.handle, py))?;
        match drain_result {
            DrainResult::Items(items) => {
                result.extend(items);
                Ok(result)
            }
            DrainResult::Ended(items) => {
                result.extend(items);
                self.ended = true;
                Ok(result)
            }
            DrainResult::Disconnected { items, message } => {
                result.extend(items);
                self.disconnected = Some(message.clone());
                if result.is_empty() {
                    Err(PyRuntimeError::new_err(format!(
                        "Subscription disconnected: {message}"
                    )))
                } else {
                    Ok(result)
                }
            }
        }
    }

    /// Wait for the subscription to be confirmed by the node.
    ///
    /// Resolves when the WS subscription is active. Raises if the
    /// subscription failed to establish.
    fn started<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        // Fast path: already started/failed
        if self.handle.started.load(Ordering::Acquire) {
            return Ok(py.None().into_bound(py).into_any());
        }
        if self.handle.start_failed.load(Ordering::Acquire) {
            let msg = self
                .handle
                .start_error
                .lock()
                .clone()
                .unwrap_or_else(|| "Subscription failed to start".into());
            return Err(PyRuntimeError::new_err(msg));
        }

        let handle = Arc::clone(&self.handle);

        future_into_py(py, async move {
            let mut rx = handle
                .start_notify_rx
                .lock()
                .take()
                .ok_or_else(|| {
                    PyRuntimeError::new_err("Start notification channel already consumed")
                })?;

            let _ = rx.recv().await;

            if handle.started.load(Ordering::Acquire) {
                Ok(())
            } else if handle.start_failed.load(Ordering::Acquire) {
                let msg = handle
                    .start_error
                    .lock()
                    .clone()
                    .unwrap_or_else(|| "Subscription failed to start".into());
                Err(PyRuntimeError::new_err(msg))
            } else {
                Err(PyRuntimeError::new_err(
                    "Start notification received but subscription state is ambiguous",
                ))
            }
        })
    }

    /// Unsubscribe from the event stream.
    ///
    /// Stops the background pump task. After calling this,
    /// `__anext__()` will raise `StopAsyncIteration`.
    fn unsubscribe(&self) {
        self.handle.unsubscribe();
    }

    fn __repr__(&self) -> String {
        let state = if self.ended {
            "ended"
        } else if self.disconnected.is_some() {
            "disconnected"
        } else {
            "active"
        };
        format!("AlloySubscription(state={state})")
    }
}

impl PyAlloySubscription {
    /// Try to get an item immediately from the local batch or Rust buffer.
    /// Returns `Some(future)` if an item (or terminal state) is ready,
    /// `None` if no items are available and the slow path is needed.
    fn try_fast_drain<'py>(
        &mut self,
        py: Python<'py>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        // Fast path: return from local batch
        let item = self.local_batch.lock().pop();
        if let Some(item) = item {
            return Ok(Some(future_into_py(py, async move { Ok(item) })?));
        }

        // Try non-blocking drain from Rust buffer
        let drain_result = Python::attach(|py| drain_buffer(&self.handle, py))?;
        match drain_result {
            DrainResult::Items(items) if items.is_empty() => Ok(None),
            DrainResult::Items(mut items) => {
                let first = items.remove(0);
                if !items.is_empty() {
                    self.local_batch.lock().extend(items);
                }
                Ok(Some(future_into_py(py, async move { Ok(first) })?))
            }
            DrainResult::Ended(mut items) => {
                self.ended = true;
                if let Some(first) = items.pop() {
                    if !items.is_empty() {
                        self.local_batch.lock().extend(items);
                    }
                    let result: PyResult<Py<PyAny>> = Ok(first);
                    Ok(Some(future_into_py(py, async move { result })?))
                } else {
                    let result: PyResult<Py<PyAny>> = Err(PyStopAsyncIteration::new_err("Subscription ended"));
                    Ok(Some(future_into_py(py, async move { result })?))
                }
            }
            DrainResult::Disconnected { mut items, message } => {
                self.disconnected = Some(message.clone());
                if let Some(first) = items.pop() {
                    if !items.is_empty() {
                        self.local_batch.lock().extend(items);
                    }
                    Ok(Some(future_into_py(py, async move { Ok(first) })?))
                } else {
                    let result: PyResult<Py<PyAny>> = Err(PyRuntimeError::new_err(format!(
                        "Subscription disconnected: {message}"
                    )));
                    Ok(Some(future_into_py(py, async move { result })?))
                }
            }
        }
    }

    /// Create a new Python subscription wrapper from a Rust subscription handle.
    pub fn from_handle(handle: Arc<SubscriptionHandle>) -> Self {
        Self {
            handle,
            local_batch: Arc::new(parking_lot::Mutex::new(Vec::new())),
            ended: false,
            disconnected: None,
        }
    }
}
