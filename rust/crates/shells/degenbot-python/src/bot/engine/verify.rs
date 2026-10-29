//! `PyO3` wrapper for the engine stage surface — verify `#[pymethods]` slice.
//!
//! Split out of the former monolithic `py_binding.rs` ,
//! mirroring `crates/engine/degenbot-bot/src/arb_engine/`'s per-concern
//! layout. `PyO3` allows multiple `#[pymethods] impl PyArbEngine { … }`
//! blocks per type, so each concern file contributes one slice.

use super::PyArbEngine;
use crate::prelude::*;

#[pymethods]
impl PyArbEngine {
    /// Verify a registered pool's liquidity map against on-chain state.
    ///
    /// `family` is `"v3"` (targeting `address`) or `"v4"` (targeting the
    /// `(pool_manager, pool_id_hex)` pair); the key's registered identity is
    /// also the on-chain read target — the pool contract for V3, the canonical
    /// `PoolManager` storage for V4. Every tick row, the bitmap words those rows
    /// imply, and the ±2 word window around the active tick are compared at
    /// `block_number` (the chain tip when omitted).
    ///
    /// The pool's tick map is cloned out of the engine inside `py.detach`; the
    /// comparison runs on the caller's event loop, so the method is async and
    /// returns a coroutine to await.
    ///
    /// # Errors
    ///
    /// `ValueError` on a family/target that does not parse; `RuntimeError` when
    /// the pool is not registered in the engine, the provider cannot be built,
    /// or the comparison finds a divergence.
    #[expect(
        clippy::too_many_arguments,
        reason = "the family names the key shape, so its target fields are keyword-only beside it"
    )]
    #[pyo3(signature = (family, rpc_url, block_number=None, *, address=None, pool_manager=None, pool_id_hex=None))]
    fn verify_pool<'py>(
        &self,
        py: Python<'py>,
        family: &str,
        rpc_url: &str,
        block_number: Option<u64>,
        address: Option<&str>,
        pool_manager: Option<&str>,
        pool_id_hex: Option<&str>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let key = crate::bot::registration_lifecycle::pool_lookup(
            family,
            address,
            pool_manager,
            pool_id_hex,
        )?;

        // GIL hygiene: engine Mutex + read guard acquired inside py.detach;
        // the owned tick map comes out, the PyErr (if any) is built under the GIL.
        let pool = self.with_core(py, |core| key.tick_map_snapshot(core));
        let Some(pool) = pool else {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "pool {key} not registered in engine"
            )));
        };

        let rpc_url = rpc_url.to_owned();
        crate::ambient_runtime::future_into_py(py, async move {
            let provider =
                crate::provider_seam::live_provider(&rpc_url, crate::provider::DEFAULT_MAX_RETRIES)
                    .await
                    .map_err(|e| {
                        pyo3::exceptions::PyRuntimeError::new_err(format!(
                            "verify_pool: failed to create provider: {e}"
                        ))
                    })?;

            degenbot_bot::bot_core::verify_live_map(&provider, key, pool.as_ref(), block_number)
                .await
                .map_err(|mismatch| {
                    pyo3::exceptions::PyRuntimeError::new_err(format!(
                        "pool {key} liquidity map verification FAILED: {mismatch}"
                    ))
                })
        })
    }

    /// Set the HTTP RPC URL used for verification during registration.
    ///
    /// Must be called before enabling `verify_on_register`.
    #[pyo3(signature = (rpc_url))]
    fn set_verify_rpc_url(&self, rpc_url: &str) {
        // ADR-006 D4 (T4): delegates to the shared `PumpState`.
        self.session.driver().set_verify_rpc_url(rpc_url);
    }

    /// Set the `StateView` contract address for V4 verification during registration.
    ///
    /// Must be called before any V4 pools are registered with verification enabled.
    #[pyo3(signature = (state_view_address))]
    fn set_verify_state_view(&self, state_view_address: &str) {
        // ADR-006 D4 (T4): delegates to the shared `PumpState`.
        self.session
            .driver()
            .set_verify_state_view(state_view_address);
    }
}
