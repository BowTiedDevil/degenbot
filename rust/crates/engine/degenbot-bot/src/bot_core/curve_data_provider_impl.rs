//! Pure-Rust [`CurveDataProvider`] implementation over an RPC double.
//!
//! The concrete RPC implementation of the `CurveDataProvider` trait (defined
//! in the `pyo3`-free `degenbot-pools` crate) — a port of the Python
//! `CurveDataProviderImpl` (`curve/data_provider_impl.py`). It issues the
//! off-chain, per-block on-chain reads a Curve pool's calc needs (virtual
//! price, lending rates, crypto `D`/`gamma`/`price_scale`, admin balances,
//! redemption price) over an [`RpcConstruction`] / [`ConstructionIo`]. It is
//! the layer-2 stored trait object (ADR-005): a standalone `cargo add
//! degenbot` consumer constructs it (no Python) and both the Python companion
//! and any Rust calc read through it.
//!
//! ## Sync-over-async (the trait is sync; `ContractRead::call` is async)
//!
//! `CurveDataProvider` methods are synchronous, so each call bridges through
//! [`degenbot_core::runtime::bridged_block_on`] — the one in-runtime
//! hand-off, valid on the bot's multi-threaded pump loop, a bare construction
//! thread, and a standalone sync consumer alike.

use hashbrown::HashMap;
use std::sync::{Arc, Mutex};

use alloy::primitives::{Address, Bytes, B256, U256};
use alloy::sol_types::SolCall;
use degenbot_core::runtime::bridged_block_on;
use degenbot_pools::{CurveDataProvider, CurveDataProviderError};
use degenbot_rpc::abi::{ContractRead, ICurvePool, ILendingRateReads, IERC20};

use crate::bot_core::construction_io::RpcConstruction;

/// The `lending_rates` cache key is the block number; the value the per-coin
/// lending rates for that block. `Metrics`-free; mirrors the Python
/// `_lending_rate_cache` (a `dict[int, tuple[int, ...]]`).
type LendingCache = Mutex<HashMap<u64, Vec<U256>>>;

#[expect(clippy::missing_fields_in_debug)] // the RPC trait object is not `Debug`
impl std::fmt::Debug for RpcCurveDataProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The RPC trait object is not `Debug`; print the identifiable fields
        // only.
        f.debug_struct("RpcCurveDataProvider")
            .field("pool_address", &self.pool_address)
            .field("base_pool_address", &self.base_pool_address)
            .field("n_coins", &self.n_coins)
            .field("lending_rate_style", &self.lending_rate_style)
            .field("token_addresses", &self.token_addresses)
            .finish()
    }
}

/// Concrete on-chain `CurveDataProvider` reading over an RPC double.
///
/// Holds the pool identity + the per-pool rate/precision config the calc
/// needs, plus the lending-rate cache. `Send + Sync` (all fields are), so it
/// can live behind the `Option<Arc<dyn CurveDataProvider>>` on a
/// [`CurvePoolState`].
pub struct RpcCurveDataProvider {
    /// The RPC double driving all reads.
    rpc: Arc<dyn RpcConstruction + Send + Sync>,
    /// Pool contract address.
    pool_address: Address,
    /// Base pool address for metapools (`get_virtual_price` targets it).
    base_pool_address: Option<Address>,
    /// Number of pool coins (`balances` / `admin_balances` / `price_scale`
    /// length).
    n_coins: usize,
    /// `LendingRateStyle` discriminant (`1` = NONE, `2` = CTOKEN, …).
    lending_rate_style: u8,
    /// Coin addresses, in canonical Curve order.
    token_addresses: Vec<Address>,
    /// Per-coin `use_lending` flags (true for lending-backed coins).
    use_lending: Vec<bool>,
    /// Per-coin precision multipliers (underlying-adjusted for lending).
    precision_multipliers: Vec<U256>,
    /// Per-coin rate multipliers (precision × rate product; used by the
    /// oracle style and as the `NONE` fallback).
    rate_multipliers: Vec<U256>,
    /// Per-block lending-rate cache.
    lending_rate_cache: LendingCache,
}

impl RpcCurveDataProvider {
    /// Construct a provider bound to an RPC double.
    #[must_use]
    #[expect(clippy::too_many_arguments)] // data-dense constructor
    pub fn new(
        rpc: Arc<dyn RpcConstruction + Send + Sync>,
        pool_address: Address,
        base_pool_address: Option<Address>,
        n_coins: usize,
        lending_rate_style: u8,
        token_addresses: Vec<Address>,
        use_lending: Vec<bool>,
        precision_multipliers: Vec<U256>,
        rate_multipliers: Vec<U256>,
    ) -> Self {
        Self {
            rpc,
            pool_address,
            base_pool_address,
            n_coins,
            lending_rate_style,
            token_addresses,
            use_lending,
            precision_multipliers,
            rate_multipliers,
            lending_rate_cache: Mutex::new(HashMap::new()),
        }
    }

    /// One read of a `uint256`-returning getter through the typed read verb.
    fn call_uint<C: SolCall<Return = U256>>(
        &self,
        to: Address,
        call: C,
        block: u64,
    ) -> Result<U256, CurveDataProviderError> {
        bridged_block_on(ContractRead::read(&self.rpc, to, call, Some(block)))
            .map_err(|_| CurveDataProviderError::FetchFailed)
    }

    /// One read of an `address`-returning getter through the typed read verb.
    fn call_address<C: SolCall<Return = Address>>(
        &self,
        to: Address,
        call: C,
        block: u64,
    ) -> Result<Address, CurveDataProviderError> {
        bridged_block_on(ContractRead::read(&self.rpc, to, call, Some(block)))
            .map_err(|_| CurveDataProviderError::FetchFailed)
    }
}

impl CurveDataProvider for RpcCurveDataProvider {
    fn block_number(&self) -> Result<u64, CurveDataProviderError> {
        bridged_block_on(self.rpc.get_block_number())
            .map_err(|_| CurveDataProviderError::FetchFailed)
    }

    fn block_timestamp(&self, block_number: u64) -> Result<u64, CurveDataProviderError> {
        match bridged_block_on(self.rpc.get_block_timestamp(block_number)) {
            Ok(Some(ts)) => Ok(ts),
            _ => Err(CurveDataProviderError::FetchFailed),
        }
    }

    fn token_balance(
        &self,
        token_address: Address,
        holder_address: Address,
        block_number: u64,
    ) -> Result<U256, CurveDataProviderError> {
        self.call_uint(
            token_address,
            IERC20::balanceOfCall {
                account: holder_address,
            },
            block_number,
        )
    }

    fn token_total_supply(
        &self,
        token_address: Address,
        block_number: u64,
    ) -> Result<U256, CurveDataProviderError> {
        self.call_uint(token_address, IERC20::totalSupplyCall {}, block_number)
    }

    fn lending_rates(&self, block_number: u64) -> Result<Vec<U256>, CurveDataProviderError> {
        // Per-block cache (mirrors the Python `_lending_rate_cache`). The read
        // guard is scoped so it drops before the fetch and the insert below —
        // holding it across the whole method deadlocks on the second lock
        // (`std::sync::Mutex` is not re-entrant, and the guard would otherwise
        // also be held across the blocking RPC fetches below).
        {
            #[expect(clippy::unwrap_used)] // internal cache mutex; poison = process bug
            let cache = self.lending_rate_cache.lock().unwrap();
            if let Some(cached) = cache.get(&block_number).cloned() {
                return Ok(cached);
            }
        }

        if self.lending_rate_style == 1 {
            // NONE — no lending tokens; the calc uses rate_multipliers directly.
            return Err(CurveDataProviderError::Unsupported);
        }

        let result: Vec<U256> = match self.lending_rate_style {
            2 => self.fetch_ctoken_rates(block_number)?,
            3 => self.fetch_ytoken_rates(block_number)?,
            4 => self.fetch_cytoken_rates(block_number)?,
            5 => self.fetch_aeth_rates(block_number)?,
            6 => self.fetch_reth_rates(block_number)?,
            7 => self.fetch_oracle_rates(block_number)?,
            _ => return Err(CurveDataProviderError::Unsupported),
        };

        #[expect(clippy::unwrap_used)] // internal cache mutex; poison = process bug
        let mut cache = self.lending_rate_cache.lock().unwrap();
        cache.insert(block_number, result.clone());
        Ok(result)
    }

    fn invariant_d(&self, block_number: u64) -> Result<U256, CurveDataProviderError> {
        self.call_uint(self.pool_address, ICurvePool::DCall {}, block_number)
    }

    fn gamma(&self, block_number: u64) -> Result<U256, CurveDataProviderError> {
        self.call_uint(self.pool_address, ICurvePool::gammaCall {}, block_number)
    }

    fn price_scale(&self, block_number: u64) -> Result<Vec<U256>, CurveDataProviderError> {
        let mut scale = Vec::with_capacity(self.n_coins.saturating_sub(1));
        for k in 0..self.n_coins.saturating_sub(1) {
            #[expect(clippy::expect_used)] // n_coins <= 8 (documented contract invariant)
            let n_coins = u8::try_from(k).expect("n_coins <= 8");
            scale.push(self.call_uint(
                self.pool_address,
                ICurvePool::price_scaleCall {
                    k: U256::from(n_coins),
                },
                block_number,
            )?);
        }
        if scale.len() != self.n_coins.saturating_sub(1) {
            return Err(CurveDataProviderError::LengthMismatch);
        }
        Ok(scale)
    }

    fn admin_balances(&self, block_number: u64) -> Result<Vec<U256>, CurveDataProviderError> {
        let mut balances = Vec::new();
        for i in 0..8 {
            match self.call_uint(
                self.pool_address,
                ICurvePool::admin_balancesCall { i: U256::from(i) },
                block_number,
            ) {
                Ok(b) => balances.push(b),
                // Stop at the first revert — the pool exposes fewer coins.
                Err(_) => break,
            }
        }
        Ok(balances)
    }

    fn redemption_price(&self, block_number: u64) -> Result<U256, CurveDataProviderError> {
        let snap_contract = self.call_address(
            self.pool_address,
            ICurvePool::redemption_price_snapCall {},
            block_number,
        )?;
        let rate = self.call_uint(
            snap_contract,
            ICurvePool::snappedRedemptionPriceCall {},
            block_number,
        )?;
        Ok(rate / U256::from(10u64).pow(U256::from(9u64)))
    }

    fn base_cache_updated(&self, block_number: u64) -> Result<u64, CurveDataProviderError> {
        let v = self.call_uint(
            self.pool_address,
            ICurvePool::base_cache_updatedCall {},
            block_number,
        )?;
        Ok(v.to::<u64>())
    }

    fn base_virtual_price(&self, block_number: u64) -> Result<U256, CurveDataProviderError> {
        self.call_uint(
            self.pool_address,
            ICurvePool::base_virtual_priceCall {},
            block_number,
        )
    }

    fn virtual_price(&self, block_number: u64) -> Result<U256, CurveDataProviderError> {
        let target = self.base_pool_address.unwrap_or(self.pool_address);
        self.call_uint(target, ICurvePool::get_virtual_priceCall {}, block_number)
    }
}

impl RpcCurveDataProvider {
    /// cToken style: `exchangeRateStored` + supply-rate accrual, precision-
    /// multiplied. Mirrors `_fetch_ctoken_rates`.
    fn fetch_ctoken_rates(&self, block_number: u64) -> Result<Vec<U256>, CurveDataProviderError> {
        let mut result = Vec::with_capacity(self.token_addresses.len());
        for ((token, is_lending), multiplier) in self
            .token_addresses
            .iter()
            .zip(self.use_lending.iter())
            .zip(self.precision_multipliers.iter())
        {
            let rate = if *is_lending {
                let exchange = self.call_uint(
                    *token,
                    ILendingRateReads::exchangeRateStoredCall {},
                    block_number,
                )?;
                let supply_rate = self.call_uint(
                    *token,
                    ILendingRateReads::supplyRatePerBlockCall {},
                    block_number,
                )?;
                let old_block = self.call_uint(
                    *token,
                    ILendingRateReads::accrualBlockNumberCall {},
                    block_number,
                )?;
                let precision = U256::from(10u64).pow(U256::from(18u64));
                exchange
                    + exchange * supply_rate * (U256::from(block_number) - old_block) / precision
            } else {
                U256::from(10u64).pow(U256::from(18u64))
            };
            result.push(*multiplier * rate);
        }
        Ok(result)
    }

    /// yToken style: `getPricePerFullShare` for lending coins, `10**18`
    /// otherwise; precision-multiplied. Mirrors `_fetch_ytoken_rates`.
    fn fetch_ytoken_rates(&self, block_number: u64) -> Result<Vec<U256>, CurveDataProviderError> {
        let mut result = Vec::with_capacity(self.token_addresses.len());
        for ((token, multiplier), is_lending) in self
            .token_addresses
            .iter()
            .zip(self.precision_multipliers.iter())
            .zip(self.use_lending.iter())
        {
            let rate = if *is_lending {
                self.call_uint(
                    *token,
                    ILendingRateReads::getPricePerFullShareCall {},
                    block_number,
                )?
            } else {
                U256::from(10u64).pow(U256::from(18u64))
            };
            result.push(*multiplier * rate);
        }
        Ok(result)
    }

    /// cyToken style: all coins lending, cToken accrual. Mirrors
    /// `_fetch_cytoken_rates`.
    fn fetch_cytoken_rates(&self, block_number: u64) -> Result<Vec<U256>, CurveDataProviderError> {
        let mut result = Vec::with_capacity(self.token_addresses.len());
        let precision = U256::from(10u64).pow(U256::from(18u64));
        for (token, multiplier) in self
            .token_addresses
            .iter()
            .zip(self.precision_multipliers.iter())
        {
            let exchange = self.call_uint(
                *token,
                ILendingRateReads::exchangeRateStoredCall {},
                block_number,
            )?;
            let supply_rate = self.call_uint(
                *token,
                ILendingRateReads::supplyRatePerBlockCall {},
                block_number,
            )?;
            let old_block = self.call_uint(
                *token,
                ILendingRateReads::accrualBlockNumberCall {},
                block_number,
            )?;
            let rate = exchange
                + exchange * supply_rate * (U256::from(block_number) - old_block) / precision;
            result.push(*multiplier * rate);
        }
        Ok(result)
    }

    /// aETH style: `ratio()` on the second token, inverted. Mirrors
    /// `_fetch_aeth_rates`.
    fn fetch_aeth_rates(&self, block_number: u64) -> Result<Vec<U256>, CurveDataProviderError> {
        let ratio = self.call_uint(
            self.token_addresses[1],
            ILendingRateReads::ratioCall {},
            block_number,
        )?;
        let precision = U256::from(10u64).pow(U256::from(18u64));
        Ok(vec![precision, precision * precision / ratio])
    }

    /// rETH style: `getExchangeRate()` on the second token. Mirrors
    /// `_fetch_reth_rates`.
    fn fetch_reth_rates(&self, block_number: u64) -> Result<Vec<U256>, CurveDataProviderError> {
        let ratio = self.call_uint(
            self.token_addresses[1],
            ILendingRateReads::getExchangeRateCall {},
            block_number,
        )?;
        let precision = U256::from(10u64).pow(U256::from(18u64));
        Ok(vec![precision, ratio])
    }

    /// ORACLE style: `oracle_method()` bitmask dispatch. Mirrors
    /// `_fetch_oracle_rates`.
    fn fetch_oracle_rates(&self, block_number: u64) -> Result<Vec<U256>, CurveDataProviderError> {
        let oracle_method = self.call_uint(
            self.pool_address,
            ICurvePool::oracle_methodCall {},
            block_number,
        )?;
        let precision = U256::from(10u64).pow(U256::from(18u64));

        if oracle_method.is_zero() {
            return Ok(self.rate_multipliers.clone());
        }

        // `oracle_method & ((2**32 - 1) * 256**28)` → the 4-byte selector;
        // `oracle_method % 2**160` → the oracle contract address.
        let oracle_bit_mask =
            U256::from(u64::from(u32::MAX)) * U256::from(256u64).pow(U256::from(28u64));
        let selector_bytes = oracle_method & oracle_bit_mask;
        let mod_160 = oracle_method % U256::from(2u64).pow(U256::from(160u64));
        let oracle_addr = Address::from_word(B256::from(mod_160.to_be_bytes::<32>()));

        // The oracle's entry point is the selector packed into `oracle_method`,
        // so this calldata is data rather than a declared getter.
        let sel_bytes: [u8; 32] = selector_bytes.to_be_bytes();
        let answer = bridged_block_on(ContractRead::call(
            &self.rpc,
            oracle_addr,
            Bytes::from(sel_bytes.to_vec()),
            Some(block_number),
        ))
        .map_err(|_| CurveDataProviderError::FetchFailed)?;
        if answer.len() < 32 {
            return Err(CurveDataProviderError::FetchFailed);
        }
        let oracle_rate = U256::from_be_slice(&answer[..32]);
        Ok(vec![
            self.rate_multipliers[0],
            self.rate_multipliers[1] * oracle_rate / precision,
        ])
    }
}

#[expect(clippy::unwrap_used)]
#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;
    use async_trait::async_trait;
    use degenbot_core::errors::ProviderError;
    use degenbot_rpc::provider::EthBlock;

    const POOL: Address = address!("0x1111111111111111111111111111111111111111");
    const COIN0: Address = address!("0xaaa0000000000000000000000000000000000001");
    const COIN1: Address = address!("0xaaa0000000000000000000000000000000000002");

    /// A canned RPC double keyed by `(to, full calldata)` so multi-call
    /// methods (`admin_balances` per index, `price_scale` per index, two-step
    /// redemption, per-token lending) each resolve their own response.
    #[derive(Default)]
    struct StubRpc {
        responses: HashMap<(Address, Vec<u8>), Vec<u8>>,
    }

    impl StubRpc {
        fn new() -> Self {
            Self::default()
        }
        fn set(&mut self, to: Address, calldata: Vec<u8>, bytes: Vec<u8>) {
            self.responses.insert((to, calldata), bytes);
        }
    }

    #[async_trait]
    impl RpcConstruction for StubRpc {
        async fn get_block_number(&self) -> Result<u64, ProviderError> {
            Ok(18_000_000)
        }
        async fn get_block(&self, _b: u64) -> Result<Option<EthBlock>, ProviderError> {
            Ok(None)
        }
        async fn get_block_timestamp(&self, _b: u64) -> Result<Option<u64>, ProviderError> {
            Ok(Some(1_700_000_000))
        }
        async fn get_code(&self, _a: Address, _b: Option<u64>) -> Result<Bytes, ProviderError> {
            Ok(Bytes::new())
        }
        async fn get_balance(&self, _a: Address, _b: Option<u64>) -> Result<U256, ProviderError> {
            Ok(U256::ZERO)
        }
        async fn call(
            &self,
            to: Address,
            data: Bytes,
            _b: Option<u64>,
        ) -> Result<Bytes, ProviderError> {
            match self.responses.get(&(to, data.to_vec())) {
                Some(b) => Ok(b.clone().into()),
                None => Err(ProviderError::ExecutionReverted {
                    code: -32000,
                    message: "no stub".into(),
                }),
            }
        }
    }

    fn uint(v: u64) -> Vec<u8> {
        U256::from(v).to_be_bytes::<32>().to_vec()
    }

    fn uint_u256(v: U256) -> Vec<u8> {
        v.to_be_bytes::<32>().to_vec()
    }

    fn addr(a: Address) -> Vec<u8> {
        let mut v = vec![0u8; 12];
        v.extend_from_slice(a.as_slice());
        v
    }

    fn provider(rpc: Arc<dyn RpcConstruction + Send + Sync>) -> RpcCurveDataProvider {
        RpcCurveDataProvider::new(
            rpc,
            POOL,
            None,
            2,
            1, // NONE
            vec![COIN0, COIN1],
            vec![false, false],
            vec![U256::from(1u64), U256::from(1u64)],
            vec![U256::from(1u64), U256::from(1u64)],
        )
    }

    #[test]
    fn block_number_and_timestamp() {
        let rpc: Arc<dyn RpcConstruction + Send + Sync> = Arc::new(StubRpc::new());
        let p = provider(rpc);
        assert_eq!(p.block_number().unwrap(), 18_000_000);
        assert_eq!(p.block_timestamp(18_000_000).unwrap(), 1_700_000_000);
    }

    #[test]
    fn virtual_price_targets_pool_without_base() {
        let mut s = StubRpc::new();
        s.set(
            POOL,
            ICurvePool::get_virtual_priceCall {}.abi_encode(),
            uint(1_020_000_000_000_000_000),
        );
        let p = provider(Arc::new(s));
        assert_eq!(
            p.virtual_price(18_000_000).unwrap(),
            U256::from(1_020_000_000_000_000_000u64)
        );
    }

    #[test]
    fn virtual_price_targets_base_pool_when_present() {
        let base: Address = address!("0x0bb0000000000000000000000000000000000001");
        let mut s = StubRpc::new();
        s.set(
            base,
            ICurvePool::get_virtual_priceCall {}.abi_encode(),
            uint(1_030_000_000_000_000_000),
        );
        let rpc: Arc<dyn RpcConstruction + Send + Sync> = Arc::new(s);
        let p = RpcCurveDataProvider::new(
            rpc,
            POOL,
            Some(base),
            2,
            1,
            vec![COIN0, COIN1],
            vec![false, false],
            vec![U256::from(1u64), U256::from(1u64)],
            vec![U256::from(1u64), U256::from(1u64)],
        );
        assert_eq!(
            p.virtual_price(18_000_000).unwrap(),
            U256::from(1_030_000_000_000_000_000u64)
        );
    }

    #[test]
    fn token_balance_and_total_supply() {
        let mut s = StubRpc::new();
        let holder: Address = address!("0x0aa0000000000000000000000000000000000001");
        s.set(
            COIN0,
            IERC20::balanceOfCall { account: holder }.abi_encode(),
            uint(1_234),
        );
        s.set(
            COIN1,
            IERC20::totalSupplyCall {}.abi_encode(),
            uint(9_876_543),
        );
        let p = provider(Arc::new(s));
        assert_eq!(
            p.token_balance(COIN0, holder, 18_000_000).unwrap(),
            U256::from(1_234u64)
        );
        assert_eq!(
            p.token_total_supply(COIN1, 18_000_000).unwrap(),
            U256::from(9_876_543u64)
        );
    }

    #[test]
    fn lending_rates_none_is_unsupported() {
        let p = provider(Arc::new(StubRpc::new()));
        assert_eq!(
            p.lending_rates(18_000_000).unwrap_err(),
            CurveDataProviderError::Unsupported
        );
    }

    #[test]
    fn lending_rates_ctoken_accrual() {
        // Style CTOKEN (2): exchangeRateStored=1.01e18, supply=1e15,
        // accrualBlockNumber=1000, block_number=2000 → rate =
        // 1.01e18 + 1.01e18*1e15*1000//1e18. Precision multiplier=1.
        let mut s = StubRpc::new();
        s.set(
            COIN0,
            ILendingRateReads::exchangeRateStoredCall {}.abi_encode(),
            uint(1_010_000_000_000_000_000),
        );
        s.set(
            COIN0,
            ILendingRateReads::supplyRatePerBlockCall {}.abi_encode(),
            uint(1_000_000_000_000_000),
        );
        s.set(
            COIN0,
            ILendingRateReads::accrualBlockNumberCall {}.abi_encode(),
            uint(17_999_000),
        );
        s.set(
            COIN1,
            ILendingRateReads::exchangeRateStoredCall {}.abi_encode(),
            uint(1_000_000_000_000_000_000),
        );
        s.set(
            COIN1,
            ILendingRateReads::supplyRatePerBlockCall {}.abi_encode(),
            uint(0),
        );
        s.set(
            COIN1,
            ILendingRateReads::accrualBlockNumberCall {}.abi_encode(),
            uint(17_999_000),
        );
        let rpc: Arc<dyn RpcConstruction + Send + Sync> = Arc::new(s);
        let p = RpcCurveDataProvider::new(
            rpc,
            POOL,
            None,
            2,
            2, // CTOKEN
            vec![COIN0, COIN1],
            vec![true, true],
            vec![U256::from(1u64), U256::from(1u64)],
            vec![U256::from(1u64), U256::from(1u64)],
        );
        let rates = p.lending_rates(18_000_000).unwrap();
        // Two RPC sub-blocks are cached per block; the accrual adds exactly
        // accrual = exchange*supply*(block-old)//1e18
        //         = 1.01e18 * 1e15 * 1000 // 1e18 = 1.01e18
        // → coin0 rate = 1.01e18 + 1.01e18 = 2.02e18.
        assert_eq!(rates[0], U256::from(2_020_000_000_000_000_000u64));
        assert_eq!(rates[1], U256::from(1_000_000_000_000_000_000u64));
    }

    #[test]
    fn lending_rates_reth_second_token() {
        // Style RETH (6): getExchangeRate() on token[1].
        let mut s = StubRpc::new();
        s.set(
            COIN1,
            ILendingRateReads::getExchangeRateCall {}.abi_encode(),
            uint(1_100_000_000_000_000_000),
        );
        let rpc: Arc<dyn RpcConstruction + Send + Sync> = Arc::new(s);
        let p = RpcCurveDataProvider::new(
            rpc,
            POOL,
            None,
            2,
            6, // RETH
            vec![COIN0, COIN1],
            vec![false, true],
            vec![U256::from(1u64), U256::from(1u64)],
            vec![U256::from(1u64), U256::from(1u64)],
        );
        assert_eq!(
            p.lending_rates(18_000_000).unwrap(),
            vec![
                U256::from(10u64).pow(U256::from(18u64)),
                U256::from(1_100_000_000_000_000_000u64),
            ]
        );
    }

    #[test]
    fn price_scale_decodes_n_minus_one_entries() {
        let mut s = StubRpc::new();
        s.set(
            POOL,
            ICurvePool::price_scaleCall { k: U256::ZERO }.abi_encode(),
            uint(123),
        );
        let p = provider(Arc::new(s));
        assert_eq!(p.price_scale(18_000_000).unwrap(), vec![U256::from(123u64)]);
    }

    #[test]
    fn admin_balances_decodes_and_reverts_stop() {
        let mut s = StubRpc::new();
        s.set(
            POOL,
            ICurvePool::admin_balancesCall { i: U256::ZERO }.abi_encode(),
            uint(7),
        );
        let p = provider(Arc::new(s));
        assert_eq!(
            p.admin_balances(18_000_000).unwrap(),
            vec![U256::from(7u64)]
        );
    }

    #[test]
    fn redemption_price_two_step_divides_by_1e9() {
        let snap: Address = address!("0x0990000000000000000000000000000000000001");
        let mut s = StubRpc::new();
        s.set(
            POOL,
            ICurvePool::redemption_price_snapCall {}.abi_encode(),
            addr(snap),
        );
        s.set(
            snap,
            ICurvePool::snappedRedemptionPriceCall {}.abi_encode(),
            uint_u256(U256::from(10u64).pow(U256::from(27u64))),
        );
        let p = provider(Arc::new(s));
        // 1e27 // 1e9 = 1e18.
        assert_eq!(
            p.redemption_price(18_000_000).unwrap(),
            U256::from(10u64).pow(U256::from(18u64))
        );
    }

    #[test]
    fn invariant_d_and_gamma_decode() {
        let mut s = StubRpc::new();
        s.set(POOL, ICurvePool::DCall {}.abi_encode(), uint(42_000));
        s.set(
            POOL,
            ICurvePool::gammaCall {}.abi_encode(),
            uint(7_000_000_000),
        );
        let p = provider(Arc::new(s));
        assert_eq!(p.invariant_d(18_000_000).unwrap(), U256::from(42_000u64));
        assert_eq!(p.gamma(18_000_000).unwrap(), U256::from(7_000_000_000u64));
    }

    #[test]
    fn base_virtual_price_and_cache_updated() {
        let mut s = StubRpc::new();
        s.set(
            POOL,
            ICurvePool::base_virtual_priceCall {}.abi_encode(),
            uint(1_010_000_000_000_000_000),
        );
        s.set(
            POOL,
            ICurvePool::base_cache_updatedCall {}.abi_encode(),
            uint(17_999_000),
        );
        let p = provider(Arc::new(s));
        assert_eq!(
            p.base_virtual_price(18_000_000).unwrap(),
            U256::from(1_010_000_000_000_000_000u64)
        );
        assert_eq!(p.base_cache_updated(18_000_000).unwrap(), 17_999_000);
    }
}
