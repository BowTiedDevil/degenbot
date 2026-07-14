//! **Partially relocated.** The lib code (state structs + inherent value
//! methods) now lives in `degenbot_pools::balancer_stable_state`; re-exported here at the
//! historical `bot_core::balancer_stable_state` path so consumers resolve unchanged. The
//! `#[cfg(test)]` integration-test mod stays here (it exercises the state
//! through the `BotState` registry, which stays in bot). Transient re-export —
//! repointed at `degenbot_pools::balancer_stable_state` natively by USPN7M/P2CKRL.

pub use ::degenbot_pools::balancer_stable_state::*;

#[cfg(test)]
mod tests {
    #![allow(unused_imports)]
    use super::*;
    use crate::bot_core::rate_provider::BalancerRateProvider;
    use crate::bot_core::state_history::{BlockDelta, ReorgJournal};
    use crate::bot_core::{BotState, RegisterBalancerStablePoolParams};
    use alloy::primitives::{Address, U256};
    use std::sync::Arc;

    /// 3-token `ComposableStablePool` fixture (BPT at index 2).
    fn composable_params(
        block: u64,
        balances: &[u64],
        bpt_idx: Option<usize>,
        invariant_version: u8,
    ) -> RegisterBalancerStablePoolParams {
        RegisterBalancerStablePoolParams {
            address: Address::repeat_byte(0xb5),
            vault: Address::repeat_byte(0xba),
            pool_id: [0u8; 32],
            tokens: vec![
                Address::repeat_byte(0x01),
                Address::repeat_byte(0x02),
                Address::repeat_byte(0x03),
            ],
            amp: 200,
            scaling_factors: vec![U256::from(10u64).pow(U256::from(18u64)); 3],
            swap_fee: 1_000_000_000_000_000u128, // 0.1% of 1e18
            bpt_idx,
            invariant_version,
            balances: balances.iter().map(|&b| U256::from(b)).collect(),
            update_block: block,
            rate_provider: None,
        }
    }

    #[test]
    fn register_and_read_back_balances() {
        let mut core = BotState::new();
        let pool_id = core.register_balancer_stable_pool(&composable_params(
            10,
            &[1_000, 2_000, 3_000],
            Some(2),
            1,
        ));
        let s = core
            .get_balancer_stable_pool(pool_id)
            .expect("balancer stable pool registered");
        let id = core
            .get_balancer_stable_identity(pool_id)
            .expect("balancer stable identity registered");
        assert_eq!(id.n_tokens(), 3);
        assert_eq!(
            s.balances,
            vec![U256::from(1_000), U256::from(2_000), U256::from(3_000)]
        );
        assert_eq!(s.update_block, 10);
        // BPT-index round-trip — Some for Composable.
        assert_eq!(id.bpt_idx, Some(2));
        // invariant_version round-trip — V1.
        assert_eq!(id.invariant_version, 1);
        // Genesis anchor pushed.
        assert_eq!(core.balancer_stable_journal_len(pool_id), 1);
    }

    #[test]
    fn bpt_index_none_round_trip_for_meta_stable() {
        // MetaStablePool — bpt_idx None, invariant_version V2 (the
        // MetaStable path).
        let mut core = BotState::new();
        let pool_id = core.register_balancer_stable_pool(&composable_params(
            10,
            &[1_000, 2_000, 3_000],
            None,
            2,
        ));
        let id = core
            .get_balancer_stable_identity(pool_id)
            .expect("balancer stable identity registered");
        assert_eq!(id.bpt_idx, None);
        assert_eq!(id.invariant_version, 2);
    }

    #[test]
    fn apply_balance_update_journals_and_lands_new_balances() {
        let mut core = BotState::new();
        let pool_id = core.register_balancer_stable_pool(&composable_params(
            10,
            &[1_000, 2_000, 3_000],
            Some(2),
            1,
        ));
        let affected = core.apply_balancer_stable_balance_update_by_pool_id(
            pool_id,
            vec![U256::from(1_500), U256::from(2_500), U256::from(3_500)],
            12,
        );
        assert_eq!(affected, Some(pool_id));
        let s = core
            .get_balancer_stable_pool(pool_id)
            .expect("balancer stable pool registered");
        assert_eq!(
            s.balances,
            vec![U256::from(1_500), U256::from(2_500), U256::from(3_500)]
        );
        assert_eq!(s.update_block, 12);
        // Genesis + the new transition delta.
        assert_eq!(core.balancer_stable_journal_len(pool_id), 2);
    }

    #[test]
    fn apply_balance_update_is_silent_noop_on_v2_pool() {
        let mut core = BotState::new();
        // Register a V2 pool at pool_id 1, then try the stable apply path.
        let v2 = core
            .register_v2_pool(&crate::bot_core::RegisterV2PoolParams {
                address: Address::repeat_byte(0x22),
                token0: Address::repeat_byte(0x01),
                token1: Address::repeat_byte(0x02),
                reserve0: U256::from(1_000),
                reserve1: U256::from(2_000),
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
        let affected =
            core.apply_balancer_stable_balance_update_by_pool_id(v2, vec![U256::from(1_500)], 5);
        assert!(
            affected.is_none(),
            "Balancer stable apply on a V2 pool must be a silent no-op"
        );
    }

    #[test]
    fn restore_before_block_lands_at_prior_balances() {
        let mut core = BotState::new();
        let pool_id = core.register_balancer_stable_pool(&composable_params(
            10,
            &[1_000, 2_000, 3_000],
            Some(2),
            1,
        ));
        let _ = core.apply_balancer_stable_balance_update_by_pool_id(
            pool_id,
            vec![U256::from(1_500), U256::from(2_500), U256::from(3_500)],
            12,
        );
        // Restore to before block 12 → landed-at = the genesis registration
        // balances (the largest delta strictly below 12).
        let restored = core
            .balancer_stable_restore_before_block(pool_id, 12)
            .expect("Some(Ok) on a registered Balancer stable pool")
            .expect("Ok (target > genesis block)");
        assert_eq!(
            restored.0,
            vec![U256::from(1_000), U256::from(2_000), U256::from(3_000)]
        );
        assert_eq!(restored.1, 10);
        // Current mutable state was written back.
        let s = core
            .get_balancer_stable_pool(pool_id)
            .expect("balancer stable pool registered");
        assert_eq!(
            s.balances,
            vec![U256::from(1_000), U256::from(2_000), U256::from(3_000)]
        );
        assert_eq!(s.update_block, 10);
    }

    #[test]
    fn restore_to_before_registration_is_an_error() {
        let mut core = BotState::new();
        let pool_id = core.register_balancer_stable_pool(&composable_params(
            10,
            &[1_000, 2_000, 3_000],
            Some(2),
            1,
        ));
        // Target at the registration block → rolling back past registration.
        let res = core
            .balancer_stable_restore_before_block(pool_id, 10)
            .expect("Some on registered pool");
        assert!(
            res.is_err(),
            "restoring to the registration block must error"
        );
    }
}
