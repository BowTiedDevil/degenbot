//! `PyO3` bindings for the backrun pipeline crates (task NYVL2F slice).
//!
//! - `PyBackrunFeed` — owns the `degenbot-rpc` feed pump; `drain()` returns
//!   plain dicts (lossless fields), `status()` the counters snapshot.
//! - `classify_target` — the `degenbot-decoders` target classifier bound as a
//!   free function over `(to, calldata)`.
//! - `send_bundle_request` — one-shot relay round-trip through the
//!   submission crate's golden-tested wire leaf.

use std::time::Duration;

use alloy::primitives::Address;

use crate::prelude::*;
use alloy::hex::FromHex;
use degenbot_rpc::backrun_feed::{BackrunFeed, BackrunFeedConfig, BackrunFeedEvent};
use pyo3::exceptions::PyValueError;
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
#[pyclass(module = "degenbot._ffi")]
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

/// Classify a pending call `(to, calldata)`. Returns one of:
/// `{"kind": "swap", "legs": [...]}`, `{"kind": "inert"}` or
/// `{"kind": "opaque", "reason": "..."}`.
#[pyfunction]
fn classify_target(py: Python<'_>, to: &str, calldata_hex: &str) -> PyResult<Py<PyAny>> {
    let to_addr = Address::from_hex(to.trim_start_matches("0x"))
        .map_err(|e| PyValueError::new_err(format!("bad to: {e}")))?;
    let cd = alloy::hex::decode(calldata_hex.trim_start_matches("0x"))
        .map_err(|e| PyValueError::new_err(format!("bad calldata: {e}")))?;
    let reg = degenbot_decoders::target_classifier::RouterRegistry::mainnet();
    let out = degenbot_decoders::target_classifier::classify(to_addr, &cd, &reg);
    let dict = match out {
        degenbot_decoders::target_classifier::TargetClass::Inert => {
            let d = PyDict::new(py);
            d.set_item("kind", "inert")?;
            d.into_any().unbind()
        }
        degenbot_decoders::target_classifier::TargetClass::Opaque(r) => {
            let d = PyDict::new(py);
            d.set_item("kind", "opaque")?;
            d.set_item("reason", format!("{r:?}"))?;
            d.into_any().unbind()
        }
        degenbot_decoders::target_classifier::TargetClass::Swap(legs) => {
            let mut out = Vec::with_capacity(legs.len());
            for l in legs {
                let pool: Py<PyAny> = match l.pool {
                    Some(a) => a.to_string().into_pyobject(py)?.unbind().into_any(),
                    None => py.None().into_any(),
                };
                let tin: Py<PyAny> = match l.token_in {
                    Some(a) => a.to_string().into_pyobject(py)?.unbind().into_any(),
                    None => py.None().into_any(),
                };
                let tout: Py<PyAny> = match l.token_out {
                    Some(a) => a.to_string().into_pyobject(py)?.unbind().into_any(),
                    None => py.None().into_any(),
                };
                let ld = PyDict::new(py);
                ld.set_item("protocol", format!("{:?}", l.protocol))?;
                ld.set_item("pool", pool)?;
                ld.set_item("tokenIn", tin)?;
                ld.set_item("tokenOut", tout)?;
                ld.set_item("value", l.value.to_string())?;
                ld.set_item("amountIn", l.amount_in.map(|v| v.to_string()))?;
                ld.set_item("amountOutMin", l.amount_out_min.map(|v| v.to_string()))?;
                ld.set_item("amountOut", l.amount_out.map(|v| v.to_string()))?;
                ld.set_item("amountInMax", l.amount_in_max.map(|v| v.to_string()))?;
                ld.set_item("hops", l.hops)?;
                out.push(ld.into_any().unbind());
            }
            let d = PyDict::new(py);
            d.set_item("kind", "swap")?;
            d.set_item("legs", out)?;
            d.into_any().unbind()
        }
    };
    Ok(dict)
}

/// Register the `degenbot._ffi.backrun` submodule (feed class + classifier).
///
/// # Errors
///
/// Propagates any `PyResult` failure from the module add calls.
pub fn add_backrun_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.backrun")?;
    submod.add_class::<PyBackrunFeed>()?;
    submod.add_function(wrap_pyfunction!(classify_target, &submod)?)?;
    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.backrun", &submod)?;
    Ok(())
}
