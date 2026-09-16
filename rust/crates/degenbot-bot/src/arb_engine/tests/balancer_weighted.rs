use super::*;

// -----------------------------------------------------------------------
// Balancer weighted solve branch
// -----------------------------------------------------------------------
/// Two-token Balancer weighted pool params, 50/50 weights, 0.1% fee.
fn balancer_weighted_5050_params(
    addr: Address,
    balance0: u128,
    balance1: u128,
) -> crate::bot_core::RegisterBalancerWeightedPoolParams {
    crate::bot_core::RegisterBalancerWeightedPoolParams {
        address: addr,
        vault: Address::repeat_byte(0xba),
        pool_id: [0u8; 32],
        tokens: vec![Address::repeat_byte(0x01), Address::repeat_byte(0x02)],
        weights: vec![
            U256::from(500_000_000_000_000_000u128),
            U256::from(500_000_000_000_000_000u128),
        ],
        scaling_factors: vec![U256::from(1u64), U256::from(1u64)],
        swap_fee: 1_000_000_000_000_000u128, // 0.1% of 1e18
        pow_version: 2,
        // Balances are passed as token amounts; multiply by 1e18 to
        // upscale to 18-decimal fixed-point (scaling_factors=[1,1]).
        balances: vec![
            U256::from(balance0) * U256::from(10u64).pow(U256::from(18u64)),
            U256::from(balance1) * U256::from(10u64).pow(U256::from(18u64)),
        ],
        update_block: 0,
    }
}
/// 80/20 weighted pool params.
fn balancer_weighted_8020_params(
    addr: Address,
    balance0: u128,
    balance1: u128,
) -> crate::bot_core::RegisterBalancerWeightedPoolParams {
    crate::bot_core::RegisterBalancerWeightedPoolParams {
        address: addr,
        vault: Address::repeat_byte(0xba),
        pool_id: [0u8; 32],
        tokens: vec![Address::repeat_byte(0x01), Address::repeat_byte(0x02)],
        weights: vec![
            U256::from(800_000_000_000_000_000u128),
            U256::from(200_000_000_000_000_000u128),
        ],
        scaling_factors: vec![U256::from(1u64), U256::from(1u64)],
        swap_fee: 1_000_000_000_000_000u128,
        pow_version: 2,
        balances: vec![
            U256::from(balance0) * U256::from(10u64).pow(U256::from(18u64)),
            U256::from(balance1) * U256::from(10u64).pow(U256::from(18u64)),
        ],
        update_block: 0,
    }
}
#[test]
fn balancer_weighted_5050_finds_profitable_arb() {
    let mut engine = ArbitrageEngine::new();
    let one = U256::from(10u64).pow(U256::from(18u64));
    let _ = one; // reserved for future reserve-scale assertions
                 // Pool A: 1000 token0 / 2000 token1 (50/50 — reduces to constant product)
    let pool_a = engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_balancer_weighted_pool(&balancer_weighted_5050_params(
            Address::from([0xd1u8; 20]),
            1000,
            2000,
        ));
    // Pool B: 1000 token0 / 1950 token1 (mispriced — cheaper token1 here)
    let pool_b = engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_balancer_weighted_pool(&balancer_weighted_5050_params(
            Address::from([0xd2u8; 20]),
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
        "should find profitable 50/50 weighted arb"
    );
    let r = results.values().next().unwrap();
    assert!(
        !r.optimal_input.is_zero(),
        "optimal input should be non-zero"
    );
    assert!(!r.profit.is_zero(), "profit should be non-zero");
}
#[test]
fn balancer_weighted_8020_finds_profitable_arb() {
    let mut engine = ArbitrageEngine::new();
    // 80/20 pools with a mispricing to create an arb cycle.
    let pool_a = engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_balancer_weighted_pool(&balancer_weighted_8020_params(
            Address::from([0xe1u8; 20]),
            800_000,
            200_000,
        ));
    let pool_b = engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_balancer_weighted_pool(&balancer_weighted_8020_params(
            Address::from([0xe2u8; 20]),
            800_000,
            195_000,
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
        !results.is_empty(),
        "should find profitable 80/20 weighted arb"
    );
    let r = results.values().next().unwrap();
    assert!(!r.optimal_input.is_zero());
    assert!(!r.profit.is_zero());
}
#[test]
fn balancer_weighted_5050_matches_v2_mobius_on_same_reserves() {
    // A 50/50 weighted pool IS constant product. The engine's Balancer
    // weighted solve must agree with the V2 Möbius solve on identical
    // reserves + fee.
    let mut engine = ArbitrageEngine::new();
    // V2 pools: 0.3% fee, 1000/2000 reserves in 18-decimal (matching BW scale).
    let v2_a = engine.register_v2_pool(
        Address::from([0xf1u8; 20]),
        (U256::from(1000u64) * U256::from(10u64).pow(U256::from(18u64))).to::<U112>(),
        (U256::from(2000u64) * U256::from(10u64).pow(U256::from(18u64))).to::<U112>(),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let v2_b = engine.register_v2_pool(
        Address::from([0xf2u8; 20]),
        (U256::from(1000u64) * U256::from(10u64).pow(U256::from(18u64))).to::<U112>(),
        (U256::from(1950u64) * U256::from(10u64).pow(U256::from(18u64))).to::<U112>(),
        GAMMA_03,
        FEE_DENOM_03,
    );
    // Balancer 50/50 weighted pools with 0.3% fee, same reserves.
    let bw_params = |addr: Address, b0: u128, b1: u128| {
        crate::bot_core::RegisterBalancerWeightedPoolParams {
            address: addr,
            vault: Address::repeat_byte(0xba),
            pool_id: [0u8; 32],
            tokens: vec![Address::repeat_byte(0x01), Address::repeat_byte(0x02)],
            weights: vec![
                U256::from(500_000_000_000_000_000u128),
                U256::from(500_000_000_000_000_000u128),
            ],
            scaling_factors: vec![U256::from(1u64), U256::from(1u64)],
            swap_fee: 3_000_000_000_000_000u128, // 0.3% of 1e18
            pow_version: 2,
            balances: vec![
                U256::from(b0) * U256::from(10u64).pow(U256::from(18u64)),
                U256::from(b1) * U256::from(10u64).pow(U256::from(18u64)),
            ],
            update_block: 0,
        }
    };
    let bw_a = engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_balancer_weighted_pool(&bw_params(Address::from([0xf3u8; 20]), 1000, 2000));
    let bw_b = engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_balancer_weighted_pool(&bw_params(Address::from([0xf4u8; 20]), 1000, 1950));
    // Solve V2-V2 path
    register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v2_a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v2_b,
                zero_for_one: false,
            },
        ],
    )
    .unwrap();
    let v2_results = engine.cycle.solve_all(&engine.registry);
    let v2_profit = v2_results.values().next().unwrap().profit;
    // Solve Balancer-V2-V2 path (clear and re-solve)
    drop(
        engine
            .core
            .write_at(crate::bot_core::state_lock::LockSite::Solver),
    );
    let bw_path = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: bw_a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: bw_b,
                zero_for_one: false,
            },
        ],
    )
    .unwrap();
    // resolve + solve the bw path specifically
    let resolved = &engine.cycle.path_resolved[&bw_path];
    let bw_result = ::degenbot_solvers::mixed::solve_path(
        resolved,
        &::degenbot_solvers::profit_envelope::GateDeps::offline(),
    )
    .result
    .expect("bw path should solve");
    // The two profits should be in the same ballpark (within 1% of each
    // other — the Balancer weighted solve uses golden-section search, not
    // the exact Möbius closed form, so there's small search imprecision).
    let one_pct = v2_profit / U256::from(100u64);
    let diff = if v2_profit > bw_result.profit {
        v2_profit - bw_result.profit
    } else {
        bw_result.profit - v2_profit
    };
    assert!(
        diff <= one_pct,
        "50/50 weighted profit {} should match V2 Möbius profit {} within 1%",
        bw_result.profit,
        v2_profit,
    );
}
#[test]
fn balancer_weighted_mixed_with_v2_finds_arb() {
    let mut engine = ArbitrageEngine::new();
    let one_e18 = U256::from(10u64).pow(U256::from(18u64));
    // V2 pool: token0/token1, 1000/2000 in 18dp, 0.3% fee
    let v2 = engine.register_v2_pool(
        Address::from([0xa1u8; 20]),
        (U256::from(1000u64) * one_e18).to::<U112>(),
        (U256::from(2000u64) * one_e18).to::<U112>(),
        GAMMA_03,
        FEE_DENOM_03,
    );
    // Balancer weighted 50/50 pool: 1000/1950, 0.3% fee (mispriced)
    let bw_params = crate::bot_core::RegisterBalancerWeightedPoolParams {
        address: Address::from([0xa2u8; 20]),
        vault: Address::repeat_byte(0xba),
        pool_id: [0u8; 32],
        tokens: vec![Address::repeat_byte(0x01), Address::repeat_byte(0x02)],
        weights: vec![
            U256::from(500_000_000_000_000_000u128),
            U256::from(500_000_000_000_000_000u128),
        ],
        scaling_factors: vec![U256::from(1u64), U256::from(1u64)],
        swap_fee: 3_000_000_000_000_000u128, // 0.3%
        pow_version: 2,
        balances: vec![
            U256::from(1000u64) * U256::from(10u64).pow(U256::from(18u64)),
            U256::from(1950u64) * U256::from(10u64).pow(U256::from(18u64)),
        ],
        update_block: 0,
    };
    let bw = engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_balancer_weighted_pool(&bw_params);
    // V2 → Balancer weighted path
    register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v2,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: bw,
                zero_for_one: false,
            },
        ],
    )
    .unwrap();
    let results = engine.cycle.solve_all(&engine.registry);
    // V2+Balancer-weighted is all-V2-or-weighted with no CL — should solve
    assert!(!results.is_empty(), "should find V2+Balancer-weighted arb");
}
#[test]
fn balancer_weighted_rejects_mixed_with_cl() {
    use std::sync::Arc;
    let core = Arc::new(crate::bot_core::state_lock::StateLock::new(
        crate::bot_core::BotState::new(),
    ));
    // Register a Balancer weighted pool
    let bw = core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_balancer_weighted_pool(&balancer_weighted_5050_params(
            Address::from([0xb1u8; 20]),
            1000,
            2000,
        ));
    // Register a V3 pool
    let v3 = core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_v3_pool(&RegisterV3PoolParams {
            address: Address::from([0xc1u8; 20]),
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
                pool_id: bw,
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
    // Balancer weighted + CL is out of scope — solve_path returns None.
    assert!(
        ::degenbot_solvers::mixed::solve_path(
            resolved,
            &::degenbot_solvers::profit_envelope::GateDeps::offline()
        )
        .result
        .is_none(),
        "Balancer weighted + CL must not solve"
    );
}
/// the core `ArbitrageEngine` OWNS the lifecycle phase — a
/// standalone Rust consumer can observe + guard the state machine directly
/// (`current_phase` / `set_phase` / `require_phase` / `require_phase_before`)
/// with no Python in the loop. Pins the Created → Subscribed →
/// `SnapshotLoaded` → `Backfilled` → `Resumed` ordering with gate
/// enforcement.
#[test]
fn core_engine_owns_and_guards_the_lifecycle_phase() {
    let engine = ArbitrageEngine::new();
    // Fresh engine → Created.
    assert_eq!(engine.current_phase(), EnginePhase::Created);
    // Gate: require_phase(SnapshotLoaded) fails from Created.
    assert!(engine
        .require_phase(EnginePhase::SnapshotLoaded, "resume")
        .is_err());
    // require_phase_before(Subscribed) succeeds while Created.
    assert!(engine
        .require_phase_before(EnginePhase::Subscribed, "subscribe")
        .is_ok());
    // subscribe is allowed from Created, advances to Subscribed.
    assert!(engine.current_phase().allow_subscribe("subscribe").is_ok());
    engine.set_phase(EnginePhase::Subscribed);
    assert_eq!(engine.current_phase(), EnginePhase::Subscribed);
    // Advance through the full ordering.
    engine.set_phase(EnginePhase::SnapshotLoaded);
    engine.set_phase(EnginePhase::Backfilled);
    engine.set_phase(EnginePhase::Resumed);
    assert_eq!(engine.current_phase(), EnginePhase::Resumed);
    // Once Resumed, require_phase_before(Resumed) fails (already past),
    // and require_phase(Resumed) is satisfied.
    assert!(engine
        .require_phase_before(EnginePhase::Resumed, "resume")
        .is_err());
    assert!(engine.require_phase(EnginePhase::Resumed, "solve").is_ok());
}
/// `allow_subscribe` accepts `Created` (legacy subscribe-first path)
/// AND `SnapshotLoaded` (construction-time-load path: load snapshot, then
/// subscribe). Rejects `Subscribed`/`Backfilled`/`Resumed`.
#[test]
fn allow_subscribe_accepts_created_and_snapshot_loaded() {
    assert!(EnginePhase::Created.allow_subscribe("subscribe").is_ok());
    assert!(EnginePhase::SnapshotLoaded
        .allow_subscribe("subscribe")
        .is_ok());
    assert!(EnginePhase::Subscribed
        .allow_subscribe("subscribe")
        .is_err());
    assert!(EnginePhase::Backfilled
        .allow_subscribe("subscribe")
        .is_err());
    assert!(EnginePhase::Resumed.allow_subscribe("subscribe").is_err());
}
/// J3FMDO regression: `subscribe()` must not regress the phase below
/// `SnapshotLoaded` when the core already has a snapshot loaded (the
/// construction-time-load path: `load_snapshot_from_db` at `Bot`
/// construction → `subscribe`). The snapshot is loaded into the shared
/// core `BotState` and never advances the engine phase, so an
/// unconditional `set_phase(Subscribed)` after subscribe left the phase
/// at `Subscribed` (1) and `resume()`'s `require(SnapshotLoaded)` guard
/// (needs `>= 2`) crashed the production settlement-arbitrage bot:
///
///   `RuntimeError`: Cannot call resume: engine is in phase Subscribed,
///                 but requires `SnapshotLoaded`
///
/// `after_subscribe(current, core_has_snapshot)` computes the correct
/// post-subscribe phase so `resume()` is reachable from BOTH paths.
#[test]
fn after_subscribe_advances_to_snapshot_loaded_when_core_has_snapshot() {
    // Legacy path (no core snapshot; snapshot loaded AFTER subscribe via
    // `load_*_snapshot_from_py`): Created → subscribe → Subscribed.
    assert_eq!(
        EnginePhase::after_subscribe(EnginePhase::Created, false),
        EnginePhase::Subscribed,
        "legacy path: no core snapshot → Subscribed after subscribe"
    );
    // Construction-time-load path: core has a snapshot (loaded at `Bot`
    // construction via `load_snapshot_from_db`). subscribe from Created →
    // SnapshotLoaded (NOT Subscribed — that was the crash).
    assert_eq!(
        EnginePhase::after_subscribe(EnginePhase::Created, true),
        EnginePhase::SnapshotLoaded,
        "construction-load path: core has snapshot → SnapshotLoaded after subscribe"
    );
    // Legacy pre-subscribe load: snapshot already loaded into the engine
    // (phase == SnapshotLoaded) BEFORE subscribe. subscribe must NOT
    // regress the phase back to Subscribed (the old `set_phase(Subscribed)`
    // was a regression here too).
    assert_eq!(
        EnginePhase::after_subscribe(EnginePhase::SnapshotLoaded, false),
        EnginePhase::SnapshotLoaded,
        "pre-subscribe load: subscribe must not regress SnapshotLoaded → Subscribed"
    );
    // Pre-subscribe load AND core has snapshot — still SnapshotLoaded.
    assert_eq!(
        EnginePhase::after_subscribe(EnginePhase::SnapshotLoaded, true),
        EnginePhase::SnapshotLoaded,
        "SnapshotLoaded + core snapshot → SnapshotLoaded (no regression)"
    );
}
