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
/// # Panics
///
/// Panics if `params.inject_code` is `true` but `params.injected_address`
/// is `None` (mirrors the original).
#[expect(clippy::expect_used)] // inject_code => injected_address required (same contract as the original)
pub fn apply_simulation_overrides<ExtDb>(
    cache_db: &mut CacheDB<ExtDb>,
    params: &SimulationOverrideParams,
) {
    cache_db.insert_account_info(
        &params.owner,
        AccountInfo::default().with_balance(eth_amount(OWNER_FUND_ETH)),
    );

    if params.inject_code {
        let injected = params.injected_address.expect(
            "inject_code is true but injected_address is None — supply the injection address",
        );

        cache_db.insert_account_info(
            &injected,
            AccountInfo::default()
                .with_balance(eth_amount(EXECUTOR_FUND_ETH))
                .with_code(Bytecode::new_raw(params.runtime_bytecode.clone())),
        );

        // The three 1-wei warmup touches (WETH9 balanceOf, PoolManager
        // ERC6909 weth-id + native-id) — storage only, the executor's
        // balance survives.
        cache_db.insert_account_storage(
            &params.weth_address,
            &params.warmup.weth_balance,
            &ONE_WEI,
        );
        cache_db.insert_account_storage(
            &params.pool_manager_address,
            &params.warmup.erc6909_weth,
            &ONE_WEI,
        );
        cache_db.insert_account_storage(
            &params.pool_manager_address,
            &params.warmup.erc6909_native,
            &ONE_WEI,
        );

        // Raise the WETH9 balanceOf slot from the 1-wei touch to the
        // operational amount for V3 callbacks.
        cache_db.insert_account_storage(
            &params.weth_address,
            &params.warmup.weth_balance,
            &eth_amount(EXECUTOR_FUND_ETH),
        );
    }
}

/// A whole-ETH `U256` balance (`eth * 10**18`).
fn eth_amount(eth: u64) -> U256 {
    U256::from(eth) * U256::from(10u128.pow(18))
}
