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
//!   existing `crates/degenbot-bot/src/solvers/uniswap_engine/` core split.
//!   (ergo UG6FKN task 74W2Z6.)

mod errors;
mod register;
mod result_channel;
mod snapshot;
mod solve;
mod verify;

pub(crate) use register::map_register_v4_err;

pub use errors::*;
pub use result_channel::BlockStream;

use crate::prelude::*;
pub(crate) use std::collections::HashMap;
pub(crate) use std::sync::Arc;

pub(crate) use alloy::primitives::{Address, U256};
pub(crate) use pyo3::exceptions::PyStopAsyncIteration;
pub(crate) use pyo3::types::{PyDict, PyList};
pub(crate) use tokio::sync::mpsc;

pub(crate) use crate::bot::PyBot;
pub(crate) use degenbot_bot::bot_core::reorg_coordinator::ReorgCoordinator;
pub(crate) use degenbot_bot::bot_core::solve_coordinator::SolveCoordinator;
pub(crate) use degenbot_bot::bot_core::{
    drain_sink::DrainSink, Bot, V3SwapUpdate, V4StateSync, V4SwapUpdate,
};

pub(crate) use degenbot_bot::solvers::uniswap_engine::engine_handle::EngineHandle;
pub(crate) use degenbot_bot::solvers::uniswap_engine::engine_subscriber::EngineSubscriber;

pub(crate) use degenbot_bot::bot_core::snapshot_verify::{SnapshotStore, VerifyError};
pub(crate) use degenbot_bot::solvers::uniswap_engine::{
    BlockMetadata, BlockNotification, EnginePhase, HopType, MixedPoolRef, PoolHop, ResultBatch,
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
    /// ADR-006 D4 (T3): the pump lifecycle state (coordinator, reorg
    /// coordinator, bot, shutdown, pump handle, subscribe state, phase) now
    /// lives in a shared `Arc<PumpState>` co-owned with `PyBot`. The legacy
    /// individual fields (`coordinator`, `reorg_coordinator`, `bot`, `shutdown`,
    /// `pump_handle`, `subscribe_state`, `phase`) are reachable through this
    /// handle — so snapshot.rs/solve.rs keep reading the SAME state while the
    /// three pump methods also move onto `PyBot`.
    pump: Arc<crate::bot::pump::PumpState>,
    /// Receiver for the result batch channel.
    /// Created in `new()`, consumed by `__anext__`.
    /// Wrapped in Arc so the async coroutine can share it.
    result_rx: Arc<parking_lot::Mutex<Option<mpsc::UnboundedReceiver<ResultBatch>>>>,
    /// Receiver for the block-notification channel (epic 6W35AI). The pump
    /// forwards `newHeads` ticks here via `DrainSink::notify_block`;
    /// Python consumes this as its block clock (not `ResultBatch::solve_block`).
    /// Consumed by `BlockStream::__anext__`; wrapped in Arc for the coroutine.
    block_rx: Arc<parking_lot::Mutex<Option<mpsc::UnboundedReceiver<BlockNotification>>>>,
    /// V3 snapshot tick data, loaded via `load_v3_snapshot()` and consumed
    /// at registration time. One-way transfer: `remove()` not `clone()`.
    v3_snapshot: SnapshotStore<Address>,
    /// V4 snapshot tick data, loaded via `load_v4_snapshot()` and consumed
    /// at registration time. One-way transfer: `remove()` not `clone()`.
    v4_snapshot: SnapshotStore<(Address, degenbot_decoders::v4_swap_decoder::PoolId)>,
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
        self.pump.current_phase()
    }

    /// Set the engine phase (advancing only).
    pub(crate) fn set_phase(&self, phase: EnginePhase) {
        self.pump.set_phase(phase);
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

    use super::verify::map_verify_err;
    use super::*;
    use degenbot_bot::solvers::uniswap_engine::snapshot_verify::VerifyError;

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
