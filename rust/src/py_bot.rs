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

use crate::prelude::*;
use std::sync::Arc;

use alloy::primitives::Address;

use crate::py_erc20_token::PyErc20Token;
use crate::py_liquidity_pool::PyLiquidityPool;
use degenbot_bot::bot_core::state_history::JournalError;
use degenbot_bot::bot_core::{Bot, RegisterV2PoolParams, RegisterV3PoolParams};

/// Build an `alloy::rpc::types::Log` from the WS-log shape Python tests pass —
/// `(address, topics, data, block_number)` reconstructed into the same
/// `alloy::rpc::types::Log` the `BlockPump` feeds `Bot::dispatch_log`. Hex
/// strings accept an optional `0x` prefix. This is the marshalling seam for
/// the Python-facing `dispatch_log` (ADR-006, deferred §17 closure): it lets
/// an offline test drive the full pump→dispatch→notify→solve loop without a
/// live WS node, reusing the existing pure-logic dispatcher untouched.
fn build_rpc_log(
    address: &str,
    topics: Vec<String>,
    data: &str,
    block_number: u64,
) -> PyResult<alloy::rpc::types::Log> {
    let addr: Address = address.parse().map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!("Invalid address '{address}': {e}"))
    })?;
    let mut topic_hashes = Vec::with_capacity(topics.len());
    for t in topics {
        let stripped = t.strip_prefix("0x").unwrap_or(&t);
        let b: alloy::primitives::B256 = stripped.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid topic '{t}': {e}"))
        })?;
        topic_hashes.push(b);
    }
    let data_stripped = data.strip_prefix("0x").unwrap_or(data);
    let data_bytes = alloy::hex::decode(data_stripped).map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!("Invalid data hex '{data}': {e}"))
    })?;
    let inner = alloy::primitives::Log::new_unchecked(
        addr,
        topic_hashes,
        alloy::primitives::Bytes::from(data_bytes),
    );
    Ok(alloy::rpc::types::Log {
        inner,
        block_hash: None,
        block_number: Some(block_number),
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: None,
        removed: false,
    })
}

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

/// Python handle to the per-chain `Bot` orchestrator (ADR-006 D4).
///
/// Python constructs a `PyBot` (or receives a shared handle), registers
/// pools/tokens, then reads results. `PyBot` owns a [`Bot`] via `Arc` and hands
/// out clones of its shared `Arc<RwLock<BotState>>` (`state_arc`) so
/// `PyLiquidityPool` / `PyErc20Token` / `UniswapEngine` all reach ONE
/// Rust-owned `BotState`; `BlockPump` clones the same `Arc<Bot>` so its
/// `dispatch_log` writes flow through to the engine's reads (N handles → one
/// state — the Polars three-layer invariant, preserved + generalized by D4).
#[pyclass(skip_from_py_object)]
pub struct PyBot {
    bot: Arc<Bot>,
}

/// Crate-internal Rust surface on `PyBot` (not Python-visible).
impl PyBot {
    /// Hand out a clone of the shared `Arc<Bot>` so `BlockPump` (and other
    /// sibling consumers) drive the SAME `Bot`'s `dispatch_log` + state that
    /// `PyBot` owns (ADR-006 D4: pump lifecycle relocation onto `Bot`).
    #[must_use]
    pub(crate) fn bot_arc(&self) -> Arc<Bot> {
        Arc::clone(&self.bot)
    }
}

#[pymethods]
impl PyBot {
    #[new]
    fn new() -> Self {
        // chain_id = 0 placeholder until ADR-006 slice 8 makes `bot.py` a
        // single-chain facade (Python has no single chain_id at PyBot
        // construction today). The standalone-Rust path passes the real id.
        // `Arc<Bot>` so `BlockPump` clones the same orchestrator (ADR-006 D4).
        Self {
            bot: Arc::new(Bot::new(0)),
        }
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
        let r0 = crate::conversion::alloy::extract_python_u256(reserve0)?;
        let r1 = crate::conversion::alloy::extract_python_u256(reserve1)?;

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
        let r0 = crate::conversion::alloy::extract_python_u256(reserve0)?;
        let r1 = crate::conversion::alloy::extract_python_u256(reserve1)?;

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
        let amount = crate::conversion::alloy::extract_python_u256(amount_in)?;
        let result = {
            let state = self.bot.state_arc();
            let core = state.read();
            core.calculate_tokens_out(pool_id, zero_for_one, amount)
        };
        let bound = crate::conversion::alloy::u256_to_py(py, &result)?;
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
        let amount = crate::conversion::alloy::extract_python_u256(amount_out)?;
        let result = {
            let state = self.bot.state_arc();
            let core = state.read();
            core.calculate_tokens_in(pool_id, zero_for_one, amount)
        };
        let bound = crate::conversion::alloy::u256_to_py(py, &result)?;
        Ok(bound.unbind())
    }

    /// Number of registered pools.
    fn pool_count(&self) -> usize {
        self.bot.state_arc().read().pool_count()
    }

    /// Drive a raw WS log through the event bus (ADR-006 D4).
    ///
    /// This is the Python-facing mirror of the `BlockPump`'s per-log call to
    /// `Bot::dispatch_log`: decode via the registered `LogDecoder`s, apply the
    /// decoded event to the shared `BotState` under a write guard, release it,
    /// then notify every attached `PoolStateSubscriber` (the engine adapter) so
    /// the affected `pool_id` is dirtied for the next `solve_all_paths`.
    ///
    /// Reconstructs an `alloy::rpc::types::Log` from the WS-log shape Python
    /// passes — `(address, topics, data, block_number)` — so an offline test
    /// can drive the full pump→dispatch→notify→solve loop without a live node.
    /// No-op if no decoder recognizes the log or the pool isn't registered.
    ///
    /// Args:
    ///     `address`: Emitter contract address (hex string, `0x` optional).
    ///     `topics`: Log topics (hex strings, `0x` optional). `topics[0]` is
    ///         the event signature hash the decoders match against.
    ///     `data`: ABI-encoded log data (hex string, `0x` optional).
    ///     `block_number`: Block number the synthetic log belongs to.
    #[pyo3(signature = (address, topics, data, block_number=0))]
    fn dispatch_log(
        &self,
        address: &str,
        topics: Vec<String>,
        data: &str,
        block_number: u64,
    ) -> PyResult<()> {
        let log = build_rpc_log(address, topics, data, block_number)?;
        self.bot.dispatch_log(&log);
        Ok(())
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

    /// Unregister a V2/V3 pool by its contract address.
    ///
    /// ADR-007 U3. Drops the `PoolEntry`, its reorg journal, its index entry,
    /// and any buffered V3 liquidity events for the address so a re-register
    /// does not replay stale Mint/Burn onto the fresh pool. `next_pool_id` is
    /// not reused — removed ids are retired to prevent stale `PyLiquidityPool`
    /// handles aliasing to a different pool on recreate.
    ///
    /// V2/V3 path only — `PyBot` exposes `register_v2/v3_pool` (no V4;
    /// V4 registration lives on `UniswapArbEngine`, and its symmetric
    /// unregister belongs there too — see ADR-007 Consequences).
    ///
    /// Returns `True` if a pool was found and removed; `False` if the address
    /// was never registered (silent no-op, mirroring Python `PoolRegistry.remove`).
    /// Register stays refusal-on-panic for duplicates (ADR-007 U2) — the
    /// asymmetry reflects the asymmetry in the operations' invariants.
    ///
    /// Args:
    ///     `address`: The V2/V3 pool contract address (checksum or 0x-hex).
    ///     `pool_id`: Reserved — must be `None` on `PyBot`. (V4 tuple-key
    ///         unregister is engine-side; kept in the signature for parity
    ///         with `BotState::unregister_pool`.)
    ///
    /// Raises:
    ///     `ValueError`: If `address` cannot be parsed.
    #[pyo3(signature = (address, pool_id=None))]
    #[allow(clippy::needless_pass_by_value)] // PyO3 binding idiom for optional bytes
    fn unregister_pool(&self, address: &str, pool_id: Option<Vec<u8>>) -> PyResult<bool> {
        let addr = parse_address(address)?;
        // V4 on PyBot is intentionally not exposed: registration for V4 lives
        // on UniswapArbEngine (bot::engine), so the symmetric V4 unregister
        // belongs there too (ADR-007 Consequences / Deferred). A `Some` here
        // would be a caller bug — surface it rather than silently no-op.
        if pool_id.is_some() {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "PyBot.unregister_pool does not handle V4 (pool_id set); \
                 V4 unregister is engine-side — use the engine’s unregister path.",
            ));
        }
        Ok(self.bot.state_arc().write().unregister_pool(addr, None))
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
        let spx = crate::conversion::alloy::extract_python_u256(sqrt_price_x96)?;
        // liquidity is uint128 — extracted as U256 then narrowed.
        let liq = crate::conversion::alloy::extract_python_u256(liquidity)?.to::<u128>();

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
                coverage: degenbot_bot::bot_core::PoolTickCoverage::Sparse,
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
        let spx = crate::conversion::alloy::extract_python_u256(sqrt_price_x96)?;
        let liq = crate::conversion::alloy::extract_python_u256(liquidity)?.to::<u128>();

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
                        crate::conversion::alloy::u256_to_py(py, &p.sqrt_price_x96_before)?
                            .unbind(),
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
        let amount = crate::conversion::alloy::extract_python_u256(amount_out)?;
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
                        crate::conversion::alloy::u256_to_py(py, &r0)?.unbind(),
                        crate::conversion::alloy::u256_to_py(py, &r1)?.unbind(),
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
