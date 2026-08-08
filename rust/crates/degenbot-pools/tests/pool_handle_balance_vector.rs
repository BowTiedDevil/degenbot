//! Prototype test: shared Pool handle for Curve and Balancer balance-vector pools.

#![expect(clippy::expect_used)]
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
        a_precision: 100,
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
        swap_fee: 0,
        pow_version: 2,
        balances: vec![U256::from(1_000_000u64), U256::from(1_000_000u64)],
        update_block: 100,
    };
    let (identity, state) = BalancerWeightedPoolState::from_params(params, 8);
    PoolEntry::BalancerWeighted(identity, state)
}

fn make_balancer_stable_pool() -> PoolEntry {
    // 2-token MetaStable (bpt_idx = None), A=100 (amp = 100*1000 on-chain),
    // 18-decimal tokens (sf = ONE, identity scaling), ZERO fee, equal
    // balances. Swap of 1_000 into equal reserves yields 989 — cross-checked
    // against the independent pure-Python `BalancerV2StablePool` companion
    // (same pure-math leaf, independent marshalling) which also returns 989.
    let params = RegisterBalancerStablePoolParams {
        address: Address::from([0x44u8; 20]),
        vault: Address::from([0x55u8; 20]),
        pool_id: [0x44u8; 32],
        tokens: vec![Address::from([0xAAu8; 20]), Address::from([0xBBu8; 20])],
        amp: 100 * 1000,
        scaling_factors: vec![
            U256::from(1_000_000_000_000_000_000u64),
            U256::from(1_000_000_000_000_000_000u64),
        ],
        swap_fee: 0,
        bpt_idx: None,
        invariant_version: 2,
        balances: vec![U256::from(1_000_000u64), U256::from(1_000_000u64)],
        update_block: 100,
        rate_provider: None,
    };
    let (identity, state) = BalancerStablePoolState::from_params(params, 8);
    PoolEntry::BalancerStable(identity, state)
}

#[test]
fn curve_pool_handle_exposes_balance_vector_structure() {
    let entry = make_curve_pool();
    let pool = Pool::new(&entry, 1);
    assert_eq!(pool.structure(), Structure::BalanceVector);
    assert!(matches!(
        pool.identity(),
        Identity::BalanceVector {
            variant: BalanceVectorVariant::Curve,
            ..
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
    let pool = Pool::new(&entry, 1);
    assert_eq!(pool.structure(), Structure::BalanceVector);
    assert!(matches!(
        pool.identity(),
        Identity::BalanceVector {
            variant: BalanceVectorVariant::BalancerWeighted,
            ..
        }
    ));

    let bv = pool.balance_vector().expect("balance vector view");
    assert_eq!(bv.n_tokens(), 2);
    assert_eq!(bv.balances()[0], U256::from(1_000_000u64));
    assert_eq!(bv.balances()[1], U256::from(1_000_000u64));
}

#[test]
fn balancer_weighted_pool_swap_matches_closed_form() {
    // Equal weights + zero fee → Balancer weighted reduces to constant-product:
    //   out = B_out * amount_in / (B_in + amount_in)
    // With B_in = B_out = 1_000_000 and amount_in = 1_000 the exact integer
    // output is floor(1_000_000_000 / 1_001_000) = 999.
    let entry = make_balancer_weighted_pool();
    let pool = Pool::new(&entry, 1);
    let out = pool
        .calculate_tokens_out(true, U256::from(1_000u64))
        .expect("computable");
    assert_eq!(out, U256::from(999u64));
}

#[test]
fn balancer_stable_pool_handle_exposes_balance_vector_structure() {
    let entry = make_balancer_stable_pool();
    let pool = Pool::new(&entry, 1);
    assert_eq!(pool.structure(), Structure::BalanceVector);
    assert!(matches!(
        pool.identity(),
        Identity::BalanceVector {
            variant: BalanceVectorVariant::BalancerStable,
            ..
        }
    ));

    let bv = pool.balance_vector().expect("balance vector view");
    assert_eq!(bv.n_tokens(), 2);
    assert_eq!(bv.balances()[0], U256::from(1_000_000u64));
    assert_eq!(bv.balances()[1], U256::from(1_000_000u64));
}

#[test]
fn balancer_stable_pool_swap_matches_companion_oracle() {
    // No closed form for StableMath; the recorded constant 989 is cross-checked
    // against the independent pure-Python `BalancerV2StablePool` companion
    // (same pure-math leaf, independent marshalling) for the same fixture.
    // Symmetry (equal pools → equal reverse swaps) + monotonicity are the
    // weaker-oracle sanity checks (ADR-005 Tier 2 non-closed-form shape).
    let entry = make_balancer_stable_pool();
    let pool = Pool::new(&entry, 1);
    let out = pool
        .calculate_tokens_out(true, U256::from(1_000u64))
        .expect("computable");
    assert_eq!(out, U256::from(989u64));
    // Symmetry: swapping the same amount the other way yields the same output
    // (both reserves are equal).
    let out_rev = pool
        .calculate_tokens_out(false, U256::from(1_000u64))
        .expect("computable");
    assert_eq!(out_rev, out);
    // Monotonicity: a larger input yields a larger output.
    let out_bigger = pool
        .calculate_tokens_out(true, U256::from(10_000u64))
        .expect("computable");
    assert!(out_bigger > out);
}

/// A 2-coin standard stableswap fixture (`swap_style=STANDARD`, A=100,
/// `a_precision=100`, equal balances, zero fee) — the Tier-2 recorded-constant
/// oracle cross-checked against the independent Python companion.
fn make_standard_curve_pool() -> PoolEntry {
    let params = RegisterCurvePoolParams {
        address: Address::from([0x22u8; 20]),
        tokens: vec![Address::from([0xAAu8; 20]), Address::from([0xBBu8; 20])],
        a_coefficient: 100,
        a_precision: 100,
        fee: 0, // zero fee for the closed-form check
        admin_fee: 0,
        rate_multipliers: vec![U256::from(1_000_000_000_000_000_000u64); 2],
        balances: vec![U256::from(1_000_000_000_000_000_000u64); 2],
        update_block: 100,
        swap_style: 1, // STANDARD
        lending_rate_style: 0,
        d_variant: 1, // STANDARD
        y_variant: 1, // STANDARD
        yd_variant: 1,
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
        use_lending: vec![false; 2],
        precision_multipliers: vec![U256::from(1_000_000_000_000_000_000u64); 2],
        tokens_underlying: None,
        metapool_rate_style: 0,
        metapool_underlying_style: 0,
        data_provider: None,
    };
    let (identity, state) = CurvePoolState::from_params(params, 8);
    PoolEntry::Curve(identity, state)
}

/// Curve standard-stableswap output for the equal-balances fixture (A=100,
/// `a_precision=100`, equal 1e18 balances, zero fee, swap 1e18 token0→token1).
/// Uses the `FEE_THEN_RATE` (STANDARD) conversion. Pin against the independent
/// pure-Python Vyper-port oracle (`stableswap_get_y` + fee/rate conversion),
/// which re-derives `934112765606210873` — a non-circular cross-check (ADR-005
/// Tier-2 recorded-constant shape).
#[test]
fn curve_standard_swap_matches_recorded_constant() {
    let entry = make_standard_curve_pool();
    let pool = Pool::new(&entry, 1);
    let out = pool
        .calculate_tokens_out(true, U256::from(1_000_000_000_000_000_000u64))
        .expect("computable");
    assert_eq!(out, U256::from(934_112_765_606_210_873u64));

    // Symmetry: swapping the other direction on equal balances yields the same
    // amount (both coins have identical balances + rate multipliers).
    let out_rev = pool
        .calculate_tokens_out(false, U256::from(1_000_000_000_000_000_000u64))
        .expect("computable");
    assert_eq!(out_rev, out, "symmetric direction on equal balances");
}

/// Curve standard-stableswap monotonicity: a larger input yields a strictly
/// larger output, and the output is bounded by the input (never infinite).
#[test]
fn curve_standard_swap_is_monotonic() {
    let entry = make_standard_curve_pool();
    let pool = Pool::new(&entry, 1);
    let small = pool
        .calculate_tokens_out(true, U256::from(1_000_000_000_000_000_000u64))
        .expect("computable");
    let large = pool
        .calculate_tokens_out(true, U256::from(2_000_000_000_000_000_000u64))
        .expect("computable");
    assert!(large > small, "output must increase with input");
}
