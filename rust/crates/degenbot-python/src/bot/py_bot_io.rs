//! `PyBotIo` — the Rust `#[pyclass]` I/O façade that pool builders receive in
//! place of the Python `SyncPoolIO` adapter (ADR-005 slice 14a).
//!
//! ## What it is
//!
//! Per slice 14, `Bot.build_pool`'s I/O choreography moves into Rust. Builders
//! receive `PyBotIo` as their `io` parameter; it exposes the 7-method
//! [`PoolIO`] surface (`get_block_number`, `get_block`, `get_block_timestamp`,
//! `get_code`, `get_balance`, `call`, `call_raw`) by routing through the core
//! [`ConstructionIo`] trait (ADR-023 D1/D3), plus ~50 `fetch_*` / `probe_*`
//! encode→call→decode choreography helpers that compose over the same trait's
//! `call` + DB primitives.
//!
//! ## Single construction-I/O path (ADR-023 D1/D3)
//!
//! Every RPC method resolves a [`ConstructionIo`] via `required_construction_io`:
//! the attached handle when a `Bot` attached one, else a transient
//! `(NoDb, AlloyRpcConstruction)` built over the held alloy provider (bare test
//! fixtures that construct `PyBotIo(provider=…)`). Non-alloy Python providers
//! (the retired web3/legacy-double fallback) **error loudly** — D1 removed the
//! Python `provider` delegation tier and D3 removed the inlined `self.alloy`
//! RPC duplication, leaving one code path per method. Non-integer block tags
//! (`"latest"`) likewise error loudly: the core `RpcConstruction` trait only
//! accepts `u64` blocks (tag support is a follow-on trait change — epic
//! `VK3YDM`).
//!
//! ## Async bridging
//!
//! Each call is `py.detach(|| get_runtime().block_on(...))` over the
//! process-global tokio runtime (`degenbot_core::runtime`) — detach the GIL,
//! block the runtime, reacquire. Never `block_on` while holding the GIL.
//!
//! [PoolIO]: degenbot/builders/pool_io.py
//! [ConstructionIo]: degenbot_bot::bot_core::construction_io

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyList};
use std::fmt::Write as _;
use std::sync::Arc;

use crate::provider::AlloyProvider;
use crate::rpc::provider::PyAlloyProvider;
use crate::rpc::revert_to_pyerr;
use degenbot_core::errors::ProviderError;
use degenbot_core::runtime::get_runtime;

/// Slot0 + liquidity state tuple returned by
/// [`PyBotIo::fetch_v4_slot0_liquidity`]:
/// `(sqrtPriceX96, tick, protocolFee, lpFee, liquidity)`.
type Slot0LiquidityState = (Py<PyAny>, Py<PyAny>, Py<PyAny>, Py<PyAny>, Py<PyAny>);

/// Lowercase `0x`-prefixed hex for an address (matches the builder's
/// lowercase-address convention for Balancer tokens/rate-providers).
fn address_lower_hex(a: alloy::primitives::Address) -> String {
    let mut hex = String::with_capacity(42);
    hex.push_str("0x");
    let _ = write!(hex, "{a:x}");
    hex
}

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
/// A typed ERC-20 token DB row returned by [`PyBotIo::fetch_erc20_token`]
/// (QVMWQC). Mirrors the `SQLAlchemy` `Erc20TokenTable` ORM object's
/// attributes (`.id` / `.chain` / `.address` / `.name` / `.symbol` /
/// `.decimals`) so the builder's downstream attribute reads stay unchanged
/// after the cutover from `session.scalar(select(Erc20TokenTable)...)`.
#[cfg(feature = "db")]
#[pyclass(name = "Erc20TokenRow", module = "degenbot._ffi")]
pub struct PyErc20TokenRow {
    id: i64,
    chain: i64,
    address: String,
    name: Option<String>,
    symbol: Option<String>,
    decimals: Option<i64>,
}

#[cfg(feature = "db")]
impl PyErc20TokenRow {
    fn from(row: degenbot_db::rows::Erc20TokenRow) -> Self {
        Self {
            id: row.id,
            chain: row.chain,
            address: row.address.to_checksum(None),
            name: row.name,
            symbol: row.symbol,
            decimals: row.decimals,
        }
    }
}

#[cfg(feature = "db")]
#[pymethods]
impl PyErc20TokenRow {
    #[getter]
    fn id(&self) -> i64 {
        self.id
    }

    #[getter]
    fn chain(&self) -> i64 {
        self.chain
    }

    #[getter]
    fn address(&self) -> String {
        self.address.clone()
    }

    #[getter]
    fn name(&self) -> Option<String> {
        self.name.clone()
    }

    #[getter]
    fn symbol(&self) -> Option<String> {
        self.symbol.clone()
    }

    #[getter]
    fn decimals(&self) -> Option<i64> {
        self.decimals
    }
}

/// The Rust `#[pyclass]` I/O façade pool builders receive in place of the
/// Python `SyncPoolIO` adapter — delegates the 7-method `PoolIO` surface to
/// the held Python `provider` (see module docs). Exposed to Python as
/// `degenbot._ffi.BotIo`.
#[pyclass(name = "BotIo", module = "degenbot._ffi")]
pub struct PyBotIo {
    /// Native Rust `AlloyProvider` extracted from the held Python provider
    /// when it is `PyAlloyProvider`-backed (live alloy or the O2 `OfflineProvider`
    /// shell). The `fetch_*` choreography methods run entirely in Rust via a
    /// core [`ConstructionIo`](crate::bot::py_bot_io::PyBotIo) handle (no GIL
    /// round-trip); `get_block_timestamp` derives from `get_block(n).header.timestamp`.
    /// `None` only for non-alloy Python providers (retired by O3).
    alloy: Option<Arc<AlloyProvider>>,
    db: Option<Py<PyAny>>,
    /// The on-disk `SQLite` database path (QVMWQC). Retained for the `getter`
    /// (Python introspection) + the `database_path` is now opened ONCE at
    /// `attach_construction_io` time into a held `DegenbotDbConstruction`; the
    /// 12 DB methods no longer per-call `DegenbotDb::open` from here.
    database_path: Option<String>,
    /// The core construction-I/O handle (architecture review 2025-07-18).
    /// `Some` after `PyBot.attach_construction_io` runs (the Python `Bot.__init__`
    /// path); the 12 DB + 7 generic RPC methods delegate through this. `None`
    /// for the bare test fixtures that construct `PyBotIo(provider=…)` without
    /// a `Bot` — those methods return the no-DB / error-degrade shape.
    /// Interior-mutable (`Mutex`) because `PyBotIo` is constructed before the
    /// `ConstructionIo` is attached (the `Bot.__init__` sequence: construct
    /// `PyBot` → attach I/O → construct `PyBotIo` → attach I/O to `PyBotIo`).
    construction_io:
        parking_lot::Mutex<Option<Arc<degenbot_bot::bot_core::construction_io::ConstructionIo>>>,
}

#[pymethods]
impl PyBotIo {
    /// Construct the I/O façade over an alloy-backed `ProviderAdapter`
    /// (+ optional DB).
    ///
    /// `provider` is the `ProviderAdapter` the `Bot` was constructed with; the
    /// held `Arc<AlloyProvider>` is extracted from it (live alloy or the
    /// offline shell) and powers a transient `(NoDb, AlloyRpcConstruction)`
    /// handle for the RPC methods. Non-alloy providers (the retired web3 /
    /// legacy-double fallback) yield `alloy = None` and every RPC + choreography
    /// method errors loudly (ADR-023 D1).
    ///
    /// `db`, when provided, is the `DatabaseSessionManager` handle; it's stored
    /// so the `PyBotIo` surface mirrors `BuilderContext`, and accessed via the
    /// [`db` getter][Self::db].
    ///
    /// `database_path` (QVMWQC) is the on-disk `SQLite` path; when set, the
    /// DB-query methods (`fetch_erc20_token`, `update_erc20_token_metadata`, …)
    /// open a `degenbot_db::DegenbotDb` handle from it + route the
    /// construction-time DB reads/writes through Rust (the `SQLAlchemy`
    /// `session.scalar(select(...))` / `session.commit()` bodies retire).
    #[new]
    #[expect(clippy::needless_pass_by_value)]
    #[pyo3(signature = (provider, db=None, database_path=None))]
    pub(crate) fn new(
        py: Python<'_>,
        provider: Py<PyAny>,
        db: Option<Py<PyAny>>,
        database_path: Option<String>,
    ) -> Self {
        // Extract a native Rust `AlloyProvider` when the held Python provider
        // is `PyAlloyProvider`-backed (live alloy or the offline shell); non-
        // alloy providers yield `None` and error loudly on use (ADR-023 D1).
        let alloy = extract_native_alloy(provider.bind(py));
        // Architecture review 2025-07-18: when `database_path` is set + the
        // provider is alloy-backed, eagerly construct a `ConstructionIo` from
        // them so the 12 DB methods delegate through the held connection (no
        // per-call `DegenbotDb::open`). A `PyBot`-attached I/O via
        // [`attach_construction_io`] replaces this at `Bot.__init__` time. The
        // bare `PyBotIo(provider=…, database_path=…)` test fixtures still work
        // standalone: `database_path` constructs a `DegenbotDbConstruction`,
        // the alloy provider constructs an `AlloyRpcConstruction`. Non-alloy +
        // no DB → `construction_io` stays `None` (the RPC methods then build
        // the transient alloy handle via `required_construction_io`, or error).
        let construction_io = match (&alloy, &database_path) {
            (Some(provider), Some(path)) => {
                let path_buf = std::path::PathBuf::from(path);
                // `open_for_writes` — the construction executor does reads AND
                // the `update_erc20_token_metadata` write-back, so the held
                // `DegenbotDbConstruction` connection must be write-capable
                // (the read-only `open` would reject the write-back).
                match py.detach(|| degenbot_db::DegenbotDb::open_for_writes(&path_buf)) {
                    Ok((db, _state)) => {
                        use degenbot_bot::bot_core::construction_io::{
                            AlloyRpcConstruction, ConstructionIo, DegenbotDbConstruction,
                        };
                        Some(std::sync::Arc::new(ConstructionIo::new(
                            std::sync::Arc::new(DegenbotDbConstruction::new(db)),
                            std::sync::Arc::new(AlloyRpcConstruction::new((**provider).clone())),
                        )))
                    }
                    // DB open failure (e.g. missing Alembic stamp on a
                    // write-cold fixture) — leave `None`; the methods return
                    // the no-DB shape (`attach_construction_io` can replace).
                    Err(_) => None,
                }
            }
            _ => None,
        };
        Self {
            alloy,
            db,
            database_path,
            construction_io: parking_lot::Mutex::new(construction_io),
        }
    }

    /// Attach the core `ConstructionIo` handle sourced from `PyBot` (architecture
    /// review 2025-07-18 / candidate 1). After this call the 12 DB + 7 generic
    /// RPC methods delegate through the trait objects; the 27 choreography
    /// wrappers stay unchanged. The Python `Bot.__init__` calls this right after
    /// `PyBot.attach_construction_io` so the two stay in lockstep.
    #[expect(clippy::unnecessary_wraps)]
    fn attach_construction_io(&self, py_bot: &Bound<'_, crate::bot::PyBot>) -> PyResult<()> {
        *self.construction_io.lock() = py_bot.borrow().bot.construction_io_arc();
        Ok(())
    }

    /// The held `DatabaseSessionManager`, if any (None when the `Bot` has no DB).
    #[getter]
    fn db(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        self.db.as_ref().map(|h| h.clone_ref(py))
    }

    /// The on-disk `SQLite` database path, if any (QVMWQC). The DB-query methods
    /// open a `degenbot_db::DegenbotDb` handle from this path; `None` when the
    /// `Bot` has no DB.
    #[getter]
    fn database_path(&self) -> Option<String> {
        self.database_path.clone()
    }

    /// Fetch an ERC-20 token row from the DB by `(chain_id, address)` — the
    /// construction-time read in `Erc20Builder.build` (QVMWQC). Replaces the
    /// `SQLAlchemy` `session.scalar(select(Erc20TokenTable).where(...))` call.
    ///
    /// Returns a [`PyErc20TokenRow`] with `(id, chain, address, name, symbol,
    /// decimals)`, or `None` when the row is absent (mirrors the Python
    /// `get_token_from_database` returning `None`). Returns `None` (no error)
    /// when no `database_path` is configured (the `Bot` has no DB — matches
    /// the prior `contextlib.suppress(Exception)` skip). DB failures propagate
    /// as `ValueError` (the Python caller wraps the read in
    /// `contextlib.suppress(Exception)`).
    #[pyo3(signature = (chain_id, address))]
    #[cfg(feature = "db")]
    fn fetch_erc20_token(
        &self,
        py: Python<'_>,
        chain_id: i64,
        address: &str,
    ) -> PyResult<Option<Py<PyErc20TokenRow>>> {
        let Some(io) = self.construction_io() else {
            return Ok(None);
        };
        let addr = parse_address_for_call(address)?;
        let row = py
            .detach(|| {
                get_runtime().block_on(async {
                    io.fetch_erc20_token(chain_id, alloy::primitives::Address::from(addr))
                        .await
                })
            })
            .map_err(|e| crate::db::db_err_to_py(&e))?
            .map(PyErc20TokenRow::from);
        match row {
            Some(r) => Ok(Some(Py::new(py, r)?)),
            None => Ok(None),
        }
    }

    /// Write back an ERC-20 token row's metadata (`name` / `symbol` / `decimals`)
    /// by `(chain_id, address)` — the construction-time write-back in
    /// `Erc20Builder.build` (QVMWQC). Replaces the `SQLAlchemy`
    /// `token_from_db.decimals = …; token_from_db.name = …;
    /// token_from_db.symbol = …; session.commit()` block. Each `None` field
    /// writes `NULL` (matches the ORM attribute assignment).
    ///
    /// No-op when no `database_path` is configured. DB failures propagate as
    /// `ValueError`. A no-row match is a benign no-op (the caller
    /// only reaches the write-back when the row was already fetched).
    #[pyo3(signature = (chain_id, address, name, symbol, decimals))]
    #[cfg(feature = "db")]
    fn update_erc20_token_metadata(
        &self,
        py: Python<'_>,
        chain_id: i64,
        address: &str,
        name: Option<&str>,
        symbol: Option<&str>,
        decimals: Option<i64>,
    ) -> PyResult<()> {
        let Some(io) = self.construction_io() else {
            return Ok(());
        };
        py.detach(|| {
            get_runtime().block_on(async {
                io.update_erc20_token_metadata(chain_id, address, name, symbol, decimals)
                    .await
            })
        })
        .map_err(|e| crate::db::db_err_to_py(&e))
    }

    /// Fetch a `pools` row by `(chain_id, address)` — the pool builder's
    /// construction-time read (QVMWQC). Replaces the `SQLAlchemy`
    /// `session.scalar(select(LiquidityPoolTable).where(...))`. Returns a
    /// [`PyLiquidityPoolRow`] carrying the scalar + FK-id columns
    /// (`exchange_id` / `token0_id` / `token1_id` / `kind`); the caller hydrates
    /// the relationships via [`Self::fetch_pool_kind`] / [`Self::fetch_token_by_id`]
    /// / [`Self::fetch_exchange`].
    ///
    /// `None` when the row is absent or no `database_path` is configured (mirrors
    /// the prior `contextlib.suppress(Exception)` skip).
    #[pyo3(signature = (chain_id, address))]
    #[cfg(feature = "db")]
    fn fetch_pool_row(
        &self,
        py: Python<'_>,
        chain_id: i64,
        address: &str,
    ) -> PyResult<Option<Py<crate::db::PyLiquidityPoolRow>>> {
        let Some(io) = self.construction_io() else {
            return Ok(None);
        };
        let addr = parse_address_for_call(address)?;
        let row = py
            .detach(|| {
                get_runtime().block_on(async {
                    io.fetch_pool_row(chain_id, alloy::primitives::Address::from(addr))
                        .await
                })
            })
            .map_err(|e| crate::db::db_err_to_py(&e))?
            .map(crate::db::PyLiquidityPoolRow::from);
        match row {
            Some(r) => Ok(Some(Py::new(py, r)?)),
            None => Ok(None),
        }
    }

    /// Fetch an `exchanges` row by its FK id (QVMWQC) — hydrates the
    /// `pool.exchange` relationship (`factory` / `deployer`). `None` when absent
    /// or no path.
    #[pyo3(signature = (exchange_id))]
    #[cfg(feature = "db")]
    fn fetch_exchange(
        &self,
        py: Python<'_>,
        exchange_id: i64,
    ) -> PyResult<Option<Py<crate::db::PyExchangeRow>>> {
        let Some(io) = self.construction_io() else {
            return Ok(None);
        };
        let row = py
            .detach(|| get_runtime().block_on(async { io.fetch_exchange(exchange_id).await }))
            .map_err(|e| crate::db::db_err_to_py(&e))?
            .map(crate::db::PyExchangeRow::from);
        match row {
            Some(r) => Ok(Some(Py::new(py, r)?)),
            None => Ok(None),
        }
    }

    /// Return the current block number. Native path: direct `AlloyProvider`
    /// call (no GIL round-trip). Fallback: delegates to the held Python
    /// provider's `get_block_number()` for non-alloy providers.
    fn get_block_number(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        // Single construction-I/O path (ADR-023 D1/D3): route through the core
        // trait. `required_construction_io` returns the attached handle, else a
        // transient `(NoDb, AlloyRpcConstruction)` over the held alloy provider,
        // else errors loudly for non-alloy providers (the retired Python
        // fallback).
        let io = self.required_construction_io()?;
        let n = py
            .detach(|| get_runtime().block_on(async { io.get_block_number().await }))
            .map_err(Into::<PyErr>::into)?;
        crate::conversion::alloy::u256_to_py(py, &alloy::primitives::U256::from(n))
            .map(pyo3::Bound::into_any)
            .map(pyo3::Bound::unbind)
    }

    /// Return the code at the given address. Native path: direct
    /// `AlloyProvider.get_code(addr, block)`. Fallback: delegates to
    /// `provider.get_code(address, block=block)` (mirrors `SyncPoolIO`).
    #[pyo3(signature = (address, block=None))]
    fn get_code(
        &self,
        py: Python<'_>,
        address: &Bound<'_, PyAny>,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        // Single construction-I/O path (ADR-023 D1/D3): extract address + block,
        // route through the core trait via `required_construction_io`. No
        // Python fallback — non-native shapes error loudly.
        let addr = alloy::primitives::Address::from(parse_address_for_call(
            &address.extract::<String>()?,
        )?);
        let block_num = extract_block_u64(block)?;
        let io = self.required_construction_io()?;
        let code = py
            .detach(|| get_runtime().block_on(async { io.get_code(addr, block_num).await }))
            .map_err(Into::<PyErr>::into)?;
        crate::conversion::cache::create_hexbytes(py, code.as_ref())
            .map(pyo3::Bound::into_any)
            .map(pyo3::Bound::unbind)
    }

    /// Return the ETH balance at the given address. Native path: direct
    /// `AlloyProvider.get_balance(addr, block)`. Fallback: delegates to
    /// `provider.get_balance(address, block=block)` (mirrors `SyncPoolIO`).
    #[pyo3(signature = (address, block=None))]
    fn get_balance(
        &self,
        py: Python<'_>,
        address: &Bound<'_, PyAny>,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        // Single construction-I/O path (ADR-023 D1/D3).
        let addr = alloy::primitives::Address::from(parse_address_for_call(
            &address.extract::<String>()?,
        )?);
        let block_num = extract_block_u64(block)?;
        let io = self.required_construction_io()?;
        let balance = py
            .detach(|| get_runtime().block_on(async { io.get_balance(addr, block_num).await }))
            .map_err(Into::<PyErr>::into)?;
        crate::conversion::alloy::u256_to_py(py, &balance)
            .map(pyo3::Bound::into_any)
            .map(pyo3::Bound::unbind)
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
    fn fetch_factory_address(&self, py: Python<'_>, address: &str) -> Option<String> {
        // Delegate the encode->call->decode->checksum to the core choreography
        // (14b), then swallow any error into `None` to preserve the
        // `None`-on-failure contract this method promises (mirrors the Python
        // `except (Web3Exception, DecodingError): return None`).
        let Ok(io) = self.required_construction_io() else {
            return None;
        };
        let Ok(addr) = parse_address_for_call(address) else {
            return None;
        };
        let addr = alloy::primitives::Address::from(addr);
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::choreography::fetch_factory_address(
                    &io, addr, None,
                )
                .await
            })
        });
        match r {
            Ok(a) => Some(a.to_checksum(None)),
            Err(_) => None,
        }
    }

    /// Fetch ERC-20 `name()` / `symbol()` / `decimals()` for MANY tokens in ONE
    /// Multicall3 `aggregate3` `eth_call` (CDJEPJ-2), falling back to the
    /// per-token `fetch_erc20_metadata` path if the multicall itself errors.
    ///
    /// Returns one `Option<(name, symbol, decimals)>` per input address, in
    /// order. A token whose sub-call reverted / failed to decode is `None`,
    /// matching the single-token batched-fetch caller-side fallback contract
    /// (the Python builder then retries that token via its alternate-prototype
    /// fallback). Collapses the two separate per-token `fetch_erc20_metadata`
    /// round-trips a two-token pool build used to fire into ONE multicall.
    #[pyo3(signature = (addresses))]
    fn fetch_erc20_metadata_batch(
        &self,
        py: Python<'_>,
        addresses: Vec<String>,
    ) -> Vec<Option<(String, String, u64)>> {
        let Ok(io) = self.required_construction_io() else {
            return Vec::new();
        };
        let mut addrs = Vec::with_capacity(addresses.len());
        for a in addresses {
            let Ok(addr) = parse_address_for_call(&a) else {
                return Vec::new();
            };
            addrs.push(alloy::primitives::Address::from(addr));
        }
        py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::choreography::fetch_erc20_metadata_batch(
                    &io, &addrs,
                )
                .await
            })
        })
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
        let io = self.required_construction_io()?;
        let addr = alloy::primitives::Address::from(parse_address_for_call(pool_address)?);
        let block_num = extract_block_u64(block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::choreography::fetch_v2_reserves(
                    &io, addr, block_num,
                )
                .await
            })
        });
        let (r0, r1) = match r {
            Ok(v) => Ok(v),
            Err(ProviderError::ExecutionReverted { message, .. }) => {
                Err(revert_to_pyerr(py, pool_address, &message))
            }
            Err(e) => Err(e.into()),
        }?;
        Ok((
            crate::conversion::alloy::u256_to_py(py, &r0)?.unbind(),
            crate::conversion::alloy::u256_to_py(py, &r1)?.unbind(),
        ))
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
        let io = self.required_construction_io()?;
        let addr = alloy::primitives::Address::from(parse_address_for_call(pool_address)?);
        let block_num = extract_block_u64(block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::choreography::fetch_v3_slot0_liquidity(
                    &io, addr, block_num,
                )
                .await
            })
        });
        let (sqrt, tick, liq) = match r {
            Ok(v) => Ok(v),
            Err(ProviderError::ExecutionReverted { message, .. }) => {
                Err(revert_to_pyerr(py, pool_address, &message))
            }
            Err(e) => Err(e.into()),
        }?;
        Ok((
            crate::conversion::alloy::u256_to_py(py, &sqrt)?.unbind(),
            crate::conversion::alloy::i256_to_py(py, &tick)?.unbind(),
            crate::conversion::alloy::u256_to_py(py, &liq)?.unbind(),
        ))
    }

    /// Fetch a V4 pool's slot0 + liquidity via `getSlot0(bytes32)` +
    /// `getLiquidity(bytes32)` on the state-view contract (ADR-005 slice 14o).
    ///
    /// Mirrors `degenbot/builders/v4_pool_builder.py`'s slot0/liquidity RPC
    /// block in `_build_pool`. V4 differs from V3 (slice 14f) in two ways:
    /// 1. Methods take a `bytes32 pool_id` prefix argument (like the V4 tick
    ///    RPCs from slices 14j/14k).
    /// 2. `getSlot0` returns `(uint160 sqrtPriceX96, int24 tick, uint24
    ///    protocolFee, uint24 lpFee)` — 4 fields, not 6. The protocolFee word
    ///    packs two uint12 fees; we return the raw `uint24` and leave that
    ///    interpretation to Python's `decode_slot0` callers.
    ///
    /// Returns a 5-tuple `(sqrtPriceX96, tick, protocolFee, lpFee, liquidity)`.
    ///
    /// Errors propagate.
    #[pyo3(signature = (state_view_address, pool_id, block=None))]
    fn fetch_v4_slot0_liquidity(
        &self,
        py: Python<'_>,
        state_view_address: &str,
        pool_id: &[u8],
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Slot0LiquidityState> {
        let io = self.required_construction_io()?;
        let state_view =
            alloy::primitives::Address::from(parse_address_for_call(state_view_address)?);
        let pool_id_arr: [u8; 32] = pool_id
            .try_into()
            .map_err(|_| pyo3::exceptions::PyValueError::new_err("pool_id must be 32 bytes"))?;
        let block_num = extract_block_u64(block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::choreography::fetch_v4_slot0_liquidity(
                    &io,
                    state_view,
                    pool_id_arr,
                    block_num,
                )
                .await
            })
        });
        let (sqrt, tick, protocol_fee, lp_fee, liq) = match r {
            Ok(v) => Ok(v),
            Err(ProviderError::ExecutionReverted { message, .. }) => {
                Err(revert_to_pyerr(py, state_view_address, &message))
            }
            Err(e) => Err(e.into()),
        }?;
        Ok((
            crate::conversion::alloy::u256_to_py(py, &sqrt)?.unbind(),
            crate::conversion::alloy::i256_to_py(py, &tick)?.unbind(),
            crate::conversion::alloy::u256_to_py(py, &protocol_fee)?.unbind(),
            crate::conversion::alloy::u256_to_py(py, &lp_fee)?.unbind(),
            crate::conversion::alloy::u256_to_py(py, &liq)?.unbind(),
        ))
    }

    /// Fetch all Curve pool balances via `balances(uint256)` in a loop
    /// (ADR-005 slice 14s).
    ///
    /// Mirrors `curve_pool_builder.py::_fetch_balances`'s snapshot-update loop.
    /// Issues `count` RPCs, indexing 0..count, gathering `uint256` results into
    /// a Python list. New pattern: unsigned-integer-arg encoding (the index is
    /// ABI-encoded as a 32-byte big-endian `uint256` word).
    ///
    /// Errors propagate.
    #[pyo3(signature = (pool_address, count, block=None))]
    fn fetch_curve_balances(
        &self,
        py: Python<'_>,
        pool_address: &str,
        count: usize,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let io = self.required_construction_io()?;
        let pool = alloy::primitives::Address::from(parse_address_for_call(pool_address)?);
        let block_num = extract_block_u64(block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::curve_choreography::fetch_curve_balances(
                    &io, pool, count, block_num,
                )
                .await
            })
        });
        let balances = match r {
            Ok(v) => Ok(v),
            Err(ProviderError::ExecutionReverted { message, .. }) => {
                Err(revert_to_pyerr(py, pool_address, &message))
            }
            Err(e) => Err(e.into()),
        }?;
        let list = PyList::empty(py);
        for b in balances {
            list.append(crate::conversion::alloy::u256_to_py(py, &b)?.unbind())?;
        }
        Ok(list.into())
    }

    /// Fetch an ERC-20 token balance via `balanceOf(address)`, performing the
    /// full encode -> call -> decode choreography in Rust (ADR-005 slice 14d).
    ///
    /// Mirrors `degenbot/builders/erc20_builder.py::Erc20Builder.get_token_balance`'s
    /// I/O call path (cache + checksum are out of scope; the caller still owns
    /// those). The `balanceOf(address)` selector (`0x70a08231`) + ABI-encoded
    /// `address` arg and the `uint256` return decode are sourced from the
    /// `sol!`-generated definitions in `degenbot_rpc::abi` (B2).
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
        let io = self.required_construction_io()?;
        let token_addr = alloy::primitives::Address::from(parse_address_for_call(token)?);
        let owner_addr = alloy::primitives::Address::from(parse_address_for_call(owner)?);
        let block_num = extract_block_u64(block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::choreography::fetch_token_balance(
                    &io, token_addr, owner_addr, block_num,
                )
                .await
            })
        });
        let n = match r {
            Ok(v) => v,
            Err(ProviderError::ExecutionReverted { message, .. }) => {
                return Err(revert_to_pyerr(py, token, &message));
            }
            Err(e) => return Err(e.into()),
        };
        crate::conversion::alloy::u256_to_py(py, &n).map(pyo3::Bound::unbind)
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
        let io = self.required_construction_io()?;
        let token_addr = alloy::primitives::Address::from(parse_address_for_call(token)?);
        let owner_addr = alloy::primitives::Address::from(parse_address_for_call(owner)?);
        let spender_addr = alloy::primitives::Address::from(parse_address_for_call(spender)?);
        let block_num = extract_block_u64(block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::choreography::fetch_token_allowance(
                    &io,
                    token_addr,
                    owner_addr,
                    spender_addr,
                    block_num,
                )
                .await
            })
        });
        let n = match r {
            Ok(v) => v,
            Err(ProviderError::ExecutionReverted { message, .. }) => {
                return Err(revert_to_pyerr(py, token, &message));
            }
            Err(e) => return Err(e.into()),
        };
        crate::conversion::alloy::u256_to_py(py, &n).map(pyo3::Bound::unbind)
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
        let io = self.required_construction_io()?;
        let token_addr = alloy::primitives::Address::from(parse_address_for_call(token)?);
        let block_num = extract_block_u64(block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::choreography::fetch_token_total_supply(
                    &io, token_addr, block_num,
                )
                .await
            })
        });
        let n = match r {
            Ok(v) => v,
            Err(ProviderError::ExecutionReverted { message, .. }) => {
                return Err(revert_to_pyerr(py, token, &message));
            }
            Err(e) => return Err(e.into()),
        };
        crate::conversion::alloy::u256_to_py(py, &n).map(pyo3::Bound::unbind)
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
    ) -> String {
        use degenbot_bot::bot_core::pool_builder::builder::PoolFamily;

        // Non-`PyResult` signature (mirrors the Python `-> str` contract), so
        // the construction-IO / address / block conversions `expect`: for the
        // alloy-backed Offline/RPC providers the builder dispatches through,
        // all three succeed.
        #[expect(clippy::expect_used)] // invariant-guarded (documented)
        let io = self
            .required_construction_io()
            .expect("probe_pool_type requires the alloy-backed construction IO");
        let addr = alloy::primitives::Address::from(
            #[expect(clippy::expect_used)] // invariant-guarded (documented)
            parse_address_for_call(address).expect("probe_pool_type address parse"),
        );
        #[expect(clippy::expect_used)] // invariant-guarded (documented)
        let block_num = extract_block_u64(block).expect("probe_pool_type block parse");
        let family = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::builder::probe_pool_type(&io, addr, block_num)
                    .await
            })
        });
        match family {
            PoolFamily::V3 => "slot0".to_string(),
            PoolFamily::V2 => "getReserves".to_string(),
            PoolFamily::BalancerWeighted => "balancer_weighted".to_string(),
            PoolFamily::BalancerStable => "balancer_stable".to_string(),
            PoolFamily::Curve => "stableswap".to_string(),
        }
    }

    /// Fetch a V3 pool's tick bitmap word via `tickBitmap(int16)`, performing
    /// the encode -> call -> decode choreography in Rust (ADR-005 slice 14j).
    ///
    /// Calldata + return decode from `degenbot_rpc::abi` (B2) — no hand-rolled
    /// `selector` / `sign_extend_to_32_bytes` here. The encode/decode helpers
    /// are shared with `AlloyTickBootstrapRpc` (the standalone-Rust `cargo add
    /// degenbot` consumer path), so the choreography stays byte-identical
    /// across the pyo3 adapter and the alloy impl (5NT2OC epic / Y5MHJV).
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
        let word_i16 = i16::try_from(word_position).map_err(|_| {
            pyo3::exceptions::PyValueError::new_err("word_position out of int16 range")
        })?;
        let io = self.required_construction_io()?;
        let addr = alloy::primitives::Address::from(parse_address_for_call(pool_address)?);
        let block_num = extract_block_u64(block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::choreography::fetch_tick_bitmap(
                    &io, addr, word_i16, block_num,
                )
                .await
            })
        });
        let bitmap = match r {
            Ok(v) => Ok(v),
            Err(ProviderError::ExecutionReverted { message, .. }) => {
                Err(revert_to_pyerr(py, pool_address, &message))
            }
            Err(e) => Err(e.into()),
        }?;
        crate::conversion::alloy::u256_to_py(py, &bitmap).map(pyo3::Bound::unbind)
    }

    /// Fetch a V3 pool's tick liquidity data via `ticks(int24)`, performing
    /// the encode -> call -> decode choreography in Rust (ADR-005 slice 14j).
    ///
    /// Calldata + return decode from `degenbot_rpc::abi` (B2). The result's
    /// first two fields (`liquidity_gross: uint128`, `liquidity_net: int128`)
    /// are right-aligned in their 32-byte ABI words; `decode_tick_data` reads
    /// those 64 bytes directly.
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
        let tick_i32 = i32::try_from(tick)
            .map_err(|_| pyo3::exceptions::PyValueError::new_err("tick out of int24 range"))?;
        let io = self.required_construction_io()?;
        let addr = alloy::primitives::Address::from(parse_address_for_call(pool_address)?);
        let block_num = extract_block_u64(block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::choreography::fetch_tick_data(
                    &io, addr, tick_i32, block_num,
                )
                .await
            })
        });
        let (gross, net) = match r {
            Ok(v) => Ok(v),
            Err(ProviderError::ExecutionReverted { message, .. }) => {
                Err(revert_to_pyerr(py, pool_address, &message))
            }
            Err(e) => Err(e.into()),
        }?;
        Ok((
            crate::conversion::alloy::u256_to_py(py, &alloy::primitives::U256::from(gross))?
                .unbind(),
            crate::conversion::alloy::i256_to_py(py, &net)?.unbind(),
        ))
    }

    /// Fetch a V4 pool's tick bitmap via `getTickBitmap(bytes32,int16)` on the
    /// state-view contract (ADR-005 slice 14k).
    ///
    /// Calldata + return decode from `degenbot_rpc::abi` (B2). V4 adds a
    /// `pool_id` (`bytes32`) prefix argument before the `int16` word position.
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
        let word_i16 = i16::try_from(word_position).map_err(|_| {
            pyo3::exceptions::PyValueError::new_err("word_position out of int16 range")
        })?;
        let pool_id_arr: [u8; 32] = pool_id
            .try_into()
            .map_err(|_| pyo3::exceptions::PyValueError::new_err("pool_id must be 32 bytes"))?;
        let io = self.required_construction_io()?;
        let state_view =
            alloy::primitives::Address::from(parse_address_for_call(state_view_address)?);
        let block_num = extract_block_u64(block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::choreography::fetch_v4_tick_bitmap(
                    &io,
                    state_view,
                    pool_id_arr,
                    word_i16,
                    block_num,
                )
                .await
            })
        });
        let bitmap = match r {
            Ok(v) => Ok(v),
            Err(ProviderError::ExecutionReverted { message, .. }) => {
                Err(revert_to_pyerr(py, state_view_address, &message))
            }
            Err(e) => Err(e.into()),
        }?;
        crate::conversion::alloy::u256_to_py(py, &bitmap).map(pyo3::Bound::unbind)
    }

    /// Fetch a V4 pool's tick liquidity via `getTickLiquidity(bytes32,int24)`
    /// on the state-view contract (ADR-005 slice 14k).
    ///
    /// Calldata + return decode from `degenbot_rpc::abi` (B2). V4 adds a
    /// `pool_id` (`bytes32`) prefix argument before the `int24` tick. The V4
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
        let tick_i32 = i32::try_from(tick)
            .map_err(|_| pyo3::exceptions::PyValueError::new_err("tick out of int24 range"))?;
        let pool_id_arr: [u8; 32] = pool_id
            .try_into()
            .map_err(|_| pyo3::exceptions::PyValueError::new_err("pool_id must be 32 bytes"))?;
        let io = self.required_construction_io()?;
        let state_view =
            alloy::primitives::Address::from(parse_address_for_call(state_view_address)?);
        let block_num = extract_block_u64(block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::choreography::fetch_v4_tick_data(
                    &io,
                    state_view,
                    pool_id_arr,
                    tick_i32,
                    block_num,
                )
                .await
            })
        });
        let (gross, net) = match r {
            Ok(v) => Ok(v),
            Err(ProviderError::ExecutionReverted { message, .. }) => {
                Err(revert_to_pyerr(py, state_view_address, &message))
            }
            Err(e) => Err(e.into()),
        }?;
        Ok((
            crate::conversion::alloy::u256_to_py(py, &alloy::primitives::U256::from(gross))?
                .unbind(),
            crate::conversion::alloy::i256_to_py(py, &net)?.unbind(),
        ))
    }

    /// Fetch a Balancer pool's `pool_id` via `getPoolId()` (ADR-005 slice 14l).
    ///
    /// Mirrors `balancer_builder_base.py::_fetch_pool_id`. Returns the raw
    /// 32-byte `bytes32` value (no decode — bytes32 is already 32 bytes).
    ///
    /// Errors propagate.
    #[pyo3(signature = (address, block=None))]
    fn fetch_balancer_pool_id(
        &self,
        py: Python<'_>,
        address: &str,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let io = self.required_construction_io()?;
        let addr = alloy::primitives::Address::from(parse_address_for_call(address)?);
        let block_num = extract_block_u64(block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::choreography::fetch_balancer_pool_id(
                    &io, addr, block_num,
                )
                .await
            })
        });
        let id = match r {
            Ok(v) => Ok(v),
            Err(ProviderError::ExecutionReverted { message, .. }) => {
                Err(revert_to_pyerr(py, address, &message))
            }
            Err(e) => Err(e.into()),
        }?;
        Ok(PyBytes::new(py, &id).into())
    }

    /// Fetch a Balancer pool's swap fee via `getSwapFeePercentage()`
    /// (ADR-005 slice 14l).
    ///
    /// Mirrors `balancer_builder_base.py::_fetch_swap_fee`. Returns `uint256`.
    ///
    /// Errors propagate.
    #[pyo3(signature = (address, block=None))]
    fn fetch_balancer_swap_fee(
        &self,
        py: Python<'_>,
        address: &str,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let io = self.required_construction_io()?;
        let addr = alloy::primitives::Address::from(parse_address_for_call(address)?);
        let block_num = extract_block_u64(block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::choreography::fetch_balancer_swap_fee(
                    &io, addr, block_num,
                )
                .await
            })
        });
        let val = match r {
            Ok(v) => Ok(v),
            Err(ProviderError::ExecutionReverted { message, .. }) => {
                Err(revert_to_pyerr(py, address, &message))
            }
            Err(e) => Err(e.into()),
        }?;
        crate::conversion::alloy::u256_to_py(py, &val).map(pyo3::Bound::unbind)
    }

    /// Fetch a Balancer pool's amplification parameter via
    /// `getAmplificationParameter()` (ADR-005 slice 14l).
    ///
    /// Mirrors `balancer_builder_base.py::_fetch_amp`. The full struct is
    /// `(uint256, bool, uint256)`; we only need the first word (amp value).
    ///
    /// Errors propagate.
    #[pyo3(signature = (address, block=None))]
    fn fetch_balancer_amp(
        &self,
        py: Python<'_>,
        address: &str,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let io = self.required_construction_io()?;
        let addr = alloy::primitives::Address::from(parse_address_for_call(address)?);
        let block_num = extract_block_u64(block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::choreography::fetch_balancer_amp(
                    &io, addr, block_num,
                )
                .await
            })
        });
        let val = match r {
            Ok(v) => Ok(v),
            Err(ProviderError::ExecutionReverted { message, .. }) => {
                Err(revert_to_pyerr(py, address, &message))
            }
            Err(e) => Err(e.into()),
        }?;
        crate::conversion::alloy::u256_to_py(py, &val).map(pyo3::Bound::unbind)
    }

    /// Fetch a Balancer weighted pool's normalized weights via
    /// `getNormalizedWeights()` (ADR-005 slice 14l).
    ///
    /// Mirrors `balancer_builder_base.py::_fetch_weights`. Returns `uint256[]`
    /// as a Python list of ints. New decode pattern: ABI-encoded dynamic array
    /// = offset (32) + length (32) + N 32-byte elements.
    ///
    /// Errors propagate.
    #[pyo3(signature = (address, block=None))]
    fn fetch_balancer_weights(
        &self,
        py: Python<'_>,
        address: &str,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let io = self.required_construction_io()?;
        let addr = alloy::primitives::Address::from(parse_address_for_call(address)?);
        let block_num = extract_block_u64(block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::choreography::fetch_balancer_weights(
                    &io, addr, block_num,
                )
                .await
            })
        });
        let weights = match r {
            Ok(v) => Ok(v),
            Err(ProviderError::ExecutionReverted { message, .. }) => {
                Err(revert_to_pyerr(py, address, &message))
            }
            Err(e) => Err(e.into()),
        }?;
        let list = PyList::empty(py);
        for w in weights {
            list.append(crate::conversion::alloy::u256_to_py(py, &w)?.unbind())?;
        }
        Ok(list.into())
    }

    /// Fetch a Balancer pool's rate providers via `getRateProviders()`
    /// (ADR-005 slice 14l).
    ///
    /// Mirrors `balancer_builder_base.py::_fetch_rate_providers`. Returns
    /// `address[]` as a Python list of lowercase hex strings. Reverts propagate
    /// (the Python caller's `except (Web3Exception, DecodingError): return []`
    /// handles `WeightedPool2Tokens` / `MetaStablePools` that don't expose this).
    ///
    /// Errors propagate.
    #[pyo3(signature = (address, block=None))]
    fn fetch_balancer_rate_providers(
        &self,
        py: Python<'_>,
        address: &str,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let io = self.required_construction_io()?;
        let addr = alloy::primitives::Address::from(parse_address_for_call(address)?);
        let block_num = extract_block_u64(block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::choreography::fetch_balancer_rate_providers(
                    &io, addr, block_num,
                )
                .await
            })
        });
        let providers = match r {
            Ok(v) => Ok(v),
            Err(ProviderError::ExecutionReverted { message, .. }) => {
                // Callers mirror the Python `except (RpcError, AbiDecodeError):
                // return []` clause (WeightedPool2Tokens / MetaStable pools that
                // don't expose getRateProviders revert).
                Err(revert_to_pyerr(py, address, &message))
            }
            Err(e) => Err(e.into()),
        }?;
        let list = PyList::empty(py);
        for p in providers {
            list.append(address_lower_hex(p))?;
        }
        Ok(list.into())
    }

    /// Fetch a Balancer vault's pool tokens + balances via `getPoolTokens(
    /// bytes32)` on the vault contract (ADR-005 slice 14m).
    ///
    /// Mirrors `balancer_builder_base.py::_fetch_vault_tokens`. The hardest
    /// Balancer decode: an ABI-encoded tuple `(address[], uint256[], uint256)`
    /// — a tuple with TWO nested dynamic arrays and one static field. We only
    /// return the first two members (tokens, balances); the third (last
    /// block number) is dropped because the Python caller ignores it.
    ///
    /// Uses alloy's `DynSolType::Tuple` decoder for the full tuple, then walks
    /// the decoded `DynSolValue::Tuple` to extract the two arrays.
    ///
    /// Errors propagate.
    #[pyo3(signature = (vault_address, pool_id, block=None))]
    fn fetch_balancer_vault_tokens(
        &self,
        py: Python<'_>,
        vault_address: &str,
        pool_id: &[u8],
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<(Py<PyAny>, Py<PyAny>)> {
        let io = self.required_construction_io()?;
        let vault = alloy::primitives::Address::from(parse_address_for_call(vault_address)?);
        let pool_id: [u8; 32] = pool_id
            .try_into()
            .map_err(|_| pyo3::exceptions::PyValueError::new_err("pool_id must be 32 bytes"))?;
        let block_num = extract_block_u64(block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::choreography::fetch_balancer_vault_tokens(
                    &io, vault, &pool_id, block_num,
                )
                .await
            })
        });
        let (tokens, balances) = match r {
            Ok(v) => Ok(v),
            Err(ProviderError::ExecutionReverted { message, .. }) => {
                Err(revert_to_pyerr(py, vault_address, &message))
            }
            Err(e) => Err(e.into()),
        }?;
        let tokens_list = PyList::empty(py);
        for t in tokens {
            tokens_list.append(address_lower_hex(t))?;
        }
        let balances_list = PyList::empty(py);
        for b in balances {
            balances_list.append(crate::conversion::alloy::u256_to_py(py, &b)?.unbind())?;
        }
        Ok((
            tokens_list.into_any().unbind(),
            balances_list.into_any().unbind(),
        ))
    }

    /// Fetch a single Balancer rate provider's rate via `getRate()` (ADR-005
    /// slice 14n).
    ///
    /// Mirrors the per-provider loop body of `balancer_builder_base.py::
    /// _fetch_rates`. Each rate provider exposes a `getRate()` no-arg call
    /// returning `uint256`. The sentinel check (zero address → `ONE`) stays
    /// Python-side.
    ///
    /// Errors propagate.
    #[pyo3(signature = (provider_address, block=None))]
    fn fetch_balancer_rate(
        &self,
        py: Python<'_>,
        provider_address: &str,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let io = self.required_construction_io()?;
        let provider = alloy::primitives::Address::from(parse_address_for_call(provider_address)?);
        let block_num = extract_block_u64(block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::choreography::fetch_balancer_rate(
                    &io, provider, block_num,
                )
                .await
            })
        });
        let val = match r {
            Ok(v) => Ok(v),
            Err(ProviderError::ExecutionReverted { message, .. }) => {
                Err(revert_to_pyerr(py, provider_address, &message))
            }
            Err(e) => Err(e.into()),
        }?;
        crate::conversion::alloy::u256_to_py(py, &val).map(pyo3::Bound::unbind)
    }

    /// Probe a Balancer pool's sub-type by trying `getNormalizedWeights()` and
    /// `getAmplificationParameter()` in order (ADR-005 slice 14n).
    ///
    /// Mirrors `balancer_builder_base.py::_detect_pool_type`. By the time this
    /// runs, the pool is already known to be Balancer (via `probe_pool_type`,
    /// 14i). This sub-probe distinguishes weighted vs stable.
    ///
    /// Returns:
    /// - `"weighted"` — `getNormalizedWeights()` succeeded.
    /// - `"stable"` — `getAmplificationParameter()` succeeded (and
    ///   getNormalizedWeights reverted).
    ///
    /// Errors:
    /// - `PyValueError` — neither method responded (raises so the Python
    ///   caller can wrap as `DegenbotValueError`; linear pools unsupported).
    #[pyo3(signature = (address, block=None))]
    fn probe_balancer_pool_type(
        &self,
        py: Python<'_>,
        address: &str,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<String> {
        use degenbot_bot::bot_core::pool_builder::choreography::BalancerFamily;

        let io = self.required_construction_io()?;
        let addr = alloy::primitives::Address::from(parse_address_for_call(address)?);
        let block_num = extract_block_u64(block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::pool_builder::choreography::probe_balancer_type(
                    &io, addr, block_num,
                )
                .await
            })
        });
        let family = match r {
            Ok(v) => Ok(v),
            // Neither probe responded → surface as ValueError so the Python
            // caller (`balancer_builder_base._detect_pool_type`) can re-wrap as
            // DegenbotValueError (linear pools unsupported). The core returns
            // DecodingError, whose default PyErr mapping is RuntimeError.
            Err(ProviderError::DecodingError { .. }) => {
                Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "Cannot determine Balancer pool type for {address}."
                )))
            }
            Err(ProviderError::ExecutionReverted { message, .. }) => {
                Err(revert_to_pyerr(py, address, &message))
            }
            Err(e) => Err(e.into()),
        }?;
        Ok(match family {
            BalancerFamily::Weighted => "weighted".to_string(),
            BalancerFamily::Stable => "stable".to_string(),
        })
    }

    fn __repr__(&self) -> String {
        let has_db = self.db.is_some();
        format!("BotIo(alloy={}, db={has_db})", self.alloy.is_some())
    }
}

impl PyBotIo {
    /// The native Rust `AlloyProvider`, if the held Python provider is
    /// `PyAlloyProvider`-backed (live alloy or the offline shell). `None` for
    /// non-alloy providers (legacy test doubles).
    ///
    /// Used by the Chain-arm wiring (5NT2OC / NOD4PS) to construct an
    /// [`AlloyTickBootstrapRpc`] without a GIL round-trip per RPC call — the
    /// pure-Rust impl owns the tick-bitmap + tick-data choreography directly.
    #[must_use]
    pub(crate) fn alloy_provider(&self) -> Option<std::sync::Arc<AlloyProvider>> {
        self.alloy.clone()
    }

    /// Borrow the attached `ConstructionIo` (or `None`). The 12 DB + 7 RPC
    /// methods use this to delegate through the core trait objects; returns
    /// `None` for bare test fixtures that construct `PyBotIo(provider=…)` without
    /// a `Bot` (the methods then degrade to the no-DB / error shape).
    #[must_use]
    fn construction_io(
        &self,
    ) -> Option<std::sync::Arc<degenbot_bot::bot_core::construction_io::ConstructionIo>> {
        self.construction_io.lock().clone()
    }

    /// Resolve a [`ConstructionIo`] for the choreography adapters: the attached
    /// handle when present, else a transient `(NoDb, AlloyRpcConstruction)`
    /// built over the held alloy provider (bare test fixtures that construct
    /// `PyBotIo(provider=…)` directly). Non-alloy Python providers (the legacy
    /// fallback the builder-choreography port retires) error here.
    fn required_construction_io(
        &self,
    ) -> PyResult<std::sync::Arc<degenbot_bot::bot_core::construction_io::ConstructionIo>> {
        use degenbot_bot::bot_core::construction_io::{AlloyRpcConstruction, ConstructionIo, NoDb};
        if let Some(io) = self.construction_io() {
            return Ok(io);
        }
        match &self.alloy {
            Some(alloy) => Ok(std::sync::Arc::new(ConstructionIo::new(
                std::sync::Arc::new(NoDb),
                std::sync::Arc::new(AlloyRpcConstruction::new((**alloy).clone())),
            ))),
            None => Err(pyo3::exceptions::PyRuntimeError::new_err(
                "choreography requires a core ConstructionIo (alloy provider)",
            )),
        }
    }
}

/// Extract a native Rust `AlloyProvider` from the held Python provider when
/// it is `PyAlloyProvider`-backed (live alloy or the O2 `OfflineProvider`
/// shell). Returns `None` for non-alloy providers (legacy test doubles), which
/// fall back to the Python delegation path.
///
/// Tries, in order: (1) the provider *is* a `PyAlloyProvider`; (2) the provider
/// exposes `to_alloy_provider()` returning a `PyAlloyProvider` (the Python
/// `AlloyProvider` wrapper + the `OfflineProvider` shell both do).
pub(crate) fn extract_native_alloy(provider: &Bound<'_, PyAny>) -> Option<Arc<AlloyProvider>> {
    if let Ok(pyap) = provider.extract::<PyRef<'_, PyAlloyProvider>>() {
        return Some(Arc::clone(&pyap.provider));
    }
    if let Ok(method) = provider.getattr("to_alloy_provider") {
        if let Ok(result) = method.call0() {
            if let Ok(pyap) = result.extract::<PyRef<'_, PyAlloyProvider>>() {
                return Some(Arc::clone(&pyap.provider));
            }
        }
    }
    None
}

/// Extract an optional block number from the `block` kw-sentinel.
/// `Ok(None)` when absent; `Ok(Some(n))` when an integer; `Err` when present
/// but not an integer.
fn extract_block_u64(block: Option<&Bound<'_, PyAny>>) -> PyResult<Option<u64>> {
    match block {
        None => Ok(None),
        Some(b) => b.extract::<u64>().map(Some),
    }
}

/// Parse a 20-byte address from a hex string, returning a borrowed 20-byte array
/// view for ABI-encoding. Internally uses the core `parse_address` so input
/// validation matches every other Rust pyclass (e.g. `PyAlloyProvider::get_balance`).
pub(crate) fn parse_address_for_call(address: &str) -> PyResult<[u8; 20]> {
    use degenbot_core::address_utils::parse_address;
    parse_address(address)
        .map(alloy::primitives::Address::into_array)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}")))
}
