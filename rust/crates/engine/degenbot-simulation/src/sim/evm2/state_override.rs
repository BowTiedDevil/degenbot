//! The evm2 port of `apply_simulation_overrides`
//! (`sim::evm::state_override`) — the sim-scoped owner/executor funding +
//! bytecode injection + warmup-slot merge onto a `CacheDB`, field-for-field
//! with the same explicit-balance-wins merge (the WETH9 `balanceOf` slot IS
//! overwritten to the operational amount).
//!
//! One behavioral difference is forced by evm2's `CacheDB`: its
//! `insert_account_storage` writes the cache entry WITHOUT the backing
//! `load_account` fetch revm's version performs, so the cold-load side
//! effect (the WETH9/PoolManager code prefetch) does not happen here — those
//! accounts' code loads on first real read instead, and the application is
//! infallible (no backing fetch to fail).

use alloy::primitives::U256;

use ::evm2::bytecode::Bytecode;
use ::evm2::evm::{AccountInfo, CacheDB};

use crate::sim::state_override::{
    SimulationOverrideParams, EXECUTOR_FUND_ETH, ONE_WEI, OWNER_FUND_ETH, USDC_MAINNET,
    USDC_MAINNET_BALANCE_SLOT, WBTC_MAINNET, WBTC_MAINNET_BALANCE_SLOT,
};

/// Apply the simulation state-overrides onto an evm2 `CacheDB`,
/// field-for-field with the revm-side original (see the module doc).
///
/// The settlement warmup touches apply only for a deployment that declares the
/// identities ([`SimulationOverrideParams::settlement`]); a declared code
/// override installs its runtime at the declaration's own address.
pub fn apply_simulation_overrides<ExtDb>(
    cache_db: &mut CacheDB<ExtDb>,
    params: &SimulationOverrideParams,
) {
    cache_db.insert_account_info(
        &params.owner,
        AccountInfo::default().with_balance(eth_amount(OWNER_FUND_ETH)),
    );

    if let Some(code) = &params.code {
        cache_db.insert_account_info(
            &code.address,
            AccountInfo::default()
                .with_balance(eth_amount(EXECUTOR_FUND_ETH))
                .with_code(Bytecode::new_raw(code.runtime.clone())),
        );
    }

    if let Some(holder) = params.self_fund_usdc {
        let slot = degenbot_executor::mapping_slot(
            U256::from(USDC_MAINNET_BALANCE_SLOT),
            U256::from_be_bytes(holder.into_word().into()),
        );
        cache_db.insert_account_storage(&USDC_MAINNET, &slot, &eth_amount(EXECUTOR_FUND_ETH));
    }

    if let Some(settlement) = &params.settlement {
        // The three 1-wei warmup touches (WETH9 balanceOf, PoolManager
        // ERC6909 weth-id + native-id) — storage only, the executor's
        // balance survives.
        cache_db.insert_account_storage(
            &settlement.weth_address,
            &settlement.slots.weth_balance,
            &ONE_WEI,
        );
        if let Some(pool_manager) = settlement.pool_manager_address {
            cache_db.insert_account_storage(
                &pool_manager,
                &settlement.slots.erc6909_weth,
                &ONE_WEI,
            );
            cache_db.insert_account_storage(
                &pool_manager,
                &settlement.slots.erc6909_native,
                &ONE_WEI,
            );
        }

        // Raise the WETH9 balanceOf slot from the 1-wei touch to the
        // operational amount for V3 callbacks.
        cache_db.insert_account_storage(
            &settlement.weth_address,
            &settlement.slots.weth_balance,
            &eth_amount(EXECUTOR_FUND_ETH),
        );
    }

    if let Some(executor) = params.zero_balance {
        if let Some(code) = &params.code {
            cache_db.insert_account_info(
                &code.address,
                AccountInfo::default()
                    .with_balance(U256::ZERO)
                    .with_code(Bytecode::new_raw(code.runtime.clone())),
            );
        }
        for (token, base) in [
            (USDC_MAINNET, USDC_MAINNET_BALANCE_SLOT),
            (WBTC_MAINNET, WBTC_MAINNET_BALANCE_SLOT),
        ] {
            let slot = degenbot_executor::mapping_slot(
                U256::from(base),
                U256::from_be_bytes(executor.into_word().into()),
            );
            cache_db.insert_account_storage(&token, &slot, &U256::ZERO);
        }
        if let Some(settlement) = &params.settlement {
            cache_db.insert_account_storage(
                &settlement.weth_address,
                &settlement.slots.weth_balance,
                &U256::ZERO,
            );
            if let Some(pool_manager) = settlement.pool_manager_address {
                cache_db.insert_account_storage(
                    &pool_manager,
                    &settlement.slots.erc6909_weth,
                    &U256::ZERO,
                );
                cache_db.insert_account_storage(
                    &pool_manager,
                    &settlement.slots.erc6909_native,
                    &U256::ZERO,
                );
            }
        }
    }
}

/// A whole-ETH `U256` balance (`eth * 10**18`).
fn eth_amount(eth: u64) -> U256 {
    U256::from(eth) * U256::from(10u128.pow(18))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::Address;

    #[test]
    fn zero_balance_strips_token_capital_but_preserves_owner_gas() {
        let executor = Address::from([0x42; 20]);
        let owner = Address::from([0x24; 20]);
        let weth = Address::from([0x01; 20]);
        let pm = Address::from([0x02; 20]);
        let weth_slot = degenbot_executor::mapping_slot(
            U256::from(3),
            U256::from_be_bytes(executor.into_word().into()),
        );
        let mut db = ::evm2::evm::InMemoryDB::default();
        apply_simulation_overrides(
            &mut db,
            &SimulationOverrideParams {
                owner,
                code: Some(crate::DeclaredCode {
                    address: executor,
                    runtime: alloy::primitives::Bytes::from_static(&[0x00]),
                }),
                settlement: Some(crate::SettlementWarmup {
                    weth_address: weth,
                    pool_manager_address: Some(pm),
                    slots: degenbot_executor::WarmupSlots {
                        weth_balance: weth_slot,
                        erc6909_weth: U256::from(12),
                        erc6909_native: U256::from(13),
                    },
                }),
                zero_balance: Some(executor),
                ..Default::default()
            },
        );
        assert_eq!(
            db.account_info(&executor).map(|a| a.balance),
            Some(U256::ZERO)
        );
        assert_eq!(
            db.account_info(&owner).map(|a| a.balance),
            Some(eth_amount(OWNER_FUND_ETH))
        );
        for (token, base) in [
            (weth, 3),
            (USDC_MAINNET, USDC_MAINNET_BALANCE_SLOT),
            (WBTC_MAINNET, WBTC_MAINNET_BALANCE_SLOT),
        ] {
            let slot = degenbot_executor::mapping_slot(
                U256::from(base),
                U256::from_be_bytes(executor.into_word().into()),
            );
            assert_eq!(
                db.cache
                    .storage
                    .get(&token)
                    .and_then(|s| s.slots.get(&slot)),
                Some(&U256::ZERO),
            );
        }
        for slot in [U256::from(12), U256::from(13)] {
            assert_eq!(
                db.cache.storage.get(&pm).and_then(|s| s.slots.get(&slot)),
                Some(&U256::ZERO),
            );
        }
    }

    #[test]
    fn usdc_capital_is_scoped_to_the_executor_and_opt_in() {
        let executor = Address::from([0x42; 20]);
        let owner = Address::from([0x24; 20]);
        let slot = degenbot_executor::mapping_slot(
            U256::from(USDC_MAINNET_BALANCE_SLOT),
            U256::from_be_bytes(executor.into_word().into()),
        );
        let mut db = ::evm2::evm::InMemoryDB::default();
        let params = SimulationOverrideParams {
            owner,
            self_fund_usdc: Some(executor),
            ..Default::default()
        };
        apply_simulation_overrides(&mut db, &params);
        assert_eq!(
            db.cache
                .storage
                .get(&USDC_MAINNET)
                .and_then(|s| s.slots.get(&slot)),
            Some(&eth_amount(EXECUTOR_FUND_ETH)),
            "the deployed executor, not the owner fallback, receives probe capital",
        );
        let mut ordinary = ::evm2::evm::InMemoryDB::default();
        apply_simulation_overrides(&mut ordinary, &SimulationOverrideParams::default());
        assert_eq!(
            ordinary
                .cache
                .storage
                .get(&USDC_MAINNET)
                .and_then(|s| s.slots.get(&slot)),
            None,
            "ordinary solver simulation must not receive USDC",
        );
    }
}
