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
    SimulationOverrideParams, EXECUTOR_FUND_ETH, ONE_WEI, OWNER_FUND_ETH,
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
}

/// A whole-ETH `U256` balance (`eth * 10**18`).
fn eth_amount(eth: u64) -> U256 {
    U256::from(eth) * U256::from(10u128.pow(18))
}
