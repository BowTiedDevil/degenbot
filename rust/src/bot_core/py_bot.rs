//! `PyO3` wrappers for `BotState` — thin Python handle over Rust-owned state.
//!
//! Implements the Polars-inspired three-layer topology (ADR-005): Rust [`BotState`]
//! core → `PyBot` `#[pyclass]` wrapper holding `Arc<parking_lot::RwLock<BotState>>`
//! → Python `BotState` session that constructs `self._py_bot = PyBot()` in `__init__`.
//! `PyLiquidityPool`/`PyErc20Token` clone the same `Arc` so many Python handles reference one
//! Rust-owned `BotState`; reads take a read guard, mutations a write guard.
//!
//! See: `docs/adr/ADR-005-polars-inspired-three-layer-architecture.md` (the
//! decision, rejected alternatives, and the deferred `UniswapEngine` unification).

use std::sync::Arc;

use alloy::primitives::Address;
use pyo3::prelude::*;

use crate::bot_core::py_erc20_token::PyErc20Token;
use crate::bot_core::py_liquidity_pool::PyLiquidityPool;
use crate::bot_core::state_history::JournalError;
use crate::bot_core::{Bot, BotState, RegisterV2PoolParams, RegisterV3PoolParams};

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
// PyBot — owns the Bot orchestrator; hands out shared BotState Arcs
// ---------------------------------------------------------------------------

/// The Python handle to the per-chain `Bot` orchestrator (ADR-006 D4).
///
/// Python constructs a `PyBot` (or receives a shared handle), registers
/// pools/tokens, then reads results. `PyBot` owns a [`Bot`] outright and hands
/// out clones of its shared `Arc<RwLock<BotState>>` (`state_arc`) so
/// `PyLiquidityPool` / `PyErc20Token` / `UniswapEngine` all reach ONE
/// Rust-owned `BotState` (N handles → one state — the Polars three-layer
/// invariant, preserved).
#[pyclass(skip_from_py_object)]
pub struct PyBot {
    bot: Bot,
}

/// Crate-internal Rust surface on `PyBot` (not Python-visible).
impl PyBot {
    /// Hand out a clone of the shared `Arc<RwLock<BotState>>` so a sibling
    /// Rust-owned consumer (notably `UniswapEngine::with_core` — ADR-006 D1)
    /// can read/write the SAME state that `PyBot`/`PyLiquidityPool`/
    /// `PyErc20Token` share. This is the seam that dissolves the dual-state
    /// split (pump in `BotState` B, handles in `BotState` A —
    /// `rust-owned-bot.md` §17).
    #[must_use]
    pub(crate) fn core_arc(&self) -> Arc<parking_lot::RwLock<BotState>> {
        self.bot.state_arc()
    }
}

#[pymethods]
impl PyBot {
    #[new]
    fn new() -> Self {
        // chain_id = 0 placeholder until ADR-006 slice 8 makes `bot.py` a
        // single-chain facade (Python has no single chain_id at PyBot
        // construction today). The standalone-Rust path passes the real id.
        Self { bot: Bot::new(0) }
    }

    /// Register a V2 pool by contract address.
    ///
    /// Returns the auto-assigned pool ID.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (address, token0, token1, reserve0, reserve1, gamma_numer0, fee_denom0, gamma_numer1, fee_denom1, factory, update_block=0))]
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
        update_block: u64,
    ) -> PyResult<u64> {
        let addr = parse_address(address)?;
        let t0 = parse_address(token0)?;
        let t1 = parse_address(token1)?;
        let fac = parse_address(factory)?;
        let r0 = crate::alloy_py::extract_python_u256(reserve0)?;
        let r1 = crate::alloy_py::extract_python_u256(reserve1)?;

        Ok(self
            .bot
            .state_arc()
            .write()
            .register_v2_pool(&RegisterV2PoolParams {
                address: addr,
                token0: t0,
                token1: t1,
                reserve0: r0,
                reserve1: r1,
                fee_token0: (gamma_numer0, fee_denom0),
                fee_token1: (gamma_numer1, fee_denom1),
                factory: fac,
                update_block,
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

        self.bot
            .state_arc()
            .write()
            .update_v2_pool(addr, r0, r1, block_number);
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
            let state = self.bot.state_arc();
            let core = state.read();
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
            let state = self.bot.state_arc();
            let core = state.read();
            core.calculate_tokens_in(pool_id, zero_for_one, amount)
        };
        let bound = crate::alloy_py::u256_to_py(py, &result)?;
        Ok(bound.unbind())
    }

    /// Number of registered pools.
    fn pool_count(&self) -> usize {
        self.bot.state_arc().read().pool_count()
    }

    /// Get a thin `PyLiquidityPool` handle for the given pool ID.
    ///
    /// Args:
    ///     `pool_id`: The pool ID returned by `register_v2_pool`.
    ///
    /// Returns:
    ///     A `PyLiquidityPool` handle, or `None` if the pool ID is not registered.
    fn get_pool(&self, pool_id: u64) -> Option<PyLiquidityPool> {
        if self.bot.state_arc().read().has_pool(pool_id) {
            Some(PyLiquidityPool::new(self.bot.state_arc(), pool_id))
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

        Ok(self
            .bot
            .state_arc()
            .write()
            .register_v3_pool(&RegisterV3PoolParams {
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

        self.bot
            .state_arc()
            .write()
            .update_v3_pool(addr, spx, liq, tick, block_number, vec![]);
        Ok(())
    }

    /// Get the number of deltas in the reorg journal for a V3 pool.
    ///
    /// Returns 0 if the pool ID is not registered or is not a V3 pool.
    fn v3_journal_len(&self, pool_id: u64) -> usize {
        self.bot.state_arc().read().v3_journal_len(pool_id)
    }

    /// Discard V3 reorg journal deltas earlier than the given block.
    /// Discard V3 reorg journal deltas earlier than the given block.
    ///
    /// No-op if the earliest delta is at/after the target; errors if the target
    /// is past the newest delta. Raises `ValueError` on error (ADR-005 slice 4).
    #[pyo3(signature = (pool_id, block))]
    fn v3_discard_before_block(&self, pool_id: u64, block: u64) -> PyResult<()> {
        self.bot
            .state_arc()
            .write()
            .v3_discard_before_block(pool_id, block)
            .map_err(journal_err_to_py)
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
            let state = self.bot.state_arc();
            let mut core = state.write();
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
        self.bot.state_arc().write().register_token(
            addr,
            name.to_string(),
            symbol.to_string(),
            decimals,
            chain_id,
        );
        Ok(PyErc20Token::new(self.bot.state_arc(), addr))
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
        if self.bot.state_arc().read().has_token(&addr) {
            Ok(Some(PyErc20Token::new(self.bot.state_arc(), addr)))
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
            let state = self.bot.state_arc();
            let core = state.read();
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
        self.bot.state_arc().read().v2_journal_len(pool_id)
    }

    /// Discard V2 reorg journal deltas earlier than the given block.
    ///
    /// No-op if the earliest delta is at/after the target; errors if the
    /// target is past the newest delta (would remove every known state).
    ///
    /// Raises:
    ///     `ValueError`: If the target is past the newest delta.
    #[pyo3(signature = (pool_id, block))]
    fn v2_discard_before_block(&self, pool_id: u64, block: u64) -> PyResult<()> {
        self.bot
            .state_arc()
            .write()
            .v2_discard_before_block(pool_id, block)
            .map_err(journal_err_to_py)
    }

    /// Restore V2 pool state prior to a target block.
    ///
    /// Pops reorg journal deltas at/after the target block and restores the
    /// landed-at state into the current fluid fields.
    ///
    /// Returns `(reserve0, reserve1, block)` as Python ints, or `None`
    /// if the pool ID is not registered.
    ///
    /// Raises:
    ///     `ValueError`: If the target is at or before the registration block
    ///         (no state exists before it) — rolling back past registration is
    ///         a hard error, not a silent no-op (ADR-005 slice 4 decision 3).
    #[pyo3(signature = (pool_id, block))]
    fn v2_restore_before_block(
        &self,
        py: Python<'_>,
        pool_id: u64,
        block: u64,
    ) -> PyResult<Option<Py<PyAny>>> {
        let result = {
            let state = self.bot.state_arc();
            let mut core = state.write();
            core.v2_restore_before_block(pool_id, block)
        };
        match result {
            None => Ok(None),
            Some(Err(e)) => Err(journal_err_to_py(e)),
            Some(Ok((r0, r1, blk))) => {
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

/// Map a [`JournalError`] to a Python `ValueError` with the `NoPoolStateAvailable`
///-shaped message the Python pool companion expects (and re-raises as
/// `NoPoolStateAvailable`). ADR-005 slice 4 decision 2: reorg errors that used
/// to panic must surface as `ValueError`. Shared by `PyBot` and `PyLiquidityPool`.
pub(crate) fn journal_err_to_py(e: JournalError) -> PyErr {
    match e {
        JournalError::NoStatePriorToBlock { block } => pyo3::exceptions::PyValueError::new_err(
            format!("No pool state known prior to block {block}"),
        ),
        JournalError::NoStateAtOrAfterBlock { block } => pyo3::exceptions::PyValueError::new_err(
            format!("No pool state known at or after block {block}"),
        ),
    }
}
