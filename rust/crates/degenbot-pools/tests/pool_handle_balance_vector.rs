//! Prototype test: shared Pool handle for Curve and Balancer balance-vector pools.

use alloy::primitives::{Address, U256};
use degenbot_pools::balancer_stable_state::{
    BalancerStablePoolState, RegisterBalancerStablePoolParams,
};
use degenbot_pools::balancer_weighted_state::{
    BalancerWeightedPoolState, RegisterBalancerWeightedPoolParams,
};
use degenbot_pools::curve_state::{CurvePoolState, RegisterCurvePoolParams};
use degenbot_pools::registry::PoolEntry;
use degenbot_pools::{BalanceVectorVariant, Identity, Pool, Structure};

fn make_curve_pool() -> PoolEntry {
    let params = RegisterCurvePoolParams {
        address: Address::from([0x11u8; 20]),
        tokens: vec![
            Address::from([0xAAu8; 20]),
            Address::from([0xBBu8; 20]),
            Address::from([0xCCu8; 20]),
        ],
        a_coefficient: 100,
        fee: 4_000_000,
        admin_fee: 0,
        rate_multipliers: vec![
            U256::from(1_000_000_000_000_000_000u64),
            U256::from(1_000_000_000_000_000_000u64),
            U256::from(1_000_000_000_000_000_000u64),
        ],
        balances: vec![
            U256::from(1_000_000_000u64),
            U256::from(2_000_000_000u64),
            U256::from(3_000_000_000u64),
        ],
        update_block: 100,
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
        precision_multipliers: vec![
            U256::from(1_000_000_000_000_000_000u64),
            U256::from(1_000_000_000_000_000_000u64),
            U256::from(1_000_000_000_000_000_000u64),
        ],
        tokens_underlying: None,
        metapool_rate_style: 0,
        metapool_underlying_style: 0,
        data_provider: None,
    };
    let (identity, state) = CurvePoolState::from_params(params, 8);
    PoolEntry::Curve(identity, state)
}

fn make_balancer_weighted_pool() -> PoolEntry {
    let params = RegisterBalancerWeightedPoolParams {
        address: Address::from([0x22u8; 20]),
        vault: Address::from([0x33u8; 20]),
        pool_id: [0u8; 32],
        tokens: vec![Address::from([0xAAu8; 20]), Address::from([0xBBu8; 20])],
        weights: vec![
            U256::from(500_000_000_000_000_000u64),
            U256::from(500_000_000_000_000_000u64),
        ],
        scaling_factors: vec![
            U256::from(1_000_000_000_000_000_000u64),
            U256::from(1_000_000_000_000_000_000u64),
        ],
        swap_fee: 1_000_000_000_000_000_000u128 / 100, // 1%
        pow_version: 2,
        balances: vec![U256::from(1_000_000_000u64), U256::from(2_000_000_000u64)],
        update_block: 100,
    };
    let (identity, state) = BalancerWeightedPoolState::from_params(params, 8);
    PoolEntry::BalancerWeighted(identity, state)
}

fn make_balancer_stable_pool() -> PoolEntry {
    let params = RegisterBalancerStablePoolParams {
        address: Address::from([0x44u8; 20]),
        vault: Address::from([0x55u8; 20]),
        pool_id: [0u8; 32],
        tokens: vec![Address::from([0xAAu8; 20]), Address::from([0xBBu8; 20])],
        amp: 1_000,
        scaling_factors: vec![
            U256::from(1_000_000_000_000_000_000u64),
            U256::from(1_000_000_000_000_000_000u64),
        ],
        swap_fee: 1_000_000_000_000_000_000u128 / 100, // 1%
        bpt_idx: None,
        invariant_version: 1,
        balances: vec![U256::from(1_000_000_000u64), U256::from(2_000_000_000u64)],
        update_block: 100,
        rate_provider: None,
    };
    let (identity, state) = BalancerStablePoolState::from_params(params, 8);
    PoolEntry::BalancerStable(identity, state)
}

#[test]
fn curve_pool_handle_exposes_balance_vector_structure() {
    let entry = make_curve_pool();
    let pool = Pool::new(&entry);
    assert_eq!(pool.structure(), Structure::BalanceVector);
    assert!(matches!(
        pool.identity(),
        Identity::BalanceVector {
            variant: BalanceVectorVariant::Curve,
        }
    ));

    let bv = pool.balance_vector().expect("balance vector view");
    assert_eq!(bv.n_tokens(), 3);
    assert_eq!(bv.tokens().len(), 3);
    assert_eq!(bv.balances().len(), 3);
    assert_eq!(bv.balances()[0], U256::from(1_000_000_000));
}

#[test]
fn balancer_weighted_pool_handle_exposes_balance_vector_structure() {
    let entry = make_balancer_weighted_pool();
    let pool = Pool::new(&entry);
    assert_eq!(pool.structure(), Structure::BalanceVector);
    assert!(matches!(
        pool.identity(),
        Identity::BalanceVector {
            variant: BalanceVectorVariant::BalancerWeighted,
        }
    ));

    let bv = pool.balance_vector().expect("balance vector view");
    assert_eq!(bv.n_tokens(), 2);
    assert_eq!(bv.balances()[0], U256::from(1_000_000_000));
    assert_eq!(bv.balances()[1], U256::from(2_000_000_000));
}

#[test]
fn balancer_stable_pool_handle_exposes_balance_vector_structure() {
    let entry = make_balancer_stable_pool();
    let pool = Pool::new(&entry);
    assert_eq!(pool.structure(), Structure::BalanceVector);
    assert!(matches!(
        pool.identity(),
        Identity::BalanceVector {
            variant: BalanceVectorVariant::BalancerStable,
        }
    ));

    let bv = pool.balance_vector().expect("balance vector view");
    assert_eq!(bv.n_tokens(), 2);
    assert_eq!(bv.balances()[0], U256::from(1_000_000_000));
}
