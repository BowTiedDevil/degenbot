//! Row pyclasses returned by the `PyBotIo` pool-builder DB seam (QVMWQC).
//!
//! Mirrors the `SQLAlchemy` ORM rows + relationships the sync pool builders
//! traverse at construction time (`v2/v3/v4_pool_builder.py`): a pool row,
//! its per-DEX subclass kind row, the `exchange` / `token0` / `token1` FK rows,
//! and the V3 tick snapshot (`liquidity_positions` / `initialization_maps`).
//! Each is a thin `#[pyclass]` over the corresponding `degenbot-db` row type —
//! attribute access matches the ORM attributes the builders read, so the cutover
//! is a mechanical replacement of the `SQLAlchemy` lazy-load with a per-FK
//! fetch through `PyBotIo`.

use pyo3::prelude::*;

use crate::conversion::alloy::u256_to_py;

// ── pool + subclass rows ─────────────────────────────────────────────

/// A typed `pools` table row.
///
/// Mirrors the `SQLAlchemy` `LiquidityPoolTable` scalar + FK-id columns the
/// builders hydrate relationships from: `exchange_id` / `token0_id` /
/// `token1_id` feed the per-FK fetch methods; `kind` selects the subclass row.
#[pyclass(name = "LiquidityPoolRow")]
pub struct PyLiquidityPoolRow {
    id: i64,
    address: String,
    chain: i64,
    kind: String,
    token0_id: i64,
    token1_id: i64,
    exchange_id: i64,
}

impl PyLiquidityPoolRow {
    pub(crate) fn from(row: degenbot_db::rows::LiquidityPoolRow) -> Self {
        Self {
            id: row.id,
            address: row.address.to_checksum(None),
            chain: row.chain,
            kind: row.kind,
            token0_id: row.token0_id,
            token1_id: row.token1_id,
            exchange_id: row.exchange_id,
        }
    }
}

#[pymethods]
impl PyLiquidityPoolRow {
    #[getter]
    fn id(&self) -> i64 {
        self.id
    }
    #[getter]
    fn address(&self) -> String {
        self.address.clone()
    }
    #[getter]
    fn chain(&self) -> i64 {
        self.chain
    }
    #[getter]
    fn kind(&self) -> String {
        self.kind.clone()
    }
    #[getter]
    fn token0_id(&self) -> i64 {
        self.token0_id
    }
    #[getter]
    fn token1_id(&self) -> i64 {
        self.token1_id
    }
    #[getter]
    fn exchange_id(&self) -> i64 {
        self.exchange_id
    }
}

/// The per-DEX subclass row (`V2` / `V3` / `V4`) returned by
/// [`PyBotIo::fetch_pool_kind`]. Mirrors the `SQLAlchemy` subclass-table
/// columns the builders read: V2 fees, V3 `tick_spacing` + liquidity-update
/// marker, V4 pool-hash + hooks + currencies + fees + `tick_spacing` + marker.
///
/// The single pyclass surfaces every subclass field as an attribute; the
/// `variant` getter tells Python which family populated it (so the builder
/// picks the right fields).
#[pyclass(name = "PoolKindRow")]
pub struct PyPoolKindRow {
    variant: String,
    pool_id: i64,
    // V2 + V3 + V4 all carry fees; V4 also carries currencies.
    fee_token0: i64,
    fee_token1: i64,
    fee_denominator: i64,
    // V3 + V4.
    tick_spacing: i64,
    liquidity_update_block: Option<i64>,
    liquidity_update_log_index: Option<i64>,
    // V2-only.
    stable: Option<bool>,
    // V4-only.
    pool_hash: Option<String>,
    hooks: Option<String>,
    currency0_id: Option<i64>,
    currency1_id: Option<i64>,
    managed_pool_id: Option<i64>,
}

#[pymethods]
impl PyPoolKindRow {
    /// Which subclass populated this row: `"v2"`, `"v3"`, or `"v4"`.
    #[getter]
    fn variant(&self) -> String {
        self.variant.clone()
    }
    #[getter]
    fn pool_id(&self) -> i64 {
        self.pool_id
    }
    #[getter]
    fn fee_token0(&self) -> i64 {
        self.fee_token0
    }
    #[getter]
    fn fee_token1(&self) -> i64 {
        self.fee_token1
    }
    #[getter]
    fn fee_denominator(&self) -> i64 {
        self.fee_denominator
    }
    #[getter]
    fn tick_spacing(&self) -> i64 {
        self.tick_spacing
    }
    #[getter]
    fn liquidity_update_block(&self) -> Option<i64> {
        self.liquidity_update_block
    }
    #[getter]
    fn liquidity_update_log_index(&self) -> Option<i64> {
        self.liquidity_update_log_index
    }
    /// Present only for the Aerodrome V2 subclass (`variant == "v2"`).
    #[getter]
    fn stable(&self) -> Option<bool> {
        self.stable
    }
    /// V4 pool hash (0x-prefixed hex); `None` for V2/V3.
    #[getter]
    fn pool_hash(&self) -> Option<String> {
        self.pool_hash.clone()
    }
    /// V4 hooks address (checksummed); `None` for V2/V3.
    #[getter]
    fn hooks(&self) -> Option<String> {
        self.hooks.clone()
    }
    /// V4 currency0 FK id; `None` for V2/V3.
    #[getter]
    fn currency0_id(&self) -> Option<i64> {
        self.currency0_id
    }
    /// V4 currency1 FK id; `None` for V2/V3.
    #[getter]
    fn currency1_id(&self) -> Option<i64> {
        self.currency1_id
    }
    #[getter]
    fn managed_pool_id(&self) -> Option<i64> {
        self.managed_pool_id
    }
}

impl PyPoolKindRow {
    pub(crate) fn from(row: degenbot_db::rows::PoolKindRow) -> Self {
        match row {
            degenbot_db::rows::PoolKindRow::V2(v) => Self {
                variant: "v2".to_string(),
                pool_id: v.pool_id,
                fee_token0: v.fee_token0,
                fee_token1: v.fee_token1,
                fee_denominator: v.fee_denominator,
                tick_spacing: 0,
                liquidity_update_block: None,
                liquidity_update_log_index: None,
                stable: v.stable,
                pool_hash: None,
                hooks: None,
                currency0_id: None,
                currency1_id: None,
                managed_pool_id: None,
            },
            degenbot_db::rows::PoolKindRow::V3(v) => Self {
                variant: "v3".to_string(),
                pool_id: v.pool_id,
                fee_token0: v.fee_token0,
                fee_token1: v.fee_token1,
                fee_denominator: v.fee_denominator,
                tick_spacing: v.tick_spacing,
                liquidity_update_block: v.liquidity_update_block,
                liquidity_update_log_index: v.liquidity_update_log_index,
                stable: None,
                pool_hash: None,
                hooks: None,
                currency0_id: None,
                currency1_id: None,
                managed_pool_id: None,
            },
            degenbot_db::rows::PoolKindRow::V4(v) => Self {
                variant: "v4".to_string(),
                pool_id: v.managed_pool_id, // V4 subclasses key by managed_pool_id
                fee_token0: v.fee_currency0,
                fee_token1: v.fee_currency1,
                fee_denominator: v.fee_denominator,
                tick_spacing: v.tick_spacing,
                liquidity_update_block: v.liquidity_update_block,
                liquidity_update_log_index: v.liquidity_update_log_index,
                stable: None,
                pool_hash: Some(format!("{:?}", v.pool_hash)),
                hooks: Some(v.hooks.to_checksum(None)),
                currency0_id: Some(v.currency0_id),
                currency1_id: Some(v.currency1_id),
                managed_pool_id: Some(v.managed_pool_id),
            },
        }
    }
}

// ── FK companion rows ────────────────────────────────────────────────

/// A typed `exchanges` row (`exchange` relationship hydration).
#[pyclass(name = "ExchangeRow")]
pub struct PyExchangeRow {
    id: i64,
    chain_id: i64,
    name: String,
    active: bool,
    last_update_block: Option<i64>,
    factory: String,
    deployer: Option<String>,
}

impl PyExchangeRow {
    pub(crate) fn from(row: degenbot_db::rows::ExchangeRow) -> Self {
        Self {
            id: row.id,
            chain_id: row.chain_id,
            name: row.name,
            active: row.active,
            last_update_block: row.last_update_block,
            factory: row.factory.to_checksum(None),
            deployer: row.deployer.map(|a| a.to_checksum(None)),
        }
    }
}

#[pymethods]
impl PyExchangeRow {
    #[getter]
    fn id(&self) -> i64 {
        self.id
    }
    #[getter]
    fn chain_id(&self) -> i64 {
        self.chain_id
    }
    #[getter]
    fn name(&self) -> String {
        self.name.clone()
    }
    #[getter]
    fn active(&self) -> bool {
        self.active
    }
    #[getter]
    fn last_update_block(&self) -> Option<i64> {
        self.last_update_block
    }
    #[getter]
    fn factory(&self) -> String {
        self.factory.clone()
    }
    #[getter]
    fn deployer(&self) -> Option<String> {
        self.deployer.clone()
    }
}

// ── V4 pool-manager row ───────────────────────────────────────────────

/// A `pool_managers` row (V4). The V4 builder resolves its pool manager by
/// `(address, chain)` to obtain the `id` (for the V4 pool join) + the
/// `state_view` contract address.
#[pyclass(name = "PoolManagerRow")]
pub struct PyPoolManagerRow {
    id: i64,
    address: String,
    chain: i64,
    kind: String,
    state_view: Option<String>,
    exchange_id: i64,
}

impl PyPoolManagerRow {
    pub(crate) fn from(row: degenbot_db::rows::PoolManagerRow) -> Self {
        Self {
            id: row.id,
            address: row.address.to_checksum(None),
            chain: row.chain,
            kind: row.kind,
            state_view: row.state_view.map(|a| a.to_checksum(None)),
            exchange_id: row.exchange_id,
        }
    }
}

#[pymethods]
impl PyPoolManagerRow {
    #[getter]
    fn id(&self) -> i64 {
        self.id
    }
    #[getter]
    fn address(&self) -> String {
        self.address.clone()
    }
    #[getter]
    fn chain(&self) -> i64 {
        self.chain
    }
    #[getter]
    fn kind(&self) -> String {
        self.kind.clone()
    }
    #[getter]
    fn state_view(&self) -> Option<String> {
        self.state_view.clone()
    }
    #[getter]
    fn exchange_id(&self) -> i64 {
        self.exchange_id
    }
}

// ── V3 tick snapshot rows ────────────────────────────────────────────

/// A `liquidity_positions` row (V3 tick liquidity).
#[pyclass(name = "LiquidityPositionRow")]
pub struct PyLiquidityPositionRow {
    tick: i32,
    /// `liquidity_net` is a `uint128` stored as a decimal string; exposed as a
    /// Python `int` (signed semantics preserved — the Python builder wraps it
    /// via `int(pos.liquidity_net)`).
    liquidity_net: Py<PyAny>,
    liquidity_gross: Py<PyAny>,
}

impl PyLiquidityPositionRow {
    pub(crate) fn new(py: Python<'_>, row: &degenbot_db::rows::LiquidityPositionRow) -> PyResult<Self> {
        Ok(Self {
            tick: row.tick,
            liquidity_net: u256_to_py(py, &row.liquidity_net)?.unbind(),
            liquidity_gross: u256_to_py(py, &row.liquidity_gross)?.unbind(),
        })
    }

    /// Build from a V4 managed-pool liquidity-position row (same shape as the
    /// V3 row — `tick` / `liquidity_net` / `liquidity_gross`).
    pub(crate) fn from_managed(
        py: Python<'_>,
        row: &degenbot_db::rows::ManagedPoolLiquidityPositionRow,
    ) -> PyResult<Self> {
        Ok(Self {
            tick: row.tick,
            liquidity_net: u256_to_py(py, &row.liquidity_net)?.unbind(),
            liquidity_gross: u256_to_py(py, &row.liquidity_gross)?.unbind(),
        })
    }
}

#[pymethods]
impl PyLiquidityPositionRow {
    #[getter]
    fn tick(&self) -> i32 {
        self.tick
    }
    #[getter]
    fn liquidity_net(&self, py: Python<'_>) -> Py<PyAny> {
        self.liquidity_net.clone_ref(py)
    }
    #[getter]
    fn liquidity_gross(&self, py: Python<'_>) -> Py<PyAny> {
        self.liquidity_gross.clone_ref(py)
    }
}

/// An `initialization_maps` row (V3 tick bitmap).
#[pyclass(name = "InitializationMapRow")]
pub struct PyInitializationMapRow {
    word: i64,
    bitmap: Py<PyAny>,
}

impl PyInitializationMapRow {
    pub(crate) fn new(py: Python<'_>, row: &degenbot_db::rows::InitializationMapRow) -> PyResult<Self> {
        Ok(Self {
            word: row.word,
            bitmap: u256_to_py(py, &row.bitmap)?.unbind(),
        })
    }

    /// Build from a V4 managed-pool initialization-map row (same shape as the
    /// V3 row — `word` / `bitmap`).
    pub(crate) fn from_managed(
        py: Python<'_>,
        row: &degenbot_db::rows::ManagedPoolInitializationMapRow,
    ) -> PyResult<Self> {
        Ok(Self {
            word: row.word,
            bitmap: u256_to_py(py, &row.bitmap)?.unbind(),
        })
    }
}

#[pymethods]
impl PyInitializationMapRow {
    #[getter]
    fn word(&self) -> i64 {
        self.word
    }
    #[getter]
    fn bitmap(&self, py: Python<'_>) -> Py<PyAny> {
        self.bitmap.clone_ref(py)
    }
}