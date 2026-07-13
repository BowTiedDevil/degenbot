//! `PySimulateContext` — the session-static config bag the cockpit constructs
//! once per `BackrunSession` and reuses across every block's
//! `dispatch_profitable_py` call.
//!
//! Mirrors `PyDispatcher::for_block`'s "construct once, drive many" shape, but
//! longer-lived: the executor/weth/pm/multicall addresses + the inject flag +
//! the runtime bytecode + the precomputed [`WarmupSlots`] do not vary per
//! block. The per-block parts (`base_fee_next` / `current_block` /
//! `block_priority_fees`) are `dispatch_profitable_py` call-args (A4), not
//! fields here — they are stitched into a borrowed `SimulateContext<'_>` at
//! call time.
//!
//! Holds an `Arc<AlloyProvider>` — cloned at construction via
//! [`PyAsyncAlloyProvider::provider_arc`] — so the A4 pyfunction can move an
//! owned handle into the `future_into_py` async block + construct the
//! `SimulateContext` borrowing it (the `'static` future owns the arc; the
//! `<'a>` borrow is block-local).
//!
//! # GIL discipline
//!
//! `#[new]` runs under the GIL (it must reach into the Python
//! `PyAsyncAlloyProvider` + the bytearray + the address strings). The fields
//! are plain owned data (arc + addresses + bytes); the A4 pyfunction releases
//! the GIL across the RPC `.await`s. No business logic here — pure config
//! storage (ADR-005 §3 PyO3-layer discipline).

use crate::address_utils::parse_address;
use crate::prelude::*;
use crate::provider::AlloyProvider;
use crate::rpc::async_provider::PyAsyncAlloyProvider;
use alloy::primitives::Bytes;
use degenbot_executor::{compute_simulation_warmup_slots, WarmupSlots};
use pyo3::exceptions::PyValueError;
use pyo3::types::PyBytes;
use std::sync::Arc;

/// The session-static simulation config bag.
///
/// Construct once per session from a `&PyAsyncAlloyProvider` + the addresses +
/// the inject flag + the executor runtime bytecode. Hands to
/// `dispatch_profitable_py` (A4) each block alongside the per-block args.
#[pyclass(name = "PySimulateContext", skip_from_py_object)]
// Every field is consumed by `dispatch_profitable_py` (A4, QQFTB4) when it
// stitches them into the borrowed `SimulateContext<'_>`; until A4 lands only
// `provider` is read (by the `rpc_url` getter).
#[allow(dead_code)]
pub struct PySimulateContext {
    /// The typed RPC provider handle (cloned from the `PyAsyncAlloyProvider`
    /// the cockpit holds). Moved into A4's async block; the borrowed
    /// `SimulateContext<'_>.provider` borrows `&*this`.
    pub(crate) provider: Arc<AlloyProvider>,
    pub(crate) executor_owner: alloy::primitives::Address,
    pub(crate) executor_address: alloy::primitives::Address,
    pub(crate) weth_address: alloy::primitives::Address,
    pub(crate) pool_manager_address: alloy::primitives::Address,
    pub(crate) multicall3_address: alloy::primitives::Address,
    pub(crate) inject_code: bool,
    pub(crate) injected_address: Option<alloy::primitives::Address>,
    pub(crate) runtime_bytecode: Bytes,
    /// The three warmed storage slots — precomputed once (they depend only on
    /// the injected executor + `WETH` + `PoolManager` addresses).
    pub(crate) warmup: WarmupSlots,
}

#[pymethods]
impl PySimulateContext {
    /// Build a session-long simulation context.
    ///
    /// Args:
    ///     `provider`: the `AsyncAlloyProvider` the cockpit dispatches over.
    ///         Its inner `Arc<AlloyProvider>` is cloned here; A4 moves it into
    ///         the async block.
    ///     `executor_owner`: the operator key's address (hex string) — the
    ///         `from` of `execute()` + the owner funded with ETH in
    ///         `stateOverrides`.
    ///     `executor_address`: the `cmd_executor` contract address (hex) — the
    ///         `execute()` target + the balance-diff subject.
    ///     `weth_address`: WETH9 contract address (hex).
    ///     `pool_manager_address`: Uniswap V4 `PoolManager` address (hex).
    ///     `multicall3_address`: Multicall3 contract address (hex).
    ///     `inject_code`: whether to inject the executor runtime bytecode
    ///         (the `INJECT_EXECUTOR_CODE` flag).
    ///     `executor_runtime_bytecode`: the executor runtime bytecode bytes
    ///         (the file-load stays Python; the bytes cross here).
    ///     `injected_address`: the fresh address to inject the executor at
    ///         (hex); required when `inject_code` is `True`.
    ///
    /// # Errors
    /// `ValueError`: if any address string is not a valid EIP-55 address, or
    ///         `inject_code` is `True` and `injected_address` is `None`.
    #[new]
    #[pyo3(signature = (
        provider, executor_owner, executor_address, weth_address, pool_manager_address,
        multicall3_address, inject_code, executor_runtime_bytecode, injected_address=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        provider: &PyAsyncAlloyProvider,
        executor_owner: &str,
        executor_address: &str,
        weth_address: &str,
        pool_manager_address: &str,
        multicall3_address: &str,
        inject_code: bool,
        executor_runtime_bytecode: &Bound<'_, PyBytes>,
        injected_address: Option<&str>,
    ) -> PyResult<Self> {
        let provider = provider.provider_arc();
        let owner = parse_address(executor_owner)
            .map_err(|e| PyValueError::new_err(format!("Invalid executor_owner: {e}")))?;
        let exec = parse_address(executor_address)
            .map_err(|e| PyValueError::new_err(format!("Invalid executor_address: {e}")))?;
        let weth = parse_address(weth_address)
            .map_err(|e| PyValueError::new_err(format!("Invalid weth_address: {e}")))?;
        let pm = parse_address(pool_manager_address)
            .map_err(|e| PyValueError::new_err(format!("Invalid pool_manager_address: {e}")))?;
        let multicall = parse_address(multicall3_address)
            .map_err(|e| PyValueError::new_err(format!("Invalid multicall3_address: {e}")))?;

        let injected = match injected_address {
            Some(s) => Some(
                parse_address(s)
                    .map_err(|e| PyValueError::new_err(format!("Invalid injected_address: {e}")))?,
            ),
            None => None,
        };
        if inject_code && injected.is_none() {
            return Err(PyValueError::new_err(
                "inject_code is True but injected_address is None — supply the injection address",
            ));
        }

        // Warmup slots depend only on injected + weth + pm; compute once.
        let warmup_injected = injected.unwrap_or(owner);
        let warmup = compute_simulation_warmup_slots(warmup_injected, weth, pm);

        Ok(Self {
            provider,
            executor_owner: owner,
            executor_address: exec,
            weth_address: weth,
            pool_manager_address: pm,
            multicall3_address: multicall,
            inject_code,
            injected_address: injected,
            runtime_bytecode: Bytes::from(executor_runtime_bytecode.as_bytes().to_vec()),
            warmup,
        })
    }

    /// The RPC URL the held provider points at (for the `[sim]` log line).
    #[getter]
    fn rpc_url(&self) -> &str {
        self.provider.rpc_url()
    }
}
