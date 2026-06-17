//! `PyO3` wrappers for `Bot` — thin Python handle over Rust-owned state.
//!
//! Implements the Polars-inspired three-layer topology (ADR-005): Rust [`Bot`]
//! core → `PyBot` `#[pyclass]` wrapper holding `Arc<parking_lot::RwLock<Bot>>`
//! → Python `Bot` session that constructs `self._py_bot = PyBot()` in `__init__`.
//! `PyLiquidityPool`/`PyErc20Token` clone the same `Arc` so many Python handles reference one
//! Rust-owned `Bot`; reads take a read guard, mutations a write guard.
//!
//! See: `docs/adr/ADR-005-polars-inspired-three-layer-architecture.md` (the
//! decision, rejected alternatives, and the deferred `UniswapEngine` unification).

use std::sync::Arc;

use alloy::primitives::Address;
use pyo3::prelude::*;

use crate::bot_core::py_erc20_token::PyErc20Token;
use crate::bot_core::py_liquidity_pool::PyLiquidityPool;
use crate::bot_core::{Bot, RegisterV2PoolParams, RegisterV3PoolParams};

/// Encode a byte slice as a lowercase hex string (no "0x" prefix).
fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX_CHARS[(b >> 4) as usize] as char);
        s.push(HEX_CHARS[(b & 0x0f) as usize] as char);
    }
    s
}

// ---------------------------------------------------------------------------
// PyBot — thin handle over Arc<RwLock<Bot>>
// ---------------------------------------------------------------------------

/// The single owner of all runtime state.
///
/// Python constructs a `PyBot` (or receives a shared handle), registers
/// pools/tokens, then reads results. All state lives in Rust; Python holds a
/// thin handle. The wrapped `RwLock<Bot>` allows many concurrent readers
/// (e.g. per-pool `PyLiquidityPool` reads) while serialising mutation.
#[pyclass(skip_from_py_object)]
pub struct PyBot {
    core: Arc<parking_lot::RwLock<Bot>>,
}

#[pymethods]
impl PyBot {
    #[new]
    fn new() -> Self {
        Self {
            core: Arc::new(parking_lot::RwLock::new(Bot::new())),
        }
    }

    /// Register a V2 pool by contract address.
    ///
    /// Returns the auto-assigned pool ID.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (address, token0, token1, reserve0, reserve1, gamma_numer0, fee_denom0, gamma_numer1, fee_denom1, factory))]
    fn register_v2_pool(
        &self,
        address: &str,
        token0: &str,
        token1: &str,
        reserve0: &Bound<'_, PyAny>,
        reserve1: &Bound<'_, PyAny>,
        gamma_numer0: u64,
        fee_denom0: u64,
        gamma_numer1: u64,
        fee_denom1: u64,
        factory: &str,
    ) -> PyResult<u64> {
        let addr = parse_address(address)?;
        let t0 = parse_address(token0)?;
        let t1 = parse_address(token1)?;
        let fac = parse_address(factory)?;
        let r0 = crate::alloy_py::extract_python_u256(reserve0)?;
        let r1 = crate::alloy_py::extract_python_u256(reserve1)?;

        Ok(self.core.write().register_v2_pool(&RegisterV2PoolParams {
            address: addr,
            token0: t0,
            token1: t1,
            reserve0: r0,
            reserve1: r1,
            fee_token0: (gamma_numer0, fee_denom0),
            fee_token1: (gamma_numer1, fee_denom1),
            factory: fac,
        }))
    }

    /// Update a V2 pool's reserves from a Sync event.
    #[pyo3(signature = (address, reserve0, reserve1, block_number))]
    fn update_v2_pool(
        &self,
        address: &str,
        reserve0: &Bound<'_, PyAny>,
        reserve1: &Bound<'_, PyAny>,
        block_number: u64,
    ) -> PyResult<()> {
        let addr = parse_address(address)?;
        let r0 = crate::alloy_py::extract_python_u256(reserve0)?;
        let r1 = crate::alloy_py::extract_python_u256(reserve1)?;

        self.core.write().update_v2_pool(addr, r0, r1, block_number);
        Ok(())
    }

    /// Calculate the output token amount for a given input amount.
    ///
    /// Args:
    ///     `pool_id`: The pool ID returned by `register_v2_pool`.
    ///     `zero_for_one`: True for token0→token1, False for token1→token0.
    ///     `amount_in`: Input token amount (Python int).
    ///
    /// Returns:
    ///     The output token amount as a Python int.
    #[pyo3(signature = (pool_id, zero_for_one, amount_in))]
    fn calculate_tokens_out(
        &self,
        py: Python<'_>,
        pool_id: u64,
        zero_for_one: bool,
        amount_in: &Bound<'_, PyAny>,
    ) -> PyResult<Py<PyAny>> {
        let amount = crate::alloy_py::extract_python_u256(amount_in)?;
        let result = {
            let core = self.core.read();
            core.calculate_tokens_out(pool_id, zero_for_one, amount)
        };
        let bound = crate::alloy_py::u256_to_py(py, &result)?;
        Ok(bound.unbind())
    }

    /// Calculate the required input token amount for a given output amount.
    ///
    /// Args:
    ///     `pool_id`: The pool ID returned by `register_v2_pool`.
    ///     `zero_for_one`: True for token0→token1, False for token1→token0.
    ///     `amount_out`: Desired output token amount (Python int).
    ///
    /// Returns:
    ///     The required input token amount as a Python int.
    #[pyo3(signature = (pool_id, zero_for_one, amount_out))]
    fn calculate_tokens_in(
        &self,
        py: Python<'_>,
        pool_id: u64,
        zero_for_one: bool,
        amount_out: &Bound<'_, PyAny>,
    ) -> PyResult<Py<PyAny>> {
        let amount = crate::alloy_py::extract_python_u256(amount_out)?;
        let result = {
            let core = self.core.read();
            core.calculate_tokens_in(pool_id, zero_for_one, amount)
        };
        let bound = crate::alloy_py::u256_to_py(py, &result)?;
        Ok(bound.unbind())
    }

    /// Number of registered pools.
    fn pool_count(&self) -> usize {
        self.core.read().pool_count()
    }

    /// Get a thin `PyLiquidityPool` handle for the given pool ID.
    ///
    /// Args:
    ///     `pool_id`: The pool ID returned by `register_v2_pool`.
    ///
    /// Returns:
    ///     A `PyLiquidityPool` handle, or `None` if the pool ID is not registered.
    fn get_pool(&self, pool_id: u64) -> Option<PyLiquidityPool> {
        if self.core.read().has_pool(pool_id) {
            Some(PyLiquidityPool::new(Arc::clone(&self.core), pool_id))
        } else {
            None
        }
    }

    /// Register a V3 pool by contract address.
    ///
    /// Returns the auto-assigned pool ID.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (address, token0, token1, fee, tick_spacing, factory, sqrt_price_x96, liquidity, tick))]
    fn register_v3_pool(
        &self,
        address: &str,
        token0: &str,
        token1: &str,
        fee: u32,
        tick_spacing: i32,
        factory: &str,
        sqrt_price_x96: &Bound<'_, PyAny>,
        liquidity: &Bound<'_, PyAny>,
        tick: i32,
    ) -> PyResult<u64> {
        let addr = parse_address(address)?;
        let t0 = parse_address(token0)?;
        let t1 = parse_address(token1)?;
        let fac = parse_address(factory)?;
        let spx = crate::alloy_py::extract_python_u256(sqrt_price_x96)?;
        // liquidity is uint128 — extracted as U256 then narrowed.
        let liq = crate::alloy_py::extract_python_u256(liquidity)?.to::<u128>();

        Ok(self.core.write().register_v3_pool(&RegisterV3PoolParams {
            address: addr,
            token0: t0,
            token1: t1,
            fee,
            tick_spacing,
            factory: fac,
            sqrt_price_x96: spx,
            liquidity: liq,
            tick,
            tick_data: std::collections::HashMap::new(),
            update_block: 0,
            coverage: crate::bot_core::PoolTickCoverage::Sparse,
        }))
    }

    /// Update a V3 pool's state from a Swap event.
    ///
    /// No-op if the pool is not registered.
    #[pyo3(signature = (address, sqrt_price_x96, liquidity, tick, block_number))]
    fn update_v3_pool(
        &self,
        address: &str,
        sqrt_price_x96: &Bound<'_, PyAny>,
        liquidity: &Bound<'_, PyAny>,
        tick: i32,
        block_number: u64,
    ) -> PyResult<()> {
        let addr = parse_address(address)?;
        let spx = crate::alloy_py::extract_python_u256(sqrt_price_x96)?;
        let liq = crate::alloy_py::extract_python_u256(liquidity)?.to::<u128>();

        self.core
            .write()
            .update_v3_pool(addr, spx, liq, tick, block_number, vec![]);
        Ok(())
    }

    /// Get the number of deltas in the reorg journal for a V3 pool.
    ///
    /// Returns 0 if the pool ID is not registered or is not a V3 pool.
    fn v3_journal_len(&self, pool_id: u64) -> usize {
        self.core.read().v3_journal_len(pool_id)
    }

    /// Discard V3 reorg journal deltas earlier than the given block.
    #[pyo3(signature = (pool_id, block))]
    fn v3_discard_before_block(&self, pool_id: u64, block: u64) {
        self.core.write().v3_discard_before_block(pool_id, block);
    }

    /// Restore V3 pool state prior to a target block.
    ///
    /// Returns `(sqrt_price_x96, liquidity, tick, block)` as Python ints,
    /// or `None` if the pool ID is not registered or not a V3 pool.
    #[pyo3(signature = (pool_id, block))]
    fn v3_restore_before_block(
        &self,
        py: Python<'_>,
        pool_id: u64,
        block: u64,
    ) -> PyResult<Option<Py<PyAny>>> {
        let result = {
            let mut core = self.core.write();
            core.v3_restore_before_block(pool_id, block)
        };
        match result {
            Some(restore) => {
                // `restore.scalar_priors` is always `Some` post-restore: the
                // core `v3_restore_before_block` populates it with the current
                // state scalars when the rolled-back range was tick-only
                // (None internally — the scalars were never changed by the
                // rolled-back events, so the current scalars ARE the restored
                // scalars). See ADR-004.
                let p = restore
                    .scalar_priors
                    .as_ref()
                    .expect("post-restore scalar_priors must be Some");
                let liq_u128 = p.liquidity_before;
                let tuple = pyo3::types::PyTuple::new(
                    py,
                    [
                        crate::alloy_py::u256_to_py(py, &p.sqrt_price_x96_before)?.unbind(),
                        liq_u128.into_pyobject(py)?.into_any().unbind(),
                        p.tick_before.into_pyobject(py)?.into_any().unbind(),
                        restore.block.into_pyobject(py)?.into_any().unbind(),
                    ],
                )?;
                Ok(Some(tuple.into_any().unbind()))
            }
            None => Ok(None),
        }
    }

    /// Register a token.
    ///
    /// Args:
    ///     `address`: Token contract address (hex string).
    ///     `name`: Token name.
    ///     `symbol`: Token symbol.
    ///     `decimals`: Token decimals.
    ///     `chain_id`: Chain ID.
    #[pyo3(signature = (address, name, symbol, decimals, chain_id))]
    fn register_token(
        &self,
        address: &str,
        name: &str,
        symbol: &str,
        decimals: u8,
        chain_id: u64,
    ) -> PyResult<PyErc20Token> {
        let addr = parse_address(address)?;
        self.core.write().register_token(
            addr,
            name.to_string(),
            symbol.to_string(),
            decimals,
            chain_id,
        );
        Ok(PyErc20Token::new(Arc::clone(&self.core), addr))
    }

    /// Get a thin `PyErc20Token` handle for the given address.
    ///
    /// Args:
    ///     `address`: Token contract address (hex string).
    ///
    /// Returns:
    ///     A `PyErc20Token` handle, or `None` if the address is not registered.
    fn get_token(&self, address: &str) -> PyResult<Option<PyErc20Token>> {
        let addr = parse_address(address)?;
        if self.core.read().has_token(&addr) {
            Ok(Some(PyErc20Token::new(Arc::clone(&self.core), addr)))
        } else {
            Ok(None)
        }
    }

    /// Encode a V2 swap call, returning `(to_address_hex, calldata_hex, value)`.
    ///
    /// Args:
    ///     `pool_id`: The pool ID returned by `register_v2_pool`.
    ///     `zero_for_one`: True for token0→token1, False for token1→token0.
    ///     `amount_out`: Output token amount (Python int).
    ///     `recipient`: Address to receive output tokens (hex string).
    ///
    /// Returns:
    ///     A tuple `(to_hex, calldata_hex, value)` or `None` if pool not found.
    #[pyo3(signature = (pool_id, zero_for_one, amount_out, recipient))]
    fn encode_swap(
        &self,
        pool_id: u64,
        zero_for_one: bool,
        amount_out: &Bound<'_, PyAny>,
        recipient: &str,
    ) -> PyResult<Option<(String, String, u64)>> {
        let amount = crate::alloy_py::extract_python_u256(amount_out)?;
        let recip = parse_address(recipient)?;

        let result = {
            let core = self.core.read();
            core.encode_swap(pool_id, zero_for_one, amount, recip)
        };

        Ok(result.map(|call| {
            let to_hex = format!("{:#x}", call.to);
            let data_hex = format!("0x{}", bytes_to_hex(&call.data));
            (to_hex, data_hex, call.value.to::<u64>())
        }))
    }

    /// Get the number of deltas in the reorg journal for a V2 pool.
    ///
    /// Returns 0 if the pool ID is not registered.
    fn v2_journal_len(&self, pool_id: u64) -> usize {
        self.core.read().v2_journal_len(pool_id)
    }

    /// Discard V2 reorg journal deltas earlier than the given block.
    ///
    /// No-op if the pool ID is not registered.
    ///
    /// Raises:
    ///     `ValueError`: If all deltas are before the target block.
    #[pyo3(signature = (pool_id, block))]
    fn v2_discard_before_block(&self, pool_id: u64, block: u64) {
        self.core.write().v2_discard_before_block(pool_id, block);
    }

    /// Restore V2 pool state prior to a target block.
    ///
    /// Pops reorg journal deltas at/after the target block and restores
    /// "before" values into the current state.
    ///
    /// Returns `(reserve0, reserve1, block)` as Python ints, or `None`
    /// if the pool ID is not registered.
    ///
    /// Raises:
    ///     `ValueError`: If no delta exists before the target block.
    #[pyo3(signature = (pool_id, block))]
    fn v2_restore_before_block(
        &self,
        py: Python<'_>,
        pool_id: u64,
        block: u64,
    ) -> PyResult<Option<Py<PyAny>>> {
        let result = {
            let mut core = self.core.write();
            core.v2_restore_before_block(pool_id, block)
        };
        match result {
            Some((r0, r1, blk)) => {
                let tuple = pyo3::types::PyTuple::new(
                    py,
                    [
                        crate::alloy_py::u256_to_py(py, &r0)?.unbind(),
                        crate::alloy_py::u256_to_py(py, &r1)?.unbind(),
                        blk.into_pyobject(py)?.into_any().unbind(),
                    ],
                )?;
                Ok(Some(tuple.into_any().unbind()))
            }
            None => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_address(s: &str) -> PyResult<Address> {
    s.parse()
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("Invalid address '{s}': {e}")))
}
