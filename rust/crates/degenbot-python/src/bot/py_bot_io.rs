//! `PyBotIo` — the Rust `#[pyclass]` I/O façade that pool builders receive in
//! place of the Python `SyncPoolIO` adapter (ADR-005 slice 14a).
//!
//! ## Why a Rust pyclass at all?
//!
//! Per slice 14, `Bot.build_pool`'s I/O choreography moves into Rust. The
//! first step is a single `#[pyclass]` `PyBotIo` that the builders receive as
//! their `io` parameter (replacing `io = SyncPoolIO(self.provider)`). `PyBotIo`
//! exposes the 7-method [`PoolIO`] surface defined by the Python protocol; for
//! slice 14a it does so by **delegating** to the held Python `provider` — the
//! same `ProviderAdapter` the `Bot` was constructed with (so web3-backed,
//! alloy-backed, and offline-backed providers all work without a Rust rewrite
//! of each).
//!
//! ## Why delegation now (and not a pure Rust provider)?
//!
//! `ProviderAdapter` is a Python façade over three backends
//! (`_Web3Adapter` / `_AlloyAdapter` / `_OfflineAdapter`). Only the alloy
//! backend is already in Rust (`crate::rpc::provider::PyAlloyProvider`); the
//! web3 + offline backends aren't. Delegating lets `PyBotIo` serve every
//! backend today, and positions 14b/14c to move the delegation *into* Rust for
//! the alloy backend (swap the held `Py<PyAny>` provider for a direct
//! `AlloyProvider` field + native method bodies, no Python round-trip).
//!
//! ## `PoolIO` surface mirrored exactly
//!
//! `get_block_number`, `get_block`, `get_block_timestamp`, `get_code`,
//! `get_balance`, `call`, `call_raw` — method-by-method mirrors of
//! `degenbot/builders/pool_io.py::SyncPoolIO`, including its calling
//! convention. `chain_id` stays a builder `build()` kwarg (mirrors `pool_io.py`'s
//! deliberate exclusion of `chain_id` as a config value, not an I/O operation).
//!
//! [PoolIO]: degenbot/builders/pool_io.py

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};

/// The Rust I/O façade for pool builders (ADR-005 slice 14a).
///
/// Construct with an existing `ProviderAdapter` (the `Bot.provider`):
/// ```python
/// io = PyBotIo(provider=bot.provider, db=bot.db)
/// ```
/// then pass `io` where a builder expects an `io: PoolIO`.
///
/// Holds the optional `db` (`DatabaseSessionManager`) handle too; 14a stores
/// it so the `PyBotIo` surface mirrors `BuilderContext`'s dependencies, but
/// does not yet route DB queries through it (the DB-query choreography ports
/// in slice 14c).
#[pyclass(name = "PyBotIo")]
pub struct PyBotIo {
    provider: Py<PyAny>,
    db: Option<Py<PyAny>>,
}

#[pymethods]
impl PyBotIo {
    /// Construct the I/O façade over a Python `ProviderAdapter` (+ optional DB).
    ///
    /// `provider` is the `ProviderAdapter` the `Bot` was constructed with —
    /// any backend (web3 / alloy / offline) works, because `PyBotIo` delegates
    /// to its 7 `PoolIO` methods rather than re-implementing them.
    ///
    /// `db`, when provided, is the `DatabaseSessionManager` handle; it's stored
    /// so the `PyBotIo` surface mirrors `BuilderContext`, and accessed via the
    /// [`db` getter][Self::db]. Slice 14c routes DB queries through here.
    #[new]
    #[pyo3(signature = (provider, db=None))]
    fn new(provider: Py<PyAny>, db: Option<Py<PyAny>>) -> Self {
        Self { provider, db }
    }

    /// The held `ProviderAdapter` (round-trips for tests + introspection).
    #[getter]
    fn provider(&self, py: Python<'_>) -> Py<PyAny> {
        self.provider.clone_ref(py)
    }

    /// The held `DatabaseSessionManager`, if any (None when the `Bot` has no DB).
    #[getter]
    fn db(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        self.db.as_ref().map(|h| h.clone_ref(py))
    }

    /// Return the current block number (delegates to `provider.get_block_number()`).
    fn get_block_number(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.call_kw(py, "get_block_number", &[])
    }

    /// Return block data for the given identifier (delegates to
    /// `provider.get_block(block_identifier)` — positional, mirrors `SyncPoolIO`).
    fn get_block(
        &self,
        py: Python<'_>,
        block_identifier: &Bound<'_, PyAny>,
    ) -> PyResult<Py<PyAny>> {
        // SyncPoolIO forwards positionally: provider.get_block(block_identifier)
        let method = self.provider.bind(py).getattr("get_block")?;
        let args = PyTuple::new(py, [block_identifier])?;
        Ok(method.call(args, None)?.unbind())
    }

    /// Return the timestamp for the given block (delegates to
    /// `provider.get_block_timestamp(block=block)` — kw, mirrors `SyncPoolIO`).
    #[pyo3(signature = (block=None))]
    fn get_block_timestamp(
        &self,
        py: Python<'_>,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        self.call_kw(py, "get_block_timestamp", &[("block", block)])
    }

    /// Return the code at the given address (delegates to
    /// `provider.get_code(address, block=block)` — address positional, block
    /// kw, mirrors `SyncPoolIO` exactly).
    #[pyo3(signature = (address, block=None))]
    fn get_code(
        &self,
        py: Python<'_>,
        address: &Bound<'_, PyAny>,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let method = self.provider.bind(py).getattr("get_code")?;
        let kwargs = build_block_kw(py, block)?;
        let args = PyTuple::new(py, [address])?;
        Ok(method.call(args, kwargs.as_ref())?.unbind())
    }

    /// Return the ETH balance at the given address (delegates to
    /// `provider.get_balance(address, block=block)` — mirrors `SyncPoolIO`).
    #[pyo3(signature = (address, block=None))]
    fn get_balance(
        &self,
        py: Python<'_>,
        address: &Bound<'_, PyAny>,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let method = self.provider.bind(py).getattr("get_balance")?;
        let kwargs = build_block_kw(py, block)?;
        let args = PyTuple::new(py, [address])?;
        Ok(method.call(args, kwargs.as_ref())?.unbind())
    }

    /// Perform an `eth_call` and return the result (delegates to
    /// `provider.call(to=to, data=data, block=block)` — kw-only, mirrors
    /// `SyncPoolIO` exactly).
    ///
    /// `OfflineProvider.call(*, to, data, block_number)` has a keyword-only
    /// signature, and test doubles (`MagicMock(side_effect=mock_call)` where
    /// `mock_call(*, to, data, block=None)`) are designed against `SyncPoolIO`'s
    /// kw-call shape — both work unchanged because the forward is kw-only.
    #[pyo3(signature = (to, data, block=None))]
    fn call(
        &self,
        py: Python<'_>,
        to: &Bound<'_, PyAny>,
        data: &Bound<'_, PyAny>,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        self.call_kw(
            py,
            "call",
            &[("to", Some(to)), ("data", Some(data)), ("block", block)],
        )
    }

    /// Perform a raw `eth_call` and return the result (delegates to
    /// `provider.call_raw(tx, block=block)` — tx positional, block kw, mirrors
    /// `SyncPoolIO`).
    ///
    /// `tx` is a web3 `TxParams` dict; `PyBotIo` forwards it verbatim (the
    /// builder assembles it; this is a pass-through, not a tx-builder).
    #[pyo3(signature = (tx, block=None))]
    fn call_raw(
        &self,
        py: Python<'_>,
        tx: &Bound<'_, PyAny>,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let method = self.provider.bind(py).getattr("call_raw")?;
        let kwargs = build_block_kw(py, block)?;
        let args = PyTuple::new(py, [tx])?;
        Ok(method.call(args, kwargs.as_ref())?.unbind())
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        let has_db = self.db.is_some();
        let provider_repr = self.provider.bind(py).repr()?.to_str()?.to_string();
        Ok(format!("PyBotIo(provider={provider_repr}, db={has_db})"))
    }
}

impl PyBotIo {
    /// Call `method_name` on the held provider with the given keyword arguments.
    ///
    /// Single delegation seam for the kw-only forward shape (`call`,
    /// `get_block_timestamp`). Slice 14b/14c's "move delegation into Rust"
    /// strategy swaps the body per-method without touching the `#[pymethods]`
    /// signature.
    fn call_kw(
        &self,
        py: Python<'_>,
        method_name: &str,
        kwargs: &[(&str, Option<&Bound<'_, PyAny>>)],
    ) -> PyResult<Py<PyAny>> {
        let method = self.provider.bind(py).getattr(method_name)?;
        let kw_dict = PyDict::new(py);
        for (name, value) in kwargs {
            if let Some(v) = value {
                kw_dict.set_item(*name, v)?;
            }
        }
        Ok(method.call((), Some(&kw_dict))?.unbind())
    }
}

/// Build `Some(dict{block: …})` when `block` is present, `None` otherwise —
/// mirrors `SyncPoolIO`'s conditional kwargs (it omits `block=` when None).
fn build_block_kw<'py>(
    py: Python<'py>,
    block: Option<&Bound<'_, PyAny>>,
) -> PyResult<Option<Bound<'py, PyDict>>> {
    match block {
        Some(b) => {
            let d = PyDict::new(py);
            d.set_item("block", b)?;
            Ok(Some(d))
        }
        None => Ok(None),
    }
}
