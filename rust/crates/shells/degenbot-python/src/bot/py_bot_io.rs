//! `PyBotIo` — the Rust `#[pyclass]` I/O façade that pool builders receive in
//! place of the Python `SyncPoolIO` adapter.
//!
//! ## What it is
//!
//! `Bot.build_pool`'s I/O choreography lives in Rust. Builders receive
//! `PyBotIo` as their `io` parameter; it exposes the 7-method
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
//! RPC duplication, leaving one code path per method. A block argument is a
//! typed selector: an int, or one canonical tag string
//! (`"earliest"` / `"latest"` / `"pending"`). A tag resolves to a block number
//! here — at the seam that owns the RPC clock (`ConstructionIo::
//! resolve_block_selector`) — so the numeric core faces stay numeric; any
//! other string is refused loudly rather than coalesced to the head.
//!
//! ## Async bridging
//!
//! Each call is `py.detach(|| get_runtime().block_on(...))` over the
//! process-global tokio runtime (`degenbot_core::runtime`) — detach the GIL,
//! block the runtime, reacquire. Never `block_on` while holding the GIL.
//!
//! [PoolIO]: degenbot/builders/pool_io.py
//! [ConstructionIo]: degenbot_bot::bot_core::ConstructionIo

use alloy::primitives::U256;
use pyo3::prelude::*;
use pyo3::types::PyList;
use std::fmt::Write as _;
use std::sync::Arc;

use crate::provider::AlloyProvider;
use crate::rpc::provider::PyAlloyProvider;
use crate::rpc::revert_to_pyerr;
use degenbot_bot::bot_core::{BlockSelector, PacedRead};
use degenbot_core::errors::ProviderError;
use degenbot_core::runtime::get_runtime;
use degenbot_rpc::abi::{
    ContractRead, IBalancerPool, IBalancerVault, IUniswapV3Pool, IUniswapV4StateView, Slot0Prefix,
    TickLiquidity, V2Reserves, V4Slot0,
};

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

/// The ONE resolved pool identity, projected for the Python companion —
/// structured facts (species, family, variant, factory, persisted kind),
/// never a Python class name. The core
/// ([`degenbot_bot::bot_core::ResolvedPoolIdentity`]) is the semantic owner;
/// this is its seam projection, and the companion's class selection reads it.
#[pyclass(name = "PoolIdentity", get_all, module = "degenbot._ffi")]
#[derive(Clone, Debug)]
pub struct PyPoolIdentity {
    /// The species-manifest spelling this identity's persisted kind names
    /// (`"uniswap_v3"`, …), or `None` for a kind the manifest does not carry.
    pub species: Option<String>,
    /// The family spelling (the Python `PoolFamily` values).
    pub family: String,
    /// The fork spelling (`"sushiswap"`, `"aerodrome"`,
    /// `"balancer_stable"`, …); `None` is the canonical Uniswap identity.
    pub variant: Option<String>,
    /// The factory evidence, EIP-55 checksummed; `None` for the factory-less
    /// identities (Curve; a Balancer revision no registered factory claims).
    pub factory: Option<String>,
    /// The persisted `pools.kind` discriminator this identity carries.
    pub persisted_kind: String,
}

impl From<degenbot_bot::bot_core::ResolvedPoolIdentity> for PyPoolIdentity {
    fn from(identity: degenbot_bot::bot_core::ResolvedPoolIdentity) -> Self {
        Self {
            species: identity.species.map(str::to_string),
            family: identity.family.as_str().to_string(),
            variant: identity.variant,
            factory: identity.factory.map(|f| f.to_checksum(None)),
            persisted_kind: identity.persisted_kind,
        }
    }
}

#[pymethods]
impl PyPoolIdentity {
    fn __repr__(&self) -> String {
        format!(
            "PoolIdentity(species={:?}, family={:?}, variant={:?}, factory={:?}, persisted_kind={:?})",
            self.species, self.family, self.variant, self.factory, self.persisted_kind,
        )
    }
}

/// The Rust I/O façade for pool builders.
///
/// Construct with an existing `ProviderAdapter` (the `Bot.provider`):
/// ```python
/// io = PyBotIo(provider=bot.provider, database_path=str(bot.database_path))
/// ```
/// then pass `io` where a builder expects an `io: PoolIO`.
/// A typed ERC-20 token DB row returned by [`PyBotIo::fetch_erc20_token`]
///. Mirrors the `SQLAlchemy` `Erc20TokenTable` ORM object's
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
    /// The on-disk `SQLite` database path. Retained for the `getter`
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
    construction_io: parking_lot::Mutex<Option<Arc<degenbot_bot::bot_core::ConstructionIo>>>,
}

#[pymethods]
impl PyBotIo {
    /// Construct the I/O façade over an alloy-backed `ProviderAdapter`
    /// (+ optional file-backed database).
    ///
    /// `provider` is the `ProviderAdapter` the `Bot` was constructed with; the
    /// held `Arc<AlloyProvider>` is extracted from it (live alloy or the
    /// offline shell) and powers a transient `(NoDb, AlloyRpcConstruction)`
    /// handle for the RPC methods. Non-alloy providers (the retired web3 /
    /// legacy-double fallback) yield `alloy = None` and every RPC + choreography
    /// method errors loudly (ADR-023 D1).
    ///
    /// `database_path` is the on-disk `SQLite` path; when set, the
    /// DB-query methods (`fetch_erc20_token`, `update_erc20_token_metadata`, …)
    /// open a `degenbot_db::DegenbotDb` handle from it + route the
    /// construction-time DB reads/writes through Rust (the `SQLAlchemy`
    /// `session.scalar(select(...))` / `session.commit()` bodies retire).
    #[new]
    #[expect(clippy::needless_pass_by_value)]
    #[pyo3(signature = (provider, database_path=None))]
    pub(crate) fn new(py: Python<'_>, provider: Py<PyAny>, database_path: Option<String>) -> Self {
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
                        use degenbot_bot::bot_core::{
                            AlloyRpcConstruction, ConstructionIo, DegenbotDbConstruction,
                        };
                        Some(std::sync::Arc::new(ConstructionIo::new(
                            std::sync::Arc::new(DegenbotDbConstruction::new(std::sync::Arc::new(
                                db,
                            ))),
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

    /// The on-disk `SQLite` database path, if any. The DB-query methods
    /// open a `degenbot_db::DegenbotDb` handle from this path; `None` when the
    /// `Bot` has no DB.
    #[getter]
    fn database_path(&self) -> Option<String> {
        self.database_path.clone()
    }

    /// Fetch an ERC-20 token row from the DB by `(chain_id, address)` — the
    /// construction-time read in `Erc20Builder.build`. Replaces the
    /// `SQLAlchemy` `session.scalar(select(Erc20TokenTable).where(...))` call.
    ///
    /// Returns a [`PyErc20TokenRow`] with `(id, chain, address, name, symbol,
    /// decimals)`, or `None` when the row is absent. Returns `None` (no error)
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
    /// `Erc20Builder.build`. Replaces the `SQLAlchemy`
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
    /// construction-time read. Replaces the `SQLAlchemy`
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

    /// Fetch an `exchanges` row by its FK id — hydrates the
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

    /// Resolve one pool's identity through the ONE core ladder — the Python
    /// companion's projection seam. The structured answer (species, family,
    /// variant, factory, persisted kind) is the same value the pure-Rust
    /// consumer resolves; the companion keeps no ladder of its own.
    ///
    /// Errors:
    /// - `PyValueError` — a typed identity refusal (an unknown DB kind, an
    ///   unknown identity, an unsupported or contradictory identity); the
    ///   caller wraps as `DegenbotValueError`.
    /// - `PyRuntimeError` / `DbError` — a transport or database failure,
    ///   which is never a classification.
    #[pyo3(signature = (chain_id, address))]
    fn resolve_pool_identity(
        &self,
        py: Python<'_>,
        chain_id: i64,
        address: &str,
    ) -> PyResult<PyPoolIdentity> {
        use degenbot_bot::bot_core::{resolve_pool_identity, PoolIdentityError};
        let io = self.required_construction_io()?;
        let addr = alloy::primitives::Address::from(parse_address_for_call(address)?);
        let chain = u64::try_from(chain_id).map_err(|_| {
            pyo3::exceptions::PyValueError::new_err("chain_id must be non-negative")
        })?;
        let identity = py
            .detach(|| {
                get_runtime().block_on(async move { resolve_pool_identity(chain, addr, &io).await })
            })
            .map_err(|err| match err {
                PoolIdentityError::UnknownDbKind { .. }
                | PoolIdentityError::UnknownIdentity { .. }
                | PoolIdentityError::Unsupported { .. }
                | PoolIdentityError::Contradictory { .. } => {
                    pyo3::exceptions::PyValueError::new_err(err.to_string())
                }
                PoolIdentityError::Db(inner) => crate::db::db_err_to_py(&inner),
                PoolIdentityError::Io(inner) => {
                    pyo3::exceptions::PyRuntimeError::new_err(inner.to_string())
                }
            })?;
        Ok(PyPoolIdentity::from(identity))
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
        let block_num = self.resolve_block(py, block)?;
        let io = self.required_construction_io()?;
        let code = py
            .detach(|| get_runtime().block_on(async { io.get_code(addr, block_num).await }))
            .map_err(Into::<PyErr>::into)?;
        crate::conversion::cache::to_py_bytes(py, code.as_ref())
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
        let block_num = self.resolve_block(py, block)?;
        let io = self.required_construction_io()?;
        let balance = py
            .detach(|| get_runtime().block_on(async { io.get_balance(addr, block_num).await }))
            .map_err(Into::<PyErr>::into)?;
        crate::conversion::alloy::u256_to_py(py, &balance)
            .map(pyo3::Bound::into_any)
            .map(pyo3::Bound::unbind)
    }

    /// Fetch a V2-style pool's reserves via `getReserves()`, performing the
    /// encode -> call -> decode choreography in Rust.
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
        let block_num = self.resolve_block(py, block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                let reserves = V2Reserves::read(&io, addr, block_num).await?;
                Ok::<_, ProviderError>((reserves.reserve0, reserves.reserve1))
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
    /// encode -> call -> decode choreography in Rust.
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
        let block_num = self.resolve_block(py, block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                let slot0 = Slot0Prefix::read(&io, addr, block_num).await?;
                let liquidity = io
                    .read(addr, IUniswapV3Pool::liquidityCall {}, block_num)
                    .await?;
                Ok::<_, ProviderError>((slot0.sqrt_price_x96, slot0.tick, U256::from(liquidity)))
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
    /// `getLiquidity(bytes32)` on the state-view contract.
    ///
    /// Mirrors `degenbot/builders/v4_pool_builder.py`'s slot0/liquidity RPC
    /// block in `_build_pool`. V4 differs from V3 in two ways:
    /// 1. Methods take a `bytes32 pool_id` prefix argument (like the V4 tick
    ///    RPCs).
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
        let block_num = self.resolve_block(py, block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                let slot0 = V4Slot0::read(&io, state_view, pool_id_arr, block_num).await?;
                let liquidity = io
                    .read(
                        state_view,
                        IUniswapV4StateView::getLiquidityCall {
                            poolId: pool_id_arr.into(),
                        },
                        block_num,
                    )
                    .await?;
                Ok::<_, ProviderError>((
                    slot0.sqrt_price_x96,
                    slot0.tick,
                    slot0.protocol_fee,
                    slot0.lp_fee,
                    U256::from(liquidity),
                ))
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

    /// Fetch all Curve pool balances via `balances(uint256)` in a loop.
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
        let block_num = self.resolve_block(py, block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                degenbot_bot::bot_core::fetch_curve_balances(&io, pool, count, block_num).await
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

    /// Fetch a V3 pool's tick bitmap word via `tickBitmap(int16)`, performing
    /// the encode -> call -> decode choreography in Rust.
    ///
    /// Calldata + return decode come from `degenbot_rpc::abi`'s `sol!` home —
    /// no hand-rolled selector or word masking here. The read is shared with
    /// `AlloyTickWordSource` (the standalone-Rust `cargo add degenbot` consumer
    /// path), so the choreography stays byte-identical across the pyo3 adapter
    /// and the alloy impl.
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
        let block_num = self.resolve_block(py, block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                PacedRead::new(&io)
                    .read(
                        addr,
                        IUniswapV3Pool::tickBitmapCall {
                            wordPosition: word_i16,
                        },
                        block_num,
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
    /// the encode -> call -> decode choreography in Rust.
    ///
    /// Calldata + return decode from `degenbot_rpc::abi`'s `sol!` home. The
    /// narrowing keeps the result's first two fields (`liquidity_gross:
    /// uint128`, `liquidity_net: int128`), both right-aligned in their 32-byte
    /// ABI words, and leaves the rest of the `ticks` tuple unread.
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
        let block_num = self.resolve_block(py, block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                TickLiquidity::read(&PacedRead::new(&io), addr, tick_i32, block_num)
                    .await
                    .map(|l| (l.gross, l.net))
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
            pyo3::types::PyInt::new(py, net).into_any().unbind(),
        ))
    }

    /// Fetch a V4 pool's tick bitmap via `getTickBitmap(bytes32,int16)` on the
    /// state-view contract.
    ///
    /// Calldata + return decode from `degenbot_rpc::abi`'s `sol!` home. V4 adds
    /// a `pool_id` (`bytes32`) prefix argument before the `int16` word position.
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
        let block_num = self.resolve_block(py, block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                PacedRead::new(&io)
                    .read(
                        state_view,
                        IUniswapV4StateView::getTickBitmapCall {
                            poolId: pool_id_arr.into(),
                            wordPosition: word_i16,
                        },
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
    /// on the state-view contract.
    ///
    /// Calldata + return decode from `degenbot_rpc::abi`'s `sol!` home. V4 adds
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
        let tick_i32 = i32::try_from(tick)
            .map_err(|_| pyo3::exceptions::PyValueError::new_err("tick out of int24 range"))?;
        let pool_id_arr: [u8; 32] = pool_id
            .try_into()
            .map_err(|_| pyo3::exceptions::PyValueError::new_err("pool_id must be 32 bytes"))?;
        let io = self.required_construction_io()?;
        let state_view =
            alloy::primitives::Address::from(parse_address_for_call(state_view_address)?);
        let block_num = self.resolve_block(py, block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                TickLiquidity::read_v4(
                    &PacedRead::new(&io),
                    state_view,
                    pool_id_arr,
                    tick_i32,
                    block_num,
                )
                .await
                .map(|l| (l.gross, l.net))
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
            pyo3::types::PyInt::new(py, net).into_any().unbind(),
        ))
    }

    /// Fetch a Balancer pool's swap fee via `getSwapFeePercentage()`.
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
        let block_num = self.resolve_block(py, block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                io.read(addr, IBalancerPool::getSwapFeePercentageCall {}, block_num)
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
    /// `getAmplificationParameter()`.
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
        let block_num = self.resolve_block(py, block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                io.read(
                    addr,
                    IBalancerPool::getAmplificationParameterCall {},
                    block_num,
                )
                .await
                .map(|r| r.value)
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
    /// `getNormalizedWeights()`.
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
        let block_num = self.resolve_block(py, block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                io.read(addr, IBalancerPool::getNormalizedWeightsCall {}, block_num)
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

    /// Fetch a Balancer pool's rate providers via `getRateProviders()`.
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
        let block_num = self.resolve_block(py, block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                io.read(addr, IBalancerPool::getRateProvidersCall {}, block_num)
                    .await
            })
        });
        let providers = match r {
            Ok(v) => Ok(v),
            Err(ProviderError::ExecutionReverted { message, .. }) => {
                // Callers read a refusal as an empty list (WeightedPool2Tokens /
                // MetaStable pools that don't expose getRateProviders revert).
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
    /// bytes32)` on the vault contract.
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
        let block_num = self.resolve_block(py, block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                io.read(
                    vault,
                    IBalancerVault::getPoolTokensCall {
                        poolId: pool_id.into(),
                    },
                    block_num,
                )
                .await
                .map(|r| (r.tokens, r.balances))
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

    /// Fetch a single Balancer rate provider's rate via `getRate()`.
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
        let block_num = self.resolve_block(py, block)?;
        let r = py.detach(|| {
            get_runtime().block_on(async move {
                io.read(provider, IBalancerPool::getRateCall {}, block_num)
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

    fn __repr__(&self) -> String {
        format!("BotIo(alloy={})", self.alloy.is_some())
    }
}

impl PyBotIo {
    /// The native Rust `AlloyProvider`, if the held Python provider is
    /// `PyAlloyProvider`-backed (live alloy or the offline shell). `None` for
    /// non-alloy providers (legacy test doubles).
    ///
    /// Used by the Chain-arm wiring ) to construct an
    /// [`AlloyTickWordSource`] without a GIL round-trip per RPC call — the
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
    fn construction_io(&self) -> Option<std::sync::Arc<degenbot_bot::bot_core::ConstructionIo>> {
        self.construction_io.lock().clone()
    }

    /// Resolve a [`ConstructionIo`] for the choreography adapters: the attached
    /// handle when present, else a transient `(NoDb, AlloyRpcConstruction)`
    /// built over the held alloy provider (bare test fixtures that construct
    /// `PyBotIo(provider=…)` directly). Non-alloy Python providers (the legacy
    /// fallback the builder-choreography port retires) error here.
    pub(crate) fn required_construction_io(
        &self,
    ) -> PyResult<std::sync::Arc<degenbot_bot::bot_core::ConstructionIo>> {
        use degenbot_bot::bot_core::{AlloyRpcConstruction, ConstructionIo, NoDb};
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

    /// Resolve the `block` kw-sentinel to a concrete block number at the seam
    /// that owns the RPC clock.
    ///
    /// A number (or `None`) short-circuits with no RPC; a typed tag reads the
    /// head through the attached `ConstructionIo`. This is where a block tag
    /// becomes a number, so the core faces this seam calls stay numeric.
    pub(crate) fn resolve_block(
        &self,
        py: Python<'_>,
        block: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Option<u64>> {
        let Some(selector) = extract_block_selector(block)? else {
            return Ok(None);
        };
        if let Some(number) = selector.as_number() {
            return Ok(Some(number));
        }
        let io = self.required_construction_io()?;
        resolve_block_tag(py, &io, selector).map(Some)
    }
}

/// Map one typed account-read failure to Python: the revert arm keeps its
/// per-token `revert_to_pyerr` classification, a transport fault keeps its
/// `ProviderError` conversion, and the typed decode failure surfaces verbatim.
pub(crate) fn account_read_error(
    py: Python<'_>,
    token: &str,
    error: degenbot_rpc::erc20::AccountReadError,
) -> PyErr {
    use degenbot_rpc::erc20::AccountReadError;
    match error {
        AccountReadError::Call(ProviderError::ExecutionReverted { message, .. }) => {
            revert_to_pyerr(py, token, &message)
        }
        AccountReadError::Call(other) => other.into(),
        error @ AccountReadError::NotUint256 { .. } => {
            pyo3::exceptions::PyRuntimeError::new_err(error.to_string())
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

/// Parse the `block` kw-sentinel into a typed [`BlockSelector`].
///
/// `None` (absent) stays `None`; a Python int is a concrete number; a string
/// must be one canonical tag (`"earliest"` / `"latest"` / `"pending"`); any
/// other shape is refused. A tag is carried as a TYPED value here — never the
/// number it might have been coerced into, and never a `None` it might have
/// been silently dropped to (which reads as the head).
pub(crate) fn extract_block_selector(
    block: Option<&Bound<'_, PyAny>>,
) -> PyResult<Option<BlockSelector>> {
    let Some(raw) = block else {
        return Ok(None);
    };
    if let Ok(number) = raw.extract::<u64>() {
        return Ok(Some(BlockSelector::number(number)));
    }
    if let Ok(text) = raw.extract::<String>() {
        return BlockSelector::parse_tag(&text)
            .map(Some)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()));
    }
    Err(pyo3::exceptions::PyTypeError::new_err(format!(
        "block must be an int or a canonical block tag string, got {}",
        raw.get_type().name()?
    )))
}

/// Resolve a typed tag to a number through an already-resolved
/// [`ConstructionIo`] (the seam that owns the RPC clock).
fn resolve_block_tag(
    py: Python<'_>,
    io: &Arc<degenbot_bot::bot_core::ConstructionIo>,
    selector: BlockSelector,
) -> PyResult<u64> {
    let io = Arc::clone(io);
    py.detach(|| get_runtime().block_on(async move { io.resolve_block_selector(selector).await }))
        .map_err(Into::<PyErr>::into)
}

/// Resolve the `block` kw-sentinel to a concrete block number against an
/// already-resolved [`ConstructionIo`]. A number (or `None`) short-circuits
/// with no RPC; a typed tag reads the head.
pub(crate) fn resolve_block_against(
    py: Python<'_>,
    io: &Arc<degenbot_bot::bot_core::ConstructionIo>,
    block: Option<&Bound<'_, PyAny>>,
) -> PyResult<Option<u64>> {
    let Some(selector) = extract_block_selector(block)? else {
        return Ok(None);
    };
    if let Some(number) = selector.as_number() {
        return Ok(Some(number));
    }
    resolve_block_tag(py, io, selector).map(Some)
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
