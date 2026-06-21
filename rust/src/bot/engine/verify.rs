//! `PyO3` wrapper for the `UniswapEngine` — verify `#[pymethods]` slice.
//!
//! Split out of the former monolithic `py_binding.rs` (ergo UG6FKN task 74W2Z6),
//! mirroring `crates/degenbot-bot/src/optimizers/uniswap_engine/`'s per-concern
//! layout. PyO3 allows multiple `#[pymethods] impl PyUniswapArbEngine { … }`
//! blocks per type, so each concern file contributes one slice.

use super::*;
use crate::prelude::*;

// --- verify plumbing (free helpers + EngineVerifyRpc) ---
/// [`Result<(), VerifyError>`] → typed Python exception seam for the extracted
/// snapshot/verify module (ADR-006 slice 5b + slice-5 candidate-2).
///
/// Distinguishes the failure categories by *type* (TODO-53b7453b + VP42BP):
/// - `Snapshot` → [`VerificationMismatchError`] (genuine mismatch — fatal)
/// - `Provider` → [`VerificationRpcError`] (transport/provider-construction)
/// - `Rpc` → [`VerificationRpcError`] (per-call RPC transport — retryable,
///   VP42BP; folded into the same Python type as `Provider` so a broad
///   retry-on-`VerificationRpcError` policy catches both transport classes)
/// - `NoSnapshotStream` → `PyRuntimeError` (programmer error: no snapshot
///   stream in progress — distinct from verification failure categories)
pub(crate) fn map_verify_err(res: Result<(), VerifyError>) -> PyResult<()> {
    res.map_err(|e| match e {
        VerifyError::NoSnapshotStream => {
            pyo3::exceptions::PyRuntimeError::new_err("No snapshot stream in progress.")
        }
        VerifyError::Snapshot(msg) => VerificationMismatchError::new_err(msg),
        VerifyError::Provider(msg) | VerifyError::Rpc(msg) => VerificationRpcError::new_err(msg),
    })
}

/// Lazily create or reuse a cached verification provider (ADR-006 slice-5
/// candidate-2: the `AlloyProvider` I/O seam stays in `py_binding.rs`; the pure
/// orchestrator in `snapshot_verify.rs` reaches it through the `VerifyRpc`
/// trait this is part of).
fn verification_provider(
    rpc_url: &str,
    verify_provider: &parking_lot::Mutex<Option<degenbot_rpc::provider::AlloyProvider>>,
    label: &str,
) -> Result<degenbot_rpc::provider::AlloyProvider, VerifyError> {
    let mut cached = verify_provider.lock();
    if cached.is_none() {
        let runtime = degenbot_core::runtime::get_runtime();
        match runtime.block_on(degenbot_rpc::provider::AlloyProvider::new(rpc_url, 3)) {
            Ok(provider) => *cached = Some(provider),
            Err(e) => {
                return Err(VerifyError::Provider(format!(
                    "verify: {label}: failed to create provider: {e}"
                )));
            }
        }
    }
    Ok(cached.as_ref().unwrap().clone())
}

/// The single concrete [`VerifyRpc`] impl (ADR-006 slice-5 candidate-2).
///
/// Holds borrows of the engine's verify plumbing (`rpc_url` + cached
/// `AlloyProvider`); each method checks configuration, lazily ensures the
/// provider, `block_on`s the underlying async verifier, and maps a
/// `VerificationMismatch` into a [`VerifyError::Snapshot`] whose message
/// carries the phase + pool label (mirroring the legacy closure bodies verbatim,
/// so the `RuntimeError` text is byte-for-byte unchanged).
///
/// This is a transient, borrowed impl — constructed per `register_v3_pool` /
/// `register_v4_pool` call; lifetime-tied to the `PyUniswapArbEngine` it borrows.
pub(crate) struct EngineVerifyRpc<'a> {
    pub(crate) rpc_url: &'a parking_lot::Mutex<Option<String>>,
    pub(crate) provider: &'a parking_lot::Mutex<Option<degenbot_rpc::provider::AlloyProvider>>,
}

impl EngineVerifyRpc<'_> {
    /// Resolve the configured `rpc_url` once per call; `None` → the orchestrator's
    /// `enabled()` gate returns false and no provider is built.
    fn rpc_url(&self) -> Option<String> {
        self.rpc_url.lock().clone()
    }

    /// Ensure the cached provider for `label` exists, returning a clone. The
    /// first method call on a fresh orchestrator constructs it; subsequent calls
    /// reuse the cache. Mapped to [`VerifyError::Provider`].
    fn provider(&self, label: &str) -> Result<degenbot_rpc::provider::AlloyProvider, VerifyError> {
        let Some(rpc_url) = self.rpc_url() else {
            // Orchestrator gates on `enabled()`; reaching here is unreachable,
            // but stay no-op rather than constructing a provider with no URL.
            return Err(VerifyError::Provider(
                "verify: no rpc_url configured".to_string(),
            ));
        };
        verification_provider(&rpc_url, self.provider, label)
    }
}

/// Map a [`liquidity_verifier::LiquidityVerifyError`] (VP42BP) to the pure
/// [`VerifyError`] the orchestrator propagates, preserving the
/// mismatch-vs-RPC-transport distinction:
/// - `Mismatch` → `VerifyError::Snapshot` (genuine on-chain divergence —
///   fatal; surfaces as `VerificationMismatchError` at the seam).
/// - `Rpc` → `VerifyError::Rpc` (per-call transport failure — transient;
///   surfaces as `VerificationRpcError`). NOT flattened to `Snapshot`, so the
///   seam routes transport failures to the retryable type.
///
/// `label` is the pool label (V3 address / V4 `pool_id` hex), `phase` is
/// `"snapshot"` or `"backfill"`. The verifier's own message already carries
/// pool+block+call-site detail, so this wraps it with the phase prefix
/// (mirroring the pre-VP42BP message shape).
pub(crate) fn map_liquidity_verify_error(
    e: degenbot_bot::bot_core::liquidity_verifier::LiquidityVerifyError,
    label: &str,
    phase: &str,
    block: u64,
) -> VerifyError {
    match e {
        degenbot_bot::bot_core::liquidity_verifier::LiquidityVerifyError::Mismatch(m) => {
            VerifyError::Snapshot(format!(
                "{label} at {phase} block {block}: tick data mismatch: {m}"
            ))
        }
        degenbot_bot::bot_core::liquidity_verifier::LiquidityVerifyError::Rpc { message } => {
            VerifyError::Rpc(format!("{label} at {phase} block {block}: {message}"))
        }
    }
}

impl VerifyRpc for EngineVerifyRpc<'_> {
    fn enabled(&self) -> bool {
        self.rpc_url().is_some()
    }

    fn verify_v3_snapshot(
        &self,
        pool_address: Address,
        tick_data: &HashMap<i32, degenbot_bot::bot_core::TickInfo>,
        block: u64,
    ) -> Result<(), VerifyError> {
        let provider = self.provider(&pool_address.to_string())?;
        let runtime = degenbot_core::runtime::get_runtime();
        let addr_str = pool_address.to_string();
        runtime.block_on(async {
            degenbot_bot::bot_core::liquidity_verifier::verify_v3_liquidity_map(
                &provider,
                pool_address,
                tick_data,
                block,
            )
            .await
            .map_err(|e| {
                map_liquidity_verify_error(e, &format!("V3 pool {addr_str}"), "snapshot", block)
            })
        })
    }

    fn verify_v3_backfill(
        &self,
        pools: &HashMap<u64, degenbot_bot::bot_core::V3PoolState>,
        block: u64,
    ) -> Result<(), VerifyError> {
        // Reuse the first pool's address (or zero) as the provider-construction
        // label — the legacy closure used the registered pool's address; the
        // registry key here is the pool_id (u64), so fall back to a stable label.
        let label = pools
            .values()
            .next()
            .map_or_else(|| "V3 backfill".to_string(), |p| p.address.to_string());
        let provider = self.provider(&label)?;
        let runtime = degenbot_core::runtime::get_runtime();
        runtime.block_on(async {
            degenbot_bot::bot_core::liquidity_verifier::verify_v3_pools(
                &provider,
                Address::ZERO,
                pools,
                Some(block),
            )
            .await
            .map_err(|e| {
                map_liquidity_verify_error(e, &format!("V3 pool {label}"), "backfill", block)
            })
        })
    }

    fn verify_v4_snapshot(
        &self,
        state_view: Address,
        pool_id: [u8; 32],
        tick_data: &HashMap<i32, degenbot_bot::bot_core::TickInfo>,
        block: u64,
    ) -> Result<(), VerifyError> {
        let pool_id_hex = format!("0x{}", alloy::hex::encode(pool_id));
        let provider = self.provider(&pool_id_hex)?;
        let runtime = degenbot_core::runtime::get_runtime();
        runtime.block_on(async {
            degenbot_bot::bot_core::liquidity_verifier::verify_v4_liquidity_map(
                &provider, state_view, pool_id, tick_data, block,
            )
            .await
            .map_err(|e| {
                map_liquidity_verify_error(e, &format!("V4 pool {pool_id_hex}"), "snapshot", block)
            })
        })
    }

    fn verify_v4_backfill(
        &self,
        state_view: Address,
        pools: &HashMap<u64, degenbot_bot::bot_core::V4PoolState>,
        block: u64,
    ) -> Result<(), VerifyError> {
        // Label with the first pool's id hex (deduplicated by pool_id inside the verifier).
        let pool_id_hex = pools.values().next().map_or_else(
            || "V4 backfill".to_string(),
            |p| format!("0x{}", alloy::hex::encode(p.pool_id)),
        );
        let provider = self.provider(&pool_id_hex)?;
        let runtime = degenbot_core::runtime::get_runtime();
        runtime.block_on(async {
            degenbot_bot::bot_core::liquidity_verifier::verify_v4_pools(
                &provider,
                state_view,
                pools,
                Some(block),
            )
            .await
            .map_err(|e| {
                map_liquidity_verify_error(e, &format!("V4 pool {pool_id_hex}"), "backfill", block)
            })
        })
    }
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

        let engine = self.engine.lock();
        let core = engine.core.read();
        let v3_pools = core.v3_pools_snapshot();
        let v4_pools = core.v4_pools_snapshot();
        drop(core);
        drop(engine); // Release lock before async I/O

        let runtime = degenbot_core::runtime::get_runtime();

        let provider = runtime.block_on(async {
            degenbot_rpc::provider::AlloyProvider::new(&rpc_url, 3)
                .await
                .map_err(|e| {
                    pyo3::exceptions::PyRuntimeError::new_err(format!(
                        "verify_liquidity_maps: failed to create provider: {e}"
                    ))
                })
        })?;

        // Verify V3 pools
        let v3_result = runtime.block_on(async {
            degenbot_bot::bot_core::liquidity_verifier::verify_v3_pools(
                &provider,
                tick_lens,
                &v3_pools,
                block_number,
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
            degenbot_bot::bot_core::liquidity_verifier::verify_v4_pools(
                &provider,
                state_view,
                &v4_pools,
                block_number,
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
    fn verify_v3_liquidity_maps(&self, rpc_url: String, block_number: Option<u64>) -> PyResult<()> {
        let engine = self.engine.lock();
        let v3_pools = engine.core.read().v3_pools_snapshot();
        drop(engine);

        let runtime = degenbot_core::runtime::get_runtime();
        let provider = runtime.block_on(async {
            degenbot_rpc::provider::AlloyProvider::new(&rpc_url, 3)
                .await
                .map_err(|e| {
                    pyo3::exceptions::PyRuntimeError::new_err(format!(
                        "verify_v3_liquidity_maps: failed to create provider: {e}"
                    ))
                })
        })?;

        // TickLens address not used (V3 calls pool.ticks() directly)
        let tick_lens = Address::ZERO;
        let v3_result = runtime.block_on(async {
            degenbot_bot::bot_core::liquidity_verifier::verify_v3_pools(
                &provider,
                tick_lens,
                &v3_pools,
                block_number,
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

        let engine = self.engine.lock();
        let v4_pools = engine.core.read().v4_pools_snapshot();
        drop(engine);

        let runtime = degenbot_core::runtime::get_runtime();
        let provider = runtime.block_on(async {
            degenbot_rpc::provider::AlloyProvider::new(&rpc_url, 3)
                .await
                .map_err(|e| {
                    pyo3::exceptions::PyRuntimeError::new_err(format!(
                        "verify_v4_liquidity_maps: failed to create provider: {e}"
                    ))
                })
        })?;

        let v4_result = runtime.block_on(async {
            degenbot_bot::bot_core::liquidity_verifier::verify_v4_pools(
                &provider,
                state_view,
                &v4_pools,
                block_number,
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
        self.verify_on_register
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
    }

    /// Set the HTTP RPC URL used for verification during registration.
    ///
    /// Must be called before enabling `verify_on_register`.
    #[pyo3(signature = (rpc_url))]
    fn set_verify_rpc_url(&self, rpc_url: String) {
        // Eagerly create and cache the provider so verification reuses
        // the same HTTP connection pool instead of creating a new client
        // per pool registration.
        let runtime = degenbot_core::runtime::get_runtime();
        match runtime.block_on(degenbot_rpc::provider::AlloyProvider::new(&rpc_url, 3)) {
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
}
