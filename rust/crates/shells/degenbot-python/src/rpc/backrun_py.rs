//! `PyO3` bindings for the backrun pipeline crates (task NYVL2F slice).
//!
//! - `PyBackrunFeed` — owns the `degenbot-rpc` feed pump; `drain()` returns
//!   plain dicts (lossless fields), `status()` the counters snapshot.
//! - `send_bundle_request` — one-shot relay round-trip through the
//!   submission crate's golden-tested wire leaf.

use std::time::Duration;

use crate::prelude::*;
use degenbot_rpc::backrun_feed::{BackrunFeed, BackrunFeedConfig, BackrunFeedEvent};
use pyo3::types::PyDict;

fn ev_to_dict(py: Python<'_>, ev: &BackrunFeedEvent) -> PyResult<Py<PyAny>> {
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

/// Live `MEVBlocker` searcher feed handle (RSUB-2).
///
/// The struct keeps the Rust-internal `Py` prefix; the Python-facing name is
/// the ADR-032 clean form (`BackrunFeed`) — the naming gate extends no
/// grandfather list.
#[pyclass(name = "BackrunFeed", module = "degenbot._ffi")]
pub struct PyBackrunFeed {
    inner: BackrunFeed,
}

#[pymethods]
impl PyBackrunFeed {
    /// Spawn the feed pump. Watchdog/ring/backoff are config-able; `0`
    /// watchdog selects the production default.
    #[new]
    #[pyo3(signature = (url = None, watchdog_secs = 0u64, ring_capacity = 0usize))]
    fn new(url: Option<String>, watchdog_secs: u64, ring_capacity: usize) -> Self {
        let mut cfg = BackrunFeedConfig::for_mainnet();
        if let Some(u) = url {
            cfg.url = u;
        }
        if watchdog_secs > 0 {
            cfg.watchdog = Duration::from_secs(watchdog_secs);
        }
        if ring_capacity > 0 {
            cfg.ring_capacity = ring_capacity;
        }
        Self {
            inner: BackrunFeed::spawn(cfg),
        }
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

/// Register the `degenbot._ffi.backrun` submodule (the feed pump).
///
/// # Errors
///
/// Propagates any `PyResult` failure from the module add calls.
pub fn add_backrun_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.backrun")?;
    submod.add_class::<PyBackrunFeed>()?;
    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.backrun", &submod)?;
    Ok(())
}
