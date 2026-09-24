use super::*;

// -----------------------------------------------------------------------
// Balancer stable solve branch (IVLQRB)
// -----------------------------------------------------------------------
/// Two-token Balancer stable pool params (`MetaStable` — no BPT), amp=200,
/// 0.01% fee, invariant V2.
fn balancer_stable_params(
    addr: Address,
    balance0: u128,
    balance1: u128,
) -> crate::bot_core::RegisterBalancerStablePoolParams {
    let one_e18 = U256::from(10u64).pow(U256::from(18u64));
    crate::bot_core::RegisterBalancerStablePoolParams {
        address: addr,
        vault: Address::repeat_byte(0xba),
        pool_id: [0u8; 32],
        tokens: vec![Address::repeat_byte(0x01), Address::repeat_byte(0x02)],
        // amp=200_000 = raw_amp(200) * AMP_PRECISION(1000) — matches the
        // deployed contract's getAmplificationParameter() return.
        amp: 200_000,
        scaling_factors: vec![U256::from(1u64), U256::from(1u64)],
        swap_fee: 10_000_000_000_000u128, // 0.01% of 1e18
        bpt_idx: None,
        invariant_version: 2,
        balances: vec![
            U256::from(balance0) * one_e18,
            U256::from(balance1) * one_e18,
        ],
        update_block: 0,
        rate_provider: None,
    }
}
#[test]
fn balancer_stable_finds_profitable_arb() {
    let mut engine = ArbitrageEngine::new();
    // Pool A: 1000 token0 / 2000 token1 (amp=200 — stable curve)
    let pool_a = engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_balancer_stable_pool(&balancer_stable_params(
            Address::from([0xe1u8; 20]),
            1000,
            2000,
        ));
    // Pool B: 1000 token0 / 1950 token1 (mispriced)
    let pool_b = engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_balancer_stable_pool(&balancer_stable_params(
            Address::from([0xe2u8; 20]),
            1000,
            1950,
        ));
    // Path: token0 → token1 (pool A) → token0 (pool B)
    register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: pool_a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: pool_b,
                zero_for_one: false,
            },
        ],
    )
    .unwrap();
    let results = engine.cycle.solve_all(&engine.registry);
    assert!(
        !results.is_empty(),
        "should find profitable Balancer stable arb"
    );
    let r = results.values().next().unwrap();
    assert!(
        !r.optimal_input.is_zero(),
        "optimal input should be non-zero"
    );
    assert!(!r.profit.is_zero(), "profit should be non-zero");
}
#[test]
fn balancer_stable_unprofitable_path_returns_none() {
    let mut engine = ArbitrageEngine::new();
    // Two identical pools — no arb possible.
    let pool_a = engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_balancer_stable_pool(&balancer_stable_params(
            Address::from([0xf1u8; 20]),
            1000,
            2000,
        ));
    let pool_b = engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_balancer_stable_pool(&balancer_stable_params(
            Address::from([0xf2u8; 20]),
            1000,
            2000,
        ));
    register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: pool_a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: pool_b,
                zero_for_one: false,
            },
        ],
    )
    .unwrap();
    let results = engine.cycle.solve_all(&engine.registry);
    assert!(
        results.is_empty(),
        "identical stable pools should not produce an arb"
    );
}
#[test]
fn balancer_stable_mixed_with_v2_finds_arb() {
    let mut engine = ArbitrageEngine::new();
    let one_e18 = U256::from(10u64).pow(U256::from(18u64));
    // V2 pool: 1000/2000, 0.3% fee
    let v2 = engine.register_v2_pool(
        Address::from([0xa3u8; 20]),
        (U256::from(1000u64) * one_e18).to::<U112>(),
        (U256::from(2000u64) * one_e18).to::<U112>(),
        GAMMA_03,
        FEE_DENOM_03,
    );
    // Balancer stable pool: 1000/1950 (mispriced), 0.01% fee
    let bs = engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_balancer_stable_pool(&balancer_stable_params(
            Address::from([0xa4u8; 20]),
            1000,
            1950,
        ));
    // V2 → Balancer stable path
    register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v2,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: bs,
                zero_for_one: false,
            },
        ],
    )
    .unwrap();
    let results = engine.cycle.solve_all(&engine.registry);
    assert!(
        !results.is_empty(),
        "should find V2+Balancer-stable mixed arb"
    );
}
#[test]
fn balancer_stable_rejects_mixed_with_cl() {
    use std::sync::Arc;
    let core = Arc::new(crate::bot_core::state_lock::StateLock::new(
        crate::bot_core::BotState::new(),
    ));
    let bs = core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_balancer_stable_pool(&balancer_stable_params(
            Address::from([0xb3u8; 20]),
            1000,
            2000,
        ));
    let v3 = core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_v3_pool(&RegisterV3PoolParams {
            address: Address::from([0xc3u8; 20]),
            token0: Address::repeat_byte(0x01),
            token1: Address::repeat_byte(0x02),
            fee: 500,
            tick_spacing: 10,
            sqrt_price_x96: U256::from(1u64) << 96,
            tick: 0,
            liquidity: 1_000_000,
            tick_data: HashMap::new(),
            update_block: 0,
            tick_data_block: None,
            ..Default::default()
        })
        .expect("test setup: V3 registration");
    let mut engine = ArbitrageEngine::with_core(Arc::clone(&core));
    let path_id = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: bs,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v3,
                zero_for_one: false,
            },
        ],
    )
    .expect("path registers (resolve succeeds per-arm)");
    let resolved = &engine.cycle.path_resolved[&path_id];
    assert!(
        ::degenbot_solvers::mixed::solve_path(
            resolved,
            &::degenbot_solvers::profit_envelope::GateDeps::offline()
        )
        .result
        .is_none(),
        "Balancer stable + CL must not solve"
    );
}
