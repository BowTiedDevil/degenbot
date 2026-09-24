//! Data-provider seam for Curve `StableSwap` / Crypto pools.
//!
//! Curve pools need **off-chain, per-block on-chain state** that is NOT
//! event-log-decodable (or is impractically expensive to decode): block
//! timestamp, per-token balances/total-supply of underlying tokens, lending
//! rates, the crypto-pool `D`/`gamma`/`price_scale`, `admin_balances`,
//! `redemption_price`, and the virtual-price cache (base + current). The
//! Python companion fetches these via a `CurveDataProvider` that does RPC
//! `eth_call`/`eth_block` lookups.
//!
//! This is the **intermediate (layer-2) design** for Curve: a stored trait
//! object (pyo3-free trait in this crate; `PyCurveDataProvider` adapter in the
//! binding). Under full ADR-006 D4, the event-decodable values
//! (`virtual_price`, `A`, `price_scale`, `admin_balances`) would be
//! pump-decoded into `BotState` slots — that is a *follow-up epic* (layer 3),
//! NOT this task. The stored trait object keeps the seam crate-clean (no pyo3
//! in core, no debt across the crate boundary).
//!
//! Mirrors [`crate::rate_provider::BalancerRateProvider`] and
//! [`crate::tick_fetch::TickWordFetcher`]: `Send + Sync` so it can live
//! behind `Option<Arc<dyn …>>` on a `PoolState`, with the Py adapter
//! re-entering via `Python::attach`. Caching stays in the adapter / the
//! Python `PerBlockCache` (the trait is the *read* interface).
//!
//! ## Why this trait lives in `degenbot-pools` (not the bot)
//!
//! `CurvePoolState` holds an `Option<Arc<dyn CurveDataProvider>>` field — so
//! the trait type must be visible to this crate. Following the `std::io::Read`
//! precedent (defining an interface pulls no I/O), the trait *definition* +
//! its value-only error type live here, while the RPC *implementations* (and
//! the `PyCurveDataProvider` `PyO3` adapter) live in `degenbot-bot` /
//! `degenbot-python`.

use alloy::primitives::{Address, U256};

/// Why a [`CurveDataProvider`] read failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurveDataProviderError {
    /// The underlying fetch (RPC `eth_call` / `eth_block`) failed.
    FetchFailed,
    /// The returned value's length was wrong (e.g. `lending_rates` /
    /// `price_scale` / `admin_balances` must match the pool's coin count).
    LengthMismatch,
    /// The requested block/value is outside the provider's supported range
    /// (e.g. a future block, or a method unsupported by this pool family —
    /// `d`/`gamma`/`price_scale` are crypto-only).
    Unsupported,
}

/// Off-chain on-chain-state reader for a Curve pool.
///
/// Implementations may do I/O (RPC). The core calc path holds no `BotState`
/// lock across the call — a fetcher that re-enters `BotState` returns the
/// value here rather than mutating state in place, matching the
/// [`TickWordFetcher`] discipline.
///
/// `Send + Sync` so it can live behind an `Option<Arc<dyn …>>` on a
/// `PoolState`.
// Every method returns `Result<_, CurveDataProviderError>`; the three error
// variants are documented on the enum above and apply uniformly. Per-method
// `# Errors` sections would just restate that, so they are omitted.
#[expect(clippy::missing_errors_doc)]
pub trait CurveDataProvider: Send + Sync + std::fmt::Debug {
    /// Latest block number known to the provider.
    fn block_number(&self) -> Result<u64, CurveDataProviderError>;

    /// The block timestamp at `block_number`.
    fn block_timestamp(&self, block_number: u64) -> Result<u64, CurveDataProviderError>;

    /// `ERC20.balanceOf(holder)` for `token_address` at `block_number`.
    fn token_balance(
        &self,
        token_address: Address,
        holder_address: Address,
        block_number: u64,
    ) -> Result<U256, CurveDataProviderError>;

    /// `ERC20.totalSupply` for `token_address` at `block_number`.
    fn token_total_supply(
        &self,
        token_address: Address,
        block_number: u64,
    ) -> Result<U256, CurveDataProviderError>;

    /// Per-coin lending (`supplyRatePerBlock`) rates at `block_number`. One
    /// entry per pool coin; `0` for non-lending coins.
    fn lending_rates(&self, block_number: u64) -> Result<Vec<U256>, CurveDataProviderError>;

    /// Crypto-pool `D` (the invariant) at `block_number`. Crypto pools only.
    fn d(&self, block_number: u64) -> Result<U256, CurveDataProviderError>;

    /// Crypto-pool `gamma` at `block_number`. Crypto pools only.
    fn gamma(&self, block_number: u64) -> Result<U256, CurveDataProviderError>;

    /// Crypto-pool `price_scale` per coin at `block_number`. Crypto pools only.
    fn price_scale(&self, block_number: u64) -> Result<Vec<U256>, CurveDataProviderError>;

    /// Crypto-pool `admin_balances` per coin at `block_number`. The difference
    /// between stored balances and real balances claimed by the admin fee.
    fn admin_balances(&self, block_number: u64) -> Result<Vec<U256>, CurveDataProviderError>;

    /// Crypto-pool `redemption_price` at `block_number`.
    fn redemption_price(&self, block_number: u64) -> Result<U256, CurveDataProviderError>;

    /// The block at which the base pool's virtual-price cache was last
    /// updated (≤ `block_number`). Metapool/registry plumbing.
    fn base_cache_updated(&self, block_number: u64) -> Result<u64, CurveDataProviderError>;

    /// The base pool's cached `virtual_price` at `base_cache_updated`.
    fn base_virtual_price(&self, block_number: u64) -> Result<U256, CurveDataProviderError>;

    /// This pool's cached `virtual_price` at `block_number`.
    fn virtual_price(&self, block_number: u64) -> Result<U256, CurveDataProviderError>;
}

#[expect(clippy::unwrap_used)]
#[cfg(test)]
mod tests {
    use super::*;

    /// A canned-value stub provider for the acceptance-test: every method
    /// returns a fixed value independent of its args, so the trait-object
    /// dispatch can be exercised without I/O.
    #[derive(Clone, Debug)]
    struct StubProvider {
        block_number: u64,
        block_timestamp: u64,
        balance: U256,
        total_supply: U256,
        lending_rates: Vec<U256>,
        d: U256,
        gamma: U256,
        price_scale: Vec<U256>,
        admin_balances: Vec<U256>,
        redemption_price: U256,
        base_cache_updated: u64,
        base_virtual_price: U256,
        virtual_price: U256,
    }

    impl CurveDataProvider for StubProvider {
        fn block_number(&self) -> Result<u64, CurveDataProviderError> {
            Ok(self.block_number)
        }
        fn block_timestamp(&self, _block_number: u64) -> Result<u64, CurveDataProviderError> {
            Ok(self.block_timestamp)
        }
        fn token_balance(
            &self,
            _token_address: Address,
            _holder_address: Address,
            _block_number: u64,
        ) -> Result<U256, CurveDataProviderError> {
            Ok(self.balance)
        }
        fn token_total_supply(
            &self,
            _token_address: Address,
            _block_number: u64,
        ) -> Result<U256, CurveDataProviderError> {
            Ok(self.total_supply)
        }
        fn lending_rates(&self, _block_number: u64) -> Result<Vec<U256>, CurveDataProviderError> {
            Ok(self.lending_rates.clone())
        }
        fn d(&self, _block_number: u64) -> Result<U256, CurveDataProviderError> {
            Ok(self.d)
        }
        fn gamma(&self, _block_number: u64) -> Result<U256, CurveDataProviderError> {
            Ok(self.gamma)
        }
        fn price_scale(&self, _block_number: u64) -> Result<Vec<U256>, CurveDataProviderError> {
            Ok(self.price_scale.clone())
        }
        fn admin_balances(&self, _block_number: u64) -> Result<Vec<U256>, CurveDataProviderError> {
            Ok(self.admin_balances.clone())
        }
        fn redemption_price(&self, _block_number: u64) -> Result<U256, CurveDataProviderError> {
            Ok(self.redemption_price)
        }
        fn base_cache_updated(&self, _block_number: u64) -> Result<u64, CurveDataProviderError> {
            Ok(self.base_cache_updated)
        }
        fn base_virtual_price(&self, _block_number: u64) -> Result<U256, CurveDataProviderError> {
            Ok(self.base_virtual_price)
        }
        fn virtual_price(&self, _block_number: u64) -> Result<U256, CurveDataProviderError> {
            Ok(self.virtual_price)
        }
    }

    #[test]
    fn stub_provider_returns_canned_values_through_trait_object() {
        let stub = StubProvider {
            block_number: 18_000_000,
            block_timestamp: 1_700_000_000,
            balance: U256::from(1_234_u64),
            total_supply: U256::from(9_876_u64),
            lending_rates: vec![U256::from(1_u64), U256::from(2_u64)],
            d: U256::from(42_000_u64),
            gamma: U256::from(7_000_000_000_u64),
            price_scale: vec![U256::from(10u64).pow(U256::from(18u64)); 2],
            admin_balances: vec![U256::from(0_u64), U256::from(0_u64)],
            redemption_price: U256::from(10u64).pow(U256::from(18u64)),
            base_cache_updated: 17_999_000,
            base_virtual_price: U256::from(1_010_000_000_000_000_000_u128),
            virtual_price: U256::from(1_020_000_000_000_000_000_u128),
        };
        // Read every method through the stored trait object — the
        // acceptance criterion ("the calculator reads them through the stored
        // trait object").
        let provider: std::sync::Arc<dyn CurveDataProvider> = std::sync::Arc::new(stub);
        assert_eq!(provider.block_number().unwrap(), 18_000_000);
        assert_eq!(provider.block_timestamp(17_999_999).unwrap(), 1_700_000_000);
        assert_eq!(
            provider
                .token_balance(Address::ZERO, Address::ZERO, 17_999_999)
                .unwrap(),
            U256::from(1_234_u64)
        );
        assert_eq!(
            provider
                .token_total_supply(Address::ZERO, 17_999_999)
                .unwrap(),
            U256::from(9_876_u64)
        );
        assert_eq!(
            provider.lending_rates(17_999_999).unwrap(),
            vec![U256::from(1_u64), U256::from(2_u64)]
        );
        assert_eq!(provider.d(17_999_999).unwrap(), U256::from(42_000_u64));
        assert_eq!(
            provider.gamma(17_999_999).unwrap(),
            U256::from(7_000_000_000_u64)
        );
        assert_eq!(
            provider.price_scale(17_999_999).unwrap(),
            vec![U256::from(10u64).pow(U256::from(18u64)); 2]
        );
        assert_eq!(
            provider.admin_balances(17_999_999).unwrap(),
            vec![U256::from(0_u64), U256::from(0_u64)]
        );
        assert_eq!(
            provider.redemption_price(17_999_999).unwrap(),
            U256::from(10u64).pow(U256::from(18u64))
        );
        assert_eq!(provider.base_cache_updated(17_999_999).unwrap(), 17_999_000);
        assert_eq!(
            provider.base_virtual_price(17_999_999).unwrap(),
            U256::from(1_010_000_000_000_000_000_u128)
        );
        assert_eq!(
            provider.virtual_price(17_999_999).unwrap(),
            U256::from(1_020_000_000_000_000_000_u128)
        );
    }

    #[test]
    fn trait_object_is_send_sync() {
        // Anchors the `Send + Sync` bound required for
        // `Option<Arc<dyn CurveDataProvider>>` on a `PoolState` shared across
        // threads.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<std::sync::Arc<dyn CurveDataProvider>>();
    }
}
