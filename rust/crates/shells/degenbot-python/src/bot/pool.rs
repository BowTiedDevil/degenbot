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
use degenbot_bot::bot_core::InstallWordOutcome;
use degenbot_pools::registry::PoolEntry;
use hashbrown::HashMap;
use std::sync::Arc;

use pyo3::types::{PyDict, PyList, PyTuple};

use crate::bot::journal_err_to_py;
use degenbot_bot::bot_core::state_lock::StateLock;
use degenbot_bot::bot_core::swap_simulation::{SwapOutcome, SwapRead, SwapRequest};
use degenbot_bot::bot_core::{BotState, TickInfo};

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
/// provider, if present (Option B: route the Chain arm
/// through the pure-Rust [`AlloyTickBootstrapRpc`]).
/// Returns `None` when the `PyBotIo` has no native alloy provider (legacy
/// Python test doubles) → the caller passes `chain=None` to the assemble helper,
/// preserving the current (no-Chain-arm) behavior.
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
            let parsed: std::collections::HashMap<i32, (u128, i128, u64)> = bound
                .extract()
                .map_err(|_| FetchTickWordError::FetchFailed)?;
            let ticks = parsed
                .into_iter()
                .map(|(tick, (gross, net, blk))| {
                    (
                        tick,
                        TickInfo {
                            liquidity_gross: alloy::primitives::U128::from(gross),
                            liquidity_net: net,
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
/// Does not own any state — all data lives in Rust inside `BotState`.
#[pyclass(name = "Pool", skip_from_py_object, module = "degenbot._ffi")]
pub struct PyLiquidityPool {
    core: Arc<StateLock<BotState>>,
    pool_id: u64,
    chain_id: u64,
}

impl PyLiquidityPool {
    /// RATR5A/CXRHW3 probe (mechanical lock-freedom invariant, pair-review
    /// condition 1): the caller thread holds the GIL; the `BotState` WRITE is
    /// required free. `try_write` is instant and non-blocking - safe at any
    /// depth (it never parks).
    #[must_use]
    pub fn state_write_is_free(&self) -> bool {
        self.core
            .try_write_at(degenbot_bot::bot_core::state_lock::LockSite::Python)
            .is_some()
    }

    /// discover the missing bitmap words for
    /// the request (a collect-only transient walk), fetch them LOCK-FREE
    /// through the pool stored fetcher, and install them under SHORT
    /// writes with the fingerprint re-check. Bounded 3 passes; returns
    /// true when the sim body can run with miss recovery disarmed.
    fn ensure_missing_words_staged(
        &self,
        py: Python<'_>,
        block: u64,
        request: &degenbot_bot::bot_core::swap_simulation::SwapRequest,
    ) -> bool {
        for pass in 0..3u8 {
            // no fetcher stored: non-CL or a Tracked pool - the sim body's
            // disarm contract never fetches; nothing to stage.
            let Some(missing) = self.with_state(py, |core| {
                core.swap_missing_words(block, self.pool_id, request)
            }) else {
                return false;
            };
            if missing.is_empty() {
                return true;
            }
            // Stage the whole batch under ONE short write (the per-word
            // fingerprints gate the installs individually), then fetch
            // lock-free, then install with the fingerprint re-check.
            let Some(staged) = self.with_state_mut(py, |core| {
                missing
                    .iter()
                    .map(|word| {
                        core.stage_word_fetch_by_pool_id(self.pool_id, *word, block, pass > 0)
                    })
                    .collect::<Option<Vec<_>>>()
            }) else {
                return false;
            };
            // Fetches: GIL-attached (we hold it), NO state lock held.
            let mut fetched = Vec::new();
            for staged_word in &staged {
                match staged_word.fetch() {
                    Ok(f) => fetched.push(f),
                    Err(_) => return false,
                }
            }
            // Installs: short writes, fingerprint-gated. Any Raced means
            // the pump wrote this pool mid-batch: retry the whole pass.
            let raced = self.with_state_mut(py, |core| {
                staged
                    .iter()
                    .zip(fetched.iter())
                    .any(|(staged_word, fetched_word)| {
                        matches!(
                            core.install_word_fetch(staged_word, fetched_word),
                            InstallWordOutcome::Raced
                        )
                    })
            });
            if !raced {
                return true;
            }
        }
        false
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
                .read_at(degenbot_bot::bot_core::state_lock::LockSite::Python);
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
                .write_at(degenbot_bot::bot_core::state_lock::LockSite::Python);
            f(&mut guard)
        })
    }

    /// The registered family tag for this handle's pool id.
    /// Raises `ValueError` when the id is not registered: a handle is built
    /// from a registered pool, so an unknown id is a family gap, never an
    /// `""` sentinel a caller could mistake for a missing field.
    fn family_of(&self, py: Python<'_>) -> PyResult<&'static str> {
        self.with_state(py, |core| core.pool_family(self.pool_id))
            .ok_or_else(|| {
                pyo3::exceptions::PyValueError::new_err(format!(
                    "pool {} is not registered",
                    self.pool_id
                ))
            })
    }

    /// Clone-out the stored `CurveDataProvider` (if any) for this handle's
    /// Curve state, releasing the read guard before a (potentially
    /// re-entrant) provider call.
    fn curve_provider(
        &self,
        py: Python<'_>,
    ) -> Option<std::sync::Arc<dyn degenbot_pools::curve_data_provider::CurveDataProvider>> {
        self.with_state(
            py,
            |core| -> Option<
                std::sync::Arc<dyn degenbot_pools::curve_data_provider::CurveDataProvider>,
            > { core.get_curve_pool(self.pool_id)?.data_provider.clone() },
        )
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
            &std::sync::Arc<dyn degenbot_pools::curve_data_provider::CurveDataProvider>,
        ) -> Result<
            alloy::primitives::U256,
            degenbot_pools::curve_data_provider::CurveDataProviderError,
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

    /// Shared override-sim inner: extracts Python args and calls the gate's
    /// `simulate_override` (transient-state fetch+retry policy).
    #[expect(clippy::too_many_arguments)]
    fn sim_override_inner(
        &self,
        py: Python<'_>,
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
            Some(v) if !v.is_none() => Some(crate::conversion::alloy::extract_python_u256(v)?),
            _ => None,
        };
        let mut over = degenbot_bot::bot_core::swap_simulation::OverrideSwap {
            pool_id: self.pool_id,
            request: SwapRequest {
                zero_for_one,
                // User perspective: positive = exact-output (pool delivers),
                // negative = exact-input. Engine conventions handled in-gate.
                amount_specified: if exact_output {
                    I256::try_from(amount).map_err(|_| {
                        pyo3::exceptions::PyValueError::new_err(
                            "Pool swap math overflowed uint256 intermediate (on-chain getAmountOut SafeMath revert)",
                        )
                    })?
                } else {
                    -I256::try_from(amount).map_err(|_| {
                        pyo3::exceptions::PyValueError::new_err(
                            "Pool swap math overflowed uint256 intermediate (on-chain getAmountOut SafeMath revert)",
                        )
                    })?
                },
                sqrt_price_limit,
            },
            sqrt_price_x96: override_sqrt,
            liquidity: override_liquidity,
            tick: override_tick,
            tick_data: rust_tick_data,
        };
        // stage missing words OUTSIDE the read lock (bounded
        // passes) - the sim runs with miss recovery disarmed so no fetch can
        // execute under the caller read guard.
        for _ in 0..3u8 {
            // A None stored fetcher only aborts when there is something to
            // stage — a complete hypothetical (no misses) must still sim.
            let staged = self.with_state(py, |core| {
                let fetcher = core.stored_fetcher_for_pool(self.pool_id);
                let missing = core.override_missing_words(&over);
                missing.map(|missing| (fetcher, missing))
            });
            let Some((fetcher, missing)) = staged else {
                return Ok(None);
            };
            if missing.is_empty() {
                break;
            }
            let Some(fetcher) = fetcher else {
                // Misses exist but no fetcher is stored: the hypothetical
                // cannot be backfilled (RATR5A staged pass), fail as None
                // exactly like the disarmed sim's FetchExhausted arm.
                return Ok(None);
            };
            // Fetches: GIL-attached (we hold it), NO state lock held.
            let mut staged_words = Vec::new();
            for word in &missing {
                match fetcher.fetch_missing_tick_word(self.pool_id, *word, block) {
                    Ok(f) => staged_words.push(f),
                    Err(_) => return Ok(None),
                }
            }
            // Merge fetched ticks into the caller-owned override map.
            for f in &staged_words {
                for (tick, info) in &f.ticks {
                    over.tick_data.insert(*tick, info.clone());
                }
            }
        }
        let outcome = self.with_state(py, |core| core.simulate_override_disarmed(&over, block));
        match outcome {
            Ok(o) => Ok(o),
            // A non-CL family has no transient CL state to simulate — a typed
            // refusal, never a bare `None` an override miss could produce.
            Err(u) => Err(pyo3::exceptions::PyValueError::new_err(format!(
                "simulate_override: pool {} family {:?} has no concentrated-liquidity override state",
                u.pool_id, u.family
            ))),
        }
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
    /// product / CL swap math overflows a `uint256` intermediate (mirroring
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
        let amount_specified = -I256::try_from(amount).map_err(|_| {
            pyo3::exceptions::PyValueError::new_err(
                "Pool swap math overflowed uint256 intermediate (on-chain getAmountOut SafeMath revert)",
            )
        })?;
        // DISARMED — miss recovery cannot run (no-raise-on-miss: sparse => 0).
        // cdbc03bb (RATR5A Finding on ae2c4124f): two DISTINCT error classes:
        // - FetchExhausted/Failed (miss recovery) → U256::ZERO per the no-raise contract.
        // - NotComputable (V2 mul overflow >= 2^256) → ValueError raise (on-chain parity).
        // The disarm conversion collapsed them; this restores the distinction by
        // keeping the SwapRead return and matching outside.
        let result = self.with_state_mut(py, |core| {
            core.swap_simulation_disarmed(
                0,
                self.pool_id,
                &degenbot_bot::bot_core::swap_simulation::SwapRequest {
                    zero_for_one,
                    amount_specified,
                    sqrt_price_limit: None,
                },
            )
        });
        // cdbc03bb: MATHEMATICS-OVERFLOW (amount * gamma >= 2^256), NOT a
        // sparse-map miss (FetchExhausted/Failed => 0 by the no-raise
        // contract). The two classes are distinct: different consumer
        // contract (companion LiquidityPoolError, on-chain parity).
        match &result {
            SwapRead::Computed(outcome) => {
                let out = outcome.delivered_unsigned();
                let bound = crate::conversion::alloy::u256_to_py(py, &out)?;
                Ok(bound.unbind())
            }
            SwapRead::NotComputable => Err(pyo3::exceptions::PyValueError::new_err(
                "Pool swap math overflowed uint256 intermediate (on-chain getAmountOut SafeMath revert)",
            )),
            SwapRead::UnknownPool { pool_id } => Err(pyo3::exceptions::PyValueError::new_err(
                format!("swap_simulation: pool {pool_id} is not registered"),
            )),
            SwapRead::UnsupportedFamily { pool_id, family } => {
                Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "swap_simulation: pool {pool_id} family {family} is not supported for this operation"
                )))
            }
            SwapRead::FetchFailed { .. } | SwapRead::FetchExhausted { .. } => {
                let bound = crate::conversion::alloy::u256_to_py(py, &U256::ZERO)?;
                Ok(bound.unbind())
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
        let outcome = self.with_state(py, |core| {
            use degenbot_bot::bot_core::swap_simulation::simulate_balancer_pair_out;
            simulate_balancer_pair_out(
                core,
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
            None => Err(pyo3::exceptions::PyValueError::new_err(
                "Pool swap math overflowed uint256 intermediate (on-chain getAmountOut SafeMath revert)",
            )),
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
        let outcome = self.with_state(py, |core| {
            use degenbot_bot::bot_core::swap_simulation::simulate_balancer_pair_in_given_out;
            simulate_balancer_pair_in_given_out(
                core,
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
            None => Err(pyo3::exceptions::PyValueError::new_err(
                "Pool swap math overflowed uint256 intermediate (on-chain getAmountIn SafeMath revert)",
            )),
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
        // ADR-037: exact-output request; required input = |consumed|. Legacy
        // silent-0 contract preserved here until the Python tail task.
        let request = degenbot_bot::bot_core::swap_simulation::SwapRequest {
            zero_for_one,
            amount_specified: I256::try_from(amount).map_err(|_| {
                pyo3::exceptions::PyValueError::new_err(
                    "Pool swap math overflowed uint256 intermediate (on-chain getAmountOut SafeMath revert)",
                )
            })?,
            sqrt_price_limit: None,
        };
        // DISARMED — miss recovery cannot run (no-raise-on-miss).
        // NotComputable (V2 mul overflow) is a distinct class, NOT a miss:
        // documented legacy contract (silent-0 preserved per ADR-037).
        let read = self.with_state_mut(py, |core| {
            core.swap_simulation_disarmed(0, self.pool_id, &request)
        });
        let result = match read {
            SwapRead::Computed(outcome) => (-match &outcome {
                SwapOutcome::V2(o) => o.consumed,
                SwapOutcome::V3(o) | SwapOutcome::V4(o) => o.consumed,
            })
            .into_raw(),
            // A registered family with no exact-output path is a typed gap,
            // not the legacy silent-0 contract.
            SwapRead::UnsupportedFamily { pool_id, family } => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "calculate_tokens_in: pool {pool_id} family {family} has no exact-output path"
                )));
            }
            _ => U256::ZERO,
        };
        let bound = crate::conversion::alloy::u256_to_py(py, &result)?;
        Ok(bound.unbind())
    }

    /// Fetch+retry exact-input swap for sparse V3/V4 pools (ADR-005 slice 3).
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
        let request = degenbot_bot::bot_core::swap_simulation::SwapRequest {
            zero_for_one,
            amount_specified: -I256::try_from(amount).map_err(|_| {
                pyo3::exceptions::PyValueError::new_err(
                    "Pool swap math overflowed uint256 intermediate (on-chain getAmountOut SafeMath revert)",
                )
            })?,
            sqrt_price_limit: None,
        };
        // ADR-037: the fetch-retry policy lives behind the gate; unrecovered
        // core write lock (bounded passes); the sim runs with miss recovery
        // disarmed so no fetch can execute under the caller write guard.
        if !self.ensure_missing_words_staged(py, block, &request) {
            let zero = crate::conversion::alloy::u256_to_py(py, &U256::ZERO)?;
            return Ok(zero.unbind());
        }
        let result = self.with_state_mut(py, |core| {
            match core.swap_simulation_disarmed(block, self.pool_id, &request) {
                SwapRead::Computed(outcome) => outcome.delivered_unsigned(),
                _ => U256::ZERO,
            }
        });
        let bound = crate::conversion::alloy::u256_to_py(py, &result)?;
        Ok(bound.unbind())
    }

    /// Fetch+retry full-outcome exact-input swap for sparse V3/V4 pools
    /// (ADR-005 slice 3b). Like [`calculate_tokens_out_with_fetch`][Self::calculate_tokens_out_with_fetch]
    /// but returns the full swap outcome tuple so the companion can build
    /// `final_state`.
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
        let amount_specified = -I256::try_from(amount)
            .map_err(|_| pyo3::exceptions::PyOverflowError::new_err("amount does not fit I256"))?;
        let sqrt_price_limit = match sqrt_price_limit_x96 {
            Some(v) if !v.is_none() => Some(crate::conversion::alloy::extract_python_u256(v)?),
            _ => None,
        };
        let request = degenbot_bot::bot_core::swap_simulation::SwapRequest {
            zero_for_one,
            amount_specified,
            sqrt_price_limit,
        };
        // stage missing words OUTSIDE the write lock (bounded
        // passes) - the sim below runs with miss recovery disarmed so no
        // fetch can execute under the caller write guard.
        if !self.ensure_missing_words_staged(py, block, &request) {
            return Ok(None);
        }
        let read = self.with_state_mut(py, |core| {
            core.swap_simulation_disarmed(block, self.pool_id, &request)
        });
        let SwapRead::Computed(SwapOutcome::V3(payload) | SwapOutcome::V4(payload)) = read else {
            return Ok(None);
        };
        // ADR-037: an amount-modifying hook may have invalidated the
        // standard-math result — surface the archived exception (approximate
        // amounts attached) instead of silently returning a wrong number.
        if payload
            .caveats
            .contains(degenbot_bot::bot_core::swap_simulation::Caveats::HOOKED_POOL)
        {
            return Err(crate::bot::engine::PossibleInaccurateResult::new_err(
                format!(
                    "pool has an amount-modifying V4 hook; approximation consumed={} delivered={}",
                    -payload.consumed, payload.delivered
                ),
            ));
        }
        let (amount0, amount1) = payload.raw_token_amounts(zero_for_one);
        let tuple = pyo3::types::PyTuple::new(
            py,
            [
                crate::conversion::alloy::u256_to_py(py, &amount0)?.unbind(),
                crate::conversion::alloy::u256_to_py(py, &amount1)?.unbind(),
                crate::conversion::alloy::u256_to_py(py, &payload.end_sqrt_price_x96)?.unbind(),
                payload.end_liquidity.into_pyobject(py)?.into_any().unbind(),
                payload.end_tick.into_pyobject(py)?.into_any().unbind(),
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
        let amount_specified = I256::try_from(amount).map_err(|_| {
            pyo3::exceptions::PyValueError::new_err(
                "Pool swap math overflowed uint256 intermediate (on-chain getAmountOut SafeMath revert)",
            )
        })?;
        let sqrt_price_limit = match sqrt_price_limit_x96 {
            Some(v) if !v.is_none() => Some(crate::conversion::alloy::extract_python_u256(v)?),
            _ => None,
        };
        let request = degenbot_bot::bot_core::swap_simulation::SwapRequest {
            zero_for_one,
            // Exact-output request: POSITIVE user-perspective (pool delivers).
            // The V3/V4 engine sign conventions are handled inside the gate.
            amount_specified,
            sqrt_price_limit,
        };
        // stage missing words OUTSIDE the write lock (bounded
        // passes) - the sim below runs with miss recovery disarmed so no
        // fetch can execute under the caller write guard.
        if !self.ensure_missing_words_staged(py, block, &request) {
            return Ok(None);
        }
        let read = self.with_state_mut(py, |core| {
            core.swap_simulation_disarmed(block, self.pool_id, &request)
        });
        let payload = match read {
            SwapRead::Computed(SwapOutcome::V3(payload) | SwapOutcome::V4(payload)) => payload,
            // A non-CL family cannot produce the CL 5-tuple this exact-output
            // seam promises: a typed gap, never a bare `None`.
            SwapRead::UnsupportedFamily { pool_id, family } => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "simulate_exact_output_swap_with_fetch: pool {pool_id} family {family} has no exact-output path"
                )));
            }
            _ => return Ok(None),
        };
        // ADR-037: an amount-modifying hook may have invalidated the
        // standard-math result — surface the archived exception (approximate
        // amounts attached) instead of silently returning a wrong number.
        if payload
            .caveats
            .contains(degenbot_bot::bot_core::swap_simulation::Caveats::HOOKED_POOL)
        {
            return Err(crate::bot::engine::PossibleInaccurateResult::new_err(
                format!(
                    "pool has an amount-modifying V4 hook; approximation consumed={} delivered={}",
                    -payload.consumed, payload.delivered
                ),
            ));
        }
        let (amount0, amount1) = payload.raw_token_amounts(zero_for_one);
        let tuple = pyo3::types::PyTuple::new(
            py,
            [
                crate::conversion::alloy::u256_to_py(py, &amount0)?.unbind(),
                crate::conversion::alloy::u256_to_py(py, &amount1)?.unbind(),
                crate::conversion::alloy::u256_to_py(py, &payload.end_sqrt_price_x96)?.unbind(),
                payload.end_liquidity.into_pyobject(py)?.into_any().unbind(),
                payload.end_tick.into_pyobject(py)?.into_any().unbind(),
            ],
        )?;
        Ok(Some(tuple.into_any().unbind()))
    }

    /// Simulate an exact-input swap over a HYPOTHETICAL override pool state,
    /// with fetch+retry for sparse misses.
    /// Builds a transient V3/V4 state from `override_sqrt_price_x96`,
    /// `override_liquidity`, `override_tick` + `override_tick_data`
    /// (`{tick: (liquidity_gross, liquidity_net, block)}`, same shape as
    /// `register_*_pool`'s `tick_data`), reusing the registered pool's fee /
    /// `tick_spacing`. On a sparse miss, the fetcher is called + the word's
    /// ticks are merged into the TRANSIENT state (NOT registered `BotState`),
    /// and the sim retries. Returns the same 5-tuple as
    /// `simulate_swap_with_fetch`, or `None` if not computable.
    #[pyo3(signature = (zero_for_one, amount_in, block, override_sqrt_price_x96, override_liquidity, override_tick, override_tick_data, sqrt_price_limit_x96=None))]
    #[expect(clippy::too_many_arguments)]
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
            py,
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
    #[expect(clippy::too_many_arguments)]
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
            py,
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

        let result = self.with_state(py, |core| {
            core.encode_swap(self.pool_id, zero_for_one, amount, recip)
        });

        match result {
            Ok(call) => Ok(Some((
                format!("{:#x}", call.to),
                format!("0x{}", bytes_to_hex(&call.data)),
                call.value.to::<u64>(),
            ))),
            // The Python handle's own `encode_swap` resolves its pool id at
            // construction, so an unregistered id keeps the `None` not-found
            // contract.
            Err(degenbot_bot::bot_core::EncodeSwapError::NotRegistered { .. }) => Ok(None),
            Err(e) => Err(pyo3::exceptions::PyValueError::new_err(format!(
                "encode_swap: {e}"
            ))),
        }
    }

    // --- State read getters (ADR-005 slice 4 step 2) ---
    // These read the shared `BotState` under a read guard. Immutable identity
    // (token0/token1/factory/fees/address) stays on the Python companion —
    // only mutable state + the reorg journal delegate to Rust.

    // --- V2 identity getters (ADR-005 identity slice) ---
    // Immutable per-pool identity read from V2PoolState (+ the registration
    // descriptor) so the Python companion's `_from_py_pool(py_pool)` can be
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
                "v2" => core.get_v2_identity(self.pool_id).map(|i| i.address),
                "v3" => core.get_v3_identity(self.pool_id).map(|i| i.address),
                "aerodrome-v2" => core.get_aerodrome_identity(self.pool_id).map(|i| i.address),
                "curve" => core.get_curve_identity(self.pool_id).map(|i| i.address),
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
                "v2" => core.get_v2_identity(self.pool_id).map(|i| i.factory),
                "v3" => core.get_v3_identity(self.pool_id).map(|i| i.factory),
                "aerodrome-v2" => core.get_aerodrome_identity(self.pool_id).map(|i| i.factory),
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
                "v2" => core.get_v2_identity(self.pool_id).map(|i| i.deployer),
                "v3" => core.get_v3_identity(self.pool_id).map(|i| i.deployer),
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
                "v2" => core.get_v2_identity(self.pool_id).map(|i| i.init_hash),
                "v3" => core.get_v3_identity(self.pool_id).map(|i| i.init_hash),
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
            core.get_v2_identity(self.pool_id)
                .map(|s| s.fee_token0)
                .unwrap_or_default()
        })
    }

    /// `token1→token0` fee parameters: `(gamma_numer, fee_denom)`. `(0, 0)` if
    /// not a V2 pool.
    #[getter]
    fn fee_token1(&self, py: Python<'_>) -> (u64, u64) {
        self.with_state(py, |core| {
            core.get_v2_identity(self.pool_id)
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
                .get_v2_identity(self.pool_id)
                .map(|d| d.variant.as_str().to_string())
                .unwrap_or_default(),
            "aerodrome-v2" => core
                .get_aerodrome_identity(self.pool_id)
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
            core.get_v2_identity(self.pool_id)
                .is_some_and(|d| d.stable_swap)
        })
    }

    /// The pool-family tag for this handle's registered pool (`"v2"`,
    /// `"v3"`, `"v4"`, `"curve"`, `"balancer-weighted"`,
    /// `"balancer-stable"`). Raises if unregistered — a handle always
    /// references a registered pool, so the `""` sentinel is retired.
    /// This is the uniform family-guard primitive every `_from_py_pool`
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
            core.get_v2_identity(self.pool_id)
                .and_then(|d| d.fee_denominator)
        })
    }

    /// The resolved `DexIdentity` for this pool's registered variant, with the
    /// JSON-sourced deployer + `init_hash` merged in (Fork A, NSAZ4X). `None` if
    /// not a V2 pool. The Python companion reads this to recover deployer /
    /// init-hash without taking constructor args. Protocol-const fields
    /// (fees/ABI shape) come from the variant preset; `factory`/`deployer` /
    /// `init_hash` come from the identity (the verified values stored at
    /// registration).
    #[getter]
    fn dex(&self, py: Python<'_>) -> Option<crate::bot::dex_identity::PyDexIdentity> {
        self.with_state(py, |core| {
            let id = core.get_v2_identity(self.pool_id)?;
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
    // companion wraps these via `Erc20Token._from_py_token` so the
    // `_from_py_pool(py_pool)` seam needs no token args — the Polars
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
        }))
    }

    /// `PyErc20Token` handle for token1, or `None` if not registered in the
    /// shared `BotState`.
    fn get_token1(&self, py: Python<'_>) -> PyResult<Option<PyErc20Token>> {
        let family = self.family_of(py)?;
        Ok(self.with_state(py, |core| {
            let token_addr = match family {
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
        }))
    }

    fn get_balancer_tokens(&self, py: Python<'_>) -> Option<Vec<PyErc20Token>> {
        self.with_state(py, |core| {
            let identity = core.get_balancer_weighted_identity(self.pool_id)?;
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

    // --- V3 state read getters (plan-101 slice 8a) ---
    // Mirror the V2 family but read the V3PoolState entry. All getters take
    // one read guard and return None-defaulted values when the pool_id is not
    // a registered V3 pool (matching the V2 getters' behavior on V2).

    // --- V4 identity getters (ADR-005 sealed seam) ---
    // Read off V4PoolIdentity so UniswapV4Pool._from_py_pool is self-describing.

    /// Pool manager contract address (EIP-55 checksummed hex). Empty string if
    /// not a V4 pool.
    #[getter]
    fn pool_manager_address(&self, py: Python<'_>) -> String {
        self.with_state(py, |core| {
            core.get_v4_identity(self.pool_id)
                .map(|i| address_utils::address_to_checksum_string(&i.pool_manager))
                .unwrap_or_default()
        })
    }

    /// On-chain V4 pool ID (32-byte hex, ``0x``-prefixed). Empty string if not
    /// a V4 pool.
    #[getter]
    fn pool_id_hex(&self, py: Python<'_>) -> String {
        self.with_state(py, |core| match core.get_v4_identity(self.pool_id) {
            Some(i) => format!("0x{}", bytes_to_hex(&i.pool_id)),
            None => String::new(),
        })
    }

    /// Hook contract address (EIP-55 checksummed hex). Empty string if not a
    /// V4 pool.
    #[getter]
    fn hook_address(&self, py: Python<'_>) -> String {
        self.with_state(py, |core| {
            core.get_v4_identity(self.pool_id)
                .map(|i| address_utils::address_to_checksum_string(&i.pool_key.hooks))
                .unwrap_or_default()
        })
    }

    // --- Aerodrome V2 identity getters (ADR-005 Aerodrome state port) ---

    /// Aerodrome V2 Solidly stable-invariant flag. `false` for volatile mode
    /// or non-Aerodrome pools.
    #[getter]
    fn aerodrome_stable(&self, py: Python<'_>) -> bool {
        self.with_state(py, |core| {
            core.get_aerodrome_identity(self.pool_id)
                .is_some_and(|d| d.stable)
        })
    }

    /// Aerodrome V2 unidirectional fee as `(fee_numer, fee_denom)`. `(0, 0)` if
    /// not an Aerodrome pool.
    #[getter]
    fn aerodrome_fee(&self, py: Python<'_>) -> (u64, u64) {
        self.with_state(py, |core| {
            core.get_aerodrome_identity(self.pool_id)
                .map(|d| d.fee)
                .unwrap_or_default()
        })
    }

    /// Aerodrome V2 token0 ERC-20 decimal count. `0` for non-Aerodrome pools.
    #[getter]
    fn aerodrome_token0_decimals(&self, py: Python<'_>) -> u8 {
        self.with_state(py, |core| {
            core.get_aerodrome_identity(self.pool_id)
                .map(|d| d.token0_decimals)
                .unwrap_or_default()
        })
    }

    /// Aerodrome V2 token1 ERC-20 decimal count. `0` for non-Aerodrome pools.
    #[getter]
    fn aerodrome_token1_decimals(&self, py: Python<'_>) -> u8 {
        self.with_state(py, |core| {
            core.get_aerodrome_identity(self.pool_id)
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
            core.get_balancer_weighted_identity(self.pool_id)
                .map(|i| address_utils::address_to_checksum_string(&i.address))
                .unwrap_or_default()
        })
    }

    /// Balancer V2 vault contract address (EIP-55 checksummed hex).
    /// Empty string if not a Balancer weighted pool.
    #[getter]
    fn balancer_vault(&self, py: Python<'_>) -> String {
        self.with_state(py, |core| {
            core.get_balancer_weighted_identity(self.pool_id)
                .map(|i| address_utils::address_to_checksum_string(&i.vault))
                .unwrap_or_default()
        })
    }

    /// Balancer V2 pool ID (32-byte hex, ``0x``-prefixed). Empty string if
    /// not a Balancer weighted pool.
    #[getter]
    fn balancer_pool_id_hex(&self, py: Python<'_>) -> String {
        self.with_state(py, |core| {
            match core.get_balancer_weighted_identity(self.pool_id) {
                Some(i) => format!("0x{}", bytes_to_hex(&i.pool_id)),
                None => String::new(),
            }
        })
    }

    /// Balancer weighted token addresses (EIP-55 checksummed hex). Empty list
    /// if not a Balancer weighted pool.
    #[getter]
    fn balancer_token_addresses(&self, py: Python<'_>) -> Vec<String> {
        self.with_state(py, |core| {
            match core.get_balancer_weighted_identity(self.pool_id) {
                Some(i) => i
                    .tokens
                    .iter()
                    .map(address_utils::address_to_checksum_string)
                    .collect(),
                None => Vec::new(),
            }
        })
    }

    /// Balancer weighted denormalized weights (one `U256` per token).
    /// Empty list if not a Balancer weighted pool.
    #[getter]
    fn balancer_weights(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let weights: Vec<alloy::primitives::U256> = self.with_state(py, |s| {
            s.get_balancer_weighted_identity(self.pool_id)
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
            s.get_balancer_weighted_identity(self.pool_id)
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
            s.get_balancer_weighted_identity(self.pool_id)
                .map(|i| i.swap_fee)
                .unwrap_or_default()
        })
    }

    /// Balancer weighted Math pool implementation version (1 or 2). 0 if not
    /// a Balancer weighted pool.
    #[getter]
    fn balancer_pow_version(&self, py: Python<'_>) -> u8 {
        self.with_state(py, |s| {
            s.get_balancer_weighted_identity(self.pool_id)
                .map(|i| i.pow_version)
                .unwrap_or_default()
        })
    }

    // --- Mutations (per-handle, pool_id-keyed) ---

    // --- Aerodrome V2 reorg journal ---

    // --- V3 mutations (plan-101 slice 8a) ---
    // Pool-id-keyed — the handle already holds the canonical pool_id, so no
    // address resolution is needed (single lock, single lookup).

    /// Apply a V3/V4 `Swap` event: journals the scalar priors then lands the
    /// new `sqrt_price_x96`/`liquidity`/`tick` at `block_number`.
    /// Swap events change the V3 scalars but NOT the tick data — the
    /// `tick_priors` Vec is empty here (unlike `PyBot.update_v3_pool`, which
    /// accepts tick updates from decoded Swap logs when they carry tick
    /// mutations).
    /// Raises:
    ///     `ValueError`: If `pool_id` is not registered as a V3/V4 pool.
    #[pyo3(signature = (sqrt_price_x96, liquidity, tick, block_number))]
    fn apply_swap(
        &self,
        py: Python<'_>,
        sqrt_price_x96: &Bound<'_, PyAny>,
        liquidity: &Bound<'_, PyAny>,
        tick: i32,
        block_number: u64,
    ) -> PyResult<()> {
        let spx = crate::conversion::alloy::extract_python_u256(sqrt_price_x96)?;
        let liq = crate::conversion::alloy::extract_python_u256(liquidity)?.to::<u128>();
        // family-dispatching apply. Routes V4 pools to the V4 apply
        // path (previously this called `apply_v3_swap_by_pool_id`
        // unconditionally, which no-op'd on `PoolEntry::V4` and silently
        // dropped every Python-side V4 update). The dispatcher is one write
        // guard + two O(1) lookups; the single Python `apply_swap` API is
        // preserved.
        let _ = self.with_state_mut(py, |s| {
            s.apply_swap_by_pool_id(self.pool_id, spx, liq, tick, block_number, &[])
        });
        Ok(())
    }

    /// Registration/seed genesis anchor (two-stamp rule): push a
    /// `before == after` reorg-journal delta at `block_number` WITHOUT
    /// advancing either clock, so a split-seed (price at HEAD, tick map at the
    /// DB block) pool keeps a non-empty journal for mid-window reorg restore.
    /// Returns `True` on a registered V3/V4 pool; raises `ValueError` for a
    /// registered non-CL family (no CL journal to seed) or an unregistered id.
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

    /// Apply a V3 Mint/Burn event (liquidity update) via the handle.
    /// Initializes (or removes) tick entries at `tick_lower`/`tick_upper`,
    /// journals the priors for reorg rollback, invalidates the tick-range
    /// cache. Does NOT change the V3 scalars (`sqrt_price_x96`/`liquidity`/
    /// `tick`) — Mint/Burn is a tick-only event per ADR-004. The active
    /// `liquidity` scalar adjustments (when `current_tick` is in range) are
    /// applied by the engine's own path; this handle method is the raw
    /// `tick_data` mutation.
    /// Returns `True` when the update applied to a registered V3/V4 pool;
    /// raises `ValueError` for a registered non-CL family (no CL tick state to
    /// mutate) or an unregistered id.
    #[pyo3(signature = (tick_lower, tick_upper, liquidity_delta, block_number))]
    fn apply_liquidity_update(
        &self,
        py: Python<'_>,
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
        let applied = self.with_state_mut(py, |s| {
            s.apply_liquidity_update_by_pool_id(
                self.pool_id,
                tick_lower,
                tick_upper,
                delta,
                block_number,
            )
        });
        match applied {
            Ok(_) => Ok(true),
            Err(e) => Err(pyo3::exceptions::PyValueError::new_err(format!(
                "apply_liquidity_update: {e}"
            ))),
        }
    }

    /// Backfill an unknown tick-bitmap word for this pool (T2 FBJTUM — the
    /// write-path gate's fetch seam).
    /// STAGED fetch — the multi-second fetch (`Python::attach` + the
    /// companion's serial web3 RPC) runs with the `BotState` write guard
    /// RELEASED; the fetcher re-acquires the GIL via `Python::attach` and
    /// the pump applies events to other pools through the whole window.
    /// Choreography per attempt: (1) short write — stage (fetcher + tick
    /// fingerprint), (2) fetch — no lock held, (3) short write — install,
    /// merging only if the pool was not mutated during the fetch. A race
    /// (the pump applied an event for THIS pool mid-fetch) retries the
    /// stage+fetch bounded times rather than applying the overlay clobber.
    /// Returns `False` when no fetcher is stored OR the fetch failed/exhausted
    /// its retries — the caller RAISES rather than applying the event over
    /// an unknown word.
    #[pyo3(signature = (word, block))]
    fn ensure_word_known(&self, py: Python<'_>, word: i64, block: u64) -> PyResult<bool> {
        let word_i32 = i32::try_from(word)
            .map_err(|_| pyo3::exceptions::PyOverflowError::new_err("word must fit in i32"))?;
        // Bounded retries: each retry refetches (the RPC is the slow part);
        // a race requires a same-pool event to land inside the multi-second
        // fetch window, so three attempts exhaust only pathological bursts.
        // The retry attempt re-derives the fetch context from the pool clock
        // (RATR5A Finding-1(b)), not from the caller's stale block.
        for attempt in 0..3u8 {
            let Some(staged) = self.with_state_mut(py, |s| {
                s.stage_word_fetch_by_pool_id(self.pool_id, word_i32, block, attempt > 0)
            }) else {
                return Ok(false);
            };
            // NO state lock held across this fetch.
            let Ok(fetched) = staged.fetch() else {
                return Ok(false);
            };
            let outcome = self.with_state_mut(py, |s| s.install_word_fetch(&staged, &fetched));
            match outcome {
                InstallWordOutcome::Merged => return Ok(true),
                InstallWordOutcome::Failed => return Ok(false),
                InstallWordOutcome::Raced => {}
            }
        }
        Ok(false)
    }

    /// The pool's tick-map coverage (T2 FBJTUM): `"sparse"` or `"tracked"`
    /// for a registered V3/V4 pool, `None` for any other pool family. The
    /// Python companion's sparse-word gate reads this — Rust's coverage is
    /// sparse-map backfill). Mirrors the Python `UniswapV3Pool.update_tick_data`
    /// the companion delegates here once it's rewritten over the handle
    /// (plan-101 slice 8b). No journal delta (full-sync; the pump is the
    /// authority for event-derived ticks — mirrors `sync_v3_pool_state`).
    /// `tick_data` is the SAME shape `tick_data_snapshot` returns:
    /// `{tick: (liquidity_gross, liquidity_net, block)}` — the write path is
    /// symmetric with the read path, + the companion converts its
    /// `LiquidityAtTick` objects to this tuple shape at the boundary (matching
    /// how V2 converts its Python `Fraction` fees to the Rust `gamma_numer` at
    /// the boundary).
    /// `tick_bitmap`: the KEYS are the checked bitmap words. For Sparse pools
    /// the FFI records them in the Rust `known_bitmap_words` (a checked word
    /// is never re-fetched — the contract that retires the companion's
    /// `_bitmap_override` shadow); the VALUES are NOT stored (the bitmap is
    /// derived from the `tick_data` rows — see `tick_bitmap_snapshot`).
    /// Tracked pools never record (their bitmap is complete).
    /// Scalars (`sqrt_price_x96`/`liquidity`/`tick`) are UNCHANGED — this is
    /// tick-only. `update_block` advances to `block` if newer (monotonic).
    /// Returns `True` if the replace applied to a registered V3/V4 pool,
    /// `False` if this `pool_id` is a V2 pool or unregistered (silent no-op —
    /// mirrors the `apply_liquidity_update` family contract).
    #[pyo3(signature = (tick_bitmap, tick_data, block))]
    fn update_tick_data(
        &self,
        py: Python<'_>,
        tick_bitmap: &Bound<'_, PyAny>,
        tick_data: &Bound<'_, PyDict>,
        block: u64,
    ) -> PyResult<bool> {
        // Checked-word extraction (arch-review cand 4, T1): the bitmap KEYS
        // are words the caller has checked on-chain; Sparse pools record them
        // in `known_bitmap_words`, Tracked never do (the gate is in core —
        // `mark_bitmap_words_known` no-ops there). Values are not stored —
        // derivation from the `tick_data` rows is the bit source.
        let dict = tick_bitmap.cast::<PyDict>().map_err(|_| {
            pyo3::exceptions::PyTypeError::new_err("update_tick_data: tick_bitmap must be a dict")
        })?;
        let words: Vec<i32> = dict
            .iter()
            .map(|(key, _)| {
                key.extract::<i32>().map_err(|_| {
                    pyo3::exceptions::PyTypeError::new_err(
                        "update_tick_data: tick_bitmap keys must be ints",
                    )
                })
            })
            .collect::<Result<_, _>>()?;
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
                    liquidity_net: net,
                    block: tick_block,
                },
            );
        }
        // The write guard lives inside py.detach (GIL/BotState inversion fix,
        // 2026-08-21 run-9): GIL released while parked on the BotState lock.
        let applied = self.with_state_mut(py, |core| {
            let applied = core.sync_tick_data_by_pool_id(self.pool_id, map, block);
            if applied {
                let _ = core.mark_bitmap_words_known_by_pool_id(self.pool_id, &words);
            }
            applied
        });
        Ok(applied)
    }

    // --- Curve state read getters + mutations (ADR-005 slice 11a state port) ---

    // --- Curve identity getters (ADR-005 identity extension, BOMDRK) ---

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
            let id = core.get_curve_identity(self.pool_id)?;
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
            let id = core.get_curve_identity(self.pool_id)?;
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
            let id = core.get_curve_identity(self.pool_id)?;
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
        self.with_state(py, |core| match core.get_curve_identity(self.pool_id) {
            Some(i) => i.use_lending.to_vec(),
            None => Vec::new(),
        })
    }

    /// Curve per-token `precision_multipliers` (one `U256` per token). Empty
    /// list for a non-Curve pool.
    #[getter]
    fn curve_precision_multipliers(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let pms: Vec<alloy::primitives::U256> =
            self.with_state(py, |core| match core.get_curve_identity(self.pool_id) {
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
    /// pool's state (ADR-005 JFGCHJ). `False` for non-Curve pools or Curve
    /// pools registered without a provider (the no-I/O fixture case).
    #[getter]
    fn curve_has_data_provider(&self, py: Python<'_>) -> bool {
        self.with_state(py, |core| match core.get_curve_pool(self.pool_id) {
            Some(s) => s.data_provider.is_some(),
            None => false,
        })
    }

    // --- Curve identity getters (ADR-005 BQM2OA identity-from-handle) ---

    /// Curve amplification coefficient `A` (raw). 0 for a non-Curve handle.
    #[getter]
    fn curve_a_coefficient(&self, py: Python<'_>) -> u128 {
        self.with_state(py, |core| {
            core.get_curve_identity(self.pool_id)
                .map_or(0, |i| i.a_coefficient)
        })
    }

    /// Curve swap fee (`FEE_DENOMINATOR` units). 0 for a non-Curve handle.
    #[getter]
    fn curve_fee(&self, py: Python<'_>) -> u64 {
        self.with_state(py, |core| {
            core.get_curve_identity(self.pool_id).map_or(0, |i| i.fee)
        })
    }

    /// Curve admin-fee share (`FEE_DENOMINATOR` units). 0 for a non-Curve handle.
    #[getter]
    fn curve_admin_fee(&self, py: Python<'_>) -> u64 {
        self.with_state(py, |core| {
            core.get_curve_identity(self.pool_id)
                .map_or(0, |i| i.admin_fee)
        })
    }

    /// Curve rate multipliers (one `U256` per token). Empty for a non-Curve pool.
    #[getter]
    fn curve_rate_multipliers(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let rms: Vec<alloy::primitives::U256> =
            self.with_state(py, |core| match core.get_curve_identity(self.pool_id) {
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
            core.get_curve_identity(self.pool_id)
                .map_or(0, |i| i.swap_style)
        })
    }

    /// Curve `lending_rate_style` discriminant. 0 for a non-Curve handle.
    #[getter]
    fn curve_lending_rate_style(&self, py: Python<'_>) -> u8 {
        self.with_state(py, |core| {
            core.get_curve_identity(self.pool_id)
                .map_or(0, |i| i.lending_rate_style)
        })
    }

    /// Curve `d_variant` discriminant. 0 for a non-Curve handle.
    #[getter]
    fn curve_d_variant(&self, py: Python<'_>) -> u8 {
        self.with_state(py, |core| {
            core.get_curve_identity(self.pool_id)
                .map_or(0, |i| i.d_variant)
        })
    }

    /// Curve `y_variant` discriminant. 0 for a non-Curve handle.
    #[getter]
    fn curve_y_variant(&self, py: Python<'_>) -> u8 {
        self.with_state(py, |core| {
            core.get_curve_identity(self.pool_id)
                .map_or(0, |i| i.y_variant)
        })
    }

    /// Curve `yd_variant` discriminant. 0 for a non-Curve handle.
    #[getter]
    fn curve_yd_variant(&self, py: Python<'_>) -> u8 {
        self.with_state(py, |core| {
            core.get_curve_identity(self.pool_id)
                .map_or(0, |i| i.yd_variant)
        })
    }

    /// Curve `metapool_rate_style` discriminant. 0 for a non-Curve handle.
    #[getter]
    fn curve_metapool_rate_style(&self, py: Python<'_>) -> u8 {
        self.with_state(py, |core| {
            core.get_curve_identity(self.pool_id)
                .map_or(0, |i| i.metapool_rate_style)
        })
    }

    /// Curve `metapool_underlying_style` discriminant. 0 for a non-Curve handle.
    #[getter]
    fn curve_metapool_underlying_style(&self, py: Python<'_>) -> u8 {
        self.with_state(py, |core| {
            core.get_curve_identity(self.pool_id)
                .map_or(0, |i| i.metapool_underlying_style)
        })
    }

    /// Curve base-pool address (EIP-55 checksummed). `None` for plain pools.
    /// `None` for a non-Curve handle.
    // `Option<Option<_>>` distinguishes no-pool / pool-no-base / has-base.
    #[expect(clippy::option_option)]
    fn curve_base_pool_address(&self, py: Python<'_>) -> Option<Option<String>> {
        self.with_state(py, |core| {
            let id = core.get_curve_identity(self.pool_id)?;
            Some(
                id.base_pool
                    .map(|a| address_utils::address_to_checksum_string(&a)),
            )
        })
    }

    /// The Curve pool's token companion handles, resolved via the shared
    /// `BotState` token registry. `None` if this is not a Curve pool or any
    /// token isn't registered (mirror of `get_balancer_tokens`). The companion
    /// wraps each via `Erc20Token._from_py_token`.
    fn get_curve_tokens(&self, py: Python<'_>) -> Option<Vec<PyErc20Token>> {
        self.with_state(py, |core| {
            let identity = core.get_curve_identity(self.pool_id)?;
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
        })
    }

    /// The Curve pool's dedicated LP-token companion handle. `None` ⇔ the
    /// pool token is itself the LP (the common plain-pool case; the companion
    /// falls back to `tokens[0]`). Returns `None` (outer) for a non-Curve pool
    /// or if the LP token isn't registered.
    fn get_curve_lp_token(&self, py: Python<'_>) -> Option<PyErc20Token> {
        self.with_state(py, |core| {
            let identity = core.get_curve_identity(self.pool_id)?;
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
    /// caller construct ERC20 companions *before* `_from_py_pool`. `None` for
    /// a non-Curve pool.
    fn curve_token_addresses(&self, py: Python<'_>) -> Option<Vec<String>> {
        self.with_state(py, |core| {
            let identity = core.get_curve_identity(self.pool_id)?;
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
            let identity = core.get_curve_identity(self.pool_id)?;
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
            let identity = core.get_curve_identity(self.pool_id)?;
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
    /// `CurveStableswapPool._from_py_pool(handle.curve_base_pool())`.
    fn curve_base_pool(&self, py: Python<'_>) -> Option<PyLiquidityPool> {
        self.with_state(py, |core| {
            let id = core.get_curve_identity(self.pool_id)?;
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
            core.curve_get_dy(
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
            core.curve_get_dy_underlying(
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
            core.curve_calc_token_amount(self.pool_id, &amounts, deposit, block_number)
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
            core.curve_calc_withdraw_one_coin(self.pool_id, token_amount, i, block_number)
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
            core.get_curve_pool(self.pool_id)
                .and_then(|st| st.data_provider.clone())
        });
        let provider = provider?;
        provider.block_number().ok()
    }

    fn fetch_curve_block_timestamp(&self, py: Python<'_>, block_number: u64) -> Option<u64> {
        let provider = self.with_state(py, |core| {
            core.get_curve_pool(self.pool_id)
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
            core.get_curve_pool(self.pool_id)
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
            core.get_curve_pool(self.pool_id)
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

    fn fetch_curve_base_cache_updated(&self, py: Python<'_>, block_number: u64) -> Option<u64> {
        let provider = self.with_state(py, |core| {
            core.get_curve_pool(self.pool_id)
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

    // --- Balancer weighted state read getters + mutations
    //     (ADR-005 slice 12a state port) ---

    // --- Balancer stable state read getters + mutations
    //     (ADR-005 slice 12c state port) ---

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
            s.get_balancer_stable_identity(self.pool_id)
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
            s.get_balancer_stable_identity(self.pool_id)
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
            s.get_balancer_stable_identity(self.pool_id)
                .map_or(0, |i| i.invariant_version)
        })
    }

    // --- Balancer stable identity getters (ADR-005 sealed seam, MBWSGP) ---

    /// Balancer V2 stable pool's Vault singleton (EIP-55 checksummed). Empty
    /// string if not a Balancer stable pool.
    #[getter]
    fn balancer_stable_vault(&self, py: Python<'_>) -> String {
        self.with_state(py, |core| {
            core.get_balancer_stable_identity(self.pool_id)
                .map(|i| address_utils::address_to_checksum_string(&i.vault))
                .unwrap_or_default()
        })
    }

    /// Balancer V2 stable pool ID (32-byte hex, ``0x``-prefixed). Empty string
    /// if not a Balancer stable pool.
    #[getter]
    fn balancer_stable_pool_id_hex(&self, py: Python<'_>) -> String {
        self.with_state(py, |core| {
            match core.get_balancer_stable_identity(self.pool_id) {
                Some(i) => format!("0x{}", bytes_to_hex(&i.pool_id)),
                None => String::new(),
            }
        })
    }

    /// Balancer stable token addresses (EIP-55 checksummed hex). Empty list
    /// if not a Balancer stable pool.
    #[getter]
    fn balancer_stable_token_addresses(&self, py: Python<'_>) -> Vec<String> {
        self.with_state(py, |core| {
            match core.get_balancer_stable_identity(self.pool_id) {
                Some(i) => i
                    .tokens
                    .iter()
                    .map(address_utils::address_to_checksum_string)
                    .collect(),
                None => Vec::new(),
            }
        })
    }

    /// Balancer stable ``PyErc20Token`` companions (one per token, including
    /// BPT for Composable). Includes only tokens registered in this pool's
    /// Bot; unregistered tokens are skipped (the caller pre-registers them
    /// via ``Bot.register_token``). ``None`` if not a Balancer stable pool.
    fn get_balancer_stable_tokens(&self, py: Python<'_>) -> Option<Vec<PyErc20Token>> {
        self.with_state(py, |core| {
            let id = core.get_balancer_stable_identity(self.pool_id)?;
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
            match core.get_balancer_stable_identity(self.pool_id) {
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
            core.get_balancer_stable_identity(self.pool_id)
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
            match core.get_balancer_stable_pool(self.pool_id) {
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
            let s = core.get_balancer_stable_pool(self.pool_id)?;
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
            Some(PoolEntry::V3(pool)) => Some(cl_snapshot_fields(
                address_utils::address_to_checksum_string(&pool.0.token0),
                address_utils::address_to_checksum_string(&pool.0.token1),
                pool.0.fee,
                pool.0.tick_spacing,
                &pool.1,
            )),
            Some(PoolEntry::V4(pool)) => Some(cl_snapshot_fields(
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
            tick_bitmap,
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
            tick_bitmap,
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

    /// Apply a reserve-pair sync command for V2 or Aerodrome V2.
    #[pyo3(signature = (reserve0, reserve1, block_number))]
    fn apply_sync(
        &self,
        py: Python<'_>,
        reserve0: &Bound<'_, PyAny>,
        reserve1: &Bound<'_, PyAny>,
        block_number: u64,
    ) -> PyResult<()> {
        let family = self.family_of(py)?;
        if !matches!(family, "v2" | "aerodrome-v2") {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "apply_sync requires a reserve-pair pool, got {family}"
            )));
        }
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
        let _ = self.with_state_mut(py, |core| {
            core.apply_sync_by_pool_id(self.pool_id, r0, r1, block_number)
        });
        Ok(())
    }

    #[pyo3(signature = (block))]
    fn discard_before_block(&self, py: Python<'_>, block: u64) -> PyResult<()> {
        if self.family_of(py).is_err() {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "pool {} is not registered",
                self.pool_id
            )));
        }
        self.with_state_mut(py, |core| {
            core.discard_pool_before_block(self.pool_id, block)
                .unwrap_or(Ok(()))
                .map_err(journal_err_to_py)
        })
    }

    /// Restore the registered pool to its landed-at state before `block`.
    #[pyo3(signature = (block))]
    fn restore_before_block(&self, py: Python<'_>, block: u64) -> PyResult<()> {
        if self.family_of(py).is_err() {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "pool {} is not registered",
                self.pool_id
            )));
        }
        self.with_state_mut(py, |core| {
            match core.restore_pool_before_block(self.pool_id, block) {
                None | Some(Ok(())) => Ok(()),
                Some(Err(e)) => Err(journal_err_to_py(e)),
            }
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
}

type ClSnapshotFields = (
    String,
    String,
    u32,
    i32,
    U256,
    u128,
    i32,
    u64,
    Vec<(i32, (u128, i128, u64))>,
    Vec<(i32, (U256, u64))>,
    u64,
    String,
);

fn cl_snapshot_fields(
    token0: String,
    token1: String,
    fee: u32,
    tick_spacing: i32,
    state: &dyn degenbot_bot::bot_core::ConcentratedLiquidityPool,
) -> ClSnapshotFields {
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
    let mut words: std::collections::BTreeMap<i32, (U256, u64)> = std::collections::BTreeMap::new();
    let one = U256::from(1u64);
    for tick in state.tick_data().keys() {
        let compressed = *tick / tick_spacing;
        let word = compressed >> 8;
        let bit = compressed.rem_euclid(256) as u32;
        words
            .entry(word)
            .and_modify(|(bitmap, _)| *bitmap |= one << bit)
            .or_insert((one << bit, state.update_block()));
    }
    if state.coverage() == PoolTickCoverage::Sparse {
        for word in state.known_bitmap_words() {
            words
                .entry(*word)
                .or_insert((U256::ZERO, state.tick_data_block()));
        }
    }
    let coverage = match state.coverage() {
        PoolTickCoverage::Sparse => "sparse",
        PoolTickCoverage::Tracked => "tracked",
    }
    .to_string();
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
) -> PyResult<HashMap<i32, degenbot_bot::bot_core::TickInfo>> {
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
                    liquidity_net: net,
                    block: blk,
                },
            )
        })
        .collect())
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
    tick_bitmap: Vec<(i32, (U256, u64))>,
    tick_data_block: u64,
    coverage: String,
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
    fn coverage(&self) -> String {
        self.coverage.clone()
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

    #[getter]
    fn tick_bitmap(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let dict = PyDict::new(py);
        for (word, (bitmap, block)) in &self.tick_bitmap {
            let value = crate::conversion::alloy::u256_to_py(py, bitmap)?;
            dict.set_item(
                word,
                PyTuple::new(
                    py,
                    [value.unbind(), block.into_pyobject(py)?.into_any().unbind()],
                )?,
            )?;
        }
        Ok(dict.into_any().unbind())
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
