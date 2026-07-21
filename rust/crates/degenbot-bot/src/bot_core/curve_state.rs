//! **Partially relocated.** The lib code (state structs + inherent value
//! methods) now lives in `degenbot_pools::curve_state`; re-exported here at the
//! historical `bot_core::curve_state` path so consumers resolve unchanged. The
//! `#[cfg(test)]` integration-test mod stays here (it exercises the state
//! through the `BotState` registry, which stays in bot). Transient re-export —
//! repointed at `degenbot_pools::curve_state` natively by USPN7M/P2CKRL.

pub use ::degenbot_pools::curve_state::*;

#[cfg(test)]
mod tests {
    #![allow(unused_imports)]
    use super::*;
    use crate::bot_core::{BotState, RegisterCurvePoolParams};
    use ::degenbot_pools::curve_data_provider::CurveDataProvider;
    use ::degenbot_pools::state_history::{BlockDelta, ReorgJournal};
    use alloy::primitives::{aliases::U112, Address, U256};
    use std::sync::Arc;

    fn three_coin_params(block: u64, balances: &[u64]) -> RegisterCurvePoolParams {
        RegisterCurvePoolParams {
            address: Address::repeat_byte(0xc1),
            tokens: vec![
                Address::repeat_byte(0x01),
                Address::repeat_byte(0x02),
                Address::repeat_byte(0x03),
            ],
            a_coefficient: 2000,
            fee: 4_000_000,
            admin_fee: 5_000_000_000,
            rate_multipliers: vec![U256::from(10u64).pow(U256::from(18u64)); 3],
            balances: balances.iter().map(|&b| U256::from(b)).collect(),
            update_block: block,
            swap_style: 0,
            lending_rate_style: 0,
            d_variant: 0,
            y_variant: 0,
            yd_variant: 0,
            base_pool: None,
            initial_a_coefficient: None,
            future_a_coefficient: None,
            initial_a_coefficient_time: None,
            future_a_coefficient_time: None,
            create_timestamp: None,
            fee_gamma: None,
            mid_fee: None,
            offpeg_fee_multiplier: None,
            out_fee: None,
            gamma: None,
            lp_token: None,
            use_lending: vec![false; 3],
            precision_multipliers: vec![U256::from(1u64); 3],
            tokens_underlying: None,
            metapool_rate_style: 1,
            metapool_underlying_style: 1,
            data_provider: None,
        }
    }

    #[test]
    fn register_and_read_back_balances() {
        let mut core = BotState::new();
        let pool_id = core.register_curve_pool(&three_coin_params(10, &[1_000, 2_000, 3_000]));
        let s = core.get_curve_pool(pool_id).expect("curve pool registered");
        let id = core
            .get_curve_identity(pool_id)
            .expect("curve pool identity registered");
        assert_eq!(id.n_coins(), 3);
        assert_eq!(
            s.balances,
            vec![U256::from(1_000), U256::from(2_000), U256::from(3_000)]
        );
        assert_eq!(s.update_block, 10);
        // Genesis anchor pushed.
        assert_eq!(core.balance_vector_journal_len(pool_id), Some(1));
    }

    #[test]
    fn apply_balance_update_journals_and_lands_new_balances() {
        let mut core = BotState::new();
        let pool_id = core.register_curve_pool(&three_coin_params(10, &[1_000, 2_000, 3_000]));
        let affected = core.apply_curve_balance_update_by_pool_id(
            pool_id,
            vec![U256::from(1_500), U256::from(2_500), U256::from(3_500)],
            12,
        );
        assert_eq!(affected, Some(pool_id));
        let s = core.get_curve_pool(pool_id).expect("curve pool registered");
        assert_eq!(
            s.balances,
            vec![U256::from(1_500), U256::from(2_500), U256::from(3_500)]
        );
        assert_eq!(s.update_block, 12);
        // Genesis + the new transition delta.
        assert_eq!(core.balance_vector_journal_len(pool_id), Some(2));
    }

    #[test]
    fn apply_balance_update_is_silent_noop_on_v2_pool() {
        let mut core = BotState::new();
        // Register a V2 pool at pool_id 1, then try the Curve apply path.
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
        let affected = core.apply_curve_balance_update_by_pool_id(v2, vec![U256::from(1_500)], 5);
        assert!(
            affected.is_none(),
            "Curve apply on a V2 pool must be a silent no-op"
        );
    }

    #[test]
    fn restore_before_block_lands_at_prior_balances() {
        let mut core = BotState::new();
        let pool_id = core.register_curve_pool(&three_coin_params(10, &[1_000, 2_000, 3_000]));
        // Block 12: balances change.
        let _ = core.apply_curve_balance_update_by_pool_id(
            pool_id,
            vec![U256::from(1_500), U256::from(2_500), U256::from(3_500)],
            12,
        );
        // Restore to before block 12 → landed-at = the genesis registration
        // balances (the largest delta strictly below 12). The trait (ADR-016
        // ReorgPoolState) absorbs the field-write + returns `()`; the landed-at
        // values are read back through the state projection.
        core.restore_balance_vector_before_block(pool_id, 12)
            .expect("Some on a registered Curve pool")
            .expect("Ok (target > genesis block)");
        // Current mutable state was written back.
        let s = core.get_curve_pool(pool_id).expect("curve pool registered");
        assert_eq!(
            s.balances,
            vec![U256::from(1_000), U256::from(2_000), U256::from(3_000)]
        );
        assert_eq!(s.update_block, 10);
    }

    #[test]
    fn restore_to_before_registration_is_an_error() {
        let mut core = BotState::new();
        let pool_id = core.register_curve_pool(&three_coin_params(10, &[1_000, 2_000, 3_000]));
        // Target at the registration block → rolling back past registration.
        let res = core
            .restore_balance_vector_before_block(pool_id, 10)
            .expect("Some on registered pool");
        assert!(
            res.is_err(),
            "restoring to the registration block must error"
        );
    }

    #[test]
    fn a_ramp_crypto_fees_lp_lending_precision_round_trip() {
        // Every new identity field (A-ramp + crypto fees + lp_token +
        // use_lending + precision_multipliers) round-trips through
        // register/get — the BOMDRK acceptance criterion. Covers the
        // crypto-pool case where every field is populated.
        let mut core = BotState::new();
        let lp = Address::repeat_byte(0x99);
        let params = RegisterCurvePoolParams {
            address: Address::repeat_byte(0xc2),
            tokens: vec![Address::repeat_byte(0x01), Address::repeat_byte(0x02)],
            a_coefficient: 40,
            fee: 2_000_000,
            admin_fee: 5_000_000_000,
            rate_multipliers: vec![U256::from(10u64).pow(U256::from(18u64)); 2],
            balances: vec![U256::from(1_000), U256::from(2_000)],
            update_block: 0,
            swap_style: 2,
            lending_rate_style: 0,
            d_variant: 0,
            y_variant: 0,
            yd_variant: 0,
            base_pool: None,
            initial_a_coefficient: Some(40),
            future_a_coefficient: Some(80),
            initial_a_coefficient_time: Some(1_700_000_000),
            future_a_coefficient_time: Some(1_710_000_000),
            create_timestamp: Some(1_690_000_000),
            fee_gamma: Some(5_000_000_000),
            mid_fee: Some(2_600_000),
            offpeg_fee_multiplier: Some(2_000_000_000),
            out_fee: Some(3_000_000),
            gamma: Some(7_000_000_000),
            lp_token: Some(lp),
            use_lending: vec![true, false],
            precision_multipliers: vec![U256::from(1u64), U256::from(100u64)],
            tokens_underlying: None,
            metapool_rate_style: 1,
            metapool_underlying_style: 1,
            data_provider: None,
        };
        let pool_id = core.register_curve_pool(&params);
        let id = core
            .get_curve_identity(pool_id)
            .expect("curve identity present");

        // A-ramp.
        assert_eq!(id.initial_a_coefficient, Some(40));
        assert_eq!(id.future_a_coefficient, Some(80));
        assert_eq!(id.initial_a_coefficient_time, Some(1_700_000_000));
        assert_eq!(id.future_a_coefficient_time, Some(1_710_000_000));
        assert_eq!(id.create_timestamp, Some(1_690_000_000));
        // Crypto fees.
        assert_eq!(id.fee_gamma, Some(5_000_000_000));
        assert_eq!(id.mid_fee, Some(2_600_000));
        assert_eq!(id.offpeg_fee_multiplier, Some(2_000_000_000));
        assert_eq!(id.out_fee, Some(3_000_000));
        assert_eq!(id.gamma, Some(7_000_000_000));
        // LP token + lending + precision.
        assert_eq!(id.lp_token, Some(lp));
        assert_eq!(id.use_lending, vec![true, false]);
        assert_eq!(
            id.precision_multipliers,
            vec![U256::from(1u64), U256::from(100u64)]
        );
    }

    #[test]
    fn plain_pool_defaults_none_for_optional_identity() {
        // A non-ramping standard pool passes `None` for the new optional
        // fields; the identity round-trips them as `None` (the default
        // fallback shape other slices rely on).
        let mut core = BotState::new();
        let pool_id = core.register_curve_pool(&three_coin_params(10, &[1_000, 2_000, 3_000]));
        let id = core
            .get_curve_identity(pool_id)
            .expect("curve identity present");
        assert_eq!(id.initial_a_coefficient, None);
        assert_eq!(id.future_a_coefficient, None);
        assert_eq!(id.create_timestamp, None);
        assert_eq!(id.fee_gamma, None);
        assert_eq!(id.mid_fee, None);
        assert_eq!(id.gamma, None);
        assert_eq!(id.lp_token, None);
        assert!(id.use_lending.iter().all(|&b| !b));
        assert!(id
            .precision_multipliers
            .iter()
            .all(|x| *x == U256::from(1u64)));
    }
}
