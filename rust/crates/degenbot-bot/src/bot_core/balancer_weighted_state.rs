//! **Partially relocated.** The lib code (state structs + inherent value
//! methods) now lives in `degenbot_pools::balancer_weighted_state`; re-exported here at the
//! historical `bot_core::balancer_weighted_state` path so consumers resolve unchanged. The
//! `#[cfg(test)]` integration-test mod stays here (it exercises the state
//! through the `BotState` registry, which stays in bot). Transient re-export —
//! repointed at `degenbot_pools::balancer_weighted_state` natively by USPN7M/P2CKRL.

pub use ::degenbot_pools::balancer_weighted_state::*;

#[expect(clippy::expect_used, unused_imports)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot_core::{BotState, RegisterBalancerWeightedPoolParams};
    use ::degenbot_pools::state_history::{BlockDelta, ReorgJournal};
    use alloy::primitives::{aliases::U112, Address, U256};

    /// Two-token weighted pool fixture (mirrors a Curve two-coin helper).
    fn two_token_params(block: u64, balances: &[u64]) -> RegisterBalancerWeightedPoolParams {
        RegisterBalancerWeightedPoolParams {
            address: Address::repeat_byte(0xb1),
            vault: Address::repeat_byte(0xba),
            pool_id: [0u8; 32],
            tokens: vec![Address::repeat_byte(0x01), Address::repeat_byte(0x02)],
            weights: vec![
                U256::from(500_000_000_000_000_000u128),
                U256::from(500_000_000_000_000_000u128),
            ],
            scaling_factors: vec![U256::from(1u64), U256::from(1u64)],
            swap_fee: 1_000_000_000_000_000u128, // 0.1% of 1e18
            pow_version: 1,
            balances: balances.iter().map(|&b| U256::from(b)).collect(),
            update_block: block,
        }
    }

    #[test]
    fn register_and_read_back_balances() {
        let mut core = BotState::new();
        let pool_id = core.register_balancer_weighted_pool(&two_token_params(10, &[1_000, 2_000]));
        let s = core
            .get_balancer_weighted_pool(pool_id)
            .expect("balancer weighted pool registered");
        let id = core
            .get_balancer_weighted_identity(pool_id)
            .expect("balancer weighted identity registered");
        assert_eq!(id.n_tokens(), 2);
        assert_eq!(s.balances, vec![U256::from(1_000), U256::from(2_000)]);
        assert_eq!(s.update_block, 10);
        // Genesis anchor pushed.
        assert_eq!(core.pool_journal_len(pool_id), Some(1));
    }

    #[test]
    fn apply_balance_update_journals_and_lands_new_balances() {
        let mut core = BotState::new();
        let pool_id = core.register_balancer_weighted_pool(&two_token_params(10, &[1_000, 2_000]));
        let affected = core.apply_balance_update_by_pool_id(
            pool_id,
            vec![U256::from(1_500), U256::from(2_500)],
            12,
        );
        assert_eq!(affected, Some(pool_id));
        let s = core
            .get_balancer_weighted_pool(pool_id)
            .expect("balancer weighted pool registered");
        assert_eq!(s.balances, vec![U256::from(1_500), U256::from(2_500)]);
        assert_eq!(s.update_block, 12);
        // Genesis + the new transition delta.
        assert_eq!(core.pool_journal_len(pool_id), Some(2));
    }

    #[test]
    fn apply_balance_update_is_silent_noop_on_v2_pool() {
        let mut core = BotState::new();
        // Register a V2 pool at pool_id 1, then try the Balancer apply path.
        let v2 = core
            .register_v2_pool(&crate::bot_core::RegisterV2PoolParams {
                address: Address::repeat_byte(0x22),
                token0: Address::repeat_byte(0x01),
                token1: Address::repeat_byte(0x02),
                reserve0: U112::from(1_000),
                reserve1: U112::from(2_000),
                fee_token0: (997, 1000),
                fee_token1: (997, 1000),
                factory: Address::repeat_byte(0xff),
                update_block: 0,
                variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
                stable_swap: false,
                fee_denominator: None,
                ..Default::default()
            })
            .expect("test setup: V2 registration");
        let affected = core.apply_balance_update_by_pool_id(v2, vec![U256::from(1_500)], 5);
        assert!(
            affected.is_none(),
            "Balancer weighted apply on a V2 pool must be a silent no-op"
        );
    }

    #[test]
    fn restore_before_block_lands_at_prior_balances() {
        let mut core = BotState::new();
        let pool_id = core.register_balancer_weighted_pool(&two_token_params(10, &[1_000, 2_000]));
        let _ = core.apply_balance_update_by_pool_id(
            pool_id,
            vec![U256::from(1_500), U256::from(2_500)],
            12,
        );
        // Restore to before block 12 via the balance-vector trait dispatcher
        // (ADR-016 ReorgPoolState); the landed-at values are read back through
        // the state projection.
        core.restore_pool_before_block(pool_id, 12)
            .expect("Some on a registered Balancer weighted pool")
            .expect("Ok (target > genesis block)");
        let s = core
            .get_balancer_weighted_pool(pool_id)
            .expect("balancer weighted pool registered");
        assert_eq!(s.balances, vec![U256::from(1_000), U256::from(2_000)]);
        assert_eq!(s.update_block, 10);
    }

    #[test]
    fn restore_to_before_registration_is_an_error() {
        let mut core = BotState::new();
        let pool_id = core.register_balancer_weighted_pool(&two_token_params(10, &[1_000, 2_000]));
        // Target at the registration block → rolling back past registration.
        let res = core
            .restore_pool_before_block(pool_id, 10)
            .expect("Some on registered pool");
        assert!(
            res.is_err(),
            "restoring to the registration block must error"
        );
    }
}
