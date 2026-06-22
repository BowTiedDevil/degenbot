//! `PyO3` wrapper for the `UniswapEngine`.
//!
//! [`PyUniswapArbEngine`] wraps [`UniswapEngine`] with a `parking_lot::Mutex`
//! for safe access from the Tokio pump task. All Python-facing methods
//! acquire the lock, perform their operation, and release it.

//! # Layout
//!
//! - [`PyUniswapArbEngine`] (the `#[pyclass]`) is declared here; its
//!   `#[pymethods]` surface is split across [`register`], [`snapshot`],
//!   [`verify`], [`solve`], [`result_channel`] (`PyO3` permits multiple
//!   `#[pymethods] impl PyUniswapArbEngine` blocks). [`errors`] holds the
//!   `#[create_exception]` types.
//! - Mirrors `polars-python/src/expr/`'s 17-file `PyExpr` split and the
//!   existing `crates/degenbot-bot/src/optimizers/uniswap_engine/` core split.
//!   (ergo UG6FKN task 74W2Z6.)

mod errors;
mod register;
mod result_channel;
mod snapshot;
mod solve;
mod verify;

pub(crate) use register::map_register_v4_err;

pub use errors::*;

use crate::prelude::*;
pub(crate) use std::collections::HashMap;
pub(crate) use std::sync::Arc;

pub(crate) use alloy::primitives::{Address, U256};
pub(crate) use pyo3::exceptions::PyStopAsyncIteration;
pub(crate) use pyo3::types::{PyDict, PyList};
pub(crate) use tokio::sync::mpsc;

pub(crate) use crate::bot::PyBot;
pub(crate) use degenbot_bot::bot_core::block_pump::{BlockPump, WsEvent};
pub(crate) use degenbot_bot::bot_core::reorg_coordinator::ReorgCoordinator;
pub(crate) use degenbot_bot::bot_core::solve_coordinator::SolveCoordinator;
pub(crate) use degenbot_bot::bot_core::{
    drain_sink::DrainSink, Bot, RegisterV3PoolParams, V3SwapUpdate,
};
pub(crate) use degenbot_bot::bot_core::{RegisterV4PoolParams, V4StateSync, V4SwapUpdate};

pub(crate) use degenbot_bot::optimizers::uniswap_engine::engine_handle::EngineHandle;
pub(crate) use degenbot_bot::optimizers::uniswap_engine::engine_subscriber::EngineSubscriber;

pub(crate) use degenbot_bot::optimizers::uniswap_engine::snapshot_verify::{
    register_with_cl_buffers, run_cl_verification, SnapshotStore, VerifyError, VerifyRpc,
};
pub(crate) use degenbot_bot::optimizers::uniswap_engine::{
    BlockMetadata, EnginePhase, HopType, MixedPoolRef, PoolHop, PoolTickCoverage, ResultBatch,
    SolvePathResult, UniswapEngine, V3SnapshotData, V4SnapshotData,
};

/// Python-facing mixed V2/V3 arbitrage engine.
///
/// Wraps [`UniswapEngine`] with a `parking_lot::Mutex` for safe access
/// from the Tokio pump task.
#[pyclass(name = "UniswapArbEngine", skip_from_py_object)]
pub struct PyUniswapArbEngine {
    /// Shared engine state
    engine: Arc<parking_lot::Mutex<UniswapEngine>>,
    /// The drain-point solve coordinator (ADR-006 D4, slice 6). Holds the
    /// engine as `Arc<dyn Engine>` (via `EngineHandle`) and fans drain-tick /
    /// send / finalize / reorg calls to it under a `drain_lock`. `start` and
    /// `subscribe` pass this to `BlockPump` as `Arc<dyn DrainSink>`, replacing
    /// slice 5a's `EngineDrainSink` placeholder. Python polls of
    /// `last_processed_block` route through the coordinator (not the raw
    /// engine) so they block until in-flight drains complete and return a
    /// drain-consistent "good" block.
    coordinator: Arc<SolveCoordinator>,
    /// The per-event reorg coordinator (ADR-006 slice 7). Owned by the engine
    /// wrapper; passed to `BlockPump` so its `removed: true` branch routes to
    /// `dispatch_reorg_log` (per-pool restore + notify) instead of the deleted
    /// bulk `engine.handle_reorg`.
    reorg_coordinator: Arc<ReorgCoordinator>,
    /// The per-chain `Bot` orchestrator (ADR-006 D4). `BlockPump` clones this
    /// `Arc` so its `dispatch_log` writes flow through to the engine's reads
    /// (the engine shares the same `BotState` core via `with_core`).
    bot: Arc<Bot>,
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
    verify_provider: parking_lot::Mutex<Option<degenbot_rpc::provider::AlloyProvider>>,
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
    v4_snapshot: SnapshotStore<(Address, degenbot_decoders::v4_swap_decoder::PoolId)>,
}

/// Python-facing subscribe state.
///
/// Stores the pump and subscribe results between `subscribe()` and `resume()`
/// calls so that `resume()` can re-use the same pump instance.
struct PySubscribeState {
    /// The pump instance (holds `Arc<Bot>` + `Arc<dyn DrainSink>`, provider, shutdown)
    pump: BlockPump,
    /// First block number observed during subscribe
    first_block: u64,
    /// Live WS stream for the resume phase
    combined_stream: futures_util::stream::BoxStream<'static, WsEvent>,
}

impl PyUniswapArbEngine {
    /// Parse V2 Sync updates from a Python list of 3-tuples.
    pub(crate) fn parse_v2_updates(
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
            let r0 = crate::conversion::alloy::extract_python_u256(&tuple.get_item(1)?)?;
            let r1 = crate::conversion::alloy::extract_python_u256(&tuple.get_item(2)?)?;
            rust_v2.push((addr, r0, r1));
        }
        Ok(rust_v2)
    }

    /// Parse tick priors from a Python list of 2-tuples.
    pub(crate) fn parse_tick_priors(
        priors_list: &Bound<'_, PyList>,
    ) -> PyResult<Vec<(i32, degenbot_bot::bot_core::TickInfo)>> {
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
    pub(crate) fn parse_v3_updates(
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
            let sqrt_price = crate::conversion::alloy::extract_python_u256(&tuple.get_item(1)?)?;
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
    pub(crate) fn parse_v4_updates(
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
                pyo3::exceptions::PyValueError::new_err(format!(
                    "Invalid pool_manager address: {e}"
                ))
            })?;

            let pid_obj = tuple.get_item(1)?;
            let pid_str: String = pid_obj.extract()?;
            let pool_id = hex_string_to_pool_id(&pid_str)?;

            let sqrt_price = crate::conversion::alloy::extract_python_u256(&tuple.get_item(2)?)?;
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
    pub(crate) fn current_phase(&self) -> EnginePhase {
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
    pub(crate) fn set_phase(&self, phase: EnginePhase) {
        self.phase
            .store(phase as u8, std::sync::atomic::Ordering::Relaxed);
    }

    /// Deserialize a V3 binary snapshot into `V3SnapshotData`.
    pub(crate) fn deserialize_v3_snapshot(data: &[u8]) -> PyResult<V3SnapshotData> {
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
            let tick_count =
                u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
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
                let gross_hi =
                    u64::from_le_bytes(data[offset + 8..offset + 16].try_into().unwrap());
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
    pub(crate) fn deserialize_v4_snapshot(data: &[u8]) -> PyResult<V4SnapshotData> {
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
            let pool_id_count =
                u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
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
                let tick_count =
                    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;

                let mut tick_data = HashMap::with_capacity(tick_count);
                for _ in 0..tick_count {
                    // tick_index (4 bytes LE, i32)
                    if offset + 4 > data.len() {
                        let msg = "V4 snapshot truncated: expected tick_index";
                        return Err(pyo3::exceptions::PyValueError::new_err(msg));
                    }
                    let tick_index =
                        i32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                    offset += 4;

                    // liquidity_gross (16 bytes LE, u128)
                    if offset + 16 > data.len() {
                        let msg = "V4 snapshot truncated: expected liquidity_gross";
                        return Err(pyo3::exceptions::PyValueError::new_err(msg));
                    }
                    let gross_lo = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                    let gross_hi =
                        u64::from_le_bytes(data[offset + 8..offset + 16].try_into().unwrap());
                    let liquidity_gross = u128::from(gross_hi) << 64 | u128::from(gross_lo);
                    offset += 16;

                    // liquidity_net (16 bytes LE, i128)
                    if offset + 16 > data.len() {
                        let msg = "V4 snapshot truncated: expected liquidity_net";
                        return Err(pyo3::exceptions::PyValueError::new_err(msg));
                    }
                    let net_lo = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                    let net_hi =
                        u64::from_le_bytes(data[offset + 8..offset + 16].try_into().unwrap());
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

pub(crate) fn make_tick_info(
    liquidity_gross: u128,
    liquidity_net: i128,
) -> degenbot_bot::bot_core::TickInfo {
    use alloy::primitives::{I256, U128};
    degenbot_bot::bot_core::TickInfo {
        liquidity_gross: U128::from(liquidity_gross),
        liquidity_net: I256::try_from(liquidity_net).unwrap_or(I256::ZERO),
        block: 0,
    }
}

/// Helper to decode a hex string (e.g. "0xabcd...") to a V4 `PoolId` ([u8; 32]).
pub(crate) fn hex_string_to_pool_id(
    hex_str: &str,
) -> PyResult<degenbot_decoders::v4_swap_decoder::PoolId> {
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

#[cfg(test)]
mod tests {
    //! VP42BP: pin the `LiquidityVerifyError` → `VerifyError` → Python
    //! exception-type chain so a per-call RPC transport failure surfaces as
    //! `VerificationRpcError` (retryable), distinct from a genuine on-chain
    //! mismatch which surfaces as `VerificationMismatchError` (fatal). The AC
    //! test ("a mock-provider verifier that returns a transport error surfaces
    //! `VerificationRpcError`") is exercised at the mapping seam — the
    //! verifier's RPC-failure branch (in `liquidity_verifier`) feeds a
    //! `LiquidityVerifyError::Rpc` through `map_liquidity_verify_error` and
    //! `map_verify_err` to a typed Python exception.

    use super::verify::{map_liquidity_verify_error, map_verify_err};
    use super::*;
    use degenbot_bot::bot_core::liquidity_verifier::{LiquidityVerifyError, VerificationMismatch};
    use degenbot_bot::optimizers::uniswap_engine::snapshot_verify::VerifyError;

    /// `map_liquidity_verify_error` preserves the distinction: `Mismatch` →
    /// `Snapshot`, `Rpc` → `Rpc` (NOT flattened to `Snapshot`).
    #[test]
    fn map_liquidity_verify_error_routes_rpc_separately_from_mismatch() {
        // Transport failure → Rpc variant, with phase prefix + the verifier's
        // per-call message.
        let rpc_err = LiquidityVerifyError::Rpc {
            message: "tickBitmap(0) RPC call failed: timeout".to_string(),
        };
        let mapped = map_liquidity_verify_error(rpc_err, "V3 pool 0x..", "snapshot", 10);
        assert!(
            matches!(mapped, VerifyError::Rpc(m) if m.contains("tickBitmap(0) RPC call failed")
                && m.contains("snapshot block 10")
                && m.contains("V3 pool 0x..")),
            "per-call RPC transport failure must map to VerifyError::Rpc, not Snapshot"
        );

        // Genuine mismatch → Snapshot variant.
        let mismatch = LiquidityVerifyError::Mismatch(VerificationMismatch {
            message: "tick 5 lg mismatch".to_string(),
        });
        let mapped = map_liquidity_verify_error(mismatch, "V4 pool 0x..", "backfill", 20);
        assert!(
            matches!(mapped, VerifyError::Snapshot(m) if m.contains("tick 5 lg mismatch")
                && m.contains("backfill block 20")
                && m.contains("V4 pool 0x..")),
            "genuine on-chain mismatch must map to VerifyError::Snapshot"
        );
    }

    /// `map_verify_err` (the `PyO3` seam) routes `VerifyError::Rpc` →
    /// `VerificationRpcError` and `VerifyError::Snapshot` →
    /// `VerificationMismatchError` (distinct Python types). Requires the GIL to
    /// construct the Python exceptions.
    #[test]
    fn map_verify_err_routes_rpc_to_verification_rpc_error() {
        pyo3::Python::attach(|py| {
            // RPC transport failure → VerificationRpcError (retryable).
            let res: PyResult<()> = map_verify_err(Err(VerifyError::Rpc(
                "tickBitmap(0) RPC call failed: timeout".to_string(),
            )));
            let err = res.expect_err("Rpc must surface as a PyErr");
            assert!(
                err.is_instance_of::<VerificationRpcError>(py),
                "VerifyError::Rpc must surface as VerificationRpcError (retryable), not VerificationMismatchError"
            );

            // Genuine mismatch → VerificationMismatchError (fatal).
            let res: PyResult<()> = map_verify_err(Err(VerifyError::Snapshot(
                "tick 5 lg mismatch".to_string(),
            )));
            let err = res.expect_err("Snapshot must surface as a PyErr");
            assert!(
                err.is_instance_of::<VerificationMismatchError>(py),
                "VerifyError::Snapshot must surface as VerificationMismatchError (fatal)"
            );
            // Cross-check the two are distinct types.
            assert!(
                !err.is_instance_of::<VerificationRpcError>(py),
                "genuine mismatch is NOT an Rpc error (distinct types)"
            );

            // Provider construction → VerificationRpcError (unchanged, now
            // shares the arm with Rpc).
            let res: PyResult<()> = map_verify_err(Err(VerifyError::Provider(
                "failed to create provider: connection refused".to_string(),
            )));
            let err = res.expect_err("Provider must surface as a PyErr");
            assert!(
                err.is_instance_of::<VerificationRpcError>(py),
                "VerifyError::Provider still surfaces as VerificationRpcError"
            );

            Ok::<_, pyo3::PyErr>(())
        })
        .expect("gil test must not panic");
    }
}
