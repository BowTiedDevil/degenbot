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
use pyo3::types::{PyBytes, PyDict, PyString, PyTuple};

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

    /// Fetch the factory address for a V2-style pool, performing the full
    /// encode -> call -> decode -> checksum choreography in Rust (ADR-005 slice 14b).
    ///
    /// Mirrors `degenbot/builders/type_resolution.py::fetch_factory_from_chain`:
    /// encode `factory()`, `eth_call` the pool, ABI-decode the right-aligned
    /// 20-byte `address`, EIP-55 checksum it. The RPC primitive (`call`)
    /// still delegates to the held provider (the native-alloy swap is a later
    /// slice); the *choreography* now lives in Rust — slice 14's
    /// "methods for the builder I/O choreography … moved here, called from
    /// Python via `PyBotIo`".
    ///
    /// Returns `None` on any provider error, decode failure, or short result —
    /// mirrors the Python impl's `except (Web3Exception, DecodingError): return None`
    /// contract (a pool whose `factory()` reverts yields `None`, not an error).
    #[pyo3(signature = (address))]
    fn fetch_factory_address(&self, py: Python<'_>, address: &str) -> PyResult<Option<String>> {
        // `factory()` 4-byte selector: keccak256("factory()")[..4] = 0xc45a0155.
        // Same bytes every real `factory()` selector matches at runtime.
        let selector_bytes = selector(b"factory()");

        // Build Python `bytes` for the `data=` kwarg, and a Python `str` for
        // the `to=` kwarg (the held provider's `call(*, to, data, block)` is
        // kw-only and operates on Python objects).
        let address_obj = PyString::new(py, address);
        let data_obj = PyBytes::new(py, &selector_bytes);
        let result_obj = match self.call_kw(
            py,
            "call",
            &[
                ("to", Some(address_obj.as_any())),
                ("data", Some(data_obj.as_any())),
                ("block", None),
            ],
        ) {
            Ok(r) => r,
            Err(_) => return Ok(None), // provider call raised -- mirror `return None`.
        };

        // ABI-decode: result must be >=32 bytes; the address is the
        // right-aligned 20 bytes of the first 32-byte return word.
        let bytes: &[u8] = match result_obj.bind(py).extract::<&[u8]>() {
            Ok(b) => b,
            Err(_) => return Ok(None), // not bytes — mirror `return None`.
        };
        if bytes.len() < 32 {
            return Ok(None); // truncated / decode error.
        }
        let addr_bytes = &bytes[12..32];

        // EIP-55 checksum.
        match degenbot_core::address_utils::to_checksum_address_bytes(addr_bytes) {
            Ok(s) => Ok(Some(s)),
            Err(_) => Ok(None),
        }
    }

    /// Fetch ERC-20 token name / symbol / decimals via batched RPC calls,
    /// performing the full encode -> call (x3) -> decode choreography in Rust
    /// (ADR-005 slice 14c).
    ///
    /// Mirrors `degenbot/builders/erc20_builder.py::_fetch_name_symbol_decimals_batched`:
    /// encode the 4-byte selectors for `name()`, `symbol()`, `decimals()`, fire
    /// three `eth_call`s at the token address, ABI-decode each as `string`,
    /// `string`, `uint256` respectively. Returns the `(name, symbol, decimals)`
    /// tuple on success.
    ///
    /// Returns `None` on any provider error or decode failure — mirrors the
    /// Python batched impl's caller-side `except (Web3Exception, DecodingError)`
    /// contract, which falls back to per-call `bytes32` alternate prototypes when
    /// the batched path fails. (The Rust impl surfaces the same `None` signal so
    /// the caller's fallback kicks in identically.)
    #[pyo3(signature = (address))]
    fn fetch_erc20_metadata(
        &self,
        py: Python<'_>,
        address: &str,
    ) -> PyResult<Option<(String, String, u64)>> {
        use alloy::dyn_abi::{DynSolType, DynSolValue};

        // Encode the three selectors at compile time.
        let name_selector = selector(b"name()");
        let symbol_selector = selector(b"symbol()");
        let decimals_selector = selector(b"decimals()");

        let address_obj = pyo3::types::PyString::new(py, address);

        // Fire the three calls; any error -> Ok(None).
        let name_result = match self.call_kw(
            py,
            "call",
            &[
                ("to", Some(address_obj.as_any())),
                (
                    "data",
                    Some(pyo3::types::PyBytes::new(py, &name_selector).as_any()),
                ),
                ("block", None),
            ],
        ) {
            Ok(r) => r,
            Err(_) => return Ok(None),
        };
        let symbol_result = match self.call_kw(
            py,
            "call",
            &[
                ("to", Some(address_obj.as_any())),
                (
                    "data",
                    Some(pyo3::types::PyBytes::new(py, &symbol_selector).as_any()),
                ),
                ("block", None),
            ],
        ) {
            Ok(r) => r,
            Err(_) => return Ok(None),
        };
        let decimals_result = match self.call_kw(
            py,
            "call",
            &[
                ("to", Some(address_obj.as_any())),
                (
                    "data",
                    Some(pyo3::types::PyBytes::new(py, &decimals_selector).as_any()),
                ),
                ("block", None),
            ],
        ) {
            Ok(r) => r,
            Err(_) => return Ok(None),
        };

        // Extract &[u8] from each HexBytes/bytes result.
        let name_bytes: &[u8] = match name_result.bind(py).extract::<&[u8]>() {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };
        let symbol_bytes: &[u8] = match symbol_result.bind(py).extract::<&[u8]>() {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };
        let decimals_bytes: &[u8] = match decimals_result.bind(py).extract::<&[u8]>() {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };

        // Decode dynamic `string`, `string`, and `uint256`. Any decode error -> Ok(None)
        // (mirror the Python `except DecodingError`).
        let name = match DynSolType::String.abi_decode(name_bytes) {
            Ok(DynSolValue::String(s)) => s,
            _ => return Ok(None),
        };
        let symbol = match DynSolType::String.abi_decode(symbol_bytes) {
            Ok(DynSolValue::String(s)) => s,
            _ => return Ok(None),
        };
        let decimals = match DynSolType::Uint(256).abi_decode(decimals_bytes) {
            Ok(DynSolValue::Uint(n, _)) => n.to::<u64>(),
            _ => return Ok(None),
        };

        Ok(Some((name, symbol, decimals)))
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

/// Compute a 4-byte Solidity function selector (`keccak256(signature)[..4]`).
///
/// Same construction as `alloy_sol_types::sol!` would emit at compile time.
/// Used by `fetch_factory_address` and `fetch_erc20_metadata` to build the
/// 4-byte calldata prefixes that router-style pool/token read methods (e.g.
/// `factory()`, `name()`, `symbol()`, `decimals()`) expect.
fn selector(signature: &[u8]) -> [u8; 4] {
    use alloy::primitives::keccak256;
    let hash = keccak256(signature);
    let mut s = [0u8; 4];
    s.copy_from_slice(&hash[..4]);
    s
}
