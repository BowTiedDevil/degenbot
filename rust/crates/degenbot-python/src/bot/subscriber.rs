//! Python↔Rust pub/sub bridge: a `PoolStateSubscriber` adapter that forwards
//! Rust `BotState` mutation notifications into a Python callback.
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
//! Mirrors [`EngineSubscriber`]'s adapter shape: a shared `Arc<PySubscriberAdapter>`
//! registered as a `Weak<dyn PoolStateSubscriber>` so a dropped subscriber is
//! silently skipped by `LogDispatcher::notify`'s `Weak::upgrade` (no leak, no
//! panic). The difference is the lifetime anchor: the engine adapter's strong
//! `Arc` lives on the shared `ArbitrageEngine`; THIS adapter's strong `Arc` lives
//! on a returned [`PySubscription`] handle Python holds — drop the handle
//! (or call `.unsubscribe()`) → the Arc drops → the registered `Weak` goes dead
//! (explicit unsubscribe, mirroring `PublisherMixin.unsubscribe`).
//!
//! # GIL discipline
//!
//! `LogDispatcher::notify` fires from the pump's async task (a `tokio::spawn`
//! GIL-free context) per decoded log — so `on_pool_state_updated` runs WITHOUT
//! the GIL held. The adapter re-acquires it via `Python::attach` before
//! invoking the Python callback. When `dispatch_log` is driven synchronously
//! from Python (the main GIL thread), `Python::attach` re-enters the already-held
//! GIL via `PyGILState_Ensure` (re-entrant safe). The pump's `dispatch_log`
//! write guard is released before notify (the `LogDispatcher` lock-order
//! invariant), so the GIL acquire here never nests against a `BotState` write.
//!
//! # §4.2 parity
//!
//! The oracle is `src/degenbot/types/concrete.py::PublisherMixin._notify_subscribers`:
//! `for subscriber in self._subscribers: subscriber.notify(publisher=self,
//! message=message)`. The Rust fan-out order is identical — `LogDispatcher::notify`
//! iterates the `Vec<Weak<dyn PoolStateSubscriber>>` in registration order,
//! upgrading each. Pinned by `tests/test_pubsub_seam_parity.py` (a Rust-backed
//! `PyBot` + Python callbacks receive `pool_id` in the same registration order
//! `PublisherMixin` delivers, and the same skip-on-drop fan-out).
//!
//! The Rust notify carries only `pool_id` (not the full Python `PoolStateUpdated`
//! state object) — reconstructing the latter requires reading `BotState` + building
//! the Python state wrapper, which is the Python cutover (retiring `PublisherMixin`
//! on pool classes), a deferred sibling task. The adapter surfaces the
//! `pool_id` to Python; the cutover task will widen the payload.

use std::sync::{Arc, Weak};

use degenbot_bot::bot_core::log_dispatcher::PoolStateSubscriber;

use crate::bot::PyBot;
use crate::prelude::*;

/// A `PoolStateSubscriber` backed by a Python callback.
///
/// Constructed from a `Py<PyAny>` (a Python callable OR a `Subscriber`-shaped
/// object exposing `notify`). On `on_pool_state_updated(pool_id)`, re-acquires
/// the GIL and invokes the callback with `pool_id` as the sole argument. Errors
/// raised by the Python callback are logged (via `pyo3-log`) and swallowed — a
/// subscriber's failure must never break the pump's decode→apply→notify spine
/// (mirrors `EngineSubscriber`'s no-panic contract; the trait returns `()`).
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
        // Fires from the pump's async task (GIL-free) → re-acquire the GIL.
        // `attach` from a non-Python thread attaches it as needed (the
        // interpreter is initialized via the `auto-initialize` feature); on
        // the main GIL thread it re-enters via `PyGILState_Ensure`.
        Python::attach(|py| {
            // A Python-side exception must not abort the fan-out — log + swallow.
            if let Err(err) = self.callback.call1(py, (pool_id,)) {
                log::warn!(
                    "PySubscriberAdapter: Python callback raised on pool_id={pool_id} notify: {err}"
                );
            }
        });
    }
}

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

/// Register a Python callback as a `PoolStateSubscriber` for `pool_id` (ZBD4MS).
///
/// The callback receives `pool_id: int` each time `Bot::dispatch_log` applies a
/// decoded event to `pool_id` in the shared `BotState` (or `notify_pool_state_updated`
/// fires after a reorg restore) — the SAME `LogDispatcher` path the engine
/// adapter uses. Notifications fire in registration order; a dropped (GC'd)
/// callback is silently skipped (mirrors a dropped `Weak` subscriber).
///
/// Returns a `PySubscription` handle — hold it for as long as the subscriber
/// should stay registered. Dropping the handle (or calling `.unsubscribe()`)
/// unregisters: the strong `Arc` drops, `LogDispatcher`'s `Weak` goes dead,
/// and subsequent `notify` calls silently skip this subscriber.
///
/// Two call shapes are accepted:
///  - a callable:           `register_subscriber(bot, pool_id, lambda pid: ...)`
///  - a `Subscriber`-shaped object exposing `__call__` / `notify`
///    (`register_subscriber` invokes it as `callback(pool_id)`; the full
///    `PublisherMixin` `notify(publisher, message)` payload is rebuilt in the
///    cutover task).
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

/// Register the pub/sub seam (feature = "bot"): the `register_subscriber`
/// pyfunction + the `PySubscription` handle class. Mirrors `add_dex_identity` /
/// `add_deployments`.
pub(crate) fn add_subscriber_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
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
