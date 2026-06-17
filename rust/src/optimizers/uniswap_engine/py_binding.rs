//! `PyO3` wrapper for the `UniswapEngine`.
//!
//! [`PyUniswapArbEngine`] wraps [`UniswapEngine`] with a `parking_lot::Mutex`
//! for safe access from the Tokio pump task. All Python-facing methods
//! acquire the lock, perform their operation, and release it.

use std::collections::HashMap;
use std::sync::Arc;

use alloy::primitives::{Address, U256};
use pyo3::exceptions::PyStopAsyncIteration;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use tokio::sync::mpsc;

use crate::bot_core::{RegisterV3PoolParams, V3SwapUpdate};
use crate::optimizers::v4_block_engine::{RegisterV4PoolParams, V4SwapUpdate};

use super::{UniswapEngine, ResultBatch, EnginePhase, PoolTickCoverage, HopType, MixedPoolRef, BlockMetadata, SolvePathResult, V3SnapshotData, V4SnapshotData};

/// Snapshot storage keyed by pool identifier (V3 address or V4 pool manager + pool ID).
///
/// Holds a one-way transfer of tick data: `load()` replaces the store, and
/// `take()` removes a single pool's data at registration time. Streaming loads
/// begin with `begin_load()` and are populated via `insert()`.
struct SnapshotStore<K: Eq + std::hash::Hash> {
    data: parking_lot::Mutex<Option<HashMap<K, HashMap<i32, crate::bot_core::TickInfo>>>>,
}

impl<K: Eq + std::hash::Hash + Clone> SnapshotStore<K> {
    fn new() -> Self {
        Self {
            data: parking_lot::Mutex::new(None),
        }
    }

    fn is_loaded(&self) -> bool {
        self.data.lock().is_some()
    }

    fn load(&self, data: HashMap<K, HashMap<i32, crate::bot_core::TickInfo>>) {
        *self.data.lock() = Some(data);
    }

    fn begin_load(&self) {
        *self.data.lock() = Some(HashMap::new());
    }

    fn insert(
        &self,
        key: K,
        tick_data: HashMap<i32, crate::bot_core::TickInfo>,
    ) -> PyResult<()> {
        let mut guard = self.data.lock();
        let Some(ref mut map) = *guard else {
            let msg = "No snapshot stream in progress.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        };
        map.insert(key, tick_data);
        Ok(())
    }

    /// Remove a single pool's tick data from the store.
    ///
    /// Returns `Tracked` coverage if the key existed, otherwise `Sparse`.
    fn take(&self, key: &K) -> (HashMap<i32, crate::bot_core::TickInfo>, PoolTickCoverage) {
        let mut guard = self.data.lock();
        if let Some(ref mut map) = *guard {
            if let Some(tick_data) = map.remove(key) {
                return (tick_data, PoolTickCoverage::Tracked);
            }
        }
        (HashMap::new(), PoolTickCoverage::Sparse)
    }

    fn clear(&self) {
        *self.data.lock() = None;
    }
}

/// Helper that registers a CL pool, applies backfill/pump buffers, and captures
/// a backfill-boundary snapshot while the engine lock is held.
fn register_with_cl_buffers<Key, BackfillSnapshot>(
    engine: &Arc<parking_lot::Mutex<UniswapEngine>>,
    register: impl FnOnce(&mut UniswapEngine) -> Key,
    apply_backfill: impl FnOnce(&mut UniswapEngine),
    take_backfill_snapshot: impl FnOnce(&UniswapEngine, &Key) -> Option<BackfillSnapshot>,
    apply_pump: impl FnOnce(&mut UniswapEngine),
) -> (Key, Option<BackfillSnapshot>) {
    let mut engine = engine.lock();
    let key = register(&mut engine);
    apply_backfill(&mut engine);
    let backfill_snapshot = take_backfill_snapshot(&engine, &key);
    apply_pump(&mut engine);
    (key, backfill_snapshot)
}

/// Lazily create or reuse a cached verification provider.
fn verification_provider(
    rpc_url: &str,
    verify_provider: &parking_lot::Mutex<Option<crate::provider::AlloyProvider>>,
    label: &str,
) -> PyResult<crate::provider::AlloyProvider> {
    let mut cached = verify_provider.lock();
    if cached.is_none() {
        let runtime = crate::runtime::get_runtime();
        match runtime.block_on(crate::provider::AlloyProvider::new(rpc_url, 3)) {
            Ok(provider) => *cached = Some(provider),
            Err(e) => {
                let msg = format!("verify: {label}: failed to create provider: {e}");
                return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
            }
        }
    }
    Ok(cached.as_ref().unwrap().clone())
}

/// Run the two-phase CL verification (snapshot block + backfill block) if both
/// blocks are configured.
fn run_cl_verification<F1, F2>(
    rpc_url: Option<String>,
    verify_provider: &parking_lot::Mutex<Option<crate::provider::AlloyProvider>>,
    snapshot_block: Option<u64>,
    backfill_block: Option<u64>,
    label: &str,
    verify_snapshot: F1,
    verify_backfill: F2,
) -> PyResult<()>
where
    F1: FnOnce(&crate::provider::AlloyProvider, u64) -> PyResult<()>,
    F2: FnOnce(&crate::provider::AlloyProvider, u64) -> PyResult<()>,
{
    let Some(rpc_url) = rpc_url else {
        return Ok(());
    };

    let provider = verification_provider(&rpc_url, verify_provider, label)?;

    if let Some(block) = snapshot_block {
        verify_snapshot(&provider, block)?;
    }
    if let Some(block) = backfill_block {
        verify_backfill(&provider, block)?;
    }

    Ok(())
}

/// Python-facing mixed V2/V3 arbitrage engine.
///
/// Wraps [`UniswapEngine`] with a `parking_lot::Mutex` for safe access
/// from the Tokio pump task.
#[pyclass(name = "UniswapArbEngine", skip_from_py_object)]
#[allow(dead_code)]
pub struct PyUniswapArbEngine {
    /// Shared engine state
    engine: Arc<parking_lot::Mutex<UniswapEngine>>,
    /// Shutdown flag for the pump
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    /// Handle for the pump task (None until `start()` is called)
    pump_handle: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Subscribe state held between `subscribe()` and `resume()` calls.
    /// Contains the live WS stream and first observed block number.
    subscribe_state: parking_lot::Mutex<Option<PySubscribeState>>,
    /// Receiver for the result batch channel.
    /// Created in `new()`, consumed by `__anext__`.
    /// Wrapped in Arc so the async coroutine can share it.
    result_rx: Arc<parking_lot::Mutex<Option<mpsc::UnboundedReceiver<ResultBatch>>>>,
    /// When True, verify each V3/V4 pool's tick data against on-chain state
    /// immediately after registration. The snapshot is taken while the engine
    /// lock is held (so the pump can't race), then verification runs via RPC
    /// after the lock is released. Failures are logged as errors.
    verify_on_register: std::sync::atomic::AtomicBool,
    /// Optional HTTP RPC URL for verification during registration.
    /// Must be set before `verify_on_register` is enabled.
    verify_rpc_url: parking_lot::Mutex<Option<String>>,
    /// Cached Alloy provider for verification RPCs.
    /// Created on first use (or when `set_verify_rpc_url` is called) and reused
    /// across all verifications to avoid creating a new HTTP client per pool.
    verify_provider: parking_lot::Mutex<Option<crate::provider::AlloyProvider>>,
    /// Optional `StateView` contract address for V4 verification.
    verify_state_view: parking_lot::Mutex<Option<Address>>,
    /// The snapshot block — the block at which the DB tick data was captured.
    /// Set automatically by `backfill_from_snapshot()`. Verification compares
    /// the raw snapshot `tick_data` (before buffer) against on-chain at this block.
    verify_snapshot_block: parking_lot::Mutex<Option<u64>>,
    /// The backfill block — the last block processed by backfill, before the
    /// WS pump starts. Set automatically by `backfill_from_snapshot()`.
    /// Verification compares engine state (snapshot + backfill buffer) against
    /// on-chain at this block.
    verify_backfill_block: parking_lot::Mutex<Option<u64>>,
    /// Engine lifecycle phase (Plan 098).
    /// Enforces ordering: Created → Subscribed → `SnapshotLoaded` → Backfilled → Resumed.
    phase: std::sync::atomic::AtomicU8,
    /// V3 snapshot tick data, loaded via `load_v3_snapshot()` and consumed
    /// at registration time. One-way transfer: `remove()` not `clone()`.
    v3_snapshot: SnapshotStore<Address>,
    /// V4 snapshot tick data, loaded via `load_v4_snapshot()` and consumed
    /// at registration time. One-way transfer: `remove()` not `clone()`.
    v4_snapshot: SnapshotStore<(Address, crate::bot_core::v4_swap_decoder::PoolId)>,
}

/// Python-facing subscribe state.
///
/// Stores the pump and subscribe results between `subscribe()` and `resume()`
/// calls so that `resume()` can re-use the same pump instance.
struct PySubscribeState {
    /// The pump instance (holds engine, provider, shutdown, and `block_tx`)
    pump: crate::optimizers::uniswap_engine_pump::UniswapEnginePump,
    /// First block number observed during subscribe
    first_block: u64,
    /// Live WS stream for the resume phase
    combined_stream: futures_util::stream::BoxStream<'static, crate::optimizers::uniswap_engine_pump::WsEvent>,
}

impl PyUniswapArbEngine {
    /// Parse V2 Sync updates from a Python list of 3-tuples.
    fn parse_v2_updates(
        v2_sync_updates: &Bound<'_, PyList>,
    ) -> PyResult<Vec<(Address, U256, U256)>> {
        let mut rust_v2: Vec<(Address, U256, U256)> = Vec::with_capacity(v2_sync_updates.len());
        for item in v2_sync_updates.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 3 {
                let msg = format!(
                    "Expected 3-tuple (address, reserve0, reserve1), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let addr_obj = tuple.get_item(0)?;
            let addr_str: String = addr_obj.extract()?;
            let addr = addr_str.parse::<Address>().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}"))
            })?;
            let r0 = crate::alloy_py::extract_python_u256(&tuple.get_item(1)?)?;
            let r1 = crate::alloy_py::extract_python_u256(&tuple.get_item(2)?)?;
            rust_v2.push((addr, r0, r1));
        }
        Ok(rust_v2)
    }

    /// Parse tick priors from a Python list of 2-tuples.
    fn parse_tick_priors(priors_list: &Bound<'_, PyList>) -> PyResult<Vec<(i32, crate::bot_core::TickInfo)>> {
        let mut tick_priors = Vec::new();
        for prior_item in priors_list.iter() {
            let prior_tuple = prior_item.cast::<pyo3::types::PyTuple>()?;
            if prior_tuple.len() != 2 {
                let msg = format!(
                    "Expected 2-tuple (tick_index, (lg, ln)), got {} elements",
                    prior_tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let tick_idx: i32 = prior_tuple.get_item(0)?.extract()?;
            let info_obj = prior_tuple.get_item(1)?;
            let info_tuple = info_obj.cast::<pyo3::types::PyTuple>()?;
            if info_tuple.len() != 2 {
                let msg = format!(
                    "Expected 2-tuple (liquidity_gross, liquidity_net), got {} elements",
                    info_tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let lg: u128 = info_tuple.get_item(0)?.extract()?;
            let ln: i128 = info_tuple.get_item(1)?.extract()?;
            tick_priors.push((tick_idx, make_tick_info(lg, ln)));
        }
        Ok(tick_priors)
    }

    /// Parse V3 Swap updates from a Python list of 5-tuples.
    fn parse_v3_updates(
        v3_swap_updates: &Bound<'_, PyList>,
    ) -> PyResult<Vec<V3SwapUpdate>> {
        let mut rust_v3: Vec<V3SwapUpdate> = Vec::with_capacity(v3_swap_updates.len());
        for item in v3_swap_updates.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 5 {
                let msg = format!(
                    "Expected 5-tuple (address, sqrt_price, liquidity, tick, tick_priors), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }

            let addr_obj = tuple.get_item(0)?;
            let addr_str: String = addr_obj.extract()?;
            let addr = addr_str.parse::<Address>().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}"))
            })?;
            let sqrt_price = crate::alloy_py::extract_python_u256(&tuple.get_item(1)?)?;
            let liquidity: u128 = tuple.get_item(2)?.extract()?;
            let tick: i32 = tuple.get_item(3)?.extract()?;

            let priors_obj = tuple.get_item(4)?;
            let priors_list = priors_obj.cast::<PyList>()?;
            let tick_priors = Self::parse_tick_priors(priors_list)?;

            rust_v3.push(V3SwapUpdate {
                pool_address: addr,
                sqrt_price_x96: sqrt_price,
                liquidity,
                tick,
                tick_priors,
            });
        }
        Ok(rust_v3)
    }

    /// Parse V4 Swap updates from a Python list of 6-tuples.
    fn parse_v4_updates(
        v4_swap_updates: &Bound<'_, PyList>,
    ) -> PyResult<Vec<V4SwapUpdate>> {
        let mut rust_v4: Vec<V4SwapUpdate> = Vec::with_capacity(v4_swap_updates.len());
        for item in v4_swap_updates.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 6 {
                let msg = format!(
                    "Expected 6-tuple (pool_manager, pool_id_hex, sqrt_price, liquidity, tick, tick_priors), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }

            let pm_obj = tuple.get_item(0)?;
            let pm_str: String = pm_obj.extract()?;
            let pool_manager = pm_str.parse::<Address>().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid pool_manager address: {e}"))
            })?;

            let pid_obj = tuple.get_item(1)?;
            let pid_str: String = pid_obj.extract()?;
            let pool_id = hex_string_to_pool_id(&pid_str)?;

            let sqrt_price = crate::alloy_py::extract_python_u256(&tuple.get_item(2)?)?;
            let liquidity: u128 = tuple.get_item(3)?.extract()?;
            let tick: i32 = tuple.get_item(4)?.extract()?;

            let priors_obj = tuple.get_item(5)?;
            let priors_list = priors_obj.cast::<PyList>()?;
            let tick_priors = Self::parse_tick_priors(priors_list)?;

            rust_v4.push(V4SwapUpdate {
                pool_manager,
                pool_id,
                sqrt_price_x96: sqrt_price,
                liquidity,
                tick,
                tick_priors,
            });
        }
        Ok(rust_v4)
    }

    /// Get the current engine phase.
    fn current_phase(&self) -> EnginePhase {
        match self.phase.load(std::sync::atomic::Ordering::Relaxed) {
            0 => EnginePhase::Created,
            1 => EnginePhase::Subscribed,
            2 => EnginePhase::SnapshotLoaded,
            3 => EnginePhase::Backfilled,
            4 => EnginePhase::Resumed,
            #[allow(clippy::match_same_arms)]
            _ => EnginePhase::Created, // fallthrough for corrupt state
        }
    }

    /// Set the engine phase (advancing only).
    fn set_phase(&self, phase: EnginePhase) {
        self.phase.store(phase as u8, std::sync::atomic::Ordering::Relaxed);
    }

    /// Deserialize a V3 binary snapshot into `V3SnapshotData`.
    fn deserialize_v3_snapshot(data: &[u8]) -> PyResult<V3SnapshotData> {
        const MIN_HEADER: usize = 5; // version(1) + pool_count(4)

        if data.len() < MIN_HEADER {
            let msg = format!(
                "V3 snapshot data too short: {} bytes (minimum {})",
                data.len(),
                MIN_HEADER
            );
            return Err(pyo3::exceptions::PyValueError::new_err(msg));
        }

        let version = data[0];
        if version != 1 {
            let msg = format!("Unsupported V3 snapshot format version: {version}");
            return Err(pyo3::exceptions::PyValueError::new_err(msg));
        }

        let pool_count = u32::from_le_bytes(data[1..5].try_into().unwrap()) as usize;
        let mut result = HashMap::with_capacity(pool_count);

        let mut offset = MIN_HEADER;
        for _ in 0..pool_count {
            // Pool address (20 bytes)
            if offset + 20 > data.len() {
                let msg = "V3 snapshot truncated: expected 20-byte pool address";
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let addr_bytes: [u8; 20] = data[offset..offset + 20].try_into().unwrap();
            let address = Address::from(addr_bytes);
            offset += 20;

            // Tick count (4 bytes LE)
            if offset + 4 > data.len() {
                let msg = "V3 snapshot truncated: expected tick_count";
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let tick_count = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;

            let mut tick_data = HashMap::with_capacity(tick_count);
            for _ in 0..tick_count {
                // tick_index (4 bytes LE, i32)
                if offset + 4 > data.len() {
                    let msg = "V3 snapshot truncated: expected tick_index";
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
                let tick_index = i32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                offset += 4;

                // liquidity_gross (16 bytes LE, u128)
                if offset + 16 > data.len() {
                    let msg = "V3 snapshot truncated: expected liquidity_gross";
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
                let gross_lo = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                let gross_hi = u64::from_le_bytes(data[offset + 8..offset + 16].try_into().unwrap());
                let liquidity_gross = u128::from(gross_hi) << 64 | u128::from(gross_lo);
                offset += 16;

                // liquidity_net (16 bytes LE, i128)
                if offset + 16 > data.len() {
                    let msg = "V3 snapshot truncated: expected liquidity_net";
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
                let net_lo = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                let net_hi = u64::from_le_bytes(data[offset + 8..offset + 16].try_into().unwrap());
                let unsigned_net = u128::from(net_hi) << 64 | u128::from(net_lo);
                // SAFETY: intentional bit-pattern reinterpretation of LE bytes as signed
                #[allow(clippy::cast_possible_wrap)]
                let liquidity_net = unsigned_net as i128;
                offset += 16;

                tick_data.insert(tick_index, make_tick_info(liquidity_gross, liquidity_net));
            }

            result.insert(address, tick_data);
        }

        Ok(result)
    }

    /// Deserialize a V4 binary snapshot into `V4SnapshotData`.
    fn deserialize_v4_snapshot(data: &[u8]) -> PyResult<V4SnapshotData> {
        const MIN_HEADER: usize = 5; // version(1) + pool_manager_count(4)

        if data.len() < MIN_HEADER {
            let msg = format!(
                "V4 snapshot data too short: {} bytes (minimum {})",
                data.len(),
                MIN_HEADER
            );
            return Err(pyo3::exceptions::PyValueError::new_err(msg));
        }

        let version = data[0];
        if version != 1 {
            let msg = format!("Unsupported V4 snapshot format version: {version}");
            return Err(pyo3::exceptions::PyValueError::new_err(msg));
        }

        let pm_count = u32::from_le_bytes(data[1..5].try_into().unwrap()) as usize;
        let mut result = HashMap::with_capacity(pm_count);

        let mut offset = MIN_HEADER;
        for _ in 0..pm_count {
            // Pool manager address (20 bytes)
            if offset + 20 > data.len() {
                let msg = "V4 snapshot truncated: expected pool_manager address";
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let pm_bytes: [u8; 20] = data[offset..offset + 20].try_into().unwrap();
            let pool_manager = Address::from(pm_bytes);
            offset += 20;

            // Pool ID count (4 bytes LE)
            if offset + 4 > data.len() {
                let msg = "V4 snapshot truncated: expected pool_id_count";
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let pool_id_count = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;

            for _ in 0..pool_id_count {
                // Pool ID (32 bytes)
                if offset + 32 > data.len() {
                    let msg = "V4 snapshot truncated: expected pool_id";
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
                let pool_id: [u8; 32] = data[offset..offset + 32].try_into().unwrap();
                offset += 32;

                // Tick count (4 bytes LE)
                if offset + 4 > data.len() {
                    let msg = "V4 snapshot truncated: expected tick_count";
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
                let tick_count = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;

                let mut tick_data = HashMap::with_capacity(tick_count);
                for _ in 0..tick_count {
                    // tick_index (4 bytes LE, i32)
                    if offset + 4 > data.len() {
                        let msg = "V4 snapshot truncated: expected tick_index";
                        return Err(pyo3::exceptions::PyValueError::new_err(msg));
                    }
                    let tick_index = i32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                    offset += 4;

                    // liquidity_gross (16 bytes LE, u128)
                    if offset + 16 > data.len() {
                        let msg = "V4 snapshot truncated: expected liquidity_gross";
                        return Err(pyo3::exceptions::PyValueError::new_err(msg));
                    }
                    let gross_lo = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                    let gross_hi = u64::from_le_bytes(data[offset + 8..offset + 16].try_into().unwrap());
                    let liquidity_gross = u128::from(gross_hi) << 64 | u128::from(gross_lo);
                    offset += 16;

                    // liquidity_net (16 bytes LE, i128)
                    if offset + 16 > data.len() {
                        let msg = "V4 snapshot truncated: expected liquidity_net";
                        return Err(pyo3::exceptions::PyValueError::new_err(msg));
                    }
                    let net_lo = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                    let net_hi = u64::from_le_bytes(data[offset + 8..offset + 16].try_into().unwrap());
                    let unsigned_net = u128::from(net_hi) << 64 | u128::from(net_lo);
                    // SAFETY: intentional bit-pattern reinterpretation of LE bytes as signed
                    #[allow(clippy::cast_possible_wrap)]
                    let liquidity_net = unsigned_net as i128;
                    offset += 16;

                    tick_data.insert(tick_index, make_tick_info(liquidity_gross, liquidity_net));
                }

                result.insert((pool_manager, pool_id), tick_data);
            }
        }

        Ok(result)
    }
}

#[pymethods]
impl PyUniswapArbEngine {
    #[new]
    fn new() -> Self {
        let (result_tx, result_rx) = mpsc::unbounded_channel();
        let mut engine = UniswapEngine::new();
        engine.set_result_channel(result_tx);
        Self {
            engine: Arc::new(parking_lot::Mutex::new(engine)),
            shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pump_handle: parking_lot::Mutex::new(None),
            subscribe_state: parking_lot::Mutex::new(None),
            result_rx: Arc::new(parking_lot::Mutex::new(Some(result_rx))),
            verify_on_register: std::sync::atomic::AtomicBool::new(false),
            verify_rpc_url: parking_lot::Mutex::new(None),
            verify_provider: parking_lot::Mutex::new(None),
            verify_state_view: parking_lot::Mutex::new(None),
            verify_snapshot_block: parking_lot::Mutex::new(None),
            verify_backfill_block: parking_lot::Mutex::new(None),
            phase: std::sync::atomic::AtomicU8::new(EnginePhase::Created as u8),
            v3_snapshot: SnapshotStore::new(),
            v4_snapshot: SnapshotStore::new(),
        }
    }

    /// Load a V3 liquidity snapshot from a binary buffer.
    ///
    /// The binary format is documented in `snapshot_binary.py`:
    /// ```text
    /// [1 byte: version] [4 bytes LE: pool_count]
    /// Per pool:
    ///   [20 bytes: pool address]
    ///   [4 bytes LE: tick_count]
    ///   Per tick:
    ///     [4 bytes LE: tick_index (i32)]
    ///     [16 bytes LE: liquidity_gross (u128)]
    ///     [16 bytes LE: liquidity_net (i128)]
    /// ```
    ///
    /// Requires `Subscribed` or `SnapshotLoaded` phase.
    /// Raises `RuntimeError` if V3 snapshot already loaded.
    #[allow(clippy::needless_pass_by_value)]
    fn load_v3_snapshot(&self, data: Vec<u8>) -> PyResult<()> {
        let phase = self.current_phase();
        // Allow loading from Created (unit tests) or Subscribed/SnapshotLoaded (production)
        phase.require_before(EnginePhase::Resumed, "load_v3_snapshot")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        if self.v3_snapshot.is_loaded() {
            let msg = "Cannot load V3 snapshot: already loaded. Call clear_v3_snapshot() first.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }

        let snapshot = Self::deserialize_v3_snapshot(&data)?;
        self.v3_snapshot.load(snapshot);

        // Advance phase to SnapshotLoaded (if not already there from V4)
        if phase < EnginePhase::SnapshotLoaded {
            self.set_phase(EnginePhase::SnapshotLoaded);
        }

        Ok(())
    }

    /// Load a V4 liquidity snapshot from a binary buffer.
    ///
    /// The binary format is documented in `snapshot_binary.py`:
    /// ```text
    /// [1 byte: version] [4 bytes LE: pool_manager_count]
    /// Per pool_manager:
    ///   [20 bytes: pool_manager address]
    ///   [4 bytes LE: pool_id_count]
    ///   Per pool_id:
    ///     [32 bytes: pool_id]
    ///     [4 bytes LE: tick_count]
    ///     Per tick:
    ///       [4 bytes LE: tick_index (i32)]
    ///       [16 bytes LE: liquidity_gross (u128)]
    ///       [16 bytes LE: liquidity_net (i128)]
    /// ```
    ///
    /// Requires `Subscribed` or `SnapshotLoaded` phase.
    /// Raises `RuntimeError` if V4 snapshot already loaded.
    #[allow(clippy::needless_pass_by_value)]
    fn load_v4_snapshot(&self, data: Vec<u8>) -> PyResult<()> {
        let phase = self.current_phase();
        phase.require_before(EnginePhase::Resumed, "load_v4_snapshot")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        if self.v4_snapshot.is_loaded() {
            let msg = "Cannot load V4 snapshot: already loaded. Call clear_v4_snapshot() first.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }

        let snapshot = Self::deserialize_v4_snapshot(&data)?;
        self.v4_snapshot.load(snapshot);

        // Advance phase to SnapshotLoaded (if not already there from V3)
        if phase < EnginePhase::SnapshotLoaded {
            self.set_phase(EnginePhase::SnapshotLoaded);
        }

        Ok(())
    }

    /// Drop the stored V3 snapshot, freeing memory.
    /// Idempotent — no-op if no V3 snapshot is loaded.
    fn clear_v3_snapshot(&self) {
        self.v3_snapshot.clear();
    }

    /// Drop the stored V4 snapshot, freeing memory.
    /// Idempotent — no-op if no V4 snapshot is loaded.
    fn clear_v4_snapshot(&self) {
        self.v4_snapshot.clear();
    }

    /// Begin streaming V3 snapshot data into the engine, one pool at a time.
    ///
    /// Call `insert_v3_pool_snapshot` for each pool, then `finish_v3_snapshot`
    /// to finalize. This avoids building the entire snapshot dict in memory.
    ///
    /// Can be called in Created or Subscribed phase. Idempotent — calling again
    /// while a stream is in progress is a no-op.
    fn begin_v3_snapshot_stream(&self) -> PyResult<()> {
        let phase = self.current_phase();
        phase.require_before(EnginePhase::Resumed, "begin_v3_snapshot_stream")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        if self.v3_snapshot.is_loaded() {
            let msg = "Cannot begin V3 snapshot stream: snapshot already loaded.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }
        self.v3_snapshot.begin_load();
        Ok(())
    }

    /// Insert a single V3 pool's tick data into the in-progress snapshot stream.
    ///
    /// Args:
    ///     `pool_address`: Hex string of the pool address.
    ///     `tick_data`: Dict mapping `tick_index` (int) → (`liquidity_gross`, `liquidity_net`) tuple.
    fn insert_v3_pool_snapshot(
        &self,
        pool_address: &str,
        tick_data: &Bound<'_, pyo3::types::PyDict>,
    ) -> PyResult<()> {
        let addr = pool_address.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;

        let mut rust_tick_data = HashMap::new();
        for (py_tick, py_values) in tick_data.iter() {
            let tick_index: i32 = py_tick.extract()?;
            let values: (u128, i128) = py_values.extract()?;
            rust_tick_data.insert(tick_index, make_tick_info(values.0, values.1));
        }

        self.v3_snapshot.insert(addr, rust_tick_data)
    }

    /// Finalize the V3 snapshot stream and transition to `SnapshotLoaded` phase.
    fn finish_v3_snapshot(&self) -> PyResult<()> {
        let phase = self.current_phase();
        if !self.v3_snapshot.is_loaded() {
            let msg = "No V3 snapshot stream in progress.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }
        if phase < EnginePhase::SnapshotLoaded {
            self.set_phase(EnginePhase::SnapshotLoaded);
        }
        Ok(())
    }

    /// Begin streaming V4 snapshot data into the engine, one pool at a time.
    fn begin_v4_snapshot_stream(&self) -> PyResult<()> {
        let phase = self.current_phase();
        phase.require_before(EnginePhase::Resumed, "begin_v4_snapshot_stream")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        if self.v4_snapshot.is_loaded() {
            let msg = "Cannot begin V4 snapshot stream: snapshot already loaded.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }
        self.v4_snapshot.begin_load();
        Ok(())
    }

    /// Insert a single V4 pool's tick data into the in-progress snapshot stream.
    ///
    /// Args:
    ///     `pool_manager`: Hex string of the pool manager address.
    ///     `pool_id_hex`: Hex string of the 32-byte pool ID.
    ///     `tick_data`: Dict mapping `tick_index` (int) → (`liquidity_gross`, `liquidity_net`) tuple.
    fn insert_v4_pool_snapshot(
        &self,
        pool_manager: &str,
        pool_id_hex: &str,
        tick_data: &Bound<'_, pyo3::types::PyDict>,
    ) -> PyResult<()> {
        let pm_addr = pool_manager.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool_manager address: {e}"))
        })?;
        let pool_id = crate::hex_utils::decode_32byte_hex(pool_id_hex)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;

        let mut rust_tick_data = HashMap::new();
        for (py_tick, py_values) in tick_data.iter() {
            let tick_index: i32 = py_tick.extract()?;
            let values: (u128, i128) = py_values.extract()?;
            rust_tick_data.insert(tick_index, make_tick_info(values.0, values.1));
        }

        self.v4_snapshot.insert((pm_addr, pool_id), rust_tick_data)
    }

    /// Finalize the V4 snapshot stream and transition to `SnapshotLoaded` phase.
    fn finish_v4_snapshot(&self) -> PyResult<()> {
        let phase = self.current_phase();
        if !self.v4_snapshot.is_loaded() {
            let msg = "No V4 snapshot stream in progress.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }
        if phase < EnginePhase::SnapshotLoaded {
            self.set_phase(EnginePhase::SnapshotLoaded);
        }
        Ok(())
    }

    /// Load a V3 liquidity snapshot from a Python dict.
    ///
    /// The dict maps pool address (hex string) → tick data dict,
    /// where tick data maps `tick_index` (int) → (`liquidity_gross`, `liquidity_net`) tuple.
    ///
    /// This is the fast path — no intermediate binary serialization in Python.
    /// The Rust side iterates the `PyO3` dict and builds the internal `HashMap` directly.
    fn load_v3_snapshot_from_py(&self, py_data: &Bound<'_, pyo3::types::PyDict>) -> PyResult<()> {
        let phase = self.current_phase();
        phase.require_before(EnginePhase::Resumed, "load_v3_snapshot_from_py")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        if self.v3_snapshot.is_loaded() {
            let msg = "Cannot load V3 snapshot: already loaded. Call clear_v3_snapshot() first.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }

        let mut result = HashMap::new();
        for (py_addr, py_tick_dict) in py_data.iter() {
            let addr_str: String = py_addr.extract()?;
            let address = addr_str.parse::<Address>().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
            })?;

            let tick_dict = py_tick_dict.cast::<pyo3::types::PyDict>().map_err(|_| {
                pyo3::exceptions::PyTypeError::new_err("tick_data must be a dict")
            })?;

            let mut tick_data = HashMap::new();
            for (py_tick, py_values) in tick_dict.iter() {
                let tick_index: i32 = py_tick.extract()?;
                let values: (u128, i128) = py_values.extract()?;
                tick_data.insert(tick_index, make_tick_info(values.0, values.1));
            }
            result.insert(address, tick_data);
        }

        self.v3_snapshot.load(result);
        if phase < EnginePhase::SnapshotLoaded {
            self.set_phase(EnginePhase::SnapshotLoaded);
        }
        Ok(())
    }

    /// Load a V4 liquidity snapshot from a Python dict.
    ///
    /// The dict maps `pool_manager` address (hex) → inner dict,
    /// where inner dict maps `pool_id` (hex) → tick data dict,
    /// and tick data maps `tick_index` (int) → (`liquidity_gross`, `liquidity_net`) tuple.
    fn load_v4_snapshot_from_py(&self, py_data: &Bound<'_, pyo3::types::PyDict>) -> PyResult<()> {
        let phase = self.current_phase();
        phase.require_before(EnginePhase::Resumed, "load_v4_snapshot_from_py")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        if self.v4_snapshot.is_loaded() {
            let msg = "Cannot load V4 snapshot: already loaded. Call clear_v4_snapshot() first.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }

        let mut result = HashMap::new();
        for (py_pm, py_pool_dict) in py_data.iter() {
            let pm_str: String = py_pm.extract()?;
            let pm_address = pm_str.parse::<Address>().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!(
                    "Invalid pool_manager address: {e}"
                ))
            })?;

            let pool_dict = py_pool_dict.cast::<pyo3::types::PyDict>().map_err(|_| {
                pyo3::exceptions::PyTypeError::new_err("pool_manager value must be a dict")
            })?;

            for (py_pool_id, py_tick_dict) in pool_dict.iter() {
                let pool_id_hex: String = py_pool_id.extract()?;
                let pool_id = crate::hex_utils::decode_32byte_hex(&pool_id_hex)
                    .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;

                let tick_dict = py_tick_dict.cast::<pyo3::types::PyDict>().map_err(|_| {
                    pyo3::exceptions::PyTypeError::new_err("tick_data must be a dict")
                })?;

                let mut tick_data = HashMap::new();
                for (py_tick, py_values) in tick_dict.iter() {
                    let tick_index: i32 = py_tick.extract()?;
                    let values: (u128, i128) = py_values.extract()?;
                    tick_data.insert(tick_index, make_tick_info(values.0, values.1));
                }
                result.insert((pm_address, pool_id), tick_data);
            }
        }

        self.v4_snapshot.load(result);
        if phase < EnginePhase::SnapshotLoaded {
            self.set_phase(EnginePhase::SnapshotLoaded);
        }
        Ok(())
    }

    /// Register a V2 pool by contract address and initial reserves.
    /// Returns the forward `pool_id`. The reverse `pool_id` is `forward_id + 1`.
    #[pyo3(signature = (address, reserve0, reserve1, gamma_numer, fee_denom))]
    fn register_v2_pool(
        &self,
        address: &str,
        reserve0: &Bound<'_, pyo3::PyAny>,
        reserve1: &Bound<'_, pyo3::PyAny>,
        gamma_numer: u64,
        fee_denom: u64,
    ) -> PyResult<u64> {
        let addr: Address = address.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}"))
        })?;
        let r0 = crate::alloy_py::extract_python_u256(reserve0)?;
        let r1 = crate::alloy_py::extract_python_u256(reserve1)?;

        // ADR-003: V2 state lives in BotCore. The engine delegates registration
        // to the core and returns the single `pool_id` (orientation is selected
        // at solve time via `zero_for_one`, not by a separate reverse id).
        Ok(self.engine.lock().register_v2_pool(addr, r0, r1, gamma_numer, fee_denom))
    }

    /// Register a V3 pool by contract address and initial state.
    /// Returns the pool key for use in path registration.
    ///
    /// Tick data is resolved automatically from the stored V3 snapshot:
    /// - Pool found in snapshot → `Tracked` coverage (`tick_data` consumed via `remove()`)
    /// - Pool not in snapshot → `Sparse` coverage (empty `tick_data`)
    ///
    /// The buffer is always applied (Plan 098: snapshot data is always stale
    /// from the DB, so the buffer must bring it forward).
    #[allow(clippy::too_many_lines)]
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (address, token0, token1, fee, tick_spacing, factory, sqrt_price_x96, liquidity, tick, block=0))]
    fn register_v3_pool(
        &self,
        address: &str,
        token0: &str,
        token1: &str,
        fee: u32,
        tick_spacing: i32,
        factory: &str,
        sqrt_price_x96: &Bound<'_, pyo3::PyAny>,
        liquidity: u128,
        tick: i32,
        block: u64,
    ) -> PyResult<u64> {
        // No phase check on registration — the engine lock serializes access.
        // Registration is allowed in any phase.

        let addr = address.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;
        let t0 = token0.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid token0 address: {e}"))
        })?;
        let t1 = token1.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid token1 address: {e}"))
        })?;
        let fac = factory.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid factory address: {e}"))
        })?;
        let sp = crate::alloy_py::extract_python_u256(sqrt_price_x96)?;

        // Look up tick_data from stored V3 snapshot (one-way transfer via remove)
        let (rust_tick_data, coverage) = self.v3_snapshot.take(&addr);
        let is_tracked = coverage == PoolTickCoverage::Tracked;

        // Clone tick_data for snapshot verification before it's moved into register_pool.
        let tick_data_for_snapshot_verify = if is_tracked { Some(rust_tick_data.clone()) } else { None };

        let (key, backfill_verify_snapshot) = register_with_cl_buffers(
            &self.engine,
            |engine| {
                engine.register_v3_pool(&RegisterV3PoolParams {
                    address: addr,
                    token0: t0,
                    token1: t1,
                    fee,
                    tick_spacing,
                    factory: fac,
                    sqrt_price_x96: sp,
                    liquidity,
                    tick,
                    tick_data: rust_tick_data,
                    update_block: block,
                    coverage,
                })
            },
            |engine| engine.core.lock().apply_backfill_buffer_v3(&addr),
            |engine, key| {
                is_tracked
                    .then(|| engine.core.lock().get_v3_pool(*key).cloned())
                    .flatten()
            },
            |engine| engine.core.lock().apply_pump_buffer_v3(&addr),
        );

        if is_tracked && self.verify_on_register.load(std::sync::atomic::Ordering::Relaxed) {
            let rpc_url = self.verify_rpc_url.lock().clone();
            let snapshot_block = *self.verify_snapshot_block.lock();
            let backfill_block = *self.verify_backfill_block.lock();
            let label = address.to_string();

            let verify_snapshot = |provider: &crate::provider::AlloyProvider, block: u64| -> PyResult<()> {
                let Some(ref td) = tick_data_for_snapshot_verify else {
                    return Ok(());
                };
                let runtime = crate::runtime::get_runtime();
                let addr_str = address.to_string();
                runtime.block_on(async {
                    crate::bot_core::liquidity_verifier::verify_v3_liquidity_map(
                        provider, addr, td, block,
                    )
                    .await
                    .map_err(|mismatch| {
                        pyo3::exceptions::PyRuntimeError::new_err(format!(
                            "V3 pool {addr_str} at snapshot block {block}: tick data mismatch: {mismatch}"
                        ))
                    })
                })
            };

            let verify_backfill = |provider: &crate::provider::AlloyProvider, block: u64| -> PyResult<()> {
                let Some(ref pool_snapshot) = backfill_verify_snapshot else {
                    return Ok(());
                };
                let mut pool_map = HashMap::new();
                pool_map.insert(key, pool_snapshot.clone());

                let runtime = crate::runtime::get_runtime();
                let addr_str = address.to_string();
                runtime.block_on(async {
                    crate::bot_core::liquidity_verifier::verify_v3_pools(
                        provider, Address::ZERO, &pool_map, Some(block),
                    )
                    .await
                    .map_err(|mismatch| {
                        pyo3::exceptions::PyRuntimeError::new_err(format!(
                            "V3 pool {addr_str} at backfill block {block}: tick data mismatch: {mismatch}"
                        ))
                    })
                })
            };

            run_cl_verification(
                rpc_url,
                &self.verify_provider,
                snapshot_block,
                backfill_block,
                &label,
                verify_snapshot,
                verify_backfill,
            )?;
        }

        Ok(key)
    }

    /// Register a V4 pool with the engine.
    ///
    /// Hook filtering: pools with amount-modifying hook flags (`BEFORE_SWAP`,
    /// `AFTER_SWAP`, `BEFORE_SWAP_RETURNS_DELTA`, `AFTER_SWAP_RETURNS_DELTA`)
    /// are rejected. Dynamic-fee pools (fee=0x100000) are also rejected.
    ///
    /// Tick data is resolved automatically from the stored V4 snapshot:
    /// - Pool found in snapshot → `Tracked` coverage (`tick_data` consumed via `remove()`)
    /// - Pool not in snapshot → `Sparse` coverage (empty `tick_data`)
    ///
    /// The buffer is always applied (Plan 098: snapshot data is always stale
    /// from the DB, so the buffer must bring it forward).
    ///
    /// Returns the forward pool key for use in path registration,
    /// or raises `ValueError` if the pool is excluded.
    #[allow(clippy::too_many_lines)]
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (pool_manager, pool_id_hex, currency0, currency1, fee, tick_spacing, hook_flags, sqrt_price_x96, liquidity, tick, block=0))]
    fn register_v4_pool(
        &self,
        pool_manager: &str,
        pool_id_hex: &str,
        currency0: &str,
        currency1: &str,
        fee: u32,
        tick_spacing: i32,
        hook_flags: u16,
        sqrt_price_x96: &Bound<'_, pyo3::PyAny>,
        liquidity: u128,
        tick: i32,
        block: u64,
    ) -> PyResult<u64> {
        // No phase check on registration — the engine lock serializes access.
        // Registration is allowed in any phase.

        let pm = pool_manager.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool_manager address: {e}"))
        })?;

        // Decode pool_id from hex string (e.g. "0x1234...") to [u8; 32]
        let pool_id = hex_string_to_pool_id(pool_id_hex)?;

        let c0 = currency0.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid currency0 address: {e}"))
        })?;
        let c1 = currency1.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid currency1 address: {e}"))
        })?;
        let sp = crate::alloy_py::extract_python_u256(sqrt_price_x96)?;

        // Look up tick_data from stored V4 snapshot (one-way transfer via remove)
        let (rust_tick_data, coverage) = self.v4_snapshot.take(&(pm, pool_id));
        let is_tracked = coverage == PoolTickCoverage::Tracked;

        // Clone tick_data for snapshot verification before it's moved into register_pool.
        let tick_data_for_snapshot_verify = if is_tracked { Some(rust_tick_data.clone()) } else { None };

        let (key, backfill_verify_snapshot) = register_with_cl_buffers(
            &self.engine,
            |engine| -> Result<u64, pyo3::PyErr> {
                engine
                    .v4_engine()
                    .register_pool(RegisterV4PoolParams {
                        pool_manager: pm,
                        pool_id,
                        pool_key: crate::optimizers::v4_block_engine::V4PoolKey {
                            currency0: c0,
                            currency1: c1,
                            fee,
                            tick_spacing,
                            hooks: Address::ZERO, // Not needed for solving; hook filtering already done
                        },
                        hook_flags,
                        sqrt_price_x96: sp,
                        liquidity,
                        tick,
                        tick_data: rust_tick_data,
                        update_block: block,
                        coverage,
                    })
                    .map_err(pyo3::exceptions::PyValueError::new_err)
            },
            |engine| engine.v4_engine().apply_backfill_buffer(pm, pool_id),
            |engine, key| {
                let Ok(key) = key else {
                    return None;
                };
                is_tracked
                    .then(|| engine.v4_engine_ref().get_pool(*key).cloned())
                    .flatten()
            },
            |engine| engine.v4_engine().apply_pump_buffer(pm, pool_id),
        );

        let key = key?;

        if is_tracked && self.verify_on_register.load(std::sync::atomic::Ordering::Relaxed) {
            let rpc_url = self.verify_rpc_url.lock().clone();
            let state_view = *self.verify_state_view.lock();
            let snapshot_block = *self.verify_snapshot_block.lock();
            let backfill_block = *self.verify_backfill_block.lock();
            let label = pool_id_hex.to_string();

            let verify_snapshot = |provider: &crate::provider::AlloyProvider, block: u64| -> PyResult<()> {
                let (Some(sv), Some(ref td)) = (state_view, tick_data_for_snapshot_verify) else {
                    return Ok(());
                };
                let runtime = crate::runtime::get_runtime();
                let pool_id_str = pool_id_hex.to_string();
                runtime.block_on(async {
                    crate::bot_core::liquidity_verifier::verify_v4_liquidity_map(
                        provider, sv, pool_id, td, block,
                    )
                    .await
                    .map_err(|mismatch| {
                        pyo3::exceptions::PyRuntimeError::new_err(format!(
                            "V4 pool {pool_id_str} at snapshot block {block}: tick data mismatch: {mismatch}"
                        ))
                    })
                })
            };

            let verify_backfill = |provider: &crate::provider::AlloyProvider, block: u64| -> PyResult<()> {
                let (Some(sv), Some(ref pool_snapshot)) = (state_view, backfill_verify_snapshot) else {
                    return Ok(());
                };
                let mut pool_map = HashMap::new();
                pool_map.insert(key, pool_snapshot.clone());

                let runtime = crate::runtime::get_runtime();
                let pool_id_str = pool_id_hex.to_string();
                runtime.block_on(async {
                    crate::bot_core::liquidity_verifier::verify_v4_pools(
                        provider, sv, &pool_map, Some(block),
                    )
                    .await
                    .map_err(|mismatch| {
                        pyo3::exceptions::PyRuntimeError::new_err(format!(
                            "V4 pool {pool_id_str} at backfill block {block}: tick data mismatch: {mismatch}"
                        ))
                    })
                })
            };

            run_cl_verification(
                rpc_url,
                &self.verify_provider,
                snapshot_block,
                backfill_block,
                &label,
                verify_snapshot,
                verify_backfill,
            )?;
        }

        Ok(key)
    }

    /// Register a mixed arbitrage path.
    ///
    /// Each entry is (`hop_type_str`, `pool_key`, `zero_for_one`) where
    /// `hop_type_str` is "V2" or "V3".
    #[pyo3(signature = (pool_refs))]
    fn register_path(&self, pool_refs: &Bound<'_, PyList>) -> PyResult<u64> {
        let mut rust_refs = Vec::with_capacity(pool_refs.len());
        for item in pool_refs.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 3 {
                let msg = format!(
                    "Expected 3-tuple (hop_type, pool_key, zero_for_one), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let hop_type_str: String = tuple.get_item(0)?.extract()?;
            let pool_key: u64 = tuple.get_item(1)?.extract()?;
            let zero_for_one: bool = tuple.get_item(2)?.extract()?;

            let hop_type = match hop_type_str.as_str() {
                "V2" => HopType::V2,
                "V3" => HopType::V3,
                "V4" => HopType::V4,
                _ => {
                    let msg = format!("Invalid hop_type: {hop_type_str}. Expected 'V2', 'V3', or 'V4'");
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
            };

            rust_refs.push(MixedPoolRef {
                hop_type,
                pool_key,
                zero_for_one,
            });
        }

        if rust_refs.len() < 2 {
            let msg = format!("Need at least 2 pool refs, got {}", rust_refs.len());
            return Err(pyo3::exceptions::PyValueError::new_err(msg));
        }

        Ok(self.engine.lock().register_path(rust_refs))
    }

    /// Register a mixed arbitrage path and eagerly solve it.
    ///
    /// Unlike `register_path`, this method also resolves and solves the path
    /// immediately, appending any profitable result to the engine's results.
    /// Used when the engine is already running (after the pump has started)
    /// so that new paths are immediately available to `latest_results()`.
    #[pyo3(signature = (pool_refs))]
    fn register_and_solve_path(&self, pool_refs: &Bound<'_, PyList>) -> PyResult<u64> {
        let mut rust_refs = Vec::with_capacity(pool_refs.len());
        for item in pool_refs.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 3 {
                let msg = format!(
                    "Expected 3-tuple (hop_type, pool_key, zero_for_one), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let hop_type_str: String = tuple.get_item(0)?.extract()?;
            let pool_key: u64 = tuple.get_item(1)?.extract()?;
            let zero_for_one: bool = tuple.get_item(2)?.extract()?;

            let hop_type = match hop_type_str.as_str() {
                "V2" => HopType::V2,
                "V3" => HopType::V3,
                "V4" => HopType::V4,
                _ => {
                    let msg = format!("Invalid hop_type: {hop_type_str}. Expected 'V2', 'V3', or 'V4'");
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
            };

            rust_refs.push(MixedPoolRef {
                hop_type,
                pool_key,
                zero_for_one,
            });
        }

        if rust_refs.len() < 2 {
            let msg = format!("Need at least 2 pool refs, got {}", rust_refs.len());
            return Err(pyo3::exceptions::PyValueError::new_err(msg));
        }

        Ok(self.engine.lock().register_and_solve_path(rust_refs))
    }

    /// Start the engine. Freezes registration and spawns the unified pump.
    ///
    /// The pump subscribes to both block headers and log events via WS.
    /// Logs are buffered and processed atomically when the next block header
    /// arrives. If no logs are received for a block, `eth_getLogs` is used to
    /// verify. A 60s timeout triggers backfill for the missing range.
    ///
    /// After calling `start()`, the engine processes events autonomously.
    /// Python reads results via the result batch channel (`async for`).
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (rpc_url))]
    fn start(&self, rpc_url: String) -> PyResult<()> {
        // Spawn the unified pump
        let engine = Arc::clone(&self.engine);
        let shutdown = Arc::clone(&self.shutdown);
        let handle = crate::optimizers::uniswap_engine_pump::UniswapEnginePump::spawn(
            rpc_url,
            engine,
            &shutdown,
        )
        .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        *self.pump_handle.lock() = Some(handle);

        Ok(())
    }

    /// Subscribe phase: open WS connections and observe until first complete block.
    ///
    /// Returns the first observed block number. Python should:
    /// 1. Run backfill up to the returned block number
    /// 2. Call `resume()` to begin normal processing
    ///
    /// A "complete" block is one where both a `newHeads` notification and at
    /// least one log for the same block have been received. This guarantees
    /// the logs subscription did not miss the start of the block.
    /// No events are buffered during subscribe — the backfill is the sole
    /// authority for the gap between snapshot and WS start.
    ///
    /// Raises `RuntimeError` if the pump is already started or subscribed.
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (rpc_url))]
    fn subscribe(
        &self,
        rpc_url: String,
    ) -> PyResult<u64> {
        // Phase check: must be Created
        let phase = self.current_phase();
        phase.require(EnginePhase::Created, "subscribe")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        // Ensure we're not already running
        if self.pump_handle.lock().is_some() {
            let msg = "Cannot subscribe: pump is already started. Call stop() first.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }
        if self.subscribe_state.lock().is_some() {
            let msg = "Cannot subscribe: already subscribed. Call resume() first.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }

        let engine = Arc::clone(&self.engine);
        let shutdown = Arc::clone(&self.shutdown);

        // Run the subscribe phase synchronously (blocks Python until first block observed)
        let runtime = crate::runtime::get_runtime();
        let subscribe_result = runtime
            .block_on(async {
                crate::optimizers::uniswap_engine_pump::UniswapEnginePump::subscribe(
                    &rpc_url,
                    engine,
                    shutdown,
                )
                .await
            })
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        let (pump, state) = subscribe_result;

        // Store the subscribe state for resume()
        *self.subscribe_state.lock() = Some(PySubscribeState {
            pump,
            first_block: state.first_block,
            combined_stream: state
                .combined_stream
                .expect("subscribe() always returns a stream"),
        });

        // Advance phase
        self.set_phase(EnginePhase::Subscribed);

        Ok(state.first_block)
    }

    /// Backfill Mint/Burn/ModifyLiquidity events from the last DB snapshot
    /// block to the first WS block observed during `subscribe()`.
    ///
    /// Must be called after `subscribe()`, before `resume()`. Uses
    /// `eth_getLogs` to fetch events for the gap between the DB snapshot
    /// and the live WS connection, then applies them to the V3/V4 engines
    /// via `backfill_logs()`.
    ///
    /// This ensures that when pools are registered (with `tick_data` from the
    /// DB snapshot), any liquidity changes between the snapshot block and
    /// the current chain head are reflected in the Rust engine's state.
    ///
    /// Args:
    ///     `rpc_url`: HTTP RPC endpoint for `eth_getLogs` requests
    ///     `chunk_size`: Number of blocks per `eth_getLogs` request (default 2000)
    ///
    /// Returns the number of blocks backfilled (0 if snapshot is current).
    #[pyo3(signature = (rpc_url, snapshot_block, chunk_size=2000))]
    fn backfill_from_snapshot(&self, rpc_url: &str, snapshot_block: u64, chunk_size: u64) -> PyResult<u64> {
        // Phase check: must be at least SnapshotLoaded
        let phase = self.current_phase();
        phase.require(EnginePhase::SnapshotLoaded, "backfill_from_snapshot")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        // Ensure no double-backfill
        phase.require_before(EnginePhase::Backfilled, "backfill_from_snapshot")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        // Ensure subscribe() was called — we need the first WS block
        let first_ws_block = {
            let state_lock = self.subscribe_state.lock();
            if let Some(s) = state_lock.as_ref() { s.first_block } else {
                let msg = "Cannot backfill: subscribe() has not been called. Call subscribe() first.";
                return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
            }
        };

        if snapshot_block == 0 {
            log::warn!("backfill_from_snapshot: snapshot_block is 0, skipping");
            return Ok(0);
        }

        if snapshot_block >= first_ws_block {
            log::info!(
                "backfill_from_snapshot: snapshot at {snapshot_block} >= WS block {first_ws_block}, nothing to backfill"
            );
            return Ok(0);
        }

        let from_block = snapshot_block + 1;
        // Backfill up to (first_ws_block - 1) to avoid overlap with
        // WS events that the pump already captured during subscribe().
        let to_block = first_ws_block - 1;
        let total_blocks = to_block - from_block + 1;

        log::info!(
            "backfill_from_snapshot: fetching events from block {from_block} to {to_block} ({total_blocks} blocks, chunk_size={chunk_size})"
        );

        // Create an HTTP provider for eth_getLogs
        let runtime = crate::runtime::get_runtime();
        let provider = runtime.block_on(async {
            crate::provider::AlloyProvider::new(rpc_url, 3)
                .await
                .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("Failed to create provider: {e}")))
        })?;

        let provider_arc = provider.provider_arc();

        // Fetch and apply logs in paginated chunks
        let mut total_logs = 0usize;
        let mut chunk_start = from_block;
        while chunk_start <= to_block {
            let chunk_end = (chunk_start + chunk_size - 1).min(to_block);

            let filter = crate::optimizers::uniswap_engine_pump::build_backfill_filter(
                chunk_start,
                chunk_end,
            );

            let logs = runtime.block_on(async {
                provider_arc.get_logs(&filter).await
                    .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(
                        format!("eth_getLogs failed for blocks {chunk_start}-{chunk_end}: {e}")
                    ))
            })?;

            let chunk_log_count = logs.len();
            total_logs += chunk_log_count;

            // Apply to the engines — process_backfill_logs splits V3/V4
            {
                let mut engine = self.engine.lock();
                engine.process_backfill_logs(&logs, chunk_end);
            }

            log::info!(
                "backfill_from_snapshot: blocks {chunk_start}-{chunk_end}: {chunk_log_count} logs applied"
            );

            chunk_start = chunk_end + 1;
        }

        log::info!(
            "backfill_from_snapshot: complete — {total_logs} total logs applied across {total_blocks} blocks"
        );

        // Capture verification blocks. These are used by verify_on_register
        // to check tick data at two boundaries:
        // 1. snapshot_block: raw DB tick_data (before buffer) vs on-chain
        // 2. backfill block: engine state (after buffer) vs on-chain
        *self.verify_snapshot_block.lock() = Some(snapshot_block);
        let backfill_block = self.engine.lock().last_processed_block().unwrap_or(to_block);
        *self.verify_backfill_block.lock() = Some(backfill_block);

        // Advance phase
        self.set_phase(EnginePhase::Backfilled);

        Ok(total_blocks)
    }

    /// Resume phase: begin normal pump processing.
    ///
    /// Must be called after `subscribe()`. Takes the WS stream from the
    /// subscribe phase and begins processing events on block boundaries.
    ///
    /// After calling `resume()`, the engine processes events autonomously.
    /// Python reads results via `latest_results()` and awaits new blocks
    /// via `wait_for_block()`.
    ///
    /// Raises `RuntimeError` if `subscribe()` has not been called first.
    fn resume(&self, _py: Python<'_>) -> PyResult<()> {
        // Phase check: must be at least SnapshotLoaded (can skip backfill)
        let phase = self.current_phase();
        phase.require(EnginePhase::SnapshotLoaded, "resume")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        // No double-resume
        if phase == EnginePhase::Resumed {
            let msg = "Cannot resume: engine is already in Resumed phase.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }

        let subscribe_state = self.subscribe_state.lock().take();
        let state = subscribe_state.ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(
                "Cannot resume: subscribe() has not been called. Call subscribe() first.",
            )
        })?;

        let mut pump = state.pump;
        let first_block = state.first_block;
        let combined_stream = state.combined_stream;

        // Spawn the resume task on the Tokio runtime
        let handle = crate::runtime::get_runtime().spawn(async move {
            let inner_state =
                crate::optimizers::uniswap_engine_pump::SubscribeState {
                    first_block,
                    first_timestamp: 0,
                    combined_stream: Some(combined_stream),
                };
            pump.resume_from_subscribe(inner_state).await;
        });

        *self.pump_handle.lock() = Some(handle);

        // Advance phase
        self.set_phase(EnginePhase::Resumed);

        Ok(())
    }

    /// Last block number processed by `process_block` or `process_logs`.
    /// Returns `None` if no block has been processed yet.
    fn last_processed_block(&self) -> Option<u64> {
        self.engine.lock().last_processed_block()
    }

    /// Set the last processed block manually after Python backfill.
    #[pyo3(signature = (block))]
    fn set_last_processed_block(&self, block: u64) {
        self.engine.lock().set_last_processed_block(block);
    }

    /// Resolve and solve all registered paths.
    ///
    /// Called to populate results for the first time (replaces the
    /// removed `freeze()` + `initial_solve()`). Subsequent `process_logs`
    /// calls use dependency tracking to only re-solve affected paths.
    fn solve_all_paths(&self, block_number: u64) {
        self.engine.lock().solve_all_paths(block_number);
    }

    /// Set the maximum age (in blocks) for buffered liquidity events.
    ///
    /// Applies to V3 and V4 sub-engine buffers. Pass `None` for unbounded
    /// (no automatic expiry). Events older than `current_block - max_age`
    /// are expired during `process_block`.
    #[pyo3(signature = (max_age))]
    fn set_event_buffer_max_age(&self, max_age: Option<u64>) {
        self.engine.lock().set_event_buffer_max_age(max_age);
    }

    /// Discard all buffered liquidity events for all unregistered pools.
    fn flush_event_buffer(&self) {
        self.engine.lock().flush_event_buffer();
    }

    /// Number of registered V2 pools.
    fn v2_pool_count(&self) -> usize {
        self.engine.lock().v2_pool_count()
    }

    /// Number of registered V3 pools.
    fn v3_pool_count(&self) -> usize {
        self.engine.lock().v3_pool_count()
    }

    /// Number of registered V4 pools.
    fn v4_pool_count(&self) -> usize {
        self.engine.lock().v4_pool_count()
    }

    /// Debug: return the number of buffered liquidity events for a V3 pool address.
    fn debug_v3_buffer_count(&self, pool_address: &str) -> PyResult<usize> {
        let addr = pool_address.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;
        let engine = self.engine.lock();
        let count = engine.core.lock().buffered_v3_event_count(&addr);
        Ok(count)
    }

    /// Debug: return the engine's tick data for a V3 pool address as a Python dict.
    /// Maps `tick_index` (int) → (`liquidity_gross`: int, `liquidity_net`: int) tuple.
    /// Returns None if the pool is not registered.
    fn debug_v3_tick_data<'py>(&self, py: Python<'py>, pool_address: &str) -> PyResult<Option<Bound<'py, pyo3::types::PyDict>>> {
        let addr = pool_address.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;
        let tick_data = {
            let engine = self.engine.lock();
            let core = engine.core.lock();
            let Some(key) = core.pool_id_by_address(&addr) else {
                return Ok(None);
            };
            let Some(pool) = core.get_v3_pool(key) else {
                return Ok(None);
            };
            pool.tick_data.clone()
        };

        let dict = pyo3::types::PyDict::new(py);
        for (&tick_idx, info) in &tick_data {
            let lg = info.liquidity_gross.to::<u128>();
            let ln: i128 = info.liquidity_net.try_into().unwrap_or(0i128);
            dict.set_item(tick_idx, (lg, ln))?;
        }
        Ok(Some(dict))
    }

    /// Number of registered paths.
    fn path_count(&self) -> usize {
        self.engine.lock().path_count()
    }

    /// Snapshot the engine-owned state for every hop in a registered path.
    ///
    /// This is a diagnostic helper for investigating simulation failures.
    /// Engine state is captured while the engine lock is held; the lock is
    /// released before any RPC calls. The `rpc_url` argument is accepted for
    /// forward compatibility with on-chain comparison (Slice 3) but is
    /// currently ignored.
    ///
    /// Returns a Python `dict` containing `path_id`, `path_type`, `solve_block`,
    /// and a `hops` list with per-hop engine state.
    ///
    /// Raises `KeyError` if `path_id` is not registered.
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (path_id, rpc_url=None))]
    fn diagnostic_inspect_path(
        &self,
        py: Python<'_>,
        path_id: u64,
        rpc_url: Option<String>,
    ) -> PyResult<pyo3::Py<pyo3::PyAny>> {
        // 1. Snapshot engine-owned state under the lock.
        let engine = self.engine.lock();
        let snapshot = engine.diagnostic_path_state(path_id);
        drop(engine);

        let Some(mut snapshot) = snapshot else {
            return Err(pyo3::exceptions::PyKeyError::new_err(format!(
                "path_id {path_id} is not registered"
            )));
        };

        // 2. Optionally fetch on-chain state and compute diffs.
        let rpc_url = rpc_url.or_else(|| self.verify_rpc_url.lock().clone());
        if let Some(rpc_url) = rpc_url {
            let runtime = crate::runtime::get_runtime();
            let state_view = *self.verify_state_view.lock();

            match runtime.block_on(crate::provider::AlloyProvider::new(&rpc_url, 3)) {
                Ok(provider) => {
                    if let Err(e) =
                        runtime.block_on(snapshot.fetch_onchain(&provider, state_view))
                    {
                        eprintln!(
                            "[diagnostic_inspect_path] on-chain fetch failed for path {path_id}: {e}"
                        );
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[diagnostic_inspect_path] failed to create provider for path {path_id}: {e}"
                    );
                }
            }
        }

        // 3. Convert the snapshot to a Python dict via JSON round-trip.
        let json = serde_json::to_string(&snapshot).map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!(
                "diagnostic_inspect_path: serialization failed: {e}"
            ))
        })?;

        let json_module = py.import("json").map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!(
                "diagnostic_inspect_path: failed to import json: {e}"
            ))
        })?;
        let dict = json_module.getattr("loads")?.call1((json,))?;
        Ok(dict.unbind())
    }

    /// Verify all V3 and V4 pool liquidity maps against on-chain state.
    ///
    /// Calls `TickLens` for V3 pools and `StateView` for V4 pools. Compares
    /// `sqrtPriceX96`, `tick`, `liquidity`, and every tick's
    /// `(liquidityGross, liquidityNet)`.
    ///
    /// Raises `RuntimeError` on the FIRST mismatch. The bot must not operate
    /// with stale tick data — fail fast.
    ///
    /// Args:
    ///     `rpc_url`: RPC endpoint URL (WS or HTTP).
    ///     `tick_lens_address`: Deployed `TickLens` contract address (hex string).
    ///     `state_view_address`: Deployed `StateView` contract address (hex string).
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (rpc_url, tick_lens_address, state_view_address, block_number))]
    fn verify_liquidity_maps(
        &self,
        rpc_url: String,
        tick_lens_address: String,
        state_view_address: String,
        block_number: Option<u64>,
    ) -> PyResult<()> {
        let tick_lens: Address = tick_lens_address.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid tick_lens address: {e}"))
        })?;
        let state_view: Address = state_view_address.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid state_view address: {e}"))
        })?;

        let mut engine = self.engine.lock();
        let v3_pools = engine.core.lock().v3_pools_snapshot();
        let v4_pools = engine.v4_engine().pools_snapshot();
        drop(engine); // Release lock before async I/O

        let runtime = crate::runtime::get_runtime();

        let provider = runtime.block_on(async {
            crate::provider::AlloyProvider::new(&rpc_url, 3).await.map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "verify_liquidity_maps: failed to create provider: {e}"
                ))
            })
        })?;

        // Verify V3 pools
        let v3_result = runtime.block_on(async {
            crate::bot_core::liquidity_verifier::verify_v3_pools(
                &provider, tick_lens, &v3_pools, block_number,
            )
            .await
        });
        if let Err(mismatch) = v3_result {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "Liquidity map verification FAILED: {mismatch}"
            )));
        }

        // Verify V4 pools
        let v4_result = runtime.block_on(async {
            crate::bot_core::liquidity_verifier::verify_v4_pools(
                &provider, state_view, &v4_pools, block_number,
            )
            .await
        });
        if let Err(mismatch) = v4_result {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "Liquidity map verification FAILED: {mismatch}"
            )));
        }

        Ok(())
    }

    /// Verify V3 liquidity maps only, at a specific block.
    ///
    /// Same as `verify_liquidity_maps` but only checks V3 pools.
    /// Useful for verifying against a V3-specific snapshot block.
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (rpc_url, block_number))]
    fn verify_v3_liquidity_maps(
        &self,
        rpc_url: String,
        block_number: Option<u64>,
    ) -> PyResult<()> {
        let engine = self.engine.lock();
        let v3_pools = engine.core.lock().v3_pools_snapshot();
        drop(engine);

        let runtime = crate::runtime::get_runtime();
        let provider = runtime.block_on(async {
            crate::provider::AlloyProvider::new(&rpc_url, 3).await.map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "verify_v3_liquidity_maps: failed to create provider: {e}"
                ))
            })
        })?;

        // TickLens address not used (V3 calls pool.ticks() directly)
        let tick_lens = Address::ZERO;
        let v3_result = runtime.block_on(async {
            crate::bot_core::liquidity_verifier::verify_v3_pools(
                &provider, tick_lens, &v3_pools, block_number,
            )
            .await
        });
        if let Err(mismatch) = v3_result {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "V3 liquidity map verification FAILED: {mismatch}"
            )));
        }

        Ok(())
    }

    /// Verify V4 liquidity maps only, at a specific block.
    ///
    /// Same as `verify_liquidity_maps` but only checks V4 pools.
    /// Useful for verifying against a V4-specific snapshot block.
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (rpc_url, state_view_address, block_number))]
    fn verify_v4_liquidity_maps(
        &self,
        rpc_url: String,
        state_view_address: String,
        block_number: Option<u64>,
    ) -> PyResult<()> {
        let state_view: Address = state_view_address.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid state_view address: {e}"))
        })?;

        let mut engine = self.engine.lock();
        let v4_pools = engine.v4_engine().pools_snapshot();
        drop(engine);

        let runtime = crate::runtime::get_runtime();
        let provider = runtime.block_on(async {
            crate::provider::AlloyProvider::new(&rpc_url, 3).await.map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "verify_v4_liquidity_maps: failed to create provider: {e}"
                ))
            })
        })?;

        let v4_result = runtime.block_on(async {
            crate::bot_core::liquidity_verifier::verify_v4_pools(
                &provider, state_view, &v4_pools, block_number,
            )
            .await
        });
        if let Err(mismatch) = v4_result {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "V4 liquidity map verification FAILED: {mismatch}"
            )));
        }

        Ok(())
    }

    /// Verify a single V3 pool's liquidity map against on-chain state.
    ///
    /// Takes a pool address and verifies the `tick_data` at the given block.
    /// Returns Ok if the liquidity map matches, or a `RuntimeError` with
    /// details of the mismatch.
    ///
    /// This is an async method — returns a coroutine that must be awaited.
    /// Uses `future_into_py` instead of `block_on` so it integrates with
    /// the Python asyncio event loop (no deadlock when called from async code).
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (address, rpc_url, block_number))]
    fn verify_v3_pool<'py>(
        &self,
        py: Python<'py>,
        address: String,
        rpc_url: String,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let pool_addr: Address = address.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}"))
        })?;

        let v3_pools = {
            let engine = self.engine.lock();
            let core = engine.core.lock();
            let Some(key) = core.pool_id_by_address(&pool_addr) else {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "V3 pool {address} not registered in engine"
                )));
            };
            let mut map = std::collections::HashMap::new();
            if let Some(pool) = core.get_v3_pool(key) {
                map.insert(key, pool.clone());
            }
            map
        };

        let tick_lens = Address::ZERO;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let provider = crate::provider::AlloyProvider::new(&rpc_url, 3)
                .await
                .map_err(|e| {
                    pyo3::exceptions::PyRuntimeError::new_err(format!(
                        "verify_v3_pool: failed to create provider: {e}"
                    ))
                })?;

            let v3_result =
                crate::bot_core::liquidity_verifier::verify_v3_pools(
                    &provider, tick_lens, &v3_pools, block_number,
                )
                .await;

            if let Err(mismatch) = v3_result {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "V3 pool {address} liquidity map verification FAILED: {mismatch}"
                )));
            }

            Ok(())
        })
    }

    /// Verify a single V4 pool's liquidity map against on-chain state.
    ///
    /// Takes a `pool_id` (hex) and verifies the `tick_data` at the given block
    /// using the `StateView` contract.
    ///
    /// This is an async method — returns a coroutine that must be awaited.
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (pool_id_hex, rpc_url, state_view_address, block_number))]
    fn verify_v4_pool<'py>(
        &self,
        py: Python<'py>,
        pool_id_hex: String,
        rpc_url: String,
        state_view_address: String,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state_view: Address = state_view_address.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid state_view address: {e}"))
        })?;

        let pool_id = hex_string_to_pool_id(&pool_id_hex)?;

        let mut engine = self.engine.lock();
        let v4_keys = engine.v4_engine().pool_keys_for_id(Address::ZERO, &pool_id);
        // V4 pools are registered with the actual pool_manager address, not ZERO.
        // Fallback: scan all V4 pools for matching pool_id.
        let v4_keys = v4_keys.or_else(|| {
            let v4_snapshot = engine.v4_engine().pools_snapshot();
            for (key, pool) in &v4_snapshot {
                if pool.pool_id == pool_id {
                    return Some((*key, *key + 1));
                }
            }
            None
        });

        let v4_pools = if let Some((fwd_key, _rev_key)) = v4_keys {
            let mut map = std::collections::HashMap::new();
            if let Some(pool) = engine.v4_engine().get_pool(fwd_key) {
                map.insert(fwd_key, pool.clone());
            }
            map
        } else {
            drop(engine);
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "V4 pool {pool_id_hex} not registered in engine"
            )));
        };
        drop(engine);

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let provider = crate::provider::AlloyProvider::new(&rpc_url, 3)
                .await
                .map_err(|e| {
                    pyo3::exceptions::PyRuntimeError::new_err(format!(
                        "verify_v4_pool: failed to create provider: {e}"
                    ))
                })?;

            let v4_result =
                crate::bot_core::liquidity_verifier::verify_v4_pools(
                    &provider, state_view, &v4_pools, block_number,
                )
                .await;

            if let Err(mismatch) = v4_result {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "V4 pool {pool_id_hex} liquidity map verification FAILED: {mismatch}"
                )));
            }

            Ok(())
        })
    }

    /// Enable or disable automatic verification on pool registration.
    ///
    /// When enabled, V3 and V4 pools registered from snapshot data (with
    /// `Tracked` coverage) are automatically verified against on-chain state.
    /// The tick data snapshot is taken while the engine lock is held, so the
    /// pump cannot race between registration and verification. The RPC call
    /// happens after the lock is released.
    ///
    /// Must call `set_verify_rpc_url()` before enabling this.
    /// V4 verification also requires `set_verify_state_view()`.
    ///
    /// Args:
    ///     enabled: Whether to enable verification on register.
    #[pyo3(signature = (enabled))]
    fn set_verify_on_register(&self, enabled: bool) {
        self.verify_on_register.store(enabled, std::sync::atomic::Ordering::Relaxed);
    }

    /// Set the HTTP RPC URL used for verification during registration.
    ///
    /// Must be called before enabling `verify_on_register`.
    #[pyo3(signature = (rpc_url))]
    fn set_verify_rpc_url(&self, rpc_url: String) {
        // Eagerly create and cache the provider so verification reuses
        // the same HTTP connection pool instead of creating a new client
        // per pool registration.
        let runtime = crate::runtime::get_runtime();
        match runtime.block_on(crate::provider::AlloyProvider::new(&rpc_url, 3)) {
            Ok(provider) => {
                *self.verify_provider.lock() = Some(provider);
            }
            Err(e) => {
                eprintln!("[warn] Failed to create verification provider: {e}");
            }
        }
        *self.verify_rpc_url.lock() = Some(rpc_url);
    }

    /// Set the `StateView` contract address for V4 verification during registration.
    ///
    /// Must be called before any V4 pools are registered with verification enabled.
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (state_view_address))]
    fn set_verify_state_view(&self, state_view_address: String) {
        let addr: Address = state_view_address.parse().unwrap_or(Address::ZERO);
        *self.verify_state_view.lock() = Some(addr);
    }



    /// Full-sync V3 pool `tick_data` from Python backfill.
    ///
    /// Unlike `process_logs` (which only inserts `tick_priors`), this method
    /// **replaces** the entire `tick_data` map. This ensures that ticks removed
    /// from Python (because `liquidityGross` went to zero after a Burn) are
    /// also removed from the Rust engine.
    ///
    /// `v3_sync_updates`: list of (`address_str`, `sqrt_price_x96`, liquidity, tick, `tick_data`)
    ///   where `tick_data` is a dict of {`tick_index`: (`liquidity_gross`, `liquidity_net`)}
    #[pyo3(signature = (v3_sync_updates, block_number))]
    fn sync_v3_pool_states(
        &self,
        v3_sync_updates: &Bound<'_, PyList>,
        block_number: u64,
    ) -> PyResult<()> {
        let engine = self.engine.lock();
        let mut core = engine.core.lock();
        for item in v3_sync_updates.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 5 {
                let msg = format!(
                    "Expected 5-tuple (address, sqrt_price, liquidity, tick, tick_data), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }

            let addr_str: String = tuple.get_item(0)?.extract()?;
            let addr: Address = addr_str.parse().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}"))
            })?;
            let sqrt_price = crate::alloy_py::extract_python_u256(&tuple.get_item(1)?)?;
            let liquidity: u128 = tuple.get_item(2)?.extract()?;
            let tick: i32 = tuple.get_item(3)?.extract()?;

            let td_obj = tuple.get_item(4)?;
            let td_dict = td_obj.cast::<pyo3::types::PyDict>()?;
            let mut rust_tick_data = HashMap::new();
            for (key, value) in td_dict.iter() {
                let tick_idx: i32 = key.extract()?;
                let info_tuple = value.cast::<pyo3::types::PyTuple>()?;
                if info_tuple.len() != 2 {
                    let msg = format!(
                        "Expected 2-tuple (liquidity_gross, liquidity_net), got {} elements",
                        info_tuple.len()
                    );
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
                let liquidity_gross: u128 = info_tuple.get_item(0)?.extract()?;
                let liquidity_net: i128 = info_tuple.get_item(1)?.extract()?;
                rust_tick_data.insert(tick_idx, make_tick_info(liquidity_gross, liquidity_net));
            }

            core.sync_v3_pool_state(
                addr,
                sqrt_price,
                liquidity,
                tick,
                rust_tick_data,
                block_number,
            );
        }
        Ok(())
    }

    /// Full-sync V4 pool `tick_data` from Python backfill.
    ///
    /// Replaces the entire `tick_data` map. See `sync_v3_pool_states` for rationale.
    ///
    /// `v4_sync_updates`: list of (`pool_manager_str`, `pool_id_hex`, `sqrt_price_x96`, liquidity, tick, `tick_data`)
    ///   where `tick_data` is a dict of {`tick_index`: (`liquidity_gross`, `liquidity_net`)}
    #[pyo3(signature = (v4_sync_updates, block_number))]
    fn sync_v4_pool_states(
        &self,
        v4_sync_updates: &Bound<'_, PyList>,
        block_number: u64,
    ) -> PyResult<()> {
        let mut engine = self.engine.lock();
        for item in v4_sync_updates.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 6 {
                let msg = format!(
                    "Expected 6-tuple (pool_manager, pool_id, sqrt_price, liquidity, tick, tick_data), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }

            let pm_str: String = tuple.get_item(0)?.extract()?;
            let pool_manager: Address = pm_str.parse().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid pool_manager: {e}"))
            })?;
            let pid_str: String = tuple.get_item(1)?.extract()?;
            let pool_id = hex_string_to_pool_id(&pid_str)?;

            let sqrt_price = crate::alloy_py::extract_python_u256(&tuple.get_item(2)?)?;
            let liquidity: u128 = tuple.get_item(3)?.extract()?;
            let tick: i32 = tuple.get_item(4)?.extract()?;

            let td_obj = tuple.get_item(5)?;
            let td_dict = td_obj.cast::<pyo3::types::PyDict>()?;
            let mut rust_tick_data = HashMap::new();
            for (key, value) in td_dict.iter() {
                let tick_idx: i32 = key.extract()?;
                let info_tuple = value.cast::<pyo3::types::PyTuple>()?;
                if info_tuple.len() != 2 {
                    let msg = format!(
                        "Expected 2-tuple (liquidity_gross, liquidity_net), got {} elements",
                        info_tuple.len()
                    );
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
                let liquidity_gross: u128 = info_tuple.get_item(0)?.extract()?;
                let liquidity_net: i128 = info_tuple.get_item(1)?.extract()?;
                rust_tick_data.insert(tick_idx, make_tick_info(liquidity_gross, liquidity_net));
            }

            engine.v4_engine().sync_pool_state(
                pool_manager,
                pool_id,
                sqrt_price,
                liquidity,
                tick,
                rust_tick_data,
                block_number,
            );
        }
        Ok(())
    }

    /// Process Sync, V3 Swap, and V4 Swap events synchronously (for testing).
    ///
    /// `v2_sync_updates`: list of (`address_str`, `reserve0`, `reserve1`)
    /// `v3_swap_updates`: list of (`address_str`, `sqrt_price_x96`, liquidity, tick, `tick_priors`)
    ///   where `tick_priors` is a list of (`tick_index`, (`liquidity_gross`, `liquidity_net`))
    /// `v4_swap_updates`: list of (`pool_manager_str`, `pool_id_hex`, `sqrt_price_x96`, liquidity, tick, `tick_priors`)
    #[pyo3(signature = (v2_sync_updates, v3_swap_updates, v4_swap_updates, block_number))]
    fn process_logs(
        &self,
        v2_sync_updates: &Bound<'_, PyList>,
        v3_swap_updates: &Bound<'_, PyList>,
        v4_swap_updates: &Bound<'_, PyList>,
        block_number: u64,
    ) -> PyResult<()> {
        let rust_v2 = Self::parse_v2_updates(v2_sync_updates)?;
        let rust_v3 = Self::parse_v3_updates(v3_swap_updates)?;
        let rust_v4 = Self::parse_v4_updates(v4_swap_updates)?;
        self.engine
            .lock()
            .process_all_updates(&rust_v2, &rust_v3, &rust_v4, block_number, &BlockMetadata::default());
        Ok(())
    }

    /// Read the last solved results and block number.
    ///
    /// Inspect a registered path by ID.
    ///
    /// Returns a dict with:
    ///   - "`path_id"`: int
    ///   - "hops": list of dicts, each with:
    ///     - "type": "V2" | "V3" | "V4"
    ///     - "address": str (V2/V3 contract address, or V4 `pool_manager`)
    ///     - "`pool_id"`: str (V4 only — the pool ID hex)
    ///     - "`zero_for_one"`: bool
    ///     - "fee": int (V2: `gamma_numer`; V3: pool fee; V4: pool fee)
    ///     - "`tick_spacing"`: int (V3/V4 only)
    ///   Returns None if the `path_id` is not found.
    #[pyo3(signature = (path_id))]
    #[allow(clippy::items_after_statements)]
    fn inspect_path(&self, path_id: u64, py: Python<'_>) -> PyResult<Option<Py<PyDict>>> {
        // Phase 1: Collect pool refs from the path
        let pool_refs: Vec<MixedPoolRef> = {
            let engine = self.engine.lock();
            let Some(path) = engine.path_pools.get(&path_id) else {
                return Ok(None);
            };
            path.pools.clone()
        };

        // Phase 2: Query sub-engines for pool details
        struct HopInfo {
            hop_type: String,
            address: Option<String>,
            pool_id: Option<String>,
            zero_for_one: bool,
            fee: Option<u64>,
            tick_spacing: Option<i32>,
        }

        let mut hops: Vec<HopInfo> = Vec::new();
        let engine = self.engine.lock();
        // ADR-003: V2 state lives in BotCore. One core-lock window covers all
        // V2 lookups in this loop (engine-then-core ordering; V3/V4 state still
        // reads the per-family engines, which are disjoint fields).
        let core = engine.core.lock();

        for pool_ref in &pool_refs {
            match pool_ref.hop_type {
                HopType::V2 => {
                    let state = core.get_v2_pool_state(pool_ref.pool_key);
                    let addr = state.map(|s| format!("{}", s.address));
                    // V2 fee is `gamma_numer`, orientation-selected (ADR-003).
                    let gamma_numer = state.map(|s| {
                        if pool_ref.zero_for_one { s.fee_token0.0 } else { s.fee_token1.0 }
                    });
                    hops.push(HopInfo {
                        hop_type: "V2".to_string(),
                        address: addr,
                        pool_id: None,
                        zero_for_one: pool_ref.zero_for_one,
                        fee: gamma_numer,
                        tick_spacing: None,
                    });
                }
                HopType::V3 => {
                    let pool = core.get_v3_pool(pool_ref.pool_key);
                    let (addr, fee, ts) = pool.map_or((None, None, None), |p| {
                        (Some(format!("{}", p.address)), Some(u64::from(p.fee)), Some(p.tick_spacing))
                    });
                    hops.push(HopInfo {
                        hop_type: "V3".to_string(),
                        address: addr,
                        pool_id: None,
                        zero_for_one: pool_ref.zero_for_one,
                        fee,
                        tick_spacing: ts,
                    });
                }
                HopType::V4 => {
                    let pool = engine.v4_engine.get_pool(pool_ref.pool_key);
                    let (pm, pid, fee, ts) = pool.map_or((None, None, None, None), |p| {
                        (Some(format!("{}", p.pool_manager)), Some(format!("0x{}", alloy::hex::encode(p.pool_id))), Some(u64::from(p.pool_key.fee)), Some(p.pool_key.tick_spacing))
                    });
                    hops.push(HopInfo {
                        hop_type: "V4".to_string(),
                        address: pm,
                        pool_id: pid,
                        zero_for_one: pool_ref.zero_for_one,
                        fee,
                        tick_spacing: ts,
                    });
                }
            }
        }

        drop(core);
        drop(engine);

        // Phase 3: Build the Python dict
        let dict = PyDict::new(py);
        dict.set_item("path_id", path_id)?;

        let hops_list = PyList::empty(py);
        for hop in &hops {
            let hop_dict = PyDict::new(py);
            hop_dict.set_item("type", hop.hop_type.as_str())?;
            if let Some(ref a) = hop.address {
                hop_dict.set_item("address", a)?;
            }
            if let Some(ref pid) = hop.pool_id {
                hop_dict.set_item("pool_id", pid)?;
            }
            hop_dict.set_item("zero_for_one", hop.zero_for_one)?;
            if let Some(f) = hop.fee {
                hop_dict.set_item("fee", f)?;
            }
            if let Some(ts) = hop.tick_spacing {
                hop_dict.set_item("tick_spacing", ts)?;
            }
            hops_list.append(hop_dict)?;
        }
        dict.set_item("hops", hops_list)?;

        Ok(Some(dict.unbind()))
    }

    /// Returns (`results`, `block_number`) where results is a flat list:
    /// [`path_id_0`, `optimal_input_0`, `profit_0`, `path_id_1`, ...]
    #[allow(clippy::significant_drop_tightening)]
    fn latest_results(&self, py: Python<'_>) -> PyResult<(Py<PyList>, u64)> {
        let (results, block_num) = {
            let engine = self.engine.lock();
            let (r, b) = engine.latest_results();
            (r.clone(), b)
        };

        let py_list = PyList::empty(py);
        for (path_id, solve_result) in results {
            let path_id_py = path_id.into_pyobject(py)?;
            let input_py = crate::alloy_py::PyU256(solve_result.optimal_input).into_pyobject(py)?;
            let profit_py = crate::alloy_py::PyU256(solve_result.profit).into_pyobject(py)?;

            // Build hop_outputs as a Python tuple
            let hop_outputs_py = PyList::empty(py);
            for hop_out in &solve_result.hop_outputs {
                let hop_py = crate::alloy_py::PyU256(*hop_out).into_pyobject(py)?;
                hop_outputs_py.append(hop_py)?;
            }
            let hop_tuple = hop_outputs_py.into_pyobject(py)?;

            // Build consumed_inputs as a Python tuple
            let consumed_inputs_py = PyList::empty(py);
            for consumed in &solve_result.consumed_inputs {
                let consumed_py = crate::alloy_py::PyU256(*consumed).into_pyobject(py)?;
                consumed_inputs_py.append(consumed_py)?;
            }
            let consumed_tuple = consumed_inputs_py.into_pyobject(py)?;

            let result_tuple = (path_id_py, input_py, profit_py, hop_tuple, consumed_tuple).into_pyobject(py)?;
            py_list.append(result_tuple)?;
        }

        Ok((py_list.unbind(), block_num))
    }

    /// De-register a path from the engine.
    ///
    /// Removes the path from the engine's internal state. The path's pools
    /// are **not** removed — other paths may still reference them.
    ///
    /// Returns `true` if the path existed and was removed.
    #[pyo3(signature = (path_id))]
    fn deregister_path(&self, path_id: u64) -> bool {
        self.engine.lock().deregister_path(path_id)
    }

    /// Set the profit thresholds for the result batch channel.
    ///
    /// Only paths with `profit > min_profit` and `profit < max_profit`
    /// appear in batch `fresh` / `updated` entries.
    #[pyo3(signature = (min_profit, max_profit))]
    fn set_profit_thresholds(&self, min_profit: u64, max_profit: u64) {
        self.engine
            .lock()
            .set_profit_thresholds(U256::from(min_profit), U256::from(max_profit));
    }

    /// DIAG-a3f2: Dump V2 pool state for a given address.
    /// Returns (`pool_id`, `reserve0`, `reserve1`) or None.
    ///
    /// ADR-003: V2 state lives in `BotCore` as a single entry per address
    /// (orientation is selected at solve time via `zero_for_one`, not by a
    /// separate reverse key). The former forward/reverse dual keys are gone.
    #[pyo3(signature = (address_hex))]
    fn diag_v2_pool(&self, address_hex: &str) -> PyResult<Option<(u64, String, String)>> {
        let addr: Address = address_hex.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}"))
        })?;
        let engine = self.engine.lock();
        let core = engine.core.lock();
        let Some(pool_id) = core.pool_id_by_address(&addr) else {
            return Ok(None);
        };
        let Some(state) = core.get_v2_pool_state(pool_id) else {
            return Ok(None);
        };
        Ok(Some((pool_id, state.reserve0.to_string(), state.reserve1.to_string())))
    }

    /// Return self as an async iterator over result batches.
    #[allow(clippy::missing_const_for_fn)]
    fn __aiter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    /// Await the next result batch from the engine.
    ///
    /// Returns a dict with keys:
    ///   - "`solve_block"`: int
    ///   - "timestamp": int
    ///   - "`base_fee_per_gas"`: int | None
    ///   - "`gas_used"`: int
    ///   - "`gas_limit"`: int
    ///   - "fresh": list of (`path_id`, `optimal_input`, profit, `hop_outputs`, `consumed_inputs`)
    ///   - "updated": list of (`path_id`, `optimal_input`, profit, `hop_outputs`, `consumed_inputs`)
    ///   - "expired": list of int (`path_ids`)
    ///   - "removed": list of int (`path_ids`)
    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let result_rx = Arc::clone(&self.result_rx);

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            // Take the receiver for awaiting
            let mut rx = result_rx
                .lock()
                .take()
                .ok_or_else(|| PyStopAsyncIteration::new_err("Result channel closed"))?;

            // Wait for the next batch
            let batch = rx.recv().await.ok_or_else(|| {
                PyStopAsyncIteration::new_err(
                    "Result channel closed — pump may have stopped.",
                )
            })?;

            // Put the receiver back
            *result_rx.lock() = Some(rx);

            // Convert batch to Python dict (requires GIL)
            Python::attach(|py| batch_to_py_dict(&batch, py))
        })
    }
}

/// Helper to construct `TickInfo` from Python-extracted values.
/// Convert a `ResultBatch` to a Python dict.
///
/// Called under the GIL after receiving a batch from the result channel.
fn batch_to_py_dict(batch: &ResultBatch, py: Python<'_>) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("solve_block", batch.solve_block)?;
    dict.set_item("timestamp", batch.timestamp)?;
    dict.set_item("base_fee_per_gas", batch.base_fee_per_gas)?;
    dict.set_item("gas_used", batch.gas_used)?;
    dict.set_item("gas_limit", batch.gas_limit)?;

    // fresh: list of (path_id, optimal_input, profit, hop_outputs, consumed_inputs)
    let fresh_list = PyList::empty(py);
    for (path_id, result) in &batch.fresh {
        let tuple = solve_result_to_py_tuple(*path_id, result, py)?;
        fresh_list.append(tuple)?;
    }
    dict.set_item("fresh", fresh_list)?;

    // updated: same format as fresh
    let updated_list = PyList::empty(py);
    for (path_id, result) in &batch.updated {
        let tuple = solve_result_to_py_tuple(*path_id, result, py)?;
        updated_list.append(tuple)?;
    }
    dict.set_item("updated", updated_list)?;

    // expired: list of path_ids
    let expired_list = PyList::empty(py);
    for &path_id in &batch.expired {
        expired_list.append(path_id)?;
    }
    dict.set_item("expired", expired_list)?;

    // removed: list of path_ids
    let removed_list = PyList::empty(py);
    for &path_id in &batch.removed {
        removed_list.append(path_id)?;
    }
    dict.set_item("removed", removed_list)?;

    Ok(dict.unbind())
}

/// Convert a (`path_id`, `SolvePathResult`) to a Python tuple.
fn solve_result_to_py_tuple<'py>(
    path_id: u64,
    result: &SolvePathResult,
    py: Python<'py>,
) -> PyResult<Bound<'py, pyo3::types::PyTuple>> {
    let path_id_py = path_id.into_pyobject(py)?;
    let input_py = crate::alloy_py::PyU256(result.optimal_input).into_pyobject(py)?;
    let profit_py = crate::alloy_py::PyU256(result.profit).into_pyobject(py)?;

    let hop_outputs_py = PyList::empty(py);
    for hop_out in &result.hop_outputs {
        let hop_py = crate::alloy_py::PyU256(*hop_out).into_pyobject(py)?;
        hop_outputs_py.append(hop_py)?;
    }

    let consumed_inputs_py = PyList::empty(py);
    for consumed in &result.consumed_inputs {
        let consumed_py = crate::alloy_py::PyU256(*consumed).into_pyobject(py)?;
        consumed_inputs_py.append(consumed_py)?;
    }

    (
        path_id_py,
        input_py,
        profit_py,
        hop_outputs_py,
        consumed_inputs_py,
    )
        .into_pyobject(py)
}

fn make_tick_info(liquidity_gross: u128, liquidity_net: i128) -> crate::bot_core::TickInfo {
    use alloy::primitives::{I256, U128};
    crate::bot_core::TickInfo {
        liquidity_gross: U128::from(liquidity_gross),
        liquidity_net: I256::try_from(liquidity_net).unwrap_or(I256::ZERO),
    }
}

/// Helper to decode a hex string (e.g. "0xabcd...") to a V4 `PoolId` ([u8; 32]).
fn hex_string_to_pool_id(hex_str: &str) -> PyResult<crate::bot_core::v4_swap_decoder::PoolId> {
    let hex_str = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    if hex_str.len() != 64 {
        let msg = format!(
            "Pool ID hex string must be 64 hex chars (32 bytes), got {}",
            hex_str.len()
        );
        return Err(pyo3::exceptions::PyValueError::new_err(msg));
    }
    let mut pool_id = [0u8; 32];
    for i in 0..32 {
        let byte_str = &hex_str[i * 2..i * 2 + 2];
        pool_id[i] = u8::from_str_radix(byte_str, 16).map_err(|e| {
            let msg = format!("Invalid hex in pool_id at byte {i}: {e}");
            pyo3::exceptions::PyValueError::new_err(msg)
        })?;
    }
    Ok(pool_id)
}
