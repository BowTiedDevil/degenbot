//! `PyO3` seam over the `degenbot-fork` core crate (ADR-005 three-layer).
//!
//! [`PyAnvilFork`] wraps the pure-Rust [`degenbot_fork::AnvilFork`]:
//! lifecycle (subprocess spawn + IPC `DynProvider` connect) + the 12 anvil
//! dev-RPC methods. The Python-visible class name is `AnvilFork`
//! (registered as `degenbot_rs.AnvilFork`).
//!
//! ## Layering
//!
//! - **Core** (`degenbot-fork`): subprocess handle + erased alloy
//!   `DynProvider` + `AnvilApi` callers. Zero `pyo3`.
//! - **`PyO3` wrapper** (this module): `#[pyclass]`/`#[pyfunction]` only —
//!   arg extract → GIL release (`py.detach`) → core call (`get_runtime()
//!   ::block_on(async move { ... })`) → result wrap. Zero business logic.
//!
//! ## Sync-only surface (FF3 decision)
//!
//! The 12 dev methods are **sync** at the `PyO3` boundary, mirroring the
//! `PyAlloyProvider` precedent (its async counterpart is a separate
//! `PyAsyncAlloyProvider` pyclass over the same `Arc<AlloyProvider>`).
//! An async twin can be added later (FF4-era) along the same shape if the
//! cockpit needs it — the underlying tokio runtime is already shared.
//!
//! ## `provider` property (deferred to FF4)
//!
//! The legacy Python `AnvilFork` exposed `self.w3`/`self.async_w3()` (a
//! Web3 instance) constructed over the spawned anvil process's URL. The
//! Rust direction replaces that with `AlloyProvider` constructed from
//! [`PyAnvilFork::ipc_path`]. FF4 (`WXRNHH`) — the Python companion-shell
//! rewrite — owns the decision of whether `AnvilFork.provider` becomes:
//!
//! 1. a re-constructed `AlloyProvider` over `ipc_path` (the same path the
//!    core `DynProvider` is already connected over — IPC supports
//!    multiple connection handles), or
//! 2. a thin wrapper around the core `DynProvider` itself.
//!
//! Either way the FF4 shell already has everything it needs: `ipc_path`.
//!
//! ## Override queues (deferred to FF4)
//!
//! The legacy `AnvilFork.__init__` accepted `balance_overrides` /
//! `code_overrides` / `nonce_overrides` / `storage_overrides` kwargs +
//! applied them post-spawn. FF4's shell can call the registered
//! `set_balance`/`set_code`/`set_nonce`/`set_storage_at` methods directly
//! after construction — that's the same action, just explicit at the
//! companion-shell layer. Deferring keeps FF3 surface minimal + keeps
//! `arg-extraction` out of the borrow-lifetime trap that arises when
//! passing `&Bound<PyAny>` references across the GIL-release boundary
//! into a `Send`-bound closure.

use crate::address_utils::parse_address;
use crate::conversion::alloy::{extract_python_u256, u256_to_py};
use crate::runtime::get_runtime;
use alloy::primitives::{Address, Bytes};
use degenbot_fork::{AnvilFork as CoreAnvilFork, AnvilForkBuilder, ForkError, MiningMode};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyModule};

/// Default mnemonic — matches the legacy Python `AnvilFork.__init__`
/// default (the Brownie/Ganache test mnemonic). The default value
/// referenced in the `#[pyo3(signature)]` annotation expands to `&'static
/// str`, so it must be a `const`, not an inline literal.
const DEFAULT_MNEMONIC: &str =
    "patient rude simple dog close planet oval animal hunt sketch suspect slim";

/// Map a core [`ForkError`] to a `PyErr`.
///
/// `PyRuntimeError` for everything (the legacy `AnvilError` was also a
/// `RuntimeError` subclass). Specific subclasses (`Spawn`, `Connect`,
/// `Rpc`) are reflected in the message text for now; if the Python
/// companion shell gains typed exception subclasses (FF4-era), this mapping
/// is the single place to upgrade them.
#[allow(clippy::needless_pass_by_value)] // `Result::map_err` requires the arg by value.
fn map_fork_err(e: ForkError) -> PyErr {
    PyRuntimeError::new_err(e.to_string())
}

/// Validate + parse a `0x…` address argument → [`Address`].
fn parse_addr(address: &str) -> PyResult<Address> {
    parse_address(address).map_err(|e| PyValueError::new_err(format!("Invalid address: {e}")))
}

/// Rust-owned Anvil fork handle: subprocess + IPC `DynProvider` + dev-RPC
/// surface. Construct via `AnvilFork(**kwargs)`; see `__new__` for the
/// keyword surface.
#[pyclass(name = "AnvilFork", skip_from_py_object, module = "degenbot._ffi.fork")]
pub struct PyAnvilFork {
    /// The Rust core handle. `Arc` so each `#[pymethods]` call can cheaply
    /// clone out before `py.detach` (the underlying `AnvilFork` itself is
    /// not `Clone` — it owns the spawned subprocess).
    fork: std::sync::Arc<CoreAnvilFork>,
}

#[pymethods]
impl PyAnvilFork {
    /// Spawn the `anvil` subprocess + connect the IPC `DynProvider`.
    ///
    /// Mirrors the legacy Python `AnvilFork.__init__` keyword surface for
    /// spawn-time config; the legacy `mining_mode` string literal
    /// (`"auto"` / `"interval"` / `"none"`) maps to the Rust
    /// [`MiningMode`] enum. `mining_interval` only applies when
    /// `mining_mode == "interval"`.
    ///
    /// # Errors
    /// - `ValueError` on invalid `mining_mode` value.
    /// - `RuntimeError` on spawn / IPC / RPC failure.
    ///
    /// Post-spawn state overrides (the legacy `balance_overrides` /
    /// `code_overrides` / `nonce_overrides` / `storage_overrides` kwargs)
    /// are NOT exposed on the constructor — apply them after construction
    /// via the registered `set_balance` / `set_code` / `set_nonce` /
    /// `set_storage_at` methods. See module-level note.
    #[new]
    #[pyo3(signature = (
        *,
        fork_url=None,
        fork_block=None,
        fork_transaction_hash=None,
        mining_mode="auto",
        mining_interval=None,
        storage_caching=true,
        base_fee=None,
        ipc_path=None,
        port=None,
        host="127.0.0.1",
        mnemonic=DEFAULT_MNEMONIC,
        chain_id=None,
        anvil_opts=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        py: Python<'_>,
        fork_url: Option<String>,
        fork_block: Option<u64>,
        fork_transaction_hash: Option<String>,
        mining_mode: &str,
        mining_interval: Option<u64>,
        storage_caching: bool,
        base_fee: Option<u128>,
        ipc_path: Option<String>,
        port: Option<u16>,
        host: &str,
        mnemonic: &str,
        chain_id: Option<u64>,
        anvil_opts: Option<Vec<String>>,
    ) -> PyResult<Self> {
        // Arg extraction (no business logic; the closure below is `Send`,
        // so all borrows must be drained to owned values BEFORE the
        // `py.detach` call).
        let mining_mode = match mining_mode {
            "auto" => MiningMode::Auto,
            "interval" => MiningMode::Interval(mining_interval.ok_or_else(|| {
                PyValueError::new_err("mining_interval required when mining_mode='interval'")
            })?),
            "none" => MiningMode::None,
            other => {
                return Err(PyValueError::new_err(format!(
                    "Invalid mining_mode: {other} (expected 'auto' | 'interval' | 'none')"
                )));
            }
        };

        let mut builder = AnvilForkBuilder::new()
            .mining_mode(mining_mode)
            .storage_caching(storage_caching)
            .host(host)
            .mnemonic(mnemonic);
        if let Some(url) = fork_url {
            builder = builder.fork_url(url);
        }
        if let Some(b) = fork_block {
            builder = builder.fork_block(b);
        }
        if let Some(h) = fork_transaction_hash {
            builder = builder.fork_transaction_hash(h);
        }
        if let Some(fee) = base_fee {
            builder = builder.base_fee(fee);
        }
        if let Some(p) = ipc_path {
            builder = builder.ipc_path(p);
        }
        if let Some(p) = port {
            builder = builder.port(p);
        }
        if let Some(id) = chain_id {
            builder = builder.chain_id(id);
        }
        if let Some(args) = anvil_opts {
            builder = builder.extra_args(args);
        }

        let fork = py
            .detach(|| get_runtime().block_on(async move { builder.try_spawn().await }))
            .map_err(map_fork_err)?;

        Ok(Self {
            fork: std::sync::Arc::new(fork),
        })
    }

    /// Resolved IPC socket path the spawned anvil process is listening on
    /// (the requested `--ipc` arg, or anvil's default `/tmp/anvil.ipc`).
    ///
    /// Use this to construct a secondary `AlloyProvider` against the
    /// same IPC socket (e.g. retry-aware `eth_call` / `get_logs` against
    /// the in-memory fork). The legacy Python `AnvilFork.w3` handle is
    /// replaced by that pattern in FF4.
    #[getter]
    fn ipc_path(&self) -> &str {
        self.fork.ipc_path()
    }

    /// HTTP endpoint of the spawned anvil node (`http://127.0.0.1:PORT`).
    #[getter]
    fn http_url(&self) -> String {
        self.fork.http_url()
    }

    /// WebSocket endpoint of the spawned anvil node (`ws://127.0.0.1:PORT`).
    #[getter]
    fn ws_url(&self) -> String {
        self.fork.ws_url()
    }

    /// TCP port the spawned anvil node listens on.
    #[getter]
    fn port(&self) -> u16 {
        self.fork.port()
    }

    /// `evm_mine` — mine a single block.
    ///
    /// # Errors
    /// `RuntimeError` if the RPC call fails.
    fn mine(&self, py: Python<'_>) -> PyResult<()> {
        let fork = std::sync::Arc::clone(&self.fork);
        py.detach(|| get_runtime().block_on(async move { fork.mine().await }))
            .map_err(map_fork_err)
    }

    /// `anvil_reset` — reset the fork, optionally re-fork to a new block
    /// number.
    ///
    /// # Errors
    /// `RuntimeError` if the RPC call fails.
    #[pyo3(signature = (block_number=None))]
    fn reset(&self, py: Python<'_>, block_number: Option<u64>) -> PyResult<()> {
        let fork = std::sync::Arc::clone(&self.fork);
        py.detach(|| get_runtime().block_on(async move { fork.reset(block_number).await }))
            .map_err(map_fork_err)
    }

    /// `anvil_snapshot` — take a snapshot, returning the snapshot id as a
    /// Python `int`.
    ///
    /// # Errors
    /// `RuntimeError` if the RPC call fails.
    fn snapshot(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let fork = std::sync::Arc::clone(&self.fork);
        let snap = py
            .detach(|| get_runtime().block_on(async move { fork.snapshot().await }))
            .map_err(map_fork_err)?;
        Ok(u256_to_py(py, &snap)?.into())
    }

    /// `anvil_revert` — revert to a snapshot. Returns `True` if reverted.
    ///
    /// Accepts the `int` returned by [`PyAnvilFork::snapshot`].
    ///
    /// # Errors
    /// `RuntimeError` if the RPC call fails.
    fn revert(&self, py: Python<'_>, snapshot_id: &Bound<'_, PyAny>) -> PyResult<bool> {
        let id = extract_python_u256(snapshot_id)?;
        let fork = std::sync::Arc::clone(&self.fork);
        py.detach(|| get_runtime().block_on(async move { fork.revert(id).await }))
            .map_err(map_fork_err)
    }

    /// `anvil_set_balance`.
    ///
    /// # Errors
    /// `ValueError` on invalid address; `RuntimeError` if the RPC call
    /// fails.
    #[pyo3(signature = (address, balance))]
    fn set_balance(
        &self,
        py: Python<'_>,
        address: &str,
        balance: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let addr = parse_addr(address)?;
        let bal = extract_python_u256(balance)?;
        let fork = std::sync::Arc::clone(&self.fork);
        py.detach(|| get_runtime().block_on(async move { fork.set_balance(addr, bal).await }))
            .map_err(map_fork_err)
    }

    /// `anvil_set_code`.
    ///
    /// # Errors
    /// `ValueError` on invalid address; `RuntimeError` if the RPC call
    /// fails.
    fn set_code(&self, py: Python<'_>, address: &str, code: &Bound<'_, PyBytes>) -> PyResult<()> {
        let addr = parse_addr(address)?;
        let bytes = Bytes::from(code.as_bytes().to_vec());
        let fork = std::sync::Arc::clone(&self.fork);
        py.detach(|| get_runtime().block_on(async move { fork.set_code(addr, bytes).await }))
            .map_err(map_fork_err)
    }

    /// `anvil_set_nonce`.
    ///
    /// # Errors
    /// `ValueError` on invalid address; `RuntimeError` if the RPC call
    /// fails.
    fn set_nonce(&self, py: Python<'_>, address: &str, nonce: u64) -> PyResult<()> {
        let addr = parse_addr(address)?;
        let fork = std::sync::Arc::clone(&self.fork);
        py.detach(|| get_runtime().block_on(async move { fork.set_nonce(addr, nonce).await }))
            .map_err(map_fork_err)
    }

    /// `anvil_set_storage_at`.
    ///
    /// # Errors
    /// `ValueError` on invalid address; `RuntimeError` if the RPC call
    /// fails.
    #[pyo3(signature = (address, slot, value))]
    fn set_storage_at(
        &self,
        py: Python<'_>,
        address: &str,
        slot: &Bound<'_, PyAny>,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let addr = parse_addr(address)?;
        let slot = extract_python_u256(slot)?;
        let value = extract_python_u256(value)?;
        let fork = std::sync::Arc::clone(&self.fork);
        py.detach(|| {
            get_runtime().block_on(async move { fork.set_storage_at(addr, slot, value).await })
        })
        .map_err(map_fork_err)
    }

    /// `anvil_set_next_block_base_fee_per_gas`.
    ///
    /// # Errors
    /// `RuntimeError` if the RPC call fails.
    fn set_next_base_fee(&self, py: Python<'_>, basefee: u128) -> PyResult<()> {
        let fork = std::sync::Arc::clone(&self.fork);
        py.detach(|| {
            get_runtime().block_on(async move { fork.set_next_block_base_fee(basefee).await })
        })
        .map_err(map_fork_err)
    }

    /// `anvil_set_next_block_timestamp`.
    ///
    /// # Errors
    /// `RuntimeError` if the RPC call fails.
    fn set_next_block_timestamp(&self, py: Python<'_>, timestamp: u64) -> PyResult<()> {
        let fork = std::sync::Arc::clone(&self.fork);
        py.detach(|| {
            get_runtime().block_on(async move { fork.set_next_block_timestamp(timestamp).await })
        })
        .map_err(map_fork_err)
    }

    /// `anvil_set_block_timestamp_interval`.
    ///
    /// # Errors
    /// `RuntimeError` if the RPC call fails.
    fn set_block_timestamp_interval(&self, py: Python<'_>, seconds: u64) -> PyResult<()> {
        let fork = std::sync::Arc::clone(&self.fork);
        py.detach(|| {
            get_runtime().block_on(async move { fork.set_block_timestamp_interval(seconds).await })
        })
        .map_err(map_fork_err)
    }

    /// `anvil_set_coinbase`.
    ///
    /// # Errors
    /// `ValueError` on invalid address; `RuntimeError` if the RPC call
    /// fails.
    fn set_coinbase(&self, py: Python<'_>, address: &str) -> PyResult<()> {
        let addr = parse_addr(address)?;
        let fork = std::sync::Arc::clone(&self.fork);
        py.detach(|| get_runtime().block_on(async move { fork.set_coinbase(addr).await }))
            .map_err(map_fork_err)
    }

    fn __repr__(&self) -> String {
        format!("<AnvilFork ipc={}>", self.fork.ipc_path())
    }
}

/// Register the `AnvilFork` pyclass on the Python `degenbot_rs` module.
///
/// # Errors
/// Returns `PyErr` if `add_class` fails (e.g. name collision).
pub fn add_fork_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.fork")?;
    submod.add_class::<PyAnvilFork>()?;
    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.fork", &submod)?;
    Ok(())
}
