//! `PyLiquidityPool` — thin Python handle over a `pool_id` key into `BotState`.
//!
//! Shares the same `Arc<parking_lot::RwLock<BotState>>` as the owning `PyBot` (one
//! Rust-owned `BotState`, many thin Python handles). Part of the Polars-inspired
//! three-layer topology — see `docs/adr/ADR-005-polars-inspired-three-layer-architecture.md`.
//!
//! Owns no state — property reads and calculation calls cross `PyO3` on every
//! access, locking the shared `BotState` for reading.

use crate::bot::token::PyErc20Token;
use crate::prelude::*;
use alloy::primitives::U256;
use std::collections::HashMap;
use std::sync::Arc;

use pyo3::types::{PyDict, PyList};

use crate::bot::journal_err_to_py;
use degenbot_bot::bot_core::{
    BalancerStablePoolIdentity, BalancerWeightedPoolIdentity, BotState, CurvePoolIdentity,
    SimulateSwapError, TickInfo,
};

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

/// `PyO3` adapter wrapping a Python fetch-word callable as a
/// [`TickWordFetcher`] (ADR-005 sparse-map parity, slice 3).
///
/// The callable is `fetcher(word: int, block: int) ->
/// dict[int, tuple[int, int, int]] | None`. It must RETURN the fetched tick data
/// (not write it back via `update_tick_data`) — the Rust loop merges the
/// returned [`FetchedTickWord`] itself. See [`PyLiquidityPool::calculate_tokens_out_with_fetch`].
#[derive(Debug)]
struct PyTickWordFetcher {
    callback: pyo3::Py<pyo3::PyAny>,
}

/// Wrap a Python callable as a stored `Arc<dyn TickWordFetcher>` for
/// registration-time storage on `V3PoolState`/`V4PoolState` (ADR-006 I/O trait
/// object). The callable signature is `fetcher(word, block) -> dict | None`
/// (same as the per-call path this replaces).
pub(crate) fn make_tick_fetcher(
    callback: pyo3::Py<pyo3::PyAny>,
) -> std::sync::Arc<dyn degenbot_pools::tick_fetch::TickWordFetcher> {
    std::sync::Arc::new(PyTickWordFetcher { callback })
}

/// Construct a Chain-arm `TickBootstrapRpc` from a `PyBotIo`'s native alloy
/// provider, if present (5NT2OC / NOD4PS — Option B: route the Chain arm
/// through the pure-Rust [`AlloyTickBootstrapRpc`]).
///
/// Returns `None` when the `PyBotIo` has no native alloy provider (legacy
/// Python test doubles) → the caller passes `chain=None` to the assemble helper,
/// preserving the current (no-Chain-arm) behavior.
///
/// The returned `Arc<dyn TickBootstrapRpc>` is `Send + Sync` + holds no GIL
/// state — the full choreography (bitmap decode, bit enumeration, per-tick
/// `eth_call`) runs in pure Rust under `py.detach`, no per-RPC GIL re-entry
/// (the architectural end state: Rust is the engine; Python is a driver
/// shell, not a co-implementation).
#[must_use]
pub(crate) fn make_tick_bootstrap_rpc(
    io: &crate::bot::py_bot_io::PyBotIo,
) -> Option<std::sync::Arc<dyn degenbot_pools::tick_fetch::TickBootstrapRpc>> {
    io.alloy_provider().map(|provider| {
        std::sync::Arc::new(degenbot_rpc::AlloyTickBootstrapRpc::new(provider))
            as std::sync::Arc<dyn degenbot_pools::tick_fetch::TickBootstrapRpc>
    })
}

impl degenbot_pools::tick_fetch::TickWordFetcher for PyTickWordFetcher {
    fn fetch_missing_tick_word(
        &self,
        _pool_id: u64,
        word: i32,
        block: u64,
    ) -> Result<
        degenbot_pools::tick_fetch::FetchedTickWord,
        degenbot_pools::tick_fetch::FetchTickWordError,
    > {
        use degenbot_pools::tick_fetch::{FetchTickWordError, FetchedTickWord};
        Python::attach(|py| {
            let result = self
                .callback
                .call1(py, (word, block))
                .map_err(|_| FetchTickWordError::FetchFailed)?;
            // `None` or `{}` → empty word (known, no initialized ticks).
            let bound = result.bind(py);
            if bound.is_none() {
                return Ok(FetchedTickWord {
                    word,
                    ticks: HashMap::new(),
                });
            }
            // Extract `{tick: (gross, net, block)}` directly into a HashMap.
            let parsed: HashMap<i32, (u128, i128, u64)> = bound
                .extract()
                .map_err(|_| FetchTickWordError::FetchFailed)?;
            let ticks = parsed
                .into_iter()
                .map(|(tick, (gross, net, blk))| {
                    (
                        tick,
                        TickInfo {
                            liquidity_gross: alloy::primitives::U128::from(gross),
                            liquidity_net: alloy::primitives::I256::try_from(net)
                                .unwrap_or(alloy::primitives::I256::ZERO),
                            block: blk,
                        },
                    )
                })
                .collect();
            Ok(FetchedTickWord { word, ticks })
        })
    }
}

// ---------------------------------------------------------------------------
// Balancer rate-provider Py adapter (ADR-005 slice 12c I/O trait object).
// ---------------------------------------------------------------------------

/// `PyO3` adapter wrapping a Python rate-provider callable (or any object
/// exposing `get_rates(block_identifier) -> tuple[int, ...]`) as a stored
/// `Arc<dyn BalancerRateProvider>` for registration-time storage on
/// `BalancerStablePoolState`.
///
/// The callable signature is `get_rates(block_identifier: int | None) ->
/// tuple[int, ...]` (one rate per token, `1e18` for tokens without a rate
/// provider). Mirrors [`PyTickWordFetcher`]'s GIL re-entry discipline: the
/// provider re-enters via `Python::attach`, holds no `BotState` lock across
/// the call, and returns the rates for the caller to merge.
#[derive(Debug)]
pub(crate) struct PyBalancerRateProvider {
    callback: pyo3::Py<pyo3::PyAny>,
}

/// Wrap a Python rate-provider object as a stored
/// `Arc<dyn BalancerRateProvider>`. `None` should be passed through (not
/// wrapped) to keep the static `1e18` fallback.
#[must_use]
pub fn make_balancer_rate_provider(
    callback: pyo3::Py<pyo3::PyAny>,
) -> Arc<dyn degenbot_pools::rate_provider::BalancerRateProvider> {
    Arc::new(PyBalancerRateProvider { callback })
}

impl degenbot_pools::rate_provider::BalancerRateProvider for PyBalancerRateProvider {
    fn get_rates(
        &self,
        block_identifier: Option<u64>,
    ) -> Result<Vec<alloy::primitives::U256>, degenbot_pools::rate_provider::RateProviderError>
    {
        use degenbot_pools::rate_provider::RateProviderError;
        pyo3::Python::attach(|py| {
            let py_none = pyo3::types::PyNone::get(py);
            let result = match block_identifier {
                Some(b) => self
                    .callback
                    .call_method1(py, "get_rates", (b,))
                    .map_err(|_| RateProviderError::FetchFailed),
                None => self
                    .callback
                    .call_method1(py, "get_rates", (py_none,))
                    .map_err(|_| RateProviderError::FetchFailed),
            }?;
            let bound = result.bind(py);
            // Accept any iterable of ints; the length check is the caller's
            // responsibility (it knows the token count).
            let rates: Vec<alloy::primitives::U256> = bound
                .try_iter()
                .map_err(|_| RateProviderError::FetchFailed)?
                .map(|item| {
                    let v: u128 = item
                        .map_err(|_| RateProviderError::FetchFailed)?
                        .extract()
                        .map_err(|_| RateProviderError::FetchFailed)?;
                    Ok(alloy::primitives::U256::from(v))
                })
                .collect::<Result<_, _>>()?;
            Ok(rates)
        })
    }
}

// ---------------------------------------------------------------------------
// Curve data-provider Py adapter (ADR-005 JFGCHJ I/O trait object).
// ---------------------------------------------------------------------------

/// `PyO3` adapter wrapping a Python `CurveDataProvider` as a stored
/// `Arc<dyn CurveDataProvider>`. Each read re-enters via `Python::attach` and
/// calls the matching Python method by name; the return is converted to the
/// Rust type. Caching stays in the Python `PerBlockCache` / the adapter — the
/// trait is the *read* interface (see the task note).
#[derive(Debug)]
pub(crate) struct PyCurveDataProvider {
    callback: pyo3::Py<pyo3::PyAny>,
}

/// Wrap a Python `CurveDataProvider` object as a stored
/// `Arc<dyn CurveDataProvider>` for registration-time storage on
/// `CurvePoolState`.
#[must_use]
pub fn make_curve_data_provider(
    callback: pyo3::Py<pyo3::PyAny>,
) -> Arc<dyn degenbot_pools::curve_data_provider::CurveDataProvider> {
    Arc::new(PyCurveDataProvider { callback })
}

impl degenbot_pools::curve_data_provider::CurveDataProvider for PyCurveDataProvider {
    fn block_number(
        &self,
    ) -> Result<u64, degenbot_pools::curve_data_provider::CurveDataProviderError> {
        use degenbot_pools::curve_data_provider::CurveDataProviderError;
        pyo3::Python::attach(|py| {
            let result = self
                .callback
                .call_method0(py, "block_number")
                .map_err(|_| CurveDataProviderError::FetchFailed)?;
            let v: u64 = result
                .extract(py)
                .map_err(|_| CurveDataProviderError::FetchFailed)?;
            Ok(v)
        })
    }

    fn block_timestamp(
        &self,
        block_number: u64,
    ) -> Result<u64, degenbot_pools::curve_data_provider::CurveDataProviderError> {
        use degenbot_pools::curve_data_provider::CurveDataProviderError;
        pyo3::Python::attach(|py| {
            let result = self
                .callback
                .call_method1(py, "block_timestamp", (block_number,))
                .map_err(|_| CurveDataProviderError::FetchFailed)?;
            let v: u64 = result
                .extract(py)
                .map_err(|_| CurveDataProviderError::FetchFailed)?;
            Ok(v)
        })
    }

    fn token_balance(
        &self,
        token_address: alloy::primitives::Address,
        holder_address: alloy::primitives::Address,
        block_number: u64,
    ) -> Result<alloy::primitives::U256, degenbot_pools::curve_data_provider::CurveDataProviderError>
    {
        use degenbot_pools::curve_data_provider::CurveDataProviderError;
        pyo3::Python::attach(|py| {
            let tok = address_utils::address_to_checksum_string(&token_address);
            let holder = address_utils::address_to_checksum_string(&holder_address);
            let result = self
                .callback
                .call_method1(py, "token_balance", (tok, holder, block_number))
                .map_err(|_| CurveDataProviderError::FetchFailed)?;
            let v: u128 = result
                .extract(py)
                .map_err(|_| CurveDataProviderError::FetchFailed)?;
            Ok(alloy::primitives::U256::from(v))
        })
    }

    fn token_total_supply(
        &self,
        token_address: alloy::primitives::Address,
        block_number: u64,
    ) -> Result<alloy::primitives::U256, degenbot_pools::curve_data_provider::CurveDataProviderError>
    {
        use degenbot_pools::curve_data_provider::CurveDataProviderError;
        pyo3::Python::attach(|py| {
            let tok = address_utils::address_to_checksum_string(&token_address);
            let result = self
                .callback
                .call_method1(py, "token_total_supply", (tok, block_number))
                .map_err(|_| CurveDataProviderError::FetchFailed)?;
            let v: u128 = result
                .extract(py)
                .map_err(|_| CurveDataProviderError::FetchFailed)?;
            Ok(alloy::primitives::U256::from(v))
        })
    }

    fn lending_rates(
        &self,
        block_number: u64,
    ) -> Result<
        Vec<alloy::primitives::U256>,
        degenbot_pools::curve_data_provider::CurveDataProviderError,
    > {
        self.read_u256_vec("lending_rates", block_number)
    }

    fn d(
        &self,
        block_number: u64,
    ) -> Result<alloy::primitives::U256, degenbot_pools::curve_data_provider::CurveDataProviderError>
    {
        self.read_u256("d", block_number)
    }

    fn gamma(
        &self,
        block_number: u64,
    ) -> Result<alloy::primitives::U256, degenbot_pools::curve_data_provider::CurveDataProviderError>
    {
        self.read_u256("gamma", block_number)
    }

    fn price_scale(
        &self,
        block_number: u64,
    ) -> Result<
        Vec<alloy::primitives::U256>,
        degenbot_pools::curve_data_provider::CurveDataProviderError,
    > {
        self.read_u256_vec("price_scale", block_number)
    }

    fn admin_balances(
        &self,
        block_number: u64,
    ) -> Result<
        Vec<alloy::primitives::U256>,
        degenbot_pools::curve_data_provider::CurveDataProviderError,
    > {
        self.read_u256_vec("admin_balances", block_number)
    }

    fn redemption_price(
        &self,
        block_number: u64,
    ) -> Result<alloy::primitives::U256, degenbot_pools::curve_data_provider::CurveDataProviderError>
    {
        self.read_u256("redemption_price", block_number)
    }

    fn base_cache_updated(
        &self,
        block_number: u64,
    ) -> Result<u64, degenbot_pools::curve_data_provider::CurveDataProviderError> {
        use degenbot_pools::curve_data_provider::CurveDataProviderError;
        pyo3::Python::attach(|py| {
            let result = self
                .callback
                .call_method1(py, "base_cache_updated", (block_number,))
                .map_err(|_| CurveDataProviderError::FetchFailed)?;
            let v: u64 = result
                .extract(py)
                .map_err(|_| CurveDataProviderError::FetchFailed)?;
            Ok(v)
        })
    }

    fn base_virtual_price(
        &self,
        block_number: u64,
    ) -> Result<alloy::primitives::U256, degenbot_pools::curve_data_provider::CurveDataProviderError>
    {
        self.read_u256("base_virtual_price", block_number)
    }

    fn virtual_price(
        &self,
        block_number: u64,
    ) -> Result<alloy::primitives::U256, degenbot_pools::curve_data_provider::CurveDataProviderError>
    {
        self.read_u256("virtual_price", block_number)
    }
}

impl PyCurveDataProvider {
    /// Call a `(&self, block_number) -> int` Python method → `U256`.
    fn read_u256(
        &self,
        method: &str,
        block_number: u64,
    ) -> Result<alloy::primitives::U256, degenbot_pools::curve_data_provider::CurveDataProviderError>
    {
        use degenbot_pools::curve_data_provider::CurveDataProviderError;
        pyo3::Python::attach(|py| {
            let result = self
                .callback
                .call_method1(py, method, (block_number,))
                .map_err(|_| CurveDataProviderError::FetchFailed)?;
            let v: u128 = result
                .extract(py)
                .map_err(|_| CurveDataProviderError::FetchFailed)?;
            Ok(alloy::primitives::U256::from(v))
        })
    }

    /// Call a `(&self, block_number) -> iterable[int]` Python method → `Vec<U256>`.
    fn read_u256_vec(
        &self,
        method: &str,
        block_number: u64,
    ) -> Result<
        Vec<alloy::primitives::U256>,
        degenbot_pools::curve_data_provider::CurveDataProviderError,
    > {
        use degenbot_pools::curve_data_provider::CurveDataProviderError;
        pyo3::Python::attach(|py| {
            let result = self
                .callback
                .call_method1(py, method, (block_number,))
                .map_err(|_| CurveDataProviderError::FetchFailed)?;
            let bound = result.bind(py);
            let rates: Vec<alloy::primitives::U256> = bound
                .try_iter()
                .map_err(|_| CurveDataProviderError::FetchFailed)?
                .map(|item| {
                    let v: u128 = item
                        .map_err(|_| CurveDataProviderError::FetchFailed)?
                        .extract()
                        .map_err(|_| CurveDataProviderError::FetchFailed)?;
                    Ok(alloy::primitives::U256::from(v))
                })
                .collect::<Result<_, _>>()?;
            Ok(rates)
        })
    }
}

/// A thin Python handle to a pool registered in `BotState`.
///
/// Does not own any state — all data lives in Rust inside `BotState`.
#[pyclass(skip_from_py_object, module = "degenbot._ffi")]
pub struct PyLiquidityPool {
    core: Arc<parking_lot::RwLock<BotState>>,
    pool_id: u64,
}

impl PyLiquidityPool {
    /// Create a new thin pool handle.
    pub(crate) const fn new(core: Arc<parking_lot::RwLock<BotState>>, pool_id: u64) -> Self {
        Self { core, pool_id }
    }

    /// Clone-out the stored `CurveDataProvider` (if any) for this handle's
    /// Curve state, releasing the read guard before a (potentially
    /// re-entrant) provider call.
    fn curve_provider(
        &self,
    ) -> Option<std::sync::Arc<dyn degenbot_pools::curve_data_provider::CurveDataProvider>> {
        let core = self.core.read();
        core.get_curve_pool(self.pool_id)?.data_provider.clone()
    }

    /// Read a `Vec<U256>` from the stored Curve data provider via `f`,
    /// converting to a Python list. Empty list ⇔ no provider / error.
    fn read_provider_vec(
        &self,
        py: Python<'_>,
        f: impl Fn(
            &std::sync::Arc<dyn degenbot_pools::curve_data_provider::CurveDataProvider>,
        ) -> Result<
            Vec<alloy::primitives::U256>,
            degenbot_pools::curve_data_provider::CurveDataProviderError,
        >,
    ) -> PyResult<Py<PyAny>> {
        let Some(provider) = self.curve_provider() else {
            return Ok(pyo3::types::PyList::empty(py).into_any().unbind());
        };
        let values = f(&provider).unwrap_or_default();
        let py_vals: Vec<Py<PyAny>> = values
            .iter()
            .map(|b| crate::conversion::alloy::u256_to_py(py, b).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        Ok(pyo3::types::PyList::new(py, py_vals)?.into_any().unbind())
    }

    /// Read a single `U256` from the stored Curve data provider via `f`,
    /// converting to a Python int. `None` ⇔ no provider / error.
    fn read_provider_opt(
        &self,
        py: Python<'_>,
        f: impl Fn(
            &std::sync::Arc<dyn degenbot_pools::curve_data_provider::CurveDataProvider>,
        ) -> Result<
            alloy::primitives::U256,
            degenbot_pools::curve_data_provider::CurveDataProviderError,
        >,
    ) -> PyResult<Option<Py<PyAny>>> {
        let Some(provider) = self.curve_provider() else {
            return Ok(None);
        };
        match f(&provider) {
            Ok(v) => Ok(Some(crate::conversion::alloy::u256_to_py(py, &v)?.unbind())),
            Err(_) => Ok(None),
        }
    }

    /// Shared override-sim inner: extracts Python args, builds the fetcher
    /// adapter, locks the core, calls `simulate_swap_with_override`.
    #[allow(clippy::too_many_arguments)]
    fn sim_override_inner(
        &self,
        zero_for_one: bool,
        amount: alloy::primitives::U256,
        exact_output: bool,
        block: u64,
        override_sqrt_price_x96: &Bound<'_, PyAny>,
        override_liquidity: &Bound<'_, PyAny>,
        override_tick: &Bound<'_, PyAny>,
        override_tick_data: &Bound<'_, PyAny>,
        sqrt_price_limit_x96: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Option<degenbot_bot::bot_core::V3SwapOutcome>> {
        let override_sqrt = crate::conversion::alloy::extract_python_u256(override_sqrt_price_x96)?;
        let override_liquidity = u128::try_from(crate::conversion::alloy::extract_python_u256(
            override_liquidity,
        )?)
        .map_err(|_| {
            pyo3::exceptions::PyOverflowError::new_err("override_liquidity must fit in u128")
        })?;
        let override_tick =
            i32::try_from(override_tick.extract::<i64>().map_err(|_| {
                pyo3::exceptions::PyTypeError::new_err("override_tick must be an int")
            })?)
            .map_err(|_| {
                pyo3::exceptions::PyOverflowError::new_err("override_tick must fit in i32")
            })?;
        let rust_tick_data = extract_tick_data(override_tick_data)?;
        let sqrt_price_limit = match sqrt_price_limit_x96 {
            Some(v) if !v.is_none() => crate::conversion::alloy::extract_python_u256(v)?,
            _ => degenbot_bot::bot_core::V3PoolState::default_sqrt_price_limit(zero_for_one),
        };
        let outcome = {
            let core = self.core.read();
            core.simulate_swap_with_override(
                self.pool_id,
                zero_for_one,
                amount,
                exact_output,
                sqrt_price_limit,
                override_sqrt,
                override_liquidity,
                override_tick,
                rust_tick_data,
                block,
            )
        };
        Ok(outcome)
    }

    /// The pool ID this handle references.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.pool_id
    }
}

#[pymethods]
impl PyLiquidityPool {
    /// The pool ID this handle references.
    #[getter]
    #[allow(clippy::missing_const_for_fn)]
    fn pool_id(&self) -> u64 {
        self.pool_id
    }

    /// Calculate the output token amount for a given input amount.
    ///
    /// Surfaces the cdbc03bb on-chain-equivalent revert: when the constant-
    /// product / CL swap math overflows a `uint256` intermediate (mirroring
    /// on-chain `getAmountOut` `SafeMath` revert), this raises `ValueError` so the
    /// Python companion can translate it to a domain `LiquidityPoolError`.
    /// A V3/V4 sparse-map miss is still mapped to 0 — callers needing the miss
    /// surfaced use [`calculate_tokens_out_with_fetch`][Self::calculate_tokens_out_with_fetch].
    ///
    /// Raises:
    ///     `ValueError`: If the swap math overflows `uint256` (on-chain revert).
    #[pyo3(signature = (zero_for_one, amount_in))]
    fn calculate_tokens_out(
        &self,
        py: Python<'_>,
        zero_for_one: bool,
        amount_in: &Bound<'_, PyAny>,
    ) -> PyResult<Py<PyAny>> {
        let amount = crate::conversion::alloy::extract_python_u256(amount_in)?;
        let result = {
            let core = self.core.read();
            core.calculate_tokens_out_miss_aware(self.pool_id, zero_for_one, amount)
        };
        // cdbc03bb: V2/V3/V4 swap math reverts on `uint256` overflow (mirrors
        // on-chain `getAmountOut` SafeMath revert). The plain
        // `calculate_tokens_out` surfaces that revert as a Python `ValueError`
        // (the V2 companion translates it to a domain `LiquidityPoolError`,
        // mirroring `calculate_tokens_in_from_tokens_out`'s overdraw-sentinel
        // translation). A V3/V4 sparse-map miss stays mapped to 0 so callers
        // that don't opt into `calculate_tokens_out_with_fetch` keep the
        // legacy no-raise-on-miss contract documented on the underlying
        // `BotState::calculate_tokens_out`.
        let out = match result {
            Ok(v) => v,
            Err(SimulateSwapError::MissingTickWord(_)) => U256::ZERO,
            Err(SimulateSwapError::NotComputable) => {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "Pool swap math overflowed uint256 intermediate (on-chain getAmountOut SafeMath revert)",
                ));
            }
        };
        let bound = crate::conversion::alloy::u256_to_py(py, &out)?;
        Ok(bound.unbind())
    }

    /// Calculate the required input token amount for a given output amount.
    #[pyo3(signature = (zero_for_one, amount_out))]
    fn calculate_tokens_in(
        &self,
        py: Python<'_>,
        zero_for_one: bool,
        amount_out: &Bound<'_, PyAny>,
    ) -> PyResult<Py<PyAny>> {
        let amount = crate::conversion::alloy::extract_python_u256(amount_out)?;
        let result = {
            let core = self.core.read();
            core.calculate_tokens_in(self.pool_id, zero_for_one, amount)
        };
        let bound = crate::conversion::alloy::u256_to_py(py, &result)?;
        Ok(bound.unbind())
    }

    /// Fetch+retry exact-input swap for sparse V3/V4 pools (ADR-005 slice 3).
    ///
    /// Like [`calculate_tokens_out`][Self::calculate_tokens_out], but on a
    /// sparse tick-map miss it calls `fetcher(word, block)` to fetch the
    /// missing word's tick data, merges it, and retries (dedup-protected — a
    /// repeated miss on a word gives up with `0`). Mirrors the Python
    /// companion's `MissingLiquidityData` → `_tick_data_fetcher` loop, now
    /// driven from the Rust calc path.
    ///
    /// `fetcher` is a Python callable
    /// `fetcher(word: int, block: int) -> dict[int, tuple[int, int, int]] | None`.
    /// It MUST return the fetched tick data mapping tick →
    /// `(liquidity_gross, liquidity_net, block)`; returning `None` or `{}` marks
    /// the word known with no initialized ticks (an all-zero bitmap word).
    ///
    /// The fetcher MUST NOT write back into the pool via `update_tick_data`
    /// (the Rust loop merges the returned data itself) — doing so would re-enter
    /// the `BotState` write lock this call holds and deadlock.
    #[pyo3(signature = (zero_for_one, amount_in, block))]
    fn calculate_tokens_out_with_fetch(
        &self,
        py: Python<'_>,
        zero_for_one: bool,
        amount_in: &Bound<'_, PyAny>,
        block: u64,
    ) -> PyResult<Py<PyAny>> {
        let amount = crate::conversion::alloy::extract_python_u256(amount_in)?;
        let result = {
            let mut core = self.core.write();
            core.calculate_tokens_out_with_fetch(self.pool_id, zero_for_one, amount, block)
        };
        let bound = crate::conversion::alloy::u256_to_py(py, &result)?;
        Ok(bound.unbind())
    }

    /// Fetch+retry full-outcome exact-input swap for sparse V3/V4 pools
    /// (ADR-005 slice 3b). Like [`calculate_tokens_out_with_fetch`][Self::calculate_tokens_out_with_fetch]
    /// but returns the full swap outcome tuple so the companion can build
    /// `final_state`.
    ///
    /// Returns `(amount0, amount1, sqrt_price_x96, liquidity, tick)` or `None`
    /// (pool not V3/V4, zero amount, fetch failed, or not computable).
    #[pyo3(signature = (zero_for_one, amount_in, block, sqrt_price_limit_x96=None))]
    fn simulate_swap_with_fetch(
        &self,
        py: Python<'_>,
        zero_for_one: bool,
        amount_in: &Bound<'_, PyAny>,
        block: u64,
        sqrt_price_limit_x96: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Option<Py<PyAny>>> {
        let amount = crate::conversion::alloy::extract_python_u256(amount_in)?;
        let sqrt_price_limit = match sqrt_price_limit_x96 {
            Some(v) if !v.is_none() => crate::conversion::alloy::extract_python_u256(v)?,
            _ => degenbot_bot::bot_core::V3PoolState::default_sqrt_price_limit(zero_for_one),
        };
        let outcome = {
            let mut core = self.core.write();
            core.simulate_exact_input_swap_with_fetch(
                self.pool_id,
                zero_for_one,
                amount,
                sqrt_price_limit,
                block,
            )
        };
        let Some(outcome) = outcome else {
            return Ok(None);
        };
        let tuple = pyo3::types::PyTuple::new(
            py,
            [
                crate::conversion::alloy::u256_to_py(py, &outcome.amount0)?.unbind(),
                crate::conversion::alloy::u256_to_py(py, &outcome.amount1)?.unbind(),
                crate::conversion::alloy::u256_to_py(py, &outcome.sqrt_price_x96)?.unbind(),
                outcome.liquidity.into_pyobject(py)?.into_any().unbind(),
                outcome.tick.into_pyobject(py)?.into_any().unbind(),
            ],
        )?;
        Ok(Some(tuple.into_any().unbind()))
    }

    /// Exact-OUTPUT fetch+retry swap: caller passes the desired `amount_out`,
    /// the sim derives the required input. Same 5-tuple return shape as
    /// `simulate_swap_with_fetch` (the caller extracts the opposing amount as
    /// the required input). Mirrors the Python `calculate_tokens_in_from_
    /// tokens_out` / `simulate_exact_output_swap` frozen path; the V3/V4
    /// exact-output sign convention is handled in the core.
    #[pyo3(signature = (zero_for_one, amount_out, block, sqrt_price_limit_x96=None))]
    fn simulate_exact_output_swap_with_fetch(
        &self,
        py: Python<'_>,
        zero_for_one: bool,
        amount_out: &Bound<'_, PyAny>,
        block: u64,
        sqrt_price_limit_x96: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Option<Py<PyAny>>> {
        let amount = crate::conversion::alloy::extract_python_u256(amount_out)?;
        let sqrt_price_limit = match sqrt_price_limit_x96 {
            Some(v) if !v.is_none() => crate::conversion::alloy::extract_python_u256(v)?,
            _ => degenbot_bot::bot_core::V3PoolState::default_sqrt_price_limit(zero_for_one),
        };
        let outcome = {
            let mut core = self.core.write();
            core.simulate_exact_output_swap_with_fetch(
                self.pool_id,
                zero_for_one,
                amount,
                sqrt_price_limit,
                block,
            )
        };
        let Some(outcome) = outcome else {
            return Ok(None);
        };
        let tuple = pyo3::types::PyTuple::new(
            py,
            [
                crate::conversion::alloy::u256_to_py(py, &outcome.amount0)?.unbind(),
                crate::conversion::alloy::u256_to_py(py, &outcome.amount1)?.unbind(),
                crate::conversion::alloy::u256_to_py(py, &outcome.sqrt_price_x96)?.unbind(),
                outcome.liquidity.into_pyobject(py)?.into_any().unbind(),
                outcome.tick.into_pyobject(py)?.into_any().unbind(),
            ],
        )?;
        Ok(Some(tuple.into_any().unbind()))
    }

    /// Simulate an exact-input swap over a HYPOTHETICAL override pool state,
    /// with fetch+retry for sparse misses.
    ///
    /// Builds a transient V3/V4 state from `override_sqrt_price_x96`,
    /// `override_liquidity`, `override_tick` + `override_tick_data`
    /// (`{tick: (liquidity_gross, liquidity_net, block)}`, same shape as
    /// `register_*_pool`'s `tick_data`), reusing the registered pool's fee /
    /// `tick_spacing`. On a sparse miss, the fetcher is called + the word's
    /// ticks are merged into the TRANSIENT state (NOT registered `BotState`),
    /// and the sim retries. Returns the same 5-tuple as
    /// `simulate_swap_with_fetch`, or `None` if not computable.
    #[pyo3(signature = (zero_for_one, amount_in, block, override_sqrt_price_x96, override_liquidity, override_tick, override_tick_data, sqrt_price_limit_x96=None))]
    #[allow(clippy::too_many_arguments)]
    fn simulate_swap_with_override(
        &self,
        py: Python<'_>,
        zero_for_one: bool,
        amount_in: &Bound<'_, PyAny>,
        block: u64,
        override_sqrt_price_x96: &Bound<'_, PyAny>,
        override_liquidity: &Bound<'_, PyAny>,
        override_tick: &Bound<'_, PyAny>,
        override_tick_data: &Bound<'_, PyAny>,
        sqrt_price_limit_x96: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Option<Py<PyAny>>> {
        let amount = crate::conversion::alloy::extract_python_u256(amount_in)?;
        let outcome = self.sim_override_inner(
            zero_for_one,
            amount,
            false, // exact_input
            block,
            override_sqrt_price_x96,
            override_liquidity,
            override_tick,
            override_tick_data,
            sqrt_price_limit_x96,
        )?;
        outcome.map_or(Ok(None), |o| build_swap_outcome_tuple(py, &o))
    }

    /// Exact-OUTPUT swap over a HYPOTHETICAL override pool state, with
    /// fetch+retry for sparse misses. Caller passes the desired `amount_out`,
    /// the sim derives the required input. Same 5-tuple return shape as
    /// `simulate_swap_with_override`. Combines the override-state build with
    /// the V3/V4 exact-output sign convention (handled in the core).
    #[pyo3(signature = (zero_for_one, amount_out, block, override_sqrt_price_x96, override_liquidity, override_tick, override_tick_data, sqrt_price_limit_x96=None))]
    #[allow(clippy::too_many_arguments)]
    fn simulate_exact_output_swap_with_override(
        &self,
        py: Python<'_>,
        zero_for_one: bool,
        amount_out: &Bound<'_, PyAny>,
        block: u64,
        override_sqrt_price_x96: &Bound<'_, PyAny>,
        override_liquidity: &Bound<'_, PyAny>,
        override_tick: &Bound<'_, PyAny>,
        override_tick_data: &Bound<'_, PyAny>,
        sqrt_price_limit_x96: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Option<Py<PyAny>>> {
        let amount = crate::conversion::alloy::extract_python_u256(amount_out)?;
        let outcome = self.sim_override_inner(
            zero_for_one,
            amount,
            true, // exact_output
            block,
            override_sqrt_price_x96,
            override_liquidity,
            override_tick,
            override_tick_data,
            sqrt_price_limit_x96,
        )?;
        outcome.map_or(Ok(None), |o| build_swap_outcome_tuple(py, &o))
    }

    #[pyo3(signature = (zero_for_one, amount_out, recipient))]
    fn encode_swap(
        &self,
        zero_for_one: bool,
        amount_out: &Bound<'_, PyAny>,
        recipient: &str,
    ) -> PyResult<Option<(String, String, u64)>> {
        let amount = crate::conversion::alloy::extract_python_u256(amount_out)?;
        let recip = match recipient.parse() {
            Ok(addr) => addr,
            Err(e) => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "Invalid address '{recipient}': {e}"
                )));
            }
        };

        let result = {
            let core = self.core.read();
            core.encode_swap(self.pool_id, zero_for_one, amount, recip)
        };

        Ok(result.map(|call| {
            let to_hex = format!("{:#x}", call.to);
            let data_hex = format!("0x{}", bytes_to_hex(&call.data));
            (to_hex, data_hex, call.value.to::<u64>())
        }))
    }

    // --- State read getters (ADR-005 slice 4 step 2) ---
    // These read the shared `BotState` under a read guard. Immutable identity
    // (token0/token1/factory/fees/address) stays on the Python companion —
    // only mutable state + the reorg journal delegate to Rust.

    /// Current reserve of token0.
    #[getter]
    fn reserve0(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let r = {
            let core = self.core.read();
            core.get_v2_pool_state(self.pool_id)
                .map(|s| s.reserve0.to::<U256>())
                .unwrap_or_default()
        };
        Ok(crate::conversion::alloy::u256_to_py(py, &r)?.unbind())
    }

    /// Current reserve of token1.
    #[getter]
    fn reserve1(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let r = {
            let core = self.core.read();
            core.get_v2_pool_state(self.pool_id)
                .map(|s| s.reserve1.to::<U256>())
                .unwrap_or_default()
        };
        Ok(crate::conversion::alloy::u256_to_py(py, &r)?.unbind())
    }

    /// Block number of the most recent state update. Falls through V2→V3→V4
    /// so the same Python companion property works for all families.
    #[getter]
    fn update_block(&self) -> u64 {
        let core = self.core.read();
        if let Some(s) = core.get_v2_pool_state(self.pool_id) {
            return s.update_block;
        }
        // J63J3N: V3 *or* V4 (previously V3-only via get_v3_pool, which
        // returned None for V4 and fell through to 0).
        if let Some(s) = core.get_v3_or_v4_pool(self.pool_id) {
            return s.update_block();
        }
        // Curve: the ADR-005 slice 11a state port. Mirrors V2/V3/V4 — the
        // mutable update_block slot lives in Rust now.
        if let Some(s) = core.get_curve_pool(self.pool_id) {
            return s.update_block;
        }
        // Balancer weighted: the ADR-005 slice 12a state port. Same
        // family-falling-through discipline.
        if let Some(s) = core.get_balancer_weighted_pool(self.pool_id) {
            return s.update_block;
        }
        // Balancer stable: the ADR-005 slice 12c state port. Same
        // family-falling-through discipline.
        if let Some(s) = core.get_balancer_stable_pool(self.pool_id) {
            return s.update_block;
        }
        0
    }

    /// The pool's **liquidity** clock (`tick_data_block`, two-stamp OB7UNY) —
    /// the block its tick map reflects. CL (V3/V4) only; the families without
    /// a tick-data clock (V2/Curve/Balancer) and unregistered ids return `0`.
    /// Mirrors the [`Self::update_block`] getter's family-falling-through
    /// discipline. A CL pool whose `tick_data_block` is well below its
    /// `update_block` is the staged-clock desync class (`0x5653`: fresh price,
    /// stale liquidity map) — expose both clocks so a Python driver can tell
    /// them apart.
    #[getter]
    fn tick_data_block(&self) -> u64 {
        let core = self.core.read();
        if let Some(s) = core.get_v3_or_v4_pool(self.pool_id) {
            return s.tick_data_block();
        }
        0
    }

    /// Atomic snapshot of (reserve0, reserve1, `update_block`) under one read guard.
    ///
    /// The companion's `state` property + `simulate_*` methods build their
    /// state object from this single snapshot so a Rust-side `sync_reserves`
    /// (pump update) can't interleave between separate `reserve0`/`reserve1`
    /// reads (replaces the `StateCache.lock()` atomicity the drop-`StateCache`
    /// refactor loses). Returns `None` if the pool isn't registered or isn't a
    /// V2 pool.
    #[pyo3(signature = ())]
    fn snapshot(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let snap = { self.core.read().v2_snapshot(self.pool_id) };
        match snap {
            None => Ok(None),
            Some((r0, r1, blk)) => {
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

    // --- V2 identity getters (ADR-005 identity slice) ---
    // Immutable per-pool identity read from V2PoolState (+ the registration
    // descriptor) so the Python companion's `_from_py_pool(py_pool)` can be
    // self-describing — the Polars `_from_pydf` end state. These re-export
    // what V2PoolState already holds; the descriptor (variant/stable_swap/
    // fee_denominator) + resolved DexIdentity preset are new in this slice.

    /// Pool contract address (EIP-55 checksummed hex). Empty string if not a
    /// V2/V3 pool.
    #[getter]
    fn address(&self) -> String {
        let core = self.core.read();
        let addr = match core.pool_family(self.pool_id) {
            "v2" => core.get_v2_identity(self.pool_id).map(|i| i.address),
            "v3" => core.get_v3_identity(self.pool_id).map(|i| i.address),
            "aerodrome-v2" => core.get_aerodrome_identity(self.pool_id).map(|i| i.address),
            "curve" => core.get_curve_identity(self.pool_id).map(|i| i.address),
            _ => None,
        };
        addr.map(|a| address_utils::address_to_checksum_string(&a))
            .unwrap_or_default()
    }

    /// Token0 contract address (EIP-55 checksummed hex). Empty string if not
    /// a V2/V3/V4 pool.
    #[getter]
    fn token0_address(&self) -> String {
        let core = self.core.read();
        let addr = match core.pool_family(self.pool_id) {
            "v2" => core.get_v2_identity(self.pool_id).map(|i| i.token0),
            "v3" => core.get_v3_identity(self.pool_id).map(|i| i.token0),
            "v4" => core
                .get_v4_identity(self.pool_id)
                .map(|i| i.pool_key.currency0),
            "aerodrome-v2" => core.get_aerodrome_identity(self.pool_id).map(|i| i.token0),
            _ => None,
        };
        addr.map(|a| address_utils::address_to_checksum_string(&a))
            .unwrap_or_default()
    }

    /// Token1 contract address (EIP-55 checksummed hex). Empty string if not
    /// a V2/V3/V4 pool.
    #[getter]
    fn token1_address(&self) -> String {
        let core = self.core.read();
        let addr = match core.pool_family(self.pool_id) {
            "v2" => core.get_v2_identity(self.pool_id).map(|i| i.token1),
            "v3" => core.get_v3_identity(self.pool_id).map(|i| i.token1),
            "v4" => core
                .get_v4_identity(self.pool_id)
                .map(|i| i.pool_key.currency1),
            "aerodrome-v2" => core.get_aerodrome_identity(self.pool_id).map(|i| i.token1),
            _ => None,
        };
        addr.map(|a| address_utils::address_to_checksum_string(&a))
            .unwrap_or_default()
    }

    /// Factory contract address (EIP-55 checksummed hex). Empty string if not a
    /// V2/V3 pool.
    #[getter]
    fn factory(&self) -> String {
        let core = self.core.read();
        let addr = match core.pool_family(self.pool_id) {
            "v2" => core.get_v2_identity(self.pool_id).map(|i| i.factory),
            "v3" => core.get_v3_identity(self.pool_id).map(|i| i.factory),
            "aerodrome-v2" => core.get_aerodrome_identity(self.pool_id).map(|i| i.factory),
            _ => None,
        };
        addr.map(|a| address_utils::address_to_checksum_string(&a))
            .unwrap_or_default()
    }

    /// The CREATE2 deployer this pool's address was verified against (Fork A).
    /// The JSON row's `deployer` (or the factory for ``null``), resolved at
    /// registration. V2 and V3 pools carry this; other pool families return an
    /// empty string.
    #[getter]
    fn deployer(&self) -> String {
        let core = self.core.read();
        let addr = match core.pool_family(self.pool_id) {
            "v2" => core.get_v2_identity(self.pool_id).map(|i| i.deployer),
            "v3" => core.get_v3_identity(self.pool_id).map(|i| i.deployer),
            _ => None,
        };
        addr.map(|a| address_utils::address_to_checksum_string(&a))
            .unwrap_or_default()
    }

    /// The CREATE2 init code hash this pool's address was verified against
    /// (Fork A). The JSON row's `init_hash` when shipped, else the Uniswap
    /// mainnet fallback (V2 or V3 const). V2 and V3 pools carry this; other
    /// pool families return an empty string.
    #[getter]
    fn init_hash(&self) -> String {
        let core = self.core.read();
        let h = match core.pool_family(self.pool_id) {
            "v2" => core.get_v2_identity(self.pool_id).map(|i| i.init_hash),
            "v3" => core.get_v3_identity(self.pool_id).map(|i| i.init_hash),
            _ => None,
        };
        h.map(|b| format!("{b:#x}")).unwrap_or_default()
    }

    /// `token0→token1` fee parameters: `(gamma_numer, fee_denom)` — the
    /// retained post-fee fraction (e.g. `(997, 1000)` for 0.3%). `(0, 0)` if
    /// not a V2 pool.
    #[getter]
    fn fee_token0(&self) -> (u64, u64) {
        let core = self.core.read();
        core.get_v2_identity(self.pool_id)
            .map(|s| s.fee_token0)
            .unwrap_or_default()
    }

    /// `token1→token0` fee parameters: `(gamma_numer, fee_denom)`. `(0, 0)` if
    /// not a V2 pool.
    #[getter]
    fn fee_token1(&self) -> (u64, u64) {
        let core = self.core.read();
        core.get_v2_identity(self.pool_id)
            .map(|s| s.fee_token1)
            .unwrap_or_default()
    }

    /// The DEX+variant discriminator as a kebab-case string (e.g.
    /// `"uniswap-v2"`, `"camelot-v2-stable"`). Empty string if not a V2 pool.
    #[getter]
    fn variant(&self) -> String {
        let core = self.core.read();
        match core.pool_family(self.pool_id) {
            "v2" => core
                .get_v2_identity(self.pool_id)
                .map(|d| d.variant.as_str().to_string())
                .unwrap_or_default(),
            "aerodrome-v2" => core
                .get_aerodrome_identity(self.pool_id)
                .map(|d| d.variant.as_str().to_string())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// Camelot solidly-stable strategy flag. `false` for all non-Camelot V2
    /// and for non-V2 `pool_ids`.
    #[getter]
    fn stable_swap(&self) -> bool {
        let core = self.core.read();
        core.get_v2_identity(self.pool_id)
            .is_some_and(|d| d.stable_swap)
    }

    /// The pool-family tag for this handle's registered pool (`"v2"`,
    /// `"v3"`, `"v4"`, `"curve"`, `"balancer-weighted"`,
    /// `"balancer-stable"`). Empty string if unregistered.
    ///
    /// This is the uniform family-guard primitive every `_from_py_pool`
    /// seam asserts against — dispatches on the `PoolEntry` variant
    /// directly, so it is correct for every registered family (unlike
    /// `variant`, which is V2-only and returns `""` for non-V2).
    #[getter]
    fn pool_family(&self) -> String {
        let core = self.core.read();
        core.pool_family(self.pool_id).to_string()
    }

    /// Camelot integer fee scaling. `None` for non-Camelot V2 / non-V2.
    #[getter]
    fn fee_denominator(&self) -> Option<u64> {
        let core = self.core.read();
        core.get_v2_identity(self.pool_id)
            .and_then(|d| d.fee_denominator)
    }

    /// The resolved `DexIdentity` for this pool's registered variant, with the
    /// JSON-sourced deployer + `init_hash` merged in (Fork A, NSAZ4X). `None` if
    /// not a V2 pool. The Python companion reads this to recover deployer /
    /// init-hash without taking constructor args. Protocol-const fields
    /// (fees/ABI shape) come from the variant preset; `factory`/`deployer` /
    /// `init_hash` come from the identity (the verified values stored at
    /// registration).
    #[getter]
    fn dex(&self) -> Option<crate::bot::dex_identity::PyDexIdentity> {
        let core = self.core.read();
        let id = core.get_v2_identity(self.pool_id)?;
        let mut ident = degenbot_uniswap::dex_identity::preset_for_variant(id.variant);
        ident.factory = id.factory;
        ident.deployer = id.deployer;
        ident.init_hash = id.init_hash;
        Some(crate::bot::dex_identity::PyDexIdentity::from_core(&ident))
    }

    // --- V2 token-recovery getters (ADR-005 identity slice, task EO2SLK) ---
    // Recover `PyErc20Token` handles for the pool's token0/token1 from the
    // SAME shared BotState (ADR-006: one Bot per chain owns all assets). The
    // companion wraps these via `Erc20Token._from_py_token` so the
    // `_from_py_pool(py_pool)` seam needs no token args — the Polars
    // `_from_pydf` end state.
    //
    // Returns `None` if the pool isn't V2 or the token address isn't
    // registered in the shared BotState.tokens registry (the failure mode the
    // test-factory cross-Bot-token fix addresses — production always registers).

    /// `PyErc20Token` handle for token0, or `None` if not registered in the
    /// shared `BotState`.
    fn get_token0(&self) -> Option<PyErc20Token> {
        let core = self.core.read();
        let token_addr = match core.pool_family(self.pool_id) {
            "v2" => core.get_v2_identity(self.pool_id).map(|i| i.token0),
            "v3" => core.get_v3_identity(self.pool_id).map(|i| i.token0),
            "v4" => core
                .get_v4_identity(self.pool_id)
                .map(|i| i.pool_key.currency0),
            "aerodrome-v2" => core.get_aerodrome_identity(self.pool_id).map(|i| i.token0),
            _ => None,
        }?;
        if core.has_token(&token_addr) {
            Some(PyErc20Token::new(Arc::clone(&self.core), token_addr))
        } else {
            None
        }
    }

    /// `PyErc20Token` handle for token1, or `None` if not registered in the
    /// shared `BotState`.
    fn get_token1(&self) -> Option<PyErc20Token> {
        let core = self.core.read();
        let token_addr = match core.pool_family(self.pool_id) {
            "v2" => core.get_v2_identity(self.pool_id).map(|i| i.token1),
            "v3" => core.get_v3_identity(self.pool_id).map(|i| i.token1),
            "v4" => core
                .get_v4_identity(self.pool_id)
                .map(|i| i.pool_key.currency1),
            "aerodrome-v2" => core.get_aerodrome_identity(self.pool_id).map(|i| i.token1),
            _ => None,
        }?;
        if core.has_token(&token_addr) {
            Some(PyErc20Token::new(Arc::clone(&self.core), token_addr))
        } else {
            None
        }
    }

    fn get_balancer_tokens(&self) -> Option<Vec<PyErc20Token>> {
        let core = self.core.read();
        let identity = core.get_balancer_weighted_identity(self.pool_id)?;
        let mut out = Vec::with_capacity(identity.tokens.len());
        for token_addr in &identity.tokens {
            if !core.has_token(token_addr) {
                return None;
            }
            out.push(PyErc20Token::new(Arc::clone(&self.core), *token_addr));
        }
        Some(out)
    }

    // --- V3 state read getters (plan-101 slice 8a) ---
    // Mirror the V2 family but read the V3PoolState entry. All getters take
    // one read guard and return None-defaulted values when the pool_id is not
    // a registered V3 pool (matching the V2 getters' behavior on V2).

    /// Current `sqrt_price_x96` (Q64.96) for a V3/V4 pool. 0 if not V3/V4.
    #[getter]
    fn sqrt_price_x96(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let spx = {
            let core = self.core.read();
            core.get_v3_or_v4_pool(self.pool_id)
                .map(degenbot_bot::bot_core::ConcentratedLiquidityPool::sqrt_price_x96)
                .unwrap_or_default()
        };
        Ok(crate::conversion::alloy::u256_to_py(py, &spx)?.unbind())
    }

    /// Current active liquidity for a V3/V4 pool. 0 if not V3/V4.
    #[getter]
    fn liquidity(&self) -> u128 {
        self.core
            .read()
            .get_v3_or_v4_pool(self.pool_id)
            .map(degenbot_bot::bot_core::ConcentratedLiquidityPool::liquidity)
            .unwrap_or_default()
    }

    /// Current tick for a V3/V4 pool. 0 if not V3/V4.
    #[getter]
    fn tick(&self) -> i32 {
        self.core
            .read()
            .get_v3_or_v4_pool(self.pool_id)
            .map(degenbot_bot::bot_core::ConcentratedLiquidityPool::tick)
            .unwrap_or_default()
    }

    /// Pool fee (immutable) for a V3/V4 pool. 0 if not V3/V4.
    #[getter]
    fn fee(&self) -> u32 {
        let core = self.core.read();
        core.get_v3_identity(self.pool_id)
            .map(|i| i.fee)
            .or_else(|| core.get_v4_identity(self.pool_id).map(|i| i.pool_key.fee))
            .unwrap_or_default()
    }

    /// Tick spacing (immutable) for a V3/V4 pool. 0 if not V3/V4.
    #[getter]
    fn tick_spacing(&self) -> i32 {
        let core = self.core.read();
        core.get_v3_identity(self.pool_id)
            .map(|i| i.tick_spacing)
            .or_else(|| {
                core.get_v4_identity(self.pool_id)
                    .map(|i| i.pool_key.tick_spacing)
            })
            .unwrap_or_default()
    }

    // --- V4 identity getters (ADR-005 sealed seam) ---
    // Read off V4PoolIdentity so UniswapV4Pool._from_py_pool is self-describing.

    /// Pool manager contract address (EIP-55 checksummed hex). Empty string if
    /// not a V4 pool.
    #[getter]
    fn pool_manager_address(&self) -> String {
        let core = self.core.read();
        core.get_v4_identity(self.pool_id)
            .map(|i| address_utils::address_to_checksum_string(&i.pool_manager))
            .unwrap_or_default()
    }

    /// On-chain V4 pool ID (32-byte hex, ``0x``-prefixed). Empty string if not
    /// a V4 pool.
    #[getter]
    fn pool_id_hex(&self) -> String {
        let core = self.core.read();
        match core.get_v4_identity(self.pool_id) {
            Some(i) => format!("0x{}", bytes_to_hex(&i.pool_id)),
            None => String::new(),
        }
    }

    /// Hook contract address (EIP-55 checksummed hex). Empty string if not a
    /// V4 pool.
    #[getter]
    fn hook_address(&self) -> String {
        let core = self.core.read();
        core.get_v4_identity(self.pool_id)
            .map(|i| address_utils::address_to_checksum_string(&i.pool_key.hooks))
            .unwrap_or_default()
    }

    // --- Aerodrome V2 identity getters (ADR-005 Aerodrome state port) ---

    /// Aerodrome V2 Solidly stable-invariant flag. `false` for volatile mode
    /// or non-Aerodrome pools.
    #[getter]
    fn aerodrome_stable(&self) -> bool {
        let core = self.core.read();
        core.get_aerodrome_identity(self.pool_id)
            .is_some_and(|d| d.stable)
    }

    /// Aerodrome V2 unidirectional fee as `(fee_numer, fee_denom)`. `(0, 0)` if
    /// not an Aerodrome pool.
    #[getter]
    fn aerodrome_fee(&self) -> (u64, u64) {
        let core = self.core.read();
        core.get_aerodrome_identity(self.pool_id)
            .map(|d| d.fee)
            .unwrap_or_default()
    }

    /// Aerodrome V2 reserve of token0. Read via the
    /// `AerodromeV2PoolState` entry (one read guard). 0 if not an Aerodrome
    /// pool.
    #[getter]
    fn aerodrome_reserve0(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let v = {
            let core = self.core.read();
            core.get_aerodrome_pool(self.pool_id)
                .map(|s| s.reserve0.to::<U256>())
                .unwrap_or_default()
        };
        Ok(crate::conversion::alloy::u256_to_py(py, &v)?.unbind())
    }

    /// Aerodrome V2 reserve of token1. 0 if not an Aerodrome pool.
    #[getter]
    fn aerodrome_reserve1(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let v = {
            let core = self.core.read();
            core.get_aerodrome_pool(self.pool_id)
                .map(|s| s.reserve1.to::<U256>())
                .unwrap_or_default()
        };
        Ok(crate::conversion::alloy::u256_to_py(py, &v)?.unbind())
    }

    /// Snapshot an Aerodrome V2 pool's mutable state as
    /// `(reserve0, reserve1, update_block)`. Returns `None` for non-Aerodrome
    /// pools. Reserves are returned as `U256` (matching `snapshot` for V2):
    /// Aerodrome reserves commonly exceed `u64::MAX` in raw wei (e.g. `5_000`
    /// WETH = 5e21), so the prior `to::<u64>()` conversion panicked on
    /// overflow.
    fn snapshot_aerodrome(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let snap = self.core.read().get_aerodrome_pool(self.pool_id).map(|s| {
            (
                s.reserve0.to::<U256>(),
                s.reserve1.to::<U256>(),
                s.update_block,
            )
        });
        match snap {
            None => Ok(None),
            Some((reserve0, reserve1, block)) => {
                let tuple = pyo3::types::PyTuple::new(
                    py,
                    [
                        crate::conversion::alloy::u256_to_py(py, &reserve0)?.unbind(),
                        crate::conversion::alloy::u256_to_py(py, &reserve1)?.unbind(),
                        block.into_pyobject(py)?.into_any().unbind(),
                    ],
                )?;
                Ok(Some(tuple.into_any().unbind()))
            }
        }
    }

    /// Apply an Aerodrome V2 `Sync` event: journals the prior reserves, then
    /// lands the new reserves + `block_number`. Equivalent to
    /// `PyBot.update_aerodrome_pool(...)` but keyed by the handle's `pool_id`.
    #[pyo3(signature = (reserve0, reserve1, block_number))]
    fn apply_aerodrome_sync(
        &self,
        reserve0: &Bound<'_, PyAny>,
        reserve1: &Bound<'_, PyAny>,
        block_number: u64,
    ) -> PyResult<()> {
        let r0 = degenbot_pools::spec_bounds::narrow_v2_reserve(
            crate::conversion::alloy::extract_python_u256(reserve0)?,
            "reserve0",
        )
        .map_err(|sv| crate::bot::engine::SpecViolationError::new_err(format!("{sv}")))?;
        let r1 = degenbot_pools::spec_bounds::narrow_v2_reserve(
            crate::conversion::alloy::extract_python_u256(reserve1)?,
            "reserve1",
        )
        .map_err(|sv| crate::bot::engine::SpecViolationError::new_err(format!("{sv}")))?;
        let _ = self
            .core
            .write()
            .apply_sync_by_pool_id(self.pool_id, r0, r1, block_number);
        Ok(())
    }

    // --- Balancer weighted identity getters (ADR-005 sealed seam) ---

    /// Balancer V2 pool contract address (EIP-55 checksummed hex).
    /// Empty string if not a Balancer weighted pool.
    #[getter]
    fn balancer_address(&self) -> String {
        let core = self.core.read();
        core.get_balancer_weighted_identity(self.pool_id)
            .map(|i| address_utils::address_to_checksum_string(&i.address))
            .unwrap_or_default()
    }

    /// Balancer V2 vault contract address (EIP-55 checksummed hex).
    /// Empty string if not a Balancer weighted pool.
    #[getter]
    fn balancer_vault(&self) -> String {
        let core = self.core.read();
        core.get_balancer_weighted_identity(self.pool_id)
            .map(|i| address_utils::address_to_checksum_string(&i.vault))
            .unwrap_or_default()
    }

    /// Balancer V2 pool ID (32-byte hex, ``0x``-prefixed). Empty string if
    /// not a Balancer weighted pool.
    #[getter]
    fn balancer_pool_id_hex(&self) -> String {
        let core = self.core.read();
        match core.get_balancer_weighted_identity(self.pool_id) {
            Some(i) => format!("0x{}", bytes_to_hex(&i.pool_id)),
            None => String::new(),
        }
    }

    /// Balancer weighted token addresses (EIP-55 checksummed hex). Empty list
    /// if not a Balancer weighted pool.
    #[getter]
    fn balancer_token_addresses(&self) -> Vec<String> {
        let core = self.core.read();
        match core.get_balancer_weighted_identity(self.pool_id) {
            Some(i) => i
                .tokens
                .iter()
                .map(address_utils::address_to_checksum_string)
                .collect(),
            None => Vec::new(),
        }
    }

    /// Balancer weighted denormalized weights (one `U256` per token).
    /// Empty list if not a Balancer weighted pool.
    #[getter]
    fn balancer_weights(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let weights: Vec<alloy::primitives::U256> = self
            .core
            .read()
            .get_balancer_weighted_identity(self.pool_id)
            .map(|i| i.weights.clone())
            .unwrap_or_default();
        let py_w: Vec<Py<PyAny>> = weights
            .iter()
            .map(|w| crate::conversion::alloy::u256_to_py(py, w).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        Ok(pyo3::types::PyList::new(py, py_w)?.into_any().unbind())
    }

    /// Balancer weighted scaling factors (one `U256` per token). Empty list
    /// if not a Balancer weighted pool.
    #[getter]
    fn balancer_scaling_factors(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let sf: Vec<alloy::primitives::U256> = self
            .core
            .read()
            .get_balancer_weighted_identity(self.pool_id)
            .map(|i| i.scaling_factors.clone())
            .unwrap_or_default();
        let py_sf: Vec<Py<PyAny>> = sf
            .iter()
            .map(|s| crate::conversion::alloy::u256_to_py(py, s).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        Ok(pyo3::types::PyList::new(py, py_sf)?.into_any().unbind())
    }

    /// Balancer weighted swap fee (fixed-point, 1e18 scale). 0 if not a
    /// Balancer weighted pool.
    #[getter]
    fn balancer_swap_fee(&self) -> u128 {
        self.core
            .read()
            .get_balancer_weighted_identity(self.pool_id)
            .map(|i| i.swap_fee)
            .unwrap_or_default()
    }

    /// Balancer weighted Math pool implementation version (1 or 2). 0 if not
    /// a Balancer weighted pool.
    #[getter]
    fn balancer_pow_version(&self) -> u8 {
        self.core
            .read()
            .get_balancer_weighted_identity(self.pool_id)
            .map(|i| i.pow_version)
            .unwrap_or_default()
    }

    // --- Mutations (per-handle, pool_id-keyed) ---

    /// Apply a V2 `Sync` event: journals the prior reserves then lands the new.
    /// Equivalent to `PyBot.update_v2_pool(address, ...)` but keyed by the
    /// handle's `pool_id` (no address resolution, single lock).
    #[pyo3(signature = (reserve0, reserve1, block_number))]
    fn sync_reserves(
        &self,
        reserve0: &Bound<'_, PyAny>,
        reserve1: &Bound<'_, PyAny>,
        block_number: u64,
    ) -> PyResult<()> {
        let r0 = degenbot_pools::spec_bounds::narrow_v2_reserve(
            crate::conversion::alloy::extract_python_u256(reserve0)?,
            "reserve0",
        )
        .map_err(|sv| crate::bot::engine::SpecViolationError::new_err(format!("{sv}")))?;
        let r1 = degenbot_pools::spec_bounds::narrow_v2_reserve(
            crate::conversion::alloy::extract_python_u256(reserve1)?,
            "reserve1",
        )
        .map_err(|sv| crate::bot::engine::SpecViolationError::new_err(format!("{sv}")))?;
        let _ = self
            .core
            .write()
            .apply_sync_by_pool_id(self.pool_id, r0, r1, block_number);
        Ok(())
    }

    /// Number of deltas in the V2 reorg journal (genesis + transitions).
    fn journal_len(&self) -> usize {
        let core = self.core.read();
        if core.get_v2_pool_state(self.pool_id).is_none() {
            0
        } else {
            core.pool_journal_len(self.pool_id).unwrap_or(0)
        }
    }

    /// Discard V2 reorg journal deltas earlier than `block`.
    ///
    /// Raises:
    ///     `ValueError`: If the target is past the newest delta (would remove
    ///         every known state).
    #[pyo3(signature = (block))]
    fn discard_before_block(&self, block: u64) -> PyResult<()> {
        let mut core = self.core.write();
        if core.get_v2_pool_state(self.pool_id).is_none() {
            return Ok(());
        }
        core.discard_pool_before_block(self.pool_id, block)
            .unwrap_or(Ok(()))
            .map_err(journal_err_to_py)
    }

    /// Restore the V2 pool to the landed-at state just before `block`.
    ///
    /// Returns `(reserve0, reserve1, block)` as Python ints, or `None` if the
    /// pool ID is not registered.
    ///
    /// Raises:
    ///     `ValueError`: If `block` is at or before the registration block.
    #[pyo3(signature = (block))]
    fn restore_before_block(&self, py: Python<'_>, block: u64) -> PyResult<Option<Py<PyAny>>> {
        // Read-after-restore (ADR-016): post-restore reserves == the
        // before-values the per-family tuple previously carried.
        let (r0, r1, blk) = {
            let mut core = self.core.write();
            if core.get_v2_pool_state(self.pool_id).is_none() {
                return Ok(None);
            }
            match core.restore_pool_before_block(self.pool_id, block) {
                None => return Ok(None),
                Some(Err(e)) => return Err(journal_err_to_py(e)),
                Some(Ok(())) => {
                    let state = core
                        .get_v2_pool_state(self.pool_id)
                        .expect("V2 pool confirmed above");
                    (
                        state.reserve0.to::<alloy::primitives::U256>(),
                        state.reserve1.to::<alloy::primitives::U256>(),
                        state.update_block,
                    )
                }
            }
        };
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

    // --- Aerodrome V2 reorg journal ---

    /// Discard Aerodrome reorg journal deltas earlier than `block`.
    ///
    /// Raises:
    ///     `ValueError`: If the target is past the newest delta.
    #[pyo3(signature = (block))]
    fn discard_aerodrome_before_block(&self, block: u64) -> PyResult<()> {
        let mut core = self.core.write();
        if core.get_aerodrome_pool(self.pool_id).is_none() {
            return Ok(());
        }
        core.discard_pool_before_block(self.pool_id, block)
            .unwrap_or(Ok(()))
            .map_err(journal_err_to_py)
    }

    /// Restore the Aerodrome pool to the landed-at state just before `block`.
    ///
    /// Returns `(reserve0, reserve1, block)` as Python ints, or `None` if the
    /// pool ID is not registered / not an Aerodrome pool.
    ///
    /// Raises:
    ///     `ValueError`: If `block` is at or before the registration block.
    #[pyo3(signature = (block))]
    fn restore_aerodrome_before_block(
        &self,
        py: Python<'_>,
        block: u64,
    ) -> PyResult<Option<Py<PyAny>>> {
        // Read-after-restore (ADR-016).
        let (r0, r1, blk) = {
            let mut core = self.core.write();
            if core.get_aerodrome_pool(self.pool_id).is_none() {
                return Ok(None);
            }
            match core.restore_pool_before_block(self.pool_id, block) {
                None => return Ok(None),
                Some(Err(e)) => return Err(journal_err_to_py(e)),
                Some(Ok(())) => {
                    let state = core
                        .get_aerodrome_pool(self.pool_id)
                        .expect("Aerodrome pool confirmed above");
                    (
                        state.reserve0.to::<alloy::primitives::U256>(),
                        state.reserve1.to::<alloy::primitives::U256>(),
                        state.update_block,
                    )
                }
            }
        };
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

    // --- V3 mutations (plan-101 slice 8a) ---
    // Pool-id-keyed — the handle already holds the canonical pool_id, so no
    // address resolution is needed (single lock, single lookup).

    /// Apply a V3/V4 `Swap` event: journals the scalar priors then lands the
    /// new `sqrt_price_x96`/`liquidity`/`tick` at `block_number`.
    ///
    /// Swap events change the V3 scalars but NOT the tick data — the
    /// `tick_priors` Vec is empty here (unlike `PyBot.update_v3_pool`, which
    /// accepts tick updates from decoded Swap logs when they carry tick
    /// mutations).
    ///
    /// Raises:
    ///     `ValueError`: If `pool_id` is not registered as a V3/V4 pool.
    #[pyo3(signature = (sqrt_price_x96, liquidity, tick, block_number))]
    fn apply_swap(
        &self,
        sqrt_price_x96: &Bound<'_, PyAny>,
        liquidity: &Bound<'_, PyAny>,
        tick: i32,
        block_number: u64,
    ) -> PyResult<()> {
        let spx = crate::conversion::alloy::extract_python_u256(sqrt_price_x96)?;
        let liq = crate::conversion::alloy::extract_python_u256(liquidity)?.to::<u128>();
        // RAJ3PP: family-dispatching apply. Routes V4 pools to the V4 apply
        // path (previously this called `apply_v3_swap_by_pool_id`
        // unconditionally, which no-op'd on `PoolEntry::V4` and silently
        // dropped every Python-side V4 update). The dispatcher is one write
        // guard + two O(1) lookups; the single Python `apply_swap` API is
        // preserved.
        let _ = self.core.write().apply_swap_by_pool_id(
            self.pool_id,
            spx,
            liq,
            tick,
            block_number,
            &[],
        );
        Ok(())
    }

    /// Registration/seed genesis anchor (two-stamp OB7UNY): push a
    /// `before == after` reorg-journal delta at `block_number` WITHOUT
    /// advancing either clock, so a split-seed (price at HEAD, tick map at the
    /// DB block) pool keeps a non-empty journal for mid-window reorg restore.
    /// Returns `True` on a registered V3/V4 pool, `False` otherwise.
    #[pyo3(signature = (block_number))]
    fn seed_genesis(&self, block_number: u64) -> bool {
        self.core
            .write()
            .seed_genesis_by_pool_id(self.pool_id, block_number)
            .is_some()
    }

    /// Apply a V3 Mint/Burn event (liquidity update) via the handle.
    ///
    /// Initializes (or removes) tick entries at `tick_lower`/`tick_upper`,
    /// journals the priors for reorg rollback, invalidates the tick-range
    /// cache. Does NOT change the V3 scalars (`sqrt_price_x96`/`liquidity`/
    /// `tick`) — Mint/Burn is a tick-only event per ADR-004. The active
    /// `liquidity` scalar adjustments (when `current_tick` is in range) are
    /// applied by the engine's own path; this handle method is the raw
    /// `tick_data` mutation.
    ///
    /// Returns `True` if the update applied to a registered V3 pool, `False`
    /// if this `pool_id` is not a V3 pool (silent no-op — don't corrupt a V2
    /// pool).
    #[pyo3(signature = (tick_lower, tick_upper, liquidity_delta, block_number))]
    fn apply_liquidity_update(
        &self,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: &Bound<'_, PyAny>,
        block_number: u64,
    ) -> PyResult<bool> {
        // liquidity_delta is a signed V3 Mint/Burn delta (Burn events are
        // negative). Extract as i128 directly — V3 deltas fit in i128 (the
        // contract's int128 type). For unusual callers passing values outside
        // i128 range, surface OverflowError rather than silently muffling.
        let delta: i128 = liquidity_delta.extract().map_err(|_| {
            pyo3::exceptions::PyOverflowError::new_err(
                "liquidity_delta must fit in i128 (V3 contract int128 range)",
            )
        })?;
        let applied = self.core.write().apply_liquidity_update_by_pool_id(
            self.pool_id,
            tick_lower,
            tick_upper,
            delta,
            block_number,
        );
        Ok(applied.is_some())
    }

    /// Replace this pool's `tick_data` with an external snapshot (Python
    /// sparse-map backfill). Mirrors the Python `UniswapV3Pool.update_tick_data`
    /// — the companion delegates here once it's rewritten over the handle
    /// (plan-101 slice 8b). No journal delta (full-sync; the pump is the
    /// authority for event-derived ticks — mirrors `sync_v3_pool_state`).
    ///
    /// `tick_data` is the SAME shape `tick_data_snapshot` returns:
    /// `{tick: (liquidity_gross, liquidity_net, block)}` — the write path is
    /// symmetric with the read path, + the companion converts its
    /// `LiquidityAtTick` objects to this tuple shape at the boundary (matching
    /// how V2 converts its Python `Fraction` fees to the Rust `gamma_numer` at
    /// the boundary). The `tick_bitmap` dict is REDUNDANT in Rust (the bitmap
    /// is derived from `tick_data` keys — see `tick_bitmap_snapshot`);
    /// accepted for API parity with the Python caller signature, ignored.
    ///
    /// Scalars (`sqrt_price_x96`/`liquidity`/`tick`) are UNCHANGED — this is
    /// tick-only. `update_block` advances to `block` if newer (monotonic).
    ///
    /// Returns `True` if the replace applied to a registered V3/V4 pool,
    /// `False` if this `pool_id` is a V2 pool or unregistered (silent no-op —
    /// mirrors the `apply_liquidity_update` family contract).
    #[pyo3(signature = (tick_bitmap, tick_data, block))]
    fn update_tick_data(
        &self,
        tick_bitmap: &Bound<'_, PyAny>,
        tick_data: &Bound<'_, PyDict>,
        block: u64,
    ) -> PyResult<bool> {
        let _ = tick_bitmap; // redundant in Rust (derived from tick_data keys)
        let mut map: HashMap<i32, TickInfo> = HashMap::with_capacity(tick_data.len());
        for (key, value) in tick_data.iter() {
            let tick: i32 = key.extract().map_err(|_| {
                pyo3::exceptions::PyTypeError::new_err(
                    "update_tick_data: tick_data keys must be ints",
                )
            })?;
            // Symmetric with `tick_data_snapshot`: a 3-tuple
            // `(liquidity_gross, liquidity_net, block)` (the block is the
            // Symmetric with `tick_data_snapshot`: a 3-tuple
            // `(liquidity_gross, liquidity_net, block)`. The per-tick block is
            // preserved on the Rust ``TickInfo.block`` field (mirrors the
            // Python ``LiquidityAtTick.block`` — the snapshot round-trip's
            // per-tick block contract).
            let (gross, net, tick_block): (u128, i128, u64) = value.extract().map_err(|_| {
                pyo3::exceptions::PyTypeError::new_err(
                    "update_tick_data: tick_data values must be (gross, net, block) tuples",
                )
            })?;
            map.insert(
                tick,
                TickInfo {
                    liquidity_gross: alloy::primitives::U128::from(gross),
                    liquidity_net: alloy::primitives::I256::try_from(net)
                        .unwrap_or(alloy::primitives::I256::ZERO),
                    block: tick_block,
                },
            );
        }
        Ok(self
            .core
            .write()
            .sync_tick_data_by_pool_id(self.pool_id, map, block))
    }

    /// Atomic V3/V4 scalar snapshot: `(sqrt_price_x96, liquidity, tick, block)`.
    ///
    /// All four fields are read under ONE read guard (the same atomicity
    /// contract as V2 `snapshot()`). The Python companion's `state` property
    /// builds a `UniswapV3PoolState` from this single tuple — no torn reads.
    ///
    /// Returns `None` if this `pool_id` is not registered as a V3/V4 pool.
    #[pyo3(signature = ())]
    fn snapshot_v3(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let snap = {
            let core = self.core.read();
            let Some(s) = core.get_v3_or_v4_pool(self.pool_id) else {
                return Ok(None);
            };
            (
                s.sqrt_price_x96(),
                s.liquidity(),
                s.tick(),
                s.update_block(),
            )
        };
        let tuple = pyo3::types::PyTuple::new(
            py,
            [
                crate::conversion::alloy::u256_to_py(py, &snap.0)?.unbind(),
                snap.1.into_pyobject(py)?.into_any().unbind(),
                snap.2.into_pyobject(py)?.into_any().unbind(),
                snap.3.into_pyobject(py)?.into_any().unbind(),
            ],
        )?;
        Ok(Some(tuple.into_any().unbind()))
    }

    /// Discard V3/V4 reorg journal deltas earlier than `block`.
    ///
    /// No-op if the earliest delta is at/after the target; errors if the
    /// target is past the newest delta. Silent no-op when this `pool_id` is not
    /// a registered V3/V4 pool (so a Python companion built for V3 doesn't
    /// touch V2/V4 journal state).
    ///
    /// Raises:
    ///     `ValueError`: If the target is past the newest delta.
    #[pyo3(signature = (block))]
    fn discard_v3_before_block(&self, block: u64) -> PyResult<()> {
        let mut core = self.core.write();
        // J63J3N: only apply when this handle points at a V3/V4 pool —
        // otherwise silently no-op. Unified dispatcher (ADR-016).
        if core.get_v3_or_v4_pool(self.pool_id).is_none() {
            return Ok(());
        }
        core.discard_pool_before_block(self.pool_id, block)
            .unwrap_or(Ok(()))
            .map_err(journal_err_to_py)
    }

    /// Restore a V3/V4 pool to the landed-at state just before `block`.
    ///
    /// Returns `(sqrt_price_x96, liquidity, tick, block)` as Python ints, or
    /// `None` if this `pool_id` is not registered as a V3/V4 pool.
    ///
    /// Raises:
    ///     `ValueError`: If `block` is at or before the registration block.
    #[pyo3(signature = (block))]
    fn restore_v3_before_block(&self, py: Python<'_>, block: u64) -> PyResult<Option<Py<PyAny>>> {
        // Read-after-restore (ADR-016 D4): the trait returns `()`; the
        // post-restore scalar fields ARE the before-values the
        // `V3RestoreResult.scalar_priors` previously carried.
        let (sqrt_p, liq, tick, blk) = {
            let mut core = self.core.write();
            if core.get_v3_or_v4_pool(self.pool_id).is_none() {
                return Ok(None);
            }
            match core.restore_pool_before_block(self.pool_id, block) {
                None => return Ok(None),
                Some(Err(e)) => return Err(journal_err_to_py(e)),
                Some(Ok(())) => {
                    let s = core
                        .get_v3_or_v4_pool(self.pool_id)
                        .expect("V3/V4 pool confirmed above");
                    (
                        s.sqrt_price_x96(),
                        s.liquidity(),
                        s.tick(),
                        s.update_block(),
                    )
                }
            }
        };
        let tuple = pyo3::types::PyTuple::new(
            py,
            [
                crate::conversion::alloy::u256_to_py(py, &sqrt_p)?.unbind(),
                liq.into_pyobject(py)?.into_any().unbind(),
                tick.into_pyobject(py)?.into_any().unbind(),
                blk.into_pyobject(py)?.into_any().unbind(),
            ],
        )?;
        Ok(Some(tuple.into_any().unbind()))
    }

    /// Snapshot of the V3/V4 `tick_data` `HashMap` as a Python dict.
    ///
    /// Returns `{tick: (liquidity_gross, liquidity_net, block)}` — the Python
    /// companion's `tick_data` property lifts each row into an immutable
    /// `LiquidityAtTick(liquidity_net, liquidity_gross, block)`. Rust's
    /// `TickInfo` stores `liquidity_gross`, `liquidity_net`, and `block`
    /// (the block at which the tick was last mutated — mirrors the Python
    /// ``LiquidityAtTick.block`` field).
    ///
    /// Returns an empty dict if this `pool_id` is not registered as a V3/V4
    /// pool (defensive — non-V3 callers shouldn't crash).
    #[pyo3(signature = ())]
    fn tick_data_snapshot(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let rows: Vec<(i32, (u128, i128, u64))> = {
            let core = self.core.read();
            let Some(s) = core.get_v3_or_v4_pool(self.pool_id) else {
                return Ok(pyo3::types::PyDict::new(py).into_any().unbind());
            };
            s.tick_data()
                .iter()
                .map(|(tick, info)| {
                    let net: i128 = i128::try_from(info.liquidity_net).unwrap_or(0);
                    let gross: u128 = info.liquidity_gross.to::<u128>();
                    (*tick, (gross, net, info.block))
                })
                .collect()
        };
        let dict = pyo3::types::PyDict::new(py);
        for (tick, (gross, net, block)) in rows {
            let row = pyo3::types::PyTuple::new(
                py,
                [
                    gross.into_pyobject(py)?.into_any().unbind(),
                    net.into_pyobject(py)?.into_any().unbind(),
                    block.into_pyobject(py)?.into_any().unbind(),
                ],
            )?;
            dict.set_item(tick, row)?;
        }
        Ok(dict.into_any().unbind())
    }

    /// Snapshot of the V3/V4 tick bitmap, synthesized from `tick_data` keys.
    ///
    /// Rust's `V3PoolState` doesn't store a separate bitmap — the bitmap is
    /// derivable from `tick_data` keys (initialized ticks). Returns
    /// `{word_pos: (bitmap_int, block)}` where `word_pos = (tick //
    /// tick_spacing) >> 8` and the bit set is `(tick // tick_spacing) % 256`.
    /// Matches Solidity `TickBitmap.position(tick / tickSpacing)`.
    ///
    /// Returns an empty dict for non-V3/V4 `pool_ids`.
    #[pyo3(signature = ())]
    fn tick_bitmap_snapshot(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        // Collect (word_pos, bit_pos, block) per initialized tick under one read
        // guard, then build the output dict without holding the lock.
        let rows: Vec<(i32, u32, u64)> = {
            let core = self.core.read();
            let spacing = core
                .get_v3_identity(self.pool_id)
                .map(|i| i.tick_spacing)
                .or_else(|| {
                    core.get_v4_identity(self.pool_id)
                        .map(|i| i.pool_key.tick_spacing)
                })
                .unwrap_or(1)
                .max(1);
            let Some(s) = core.get_v3_or_v4_pool(self.pool_id) else {
                return Ok(pyo3::types::PyDict::new(py).into_any().unbind());
            };
            let update_block = s.update_block();
            // U256 (256 bits per word) — `u128` would overflow for bit positions
            // ≥ 128 (large ticks land in high bits). Mirrors Solidity's uint256.
            s.tick_data()
                .keys()
                .map(|&tick| {
                    let compressed = tick / spacing;
                    (
                        compressed >> 8,
                        compressed.rem_euclid(256) as u32,
                        update_block,
                    )
                })
                .collect()
        };
        // Fold bits into per-word (bitmap_int, block) accumulators.
        let one = alloy::primitives::U256::from(1u64);
        let mut words: std::collections::BTreeMap<i32, (alloy::primitives::U256, u64)> =
            std::collections::BTreeMap::new();
        for (word_pos, bit_pos, block) in rows {
            words
                .entry(word_pos)
                .and_modify(|(bits, _)| *bits |= one << bit_pos)
                .or_insert((one << bit_pos, block));
        }
        let dict = pyo3::types::PyDict::new(py);
        for (word_pos, (bits, block)) in words {
            let tuple = pyo3::types::PyTuple::new(
                py,
                [
                    crate::conversion::alloy::u256_to_py(py, &bits)?.unbind(),
                    block.into_pyobject(py)?.into_any().unbind(),
                ],
            )?;
            dict.set_item(word_pos, tuple)?;
        }
        Ok(dict.into_any().unbind())
    }

    // --- Curve state read getters + mutations (ADR-005 slice 11a state port) ---

    /// Number of tokens for a Curve pool (`balances.len()`).
    ///
    /// Returns 0 if this `pool_id` is not registered as a Curve pool.
    #[getter]
    fn n_coins(&self) -> usize {
        self.core
            .read()
            .get_curve_identity(self.pool_id)
            .map_or(0, CurvePoolIdentity::n_coins)
    }

    /// Current balances for a Curve pool (one `U256` per token).
    ///
    /// Returns `None` if this `pool_id` is not registered as a Curve pool
    /// (so a V2/V3/V4 companion built for a different family doesn't crash).
    #[getter]
    fn balances(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let bal: Vec<alloy::primitives::U256> = {
            let core = self.core.read();
            let Some(s) = core.get_curve_pool(self.pool_id) else {
                return Ok(pyo3::types::PyList::empty(py).into_any().unbind());
            };
            s.balances.clone()
        };
        let py_bal: Vec<Py<PyAny>> = bal
            .iter()
            .map(|b| crate::conversion::alloy::u256_to_py(py, b).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        Ok(pyo3::types::PyList::new(py, py_bal)?.into_any().unbind())
    }

    /// Snapshot a Curve pool's mutable state as `(balances, update_block)`.
    ///
    /// Returns `None` for non-Curve pools (the V3/V4 `snapshot_v3` family
    /// analogue — family-dispatching readers).
    #[pyo3(signature = ())]
    fn snapshot_curve(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let snap: Option<(Vec<alloy::primitives::U256>, u64)> = {
            let core = self.core.read();
            let Some(s) = core.get_curve_pool(self.pool_id) else {
                return Ok(None);
            };
            Some((s.balances.clone(), s.update_block))
        };
        let Some(snap) = snap else {
            return Ok(None);
        };
        let py_bal: Vec<Py<PyAny>> = snap
            .0
            .iter()
            .map(|b| crate::conversion::alloy::u256_to_py(py, b).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        let list = pyo3::types::PyList::new(py, py_bal)?;
        let tuple = pyo3::types::PyTuple::new(
            py,
            [
                list.into_any().unbind(),
                snap.1.into_pyobject(py)?.into_any().unbind(),
            ],
        )?;
        Ok(Some(tuple.into_any().unbind()))
    }

    // --- Curve identity getters (ADR-005 identity extension, BOMDRK) ---

    /// Curve A-ramping: `(initial_a, future_a, initial_a_time,
    /// future_a_time, create_timestamp)` — all `None` for non-ramping pools.
    /// Returns `None` for a non-Curve handle.
    ///
    /// Each element is the option value so a non-ramping pool reports `None`
    /// for every field instead of a sentinel zero.
    // The nested-`Option` tuple mirrors the Python-facing Curve ramp shape.
    #[allow(clippy::type_complexity)]
    fn curve_a_ramp(
        &self,
    ) -> Option<(
        Option<u128>,
        Option<u128>,
        Option<u64>,
        Option<u64>,
        Option<u64>,
    )> {
        let core = self.core.read();
        let id = core.get_curve_identity(self.pool_id)?;
        Some((
            id.initial_a_coefficient,
            id.future_a_coefficient,
            id.initial_a_coefficient_time,
            id.future_a_coefficient_time,
            id.create_timestamp,
        ))
    }

    /// Curve crypto-pool fees: `(fee_gamma, mid_fee, offpeg_fee_multiplier,
    /// out_fee, gamma)` — `None` for standard stableswap pools. Returns `None`
    /// for a non-Curve handle.
    // The nested-`Option` tuple mirrors the Python-facing Curve fee shape.
    #[allow(clippy::type_complexity)]
    fn curve_crypto_fees(
        &self,
    ) -> Option<(
        Option<u64>,
        Option<u64>,
        Option<u64>,
        Option<u64>,
        Option<u64>,
    )> {
        let core = self.core.read();
        let id = core.get_curve_identity(self.pool_id)?;
        Some((
            id.fee_gamma,
            id.mid_fee,
            id.offpeg_fee_multiplier,
            id.out_fee,
            id.gamma,
        ))
    }

    /// Curve dedicated LP token address (EIP-55 checksummed) — `None` when the
    /// pool token itself is the LP. Returns `None` for a non-Curve handle.
    // `Option<Option<_>>` distinguishes no-pool / pool-no-lp-token / has-token.
    #[allow(clippy::option_option)]
    fn curve_lp_token(&self) -> Option<Option<String>> {
        let core = self.core.read();
        let id = core.get_curve_identity(self.pool_id)?;
        Some(
            id.lp_token
                .map(|a| address_utils::address_to_checksum_string(&a)),
        )
    }

    /// Curve per-token `use_lending` flags. Empty list for a non-Curve pool
    /// or when none were registered.
    #[getter]
    fn curve_use_lending(&self) -> Vec<bool> {
        let core = self.core.read();
        match core.get_curve_identity(self.pool_id) {
            Some(i) => i.use_lending.clone(),
            None => Vec::new(),
        }
    }

    /// Curve per-token `precision_multipliers` (one `U256` per token). Empty
    /// list for a non-Curve pool.
    #[getter]
    fn curve_precision_multipliers(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let pms: Vec<alloy::primitives::U256> = {
            let core = self.core.read();
            match core.get_curve_identity(self.pool_id) {
                Some(i) => i.precision_multipliers.clone(),
                None => Vec::new(),
            }
        };
        let py_pms: Vec<Py<PyAny>> = pms
            .iter()
            .map(|b| crate::conversion::alloy::u256_to_py(py, b).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        Ok(pyo3::types::PyList::new(py, py_pms)?.into_any().unbind())
    }

    /// Whether a Curve data-provider I/O trait object is stored on this
    /// pool's state (ADR-005 JFGCHJ). `False` for non-Curve pools or Curve
    /// pools registered without a provider (the no-I/O fixture case).
    #[getter]
    fn curve_has_data_provider(&self) -> bool {
        let core = self.core.read();
        match core.get_curve_pool(self.pool_id) {
            Some(s) => s.data_provider.is_some(),
            None => false,
        }
    }

    // --- Curve identity getters (ADR-005 BQM2OA identity-from-handle) ---

    /// Curve amplification coefficient `A` (raw). 0 for a non-Curve handle.
    #[getter]
    fn curve_a_coefficient(&self) -> u128 {
        let core = self.core.read();
        core.get_curve_identity(self.pool_id)
            .map_or(0, |i| i.a_coefficient)
    }

    /// Curve swap fee (`FEE_DENOMINATOR` units). 0 for a non-Curve handle.
    #[getter]
    fn curve_fee(&self) -> u64 {
        let core = self.core.read();
        core.get_curve_identity(self.pool_id).map_or(0, |i| i.fee)
    }

    /// Curve admin-fee share (`FEE_DENOMINATOR` units). 0 for a non-Curve handle.
    #[getter]
    fn curve_admin_fee(&self) -> u64 {
        let core = self.core.read();
        core.get_curve_identity(self.pool_id)
            .map_or(0, |i| i.admin_fee)
    }

    /// Curve rate multipliers (one `U256` per token). Empty for a non-Curve pool.
    #[getter]
    fn curve_rate_multipliers(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let rms: Vec<alloy::primitives::U256> = {
            let core = self.core.read();
            match core.get_curve_identity(self.pool_id) {
                Some(i) => i.rate_multipliers.clone(),
                None => Vec::new(),
            }
        };
        let py_rms: Vec<Py<PyAny>> = rms
            .iter()
            .map(|b| crate::conversion::alloy::u256_to_py(py, b).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        Ok(pyo3::types::PyList::new(py, py_rms)?.into_any().unbind())
    }

    /// Curve `swap_style` discriminant (`PoolStrategies.swap_style.value`).
    /// 0 for a non-Curve handle.
    #[getter]
    fn curve_swap_style(&self) -> u8 {
        let core = self.core.read();
        core.get_curve_identity(self.pool_id)
            .map_or(0, |i| i.swap_style)
    }

    /// Curve `lending_rate_style` discriminant. 0 for a non-Curve handle.
    #[getter]
    fn curve_lending_rate_style(&self) -> u8 {
        let core = self.core.read();
        core.get_curve_identity(self.pool_id)
            .map_or(0, |i| i.lending_rate_style)
    }

    /// Curve `d_variant` discriminant. 0 for a non-Curve handle.
    #[getter]
    fn curve_d_variant(&self) -> u8 {
        let core = self.core.read();
        core.get_curve_identity(self.pool_id)
            .map_or(0, |i| i.d_variant)
    }

    /// Curve `y_variant` discriminant. 0 for a non-Curve handle.
    #[getter]
    fn curve_y_variant(&self) -> u8 {
        let core = self.core.read();
        core.get_curve_identity(self.pool_id)
            .map_or(0, |i| i.y_variant)
    }

    /// Curve `yd_variant` discriminant. 0 for a non-Curve handle.
    #[getter]
    fn curve_yd_variant(&self) -> u8 {
        let core = self.core.read();
        core.get_curve_identity(self.pool_id)
            .map_or(0, |i| i.yd_variant)
    }

    /// Curve `metapool_rate_style` discriminant. 0 for a non-Curve handle.
    #[getter]
    fn curve_metapool_rate_style(&self) -> u8 {
        let core = self.core.read();
        core.get_curve_identity(self.pool_id)
            .map_or(0, |i| i.metapool_rate_style)
    }

    /// Curve `metapool_underlying_style` discriminant. 0 for a non-Curve handle.
    #[getter]
    fn curve_metapool_underlying_style(&self) -> u8 {
        let core = self.core.read();
        core.get_curve_identity(self.pool_id)
            .map_or(0, |i| i.metapool_underlying_style)
    }

    /// Curve base-pool address (EIP-55 checksummed). `None` for plain pools.
    /// `None` for a non-Curve handle.
    // `Option<Option<_>>` distinguishes no-pool / pool-no-base / has-base.
    #[allow(clippy::option_option)]
    fn curve_base_pool_address(&self) -> Option<Option<String>> {
        let core = self.core.read();
        let id = core.get_curve_identity(self.pool_id)?;
        Some(
            id.base_pool
                .map(|a| address_utils::address_to_checksum_string(&a)),
        )
    }

    /// The Curve pool's token companion handles, resolved via the shared
    /// `BotState` token registry. `None` if this is not a Curve pool or any
    /// token isn't registered (mirror of `get_balancer_tokens`). The companion
    /// wraps each via `Erc20Token._from_py_token`.
    fn get_curve_tokens(&self) -> Option<Vec<PyErc20Token>> {
        let core = self.core.read();
        let identity = core.get_curve_identity(self.pool_id)?;
        let mut out = Vec::with_capacity(identity.tokens.len());
        for token_addr in &identity.tokens {
            if !core.has_token(token_addr) {
                return None;
            }
            out.push(PyErc20Token::new(Arc::clone(&self.core), *token_addr));
        }
        Some(out)
    }

    /// The Curve pool's *underlying* token companion handles (metapool coins
    /// beneath the base-pool intermediaries). `None` for plain pools, or if
    /// this isn't a Curve pool, or any underlying token isn't registered.
    fn get_curve_tokens_underlying(&self) -> Option<Vec<PyErc20Token>> {
        let core = self.core.read();
        let identity = core.get_curve_identity(self.pool_id)?;
        let underlying = identity.tokens_underlying.clone()?;
        let mut out = Vec::with_capacity(underlying.len());
        for token_addr in &underlying {
            if !core.has_token(token_addr) {
                return None;
            }
            out.push(PyErc20Token::new(Arc::clone(&self.core), *token_addr));
        }
        Some(out)
    }

    /// The Curve pool's dedicated LP-token companion handle. `None` ⇔ the
    /// pool token is itself the LP (the common plain-pool case; the companion
    /// falls back to `tokens[0]`). Returns `None` (outer) for a non-Curve pool
    /// or if the LP token isn't registered.
    fn get_curve_lp_token(&self) -> Option<PyErc20Token> {
        let core = self.core.read();
        let identity = core.get_curve_identity(self.pool_id)?;
        let lp = identity.lp_token?;
        if !core.has_token(&lp) {
            return None;
        }
        Some(PyErc20Token::new(Arc::clone(&self.core), lp))
    }

    /// The Curve pool's raw ERC-20 coin addresses in canonical order. Unlike
    /// `get_curve_tokens`, this does NOT require the tokens to be registered
    /// first — it is the builder/companion-orchestration seam that lets a
    /// caller construct ERC20 companions *before* `_from_py_pool`. `None` for
    /// a non-Curve pool.
    fn curve_token_addresses(&self) -> Option<Vec<String>> {
        let core = self.core.read();
        let identity = core.get_curve_identity(self.pool_id)?;
        let mut out = Vec::with_capacity(identity.tokens.len());
        for address in &identity.tokens {
            out.push(address_utils::address_to_checksum_string(address));
        }
        Some(out)
    }

    /// The Curve pool's raw *underlying* coin addresses (metapool coins beneath
    /// the base-pool intermediaries). `None` for a plain pool or a non-Curve
    /// handle. Not registration-gated (twin of `curve_token_addresses`).
    fn curve_token_addresses_underlying(&self) -> Option<Vec<String>> {
        let core = self.core.read();
        let identity = core.get_curve_identity(self.pool_id)?;
        let underlying = identity.tokens_underlying.as_ref()?;
        let mut out = Vec::with_capacity(underlying.len());
        for address in underlying {
            out.push(address_utils::address_to_checksum_string(address));
        }
        Some(out)
    }

    /// The Curve pool's raw dedicated LP-token address, or `None` if the pool
    /// token is itself the LP (or the handle is not a Curve pool). Not
    /// registration-gated (twin of `curve_token_addresses`).
    fn curve_lp_token_address(&self) -> Option<String> {
        let core = self.core.read();
        let identity = core.get_curve_identity(self.pool_id)?;
        identity
            .lp_token
            .map(|address| address_utils::address_to_checksum_string(&address))
    }

    /// **The go-between (BQM2OA).** A `PyLiquidityPool` handle over this
    /// metapool's *base pool*, sharing the same `BotState` core. Resolves the
    /// stored `base_pool` address through the existing `pool_id_by_address`
    /// index — no Python registry needed. `None` for plain pools, non-Curve
    /// handles, or when the base pool isn't registered. The companion recurses:
    /// `CurveStableswapPool._from_py_pool(handle.curve_base_pool())`.
    fn curve_base_pool(&self) -> Option<PyLiquidityPool> {
        let core = self.core.read();
        let id = core.get_curve_identity(self.pool_id)?;
        let base_addr = id.base_pool?;
        let base_id = core.pool_id_by_address(&base_addr)?;
        // Construct a handle over the base pool's id, sharing this core.
        drop(core);
        Some(PyLiquidityPool::new(Arc::clone(&self.core), base_id))
    }

    /// Rust-owned Curve stableswap `curve_get_dy(i, j, dx, block_number,
    /// override_balances)` on this handle's pool — the single-call shape the
    /// companion `CurveStableswapPool.get_dy` delegates to (task `V5X2YP`).
    /// Mirrors `PyBot.curve_get_dy` but bound to the handle's `pool_id`, so a
    /// swap runs start-to-finish with no Python provider / cache / calculator.
    #[pyo3(signature = (i, j, dx, block_number, override_balances=None))]
    fn curve_get_dy(
        &self,
        py: Python<'_>,
        i: usize,
        j: usize,
        dx: &Bound<'_, PyAny>,
        block_number: u64,
        override_balances: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let amount = crate::conversion::alloy::extract_python_u256(dx)?;
        let overrides = match override_balances {
            Some(list) => {
                let cast = list.cast::<pyo3::types::PyList>().map_err(|_| {
                    pyo3::exceptions::PyTypeError::new_err("override_balances must be a list[int]")
                })?;
                Some(crate::bot::extract_u256_list(cast)?)
            }
            None => None,
        };
        let result = {
            let core = self.core.read();
            core.curve_get_dy(
                self.pool_id,
                i,
                j,
                amount,
                block_number,
                overrides.as_deref(),
            )
        };
        let out = match result {
            Ok(v) => v,
            Err(e) => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "Curve get_dy failure: {e:?}"
                )));
            }
        };
        let bound = crate::conversion::alloy::u256_to_py(py, &out)?;
        Ok(bound.unbind())
    }

    /// Rust-owned Curve metapool `curve_get_dy_underlying(i, j, dx,
    /// block_number, override_balances)` on this handle's pool — the
    /// single-call shape the companion `CurveStableswapPool
    /// ._get_dy_underlying` delegates to. Base-pool ops go through the Rust
    /// port (`BotCurveBasePoolPort`), so the Python `_LazyBasePool` go-between
    /// is retired for the swap path.
    #[pyo3(signature = (i, j, dx, block_number, override_balances=None))]
    fn curve_get_dy_underlying(
        &self,
        py: Python<'_>,
        i: usize,
        j: usize,
        dx: &Bound<'_, PyAny>,
        block_number: u64,
        override_balances: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let amount = crate::conversion::alloy::extract_python_u256(dx)?;
        let overrides = match override_balances {
            Some(list) => {
                let cast = list.cast::<pyo3::types::PyList>().map_err(|_| {
                    pyo3::exceptions::PyTypeError::new_err("override_balances must be a list[int]")
                })?;
                Some(crate::bot::extract_u256_list(cast)?)
            }
            None => None,
        };
        let result = {
            let core = self.core.read();
            core.curve_get_dy_underlying(
                self.pool_id,
                i,
                j,
                amount,
                block_number,
                overrides.as_deref(),
            )
        };
        let out = match result {
            Ok(v) => v,
            Err(e) => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "Curve get_dy_underlying failure: {e:?}"
                )));
            }
        };
        let bound = crate::conversion::alloy::u256_to_py(py, &out)?;
        Ok(bound.unbind())
    }

    /// Rust-owned Curve `curve_calc_token_amount(amounts, deposit,
    /// block_number)` on this handle's pool — the single-call shape the
    /// companion `CurveStableswapPool.calc_token_amount` delegates to (task
    /// `WKKMJM`). No Python provider / cache / calculator on the path.
    fn curve_calc_token_amount(
        &self,
        py: Python<'_>,
        amounts: &Bound<'_, PyAny>,
        deposit: bool,
        block_number: u64,
    ) -> PyResult<Py<PyAny>> {
        let cast = amounts
            .cast::<pyo3::types::PyList>()
            .map_err(|_| pyo3::exceptions::PyTypeError::new_err("amounts must be a list[int]"))?;
        let amounts = crate::bot::extract_u256_list(cast)?;
        let result = {
            let core = self.core.read();
            core.curve_calc_token_amount(self.pool_id, &amounts, deposit, block_number)
        };
        let out = match result {
            Ok(v) => v,
            Err(e) => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "Curve calc_token_amount failure: {e:?}"
                )));
            }
        };
        let bound = crate::conversion::alloy::u256_to_py(py, &out)?;
        Ok(bound.unbind())
    }

    /// Rust-owned Curve `curve_calc_withdraw_one_coin(token_amount, i,
    /// block_number)` on this handle's pool — the single-call shape the
    /// companion `CurveStableswapPool.calc_withdraw_one_coin` delegates to
    /// (task `WKKMJM`). Returns only the coin-`i` `dy` (the companion's
    /// extra tuple fields aren't consumed anywhere).
    fn curve_calc_withdraw_one_coin(
        &self,
        py: Python<'_>,
        token_amount: &Bound<'_, PyAny>,
        i: usize,
        block_number: u64,
    ) -> PyResult<Py<PyAny>> {
        let token_amount = crate::conversion::alloy::extract_python_u256(token_amount)?;
        let result = {
            let core = self.core.read();
            core.curve_calc_withdraw_one_coin(self.pool_id, token_amount, i, block_number)
        };
        let out = match result {
            Ok(v) => v,
            Err(e) => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "Curve calc_withdraw_one_coin failure: {e:?}"
                )));
            }
        };
        let bound = crate::conversion::alloy::u256_to_py(py, &out)?;
        Ok(bound.unbind())
    }

    // --- Curve data-provider read-throughs (so the companion's PerBlockCache
    //     reads through the stored trait object via a handle adapter,
    //     mirroring the Balancer `_HandleRateProviderAdapter`). Each returns
    //     `None`/empty ⇔ no provider stored or not a Curve pool; provider
    //     errors also surface as `None`/empty so the Python calc path raises
    //     the `MissingCurveData` it already expects. ---

    fn fetch_curve_block_number(&self) -> Option<u64> {
        let core = self.core.read();
        let provider = core.get_curve_pool(self.pool_id)?.data_provider.clone()?;
        drop(core);
        provider.block_number().ok()
    }

    fn fetch_curve_block_timestamp(&self, block_number: u64) -> Option<u64> {
        let core = self.core.read();
        let provider = core.get_curve_pool(self.pool_id)?.data_provider.clone()?;
        drop(core);
        provider.block_timestamp(block_number).ok()
    }

    fn fetch_curve_token_balance(
        &self,
        py: Python<'_>,
        token_address: &str,
        holder_address: &str,
        block_number: u64,
    ) -> PyResult<Option<Py<PyAny>>> {
        let provider = {
            let core = self.core.read();
            let Some(s) = core.get_curve_pool(self.pool_id) else {
                return Ok(None);
            };
            let Some(p) = s.data_provider.clone() else {
                return Ok(None);
            };
            p
        };
        let Ok(tok) = address_utils::parse_address(token_address) else {
            return Ok(None);
        };
        let Ok(holder) = address_utils::parse_address(holder_address) else {
            return Ok(None);
        };
        match provider.token_balance(tok, holder, block_number) {
            Ok(v) => Ok(Some(crate::conversion::alloy::u256_to_py(py, &v)?.unbind())),
            Err(_) => Ok(None),
        }
    }

    fn fetch_curve_token_total_supply(
        &self,
        py: Python<'_>,
        token_address: &str,
        block_number: u64,
    ) -> PyResult<Option<Py<PyAny>>> {
        let provider = {
            let core = self.core.read();
            let Some(s) = core.get_curve_pool(self.pool_id) else {
                return Ok(None);
            };
            let Some(p) = s.data_provider.clone() else {
                return Ok(None);
            };
            p
        };
        let Ok(tok) = address_utils::parse_address(token_address) else {
            return Ok(None);
        };
        match provider.token_total_supply(tok, block_number) {
            Ok(v) => Ok(Some(crate::conversion::alloy::u256_to_py(py, &v)?.unbind())),
            Err(_) => Ok(None),
        }
    }

    fn fetch_curve_lending_rates(&self, py: Python<'_>, block_number: u64) -> PyResult<Py<PyAny>> {
        let rates = self.read_provider_vec(py, |p| p.lending_rates(block_number))?;
        Ok(rates)
    }

    fn fetch_curve_d(&self, py: Python<'_>, block_number: u64) -> PyResult<Option<Py<PyAny>>> {
        self.read_provider_opt(py, |p| p.d(block_number))
    }

    fn fetch_curve_gamma(&self, py: Python<'_>, block_number: u64) -> PyResult<Option<Py<PyAny>>> {
        self.read_provider_opt(py, |p| p.gamma(block_number))
    }

    fn fetch_curve_price_scale(&self, py: Python<'_>, block_number: u64) -> PyResult<Py<PyAny>> {
        self.read_provider_vec(py, |p| p.price_scale(block_number))
    }

    fn fetch_curve_admin_balances(&self, py: Python<'_>, block_number: u64) -> PyResult<Py<PyAny>> {
        self.read_provider_vec(py, |p| p.admin_balances(block_number))
    }

    fn fetch_curve_redemption_price(
        &self,
        py: Python<'_>,
        block_number: u64,
    ) -> PyResult<Option<Py<PyAny>>> {
        self.read_provider_opt(py, |p| p.redemption_price(block_number))
    }

    fn fetch_curve_base_cache_updated(&self, block_number: u64) -> Option<u64> {
        let core = self.core.read();
        let provider = core.get_curve_pool(self.pool_id)?.data_provider.clone()?;
        drop(core);
        provider.base_cache_updated(block_number).ok()
    }

    fn fetch_curve_base_virtual_price(
        &self,
        py: Python<'_>,
        block_number: u64,
    ) -> PyResult<Option<Py<PyAny>>> {
        self.read_provider_opt(py, |p| p.base_virtual_price(block_number))
    }

    fn fetch_curve_virtual_price(
        &self,
        py: Python<'_>,
        block_number: u64,
    ) -> PyResult<Option<Py<PyAny>>> {
        self.read_provider_opt(py, |p| p.virtual_price(block_number))
    }

    /// Apply a Curve `external_update` (new balances from an `Exchange` event).
    ///
    /// Journals the prior balances then lands the new balances + `update_block`.
    /// Silent no-op (`False`) if this `pool_id` is not registered as a Curve
    /// pool (so a V2/V3/V4 companion doesn't corrupt its state).
    #[pyo3(signature = (balances, block_number))]
    fn apply_curve_balance_update(
        &self,
        balances: &Bound<'_, PyList>,
        block_number: u64,
    ) -> PyResult<bool> {
        let bal: Vec<alloy::primitives::U256> = balances
            .iter()
            .map(|item| crate::conversion::alloy::extract_python_u256(&item))
            .collect::<PyResult<_>>()?;
        Ok(self
            .core
            .write()
            .apply_balance_update_by_pool_id(self.pool_id, bal, block_number)
            .is_some())
    }

    // --- Balancer weighted state read getters + mutations
    //     (ADR-005 slice 12a state port) ---

    /// Token count for a Balancer weighted pool (`balances.len()`).
    ///
    /// Returns 0 if this `pool_id` is not registered as a Balancer weighted pool.
    #[getter]
    fn n_balancer_tokens(&self) -> usize {
        self.core
            .read()
            .get_balancer_weighted_identity(self.pool_id)
            .map_or(0, BalancerWeightedPoolIdentity::n_tokens)
    }

    /// Current balances for a Balancer weighted pool (one `U256` per token).
    ///
    /// Returns an empty list if this `pool_id` is not registered as a
    /// Balancer weighted pool (so a V2/V3/V4/Curve companion built for a
    /// different family doesn't crash).
    #[getter]
    fn balancer_balances(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let bal: Vec<alloy::primitives::U256> = {
            let core = self.core.read();
            let Some(s) = core.get_balancer_weighted_pool(self.pool_id) else {
                return Ok(pyo3::types::PyList::empty(py).into_any().unbind());
            };
            s.balances.clone()
        };
        let py_bal: Vec<Py<PyAny>> = bal
            .iter()
            .map(|b| crate::conversion::alloy::u256_to_py(py, b).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        Ok(pyo3::types::PyList::new(py, py_bal)?.into_any().unbind())
    }

    /// Snapshot a Balancer weighted pool's mutable state as
    /// `(balances, update_block)`.
    ///
    /// Returns `None` for non-Balancer-weighted pools (the family-
    /// dispatching reader analogue to `snapshot_curve` / `snapshot_v3`).
    #[pyo3(signature = ())]
    fn snapshot_balancer_weighted(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let snap: Option<(Vec<alloy::primitives::U256>, u64)> = {
            let core = self.core.read();
            let Some(s) = core.get_balancer_weighted_pool(self.pool_id) else {
                return Ok(None);
            };
            Some((s.balances.clone(), s.update_block))
        };
        let Some(snap) = snap else {
            return Ok(None);
        };
        let py_bal: Vec<Py<PyAny>> = snap
            .0
            .iter()
            .map(|b| crate::conversion::alloy::u256_to_py(py, b).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        let list = pyo3::types::PyList::new(py, py_bal)?;
        let tuple = pyo3::types::PyTuple::new(
            py,
            [
                list.into_any().unbind(),
                snap.1.into_pyobject(py)?.into_any().unbind(),
            ],
        )?;
        Ok(Some(tuple.into_any().unbind()))
    }

    /// Apply a Balancer weighted `external_update` (new balances from a Vault
    /// `PoolBalanceChanged` event).
    ///
    /// Journals the prior balances then lands the new balances +
    /// `update_block`. Silent no-op (`False`) if this `pool_id` is not
    /// registered as a Balancer weighted pool (so a V2/V3/V4/Curve companion
    /// doesn't corrupt its state).
    #[pyo3(signature = (balances, block_number))]
    fn apply_balancer_weighted_balance_update(
        &self,
        balances: &Bound<'_, PyList>,
        block_number: u64,
    ) -> PyResult<bool> {
        let bal: Vec<alloy::primitives::U256> = balances
            .iter()
            .map(|item| crate::conversion::alloy::extract_python_u256(&item))
            .collect::<PyResult<_>>()?;
        Ok(self
            .core
            .write()
            .apply_balance_update_by_pool_id(self.pool_id, bal, block_number)
            .is_some())
    }

    // --- Balancer stable state read getters + mutations
    //     (ADR-005 slice 12c state port) ---

    /// Token count for a Balancer stable pool (`balances.len()` — includes
    /// BPT for Composable pools).
    ///
    /// Returns 0 if this `pool_id` is not registered as a Balancer stable pool.
    #[getter]
    fn n_balancer_stable_tokens(&self) -> usize {
        self.core
            .read()
            .get_balancer_stable_identity(self.pool_id)
            .map_or(0, BalancerStablePoolIdentity::n_tokens)
    }

    /// Current balances for a Balancer stable pool (one `U256` per token,
    /// including BPT for Composable pools).
    ///
    /// Returns an empty list if this `pool_id` is not registered as a Balancer
    /// stable pool (so a companion built for a different family doesn't crash).
    #[getter]
    fn balancer_stable_balances(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let bal: Vec<alloy::primitives::U256> = {
            let core = self.core.read();
            let Some(s) = core.get_balancer_stable_pool(self.pool_id) else {
                return Ok(pyo3::types::PyList::empty(py).into_any().unbind());
            };
            s.balances.clone()
        };
        let py_bal: Vec<Py<PyAny>> = bal
            .iter()
            .map(|b| crate::conversion::alloy::u256_to_py(py, b).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        Ok(pyo3::types::PyList::new(py, py_bal)?.into_any().unbind())
    }

    /// BPT token index for a Balancer stable pool: `None` for `MetaStablePools`,
    /// `Some(i)` for `ComposableStablePools`.
    ///
    /// Returns `None` if this `pool_id` is not registered as a Balancer stable
    /// pool (also a valid value for a registered `MetaStable` — see the
    /// `invariant_version` getter to distinguish).
    #[getter]
    fn balancer_bpt_index(&self) -> Option<usize> {
        self.core
            .read()
            .get_balancer_stable_identity(self.pool_id)
            .and_then(|i| i.bpt_idx)
    }

    /// Amplification coefficient `amp` for a Balancer stable pool (immutable
    /// after registration in this plan — A ramping is a future, non-epic
    /// concern resolved by the builder at registration).
    ///
    /// Returns 0 if this `pool_id` is not registered as a Balancer stable pool.
    #[getter]
    fn balancer_amp(&self) -> u128 {
        self.core
            .read()
            .get_balancer_stable_identity(self.pool_id)
            .map_or(0, |i| i.amp)
    }

    /// `invariant_version` discriminator (1 = V1 always-roundDown `D_P`
    /// accumulation; 2 = V2 roundUp-param `P_D` accumulation) — the
    /// systematic-1-wei-error guard.
    ///
    /// Returns 0 if this `pool_id` is not registered as a Balancer stable pool.
    #[getter]
    fn balancer_invariant_version(&self) -> u8 {
        self.core
            .read()
            .get_balancer_stable_identity(self.pool_id)
            .map_or(0, |i| i.invariant_version)
    }

    /// Snapshot a Balancer stable pool's mutable state as
    /// `(balances, update_block)`.
    ///
    /// Returns `None` for non-Balancer-stable pools (the family-dispatching
    /// reader analogue to `snapshot_curve` / `snapshot_balancer_weighted`).
    #[pyo3(signature = ())]
    fn snapshot_balancer_stable(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let snap: Option<(Vec<alloy::primitives::U256>, u64)> = {
            let core = self.core.read();
            let Some(s) = core.get_balancer_stable_pool(self.pool_id) else {
                return Ok(None);
            };
            Some((s.balances.clone(), s.update_block))
        };
        let Some(snap) = snap else {
            return Ok(None);
        };
        let py_bal: Vec<Py<PyAny>> = snap
            .0
            .iter()
            .map(|b| crate::conversion::alloy::u256_to_py(py, b).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        let list = pyo3::types::PyList::new(py, py_bal)?;
        let tuple = pyo3::types::PyTuple::new(
            py,
            [
                list.into_any().unbind(),
                snap.1.into_pyobject(py)?.into_any().unbind(),
            ],
        )?;
        Ok(Some(tuple.into_any().unbind()))
    }

    // --- Balancer stable identity getters (ADR-005 sealed seam, MBWSGP) ---

    /// Balancer V2 stable pool's Vault singleton (EIP-55 checksummed). Empty
    /// string if not a Balancer stable pool.
    #[getter]
    fn balancer_stable_vault(&self) -> String {
        let core = self.core.read();
        core.get_balancer_stable_identity(self.pool_id)
            .map(|i| address_utils::address_to_checksum_string(&i.vault))
            .unwrap_or_default()
    }

    /// Balancer V2 stable pool ID (32-byte hex, ``0x``-prefixed). Empty string
    /// if not a Balancer stable pool.
    #[getter]
    fn balancer_stable_pool_id_hex(&self) -> String {
        let core = self.core.read();
        match core.get_balancer_stable_identity(self.pool_id) {
            Some(i) => format!("0x{}", bytes_to_hex(&i.pool_id)),
            None => String::new(),
        }
    }

    /// Balancer stable token addresses (EIP-55 checksummed hex). Empty list
    /// if not a Balancer stable pool.
    #[getter]
    fn balancer_stable_token_addresses(&self) -> Vec<String> {
        let core = self.core.read();
        match core.get_balancer_stable_identity(self.pool_id) {
            Some(i) => i
                .tokens
                .iter()
                .map(address_utils::address_to_checksum_string)
                .collect(),
            None => Vec::new(),
        }
    }

    /// Balancer stable ``PyErc20Token`` companions (one per token, including
    /// BPT for Composable). Includes only tokens registered in this pool's
    /// Bot; unregistered tokens are skipped (the caller pre-registers them
    /// via ``Bot.register_token``). ``None`` if not a Balancer stable pool.
    fn get_balancer_stable_tokens(&self) -> Option<Vec<PyErc20Token>> {
        let core = self.core.read();
        let id = core.get_balancer_stable_identity(self.pool_id)?;
        let mut out = Vec::with_capacity(id.tokens.len());
        for token_addr in &id.tokens {
            if !core.has_token(token_addr) {
                return None;
            }
            out.push(PyErc20Token::new(Arc::clone(&self.core), *token_addr));
        }
        Some(out)
    }

    /// Balancer stable scaling factors (one ``U256`` per token,
    /// rate-multiplied). Empty list if not a Balancer stable pool.
    #[getter]
    fn balancer_stable_scaling_factors(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let sfs: Vec<alloy::primitives::U256> = {
            let core = self.core.read();
            match core.get_balancer_stable_identity(self.pool_id) {
                Some(i) => i.scaling_factors.clone(),
                None => Vec::new(),
            }
        };
        let py_sfs: Vec<Py<PyAny>> = sfs
            .iter()
            .map(|b| crate::conversion::alloy::u256_to_py(py, b).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        Ok(pyo3::types::PyList::new(py, py_sfs)?.into_any().unbind())
    }

    /// Balancer stable swap fee (fraction of `FEE_DENOMINATOR=1e18`). 0 if
    /// not a Balancer stable pool.
    #[getter]
    fn balancer_stable_swap_fee(&self) -> u128 {
        let core = self.core.read();
        core.get_balancer_stable_identity(self.pool_id)
            .map_or(0, |i| i.swap_fee)
    }

    /// Whether the stored Balancer stable rate provider is static (performs
    /// no I/O). ``True`` when no provider was registered (the static
    /// ``1e18`` fallback). Drives the Python companion's
    /// ``requires_io_at_calculation_time`` / ``_should_warn_stale_rates``
    /// flags. ``False`` if not a Balancer stable pool — a dynamic provider
    /// is the more conservative default.
    #[getter]
    fn balancer_stable_rate_provider_is_static(&self) -> bool {
        let core = self.core.read();
        match core.get_balancer_stable_pool(self.pool_id) {
            Some(s) => s.rate_provider.as_ref().is_none_or(|p| p.is_static()),
            None => false,
        }
    }

    /// Fetch rates from the stored Balancer stable rate provider at
    /// ``block_identifier`` (``None`` ⇔ latest). Returns the static
    /// ``1e18`` fallback (one per token) when no provider was registered.
    /// Returns ``None`` if not a Balancer stable pool.
    ///
    /// Raises:
    ///     `ValueError`: If the dynamic provider fetch failed.
    fn fetch_balancer_stable_rates(
        &self,
        block_identifier: Option<u64>,
    ) -> PyResult<Option<Vec<u128>>> {
        let provider = {
            let core = self.core.read();
            let Some(s) = core.get_balancer_stable_pool(self.pool_id) else {
                return Ok(None);
            };
            s.rate_provider.clone()
        };
        let Some(provider) = provider else {
            // Static 1e18 fallback — one per token.
            let n = self.n_balancer_stable_tokens();
            return Ok(Some(vec![1_000_000_000_000_000_000u128; n]));
        };
        let rates = provider
            .get_rates(block_identifier)
            .map_err(|_e| pyo3::exceptions::PyValueError::new_err("rate provider fetch failed"))?;
        let out: Vec<u128> = rates.into_iter().map(|r| r.to::<u128>()).collect();
        Ok(Some(out))
    }

    /// Apply a Balancer stable `external_update` (new balances from a Vault
    /// `PoolBalanceChanged` event).
    ///
    /// Journals the prior balances then lands the new balances +
    /// `update_block`. Silent no-op (`False`) if this `pool_id` is not
    /// registered as a Balancer stable pool (so a companion built for a
    /// different family doesn't corrupt its state).
    #[pyo3(signature = (balances, block_number))]
    fn apply_balancer_stable_balance_update(
        &self,
        balances: &Bound<'_, PyList>,
        block_number: u64,
    ) -> PyResult<bool> {
        let bal: Vec<alloy::primitives::U256> = balances
            .iter()
            .map(|item| crate::conversion::alloy::extract_python_u256(&item))
            .collect::<PyResult<_>>()?;
        Ok(self
            .core
            .write()
            .apply_balance_update_by_pool_id(self.pool_id, bal, block_number)
            .is_some())
    }
}

/// Build the Python 5-tuple `(amount0, amount1, sqrt_price_x96, liquidity,
/// tick)` returned by the swap-sim `PyO3` seams.
fn build_swap_outcome_tuple(
    py: Python<'_>,
    outcome: &degenbot_bot::bot_core::V3SwapOutcome,
) -> PyResult<Option<Py<PyAny>>> {
    let tuple = pyo3::types::PyTuple::new(
        py,
        [
            crate::conversion::alloy::u256_to_py(py, &outcome.amount0)?.unbind(),
            crate::conversion::alloy::u256_to_py(py, &outcome.amount1)?.unbind(),
            crate::conversion::alloy::u256_to_py(py, &outcome.sqrt_price_x96)?.unbind(),
            outcome.liquidity.into_pyobject(py)?.into_any().unbind(),
            outcome.tick.into_pyobject(py)?.into_any().unbind(),
        ],
    )?;
    Ok(Some(tuple.into_any().unbind()))
}

/// Convert a Python `{tick: (liquidity_gross, liquidity_net, block)}` dict into
/// the Rust `HashMap<i32, TickInfo>` (same shape as `register_*_pool`'s
/// `tick_data`, mirroring `PyLiquidityPool.update_tick_data`).
fn extract_tick_data(
    dict: &Bound<'_, PyAny>,
) -> PyResult<std::collections::HashMap<i32, degenbot_bot::bot_core::TickInfo>> {
    let parsed: std::collections::HashMap<i32, (u128, i128, u64)> =
        dict.extract().map_err(|_| {
            pyo3::exceptions::PyTypeError::new_err(
                "tick_data must be {tick: (liquidity_gross, liquidity_net, block)}",
            )
        })?;
    Ok(parsed
        .into_iter()
        .map(|(tick, (gross, net, blk))| {
            (
                tick,
                degenbot_bot::bot_core::TickInfo {
                    liquidity_gross: alloy::primitives::U128::from(gross),
                    liquidity_net: alloy::primitives::I256::try_from(net)
                        .unwrap_or(alloy::primitives::I256::ZERO),
                    block: blk,
                },
            )
        })
        .collect())
}

// ═══════════════════════════════════════════════════════════════════════════
// Prototype structural `Pool` handle (`PyPool`) — all families.
// ═══════════════════════════════════════════════════════════════════════════

/// Thin Python handle to a pool, exposing a structural (not identity-based)
/// interface that mirrors the Rust [`degenbot_pools::Pool`] handle.
#[pyclass(skip_from_py_object, module = "degenbot._ffi")]
pub struct PyPool {
    core: Arc<parking_lot::RwLock<BotState>>,
    pool_id: u64,
}

impl PyPool {
    pub(crate) const fn new(core: Arc<parking_lot::RwLock<BotState>>, pool_id: u64) -> Self {
        Self { core, pool_id }
    }

    fn with_pool<T>(&self, f: impl FnOnce(degenbot_pools::Pool<'_>) -> T) -> T {
        let core = self.core.read();
        let entry = core
            .pool_entry(self.pool_id)
            .expect("PyPool references a registered pool");
        f(degenbot_pools::Pool::new(entry))
    }
}

#[pymethods]
impl PyPool {
    /// Structural family: one of ``"reserve_pair"``, ``"concentrated_liquidity"``,
    /// ``"balance_vector"``.
    fn structure(&self) -> String {
        self.with_pool(|pool| match pool.structure() {
            degenbot_pools::Structure::ReservePair => "reserve_pair".to_string(),
            degenbot_pools::Structure::ConcentratedLiquidity => {
                "concentrated_liquidity".to_string()
            }
            degenbot_pools::Structure::BalanceVector => "balance_vector".to_string(),
        })
    }

    /// Identity as a simple tuple ``(family, variant)``.
    ///
    /// * reserve-pair: ``("reserve_pair", "uniswap_v2" | "aerodrome_v2_stable" | "aerodrome_v2_volatile")``
    /// * concentrated-liquidity: ``("concentrated_liquidity", "uniswap_v3" | "uniswap_v4")``
    /// * balance-vector: ``("balance_vector", "curve" | "balancer_weighted" | "balancer_stable")``
    fn identity(&self) -> (String, Option<String>) {
        self.with_pool(|pool| match pool.identity() {
            degenbot_pools::Identity::ReservePair { variant } => (
                "reserve_pair".to_string(),
                Some(match variant {
                    degenbot_pools::ReservePairVariant::UniswapV2 => "uniswap_v2".to_string(),
                    degenbot_pools::ReservePairVariant::AerodromeV2 { stable } => {
                        format!(
                            "aerodrome_v2_{}",
                            if stable { "stable" } else { "volatile" }
                        )
                    }
                }),
            ),
            degenbot_pools::Identity::ConcentratedLiquidity { variant } => (
                "concentrated_liquidity".to_string(),
                Some(match variant {
                    degenbot_pools::ConcentratedLiquidityVariant::UniswapV3 => {
                        "uniswap_v3".to_string()
                    }
                    degenbot_pools::ConcentratedLiquidityVariant::UniswapV4 => {
                        "uniswap_v4".to_string()
                    }
                }),
            ),
            degenbot_pools::Identity::BalanceVector { variant } => (
                "balance_vector".to_string(),
                Some(match variant {
                    degenbot_pools::BalanceVectorVariant::Curve => "curve".to_string(),
                    degenbot_pools::BalanceVectorVariant::BalancerWeighted => {
                        "balancer_weighted".to_string()
                    }
                    degenbot_pools::BalanceVectorVariant::BalancerStable => {
                        "balancer_stable".to_string()
                    }
                }),
            ),
        })
    }

    /// Reserve-pair structural view. Raises ``ValueError`` for non-reserve-pair pools.
    fn reserve_pair(&self) -> PyResult<PyReservePairView> {
        self.with_pool(|pool| match pool.reserve_pair() {
            Some(view) => Ok(PyReservePairView {
                token0: address_utils::address_to_checksum_string(&view.token0()),
                token1: address_utils::address_to_checksum_string(&view.token1()),
                reserve0: view.reserve0(),
                reserve1: view.reserve1(),
            }),
            None => Err(pyo3::exceptions::PyValueError::new_err(
                "pool is not a reserve-pair pool",
            )),
        })
    }

    /// Concentrated-liquidity structural view. Raises ``ValueError`` for non-CL pools.
    fn concentrated_liquidity(&self) -> PyResult<PyConcentratedLiquidityView> {
        self.with_pool(|pool| match pool.concentrated_liquidity() {
            Some(view) => Ok(PyConcentratedLiquidityView {
                token0: address_utils::address_to_checksum_string(&view.token0()),
                token1: address_utils::address_to_checksum_string(&view.token1()),
                fee: view.fee(),
                tick_spacing: view.tick_spacing(),
                sqrt_price_x96: view.sqrt_price_x96(),
                liquidity: view.liquidity(),
                tick: view.tick(),
            }),
            None => Err(pyo3::exceptions::PyValueError::new_err(
                "pool is not a concentrated-liquidity pool",
            )),
        })
    }

    /// Balance-vector structural view. Raises ``ValueError`` for non-balance-vector pools.
    fn balance_vector(&self) -> PyResult<PyBalanceVectorView> {
        self.with_pool(|pool| match pool.balance_vector() {
            Some(view) => Ok(PyBalanceVectorView {
                tokens: view
                    .tokens()
                    .iter()
                    .map(address_utils::address_to_checksum_string)
                    .collect(),
                balances: view.balances().to_vec(),
            }),
            None => Err(pyo3::exceptions::PyValueError::new_err(
                "pool is not a balance-vector pool",
            )),
        })
    }

    /// Exact-input swap: return output amount, or ``None`` if not computable.
    fn calculate_tokens_out(
        &self,
        py: Python<'_>,
        zero_for_one: bool,
        amount_in: &Bound<'_, PyAny>,
    ) -> PyResult<Option<Py<PyAny>>> {
        let amount_in = crate::conversion::alloy::extract_python_u256(amount_in)?;
        let out = self.with_pool(|pool| pool.calculate_tokens_out(zero_for_one, amount_in));
        Ok(out.map(|v| {
            crate::conversion::alloy::u256_to_py(py, &v)
                .unwrap()
                .unbind()
        }))
    }
}

/// Read-only reserve-pair view exposed to Python.
#[pyclass(module = "degenbot._ffi")]
pub struct PyReservePairView {
    token0: String,
    token1: String,
    reserve0: U256,
    reserve1: U256,
}

#[pymethods]
impl PyReservePairView {
    #[getter]
    fn token0(&self) -> String {
        self.token0.clone()
    }

    #[getter]
    fn token1(&self) -> String {
        self.token1.clone()
    }

    #[getter]
    fn reserve0(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        crate::conversion::alloy::u256_to_py(py, &self.reserve0).map(pyo3::Bound::unbind)
    }

    #[getter]
    fn reserve1(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        crate::conversion::alloy::u256_to_py(py, &self.reserve1).map(pyo3::Bound::unbind)
    }
}

/// Read-only concentrated-liquidity view exposed to Python.
#[pyclass(module = "degenbot._ffi")]
pub struct PyConcentratedLiquidityView {
    token0: String,
    token1: String,
    fee: u32,
    tick_spacing: i32,
    sqrt_price_x96: U256,
    liquidity: u128,
    tick: i32,
}

#[pymethods]
impl PyConcentratedLiquidityView {
    #[getter]
    fn token0(&self) -> String {
        self.token0.clone()
    }

    #[getter]
    fn token1(&self) -> String {
        self.token1.clone()
    }

    #[getter]
    fn fee(&self) -> u32 {
        self.fee
    }

    #[getter]
    fn tick_spacing(&self) -> i32 {
        self.tick_spacing
    }

    #[getter]
    fn sqrt_price_x96(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        crate::conversion::alloy::u256_to_py(py, &self.sqrt_price_x96).map(pyo3::Bound::unbind)
    }

    #[getter]
    fn liquidity(&self) -> u128 {
        self.liquidity
    }

    #[getter]
    fn tick(&self) -> i32 {
        self.tick
    }
}

/// Read-only balance-vector view exposed to Python.
#[pyclass(module = "degenbot._ffi")]
pub struct PyBalanceVectorView {
    tokens: Vec<String>,
    balances: Vec<U256>,
}

#[pymethods]
impl PyBalanceVectorView {
    #[getter]
    fn tokens(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        Ok(pyo3::types::PyList::new(py, self.tokens.clone())?
            .into_any()
            .unbind())
    }

    #[getter]
    fn balances(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let py_vals: Vec<Py<PyAny>> = self
            .balances
            .iter()
            .map(|b| crate::conversion::alloy::u256_to_py(py, b).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        Ok(pyo3::types::PyList::new(py, py_vals)?.into_any().unbind())
    }

    #[getter]
    fn n_tokens(&self) -> usize {
        self.tokens.len()
    }
}
