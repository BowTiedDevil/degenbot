//! `PyO3` wrapper for the `UniswapEngine` — verify `#[pymethods]` slice.
//!
//! Split out of the former monolithic `py_binding.rs` (ergo UG6FKN task 74W2Z6),
//! mirroring `crates/degenbot-bot/src/solvers/uniswap_engine/`'s per-concern
//! layout. `PyO3` allows multiple `#[pymethods] impl PyUniswapArbEngine { … }`
//! blocks per type, so each concern file contributes one slice.

use super::{
    hex_string_to_pool_id, Address, PyUniswapArbEngine, VerificationMismatchError,
    VerificationRpcError, VerifyError,
};
use crate::prelude::*;

/// [`Result<(), VerifyError>`] → typed Python exception seam for the
/// snapshot/verify module (ADR-006 slice 5b). Distinguishes failure
/// categories by type:
/// - `Snapshot` → [`VerificationMismatchError`] (genuine mismatch — fatal)
/// - `Provider` → [`VerificationRpcError`] (transport/provider construction)
/// - `Rpc` → [`VerificationRpcError`] (per-call RPC transport — retryable)
/// - `NoSnapshotStream` → `PyRuntimeError` (programmer error)
/// - `NotConfigured` → `PyRuntimeError` (programmer error)
///
/// ADR-006 D3 (T5): the verify-on-register path that drove this seam is
/// deleted; the two-step verify (T6) routes through PyBot's batch API. Kept
/// because `SnapshotStore::insert` (snapshot.rs) still maps its
/// `VerifyError` through this.
pub(crate) fn map_verify_err(res: Result<(), VerifyError>) -> PyResult<()> {
    res.map_err(|e| match e {
        VerifyError::NoSnapshotStream => {
            pyo3::exceptions::PyRuntimeError::new_err("No snapshot stream in progress.")
        }
        VerifyError::NotConfigured(msg) => pyo3::exceptions::PyRuntimeError::new_err(msg),
        VerifyError::Snapshot(msg) => VerificationMismatchError::new_err(msg),
        VerifyError::Provider(msg) | VerifyError::Rpc(msg) => VerificationRpcError::new_err(msg),
    })
}

#[pymethods]
impl PyUniswapArbEngine {
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
    fn verify_liquidity_maps<'py>(
        &self,
        py: Python<'py>,
        rpc_url: String,
        tick_lens_address: String,
        state_view_address: String,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        // ADR-006 D4 (T4): delegates to the shared `PumpState` — the
        // Bot-owned entry point. Production routes through PyBot.
        self.pump.verify_liquidity_maps(
            py,
            rpc_url,
            tick_lens_address,
            state_view_address,
            block_number,
        )
    }

    /// Verify V3 liquidity maps only, at a specific block.
    ///
    /// Same as `verify_liquidity_maps` but only checks V3 pools.
    /// Useful for verifying against a V3-specific snapshot block.
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (rpc_url, block_number))]
    fn verify_v3_liquidity_maps<'py>(
        &self,
        py: Python<'py>,
        rpc_url: String,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        // ADR-006 D4 (T4): delegates to the shared `PumpState`.
        self.pump.verify_v3_liquidity_maps(py, rpc_url, block_number)
    }

    /// Verify V4 liquidity maps only, at a specific block.
    ///
    /// Same as `verify_liquidity_maps` but only checks V4 pools.
    /// Useful for verifying against a V4-specific snapshot block.
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (rpc_url, state_view_address, block_number))]
    fn verify_v4_liquidity_maps<'py>(
        &self,
        py: Python<'py>,
        rpc_url: String,
        state_view_address: String,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        // ADR-006 D4 (T4): delegates to the shared `PumpState`.
        self.pump.verify_v4_liquidity_maps(py, rpc_url, state_view_address, block_number)
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
            let core = engine.core.read();
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
            let provider = degenbot_rpc::provider::AlloyProvider::new(&rpc_url, 3)
                .await
                .map_err(|e| {
                    pyo3::exceptions::PyRuntimeError::new_err(format!(
                        "verify_v3_pool: failed to create provider: {e}"
                    ))
                })?;

            let v3_result = degenbot_bot::bot_core::liquidity_verifier::verify_v3_pools(
                &provider,
                tick_lens,
                &v3_pools,
                block_number,
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

        let engine = self.engine.lock();
        let core = engine.core.read();
        // ADR-003: single V4 entry per `(pool_manager, pool_id)` — no dual
        // forward/reverse keys. v4_pool_id_by_key returns Option<u64>.
        let v4_key = core.v4_pool_id_by_key(Address::ZERO, &pool_id).or_else(|| {
            // V4 pools are registered with the actual pool_manager address,
            // not ZERO. Fallback: scan all V4 pools for matching pool_id.
            for (key, pool) in core.v4_pools_snapshot() {
                if pool.pool_id == pool_id {
                    return Some(key);
                }
            }
            None
        });

        let v4_pools = if let Some(fwd_key) = v4_key {
            let mut map = std::collections::HashMap::new();
            if let Some(pool) = core.get_v4_pool(fwd_key) {
                map.insert(fwd_key, pool.clone());
            }
            map
        } else {
            drop(core);
            drop(engine);
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "V4 pool {pool_id_hex} not registered in engine"
            )));
        };
        drop(core);
        drop(engine);

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let provider = degenbot_rpc::provider::AlloyProvider::new(&rpc_url, 3)
                .await
                .map_err(|e| {
                    pyo3::exceptions::PyRuntimeError::new_err(format!(
                        "verify_v4_pool: failed to create provider: {e}"
                    ))
                })?;

            let v4_result = degenbot_bot::bot_core::liquidity_verifier::verify_v4_pools(
                &provider,
                state_view,
                &v4_pools,
                block_number,
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

    /// Set the HTTP RPC URL used for verification during registration.
    ///
    /// Must be called before enabling `verify_on_register`.
    #[pyo3(signature = (rpc_url))]
    fn set_verify_rpc_url(&self, rpc_url: String) {
        // ADR-006 D4 (T4): delegates to the shared `PumpState`.
        self.pump.set_verify_rpc_url(rpc_url)
    }

    /// Set the `StateView` contract address for V4 verification during registration.
    ///
    /// Must be called before any V4 pools are registered with verification enabled.
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (state_view_address))]
    fn set_verify_state_view(&self, state_view_address: String) {
        // ADR-006 D4 (T4): delegates to the shared `PumpState`.
        self.pump.set_verify_state_view(state_view_address)
    }
}
