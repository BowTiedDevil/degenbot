//! Python↔Rust pub/sub bridge: a `PoolStateSubscriber` adapter that forwards
//! Rust `BotState` mutation notifications into Python callbacks via a
//! batched, GIL-free channel.
//!
//! The Rust pub/sub mechanism (`degenbot_bot::bot_core::log_dispatcher` — the
//! `PoolStateSubscriber` trait + `LogDispatcher` `Weak`-fan-out +
//! `EngineSubscriber`) is the single notification path for ALL Rust-owned state
//! changes once a pool's state is Rust-owned. This adapter is the seam that
//! lets a `#[pyclass]` / plain-Python subscriber register against that SAME
//! `LogDispatcher` path the engine uses — so Rust-owned mutation notifies Rust
//! AND Python subscribers through one fan-out (replacing the parallel Python
//! `PublisherMixin._notify_subscribers` once the pool consumers cut over).
//!
//! # GIL discipline (5FHHKL fix)
//!
//! `LogDispatcher::notify` fires from the pump's async task (GIL-free context)
//! per decoded log. Previously, `on_pool_state_updated` re-acquired the GIL via
//! `Python::attach` **per subscriber per log** — a per-record GIL round-trip on
//! the pump's tokio workers. Now, the adapter pushes the `(callback, pool_id)`
//! pair onto a bounded queue, and a dedicated OS thread (`subscriber-drainer`)
//! batches notifications and forwards them to Python via ONE `Python::attach`
//! per flush. This removes the LAST `Python::attach` from the pump's per-log
//! decode→apply→notify spine.
//!
//! # Unbounded queue + coalesce-to-latest-per-pool at flush time
//!
//! The subscriber notify queue is an **unbounded** lock-free queue
//! ([`SegQueue`](crossbeam_queue::SegQueue)) — overflow is impossible by
//! design. The drainer never blocks the emitter (the pump's notify path),
//! and the OS thread always keeps up (50ms flush interval, 256-entry
//! batches). At flush time, coalesce-to-latest-per-pool is applied: a
//! `pool_id` appears at most once per batch per subscriber. A backrun bot
//! solves off `BotState`'s current view, not every intermediate transition,
//! so coalescing within a flush window is correct.
//!
//! # §4.2 parity
//!
//! The oracle is `src/degenbot/types/concrete.py::PublisherMixin._notify_subscribers`:
//! `for subscriber in self._subscribers: subscriber.notify(publisher=self,
//! message=message)`. The Rust fan-out order is identical — `LogDispatcher::notify`
//! iterates the `Vec<Weak<dyn PoolStateSubscriber>>` in registration order,
//! upgrading each. Pinned by `tests/test_pubsub_seam_parity.py`.
//!
//! The Rust notify carries only `pool_id` (not the full Python `PoolStateUpdated`
//! state object) — reconstructing the latter requires reading `BotState` + building
//! the Python state wrapper, which is the Python cutover (retiring `PublisherMixin`
//! on pool classes), a deferred sibling task. The adapter surfaces the
//! `pool_id` to Python; the cutover task will widen the payload.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_queue::SegQueue;
use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::types::PyModule;

use degenbot_bot::bot_core::log_dispatcher::PoolStateSubscriber;

use crate::bot::PyBot;

// --- Bounded subscriber notify queue (GIL-free channel) ---

/// Maximum batch size forwarded per `Python::attach` flush.
const SUBSCRIBER_BATCH_SIZE: usize = 256;

/// Maximum time between flushes — a partially full batch is flushed after
/// this interval to avoid starving the Python side.
const SUBSCRIBER_FLUSH_INTERVAL: Duration = Duration::from_millis(50);

/// A pending subscriber notification: `(callback, pool_id)`.
///
/// Uses a raw `*mut ffi::PyObject` pointer (with incremented refcount) so
/// the notification can be sent between threads without the GIL. The drainer
/// reconstructs a `Bound<PyAny>` from the pointer and decrefs when done.
struct SubscriberNotification {
    /// Raw Python object pointer with incremented reference count.
    /// Reconstruct via `Bound::from_borrowed_ptr(py, ptr)` in the drainer.
    callback_ptr: *mut ffi::PyObject,
    pool_id: u64,
}

// Safety: `*mut ffi::PyObject` is `Send` (it's a pointer). The Python object
// is protected by the incremented refcount done at push time.
unsafe impl Send for SubscriberNotification {}

impl Drop for SubscriberNotification {
    fn drop(&mut self) {
        // Decrement refcount when the notification is dropped without being
        // processed (e.g. when the queue drops the oldest entry on overflow).
        // This is safe because the refcount was incremented at push time.
        unsafe {
            ffi::Py_DECREF(self.callback_ptr);
        }
    }
}

/// Shared state for the subscriber notify queue.
struct SubscriberQueueState {
    /// Unbounded, lock-free queue shared with the drainer thread.
    queue: SegQueue<SubscriberNotification>,
    /// Set to `true` to signal the drainer thread to shut down.
    shutdown: AtomicBool,
}

/// Global reference to the subscriber queue state, set during
/// [`init_subscriber_drainer`].
static SUBSCRIBER_QUEUE_STATE: OnceLock<Arc<SubscriberQueueState>> = OnceLock::new();

/// Initialize the subscriber drainer thread.
///
/// Called once during module init. Spawns a dedicated OS thread that
/// periodically drains the subscriber notify queue and forwards
/// notifications to Python callbacks via one `Python::attach` per flush.
///
/// # Panics
///
/// Panics if the OS thread can't be spawned.
pub(crate) fn init_subscriber_drainer() {
    SUBSCRIBER_QUEUE_STATE.get_or_init(|| {
        let state = Arc::new(SubscriberQueueState {
            queue: SegQueue::new(),
            shutdown: AtomicBool::new(false),
        });
        let drainer_state = Arc::clone(&state);
        #[expect(clippy::expect_used)] // thread spawn fails only under resource exhaustion
        {
            thread::Builder::new()
                .name("subscriber-drainer".into())
                .spawn(move || subscriber_drainer_loop(drainer_state))
                .expect("spawn subscriber-drainer thread");
        }
        state
    });
}

/// Signal the subscriber drainer to shut down and flush remaining
/// notifications.
///
/// Idempotent. Should be called before interpreter finalization.
#[pyfunction]
pub(crate) fn shutdown_subscriber_drainer() {
    if let Some(state) = SUBSCRIBER_QUEUE_STATE.get() {
        state.shutdown.store(true, Ordering::Release);
    }
}

/// The subscriber drainer thread main loop.
///
/// Collects pending notifications from the queue, coalesces to
/// latest-per-pool per callback, and forwards them to Python via one
/// `Python::attach` per flush.
#[expect(clippy::needless_pass_by_value)] // owned Arc moved into drainer thread closure
fn subscriber_drainer_loop(state: Arc<SubscriberQueueState>) {
    let mut batch: Vec<SubscriberNotification> = Vec::with_capacity(SUBSCRIBER_BATCH_SIZE);
    let mut last_flush = Instant::now();

    loop {
        // Drain as many notifications as available (up to SUBSCRIBER_BATCH_SIZE).
        while batch.len() < SUBSCRIBER_BATCH_SIZE {
            match state.queue.pop() {
                Some(notification) => batch.push(notification),
                None => break, // queue empty
            }
        }

        let elapsed = last_flush.elapsed();
        let should_flush = !batch.is_empty()
            && (batch.len() >= SUBSCRIBER_BATCH_SIZE || elapsed >= SUBSCRIBER_FLUSH_INTERVAL);

        if should_flush {
            flush_notification_batch(&batch);
            batch.clear();
            last_flush = Instant::now();
        }

        // Check shutdown.
        if state.shutdown.load(Ordering::Acquire) {
            // Flush remaining.
            if !batch.is_empty() {
                flush_notification_batch(&batch);
                batch.clear();
            }
            while let Some(notification) = state.queue.pop() {
                batch.push(notification);
            }
            if !batch.is_empty() {
                flush_notification_batch(&batch);
            }
            break;
        }

        thread::sleep(Duration::from_millis(10));
    }
}

/// Flush a batch of notifications to Python callbacks via one
/// `Python::attach`.
///
/// Coalesces to latest-per-pool per callback: if the same callback appears
/// multiple times for the same `pool_id`, only the latest entry is forwarded.
/// This matches the backrun bot's need for current state, not every transition.
fn flush_notification_batch(notifications: &[SubscriberNotification]) {
    if notifications.is_empty() {
        return;
    }

    // Coalesce: for each (callback_ptr, pool_id) pair, keep only the
    // latest occurrence. Iterate in reverse to capture the latest entry
    // for each pair.
    let coalesced: Vec<&SubscriberNotification> = {
        let mut result = Vec::with_capacity(notifications.len());
        for notification in notifications.iter().rev() {
            let already_present = result.iter().any(|existing: &&SubscriberNotification| {
                existing.callback_ptr == notification.callback_ptr
                    && existing.pool_id == notification.pool_id
            });
            if !already_present {
                result.push(notification);
            }
        }
        result.reverse();
        result
    };

    Python::attach(|py| {
        for notification in &coalesced {
            // Reconstruct a `Bound<PyAny>` from the raw pointer. The refcount
            // was incremented at push time; `SubscriberNotification::drop`
            // decrefs it. `coalesced` holds references to the original
            // notifications, so the `Bound` we create here is a borrowed
            // reference (from_borrowed_ptr does NOT incref).
            // Safety: the refcount was incremented at push time and the
            // pointer is valid for the lifetime of the notification.
            let callback: Bound<'_, PyAny> =
                unsafe { pyo3::Bound::from_borrowed_ptr(py, notification.callback_ptr) };
            if let Err(err) = callback.call1((notification.pool_id,)) {
                // Log via tracing (not log::) to avoid re-entering the log
                // drainer on the GIL-holding drainer thread.
                tracing::warn!(
                    pool_id = notification.pool_id,
                    error = %err,
                    "PySubscriberAdapter: callback raised during batched notify"
                );
            }
        }
    });
}

// --- PySubscriberAdapter (now queue-backed) ---

/// A `PoolStateSubscriber` backed by a Python callback. No longer acquires
/// the GIL per call — pushes to the global subscriber notify queue instead.
///
/// Constructed from a `Py<PyAny>` (a Python callable OR a `Subscriber`-shaped
/// object exposing `notify`). On `on_pool_state_updated(pool_id)`, pushes
/// `(callback, pool_id)` onto the shared bounded queue. A dedicated drainer
/// thread batches and forwards to Python.
///
/// The strong `Arc<Self>` is held by a [`PySubscription`] handle (Python owns
/// the lifetime); `LogDispatcher` holds only a `Weak<dyn PoolStateSubscriber>`.
/// Dropping the handle (or `.unsubscribe()`) drops the Arc → the Weak goes
/// dead → `LogDispatcher::notify` silently skips it.
pub struct PySubscriberAdapter {
    callback: Py<PyAny>,
}

impl PySubscriberAdapter {
    /// Construct from a Python callback (callable OR `Subscriber`-shaped object).
    #[must_use]
    pub fn new(callback: Py<PyAny>) -> Self {
        Self { callback }
    }
}

impl PoolStateSubscriber for PySubscriberAdapter {
    fn on_pool_state_updated(&self, pool_id: u64) {
        // Push onto the global subscriber notify queue (GIL-free).
        // The drainer thread will batch and forward to Python.
        if let Some(state) = SUBSCRIBER_QUEUE_STATE.get() {
            // Increment the Python object's refcount before sending it
            // to the GIL-free queue. The drainer (or Drop) decrefs it.
            unsafe {
                ffi::Py_INCREF(self.callback.as_ptr());
            }
            let notification = SubscriberNotification {
                callback_ptr: self.callback.as_ptr(),
                pool_id,
            };
            state.queue.push(notification);
        }
    }
}

// --- PySubscription handle ---

/// A handle keeping a registered [`PySubscriberAdapter`] alive.
///
/// Returned by [`register_subscriber`]; Python owns the lifetime. Drop the
/// handle (or call [`unsubscribe`](Self::unsubscribe)) → the strong `Arc` drops
/// → `LogDispatcher`'s `Weak` goes dead → `notify` silently skips the adapter.
/// This mirrors `PublisherMixin.unsubscribe` + `EngineSubscriber`'s
/// engine-lifetime-anchored `Arc`.
///
/// Not inherently tied to one `pool_id` (a future `subscribe_all` form could
/// register the same adapter against many pools); `unsubscribe` here is a
/// no-op-drop that simply releases the strong ref. To re-add, call
/// `register_subscriber` again (a fresh `Weak` registers + a fresh handle
/// returns).
#[pyclass(name = "PySubscription", module = "degenbot._ffi.subscriber")]
pub struct PySubscription {
    /// The strong ref keeping the adapter's `Weak` alive in `LogDispatcher`.
    /// `subscribe_pool_state_change` registered only a `Weak`; this anchor
    /// holds the `Arc` alive for as long as Python holds the handle. Setting
    /// it to `None` (`unsubscribe`) drops the `Arc` → the `Weak` goes dead →
    /// `LogDispatcher::notify` silently skips the adapter.
    strong: Option<Arc<dyn PoolStateSubscriber>>,
}

impl PySubscription {
    /// Build the adapter + register its `Weak` against `pool_id` on `Bot`,
    /// returning the strong-anchor handle. The `Arc` is kept alive via the
    /// returned handle's `strong` field (the registration only hands `Bot` a
    /// `Weak`).
    fn register(bot: &PyBot, pool_id: u64, callback: Py<PyAny>) -> Self {
        let strong: Arc<dyn PoolStateSubscriber> = Arc::new(PySubscriberAdapter::new(callback));
        let weak: Weak<dyn PoolStateSubscriber> = Arc::downgrade(&strong);
        bot.bot_arc().subscribe_pool_state_change(pool_id, weak);
        Self {
            strong: Some(strong),
        }
    }
}

#[pymethods]
impl PySubscription {
    /// Release the strong anchor — the registered `Weak` goes dead, so
    /// `LogDispatcher::notify` silently skips this subscriber on subsequent
    /// dispatches. Idempotent.
    fn unsubscribe(&mut self) {
        self.strong = None;
    }
}

// --- Registration pyfunction ---

/// Register a Python callback as a `PoolStateSubscriber` for `pool_id` (ZBD4MS).
///
/// The callback receives `pool_id: int` (batched via the subscriber drainer,
/// not per-log). Notifications fire in registration order within a batch; a
/// dropped (GC'd) callback is silently skipped (mirrors a dropped `Weak`
/// subscriber).
///
/// Returns a `PySubscription` handle — hold it for as long as the subscriber
/// should stay registered. Dropping the handle (or calling `.unsubscribe()`)
/// unregisters.
///
/// Two call shapes are accepted:
///  - a callable:           `register_subscriber(bot, pool_id, lambda pid: ...)`
///  - a `Subscriber`-shaped object exposing `__call__` / `notify`
///    (`register_subscriber` invokes it as `callback(pool_id)`).
///
/// Args:
///     `bot`: The `PyBot` owning the `Bot` whose `LogDispatcher` registers.
///     `pool_id`: The pool ID to subscribe to (from `register_v2_pool` / etc.).
///     `callback`: A Python callable invoked as `callback(pool_id)` on notify.
///
/// # Errors
/// `PyRuntimeError` if the callback isn't callable.
#[pyfunction]
#[pyo3(signature = (bot, pool_id, callback))]
pub(crate) fn register_subscriber(
    bot: &PyBot,
    pool_id: u64,
    callback: Bound<'_, PyAny>,
) -> PyResult<PySubscription> {
    if !callback.is_callable() {
        return Err(pyo3::exceptions::PyRuntimeError::new_err(
            "register_subscriber: callback must be callable (a function or a \
             Subscriber object exposing __call__)",
        ));
    }
    Ok(PySubscription::register(bot, pool_id, callback.unbind()))
}

// --- Module registration ---

/// Register the pub/sub seam (feature = "bot"): the `register_subscriber`
/// pyfunction + the `PySubscription` handle class + subscriber drainer
/// lifecycle. Mirrors `add_dex_identity` / `add_deployments`.
pub(crate) fn add_subscriber_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Initialize the subscriber drainer thread.
    init_subscriber_drainer();

    // Register the shutdown function.
    m.add_function(wrap_pyfunction!(shutdown_subscriber_drainer, m)?)?;

    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.subscriber")?;
    submod.add_function(wrap_pyfunction!(register_subscriber, &submod)?)?;
    submod.add_class::<PySubscription>()?;
    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.subscriber", &submod)?;
    Ok(())
}
