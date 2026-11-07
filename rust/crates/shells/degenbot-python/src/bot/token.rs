//! `PyErc20Token` — thin Python handle over a token address key into `BotState`.
//!
//! Shares the same `Arc<parking_lot::RwLock<BotState>>` as the owning `PyBot` (one
//! Rust-owned `BotState`, many thin Python handles). Part of the Polars-inspired
//! three-layer topology — see `docs/adr/ADR-005-polars-inspired-three-layer-architecture.md`.

use crate::prelude::*;
use std::str::FromStr;
use std::sync::Arc;

use alloy::primitives::{Address, U256};

use degenbot_substrate::session_registry::TokenIdentity;
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::BotState;

use crate::conversion::alloy::u256_to_py;

/// A thin Python handle to a token registered in `BotState`.
///
/// Does not own any state — all data lives in Rust inside `BotState`.
#[pyclass(name = "Erc20Token", module = "degenbot._ffi")]
pub struct PyErc20Token {
    core: Arc<StateLock<BotState>>,
    address: Address,
}

impl PyErc20Token {
    /// Create a new thin token handle.
    pub(crate) const fn new(core: Arc<StateLock<BotState>>, address: Address) -> Self {
        Self { core, address }
    }

    /// This handle's token identity (the session cache key).
    pub(crate) const fn token_identity(&self) -> TokenIdentity {
        TokenIdentity::Erc20(self.address)
    }

    /// Sanctioned `BotState` read access for pymethod code (GIL/`BotState`
    /// inversion class): the guard is acquired INSIDE `py.detach`. Same
    /// invariant contract as `PyBot::with_state` — see the doc comment there.
    pub(crate) fn with_state<T>(&self, py: Python<'_>, f: impl FnOnce(&BotState) -> T + Send) -> T
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
}

/// Build the `token not registered` error. Must run under the GIL (`PyErr`
/// construction touches Python); call it AFTER the detached scope returns.
fn token_not_registered(addr: &Address) -> pyo3::PyErr {
    pyo3::exceptions::PyRuntimeError::new_err(format!("token not registered: {addr}"))
}

#[pymethods]
impl PyErc20Token {
    /// The token contract address (hex string).
    #[getter]
    fn address(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let addr_str = format!("{}", self.address);
        Ok(addr_str.into_pyobject(py)?.into_any().unbind())
    }

    /// Token decimals (e.g. 6 for USDC, 18 for WETH).
    ///
    /// GIL hygiene (GIL/`BotState` inversion class): the `BotState` read guard is
    /// acquired inside `py.detach`; owned data comes out and any `PyErr` is
    /// built after the GIL is re-acquired.
    #[getter]
    fn decimals(&self, py: Python<'_>) -> PyResult<u8> {
        self.with_state(py, |s| {
            s.token_entry(&self.address).map(|entry| entry.decimals)
        })
        .ok_or_else(|| token_not_registered(&self.address))
    }

    /// Chain ID where this token is registered.
    #[getter]
    fn chain_id(&self, py: Python<'_>) -> PyResult<u64> {
        self.with_state(py, |s| {
            s.token_entry(&self.address).map(|entry| entry.chain_id)
        })
        .ok_or_else(|| token_not_registered(&self.address))
    }

    /// Token symbol (e.g. "USDC", "WETH").
    #[getter]
    fn symbol(&self, py: Python<'_>) -> PyResult<String> {
        self.with_state(py, |s| {
            s.token_entry(&self.address)
                .map(|entry| entry.symbol.clone())
        })
        .ok_or_else(|| token_not_registered(&self.address))
    }

    /// Token name (e.g. "Wrapped Ether").
    #[getter]
    fn name(&self, py: Python<'_>) -> PyResult<String> {
        self.with_state(py, |s| {
            s.token_entry(&self.address).map(|entry| entry.name.clone())
        })
        .ok_or_else(|| token_not_registered(&self.address))
    }

    // -- Session account-cache accessors (dictionary operations, no I/O) --
    //
    // Thin adapters over the session's `BotState::accounts()` streams (the
    // Rust-backed store the read-through fills). One token's identity keys
    // each stream; the depth-8 per-block FIFO semantics live core-side.

    /// The cached balance of `owner` at `block`, if this session's stream
    /// holds it.
    fn get_cached_balance(
        &self,
        py: Python<'_>,
        owner: &str,
        block: u64,
    ) -> PyResult<Option<Py<PyAny>>> {
        let owner = super::parse_address(owner)?;
        let cached = self.with_state(py, |state| {
            state
                .accounts()
                .balance(&self.token_identity().key(), owner, block)
        });
        cached
            .map(|value| u256_to_py(py, &value).map(pyo3::Bound::unbind))
            .transpose()
    }

    /// Record `owner`'s balance at `block` in this session's stream.
    fn set_cached_balance(
        &self,
        py: Python<'_>,
        owner: &str,
        block: u64,
        balance: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let owner = super::parse_address(owner)?;
        let value = u256_from_py(balance)?;
        self.with_state(py, |state| {
            state
                .accounts()
                .store_balance(&self.token_identity().key(), owner, block, value);
        });
        Ok(())
    }

    /// The cached allowance of `owner` to `spender` at `block`, if this
    /// session's stream holds it.
    fn get_cached_approval(
        &self,
        py: Python<'_>,
        owner: &str,
        spender: &str,
        block: u64,
    ) -> PyResult<Option<Py<PyAny>>> {
        let owner = super::parse_address(owner)?;
        let spender = super::parse_address(spender)?;
        let cached = self.with_state(py, |state| {
            state
                .accounts()
                .allowance(&self.token_identity().key(), owner, spender, block)
        });
        cached
            .map(|value| u256_to_py(py, &value).map(pyo3::Bound::unbind))
            .transpose()
    }

    /// Record the allowance of `owner` to `spender` at `block` in this
    /// session's bounded stream (depth 8 per owner/spender pair).
    fn set_cached_approval(
        &self,
        py: Python<'_>,
        owner: &str,
        spender: &str,
        block: u64,
        amount: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let owner = super::parse_address(owner)?;
        let spender = super::parse_address(spender)?;
        let value = u256_from_py(amount)?;
        self.with_state(py, |state| {
            state.accounts().store_allowance(
                &self.token_identity().key(),
                owner,
                spender,
                block,
                value,
            );
        });
        Ok(())
    }

    /// The cached total supply at `block`, if this session's stream holds it.
    fn get_cached_total_supply(&self, py: Python<'_>, block: u64) -> PyResult<Option<Py<PyAny>>> {
        let cached = self.with_state(py, |state| {
            state
                .accounts()
                .total_supply(&self.token_identity().key(), block)
        });
        cached
            .map(|value| u256_to_py(py, &value).map(pyo3::Bound::unbind))
            .transpose()
    }

    /// Record the total supply at `block` in this session's stream.
    fn set_cached_total_supply(
        &self,
        py: Python<'_>,
        block: u64,
        total_supply: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let value = u256_from_py(total_supply)?;
        self.with_state(py, |state| {
            state
                .accounts()
                .store_total_supply(&self.token_identity().key(), block, value);
        });
        Ok(())
    }
}

/// A Python int -> `U256` (the cache store's value wire shape).
fn u256_from_py(value: &Bound<'_, PyAny>) -> PyResult<U256> {
    let text = value.str()?;
    U256::from_str(text.to_str()?).map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!("not a uint256 account value: {e}"))
    })
}

/// A thin Python handle to the session chain's conceptual native token
/// (ADR-097).
///
/// One exists per chain; it has no protocol address of its own and holds no
/// `BotState` entry — the metadata is the core's intrinsic chain facts
/// (`degenbot_core::chain_facts`), read on access. The protocol sentinels
/// (the Curve all-Es coin, the V4 `address(0)` currency) are translated at
/// those protocols' call sites and are never a way to spell this token.
#[pyclass(name = "NativeToken", module = "degenbot._ffi")]
pub struct PyNativeToken {
    chain_id: u64,
}

impl PyNativeToken {
    /// The handle for the chain's conceptual native token.
    pub(crate) const fn new(chain_id: u64) -> Self {
        Self { chain_id }
    }
}

#[pymethods]
impl PyNativeToken {
    /// Chain ID whose native asset this is.
    #[getter]
    fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// The native asset's display name (chain facts; e.g. "Ether").
    #[getter]
    fn name(&self) -> PyResult<String> {
        Ok(native_facts_or_refuse(self.chain_id)?.name.to_owned())
    }

    /// The native asset's ticker symbol (chain facts; e.g. "ETH").
    #[getter]
    fn symbol(&self) -> PyResult<String> {
        Ok(native_facts_or_refuse(self.chain_id)?.symbol.to_owned())
    }

    /// The native asset's decimals (chain facts; e.g. 18).
    #[getter]
    fn decimals(&self) -> PyResult<u8> {
        Ok(native_facts_or_refuse(self.chain_id)?.decimals)
    }
}

/// The chain's native-asset facts, or the loud typed refusal for a chain the
/// manifest does not know.
fn native_facts_or_refuse(chain_id: u64) -> PyResult<degenbot_core::chain_facts::NativeTokenFacts> {
    degenbot_core::chain_facts::native_facts(chain_id)
        .map_err(|refusal| pyo3::exceptions::PyValueError::new_err(refusal.to_string()))
}
