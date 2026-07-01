//! Rate-provider seam for Balancer V2 yield-bearing pools
//! (`ComposableStablePools` / `MetaStablePools` with per-token rate providers).
//!
//! Rate providers are **off-chain yield-bearing token price feeds** — NOT
//! event-log-decodable — so the stored-trait-object design is *terminal* (not
//! debt): you cannot decode a rate from a `Mint`/`Burn`/`Swap` log, so the
//! rate must be fetched on demand at calculation time. This is the Polars-codec
//! pattern: the pyo3-free core holds a `BalancerRateProvider` trait, the
//! `degenbot-python` binding supplies a `PyBalancerRateProvider` adapter
//! wrapping a Python callable, and pure-Rust consumers (e.g. a static-rate
//! fallback) supply a pure-Rust `impl` — no Python in the hot path.
//!
//! Mirrors [`crate::bot_core::tick_fetch::TickWordFetcher`] (the first stored
//! I/O trait object): `Send + Sync` so it can live behind `Option<Arc<dyn …>>`
//! on a `PoolState`, with the Py adapter re-entering via `Python::attach`.

use alloy::primitives::U256;

/// Why a [`BalancerRateProvider`] fetch failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateProviderError {
    /// The underlying fetch (e.g. RPC `eth_call`) failed.
    FetchFailed,
    /// The returned rate vector had the wrong length (must match the pool's
    /// token count, including BPT for Composable pools).
    LengthMismatch,
}

/// Fetches per-block scaling-factor rates from on-chain rate providers.
///
/// Implementations may do I/O (RPC `eth_call` against each token's rate
/// provider contract). The core calc path holds no `BotState` lock across the
/// call (it merges the returned rates separately), so a fetcher that re-enters
/// `BotState` is re-entrancy-safe as long as it returns here rather than
/// mutating state in place — mirroring [`TickWordFetcher`]'s discipline.
///
/// `Send + Sync` so it can live behind an `Option<Arc<dyn …>>` on a
/// `PoolState`.
///
/// [`TickWordFetcher`]: crate::bot_core::tick_fetch::TickWordFetcher
pub trait BalancerRateProvider: Send + Sync + std::fmt::Debug {
    /// Return the current rate for each pool token (length == token count,
    /// including BPT for Composable pools). Tokens without a rate provider
    /// return ONE (`1e18`).
    ///
    /// `block_identifier` (`Some(block)`) is the block the calculation
    /// targets so the result matches on-chain state at that block; `None`
    /// means "latest".
    ///
    /// # Errors
    ///
    /// Returns [`RateProviderError::FetchFailed`] if the underlying fetch
    /// failed, or [`RateProviderError::LengthMismatch`] if the returned
    /// vector has the wrong length.
    fn get_rates(&self, block_identifier: Option<u64>) -> Result<Vec<U256>, RateProviderError>;

    /// Whether this provider returns static (construction-time) rates — i.e.
    /// it performs no I/O. The Python companion's
    /// `requires_io_at_calculation_time` / `_should_warn_stale_rates` derive
    /// from this flag rather than `isinstance` checks against a Python class.
    #[must_use]
    fn is_static(&self) -> bool {
        false
    }
}

/// A pure-Rust static rate provider: always returns the construction-time
/// rates, performs no I/O (`is_static() == true`).
///
/// Replaces the Python `_StaticRateProvider` for the no-I/O fallback path.
#[derive(Clone, Debug)]
pub struct StaticRateProvider {
    rates: Vec<U256>,
}

impl StaticRateProvider {
    /// Construct a static provider holding `rates` (length == token count).
    #[must_use]
    pub fn new(rates: Vec<U256>) -> Self {
        Self { rates }
    }
}

impl BalancerRateProvider for StaticRateProvider {
    fn get_rates(&self, _block_identifier: Option<u64>) -> Result<Vec<U256>, RateProviderError> {
        Ok(self.rates.clone())
    }

    fn is_static(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_provider_returns_construction_rates() {
        let rates = vec![
            U256::from(10u64).pow(U256::from(18u64)),
            U256::from(2u64) * U256::from(10u64).pow(U256::from(18u64)),
        ];
        let provider = StaticRateProvider::new(rates.clone());
        assert!(provider.is_static());
        // block_identifier is ignored for the static case.
        assert_eq!(
            provider.get_rates(Some(42)).unwrap(),
            rates,
            "static provider returns construction-time rates regardless of block"
        );
        assert_eq!(
            provider.get_rates(None).unwrap(),
            rates,
            "None block is equivalent"
        );
        // Length-matching is the caller's responsibility — the provider returns
        // exactly what it was constructed with.
        assert_eq!(provider.get_rates(None).unwrap().len(), 2);
    }

    #[test]
    fn trait_object_is_send_sync() {
        // The trait is `Send + Sync` so `Option<Arc<dyn …>>` can live on a
        // `PoolState` shared across threads. This compile-time check anchors
        // the bound; if a future edit breaks Send+Sync, this stops compiling.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<std::sync::Arc<dyn BalancerRateProvider>>();
        let _provider: std::sync::Arc<dyn BalancerRateProvider> =
            std::sync::Arc::new(StaticRateProvider::new(vec![U256::from(1u64)]));
    }
}
