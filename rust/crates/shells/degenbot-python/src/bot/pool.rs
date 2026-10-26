//! `PyLiquidityPool` — thin Python handle over a `pool_id` key into `BotState`.
//!
//! Shares the same `Arc<StateLock<BotState>>` as the owning `PyBot` (one
//! Rust-owned `BotState`, many thin Python handles). Part of the Polars-inspired
//! three-layer topology — see `docs/adr/ADR-005-polars-inspired-three-layer-architecture.md`.
//!
//! Owns no state — property reads and calculation calls cross `PyO3` on every
//! access, locking the shared `BotState` for reading.

use crate::bot::token::PyErc20Token;
use crate::prelude::*;
use alloy::primitives::{I256, U256};
use degenbot_pools::sim::registry::PoolEntry;
use hashbrown::HashMap;
use std::sync::Arc;

use pyo3::types::{PyDict, PyList, PySet, PyTuple};

use crate::bot::errmap::{
    encode_swap_failed, exact_output_family_gap, hooked_pool_approximation, pool_unregistered,
    swap_family_gap, swap_math_in_overflow, swap_math_overflow, swap_unknown_pool,
};
use degenbot_bot::bot_core::pool_ops::{self, SwapOpError};
use degenbot_bot::bot_core::refresh::{RefreshError, RefreshOutcome};
use degenbot_bot::bot_core::TickInfo;
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::swap_simulation::{
    Caveats, OverrideOutcome, OverrideState, ReservePairSwapOutcome, SwapOutcome, SwapRead,
    SwapRequest,
};

use super::narrow_reserve;
use degenbot_substrate::{BotState, CurveInputsError};

/// `PyO3` adapter wrapping a Python fetch-word callable as a
/// [`TickWordSource`].
/// The callable is `fetcher(word: int, block: int) ->
/// dict[int, tuple[int, int, int]] | None`. It must RETURN the fetched tick data
/// (writing it back would re-enter the state lock the caller holds) — the Rust
/// loop merges the returned [`FetchedTickWord`] itself. See [`PyLiquidityPool::calculate_tokens_out_with_fetch`].
#[derive(Debug)]
struct PyTickWordSource {
    callback: pyo3::Py<pyo3::PyAny>,
}

/// Wrap a Python callable as a stored `Arc<dyn TickWordSource>` for
/// registration-time storage on `V3PoolState`/`V4PoolState` (ADR-006 I/O trait
/// object). The callable signature is `fetcher(word, block) -> dict | None`
/// (same as the per-call path this replaces).
pub(crate) fn make_tick_fetcher(
    callback: pyo3::Py<pyo3::PyAny>,
) -> std::sync::Arc<dyn degenbot_pools::sim::tick_fetch::TickWordSource> {
    std::sync::Arc::new(PyTickWordSource { callback })
}

/// Construct a Chain-arm tick-word source from a `PyBotIo`'s native alloy
/// provider, if present (Option B: route the Chain arm
/// through the pure-Rust [`degenbot_rpc::AlloyTickWordSource`]).
/// Returns `None` when the `PyBotIo` has no native alloy provider (legacy
/// Python test doubles) → the caller passes `chain=None` to the assemble helper,
/// preserving the current (no-Chain-arm) behavior.
/// The returned `Arc<dyn TickWordSource>` is `Send + Sync` + holds no GIL
/// state — the full choreography (bitmap decode, bit enumeration, per-tick
/// `eth_call`) runs in pure Rust under `py.detach`, no per-RPC GIL re-entry
/// (the architectural end state: Rust is the engine; Python is a driver
/// shell, not a co-implementation).
#[must_use]
pub(crate) fn make_tick_bootstrap_rpc(
    io: &crate::bot::py_bot_io::PyBotIo,
) -> Option<std::sync::Arc<dyn degenbot_pools::sim::tick_fetch::TickWordSource>> {
    io.alloy_provider().map(|provider| {
        std::sync::Arc::new(degenbot_rpc::AlloyTickWordSource::new(provider))
            as std::sync::Arc<dyn degenbot_pools::sim::tick_fetch::TickWordSource>
    })
}

impl degenbot_pools::sim::tick_fetch::TickWordSource for PyTickWordSource {
    fn fetch_tick_word(
        &self,
        _key: &degenbot_pools::sim::tick_fetch::TickWordKey,
        word: i32,
        block: u64,
    ) -> Result<
        Option<degenbot_pools::sim::tick_fetch::FetchedTickWord>,
        degenbot_pools::sim::tick_fetch::TickWordFetchError,
    > {
        use degenbot_pools::sim::tick_fetch::{FetchedTickWord, TickWordFetchError};
        Python::attach(|py| {
            let result = self
                .callback
                .call1(py, (word, block))
                .map_err(|_| TickWordFetchError::FetchFailed)?;
            // `None` or `{}` → empty word (known, no initialized ticks).
            let bound = result.bind(py);
            if bound.is_none() {
                return Ok(Some(FetchedTickWord {
                    bitmap: None,
                    word,
                    ticks: HashMap::new(),
                }));
            }
            // The seed boundary owns the arity, the `U128` narrowing, and the
            // malformed-container error.
            let ticks = TickData::from_py(bound).map_err(|_| TickWordFetchError::FetchFailed)?;
            Ok(Some(FetchedTickWord {
                bitmap: None,
                word,
                ticks,
            }))
        })
    }
}

// ---------------------------------------------------------------------------
// Balancer rate-provider Py adapter.
// ---------------------------------------------------------------------------

/// `PyO3` adapter wrapping a Python rate-provider callable (or any object
/// exposing `get_rates(block_identifier) -> tuple[int, ...]`) as a stored
/// `Arc<dyn BalancerRateProvider>` for registration-time storage on
/// `BalancerStablePoolState`.
/// The callable signature is `get_rates(block_identifier: int | None) ->
/// tuple[int, ...]` (one rate per token, `1e18` for tokens without a rate
/// provider). Mirrors [`PyTickWordSource`]'s GIL re-entry discipline: the
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
) -> Arc<dyn degenbot_pools::sim::rate_provider::BalancerRateProvider> {
    Arc::new(PyBalancerRateProvider { callback })
}

impl degenbot_pools::sim::rate_provider::BalancerRateProvider for PyBalancerRateProvider {
    fn get_rates(
        &self,
        block_identifier: Option<u64>,
    ) -> Result<Vec<alloy::primitives::U256>, degenbot_pools::sim::rate_provider::RateProviderError>
    {
        use degenbot_pools::sim::rate_provider::RateProviderError;
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
// Curve data-provider Py adapter (ADR-005 I/O trait object).
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
) -> Arc<dyn degenbot_pools::sim::curve_data_provider::CurveDataProvider> {
    Arc::new(PyCurveDataProvider { callback })
}

impl degenbot_pools::sim::curve_data_provider::CurveDataProvider for PyCurveDataProvider {
    fn block_number(
        &self,
    ) -> Result<u64, degenbot_pools::sim::curve_data_provider::CurveDataProviderError> {
        use degenbot_pools::sim::curve_data_provider::CurveDataProviderError;
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
    ) -> Result<u64, degenbot_pools::sim::curve_data_provider::CurveDataProviderError> {
        use degenbot_pools::sim::curve_data_provider::CurveDataProviderError;
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
    ) -> Result<
        alloy::primitives::U256,
        degenbot_pools::sim::curve_data_provider::CurveDataProviderError,
    > {
        use degenbot_pools::sim::curve_data_provider::CurveDataProviderError;
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
    ) -> Result<
        alloy::primitives::U256,
        degenbot_pools::sim::curve_data_provider::CurveDataProviderError,
    > {
        use degenbot_pools::sim::curve_data_provider::CurveDataProviderError;
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
        degenbot_pools::sim::curve_data_provider::CurveDataProviderError,
    > {
        self.read_u256_vec("lending_rates", block_number)
    }

    fn invariant_d(
        &self,
        block_number: u64,
    ) -> Result<
        alloy::primitives::U256,
        degenbot_pools::sim::curve_data_provider::CurveDataProviderError,
    > {
        // The Python provider's method name is `d`; only the Rust trait method
        // carries the referent.
        self.read_u256("d", block_number)
    }

    fn gamma(
        &self,
        block_number: u64,
    ) -> Result<
        alloy::primitives::U256,
        degenbot_pools::sim::curve_data_provider::CurveDataProviderError,
    > {
        self.read_u256("gamma", block_number)
    }

    fn price_scale(
        &self,
        block_number: u64,
    ) -> Result<
        Vec<alloy::primitives::U256>,
        degenbot_pools::sim::curve_data_provider::CurveDataProviderError,
    > {
        self.read_u256_vec("price_scale", block_number)
    }

    fn admin_balances(
        &self,
        block_number: u64,
    ) -> Result<
        Vec<alloy::primitives::U256>,
        degenbot_pools::sim::curve_data_provider::CurveDataProviderError,
    > {
        self.read_u256_vec("admin_balances", block_number)
    }

    fn redemption_price(
        &self,
        block_number: u64,
    ) -> Result<
        alloy::primitives::U256,
        degenbot_pools::sim::curve_data_provider::CurveDataProviderError,
    > {
        self.read_u256("redemption_price", block_number)
    }

    fn base_cache_updated(
        &self,
        block_number: u64,
    ) -> Result<u64, degenbot_pools::sim::curve_data_provider::CurveDataProviderError> {
        use degenbot_pools::sim::curve_data_provider::CurveDataProviderError;
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
    ) -> Result<
        alloy::primitives::U256,
        degenbot_pools::sim::curve_data_provider::CurveDataProviderError,
    > {
        self.read_u256("base_virtual_price", block_number)
    }

    fn virtual_price(
        &self,
        block_number: u64,
    ) -> Result<
        alloy::primitives::U256,
        degenbot_pools::sim::curve_data_provider::CurveDataProviderError,
    > {
        self.read_u256("virtual_price", block_number)
    }
}

impl PyCurveDataProvider {
    /// Call a `(&self, block_number) -> int` Python method → `U256`.
    fn read_u256(
        &self,
        method: &str,
        block_number: u64,
    ) -> Result<
        alloy::primitives::U256,
        degenbot_pools::sim::curve_data_provider::CurveDataProviderError,
    > {
        use degenbot_pools::sim::curve_data_provider::CurveDataProviderError;
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
        degenbot_pools::sim::curve_data_provider::CurveDataProviderError,
    > {
        use degenbot_pools::sim::curve_data_provider::CurveDataProviderError;
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

/// Build a `degenbot.exceptions.arbitrage.NoLiquidity(message=..)` for a
/// core-reported zero-balance metapool refusal. The class stays owned by the
/// Python exceptions module; an import failure means a broken install, so it
/// surfaces directly.
fn curve_no_liquidity_error(py: Python<'_>, message: &str) -> PyErr {
    let exc_type = py
        .import("degenbot.exceptions.arbitrage")
        .and_then(|module| module.getattr("NoLiquidity"));
    match exc_type {
        Ok(exc_type) => {
            let kwargs = PyDict::new(py);
            let _ = kwargs.set_item("message", message);
            match exc_type.call((), Some(&kwargs)) {
                Ok(exc) => PyErr::from_value(exc),
                Err(err) => err,
            }
        }
        Err(err) => err,
    }
}

/// A thin Python handle to a pool registered in `BotState`.
/// Does not own any state — all data lives in Rust inside `BotState`.
#[pyclass(name = "Pool", module = "degenbot._ffi")]
pub struct PyLiquidityPool {
    core: Arc<StateLock<BotState>>,
    pool_id: u64,
    chain_id: u64,
}

impl PyLiquidityPool {
    /// Probe (mechanical lock-freedom invariant, pair-review
    /// condition 1): the caller thread holds the GIL; the `BotState` WRITE is
    /// required free. `try_write` is instant and non-blocking - safe at any
    /// depth (it never parks).
    #[must_use]
    pub fn state_write_is_free(&self) -> bool {
        self.core
            .try_write_at(degenbot_substrate::state_lock::LockSite::Python)
            .is_some()
    }

    /// Create a new thin pool handle.
    pub(crate) const fn new(core: Arc<StateLock<BotState>>, pool_id: u64, chain_id: u64) -> Self {
        Self {
            core,
            pool_id,
            chain_id,
        }
    }

    /// Sanctioned `BotState` read access for pymethod code (GIL/`BotState`
    /// inversion class, incidents 2026-08-20/21): the guard is acquired
    /// INSIDE `py.detach`. Same invariant contract as `PyBot::with_state` —
    /// see the doc comment there.
    fn with_state<T>(&self, py: Python<'_>, f: impl FnOnce(&BotState) -> T + Send) -> T
    where
        T: Send,
    {
        py.detach(|| {
            // T1-scan-exempt: sanctioned accessor — guard inside py.detach by definition.
            let guard = self
                .core
                .read_at(degenbot_substrate::state_lock::LockSite::Python);
            f(&guard)
        })
    }

    /// Sanctioned `BotState` write access — see [`Self::with_state`].
    fn with_state_mut<T>(&self, py: Python<'_>, f: impl FnOnce(&mut BotState) -> T + Send) -> T
    where
        T: Send,
    {
        py.detach(move || {
            // T1-scan-exempt: sanctioned accessor — guard inside py.detach by definition.
            let mut guard = self
                .core
                .write_at(degenbot_substrate::state_lock::LockSite::Python);
            f(&mut guard)
        })
    }

    /// The registered family tag for this handle's pool id.
    /// Raises `ValueError` when the id is not registered: a handle is built
    /// from a registered pool, so an unknown id is a family gap, never an
    /// `""` sentinel a caller could mistake for a missing field.
    fn family_of(&self, py: Python<'_>) -> PyResult<&'static str> {
        let core = self.core.clone();
        let family = py.detach(move || pool_ops::pool_family(&core, self.pool_id));
        family.ok_or_else(|| pool_unregistered(self.pool_id))
    }

    /// Clone-out the stored `CurveDataProvider` (if any) for this handle's
    /// Curve state, releasing the read guard before a (potentially
    /// re-entrant) provider call.
    fn curve_provider(
        &self,
        py: Python<'_>,
    ) -> Option<std::sync::Arc<dyn degenbot_pools::sim::curve_data_provider::CurveDataProvider>>
    {
        let core = self.core.clone();
        py.detach(move || pool_ops::curve_data_provider(&core, self.pool_id))
    }

    /// Read a `Vec<U256>` from the stored Curve data provider via `f`,
    /// converting to a Python list. Empty list ⇔ no provider / error.
    fn read_provider_vec(
        &self,
        py: Python<'_>,
        f: impl Fn(
            &std::sync::Arc<dyn degenbot_pools::sim::curve_data_provider::CurveDataProvider>,
        ) -> Result<
            Vec<alloy::primitives::U256>,
            degenbot_pools::sim::curve_data_provider::CurveDataProviderError,
        >,
    ) -> PyResult<Py<PyAny>> {
        let Some(provider) = self.curve_provider(py) else {
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
            &std::sync::Arc<dyn degenbot_pools::sim::curve_data_provider::CurveDataProvider>,
        ) -> Result<
            alloy::primitives::U256,
            degenbot_pools::sim::curve_data_provider::CurveDataProviderError,
        >,
    ) -> PyResult<Option<Py<PyAny>>> {
        let Some(provider) = self.curve_provider(py) else {
            return Ok(None);
        };
        match f(&provider) {
            Ok(v) => Ok(Some(crate::conversion::alloy::u256_to_py(py, &v)?.unbind())),
            Err(_) => Ok(None),
        }
    }

    /// Shared override-sim inner: marshall the hypothetical state for the pool's
    /// REGISTERED family, then drive the gate's override door. The dispatch IS
    /// the registration's, so a wrong-family override cannot be constructed
    /// here — the state variant names the family.
    fn sim_override_inner(
        &self,
        py: Python<'_>,
        zero_for_one: bool,
        amount_specified: I256,
        block: u64,
        override_state: &Bound<'_, PyAny>,
        sqrt_price_limit_x96: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Option<OverrideOutcome>> {
        let sqrt_price_limit = match sqrt_price_limit_x96 {
            Some(v) if !v.is_none() => Some(crate::conversion::alloy::extract_python_u256(v)?),
            _ => None,
        };
        let state = self.override_state_of(py, override_state)?;
        let mut over = degenbot_substrate::swap_simulation::OverrideSwap {
            pool_id: self.pool_id,
            request: SwapRequest {
                zero_for_one,
                amount_specified,
                sqrt_price_limit,
            },
            state,
        };
        let core = self.core.clone();
        Ok(py.detach(move || pool_ops::simulate_override_with_fetch(&core, &mut over, block)))
    }

    /// Read the pool's hypothetical state off the Python per-family pool-state
    /// object — dispatched by the pool's REGISTERED family. Immutable params
    /// (fees, tick spacing, Camelot decimal scales) are NEVER sent: the core
    /// borrows them from the registration.
    fn override_state_of(
        &self,
        py: Python<'_>,
        override_state: &Bound<'_, PyAny>,
    ) -> PyResult<OverrideState> {
        let to_u256 = crate::conversion::alloy::extract_python_u256;
        Ok(match self.family_of(py)? {
            // Reserve-pair: the caller's view of the pair, narrowed to the
            // on-chain `uint112` width at this ingestion seam.
            "v2" | "aerodrome-v2" => OverrideState::ReservePair {
                reserves_token0: narrow_reserve(
                    to_u256(&override_state.getattr("reserves_token0")?)?,
                    "reserves_token0",
                )?,
                reserves_token1: narrow_reserve(
                    to_u256(&override_state.getattr("reserves_token1")?)?,
                    "reserves_token1",
                )?,
            },
            // Concentrated liquidity: the hypothetical scalars + tick rows.
            "v3" | "v4" => {
                let liquidity = u128::try_from(to_u256(&override_state.getattr("liquidity")?)?)
                    .map_err(|_| {
                        pyo3::exceptions::PyOverflowError::new_err(
                            "override_liquidity must fit in u128",
                        )
                    })?;
                let tick =
                    i32::try_from(override_state.getattr("tick")?.extract::<i64>().map_err(
                        |_| pyo3::exceptions::PyTypeError::new_err("override_tick must be an int"),
                    )?)
                    .map_err(|_| {
                        pyo3::exceptions::PyOverflowError::new_err("override_tick must fit in i32")
                    })?;
                OverrideState::ConcentratedLiquidity {
                    sqrt_price_x96: to_u256(&override_state.getattr("sqrt_price_x96")?)?,
                    liquidity,
                    tick,
                    tick_data: TickData::from_py_liquidity_rows(
                        &override_state.getattr("tick_data")?,
                    )?,
                }
            }
            // Balance vector: the caller's view of the balances. Those
            // families answer override reads through their own pair doors
            // (`curve_get_dy` / `calculate_tokens_out_for_pair`), which carry
            // the balances as data.
            "curve" | "balancer-weighted" | "balancer-stable" => {
                let balances = override_state.getattr("balances")?;
                let balances = balances.cast::<pyo3::types::PyList>().map_err(|_| {
                    pyo3::exceptions::PyTypeError::new_err(
                        "override_state.balances must be a list[int]",
                    )
                })?;
                OverrideState::BalanceVector {
                    balances: crate::bot::extract_u256_list(balances)?,
                }
            }
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "unsupported pool family {other}"
                )));
            }
        })
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
    #[expect(clippy::missing_const_for_fn)]
    fn pool_id(&self) -> u64 {
        self.pool_id
    }

    /// Structural family discriminator.
    fn structure(&self, py: Python<'_>) -> PyResult<String> {
        Ok(match self.family_of(py)? {
            "v2" | "aerodrome-v2" => "reserve_pair",
            "v3" | "v4" => "concentrated_liquidity",
            "curve" | "balancer-weighted" | "balancer-stable" => "balance_vector",
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "unsupported pool structure {other}"
                )));
            }
        }
        .to_string())
    }

    /// Structural identity and concrete variant discriminator.
    fn identity(&self, py: Python<'_>) -> PyResult<(String, Option<String>)> {
        let family = self.family_of(py)?;
        Ok(match family {
            "v2" => ("reserve_pair".to_string(), Some("uniswap_v2".to_string())),
            "aerodrome-v2" => (
                "reserve_pair".to_string(),
                Some(if self.aerodrome_stable(py) {
                    "aerodrome_v2_stable".to_string()
                } else {
                    "aerodrome_v2_volatile".to_string()
                }),
            ),
            "v3" => (
                "concentrated_liquidity".to_string(),
                Some("uniswap_v3".to_string()),
            ),
            "v4" => (
                "concentrated_liquidity".to_string(),
                Some("uniswap_v4".to_string()),
            ),
            "curve" => ("balance_vector".to_string(), Some("curve".to_string())),
            "balancer-weighted" => (
                "balance_vector".to_string(),
                Some("balancer_weighted".to_string()),
            ),
            "balancer-stable" => (
                "balance_vector".to_string(),
                Some("balancer_stable".to_string()),
            ),
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "unsupported pool family {other}"
                )));
            }
        })
    }

    /// Resolved DEX deployment name, or ``None`` for an unknown deployment.
    #[getter]
    fn dex_name(&self, py: Python<'_>) -> Option<String> {
        self.with_state(py, |core| {
            let entry = core.pool_entry(self.pool_id)?;
            let pool = degenbot_pools::Pool::new(entry, self.chain_id);
            match pool.identity() {
                degenbot_pools::Identity::ReservePair { dex, .. }
                | degenbot_pools::Identity::ConcentratedLiquidity { dex, .. }
                | degenbot_pools::Identity::BalanceVector { dex, .. }
                | degenbot_pools::Identity::BinnedLiquidity { dex, .. } => {
                    dex.map(|name| name.as_str().to_string())
                }
            }
        })
    }

    /// Calculate the output token amount for a given input amount.
    /// Surfaces the cdbc03bb on-chain-equivalent revert: when the constant-
    /// product / concentrated liquidity swap math overflows a `uint256` intermediate (mirroring
    /// on-chain `getAmountOut` `SafeMath` revert), this raises `ValueError` so the
    /// Python companion can translate it to a domain `LiquidityPoolError`.
    /// A V3/V4 sparse-map miss is still mapped to 0 — callers needing the miss
    /// surfaced use [`calculate_tokens_out_with_fetch`][Self::calculate_tokens_out_with_fetch].
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
        let core = self.core.clone();
        let result = py.detach(move || {
            pool_ops::calculate_tokens_out(&core, self.pool_id, zero_for_one, amount)
        });
        match result {
            Ok(out) => {
                let bound = crate::conversion::alloy::u256_to_py(py, &out)?;
                Ok(bound.unbind())
            }
            Err(SwapOpError::AmountOverflow) => Err(swap_math_overflow()),
            Err(SwapOpError::UnknownPool { pool_id }) => Err(swap_unknown_pool(pool_id)),
            Err(SwapOpError::UnsupportedFamily { pool_id, family }) => {
                Err(swap_family_gap(pool_id, family))
            }
        }
    }

    /// Calculate the output for an explicit token pair (N-token pools).
    /// The weighted engine's math reads only the in/out pair, so any index
    /// selection of an N-token pool is the same 2-token computation. Surfaced
    /// for the standalone-driver `MultiTokenSwapCalculation` protocol — the
    /// seat engine's hop universe stays the token0/1 pair.
    /// Raises:
    ///     `ValueError`: On out-of-range/equal indices, the on-chain
    ///         `MAX_IN_RATIO` breach, or a uint256 intermediate overflow (the
    ///         same on-chain-parity contracts as [`calculate_tokens_out`]).
    #[pyo3(signature = (index_in, index_out, amount_in, override_balances=None, override_scaling_factors=None))]
    fn calculate_tokens_out_for_pair(
        &self,
        py: Python<'_>,
        index_in: usize,
        index_out: usize,
        amount_in: &Bound<'_, PyAny>,
        override_balances: Option<Vec<Bound<'_, PyAny>>>,
        override_scaling_factors: Option<Vec<Bound<'_, PyAny>>>,
    ) -> PyResult<Py<PyAny>> {
        let to_u256 = |v: &Bound<'_, PyAny>| crate::conversion::alloy::extract_python_u256(v);
        let override_balances = match override_balances {
            Some(list) => {
                let mut parsed = Vec::with_capacity(list.len());
                for item in &list {
                    parsed.push(to_u256(item)?);
                }
                Some(parsed)
            }
            None => None,
        };
        let override_scaling_factors = match override_scaling_factors {
            Some(list) => {
                let mut parsed = Vec::with_capacity(list.len());
                for item in &list {
                    parsed.push(to_u256(item)?);
                }
                Some(parsed)
            }
            None => None,
        };
        let amount = crate::conversion::alloy::extract_python_u256(amount_in)?;
        let core = self.core.clone();
        let outcome = py.detach(move || {
            pool_ops::calculate_tokens_out_for_pair(
                &core,
                self.pool_id,
                index_in,
                index_out,
                amount,
                override_balances.as_deref(),
                override_scaling_factors.as_deref(),
            )
        });
        match outcome {
            Some(out) => {
                let bound = crate::conversion::alloy::u256_to_py(py, &out)?;
                Ok(bound.unbind())
            }
            None => Err(swap_math_overflow()),
        }
    }

    /// Calculate the required input for an explicit token pair (N-token pools).
    /// The standalone-driver companion for the balanced `GIVEN_OUT` protocol arm
    /// (see `calculate_tokens_out_for_pair`): the weighted/stable math reads only
    /// the in/out pair of the registered identity.
    /// Raises:
    ///     `ValueError`: On out-of-range/equal indices, the on-chain `MAX_OUT_RATIO`
    ///         breach, or a uint256 intermediate overflow (the same
    ///         on-chain-parity contracts as [`calculate_tokens_out`]).
    #[pyo3(signature = (index_in, index_out, amount_out, override_balances=None, override_scaling_factors=None))]
    fn calculate_tokens_in_for_pair(
        &self,
        py: Python<'_>,
        index_in: usize,
        index_out: usize,
        amount_out: &Bound<'_, PyAny>,
        override_balances: Option<Vec<Bound<'_, PyAny>>>,
        override_scaling_factors: Option<Vec<Bound<'_, PyAny>>>,
    ) -> PyResult<Py<PyAny>> {
        let override_balances = match override_balances {
            Some(list) => {
                let mut parsed = Vec::with_capacity(list.len());
                for item in &list {
                    parsed.push(crate::conversion::alloy::extract_python_u256(item)?);
                }
                Some(parsed)
            }
            None => None,
        };
        let override_scaling_factors = match override_scaling_factors {
            Some(list) => {
                let mut parsed = Vec::with_capacity(list.len());
                for item in &list {
                    parsed.push(crate::conversion::alloy::extract_python_u256(item)?);
                }
                Some(parsed)
            }
            None => None,
        };
        let amount = crate::conversion::alloy::extract_python_u256(amount_out)?;
        let core = self.core.clone();
        let outcome = py.detach(move || {
            pool_ops::calculate_tokens_in_for_pair(
                &core,
                self.pool_id,
                index_in,
                index_out,
                amount,
                override_balances.as_deref(),
                override_scaling_factors.as_deref(),
            )
        });
        match outcome {
            Some(out) => {
                let bound = crate::conversion::alloy::u256_to_py(py, &out)?;
                Ok(bound.unbind())
            }
            None => Err(swap_math_in_overflow()),
        }
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
        let core = self.core.clone();
        let result = py.detach(move || {
            pool_ops::calculate_tokens_in(&core, self.pool_id, zero_for_one, amount)
        });
        let result = match result {
            Ok(v) => v,
            Err(SwapOpError::AmountOverflow) => return Err(swap_math_overflow()),
            // A registered family with no exact-output path is a typed gap,
            // not the legacy silent-0 contract.
            Err(SwapOpError::UnsupportedFamily { pool_id, family }) => {
                return Err(exact_output_family_gap(
                    "calculate_tokens_in",
                    pool_id,
                    family,
                ));
            }
            // Legacy silent-0: unknown pool, not-computable, and miss-recovery
            // failures keep the zero sentinel (ADR-037).
            Err(SwapOpError::UnknownPool { .. }) => U256::ZERO,
        };
        let bound = crate::conversion::alloy::u256_to_py(py, &result)?;
        Ok(bound.unbind())
    }

    /// Fetch+retry exact-input swap for sparse V3/V4 pools.
    /// Like [`calculate_tokens_out`][Self::calculate_tokens_out], but on a
    /// sparse tick-map miss it calls `fetcher(word, block)` to fetch the
    /// missing word's tick data, merges it, and retries (dedup-protected — a
    /// repeated miss on a word gives up with `0`). Mirrors the Python
    /// companion's `MissingLiquidityData` → `_tick_data_fetcher` loop, now
    /// driven from the Rust calc path.
    /// `fetcher` is a Python callable
    /// `fetcher(word: int, block: int) -> dict[int, tuple[int, int, int]] | None`.
    /// It MUST return the fetched tick data mapping tick →
    /// `(liquidity_gross, liquidity_net, block)`; returning `None` or `{}` marks
    /// the word known with no initialized ticks (an all-zero bitmap word).
    /// The fetcher MUST NOT write back into the pool (the Rust loop merges the
    /// returned data itself) — doing so would re-enter the `BotState` write
    /// lock this call holds and deadlock.
    #[pyo3(signature = (zero_for_one, amount_in, block))]
    fn calculate_tokens_out_with_fetch(
        &self,
        py: Python<'_>,
        zero_for_one: bool,
        amount_in: &Bound<'_, PyAny>,
        block: u64,
    ) -> PyResult<Py<PyAny>> {
        let amount = crate::conversion::alloy::extract_python_u256(amount_in)?;
        let core = self.core.clone();
        let result = py.detach(move || {
            pool_ops::calculate_tokens_out_with_fetch(
                &core,
                self.pool_id,
                zero_for_one,
                amount,
                block,
            )
        });
        match result {
            Ok(out) => {
                let bound = crate::conversion::alloy::u256_to_py(py, &out)?;
                Ok(bound.unbind())
            }
            Err(SwapOpError::AmountOverflow) => Err(swap_math_overflow()),
            // Staging failures keep the legacy zero sentinel.
            Err(_) => {
                let bound = crate::conversion::alloy::u256_to_py(py, &U256::ZERO)?;
                Ok(bound.unbind())
            }
        }
    }

    /// THE swap-simulation seam (ADR-037): one verb behind every "what would
    /// this swap do against current pool state" read. `amount_specified` is
    /// signed user-perspective (negative = exact input, positive = exact
    /// output). Returns a typed read dict — `status` is `computed` (with
    /// signed per-token deltas in the deposited-`+`/sent-`-` convention, the
    /// end state, and `specified_met`); otherwise the typed
    /// `SwapSimulationStatus` member for the refusal (`NotComputable`,
    /// `FetchFailed`, or `FetchExhausted`). Pass `override_state` (this
    /// pool's own pool-state object) to simulate against a hypothetical: the
    /// shell marshalls the state for the pool's REGISTERED family, so a
    /// reserve-pair read carries `reserves_token0` / `reserves_token1`
    /// (POST-swap) where a concentrated-liquidity read carries the end
    /// `sqrt_price_x96` / `liquidity` / `tick`.
    #[pyo3(signature = (zero_for_one, amount_specified, block, sqrt_price_limit_x96=None, override_state=None))]
    fn simulate(
        &self,
        py: Python<'_>,
        zero_for_one: bool,
        amount_specified: &Bound<'_, PyAny>,
        block: u64,
        sqrt_price_limit_x96: Option<&Bound<'_, PyAny>>,
        override_state: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<PySwapRead> {
        let amount_specified = extract_signed_amount(amount_specified)?;
        if let Some(state) = override_state.filter(|s| !s.is_none()) {
            let outcome = self.sim_override_inner(
                py,
                zero_for_one,
                amount_specified,
                block,
                state,
                sqrt_price_limit_x96,
            )?;
            return match outcome {
                Some(OverrideOutcome::ConcentratedLiquidity(o)) => Ok(cl_swap_read(
                    zero_for_one,
                    amount_specified,
                    o.amount0,
                    o.amount1,
                    (o.sqrt_price_x96, o.liquidity, o.tick),
                )),
                Some(OverrideOutcome::ReservePair(o)) => Ok(rp_swap_read(zero_for_one, &o)),
                None => Err(swap_refused_err(py, PySwapRefusalKind::NotComputable, None)),
            };
        }
        let request = SwapRequest {
            zero_for_one,
            amount_specified,
            sqrt_price_limit: match sqrt_price_limit_x96 {
                Some(v) if !v.is_none() => Some(crate::conversion::alloy::extract_python_u256(v)?),
                _ => None,
            },
        };
        let core = self.core.clone();
        let read =
            py.detach(move || pool_ops::simulate_mainline(&core, self.pool_id, &request, block));
        match read {
            SwapRead::Computed(SwapOutcome::V3(o) | SwapOutcome::V4(o)) => {
                if o.caveats.contains(Caveats::HOOKED_POOL) {
                    return Err(hooked_pool_approximation(
                        o.consumed,
                        o.delivered.into_raw(),
                    ));
                }
                let (amount0, amount1) = o.raw_token_amounts(zero_for_one);
                Ok(cl_swap_read(
                    zero_for_one,
                    amount_specified,
                    amount0,
                    amount1,
                    (o.end_sqrt_price_x96, o.end_liquidity, o.end_tick),
                ))
            }
            // The mainline route is concentrated-liquidity-only by contract:
            // a reserve-pair payload here would be the route's bug, never a
            // computable read (the retired flat read answered NotComputable).
            // Both the verdict and the refusal cross as NotComputable.
            SwapRead::Computed(_) | SwapRead::NotComputable => {
                Err(swap_refused_err(py, PySwapRefusalKind::NotComputable, None))
            }
            SwapRead::FetchFailed { word } => Err(swap_refused_err(
                py,
                PySwapRefusalKind::FetchFailed,
                Some(word),
            )),
            SwapRead::FetchExhausted { word } => Err(swap_refused_err(
                py,
                PySwapRefusalKind::FetchExhausted,
                Some(word),
            )),
            SwapRead::UnsupportedFamily { pool_id, family } => {
                Err(swap_family_gap(pool_id, family))
            }
            SwapRead::UnknownPool { pool_id } => Err(swap_unknown_pool(pool_id)),
        }
    }

    #[pyo3(signature = (zero_for_one, amount_out, recipient))]
    fn encode_swap(
        &self,
        py: Python<'_>,
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

        let core = self.core.clone();
        let result = py.detach(move || {
            pool_ops::encode_swap(&core, self.pool_id, zero_for_one, amount, recip)
        });

        match result {
            Ok(Some(call)) => Ok(Some((
                format!("{:#x}", call.to),
                alloy::hex::encode_prefixed(&call.data),
                call.value.to::<u64>(),
            ))),
            // The Python handle's own `encode_swap` resolves its pool id at
            // construction, so an unregistered id keeps the `None` not-found
            // contract.
            Ok(None) => Ok(None),
            Err(e) => Err(encode_swap_failed(&e.to_string())),
        }
    }

    // --- State read getters ---
    // These read the shared `BotState` under a read guard. Immutable identity
    // (token0/token1/factory/fees/address) stays on the Python companion —
    // only mutable state + the reorg journal delegate to Rust.

    // --- V2 identity getters (ADR-005 identity slice) ---
    // Immutable per-pool identity read from V2PoolState (+ the registration
    // descriptor) so the Python companion's `from_handle(py_pool)` can be
    // self-describing — the Polars `_from_pydf` end state. These re-export
    // what V2PoolState already holds; the descriptor (variant/stable_swap/
    // fee_denominator) + resolved DexIdentity preset are new in this slice.

    /// Pool contract address (EIP-55 checksummed hex). Empty string when the
    /// registered family has no address field; raises if the id is not
    /// registered.
    #[getter]
    fn address(&self, py: Python<'_>) -> PyResult<String> {
        let family = self.family_of(py)?;
        Ok(self.with_state(py, |core| {
            let addr = match family {
                "v2" => core
                    .pool_slice::<degenbot_pools::state::v2_state::V2PoolIdentity>(self.pool_id)
                    .map(|i| i.address),
                "v3" => core
                    .pool_slice::<degenbot_pools::state::v3_state::V3PoolIdentity>(self.pool_id)
                    .map(|i| i.address),
                "aerodrome-v2" => core
                    .pool_slice::<degenbot_pools::state::aerodrome_v2_state::AerodromeV2PoolIdentity>(
                        self.pool_id,
                    )
                    .map(|i| i.address),
                "curve" => core
                    .pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(self.pool_id)
                    .map(|i| i.address),
                _ => None,
            };
            addr.map(|a| address_utils::address_to_checksum_string(&a))
                .unwrap_or_default()
        }))
    }

    /// Token0 contract address (EIP-55 checksummed hex). Empty string when the
    /// registered family has no token0 field; raises if the id is not
    /// registered.
    #[getter]
    fn token0_address(&self, py: Python<'_>) -> PyResult<String> {
        let family = self.family_of(py)?;
        Ok(self.with_state(py, |core| {
            let addr = match family {
                "v2" => core
                    .pool_slice::<degenbot_pools::state::v2_state::V2PoolIdentity>(self.pool_id)
                    .map(|i| i.token0),
                "v3" => core
                    .pool_slice::<degenbot_pools::state::v3_state::V3PoolIdentity>(self.pool_id)
                    .map(|i| i.token0),
                "v4" => core
                    .pool_slice::<degenbot_pools::state::v4_state::V4PoolIdentity>(self.pool_id)
                    .map(|i| i.pool_key.currency0),
                "aerodrome-v2" => core
                    .pool_slice::<degenbot_pools::state::aerodrome_v2_state::AerodromeV2PoolIdentity>(
                        self.pool_id,
                    )
                    .map(|i| i.token0),
                _ => None,
            };
            addr.map(|a| address_utils::address_to_checksum_string(&a))
                .unwrap_or_default()
        }))
    }

    /// Token1 contract address (EIP-55 checksummed hex). Empty string when the
    /// registered family has no token1 field; raises if the id is not
    /// registered.
    #[getter]
    fn token1_address(&self, py: Python<'_>) -> PyResult<String> {
        let family = self.family_of(py)?;
        Ok(self.with_state(py, |core| {
            let addr = match family {
                "v2" => core
                    .pool_slice::<degenbot_pools::state::v2_state::V2PoolIdentity>(self.pool_id)
                    .map(|i| i.token1),
                "v3" => core
                    .pool_slice::<degenbot_pools::state::v3_state::V3PoolIdentity>(self.pool_id)
                    .map(|i| i.token1),
                "v4" => core
                    .pool_slice::<degenbot_pools::state::v4_state::V4PoolIdentity>(self.pool_id)
                    .map(|i| i.pool_key.currency1),
                "aerodrome-v2" => core
                    .pool_slice::<degenbot_pools::state::aerodrome_v2_state::AerodromeV2PoolIdentity>(
                        self.pool_id,
                    )
                    .map(|i| i.token1),
                _ => None,
            };
            addr.map(|a| address_utils::address_to_checksum_string(&a))
                .unwrap_or_default()
        }))
    }

    /// Factory contract address (EIP-55 checksummed hex). Empty string when the
    /// registered family has no factory field; raises if the id is not
    /// registered.
    #[getter]
    fn factory(&self, py: Python<'_>) -> PyResult<String> {
        let family = self.family_of(py)?;
        Ok(self.with_state(py, |core| {
            let addr = match family {
                "v2" => core
                    .pool_slice::<degenbot_pools::state::v2_state::V2PoolIdentity>(self.pool_id)
                    .map(|i| i.factory),
                "v3" => core
                    .pool_slice::<degenbot_pools::state::v3_state::V3PoolIdentity>(self.pool_id)
                    .map(|i| i.factory),
                "aerodrome-v2" => core
                    .pool_slice::<degenbot_pools::state::aerodrome_v2_state::AerodromeV2PoolIdentity>(
                        self.pool_id,
                    )
                    .map(|i| i.factory),
                _ => None,
            };
            addr.map(|a| address_utils::address_to_checksum_string(&a))
                .unwrap_or_default()
        }))
    }

    /// The CREATE2 deployer this pool's address was verified against (Fork A).
    /// The JSON row's `deployer` (or the factory for ``null``), resolved at
    /// registration. V2 and V3 pools carry this; other registered families
    /// return an empty string; raises if the id is not registered.
    #[getter]
    fn deployer(&self, py: Python<'_>) -> PyResult<String> {
        let family = self.family_of(py)?;
        Ok(self.with_state(py, |core| {
            let addr = match family {
                "v2" => core
                    .pool_slice::<degenbot_pools::state::v2_state::V2PoolIdentity>(self.pool_id)
                    .map(|i| i.deployer),
                "v3" => core
                    .pool_slice::<degenbot_pools::state::v3_state::V3PoolIdentity>(self.pool_id)
                    .map(|i| i.deployer),
                _ => None,
            };
            addr.map(|a| address_utils::address_to_checksum_string(&a))
                .unwrap_or_default()
        }))
    }

    /// The CREATE2 init code hash this pool's address was verified against
    /// (Fork A). The JSON row's `init_hash` when shipped, else the Uniswap
    /// mainnet fallback (V2 or V3 const). V2 and V3 pools carry this; other
    /// registered families return an empty string; raises if the id is not
    /// registered.
    #[getter]
    fn init_hash(&self, py: Python<'_>) -> PyResult<String> {
        let family = self.family_of(py)?;
        Ok(self.with_state(py, |core| {
            let h = match family {
                "v2" => core
                    .pool_slice::<degenbot_pools::state::v2_state::V2PoolIdentity>(self.pool_id)
                    .map(|i| i.init_hash),
                "v3" => core
                    .pool_slice::<degenbot_pools::state::v3_state::V3PoolIdentity>(self.pool_id)
                    .map(|i| i.init_hash),
                _ => None,
            };
            h.map(|b| format!("{b:#x}")).unwrap_or_default()
        }))
    }

    /// `token0→token1` fee parameters: `(gamma_numer, fee_denom)` — the
    /// retained post-fee fraction (e.g. `(997, 1000)` for 0.3%). `(0, 0)` if
    /// not a V2 pool.
    #[getter]
    fn fee_token0(&self, py: Python<'_>) -> (u64, u64) {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::v2_state::V2PoolIdentity>(self.pool_id)
                .map(|s| s.fee_token0)
                .unwrap_or_default()
        })
    }

    /// `token1→token0` fee parameters: `(gamma_numer, fee_denom)`. `(0, 0)` if
    /// not a V2 pool.
    #[getter]
    fn fee_token1(&self, py: Python<'_>) -> (u64, u64) {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::v2_state::V2PoolIdentity>(self.pool_id)
                .map(|s| s.fee_token1)
                .unwrap_or_default()
        })
    }

    /// The DEX+variant discriminator as a kebab-case string (e.g.
    /// `"uniswap-v2"`, `"camelot-v2-stable"`). Empty string when the
    /// registered family has no variant field; raises if the id is not
    /// registered.
    #[getter]
    fn variant(&self, py: Python<'_>) -> PyResult<String> {
        let family = self.family_of(py)?;
        Ok(self.with_state(py, |core| match family {
            "v2" => core
                .pool_slice::<degenbot_pools::state::v2_state::V2PoolIdentity>(self.pool_id)
                .map(|d| d.variant.as_str().to_string())
                .unwrap_or_default(),
            "aerodrome-v2" => core
                .pool_slice::<degenbot_pools::state::aerodrome_v2_state::AerodromeV2PoolIdentity>(
                    self.pool_id,
                )
                .map(|d| d.variant.as_str().to_string())
                .unwrap_or_default(),
            _ => String::new(),
        }))
    }

    /// Camelot solidly-stable strategy flag. `false` for all non-Camelot V2
    /// and for non-V2 `pool_ids`.
    #[getter]
    fn stable_swap(&self, py: Python<'_>) -> bool {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::v2_state::V2PoolIdentity>(self.pool_id)
                .is_some_and(|d| d.stable_swap)
        })
    }

    /// The pool-family tag for this handle's registered pool (`"v2"`,
    /// `"v3"`, `"v4"`, `"curve"`, `"balancer-weighted"`,
    /// `"balancer-stable"`). Raises if unregistered — a handle always
    /// references a registered pool, so the `""` sentinel is retired.
    /// This is the uniform family-guard primitive every `from_handle`
    /// seam asserts against — dispatches on the `PoolEntry` variant
    /// directly, so it is correct for every registered family (unlike
    /// `variant`, which is V2-only and returns `""` for non-V2).
    #[getter]
    fn pool_family(&self, py: Python<'_>) -> PyResult<String> {
        Ok(self.family_of(py)?.to_string())
    }

    /// Camelot integer fee scaling. `None` for non-Camelot V2 / non-V2.
    #[getter]
    fn fee_denominator(&self, py: Python<'_>) -> Option<u64> {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::v2_state::V2PoolIdentity>(self.pool_id)
                .and_then(|d| d.fee_denominator)
        })
    }

    /// The resolved `DexIdentity` for this pool's registered variant, with the
    /// JSON-sourced deployer + `init_hash` merged in (Fork A). `None` if
    /// not a V2 pool. The Python companion reads this to recover deployer /
    /// init-hash without taking constructor args. Protocol-const fields
    /// (fees/ABI shape) come from the variant preset; `factory`/`deployer` /
    /// `init_hash` come from the identity (the verified values stored at
    /// registration).
    #[getter]
    fn dex(&self, py: Python<'_>) -> Option<crate::bot::dex_identity::PyDexIdentity> {
        self.with_state(py, |core| {
            let id =
                core.pool_slice::<degenbot_pools::state::v2_state::V2PoolIdentity>(self.pool_id)?;
            let mut ident = degenbot_uniswap::dex_identity::preset_for_variant(id.variant);
            ident.factory = id.factory;
            ident.deployer = id.deployer;
            ident.init_hash = id.init_hash;
            Some(crate::bot::dex_identity::PyDexIdentity::from_core(&ident))
        })
    }

    // --- V2 token-recovery getters (ADR-005 identity slice) ---
    // Recover `PyErc20Token` handles for the pool's token0/token1 from the
    // SAME shared BotState (ADR-006: one Bot per chain owns all assets). The
    // companion wraps these via `Erc20Token.from_handle` so the
    // `from_handle(py_pool)` seam needs no token args — the Polars
    // `_from_pydf` end state.
    //
    // Returns `None` if the pool isn't V2 or the token address isn't
    // registered in the shared BotState.tokens registry (the failure mode the
    // test-factory cross-Bot-token fix addresses — production always registers).

    /// `PyErc20Token` handle for token0, or `None` if not registered in the
    /// shared `BotState`.
    fn get_token0(&self, py: Python<'_>) -> PyResult<Option<PyErc20Token>> {
        let family = self.family_of(py)?;
        Ok(self.with_state(py, |core| {
            let token_addr = match family {
                "v2" => core
                    .pool_slice::<degenbot_pools::state::v2_state::V2PoolIdentity>(self.pool_id)
                    .map(|i| i.token0),
                "v3" => core
                    .pool_slice::<degenbot_pools::state::v3_state::V3PoolIdentity>(self.pool_id)
                    .map(|i| i.token0),
                "v4" => core
                    .pool_slice::<degenbot_pools::state::v4_state::V4PoolIdentity>(self.pool_id)
                    .map(|i| i.pool_key.currency0),
                "aerodrome-v2" => core
                    .pool_slice::<degenbot_pools::state::aerodrome_v2_state::AerodromeV2PoolIdentity>(
                        self.pool_id,
                    )
                    .map(|i| i.token0),
                _ => None,
            }?;
            if core.has_token(&token_addr) {
                Some(PyErc20Token::new(Arc::clone(&self.core), token_addr))
            } else {
                None
            }
        }))
    }

    /// `PyErc20Token` handle for token1, or `None` if not registered in the
    /// shared `BotState`.
    fn get_token1(&self, py: Python<'_>) -> PyResult<Option<PyErc20Token>> {
        let family = self.family_of(py)?;
        Ok(self.with_state(py, |core| {
            let token_addr = match family {
                "v2" => core
                    .pool_slice::<degenbot_pools::state::v2_state::V2PoolIdentity>(self.pool_id)
                    .map(|i| i.token1),
                "v3" => core
                    .pool_slice::<degenbot_pools::state::v3_state::V3PoolIdentity>(self.pool_id)
                    .map(|i| i.token1),
                "v4" => core
                    .pool_slice::<degenbot_pools::state::v4_state::V4PoolIdentity>(self.pool_id)
                    .map(|i| i.pool_key.currency1),
                "aerodrome-v2" => core
                    .pool_slice::<degenbot_pools::state::aerodrome_v2_state::AerodromeV2PoolIdentity>(
                        self.pool_id,
                    )
                    .map(|i| i.token1),
                _ => None,
            }?;
            if core.has_token(&token_addr) {
                Some(PyErc20Token::new(Arc::clone(&self.core), token_addr))
            } else {
                None
            }
        }))
    }

    fn get_balancer_tokens(&self, py: Python<'_>) -> Option<Vec<PyErc20Token>> {
        self.with_state(py, |core| {
            let identity = core.pool_slice::<degenbot_pools::state::balancer_weighted_state::BalancerWeightedPoolIdentity>(self.pool_id)?;
            let mut out = Vec::with_capacity(identity.tokens.len());
            for token_addr in &identity.tokens {
                if !core.has_token(token_addr) {
                    return None;
                }
                out.push(PyErc20Token::new(Arc::clone(&self.core), *token_addr));
            }
            Some(out)
        })
    }

    // --- V3 state read getters ---
    // Mirror the V2 family but read the V3PoolState entry. All getters take
    // one read guard and return None-defaulted values when the pool_id is not
    // a registered V3 pool (matching the V2 getters' behavior on V2).

    // --- V4 identity getters (ADR-005 sealed seam) ---
    // Read off V4PoolIdentity so UniswapV4Pool.from_handle is self-describing.

    /// Pool manager contract address (EIP-55 checksummed hex). Empty string if
    /// not a V4 pool.
    #[getter]
    fn pool_manager_address(&self, py: Python<'_>) -> String {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::v4_state::V4PoolIdentity>(self.pool_id)
                .map(|i| address_utils::address_to_checksum_string(&i.pool_manager))
                .unwrap_or_default()
        })
    }

    /// On-chain V4 pool ID (32-byte hex, ``0x``-prefixed). Empty string if not
    /// a V4 pool.
    #[getter]
    fn pool_id_hex(&self, py: Python<'_>) -> String {
        self.with_state(py, |core| match core
            .pool_slice::<degenbot_pools::state::v4_state::V4PoolIdentity>(self.pool_id)
        {
            Some(i) => alloy::hex::encode_prefixed(i.pool_id),
            None => String::new(),
        })
    }

    /// Hook contract address (EIP-55 checksummed hex). Empty string if not a
    /// V4 pool.
    #[getter]
    fn hook_address(&self, py: Python<'_>) -> String {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::v4_state::V4PoolIdentity>(self.pool_id)
                .map(|i| address_utils::address_to_checksum_string(&i.pool_key.hooks))
                .unwrap_or_default()
        })
    }

    /// The `StateView` contract this pool's manager registered (EIP-55
    /// checksummed hex). Empty string if not a V4 pool or no state view was
    /// registered for its manager — the companion's `from_handle` reads it
    /// so no builder-supplied override threads the value (ADR-085 D4).
    #[getter]
    fn state_view_address(&self, py: Python<'_>) -> String {
        self.with_state(py, |core| {
            core.state_view_for_pool(self.pool_id)
                .map(|a| address_utils::address_to_checksum_string(&a))
                .unwrap_or_default()
        })
    }

    /// The packed `slot0.protocolFee` the core's state machine tracks
    /// (`zero_for_one` in the low 12 bits, `one_for_zero` in the high 12).
    /// `0` if not a V4 pool.
    #[getter]
    fn protocol_fee(&self, py: Python<'_>) -> u32 {
        self.with_state(py, |core| {
            let identity =
                core.pool_slice::<degenbot_pools::state::v4_state::V4PoolIdentity>(self.pool_id);
            identity
                .and_then(|i| core.try_registered(i.pool_manager, &i.pool_id))
                .map(|registered| registered.protocol_fee)
                .unwrap_or_default()
        })
    }

    /// The static pool-key fee (the LP fee — dynamic-fee pools are
    /// admission-rejected and never registered). `0` if not a V4 pool.
    #[getter]
    fn lp_fee(&self, py: Python<'_>) -> u32 {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::v4_state::V4PoolIdentity>(self.pool_id)
                .map(|i| i.pool_key.fee)
                .unwrap_or_default()
        })
    }

    // --- Aerodrome V2 identity getters (ADR-005 Aerodrome state port) ---

    /// Aerodrome V2 Solidly stable-invariant flag. `false` for volatile mode
    /// or non-Aerodrome pools.
    #[getter]
    fn aerodrome_stable(&self, py: Python<'_>) -> bool {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::aerodrome_v2_state::AerodromeV2PoolIdentity>(
                self.pool_id,
            )
            .is_some_and(|d| d.stable)
        })
    }

    /// Aerodrome V2 unidirectional fee as `(fee_numer, fee_denom)`. `(0, 0)` if
    /// not an Aerodrome pool.
    #[getter]
    fn aerodrome_fee(&self, py: Python<'_>) -> (u64, u64) {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::aerodrome_v2_state::AerodromeV2PoolIdentity>(
                self.pool_id,
            )
            .map(|d| d.fee)
            .unwrap_or_default()
        })
    }

    /// Aerodrome V2 token0 ERC-20 decimal count. `0` for non-Aerodrome pools.
    #[getter]
    fn aerodrome_token0_decimals(&self, py: Python<'_>) -> u8 {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::aerodrome_v2_state::AerodromeV2PoolIdentity>(
                self.pool_id,
            )
            .map(|d| d.token0_decimals)
            .unwrap_or_default()
        })
    }

    /// Aerodrome V2 token1 ERC-20 decimal count. `0` for non-Aerodrome pools.
    #[getter]
    fn aerodrome_token1_decimals(&self, py: Python<'_>) -> u8 {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::aerodrome_v2_state::AerodromeV2PoolIdentity>(
                self.pool_id,
            )
            .map(|d| d.token1_decimals)
            .unwrap_or_default()
        })
    }

    // --- Balancer weighted identity getters (ADR-005 sealed seam) ---

    /// Balancer V2 pool contract address (EIP-55 checksummed hex).
    /// Empty string if not a Balancer weighted pool.
    #[getter]
    fn balancer_address(&self, py: Python<'_>) -> String {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::balancer_weighted_state::BalancerWeightedPoolIdentity>(self.pool_id)
                .map(|i| address_utils::address_to_checksum_string(&i.address))
                .unwrap_or_default()
        })
    }

    /// Balancer V2 vault contract address (EIP-55 checksummed hex).
    /// Empty string if not a Balancer weighted pool.
    #[getter]
    fn balancer_vault(&self, py: Python<'_>) -> String {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::balancer_weighted_state::BalancerWeightedPoolIdentity>(self.pool_id)
                .map(|i| address_utils::address_to_checksum_string(&i.vault))
                .unwrap_or_default()
        })
    }

    /// Balancer V2 pool ID (32-byte hex, ``0x``-prefixed). Empty string if
    /// not a Balancer weighted pool.
    #[getter]
    fn balancer_pool_id_hex(&self, py: Python<'_>) -> String {
        self.with_state(py, |core| {
            match core.pool_slice::<degenbot_pools::state::balancer_weighted_state::BalancerWeightedPoolIdentity>(self.pool_id) {
                Some(i) => alloy::hex::encode_prefixed(i.pool_id),
                None => String::new(),
            }
        })
    }

    // (The `balancer_token_addresses` getter retired with ADR-085 D5: the
    // construction outcome's `tokens` answer is the companion loop's ONE
    // iteration source, so no family-specific handle getter is read.)

    /// Balancer weighted denormalized weights (one `U256` per token).
    /// Empty list if not a Balancer weighted pool.
    #[getter]
    fn balancer_weights(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let weights: Vec<alloy::primitives::U256> = self.with_state(py, |s| {
            s.pool_slice::<degenbot_pools::state::balancer_weighted_state::BalancerWeightedPoolIdentity>(
                self.pool_id,
            )
            .map(|i| i.weights.to_vec())
            .unwrap_or_default()
        });
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
        let sf: Vec<alloy::primitives::U256> = self.with_state(py, |s| {
            s.pool_slice::<degenbot_pools::state::balancer_weighted_state::BalancerWeightedPoolIdentity>(
                self.pool_id,
            )
            .map(|i| i.scaling_factors.to_vec())
            .unwrap_or_default()
        });
        let py_sf: Vec<Py<PyAny>> = sf
            .iter()
            .map(|s| crate::conversion::alloy::u256_to_py(py, s).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        Ok(pyo3::types::PyList::new(py, py_sf)?.into_any().unbind())
    }

    /// Balancer weighted swap fee (fixed-point, 1e18 scale). 0 if not a
    /// Balancer weighted pool.
    #[getter]
    fn balancer_swap_fee(&self, py: Python<'_>) -> u128 {
        self.with_state(py, |s| {
            s.pool_slice::<degenbot_pools::state::balancer_weighted_state::BalancerWeightedPoolIdentity>(
                self.pool_id,
            )
            .map(|i| i.swap_fee)
            .unwrap_or_default()
        })
    }

    /// Balancer weighted Math pool implementation version (1 or 2). 0 if not
    /// a Balancer weighted pool.
    #[getter]
    fn balancer_pow_version(&self, py: Python<'_>) -> u8 {
        self.with_state(py, |s| {
            s.pool_slice::<degenbot_pools::state::balancer_weighted_state::BalancerWeightedPoolIdentity>(
                self.pool_id,
            )
            .map(|i| i.pow_version)
            .unwrap_or_default()
        })
    }

    // --- Mutations (per-handle, pool_id-keyed) ---

    /// Registration/seed genesis anchor (two-stamp rule): push a
    /// `before == after` reorg-journal delta at `block_number` WITHOUT
    /// advancing either clock, so a split-seed (price at HEAD, tick map at the
    /// DB block) pool keeps a non-empty journal for mid-window reorg restore.
    /// Returns `True` on a registered V3/V4 pool; raises `ValueError` for a
    /// registered non-concentrated-liquidity family (no concentrated liquidity journal to seed) or an unregistered id.
    #[pyo3(signature = (block_number))]
    fn seed_genesis(&self, py: Python<'_>, block_number: u64) -> PyResult<bool> {
        match self.with_state_mut(py, |s| {
            s.seed_genesis_by_pool_id(self.pool_id, block_number)
        }) {
            Ok(_) => Ok(true),
            Err(e) => Err(pyo3::exceptions::PyValueError::new_err(format!(
                "seed_genesis: {e}"
            ))),
        }
    }

    // --- Curve state read getters + mutations ---

    // --- Curve identity getters (ADR-005 identity extension) ---

    /// Curve A-ramping: `(initial_a, future_a, initial_a_time,
    /// future_a_time, create_timestamp)` — all `None` for non-ramping pools.
    /// Returns `None` for a non-Curve handle.
    /// Each element is the option value so a non-ramping pool reports `None`
    /// for every field instead of a sentinel zero.
    // The nested-`Option` tuple mirrors the Python-facing Curve ramp shape.
    #[expect(clippy::type_complexity)]
    fn curve_a_ramp(
        &self,
        py: Python<'_>,
    ) -> Option<(
        Option<u128>,
        Option<u128>,
        Option<u64>,
        Option<u64>,
        Option<u64>,
    )> {
        self.with_state(py, |core| {
            let id = core.pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(
                self.pool_id,
            )?;
            Some((
                id.initial_a_coefficient,
                id.future_a_coefficient,
                id.initial_a_coefficient_time,
                id.future_a_coefficient_time,
                id.create_timestamp,
            ))
        })
    }

    /// Curve crypto-pool fees: `(fee_gamma, mid_fee, offpeg_fee_multiplier,
    /// out_fee, gamma)` — `None` for standard stableswap pools. Returns `None`
    /// for a non-Curve handle.
    // The nested-`Option` tuple mirrors the Python-facing Curve fee shape.
    #[expect(clippy::type_complexity)]
    fn curve_crypto_fees(
        &self,
        py: Python<'_>,
    ) -> Option<(
        Option<u64>,
        Option<u64>,
        Option<u64>,
        Option<u64>,
        Option<u64>,
    )> {
        self.with_state(py, |core| {
            let id = core.pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(
                self.pool_id,
            )?;
            Some((
                id.fee_gamma,
                id.mid_fee,
                id.offpeg_fee_multiplier,
                id.out_fee,
                id.gamma,
            ))
        })
    }

    /// Curve dedicated LP token address (EIP-55 checksummed) — `None` when the
    /// pool token itself is the LP. Returns `None` for a non-Curve handle.
    // `Option<Option<_>>` distinguishes no-pool / pool-no-lp-token / has-token.
    #[expect(clippy::option_option)]
    fn curve_lp_token(&self, py: Python<'_>) -> Option<Option<String>> {
        self.with_state(py, |core| {
            let id = core.pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(
                self.pool_id,
            )?;
            Some(
                id.lp_token
                    .map(|a| address_utils::address_to_checksum_string(&a)),
            )
        })
    }

    /// Curve per-token `use_lending` flags. Empty list for a non-Curve pool
    /// or when none were registered.
    #[getter]
    fn curve_use_lending(&self, py: Python<'_>) -> Vec<bool> {
        self.with_state(py, |core| match core
            .pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(self.pool_id)
        {
            Some(i) => i.use_lending.to_vec(),
            None => Vec::new(),
        })
    }

    /// Curve per-token `precision_multipliers` (one `U256` per token). Empty
    /// list for a non-Curve pool.
    #[getter]
    fn curve_precision_multipliers(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let pms: Vec<alloy::primitives::U256> =
            self.with_state(py, |core| match core
                .pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(
                self.pool_id,
            ) {
                Some(i) => i.precision_multipliers.to_vec(),
                None => Vec::new(),
            });
        let py_pms: Vec<Py<PyAny>> = pms
            .iter()
            .map(|b| crate::conversion::alloy::u256_to_py(py, b).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        Ok(pyo3::types::PyList::new(py, py_pms)?.into_any().unbind())
    }

    /// Whether a Curve data-provider I/O trait object is stored on this
    /// pool's state (ADR-005). `False` for non-Curve pools or Curve
    /// pools registered without a provider (the no-I/O fixture case).
    #[getter]
    fn curve_has_data_provider(&self, py: Python<'_>) -> bool {
        self.with_state(py, |core| match core
            .pool_slice::<degenbot_pools::state::curve_state::CurvePoolState>(self.pool_id)
        {
            Some(s) => s.data_provider.is_some(),
            None => false,
        })
    }

    // --- Curve identity getters (ADR-005 identity-from-handle) ---

    /// Curve amplification coefficient `A` (raw). 0 for a non-Curve handle.
    #[getter]
    fn curve_a_coefficient(&self, py: Python<'_>) -> u128 {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(self.pool_id)
                .map_or(0, |i| i.a_coefficient)
        })
    }

    /// Curve swap fee (`FEE_DENOMINATOR` units). 0 for a non-Curve handle.
    #[getter]
    fn curve_fee(&self, py: Python<'_>) -> u64 {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(self.pool_id)
                .map_or(0, |i| i.fee)
        })
    }

    /// Curve admin-fee share (`FEE_DENOMINATOR` units). 0 for a non-Curve handle.
    #[getter]
    fn curve_admin_fee(&self, py: Python<'_>) -> u64 {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(self.pool_id)
                .map_or(0, |i| i.admin_fee)
        })
    }

    /// Curve rate multipliers (one `U256` per token). Empty for a non-Curve pool.
    #[getter]
    fn curve_rate_multipliers(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let rms: Vec<alloy::primitives::U256> =
            self.with_state(py, |core| match core
                .pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(
                self.pool_id,
            ) {
                Some(i) => i.rate_multipliers.to_vec(),
                None => Vec::new(),
            });
        let py_rms: Vec<Py<PyAny>> = rms
            .iter()
            .map(|b| crate::conversion::alloy::u256_to_py(py, b).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        Ok(pyo3::types::PyList::new(py, py_rms)?.into_any().unbind())
    }

    /// Curve `swap_style` discriminant (`PoolStrategies.swap_style.value`).
    /// 0 for a non-Curve handle.
    #[getter]
    fn curve_swap_style(&self, py: Python<'_>) -> u8 {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(self.pool_id)
                .map_or(0, |i| i.swap_style)
        })
    }

    /// Curve `lending_rate_style` discriminant. 0 for a non-Curve handle.
    #[getter]
    fn curve_lending_rate_style(&self, py: Python<'_>) -> u8 {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(self.pool_id)
                .map_or(0, |i| i.lending_rate_style)
        })
    }

    /// Curve `d_variant` discriminant. 0 for a non-Curve handle.
    #[getter]
    fn curve_d_variant(&self, py: Python<'_>) -> u8 {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(self.pool_id)
                .map_or(0, |i| i.d_variant)
        })
    }

    /// Curve `y_variant` discriminant. 0 for a non-Curve handle.
    #[getter]
    fn curve_y_variant(&self, py: Python<'_>) -> u8 {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(self.pool_id)
                .map_or(0, |i| i.y_variant)
        })
    }

    /// Curve `yd_variant` discriminant. 0 for a non-Curve handle.
    #[getter]
    fn curve_yd_variant(&self, py: Python<'_>) -> u8 {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(self.pool_id)
                .map_or(0, |i| i.yd_variant)
        })
    }

    /// Curve `metapool_rate_style` discriminant. 0 for a non-Curve handle.
    #[getter]
    fn curve_metapool_rate_style(&self, py: Python<'_>) -> u8 {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(self.pool_id)
                .map_or(0, |i| i.metapool_rate_style)
        })
    }

    /// Curve `metapool_underlying_style` discriminant. 0 for a non-Curve handle.
    #[getter]
    fn curve_metapool_underlying_style(&self, py: Python<'_>) -> u8 {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(self.pool_id)
                .map_or(0, |i| i.metapool_underlying_style)
        })
    }

    /// Curve base-pool address (EIP-55 checksummed). `None` for plain pools.
    /// `None` for a non-Curve handle.
    // `Option<Option<_>>` distinguishes no-pool / pool-no-base / has-base.
    #[expect(clippy::option_option)]
    fn curve_base_pool_address(&self, py: Python<'_>) -> Option<Option<String>> {
        self.with_state(py, |core| {
            let id = core.pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(
                self.pool_id,
            )?;
            Some(
                id.base_pool
                    .map(|a| address_utils::address_to_checksum_string(&a)),
            )
        })
    }

    /// The Curve pool's token companion handles, resolved via the shared
    /// `BotState` token registry. `None` if this is not a Curve pool or any
    /// token isn't registered (mirror of `get_balancer_tokens`). The companion
    /// wraps each via `Erc20Token.from_handle`.
    fn get_curve_tokens(&self, py: Python<'_>) -> Option<Vec<PyErc20Token>> {
        self.with_state(py, |core| {
            let identity = core
                .pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(
                    self.pool_id,
                )?;
            let mut out = Vec::with_capacity(identity.tokens.len());
            for token_addr in &identity.tokens {
                if !core.has_token(token_addr) {
                    return None;
                }
                out.push(PyErc20Token::new(Arc::clone(&self.core), *token_addr));
            }
            Some(out)
        })
    }

    /// The Curve pool's *underlying* token companion handles (metapool coins
    /// beneath the base-pool intermediaries). `None` for plain pools, or if
    /// this isn't a Curve pool, or any underlying token isn't registered.
    fn get_curve_tokens_underlying(&self, py: Python<'_>) -> Option<Vec<PyErc20Token>> {
        self.with_state(py, |core| {
            let identity = core
                .pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(
                    self.pool_id,
                )?;
            let underlying = identity.tokens_underlying.clone()?;
            let mut out = Vec::with_capacity(underlying.len());
            for token_addr in &underlying {
                if !core.has_token(token_addr) {
                    return None;
                }
                out.push(PyErc20Token::new(Arc::clone(&self.core), *token_addr));
            }
            Some(out)
        })
    }

    /// The Curve pool's dedicated LP-token companion handle. `None` ⇔ the
    /// pool token is itself the LP (the common plain-pool case; the companion
    /// falls back to `tokens[0]`). Returns `None` (outer) for a non-Curve pool
    /// or if the LP token isn't registered.
    fn get_curve_lp_token(&self, py: Python<'_>) -> Option<PyErc20Token> {
        self.with_state(py, |core| {
            let identity = core
                .pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(
                    self.pool_id,
                )?;
            let lp = identity.lp_token?;
            if !core.has_token(&lp) {
                return None;
            }
            Some(PyErc20Token::new(Arc::clone(&self.core), lp))
        })
    }

    /// The Curve pool's raw ERC-20 coin addresses in canonical order. Unlike
    /// `get_curve_tokens`, this does NOT require the tokens to be registered
    /// first — it is the builder/companion-orchestration seam that lets a
    /// caller construct ERC20 companions *before* `from_handle`. `None` for
    /// a non-Curve pool.
    fn curve_token_addresses(&self, py: Python<'_>) -> Option<Vec<String>> {
        self.with_state(py, |core| {
            let identity = core
                .pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(
                    self.pool_id,
                )?;
            let mut out = Vec::with_capacity(identity.tokens.len());
            for address in &identity.tokens {
                out.push(address_utils::address_to_checksum_string(address));
            }
            Some(out)
        })
    }

    /// The Curve pool's raw *underlying* coin addresses (metapool coins beneath
    /// the base-pool intermediaries). `None` for a plain pool or a non-Curve
    /// handle. Not registration-gated (twin of `curve_token_addresses`).
    fn curve_token_addresses_underlying(&self, py: Python<'_>) -> Option<Vec<String>> {
        self.with_state(py, |core| {
            let identity = core
                .pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(
                    self.pool_id,
                )?;
            let underlying = identity.tokens_underlying.as_ref()?;
            let mut out = Vec::with_capacity(underlying.len());
            for address in underlying {
                out.push(address_utils::address_to_checksum_string(address));
            }
            Some(out)
        })
    }

    /// The Curve pool's raw dedicated LP-token address, or `None` if the pool
    /// token is itself the LP (or the handle is not a Curve pool). Not
    /// registration-gated (twin of `curve_token_addresses`).
    fn curve_lp_token_address(&self, py: Python<'_>) -> Option<String> {
        self.with_state(py, |core| {
            let identity = core
                .pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(
                    self.pool_id,
                )?;
            identity
                .lp_token
                .map(|address| address_utils::address_to_checksum_string(&address))
        })
    }

    /// **The go-between.** A `PyLiquidityPool` handle over this
    /// metapool's *base pool*, sharing the same `BotState` core. Resolves the
    /// stored `base_pool` address through the existing `pool_id_by_address`
    /// index — no Python registry needed. `None` for plain pools, non-Curve
    /// handles, or when the base pool isn't registered. The companion recurses:
    /// `CurveStableswapPool.from_handle(handle.curve_base_pool())`.
    fn curve_base_pool(&self, py: Python<'_>) -> Option<PyLiquidityPool> {
        self.with_state(py, |core| {
            let id = core.pool_slice::<degenbot_pools::state::curve_state::CurvePoolIdentity>(
                self.pool_id,
            )?;
            let base_addr = id.base_pool?;
            let base_id = core.pool_id_by_address(&base_addr)?;
            // Construct a handle over the base pool's id, sharing this core.
            // (The read guard releases when the accessor returns; `new` takes
            // no lock.)
            Some(PyLiquidityPool::new(
                Arc::clone(&self.core),
                base_id,
                self.chain_id,
            ))
        })
    }

    /// Rust-owned Curve stableswap `curve_get_dy(i, j, dx, block_number,
    /// override_balances)` on this handle's pool — the single-call shape the
    /// companion `CurveStableswapPool.get_dy` delegates to.
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
        let result = self.with_state(py, |core| {
            core.get_dy(
                self.pool_id,
                i,
                j,
                amount,
                block_number,
                overrides.as_deref(),
            )
        });
        let out = match result {
            Ok(v) => v,
            Err(CurveInputsError::NoLiquidity) => {
                return Err(curve_no_liquidity_error(
                    py,
                    "One or more of the tokens has a zero balance.",
                ));
            }
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
        let result = self.with_state(py, |core| {
            core.get_dy_underlying(
                self.pool_id,
                i,
                j,
                amount,
                block_number,
                overrides.as_deref(),
            )
        });
        let out = match result {
            Ok(v) => v,
            Err(CurveInputsError::NoLiquidity) => {
                return Err(curve_no_liquidity_error(
                    py,
                    "One or more of the tokens has a zero balance.",
                ));
            }
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
    /// companion `CurveStableswapPool.calc_token_amount` delegates to. No Python provider / cache / calculator on the path.
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
        let result = self.with_state(py, |core| {
            core.calc_token_amount(self.pool_id, &amounts, deposit, block_number)
        });
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
    /// Returns only the coin-`i` `dy` (the companion's
    /// extra tuple fields aren't consumed anywhere).
    fn curve_calc_withdraw_one_coin(
        &self,
        py: Python<'_>,
        token_amount: &Bound<'_, PyAny>,
        i: usize,
        block_number: u64,
    ) -> PyResult<Py<PyAny>> {
        let token_amount = crate::conversion::alloy::extract_python_u256(token_amount)?;
        let result = self.with_state(py, |core| {
            core.calc_withdraw_one_coin(self.pool_id, token_amount, i, block_number)
        });
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

    fn fetch_curve_block_number(&self, py: Python<'_>) -> Option<u64> {
        // Provider clone inside py.detach (GIL/BotState inversion fix,
        // 2026-08-21 run-9); the re-entrant provider call stays under the GIL.
        let provider = self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::curve_state::CurvePoolState>(self.pool_id)
                .and_then(|st| st.data_provider.clone())
        });
        let provider = provider?;
        provider.block_number().ok()
    }

    fn fetch_curve_block_timestamp(&self, py: Python<'_>, block_number: u64) -> Option<u64> {
        let provider = self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::curve_state::CurvePoolState>(self.pool_id)
                .and_then(|st| st.data_provider.clone())
        });
        let provider = provider?;
        provider.block_timestamp(block_number).ok()
    }

    fn fetch_curve_token_balance(
        &self,
        py: Python<'_>,
        token_address: &str,
        holder_address: &str,
        block_number: u64,
    ) -> PyResult<Option<Py<PyAny>>> {
        let provider = self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::curve_state::CurvePoolState>(self.pool_id)
                .and_then(|st| st.data_provider.clone())
        });
        let Some(provider) = provider else {
            return Ok(None);
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
        let provider = self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::curve_state::CurvePoolState>(self.pool_id)
                .and_then(|st| st.data_provider.clone())
        });
        let Some(provider) = provider else {
            return Ok(None);
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
        self.read_provider_opt(py, |p| p.invariant_d(block_number))
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

    fn fetch_curve_base_cache_updated(&self, py: Python<'_>, block_number: u64) -> Option<u64> {
        let provider = self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::curve_state::CurvePoolState>(self.pool_id)
                .and_then(|st| st.data_provider.clone())
        });
        let provider = provider?;
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

    // --- Balancer weighted state read getters + mutations ---

    // --- Balancer stable state read getters + mutations ---

    /// Token count for a Balancer stable pool (`balances.len()` — includes
    /// BPT for Composable pools).
    /// including BPT for Composable pools).
    /// Returns an empty list if this `pool_id` is not registered as a Balancer
    /// `Some(i)` for `ComposableStablePools`.
    /// Returns `None` if this `pool_id` is not registered as a Balancer stable
    /// pool (also a valid value for a registered `MetaStable` — see the
    /// `invariant_version` getter to distinguish).
    #[getter]
    fn balancer_bpt_index(&self, py: Python<'_>) -> Option<usize> {
        self.with_state(py, |s| {
            s.pool_slice::<degenbot_pools::state::balancer_stable_state::BalancerStablePoolIdentity>(
                self.pool_id,
            )
            .and_then(|i| i.bpt_idx)
        })
    }

    /// Amplification coefficient `amp` for a Balancer stable pool (immutable
    /// after registration in this plan — A ramping is a future, non-epic
    /// concern resolved by the builder at registration).
    /// Returns 0 if this `pool_id` is not registered as a Balancer stable pool.
    #[getter]
    fn balancer_amp(&self, py: Python<'_>) -> u128 {
        self.with_state(py, |s| {
            s.pool_slice::<degenbot_pools::state::balancer_stable_state::BalancerStablePoolIdentity>(
                self.pool_id,
            )
            .map_or(0, |i| i.amp)
        })
    }

    /// `invariant_version` discriminator (1 = V1 always-roundDown `D_P`
    /// accumulation; 2 = V2 roundUp-param `P_D` accumulation) — the
    /// systematic-1-wei-error guard.
    /// Returns 0 if this `pool_id` is not registered as a Balancer stable pool.
    #[getter]
    fn balancer_invariant_version(&self, py: Python<'_>) -> u8 {
        self.with_state(py, |s| {
            s.pool_slice::<degenbot_pools::state::balancer_stable_state::BalancerStablePoolIdentity>(
                self.pool_id,
            )
            .map_or(0, |i| i.invariant_version)
        })
    }

    // --- Balancer stable identity getters (ADR-005 sealed seam) ---

    /// Balancer V2 stable pool's Vault singleton (EIP-55 checksummed). Empty
    /// string if not a Balancer stable pool.
    #[getter]
    fn balancer_stable_vault(&self, py: Python<'_>) -> String {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::balancer_stable_state::BalancerStablePoolIdentity>(
                self.pool_id,
            )
            .map(|i| address_utils::address_to_checksum_string(&i.vault))
            .unwrap_or_default()
        })
    }

    /// Balancer V2 stable pool ID (32-byte hex, ``0x``-prefixed). Empty string
    /// if not a Balancer stable pool.
    #[getter]
    fn balancer_stable_pool_id_hex(&self, py: Python<'_>) -> String {
        self.with_state(py, |core| {
            match core
                .pool_slice::<degenbot_pools::state::balancer_stable_state::BalancerStablePoolIdentity>(
                    self.pool_id,
                ) {
                Some(i) => alloy::hex::encode_prefixed(i.pool_id),
                None => String::new(),
            }
        })
    }

    // (The `balancer_stable_token_addresses` getter retired with ADR-085
    // D5 — see the weighted twin's note.)

    /// Balancer stable ``PyErc20Token`` companions (one per token, including
    /// BPT for Composable). Includes only tokens registered in this pool's
    /// Bot; unregistered tokens are skipped (the caller pre-registers them
    /// via ``Bot.register_token``). ``None`` if not a Balancer stable pool.
    fn get_balancer_stable_tokens(&self, py: Python<'_>) -> Option<Vec<PyErc20Token>> {
        self.with_state(py, |core| {
            let id = core
                .pool_slice::<degenbot_pools::state::balancer_stable_state::BalancerStablePoolIdentity>(
                    self.pool_id,
                )?;
            let mut out = Vec::with_capacity(id.tokens.len());
            for token_addr in &id.tokens {
                if !core.has_token(token_addr) {
                    return None;
                }
                out.push(PyErc20Token::new(Arc::clone(&self.core), *token_addr));
            }
            Some(out)
        })
    }

    /// Balancer stable scaling factors (one ``U256`` per token,
    /// rate-multiplied). Empty list if not a Balancer stable pool.
    #[getter]
    fn balancer_stable_scaling_factors(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let sfs: Vec<alloy::primitives::U256> = self.with_state(py, |core| {
            match core
                .pool_slice::<degenbot_pools::state::balancer_stable_state::BalancerStablePoolIdentity>(
                    self.pool_id,
                ) {
                Some(i) => i.scaling_factors.to_vec(),
                None => Vec::new(),
            }
        });
        let py_sfs: Vec<Py<PyAny>> = sfs
            .iter()
            .map(|b| crate::conversion::alloy::u256_to_py(py, b).map(pyo3::Bound::unbind))
            .collect::<PyResult<_>>()?;
        Ok(pyo3::types::PyList::new(py, py_sfs)?.into_any().unbind())
    }

    /// Balancer stable swap fee (fraction of `FEE_DENOMINATOR=1e18`). 0 if
    /// not a Balancer stable pool.
    #[getter]
    fn balancer_stable_swap_fee(&self, py: Python<'_>) -> u128 {
        self.with_state(py, |core| {
            core.pool_slice::<degenbot_pools::state::balancer_stable_state::BalancerStablePoolIdentity>(
                self.pool_id,
            )
            .map_or(0, |i| i.swap_fee)
        })
    }

    /// Whether the stored Balancer stable rate provider is static (performs
    /// no I/O). ``True`` when no provider was registered (the static
    /// ``1e18`` fallback). Drives the Python companion's
    /// ``requires_io_at_calculation_time`` / ``_should_warn_stale_rates``
    /// flags. ``False`` if not a Balancer stable pool — a dynamic provider
    /// is the more conservative default.
    #[getter]
    fn balancer_stable_rate_provider_is_static(&self, py: Python<'_>) -> bool {
        self.with_state(py, |core| {
            match core.pool_slice::<degenbot_pools::state::balancer_stable_state::BalancerStablePoolState>(
                self.pool_id,
            ) {
                Some(s) => s.rate_provider.as_ref().is_none_or(|p| p.is_static()),
                None => false,
            }
        })
    }

    /// Fetch rates from the stored Balancer stable rate provider at
    /// ``block_identifier`` (``None`` ⇔ latest). Returns the static
    /// ``1e18`` fallback (one per token) when no provider was registered.
    /// Returns ``None`` if not a Balancer stable pool.
    /// Raises:
    ///     `ValueError`: If the dynamic provider fetch failed.
    fn fetch_balancer_stable_rates(
        &self,
        py: Python<'_>,
        block_identifier: Option<u64>,
    ) -> PyResult<Option<Vec<u128>>> {
        let provider = self.with_state(py, |core| {
            let s = core
                .pool_slice::<degenbot_pools::state::balancer_stable_state::BalancerStablePoolState>(
                    self.pool_id,
                )?;
            s.rate_provider.clone()
        });
        let Some(provider) = provider else {
            // Static 1e18 fallback — one per token.
            let n = self.balance_vector(py).map_or(0, |view| view.n_tokens());
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
    /// Journals the prior balances then lands the new balances +
    /// `update_block`. Silent no-op (`False`) if this `pool_id` is not
    /// registered as a Balancer stable pool (so a companion built for a
    fn reserve_pair(&self, py: Python<'_>) -> PyResult<PyReservePairView> {
        let values = self.with_state(py, |core| match core.pool_entry(self.pool_id) {
            Some(PoolEntry::V2(pool)) => Some((
                address_utils::address_to_checksum_string(&pool.0.token0),
                address_utils::address_to_checksum_string(&pool.0.token1),
                pool.1.reserve0.to::<U256>(),
                pool.1.reserve1.to::<U256>(),
                pool.1.update_block,
            )),
            Some(PoolEntry::AerodromeV2(pool)) => Some((
                address_utils::address_to_checksum_string(&pool.0.token0),
                address_utils::address_to_checksum_string(&pool.0.token1),
                pool.1.reserve0.to::<U256>(),
                pool.1.reserve1.to::<U256>(),
                pool.1.update_block,
            )),
            Some(_) | None => None,
        });
        let Some((token0, token1, reserve0, reserve1, update_block)) = values else {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "pool is not a reserve-pair pool",
            ));
        };
        Ok(PyReservePairView {
            token0,
            token1,
            reserve0,
            reserve1,
            update_block,
        })
    }

    /// Structural concentrated-liquidity snapshot. Family mismatch is a loud refusal.
    fn concentrated_liquidity(&self, py: Python<'_>) -> PyResult<PyConcentratedLiquidityView> {
        let values = self.with_state(py, |core| match core.pool_entry(self.pool_id) {
            Some(PoolEntry::V3(pool)) => Some(concentrated_liquidity_snapshot_fields(
                address_utils::address_to_checksum_string(&pool.0.token0),
                address_utils::address_to_checksum_string(&pool.0.token1),
                pool.0.fee,
                pool.0.tick_spacing,
                &pool.1,
            )),
            Some(PoolEntry::V4(pool)) => Some(concentrated_liquidity_snapshot_fields(
                address_utils::address_to_checksum_string(&pool.0.pool_key.currency0),
                address_utils::address_to_checksum_string(&pool.0.pool_key.currency1),
                pool.0.pool_key.fee,
                pool.0.pool_key.tick_spacing,
                &pool.1,
            )),
            Some(_) | None => None,
        });
        let Some((
            token0,
            token1,
            fee,
            tick_spacing,
            sqrt_price_x96,
            liquidity,
            tick,
            update_block,
            tick_data,
            tick_bitmap_words,
            tick_data_block,
            coverage,
        )) = values
        else {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "pool is not a concentrated-liquidity pool",
            ));
        };
        Ok(PyConcentratedLiquidityView {
            token0,
            token1,
            fee,
            tick_spacing,
            sqrt_price_x96,
            liquidity,
            tick,
            update_block,
            tick_data,
            tick_bitmap_words,
            tick_data_block,
            coverage,
        })
    }

    /// Structural balance-vector snapshot. Family mismatch is a loud refusal.
    fn balance_vector(&self, py: Python<'_>) -> PyResult<PyBalanceVectorView> {
        let values = self.with_state(py, |core| match core.pool_entry(self.pool_id) {
            Some(PoolEntry::Curve(pool)) => Some((
                pool.0
                    .tokens
                    .iter()
                    .map(address_utils::address_to_checksum_string)
                    .collect::<Vec<_>>(),
                pool.1.balances.to_vec(),
                pool.1.update_block,
            )),
            Some(PoolEntry::BalancerWeighted(pool)) => Some((
                pool.0
                    .tokens
                    .iter()
                    .map(address_utils::address_to_checksum_string)
                    .collect::<Vec<_>>(),
                pool.1.balances.to_vec(),
                pool.1.update_block,
            )),
            Some(PoolEntry::BalancerStable(pool)) => Some((
                pool.0
                    .tokens
                    .iter()
                    .map(address_utils::address_to_checksum_string)
                    .collect::<Vec<_>>(),
                pool.1.balances.to_vec(),
                pool.1.update_block,
            )),
            Some(_) | None => None,
        });
        let Some((tokens, balances, update_block)) = values else {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "pool is not a balance-vector pool",
            ));
        };
        Ok(PyBalanceVectorView {
            tokens,
            balances,
            update_block,
        })
    }

    /// Number of reorg journal deltas for this registered pool.
    fn journal_len(&self, py: Python<'_>) -> usize {
        self.with_state(py, |core| core.pool_journal_len(self.pool_id).unwrap_or(0))
    }
    #[pyo3(signature = (balances, block_number))]
    fn apply_balances(
        &self,
        py: Python<'_>,
        balances: &Bound<'_, PyList>,
        block_number: u64,
    ) -> PyResult<bool> {
        let family = self.family_of(py)?;
        if !matches!(family, "curve" | "balancer-weighted" | "balancer-stable") {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "apply_balances requires a balance-vector pool, got {family}"
            )));
        }
        let values = balances
            .iter()
            .map(|item| crate::conversion::alloy::extract_python_u256(&item))
            .collect::<PyResult<Vec<_>>>()?;
        Ok(self.with_state_mut(py, |core| {
            core.apply_balance_update_by_pool_id(self.pool_id, values, block_number)
                .is_some()
        }))
    }

    /// Fetch this pool's current on-chain state and apply it when it differs —
    /// the update path's ONE core entry (`refresh_pool`).
    ///
    /// `io` is the driver's `BotIo` façade; the family's fetch/compare/apply
    /// choreography runs core-side against this handle's shared `BotState`.
    /// `block_number` is a resolved number, a canonical block tag
    /// (`"earliest"` / `"latest"` / `"pending"`), or `None` for the chain head.
    /// A tag resolves to a number here — at the seam that owns the RPC clock —
    /// before the core entry is called, so `refresh_pool` stays numeric.
    /// Returns `True` when a differing state was applied, `False` when the
    /// on-chain state already matches.
    ///
    /// Raises:
    ///     `ValueError`: The handle's pool id is not registered, or
    ///         `block_number` is a string that is not a canonical tag.
    #[pyo3(signature = (io, block_number=None))]
    fn refresh_via(
        &self,
        py: Python<'_>,
        io: &Bound<'_, crate::bot::py_bot_io::PyBotIo>,
        block_number: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        let construction_io = io.borrow().required_construction_io()?;
        let block_number =
            crate::bot::py_bot_io::resolve_block_against(py, &construction_io, block_number)?;
        let core = self.core.clone();
        let pool_id = self.pool_id;
        let outcome = py.detach(move || {
            runtime::get_runtime().block_on(async move {
                degenbot_bot::bot_core::refresh::refresh_pool(
                    &core,
                    pool_id,
                    &construction_io,
                    block_number,
                )
                .await
            })
        });
        match outcome {
            Ok(RefreshOutcome::Changed) => Ok(true),
            Ok(RefreshOutcome::Unchanged) => Ok(false),
            Err(RefreshError::Spec { message, .. }) => {
                Err(crate::bot::engine::SpecViolationError::new_err(message))
            }
            Err(RefreshError::Provider(e)) => Err(e.into()),
            Err(RefreshError::UnknownPool(_) | RefreshError::MissingIdentity { .. }) => {
                Err(pool_unregistered(pool_id))
            }
        }
    }
}

type ConcentratedLiquiditySnapshotFields = (
    String,
    String,
    u32,
    i32,
    U256,
    u128,
    i32,
    u64,
    Vec<(i32, (u128, i128, u64))>,
    Vec<i32>,
    u64,
    PyPoolTickCoverage,
);

fn concentrated_liquidity_snapshot_fields(
    token0: String,
    token1: String,
    fee: u32,
    tick_spacing: i32,
    state: &dyn degenbot_bot::bot_core::ConcentratedLiquidityPool,
) -> ConcentratedLiquiditySnapshotFields {
    use degenbot_bot::bot_core::PoolTickCoverage;
    let tick_data = state
        .tick_data()
        .iter()
        .map(|(tick, info)| {
            (
                *tick,
                (
                    info.liquidity_gross.to::<u128>(),
                    info.liquidity_net,
                    info.block,
                ),
            )
        })
        .collect();
    // The bitmap WORD positions the pool knows — the load-bearing key set.
    // The per-word bitmap VALUE is derived (dead); only which words are known
    // matters (Tracked words from the tick rows, Sparse words also from the
    // Rust `known_bitmap_words`). A `BTreeSet` keeps the emitted order stable.
    let mut words: std::collections::BTreeSet<i32> = std::collections::BTreeSet::new();
    for tick in state.tick_data().keys() {
        let compressed = *tick / tick_spacing;
        words.insert(compressed >> 8);
    }
    if state.coverage() == PoolTickCoverage::Sparse {
        for word in state.known_bitmap_words() {
            words.insert(*word);
        }
    }
    let coverage = PyPoolTickCoverage::from_core(state.coverage());
    (
        token0,
        token1,
        fee,
        tick_spacing,
        state.sqrt_price_x96(),
        state.liquidity(),
        state.tick(),
        state.update_block(),
        tick_data,
        words.into_iter().collect(),
        state.tick_data_block(),
        coverage,
    )
}

/// Extract a signed user-perspective request amount (negative = exact input).
/// The magnitude may span the full `uint256` range (a max-int256 probe), so
/// the sign is read off the integer and the absolute value narrows to `I256`.
fn extract_signed_amount(obj: &Bound<'_, PyAny>) -> PyResult<I256> {
    let type_error = || pyo3::exceptions::PyTypeError::new_err("amount_specified must be an int");
    let negative = obj.lt(0).map_err(|_| type_error())?;
    let magnitude = if negative {
        let absolute = obj.call_method0("__abs__").map_err(|_| type_error())?;
        crate::conversion::alloy::extract_python_u256(&absolute)?
    } else {
        crate::conversion::alloy::extract_python_u256(obj)?
    };
    let magnitude = I256::try_from(magnitude).map_err(|_| swap_math_overflow())?;
    Ok(if negative { -magnitude } else { magnitude })
}

/// Convert a signed 256-bit delta to a Python `int`.
fn i256_to_py(py: Python<'_>, value: I256) -> PyResult<Py<PyAny>> {
    let (sign, magnitude) = value.into_sign_and_abs();
    let magnitude = u128::try_from(magnitude).unwrap_or(u128::MAX);
    let signed: i128 = if sign.is_negative() {
        -i128::try_from(magnitude).unwrap_or(i128::MAX)
    } else {
        i128::try_from(magnitude).unwrap_or(i128::MAX)
    };
    Ok(signed.into_pyobject(py)?.into_any().unbind())
}

/// The swap-simulation read's family arm (ADR-037/t2): the computed read is
/// a sum by family, carried as data (never a subclass test).
#[pyclass(
    name = "SwapReadFamily",
    eq,
    eq_int,
    frozen,
    from_py_object,
    module = "degenbot._ffi"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PySwapReadFamily {
    /// A constant-product read: the deltas plus the POST-swap reserves.
    ReservePair,
    /// A concentrated-liquidity read: the deltas plus the end price state.
    ConcentratedLiquidity,
}

/// THE computed swap-simulation read (ADR-037/t2): `Pool.simulate` returns
/// this value; every refusal RAISES [`SwapRefused`] instead of borrowing a
/// status key. The arm is named by [`Self::family`] (data, never a subclass
/// test); the arm's own fields carry its end state (the other arm's are
/// `None` by construction).
#[pyclass(name = "SwapRead", module = "degenbot._ffi")]
pub struct PySwapRead {
    family: PySwapReadFamily,
    /// Signed per-token deltas, deposited-`+`/sent-`-` (the input side
    /// positive, the output side negative), keyed by token index.
    amount0_delta: I256,
    amount1_delta: I256,
    /// The concentrated-liquidity end price state (`None` on the
    /// reserve-pair arm: a constant-product pool has no price/tick state).
    sqrt_price_x96: Option<U256>,
    liquidity: Option<u128>,
    tick: Option<i32>,
    /// Whether the requested side was fully met (the concentrated-liquidity
    /// arm; the reserve-pair arm's exact-output semantics answer on the
    /// deltas alone).
    specified_met: bool,
    /// The POST-swap reserves (the reserve-pair arm only — the hypothetical
    /// read's own end state, which no caller may re-derive from the deltas).
    reserves_token0: Option<u128>,
    reserves_token1: Option<u128>,
}

#[pymethods]
impl PySwapRead {
    /// The family arm this read computed.
    #[getter]
    const fn family(&self) -> PySwapReadFamily {
        self.family
    }

    /// Signed token0 delta (deposited `+`, sent `-`).
    #[getter]
    fn amount0_delta(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        i256_to_py(py, self.amount0_delta)
    }

    /// Signed token1 delta (deposited `+`, sent `-`).
    #[getter]
    fn amount1_delta(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        i256_to_py(py, self.amount1_delta)
    }

    /// The end `sqrt_price_x96` (the concentrated-liquidity arm).
    #[getter]
    fn sqrt_price_x96(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        self.sqrt_price_x96
            .map(|v| crate::conversion::alloy::u256_to_py(py, &v).map(pyo3::Bound::unbind))
            .transpose()
    }

    /// The end in-range liquidity (the concentrated-liquidity arm).
    #[getter]
    const fn liquidity(&self) -> Option<u128> {
        self.liquidity
    }

    /// The end tick (the concentrated-liquidity arm).
    #[getter]
    const fn tick(&self) -> Option<i32> {
        self.tick
    }

    /// Whether the requested side was fully met (the concentrated-liquidity
    /// arm).
    #[getter]
    const fn specified_met(&self) -> bool {
        self.specified_met
    }

    /// The POST-swap token0 reserve (the reserve-pair arm).
    #[getter]
    const fn reserves_token0(&self) -> Option<u128> {
        self.reserves_token0
    }

    /// The POST-swap token1 reserve (the reserve-pair arm).
    #[getter]
    const fn reserves_token1(&self) -> Option<u128> {
        self.reserves_token1
    }
}

/// The swap-simulation refusal vocabulary (ADR-037/t2): the kind the ONE
/// Python adapter maps to the companions' exception tree.
#[pyclass(
    name = "SwapRefusalKind",
    eq,
    eq_int,
    frozen,
    from_py_object,
    module = "degenbot._ffi"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PySwapRefusalKind {
    /// Zero amount, a non-computable arithmetic/invariant outcome, or a
    /// non-computable family payload.
    NotComputable,
    /// Miss recovery ran but the fetch itself failed for the refused word.
    FetchFailed,
    /// Miss recovery was impossible or exhausted for the refused word.
    FetchExhausted,
}

pyo3::create_exception!(
    degenbot._ffi,
    SwapRefused,
    pyo3::exceptions::PyException,
    "The swap-simulation seam refused the read (ADR-037). args: (kind, word) —
     `kind` is a `SwapRefusalKind` member; `word` is the bitmap word the walk
     stalled on for the fetch arms, `None` elsewhere."
);

/// Raise the typed [`SwapRefused`] with `(kind, word)` args.
pub(crate) fn swap_refused_err(
    py: Python<'_>,
    kind: PySwapRefusalKind,
    word: Option<i32>,
) -> PyErr {
    let word_arg: Py<PyAny> = match word {
        // `i32::into_pyobject` is infallible; the arm documents that.
        Some(w) => match w.into_pyobject(py) {
            Ok(o) => o.into_any().unbind(),
            Err(e) => match e {},
        },
        None => py.None(),
    };
    let kind_arg: Py<PyAny> = match Bound::new(py, kind) {
        Ok(b) => b.unbind().into_any(),
        Err(e) => return e,
    };
    SwapRefused::new_err((kind_arg, word_arg))
}

/// THE sign-mapping site (ADR-037): an unsigned per-token amount pair becomes
/// signed per-token deltas in the deposited-`+`/sent-`-` convention — the
/// input side positive, the output side negative — keyed by the direction.
fn token_deltas(zero_for_one: bool, input: U256, output: U256) -> (I256, I256) {
    let input_delta = I256::try_from(input).unwrap_or(I256::MAX);
    let output_delta = -I256::try_from(output).unwrap_or(I256::MAX);
    if zero_for_one {
        (input_delta, output_delta)
    } else {
        (output_delta, input_delta)
    }
}

/// The concentrated-liquidity computed read: the signed deltas, the end
/// price state, and whether the requested side was fully met.
fn cl_swap_read(
    zero_for_one: bool,
    amount_specified: I256,
    amount0: U256,
    amount1: U256,
    end: (U256, u128, i32),
) -> PySwapRead {
    let (sqrt_price_x96, liquidity, tick) = end;
    let (input, output) = if zero_for_one {
        (amount0, amount1)
    } else {
        (amount1, amount0)
    };
    let (amount0_delta, amount1_delta) = token_deltas(zero_for_one, input, output);
    let specified_met = if amount_specified.is_negative() {
        (-amount_specified).into_raw() <= input
    } else {
        amount_specified.into_raw() <= output
    };
    PySwapRead {
        family: PySwapReadFamily::ConcentratedLiquidity,
        amount0_delta,
        amount1_delta,
        sqrt_price_x96: Some(sqrt_price_x96),
        liquidity: Some(liquidity),
        tick: Some(tick),
        specified_met,
        reserves_token0: None,
        reserves_token1: None,
    }
}

/// The reserve-pair computed read — the deltas PLUS the POST-swap reserves
/// (the hypothetical read's own end state, which no caller may re-derive
/// from the deltas). The concentrated-liquidity end scalars are absent by
/// construction: a constant-product pool has no price or tick state.
fn rp_swap_read(zero_for_one: bool, outcome: &ReservePairSwapOutcome) -> PySwapRead {
    let input = (-outcome.consumed).into_raw();
    let output = outcome.delivered.into_raw();
    let (amount0_delta, amount1_delta) = token_deltas(zero_for_one, input, output);
    PySwapRead {
        family: PySwapReadFamily::ReservePair,
        amount0_delta,
        amount1_delta,
        sqrt_price_x96: None,
        liquidity: None,
        tick: None,
        specified_met: false,
        reserves_token0: Some(outcome.end_reserves_token0.try_into().unwrap_or(u128::MAX)),
        reserves_token1: Some(outcome.end_reserves_token1.try_into().unwrap_or(u128::MAX)),
    }
}

/// The registration-lifecycle tick-data seed boundary: a Python
/// `{tick: (gross, net, block)}` mapping <-> the Rust
/// `HashMap<i32, TickInfo>`.
///
/// One home for the wire contract — the tuple arity, the `U128` gross
/// narrowing, the per-tick `block`, and the single error raised for a
/// malformed container, so no two seed sites can disagree about the shape.
/// The 2-tuple `(gross, net)` backfill form (the engine state-sync seams) is
/// the same contract with `block` filled from the caller's sync block — the
/// block at which each tick was last mutated/confirmed (the event block, the
/// registration seed block, or the bulk-sync block) — not a second error
/// vocabulary; `0` is a legal stamp, never a sentinel. `None` is the empty-map
/// convention on the registration entry points.
pub(crate) struct TickData;

impl TickData {
    /// The wire shape a malformed seed container is reported against; sharing
    /// one string keeps every seed site's failure indistinguishable.
    const MALFORMED: &str = "tick_data must be {tick: (liquidity_gross, liquidity_net, block)}";

    /// The ONE malformed-container error every seed entry point reports.
    fn malformed() -> PyErr {
        pyo3::exceptions::PyTypeError::new_err(Self::MALFORMED)
    }

    /// Narrow one `(gross, net, block)` triple onto the Rust tick row.
    fn info(gross: u128, net: i128, block: u64) -> TickInfo {
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(gross),
            liquidity_net: net,
            block,
        }
    }

    /// 3-tuple seed form: `{tick: (gross, net, block)}` -> `HashMap<i32, TickInfo>`.
    pub(crate) fn from_py(container: &Bound<'_, PyAny>) -> PyResult<HashMap<i32, TickInfo>> {
        let parsed: std::collections::HashMap<i32, (u128, i128, u64)> =
            container.extract().map_err(|_| Self::malformed())?;
        Ok(parsed
            .into_iter()
            .map(|(tick, (gross, net, block))| (tick, Self::info(gross, net, block)))
            .collect())
    }

    /// Override-state seed form: the Python `{tick: LiquidityAtTick}` mapping
    /// the concentrated liquidity companions carry, whose rows expose `liquidity_gross` /
    /// `liquidity_net` / `block` by name. The `TickInfo` translation stays in
    /// this one boundary, so no Python caller re-flattens the rows.
    pub(crate) fn from_py_liquidity_rows(
        container: &Bound<'_, PyAny>,
    ) -> PyResult<HashMap<i32, TickInfo>> {
        let dict = container.cast::<PyDict>().map_err(|_| Self::malformed())?;
        let mut out = HashMap::with_capacity(dict.len());
        for (key, row) in dict.iter() {
            let tick: i32 = key.extract().map_err(|_| Self::malformed())?;
            let gross: u128 = row
                .getattr("liquidity_gross")
                .and_then(|v| v.extract())
                .map_err(|_| Self::malformed())?;
            let net: i128 = row
                .getattr("liquidity_net")
                .and_then(|v| v.extract())
                .map_err(|_| Self::malformed())?;
            let block: u64 = row
                .getattr("block")
                .and_then(|v| v.extract())
                .map_err(|_| Self::malformed())?;
            out.insert(tick, Self::info(gross, net, block));
        }
        Ok(out)
    }

    /// `register_*_pool(tick_data=...)` convention: `None` -> empty map.
    pub(crate) fn from_py_optional(
        container: Option<Bound<'_, PyDict>>,
    ) -> PyResult<HashMap<i32, TickInfo>> {
        match container {
            Some(dict) => Self::from_py(dict.as_any()),
            None => Ok(HashMap::new()),
        }
    }

    /// 2-tuple backfill form: `{tick: (gross, net)}` -> `HashMap<i32, TickInfo>`
    /// with `block` filled from the caller's sync block (the block at which
    /// each tick was last mutated/confirmed — event block, registration seed,
    /// or bulk-sync block). `0` is a legal stamp, never a sentinel.
    pub(crate) fn from_py_pairs(
        container: &Bound<'_, PyAny>,
        block: u64,
    ) -> PyResult<HashMap<i32, TickInfo>> {
        let parsed: std::collections::HashMap<i32, (u128, i128)> =
            container.extract().map_err(|_| Self::malformed())?;
        Ok(parsed
            .into_iter()
            .map(|(tick, (gross, net))| (tick, Self::info(gross, net, block)))
            .collect())
    }

    /// Rust -> Python: serialize `(tick, gross, net, block)` rows into the
    /// `{tick: (gross, net, block)}` dict [`Self::from_py`] accepts, so a
    /// snapshot round-trips through one wire shape.
    pub(crate) fn to_py(
        py: Python<'_>,
        rows: impl IntoIterator<Item = (i32, u128, i128, u64)>,
    ) -> PyResult<Bound<'_, PyDict>> {
        let dict = PyDict::new(py);
        for (tick, gross, net, block) in rows {
            let tuple = PyTuple::new(
                py,
                [
                    gross.into_pyobject(py)?.into_any(),
                    net.into_pyobject(py)?.into_any(),
                    block.into_pyobject(py)?.into_any(),
                ],
            )?;
            dict.set_item(tick, tuple)?;
        }
        Ok(dict)
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// The green path: a 3-tuple seed narrows onto the Rust row and serializes
    /// back through the same wire shape.
    #[test]
    fn tick_data_seed_round_trips_through_the_one_wire_shape() {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item(42, (7u128, -3i128, 100u64)).unwrap();
            let map = TickData::from_py(dict.as_any()).unwrap();
            let info = map.get(&42).unwrap();
            assert_eq!(info.liquidity_gross.to::<u128>(), 7);
            assert_eq!(info.liquidity_net, -3);
            assert_eq!(info.block, 100);

            let back = TickData::to_py(py, [(42, 7, -3, 100)]).unwrap();
            let row = back.get_item(42).unwrap().unwrap();
            assert_eq!(row.extract::<(u128, i128, u64)>().unwrap(), (7, -3, 100));
        });
    }

    /// The 2-tuple backfill variant fills the caller's REAL sync block onto
    /// every row (a real block, never a hardcoded `0`).
    #[test]
    fn tick_data_two_tuple_backfill_fills_the_real_sync_block() {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item(42, (7u128, -3i128)).unwrap();
            let map = TickData::from_py_pairs(dict.as_any(), 21_000_000).unwrap();
            assert_eq!(map.get(&42).unwrap().block, 21_000_000);
        });
    }

    /// The computed read is the typed `SwapRead` pyclass with the CL arm's
    /// scalars; a refusal RAISES `SwapRefused` carrying `(kind, word)` —
    /// no status key anywhere.
    #[test]
    fn simulate_read_is_the_typed_value_and_refusals_raise() {
        Python::attach(|py| {
            let computed = cl_swap_read(
                true,
                I256::try_from(-1_000_i64).unwrap(),
                U256::from(1_000_u64),
                U256::from(400_u64),
                (U256::from(1_u64), 7, 3),
            );
            assert_eq!(computed.family, PySwapReadFamily::ConcentratedLiquidity);
            assert!(computed.specified_met);
            assert_eq!(computed.liquidity, Some(7));
            assert_eq!(computed.tick, Some(3));

            let err = swap_refused_err(py, PySwapRefusalKind::FetchFailed, Some(-7));
            let args = err.value(py).getattr("args").unwrap();
            let kind: PySwapRefusalKind = args.get_item(0).unwrap().extract().unwrap();
            assert_eq!(kind, PySwapRefusalKind::FetchFailed);
            assert_eq!(args.get_item(1).unwrap().extract::<i32>().unwrap(), -7);

            let err = swap_refused_err(py, PySwapRefusalKind::NotComputable, None);
            let args = err.value(py).getattr("args").unwrap();
            let kind: PySwapRefusalKind = args.get_item(0).unwrap().extract().unwrap();
            assert_eq!(kind, PySwapRefusalKind::NotComputable);
            assert!(args.get_item(1).unwrap().is_none());
        });
    }

    /// The red path: a wrong-arity container raises the ONE typed error.
    #[test]
    fn tick_data_wrong_arity_raises_the_one_error() {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item(42, (7u128, -3i128)).unwrap();
            let err = TickData::from_py(dict.as_any())
                .expect_err("a 2-tuple is not the 3-tuple seed shape");
            assert_eq!(
                err.to_string(),
                format!("TypeError: {}", TickData::MALFORMED)
            );
        });
    }
}

/// Read-only reserve-pair view exposed to Python.
#[pyclass(name = "ReservePairView", module = "degenbot._ffi")]
pub struct PyReservePairView {
    token0: String,
    token1: String,
    reserve0: U256,
    reserve1: U256,
    update_block: u64,
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

    #[getter]
    fn update_block(&self) -> u64 {
        self.update_block
    }
}

/// The concentrated liquidity tick-map coverage discriminant crossing the FFI (survey C7):
/// Python compares this enum instead of probing a stringified twin.
/// Mirrors `degenbot_pools::state::v3_state::PoolTickCoverage` (the fact's home).
#[pyclass(
    name = "PoolTickCoverage",
    eq,
    hash,
    frozen,
    from_py_object,
    module = "degenbot._ffi"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PyPoolTickCoverage {
    Sparse,
    Tracked,
}

impl PyPoolTickCoverage {
    /// Convert from the core enum.
    #[must_use]
    pub const fn from_core(coverage: degenbot_bot::bot_core::PoolTickCoverage) -> Self {
        match coverage {
            degenbot_bot::bot_core::PoolTickCoverage::Sparse => Self::Sparse,
            degenbot_bot::bot_core::PoolTickCoverage::Tracked => Self::Tracked,
        }
    }

    /// The matching core enum.
    #[must_use]
    pub const fn to_core(self) -> degenbot_bot::bot_core::PoolTickCoverage {
        match self {
            Self::Sparse => degenbot_bot::bot_core::PoolTickCoverage::Sparse,
            Self::Tracked => degenbot_bot::bot_core::PoolTickCoverage::Tracked,
        }
    }
}

/// Read-only concentrated-liquidity view exposed to Python.
#[pyclass(name = "ConcentratedLiquidityView", module = "degenbot._ffi")]
pub struct PyConcentratedLiquidityView {
    token0: String,
    token1: String,
    fee: u32,
    tick_spacing: i32,
    sqrt_price_x96: U256,
    liquidity: u128,
    tick: i32,
    update_block: u64,
    tick_data: Vec<(i32, (u128, i128, u64))>,
    tick_bitmap_words: Vec<i32>,
    tick_data_block: u64,
    coverage: PyPoolTickCoverage,
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

    #[getter]
    fn update_block(&self) -> u64 {
        self.update_block
    }

    #[getter]
    fn tick_data_block(&self) -> u64 {
        self.tick_data_block
    }

    #[getter]
    fn coverage(&self) -> PyPoolTickCoverage {
        self.coverage
    }

    #[getter]
    fn tick_data(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let dict = PyDict::new(py);
        for (tick, (gross, net, block)) in &self.tick_data {
            dict.set_item(
                tick,
                PyTuple::new(
                    py,
                    [
                        gross.into_pyobject(py)?.into_any().unbind(),
                        net.into_pyobject(py)?.into_any().unbind(),
                        block.into_pyobject(py)?.into_any().unbind(),
                    ],
                )?,
            )?;
        }
        Ok(dict.into_any().unbind())
    }

    /// The bitmap WORD positions the pool knows (the load-bearing key set).
    /// The per-word bitmap VALUE is retired — it derives from `tick_data`.
    #[getter]
    fn tick_bitmap<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PySet>> {
        let set = PySet::empty(py)?;
        for word in &self.tick_bitmap_words {
            set.add(*word)?;
        }
        Ok(set)
    }
}

/// Read-only balance-vector view exposed to Python.
#[pyclass(name = "BalanceVectorView", module = "degenbot._ffi")]
pub struct PyBalanceVectorView {
    tokens: Vec<String>,
    balances: Vec<U256>,
    update_block: u64,
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

    #[getter]
    fn update_block(&self) -> u64 {
        self.update_block
    }
}
