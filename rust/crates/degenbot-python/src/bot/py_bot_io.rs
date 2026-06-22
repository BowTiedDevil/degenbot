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
        // Delegate the encode→call→decode→checksum to the shared no-arg
        // address-returning helper, then swallow any error into `None` here
        // to preserve the `None`-on-failure contract this method promises in
        // 14b (mirrors the Python `except (Web3Exception, DecodingError): return None`).
        match self.fetch_address_returning_method(py, b"factory()", address, None) {
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

    /// Fetch a V2-style pool's immutable data — `factory()`, `token0()`, `token1()` —
    /// performing the 3-call encode -> call -> decode choreography in Rust
    /// (ADR-005 slice 14e).
    ///
    /// Mirrors the immutable-RPC block of `v2_builder_base.py::V2BuilderBase.
    /// _fetch_v2_common_data` (the fallback path when the DB lookup misses).
    /// Each of the 3 calls is a no-arg address-returning read, fulfilled by
    /// [`Self::fetch_address_returning_method`] (shared with `fetch_factory_address`).
    /// Returns `(factory, token0, token1)` as EIP-55 checksummed strings.
    ///
    /// Errors propagate: any provider call revert surfaces as `PyErr`(no
    /// swallowing) — matches the Python `except Exception: raise
    /// LiquidityPoolError` contract (the caller wraps in its own exception).
    #[pyo3(signature = (pool_address, block=None))]
    fn fetch_v2_immutable_data(
        &self,
        py: Python<'_>,
        pool_address: &str,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<(String, String, String)> {
        let factory = self.fetch_address_returning_method(py, b"factory()", pool_address, block)?;
        let token0 = self.fetch_address_returning_method(py, b"token0()", pool_address, block)?;
        let token1 = self.fetch_address_returning_method(py, b"token1()", pool_address, block)?;
        Ok((factory, token0, token1))
    }

    /// Fetch a V2-style pool's reserves via `getReserves()`, performing the
    /// encode -> call -> decode choreography in Rust (ADR-005 slice 14e).
    ///
    /// Mirrors the reserves-RPC block of `V2BuilderBase._fetch_v2_common_data`.
    /// Selector `0x0902f1ac`; no args; ABI-decodes a `(uint256, uint256)` tuple.
    /// Returns `(reserves0, reserves1)` as Python ints (preserved through
    /// [`crate::conversion::alloy::u256_to_py`] — large values stay exact).
    ///
    /// Errors propagate (see [`Self::fetch_v2_immutable_data`]).
    #[pyo3(signature = (pool_address, block=None))]
    fn fetch_v2_reserves(
        &self,
        py: Python<'_>,
        pool_address: &str,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<(Py<PyAny>, Py<PyAny>)> {
        use alloy::dyn_abi::{DynSolType, DynSolValue};

        let calldata = selector(b"getReserves()");
        let result_obj = self.forward_call_to_provider(py, pool_address, &calldata, block)?;
        let bytes: &[u8] = result_obj.bind(py).extract::<&[u8]>()?;
        // getReserves returns (uint112, uint112, uint32) packed, but the Python
        // impl decodes as `(uint256, uint256)` and takes the first two words —
        // the third word (blockTimestampLast) is unused. Replicate exactly so
        // the parity test passes.
        let tuple_type = DynSolType::Tuple(vec![DynSolType::Uint(256), DynSolType::Uint(256)]);
        let decoded = tuple_type.abi_decode(bytes).map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("invalid reserves decode: {e}"))
        })?;
        let mut it = match decoded {
            DynSolValue::Tuple(vals) => vals.into_iter(),
            _ => {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "expected tuple for getReserves",
                ))
            }
        };
        let r0 = match it.next() {
            Some(DynSolValue::Uint(n, _)) => n,
            _ => return Err(pyo3::exceptions::PyValueError::new_err("invalid reserves0")),
        };
        let r1 = match it.next() {
            Some(DynSolValue::Uint(n, _)) => n,
            _ => return Err(pyo3::exceptions::PyValueError::new_err("invalid reserves1")),
        };
        Ok((
            crate::conversion::alloy::u256_to_py(py, &r0)?.unbind(),
            crate::conversion::alloy::u256_to_py(py, &r1)?.unbind(),
        ))
    }

    /// Fetch a V3-style pool's immutable data — `factory()`, `token0()`, `token1()`,
    /// `fee()`, `tickSpacing()` — the 5-call encode -> call -> decode choreography
    /// in Rust (ADR-005 slice 14f).
    ///
    /// Mirrors the immutable-RPC block of `v3_pool_builder.py::V3PoolBuilder.build`'s
    /// DB-miss fallback path. The first 3 calls are no-arg address-returning reads
    /// (re-use [`Self::fetch_address_returning_method`]); the last 2 are no-arg
    /// numeric reads (`fee` as `uint24`, `tickSpacing` as `int24`).
    ///
    /// Returns `(factory, token0, token1, fee, tick_spacing)` with addresses
    /// EIP-55 checksummed; `fee`/`tick_spacing` returned as Python ints (small
    /// values, safe lossless conversion).
    ///
    /// Errors propagate (see [`Self::fetch_v2_immutable_data`]).
    #[pyo3(signature = (pool_address, block=None))]
    fn fetch_v3_immutable_data(
        &self,
        py: Python<'_>,
        pool_address: &str,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<(String, String, String, Py<PyAny>, Py<PyAny>)> {
        use alloy::dyn_abi::DynSolType;

        let factory = self.fetch_address_returning_method(py, b"factory()", pool_address, block)?;
        let token0 = self.fetch_address_returning_method(py, b"token0()", pool_address, block)?;
        let token1 = self.fetch_address_returning_method(py, b"token1()", pool_address, block)?;

        // fee() -> uint24, no args.
        let fee =
            self.fetch_no_arg_uint(py, b"fee()", pool_address, block, DynSolType::Uint(24))?;
        // tickSpacing() -> int24, no args.
        let tick_spacing = self.fetch_no_arg_int(
            py,
            b"tickSpacing()",
            pool_address,
            block,
            DynSolType::Int(24),
        )?;

        Ok((factory, token0, token1, fee, tick_spacing))
    }

    /// Fetch a V3-style pool's `slot0()` + `liquidity()` state — the 2-call
    /// encode -> call -> decode choreography in Rust (ADR-005 slice 14f).
    ///
    /// Mirrors `V3PoolBuilder.build`'s slot0+liquidity RPC block. `slot0()`
    /// returns `(uint160 sqrtPriceX96, int24 tick, uint16, uint16, uint16, uint8,
    /// bool)` — only the first two values are needed (the rest are ignored,
    /// matching Python `decode_slot0`). `liquidity()` returns `uint128`.
    ///
    /// Returns `(sqrt_price_x96, tick, liquidity)`: sqrtPriceX96 + liquidity as
    /// Python ints (via [`crate::conversion::alloy::u256_to_py`] —> large values
    /// stay exact); tick as a Python int (`int24` sign-extended, preserved
    /// through `I256` -> `i64` -> Python int).
    ///
    /// Errors propagate.
    #[pyo3(signature = (pool_address, block=None))]
    fn fetch_v3_slot0_liquidity(
        &self,
        py: Python<'_>,
        pool_address: &str,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<(Py<PyAny>, Py<PyAny>, Py<PyAny>)> {
        use alloy::dyn_abi::{DynSolType, DynSolValue};

        // slot0(): decode sqrtPriceX96 as uint160 (word 0) + tick as int24 (word 1);
        // the remaining 5 packed fields are ignored.
        let slot0_calldata = selector(b"slot0()");
        let slot0_obj = self.forward_call_to_provider(py, pool_address, &slot0_calldata, block)?;
        let slot0_bytes: &[u8] = slot0_obj.bind(py).extract::<&[u8]>()?;
        let slot0_tuple = DynSolType::Tuple(vec![DynSolType::Uint(160), DynSolType::Int(24)]);
        let slot0_decoded = slot0_tuple.abi_decode(slot0_bytes).map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("invalid slot0 decode: {e}"))
        })?;
        let mut slot0_it = match slot0_decoded {
            DynSolValue::Tuple(vals) => vals.into_iter(),
            _ => {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "expected tuple for slot0",
                ))
            }
        };
        let sqrt_price_x96 = match slot0_it.next() {
            Some(DynSolValue::Uint(n, _)) => n,
            _ => {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "invalid sqrtPriceX96",
                ))
            }
        };
        let tick_i256 = match slot0_it.next() {
            Some(DynSolValue::Int(n, _)) => n,
            _ => return Err(pyo3::exceptions::PyValueError::new_err("invalid tick")),
        };

        // liquidity(): uint128, right-padded in a 32-byte word; treat as uint256 decode for convenience.
        let liq_calldata = selector(b"liquidity()");
        let liq_obj = self.forward_call_to_provider(py, pool_address, &liq_calldata, block)?;
        let liq_bytes: &[u8] = liq_obj.bind(py).extract::<&[u8]>()?;
        let liquidity = match DynSolType::Uint(128).abi_decode(liq_bytes) {
            Ok(DynSolValue::Uint(n, _)) => n,
            _ => return Err(pyo3::exceptions::PyValueError::new_err("invalid liquidity")),
        };

        Ok((
            crate::conversion::alloy::u256_to_py(py, &sqrt_price_x96)?.unbind(),
            // int24 sign-extended: convert through i256_to_py (handles
            // negatives; no precision loss since int24 fits in i32).
            crate::conversion::alloy::i256_to_py(py, &tick_i256)?.unbind(),
            crate::conversion::alloy::u256_to_py(py, &liquidity)?.unbind(),
        ))
    }

    /// Fetch an ERC-20 token balance via `balanceOf(address)`, performing the
    /// full encode -> call -> decode choreography in Rust (ADR-005 slice 14d).
    ///
    /// Mirrors `degenbot/builders/erc20_builder.py::Erc20Builder.get_token_balance`'s
    /// I/O call path (cache + checksum are out of scope; the caller still owns
    /// those). The `balanceOf(address)` selector (`0x70a08231`) is built via the
    /// [`selector`] helper; the 20-byte address arg is ABI-encoded right-padded
    /// in a 32-byte word; the `uint256` result is decoded via alloy's `DynSolType`.
    ///
    /// Errors propagate: a provider call revert or decode failure surfaces as a
    /// `PyErr` to the caller (no swallowing) — matches the Python impl's no-
    /// try/except contract, which trusts the caller to handle.
    #[pyo3(signature = (token, owner, block=None))]
    fn fetch_token_balance(
        &self,
        py: Python<'_>,
        token: &str,
        owner: &str,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        self.fetch_single_address_arg_uint(py, selector(b"balanceOf(address)"), token, owner, block)
    }

    /// Fetch an ERC-20 token allowance via `allowance(address,address)`, the
    /// two-address-arg parameterized-call pattern (ADR-005 slice 14d).
    ///
    /// Mirrors `Erc20Builder.get_token_approval`'s I/O path. Selector
    /// `0xdd62ed3e`; two ABI-encoded `address` args; decoded `uint256` result.
    /// Errors propagate (see [`Self::fetch_token_balance`]).
    #[pyo3(signature = (token, owner, spender, block=None))]
    fn fetch_token_allowance(
        &self,
        py: Python<'_>,
        token: &str,
        owner: &str,
        spender: &str,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        use alloy::dyn_abi::{DynSolType, DynSolValue};

        let sel = selector(b"allowance(address,address)");
        let owner_addr = parse_address_for_call(owner)?;
        let spender_addr = parse_address_for_call(spender)?;
        // Manually pack: selector (4) + 2 right-padded 32-byte address words.
        let mut calldata = Vec::with_capacity(4 + 64);
        calldata.extend_from_slice(&sel);
        calldata.extend_from_slice(&[0u8; 12]);
        calldata.extend_from_slice(owner_addr.as_slice());
        calldata.extend_from_slice(&[0u8; 12]);
        calldata.extend_from_slice(spender_addr.as_slice());

        let result_obj = self.forward_call_to_provider(py, token, &calldata, block)?;
        let bytes: &[u8] = result_obj.bind(py).extract::<&[u8]>()?;
        let n = match DynSolType::Uint(256).abi_decode(bytes) {
            Ok(DynSolValue::Uint(n, _)) => n,
            _ => {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "invalid uint256 decode",
                ))
            }
        };
        crate::conversion::alloy::u256_to_py(py, &n).map(|b| b.unbind())
    }

    /// Fetch an ERC-20 token's total supply via `totalSupply()`, the no-arg
    /// uint256-returning call pattern (ADR-005 slice 14d).
    ///
    /// Mirrors `Erc20Builder.get_token_total_supply`'s I/O path. Selector
    /// `0x18160ddd`; no args; decoded `uint256` result. Errors propagate.
    #[pyo3(signature = (token, block=None))]
    fn fetch_token_total_supply(
        &self,
        py: Python<'_>,
        token: &str,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        use alloy::dyn_abi::{DynSolType, DynSolValue};

        let calldata = selector(b"totalSupply()");
        let result_obj = self.forward_call_to_provider(py, token, &calldata, block)?;
        let bytes: &[u8] = result_obj.bind(py).extract::<&[u8]>()?;
        let n = match DynSolType::Uint(256).abi_decode(bytes) {
            Ok(DynSolValue::Uint(n, _)) => n,
            _ => {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "invalid uint256 decode",
                ))
            }
        };
        crate::conversion::alloy::u256_to_py(py, &n).map(|b| b.unbind())
    }

    /// Fetch Aerodrome V2 pool's `stable()` flag and factory `getFee(address,bool)`
    /// — the 2-call choreography with a data dependency (ADR-005 slice 14g).
    ///
    /// Mirrors `aerodrome_v2_builder.py::AerodromeV2PoolBuilder.build`'s
    /// Aerodrome-specific RPC block. First call: `stable()` on the pool address
    /// (no-arg, returns `bool`). Second call: `getFee(address,bool)` on the
    /// factory address, with the pool address and the first call's `stable`
    /// result as ABI-encoded arguments. Returns `(stable, fee_raw)`.
    ///
    /// New pattern introduced: mixed-type ABI encoding (address + bool in a
    /// single calldata), and a data dependency between the two calls (the `stable`
    /// result from call 1 is an argument to call 2).
    ///
    /// Errors propagate: any provider revert surfaces as `PyErr`.
    #[pyo3(signature = (pool_address, factory_address, block=None))]
    fn fetch_aerodrome_v2_stable_and_fee(
        &self,
        py: Python<'_>,
        pool_address: &str,
        factory_address: &str,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<(bool, Py<PyAny>)> {
        use alloy::dyn_abi::{DynSolType, DynSolValue};

        // Call 1: stable() on the pool — no-arg, decode as bool.
        let stable_calldata = selector(b"stable()");
        let stable_result =
            self.forward_call_to_provider(py, pool_address, &stable_calldata, block)?;
        let stable_bytes: &[u8] = stable_result.bind(py).extract::<&[u8]>()?;
        let stable = match DynSolType::Bool.abi_decode(stable_bytes) {
            Ok(DynSolValue::Bool(b)) => b,
            _ => {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "invalid stable decode",
                ))
            }
        };

        // Call 2: getFee(address,bool) on the factory — ABI-encode pool address
        // (right-padded 32-byte word 0) + stable bool (32-byte word 1).
        let pool_addr = parse_address_for_call(pool_address)?;
        let get_fee_sel = selector(b"getFee(address,bool)");
        let mut calldata = Vec::with_capacity(4 + 64);
        calldata.extend_from_slice(&get_fee_sel);
        calldata.extend_from_slice(&[0u8; 12]);
        calldata.extend_from_slice(pool_addr.as_slice());
        calldata.extend_from_slice(&[0u8; 31]);
        calldata.push(if stable { 1 } else { 0 });

        let fee_result = self.forward_call_to_provider(py, factory_address, &calldata, block)?;
        let fee_bytes: &[u8] = fee_result.bind(py).extract::<&[u8]>()?;
        let fee = match DynSolType::Uint(256).abi_decode(fee_bytes) {
            Ok(DynSolValue::Uint(n, _)) => n,
            _ => {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "invalid fee decode",
                ))
            }
        };

        Ok((
            stable,
            crate::conversion::alloy::u256_to_py(py, &fee)?.unbind(),
        ))
    }

    /// Fetch an ERC-20 uint field (`decimals()` / `DECIMALS()`) -- the
    /// dynamic-signature no-arg uint-returning choreography (ADR-005 slice 14h).
    ///
    /// Mirrors `erc20_builder.py::_fetch_decimals`: the caller passes the
    /// signature dynamically (`"decimals()"` vs `"DECIMALS()"`) and the method
    /// encodes the selector, calls, and decodes `uint256`. Errors propagate.
    #[pyo3(signature = (address, signature, block=None))]
    fn fetch_erc20_uint_field(
        &self,
        py: Python<'_>,
        address: &str,
        signature: &str,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        self.fetch_no_arg_uint(
            py,
            signature.as_bytes(),
            address,
            block,
            alloy::dyn_abi::DynSolType::Uint(256),
        )
    }

    /// Fetch an ERC-20 string field (`name()` / `symbol()` / `NAME()` etc.) --
    /// performs the encode -> call -> decode choreography in Rust with a
    /// string-or-bytes32 fallback (ADR-005 slice 14h).
    ///
    /// Mirrors `erc20_builder.py::_fetch_name` / `_fetch_symbol`: try `string`
    /// ABI-decode first; on decode failure, fall back to `bytes32` decode
    /// (UTF-8 with errors ignored, leading/trailing null bytes stripped).
    /// The `signature` parameter is dynamic (e.g. `"name()"` vs `"NAME()"`)
    /// so the caller can try alternate prototypes in a loop.
    ///
    /// Errors propagate: provider revert or total decode failure (neither
    /// `string` nor `bytes32` could decode) surfaces as `PyErr` -- the Python
    /// caller catches it in its `except (Web3Exception, DecodingError)` loop.
    #[pyo3(signature = (address, signature, block=None))]
    fn fetch_erc20_string_field(
        &self,
        py: Python<'_>,
        address: &str,
        signature: &str,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<String> {
        use alloy::dyn_abi::{DynSolType, DynSolValue};

        let calldata = selector(signature.as_bytes());
        let result_obj = self.forward_call_to_provider(py, address, &calldata, block)?;
        let bytes: &[u8] = result_obj.bind(py).extract::<&[u8]>()?;

        // Try string decode first.
        if let Ok(DynSolValue::String(s)) = DynSolType::String.abi_decode(bytes) {
            return Ok(s);
        }

        // Fallback: bytes32 decode (some tokens use bytes32 for name/symbol).
        if let Ok(DynSolValue::FixedBytes(fb, _)) = DynSolType::FixedBytes(32).abi_decode(bytes) {
            return Ok(String::from_utf8_lossy(fb.as_slice())
                .trim_matches('\0')
                .to_string());
        }

        Err(pyo3::exceptions::PyValueError::new_err(
            "could not decode ERC-20 string field as string or bytes32",
        ))
    }

    /// Probe a pool's type by trying method calls in order (ADR-005 slice 14i).
    ///
    /// Mirrors `type_resolution.py::resolve_pool_type_by_probing`: tries V3
    /// (`slot0()`), then V2 (`getReserves()`), then Balancer (`getPoolId()` +
    /// `getNormalizedWeights()` sub-probe). Returns a string tag identifying
    /// which probe succeeded so the Python caller can construct a
    /// `PoolTypeDescriptor` from the registry.
    ///
    /// Returns one of:
    /// - `"slot0"` — V3 concentrated-liquidity pool.
    /// - `"getReserves"` — V2 constant-product pool.
    /// - `"balancer_weighted"` — Balancer weighted pool.
    /// - `"balancer_stable"` — Balancer stable pool.
    /// - `"stableswap"` — Curve fallback (all probes reverted).
    ///
    /// Each probe is a fire-and-forget `call` — the result is not decoded, only
    /// whether the call succeeded or reverted matters. Reverts (any `PyErr`)
    /// are caught and the next probe is tried.
    #[pyo3(signature = (address, block=None))]
    fn probe_pool_type(
        &self,
        py: Python<'_>,
        address: &str,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<String> {
        // Helper: try a no-arg call, return true if it succeeded.
        let try_call = |sig: &[u8]| -> bool {
            let calldata = selector(sig);
            self.forward_call_to_provider(py, address, &calldata, block)
                .is_ok()
        };

        // Probe 1: slot0() → V3.
        if try_call(b"slot0()") {
            return Ok("slot0".to_string());
        }
        // Probe 2: getReserves() → V2.
        if try_call(b"getReserves()") {
            return Ok("getReserves".to_string());
        }
        // Probe 3: getPoolId() → Balancer (weighted or stable).
        if try_call(b"getPoolId()") {
            if try_call(b"getNormalizedWeights()") {
                return Ok("balancer_weighted".to_string());
            }
            return Ok("balancer_stable".to_string());
        }
        // Fallback: Curve stableswap.
        Ok("stableswap".to_string())
    }

    /// Fetch a V3 pool's tick bitmap word via `tickBitmap(int16)`, performing
    /// the encode -> call -> decode choreography in Rust (ADR-005 slice 14j).
    ///
    /// Mirrors `tick_data_fetcher.py::_fetch_v3`'s bitmap RPC block. The
    /// `word_position` is a signed `int16` ABI-encoded as a sign-extended
    /// 32-byte word. The result is a `uint256` bitmap value.
    ///
    /// New pattern: signed-integer argument encoding (int16 sign extension to
    /// 32 bytes via `I256::to_be_bytes::<32>()`).
    ///
    /// Errors propagate: provider revert surfaces as `PyErr` (the Python
    /// caller's `except Exception: return` handles it).
    #[pyo3(signature = (pool_address, word_position, block=None))]
    fn fetch_tick_bitmap(
        &self,
        py: Python<'_>,
        pool_address: &str,
        word_position: i64,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        use alloy::primitives::U256;

        let sel = selector(b"tickBitmap(int16)");
        // Sign-extend the int16 argument to a 32-byte word (two's complement).
        let arg = sign_extend_to_32_bytes(word_position);
        let mut calldata = Vec::with_capacity(36);
        calldata.extend_from_slice(&sel);
        calldata.extend_from_slice(&arg);

        let result_obj = self.forward_call_to_provider(py, pool_address, &calldata, block)?;
        let bytes: &[u8] = result_obj.bind(py).extract::<&[u8]>()?;
        if bytes.len() < 32 {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "tickBitmap result < 32 bytes",
            ));
        }
        let bitmap = U256::from_be_slice(&bytes[0..32]);
        crate::conversion::alloy::u256_to_py(py, &bitmap).map(|b| b.unbind())
    }

    /// Fetch a V3 pool's tick liquidity data via `ticks(int24)`, performing
    /// the encode -> call -> decode choreography in Rust (ADR-005 slice 14j).
    ///
    /// Mirrors `tick_data_fetcher.py::_fetch_v3`'s tick-data RPC block. The
    /// `tick` is a signed `int24` ABI-encoded as a sign-extended 32-byte word.
    /// The result is a multi-field struct; only the first two fields matter:
    /// `liquidity_gross` (uint128) and `liquidity_net` (int128). These are the
    /// first two 32-byte words of the ABI-encoded return data.
    ///
    /// Errors propagate (see [`Self::fetch_tick_bitmap`]).
    #[pyo3(signature = (pool_address, tick, block=None))]
    fn fetch_tick_data(
        &self,
        py: Python<'_>,
        pool_address: &str,
        tick: i64,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<(Py<PyAny>, Py<PyAny>)> {
        use alloy::primitives::U256;

        let sel = selector(b"ticks(int24)");
        // Sign-extend the int24 argument to a 32-byte word (two's complement).
        let arg = sign_extend_to_32_bytes(tick);
        let mut calldata = Vec::with_capacity(36);
        calldata.extend_from_slice(&sel);
        calldata.extend_from_slice(&arg);

        let result_obj = self.forward_call_to_provider(py, pool_address, &calldata, block)?;
        let bytes: &[u8] = result_obj.bind(py).extract::<&[u8]>()?;
        if bytes.len() < 64 {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "ticks result < 64 bytes",
            ));
        }
        // Word 0: liquidity_gross (uint128, right-aligned in 32-byte word).
        let gross = U256::from_be_slice(&bytes[0..32]);
        // Word 1: liquidity_net (int128, sign-extended in 32-byte word).
        let net = alloy::primitives::I256::try_from_be_slice(&bytes[32..64])
            .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("invalid int128 decode"))?;

        Ok((
            crate::conversion::alloy::u256_to_py(py, &gross)?.unbind(),
            crate::conversion::alloy::i256_to_py(py, &net)?.unbind(),
        ))
    }

    /// Fetch a V4 pool's tick bitmap via `getTickBitmap(bytes32,int16)` on the
    /// state-view contract (ADR-005 slice 14k).
    ///
    /// Mirrors `tick_data_fetcher.py::_fetch_v4`'s bitmap RPC block. V4 adds a
    /// `pool_id` (`bytes32`) prefix argument before the `int16` word position.
    /// Calldata: selector (4) + pool_id (32) + sign-extended int16 (32) = 68 bytes.
    ///
    /// Errors propagate.
    #[pyo3(signature = (state_view_address, pool_id, word_position, block=None))]
    fn fetch_v4_tick_bitmap(
        &self,
        py: Python<'_>,
        state_view_address: &str,
        pool_id: &[u8],
        word_position: i64,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        use alloy::primitives::U256;

        let sel = selector(b"getTickBitmap(bytes32,int16)");
        let arg = sign_extend_to_32_bytes(word_position);
        let mut calldata = Vec::with_capacity(68);
        calldata.extend_from_slice(&sel);
        calldata.extend_from_slice(pool_id);
        calldata.extend_from_slice(&arg);

        let result_obj = self.forward_call_to_provider(py, state_view_address, &calldata, block)?;
        let bytes: &[u8] = result_obj.bind(py).extract::<&[u8]>()?;
        if bytes.len() < 32 {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "getTickBitmap result < 32 bytes",
            ));
        }
        let bitmap = U256::from_be_slice(&bytes[0..32]);
        crate::conversion::alloy::u256_to_py(py, &bitmap).map(|b| b.unbind())
    }

    /// Fetch a V4 pool's tick liquidity via `getTickLiquidity(bytes32,int24)`
    /// on the state-view contract (ADR-005 slice 14k).
    ///
    /// Mirrors `tick_data_fetcher.py::_fetch_v4`'s tick-data RPC block. V4 adds
    /// a `pool_id` (`bytes32`) prefix argument before the `int24` tick. The V4
    /// tick return is just `(uint128, int128)` — exactly 2 fields.
    ///
    /// Errors propagate.
    #[pyo3(signature = (state_view_address, pool_id, tick, block=None))]
    fn fetch_v4_tick_data(
        &self,
        py: Python<'_>,
        state_view_address: &str,
        pool_id: &[u8],
        tick: i64,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<(Py<PyAny>, Py<PyAny>)> {
        use alloy::primitives::U256;

        let sel = selector(b"getTickLiquidity(bytes32,int24)");
        let arg = sign_extend_to_32_bytes(tick);
        let mut calldata = Vec::with_capacity(68);
        calldata.extend_from_slice(&sel);
        calldata.extend_from_slice(pool_id);
        calldata.extend_from_slice(&arg);

        let result_obj = self.forward_call_to_provider(py, state_view_address, &calldata, block)?;
        let bytes: &[u8] = result_obj.bind(py).extract::<&[u8]>()?;
        if bytes.len() < 64 {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "getTickLiquidity result < 64 bytes",
            ));
        }
        let gross = U256::from_be_slice(&bytes[0..32]);
        let net = alloy::primitives::I256::try_from_be_slice(&bytes[32..64])
            .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("invalid int128 decode"))?;

        Ok((
            crate::conversion::alloy::u256_to_py(py, &gross)?.unbind(),
            crate::conversion::alloy::i256_to_py(py, &net)?.unbind(),
        ))
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

    /// Build a `call(to=token, data=calldata, block=None)` forward to the held
    /// provider -- the common skeleton the choreography methods (`fetch_factory_address`,
    /// `fetch_erc20_metadata`, `fetch_token_*`) share. Returns the raw result
    /// `Py<PyAny>` without extraction so callers can decide what to decode.
    ///
    /// Single seam: changing the call-forward strategy (e.g. native-alloy swap
    /// in a later slice) is localized to this helper.
    /// Shared skeleton for no-arg, address-returning pool read methods
    /// (`factory()`, `token0()`, `token1()`): build selector, call, decode
    /// right-aligned 20-byte address, checksum. Errors propagate
    /// (unlike `fetch_factory_address` which swallows them — different contract:
    /// the immutable-data choreography in `_fetch_v2_common_data` wants the
    /// underlying exception so the Python caller wraps it in
    /// `LiquidityPoolError`).
    /// Shared skeleton for no-arg, unsigned-int-returning pool read methods
    /// (`fee()` returns `uint24`, `liquidity()` returns `uint128`): build
    /// selector, call, decode the full ABI word as `DynSolType` `ty` (typically
    /// `Uint(bits)`), convert to a Python `int` via `u256_to_py` (large values
    /// preserved). Errors propagate.
    fn fetch_no_arg_uint(
        &self,
        py: Python<'_>,
        signature: &[u8],
        pool_address: &str,
        block: Option<&Bound<'_, PyAny>>,
        ty: alloy::dyn_abi::DynSolType,
    ) -> PyResult<Py<PyAny>> {
        use alloy::dyn_abi::DynSolValue;

        let calldata = selector(signature);
        let result_obj = self.forward_call_to_provider(py, pool_address, &calldata, block)?;
        let bytes: &[u8] = result_obj.bind(py).extract::<&[u8]>()?;
        let n = match ty.abi_decode(bytes) {
            Ok(DynSolValue::Uint(n, _)) => n,
            _ => {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "invalid uint decode for no-arg read",
                ))
            }
        };
        crate::conversion::alloy::u256_to_py(py, &n).map(|b| b.unbind())
    }

    /// Shared skeleton for no-arg, signed-int-returning pool read methods
    /// (`tickSpacing()` returns `int24`): build selector, call, decode the full
    /// ABI word as the given `DynSolType` (typically `Int(bits)`), convert to a
    /// Python `int` via `i256_to_py` (negative values preserved through the
    /// sign-extended decode). Errors propagate.
    fn fetch_no_arg_int(
        &self,
        py: Python<'_>,
        signature: &[u8],
        pool_address: &str,
        block: Option<&Bound<'_, PyAny>>,
        ty: alloy::dyn_abi::DynSolType,
    ) -> PyResult<Py<PyAny>> {
        use alloy::dyn_abi::DynSolValue;

        let calldata = selector(signature);
        let result_obj = self.forward_call_to_provider(py, pool_address, &calldata, block)?;
        let bytes: &[u8] = result_obj.bind(py).extract::<&[u8]>()?;
        let n = match ty.abi_decode(bytes) {
            Ok(DynSolValue::Int(n, _)) => n,
            _ => {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "invalid int decode for no-arg read",
                ))
            }
        };
        crate::conversion::alloy::i256_to_py(py, &n).map(|b| b.unbind())
    }

    fn fetch_address_returning_method(
        &self,
        py: Python<'_>,
        signature: &[u8],
        pool_address: &str,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<String> {
        let calldata = selector(signature);
        let result_obj = self.forward_call_to_provider(py, pool_address, &calldata, block)?;
        let bytes: &[u8] = result_obj.bind(py).extract::<&[u8]>()?;
        if bytes.len() < 32 {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "address-returning call returned <32 bytes",
            ));
        }
        let addr_bytes = &bytes[12..32];
        degenbot_core::address_utils::to_checksum_address_bytes(addr_bytes)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid address: {e}")))
    }

    fn forward_call_to_provider(
        &self,
        py: Python<'_>,
        token: &str,
        calldata: &[u8],
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let address_obj = PyString::new(py, token);
        let data_obj = PyBytes::new(py, calldata);
        self.call_kw(
            py,
            "call",
            &[
                ("to", Some(address_obj.as_any())),
                ("data", Some(data_obj.as_any())),
                ("block", block),
            ],
        )
    }

    /// Shared skeleton for the single-address-arg, uint256-returning ERC-20
    /// read methods (`balanceOf(address)`):
    /// build selector + right-padded 32-byte address word, call, decode `uint256`.
    /// Used by `fetch_token_balance` (and re-usable for any future analogous
    /// read). Errors propagate.
    fn fetch_single_address_arg_uint(
        &self,
        py: Python<'_>,
        sel: [u8; 4],
        token: &str,
        address_arg: &str,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        use alloy::dyn_abi::{DynSolType, DynSolValue};

        let addr = parse_address_for_call(address_arg)?;
        // ABI-encode: selector (4) + 32-byte word, right-padded with 12 zero
        // prefix bytes then the 20-byte address.
        let mut calldata = Vec::with_capacity(4 + 32);
        calldata.extend_from_slice(&sel);
        calldata.extend_from_slice(&[0u8; 12]);
        calldata.extend_from_slice(addr.as_slice());

        let result_obj = self.forward_call_to_provider(py, token, &calldata, block)?;
        let bytes: &[u8] = result_obj.bind(py).extract::<&[u8]>()?;
        let n = match DynSolType::Uint(256).abi_decode(bytes) {
            Ok(DynSolValue::Uint(n, _)) => n,
            _ => {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "invalid uint256 decode",
                ))
            }
        };
        crate::conversion::alloy::u256_to_py(py, &n).map(|b| b.unbind())
    }
}

/// Parse a 20-byte address from a hex string, returning a borrowed 20-byte array
/// view for ABI-encoding. Internally uses the core `parse_address` so input
/// validation matches every other Rust pyclass (e.g. `PyAlloyProvider::get_balance`).
fn parse_address_for_call(address: &str) -> PyResult<[u8; 20]> {
    use degenbot_core::address_utils::parse_address;
    parse_address(address)
        .map(|a| a.into_array())
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}")))
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

/// Sign-extend an `i64` value to a 32-byte big-endian two's complement word.
///
/// Used by `fetch_tick_bitmap` / `fetch_tick_data` to encode `int16` / `int24`
/// arguments as ABI-compliant 32-byte words. For negative values, the upper
/// bytes are filled with `0xFF` (sign extension); for non-negative, `0x00`.
fn sign_extend_to_32_bytes(val: i64) -> [u8; 32] {
    let mut buf = [0u8; 32];
    if val < 0 {
        buf[..24].fill(0xFF);
    }
    buf[24..].copy_from_slice(&val.to_be_bytes());
    buf
}
