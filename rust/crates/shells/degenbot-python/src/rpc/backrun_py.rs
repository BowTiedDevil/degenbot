//! `PyO3` bindings for the backrun pipeline crates.
//!
//! - `PyBackrunFeed` — owns the `degenbot-rpc` eager feed adapter; `drain()`
//!   returns plain dicts (lossless fields), `status()` the counters snapshot.
//! - `send_bundle_request` — one-shot relay round-trip through the
//!   submission crate's golden-tested wire leaf.

use std::time::Duration;

use crate::prelude::*;
use degenbot_eventhub::PendingTx;
use degenbot_rpc::backrun_feed::{BackrunFeedConfig, EagerBackrunFeed, RingCapacityError};
use pyo3::types::PyDict;

fn ev_to_dict(py: Python<'_>, ev: &PendingTx) -> PyResult<Py<PyAny>> {
    let to: Py<PyAny> = match ev.to {
        Some(a) => a.to_string().into_pyobject(py)?.unbind().into_any(),
        None => py.None().into_any(),
    };
    let d = PyDict::new(py);
    d.set_item("chainId", ev.chain_id)?;
    d.set_item("from", ev.from.to_string())?;
    d.set_item("to", to)?;
    d.set_item("value", ev.value.to_string())?;
    d.set_item(
        "data",
        format!("0x{}", alloy::hex::encode(ev.data.as_ref())),
    )?;
    d.set_item("gas", ev.gas)?;
    d.set_item("maxFeePerGas", ev.max_fee_per_gas)?;
    d.set_item("maxPriorityFeePerGas", ev.max_priority_fee_per_gas)?;
    d.set_item("nonce", ev.nonce)?;
    d.set_item("hash", ev.hash.to_string())?;
    d.set_item("accessList", ev.access_list.to_string())?;
    d.set_item("type", ev.tx_type)?;
    d.set_item("receivedUnixMs", ev.received_unix_ms)?;
    Ok(d.into_any().unbind())
}

/// Live `MEVBlocker` searcher feed handle.
///
/// The struct keeps the Rust-internal `Py` prefix; the Python-facing name is
/// the ADR-032 clean form (`BackrunFeed`).
#[pyclass(name = "BackrunFeed", module = "degenbot._ffi")]
pub struct PyBackrunFeed {
    inner: EagerBackrunFeed,
}

/// The feed config a Python call names, built without spawning or dialing.
///
/// `ring_capacity` is optional so the two meanings stay distinct: `None`
/// selects the production default, and an explicit `0` is a `ValueError`
/// raised BEFORE any feed exists — never a silent fallback to the default, and
/// never a ring that cannot hold a frame.
///
/// # Errors
///
/// [`pyo3::exceptions::PyValueError`] when `ring_capacity` is `Some(0)`.
fn config_from_python(
    url: Option<String>,
    watchdog_secs: u64,
    ring_capacity: Option<usize>,
) -> PyResult<BackrunFeedConfig> {
    let mut cfg = BackrunFeedConfig::for_mainnet();
    if let Some(url) = url {
        cfg.url = url;
    }
    if watchdog_secs > 0 {
        cfg.watchdog = Duration::from_secs(watchdog_secs);
    }
    cfg.with_ring_capacity(ring_capacity)
        .map_err(|refusal| match refusal {
            RingCapacityError::Zero => pyo3::exceptions::PyValueError::new_err(refusal.to_string()),
        })
}

#[pymethods]
impl PyBackrunFeed {
    /// Spawn the feed pump. Watchdog/ring/backoff are config-able; `0`
    /// watchdog selects the production default, and `ring_capacity=None` does
    /// the same for the ring — an explicit `0` ring is a `ValueError`.
    ///
    /// # Errors
    ///
    /// `ValueError` when `ring_capacity` is `0`. The refusal is decided from
    /// the config alone, before any feed is constructed, so nothing spawns and
    /// nothing dials on the rejected path.
    #[new]
    #[pyo3(signature = (url = None, watchdog_secs = 0u64, ring_capacity = None))]
    fn new(
        url: Option<String>,
        watchdog_secs: u64,
        ring_capacity: Option<usize>,
    ) -> PyResult<Self> {
        let cfg = config_from_python(url, watchdog_secs, ring_capacity)?;
        Ok(Self {
            inner: EagerBackrunFeed::spawn(cfg),
        })
    }

    /// Drain all buffered events (plain dicts; lossless fields).
    fn drain(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        self.inner
            .drain()
            .into_iter()
            .map(|ev| ev_to_dict(py, &ev))
            .collect()
    }

    /// Counters snapshot as a plain dict.
    fn status(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let s = self.inner.status();
        let d = PyDict::new(py);
        d.set_item("connected", s.connected)?;
        d.set_item("accepted", s.accepted)?;
        d.set_item("droppedRing", s.dropped_ring)?;
        d.set_item("rejectedChainId", s.rejected_chain_id)?;
        d.set_item("rejectedParse", s.rejected_parse)?;
        d.set_item("reconnects", s.reconnects)?;
        d.set_item("lastEventUnixMs", s.last_event_unix_ms)?;
        Ok(d.into_any().unbind())
    }

    fn stop(&self) {
        self.inner.stop();
    }
}

/// The `degenbot._ffi.backrun` Python submodule (declarative
/// `#[pymodule]`), carrying the backrun pipeline feed pyclass. The parent
/// module registers the submodule itself and its `sys.modules` entry.
#[pymodule(submodule)]
#[pyo3(module = "degenbot._ffi")]
pub mod backrun {
    #[pymodule_export]
    use super::PyBackrunFeed;
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "the config helper's two capacity outcomes are the assertion"
)]
mod tests {
    use super::*;

    /// The Python boundary's zero ring: an explicit `0` is a `ValueError`
    /// decided from the config alone, so the rejected call never spawns a feed
    /// and never dials.
    #[test]
    fn python_ring_capacity_zero_is_a_value_error() {
        pyo3::Python::attach(|py| {
            let refused =
                config_from_python(None, 0, Some(0)).expect_err("an explicit zero is refused");
            assert!(
                refused.is_instance_of::<pyo3::exceptions::PyValueError>(py),
                "an explicit zero ring is a ValueError: {refused}"
            );
        });
    }

    /// An absent ring capacity selects the production default — no feed is
    /// constructed on either side of the check, and a positive value becomes
    /// the ring's capacity verbatim.
    #[test]
    fn python_ring_capacity_none_uses_the_production_default() {
        let default = config_from_python(None, 0, None).expect("None selects the default");
        assert_eq!(
            default.ring_capacity,
            BackrunFeedConfig::for_mainnet().ring_capacity,
            "an absent capacity keeps the production default"
        );

        let explicit = config_from_python(Some(String::from("ws://example.invalid")), 5, Some(64))
            .expect("a positive capacity is accepted");
        assert_eq!(explicit.ring_capacity.get(), 64);
        assert_eq!(explicit.url, "ws://example.invalid");
        assert_eq!(explicit.watchdog, Duration::from_secs(5));
    }
}
