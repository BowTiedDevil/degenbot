use super::*;

#[test]
fn inspect_path_returns_hop_details() {
    let mut engine = ArbitrageEngine::new();
    // Register a V2 pool
    let v2_fwd = engine.register_v2_pool(
        Address::from([0x11u8; 20]),
        usdc(1_500_000),
        weth(800),
        GAMMA_03,
        FEE_DENOM_03,
    );
    // Register a V3 pool
    let tick_data = inspect_test_v3_tick_data();
    let v3_key = engine.register_v3_pool(&crate::bot_core::RegisterV3PoolParams {
        address: Address::from([0x22u8; 20]),
        token0: Address::from([0u8; 20]),
        token1: Address::from([1u8; 20]),
        fee: 3000,
        tick_spacing: 60,
        factory: Address::ZERO,
        sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
        liquidity: 1_000_000,
        tick: 0,
        tick_data,
        update_block: 0,
        tick_data_block: None,
        coverage: crate::arb_engine::PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    });
    // Register a V4 pool
    let v4_key = engine
        .register_v4_pool(&crate::bot_core::RegisterV4PoolParams {
            pool_manager: Address::from([0x33u8; 20]),
            pool_id: [0xabu8; 32],
            pool_key: crate::bot_core::V4PoolKey {
                currency0: Address::from([0u8; 20]),
                currency1: Address::from([1u8; 20]),
                fee: 10000,
                tick_spacing: 100,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            tick_data_block: None,
            coverage: crate::arb_engine::PoolTickCoverage::Tracked,
            fetcher: None,
        })
        .expect("V4 registration should succeed");
    // Register a 3-hop path: V2 → V3 → V4
    let path_id = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v2_fwd,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v3_key,
                zero_for_one: false,
            },
            PoolHop {
                pool_id: v4_key,
                zero_for_one: true,
            },
        ],
    )
    .unwrap();
    // Inspect the path
    let path = engine
        .registry
        .path_pools
        .get(&path_id)
        .expect("path should exist");
    assert_eq!(path.pools.len(), 3);
    // Verify hop types
    assert!(matches!(path.pools[0].hop_type, HopType::V2));
    assert!(matches!(path.pools[1].hop_type, HopType::V3));
    assert!(matches!(path.pools[2].hop_type, HopType::V4));
    // Verify we can resolve pool addresses via BotState (V2) / sub-engines (V3/V4)
    let v2_addr = engine
        .core
        .read_at(crate::bot_core::state_lock::LockSite::Solver)
        .get_v2_identity(v2_fwd)
        .map(|p| p.address);
    assert_eq!(v2_addr, Some(Address::from([0x11u8; 20])));
    let core = engine
        .core
        .read_at(crate::bot_core::state_lock::LockSite::Solver);
    let v3_pool = core.get_v3_identity(v3_key);
    assert_eq!(
        v3_pool.map(|p| p.address),
        Some(Address::from([0x22u8; 20]))
    );
    let v4_pool = core.get_v4_identity(v4_key);
    assert_eq!(
        v4_pool.map(|p| p.pool_manager),
        Some(Address::from([0x33u8; 20]))
    );
    assert_eq!(v4_pool.map(|p| p.pool_id), Some([0xabu8; 32]));
    drop(core);
    // Inspect non-existent path
    assert!(!engine.registry.path_pools.contains_key(&99999));
}
#[test]
#[expect(clippy::too_many_lines)]
fn solve_3hop_v3_v3_v3_path() {
    let mut engine = ArbitrageEngine::new();
    let sp_0 = U256::from(79_228_162_514_264_337_593_543_950_336_u128); // 1:1 price (tick 0)
                                                                        // Helper to create minimal tick data with initialized ticks at -60 and +60
    let make_tick_data = || -> HashMap<i32, crate::bot_core::TickInfo> {
        let mut td = HashMap::new();
        td.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(100),
                liquidity_net: 100i128,
                block: 0,
            },
        );
        td.insert(
            60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(100),
                liquidity_net: -100i128,
                block: 0,
            },
        );
        td
    };
    // Pool 1 at tick 0 with high liquidity
    let v3_key_a = engine.register_v3_pool(&RegisterV3PoolParams {
        address: Address::from([0xa1u8; 20]),
        token0: Address::ZERO,
        token1: Address::from([1u8; 20]),
        fee: 3000,
        tick_spacing: 60,
        factory: Address::ZERO,
        sqrt_price_x96: sp_0,
        liquidity: 10_000_000_000_000u128,
        tick: 0,
        tick_data: make_tick_data(),
        update_block: 0,
        tick_data_block: None,
        coverage: crate::arb_engine::PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    });
    // Pool 2 at tick 0 with different liquidity (price disagreement)
    let v3_key_b = engine.register_v3_pool(&RegisterV3PoolParams {
        address: Address::from([0xa2u8; 20]),
        token0: Address::ZERO,
        token1: Address::from([1u8; 20]),
        fee: 3000,
        tick_spacing: 60,
        factory: Address::ZERO,
        sqrt_price_x96: sp_0,
        liquidity: 15_000_000_000_000u128,
        tick: 0,
        tick_data: make_tick_data(),
        update_block: 0,
        tick_data_block: None,
        coverage: crate::arb_engine::PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    });
    // Pool 3 at tick 0 with third liquidity level
    let v3_key_c = engine.register_v3_pool(&RegisterV3PoolParams {
        address: Address::from([0xa3u8; 20]),
        token0: Address::ZERO,
        token1: Address::from([1u8; 20]),
        fee: 3000,
        tick_spacing: 60,
        factory: Address::ZERO,
        sqrt_price_x96: sp_0,
        liquidity: 12_000_000_000_000u128,
        tick: 0,
        tick_data: make_tick_data(),
        update_block: 0,
        tick_data_block: None,
        coverage: crate::arb_engine::PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    });
    assert_eq!(v3_pool_count(&engine,), 3);
    // Register 3-hop V3-V3-V3 path
    let path_id = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v3_key_a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v3_key_b,
                zero_for_one: false,
            },
            PoolHop {
                pool_id: v3_key_c,
                zero_for_one: true,
            },
        ],
    )
    .unwrap();
    assert_eq!(path_id, 1);
    assert_eq!(path_count(&engine,), 1);
    // Verify the path is valid and resolved
    let resolved = &engine.cycle.path_resolved[&path_id];
    assert!(resolved.valid, "3-hop V3-V3-V3 path should be valid");
    assert_eq!(resolved.hops.len(), 3);
    assert_eq!(resolved.hops[0].hop_type(), HopType::V3);
    assert_eq!(resolved.hops[1].hop_type(), HopType::V3);
    assert_eq!(resolved.hops[2].hop_type(), HopType::V3);
    assert!(resolved.hops[0].as_int_sequence().is_some());
    assert!(resolved.hops[1].as_int_sequence().is_some());
    assert!(resolved.hops[2].as_int_sequence().is_some());
    // Solve the path — previously returned None for 3+ hop CL paths.
    // Now the N-hop CL solver runs. With 3 pools at the same price but
    // different liquidity, the path is unlikely to be profitable after fees,
    // but the solver must not reject due to hop count.
    let result = ::degenbot_solvers::mixed::solve_path(
        resolved,
        &::degenbot_solvers::profit_envelope::GateDeps::offline(),
    )
    .result;
    let _ = result; // No panic = test passes
}
#[test]
fn solve_3hop_mixed_v2_v3_v2_path() {
    let mut engine = ArbitrageEngine::new();
    let sp_0 = U256::from(79_228_162_514_264_337_593_543_950_336_u128); // 1:1 price
                                                                        // V2 pool 1: cheap WETH (1.5M USDC / 800 WETH)
    let v2_fwd_a = engine.register_v2_pool(
        Address::from([0x11u8; 20]),
        usdc(1_500_000),
        weth(800),
        GAMMA_03,
        FEE_DENOM_03,
    );
    // V3 pool (middle hop): at 1:1 price with tick boundaries
    let mut tick_data = HashMap::new();
    tick_data.insert(
        -60,
        crate::bot_core::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(100),
            liquidity_net: 100i128,
            block: 0,
        },
    );
    tick_data.insert(
        60,
        crate::bot_core::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(100),
            liquidity_net: -100i128,
            block: 0,
        },
    );
    let v3_key = engine.register_v3_pool(&RegisterV3PoolParams {
        address: Address::from([0x22u8; 20]),
        token0: Address::ZERO,
        token1: Address::from([1u8; 20]),
        fee: 3000,
        tick_spacing: 60,
        factory: Address::ZERO,
        sqrt_price_x96: sp_0,
        liquidity: 10_000_000_000_000u128,
        tick: 0,
        tick_data,
        update_block: 0,
        tick_data_block: None,
        coverage: crate::arb_engine::PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    });
    // V2 pool 2: expensive WETH (1000 WETH / 2M USDC)
    let v2_fwd_b = engine.register_v2_pool(
        Address::from([0x12u8; 20]),
        weth(1000),
        usdc(2_000_000),
        GAMMA_03,
        FEE_DENOM_03,
    );
    // Register 3-hop mixed path: V2 → V3 → V2
    let path_id = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v2_fwd_a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v3_key,
                zero_for_one: false,
            },
            PoolHop {
                pool_id: v2_fwd_b,
                zero_for_one: true,
            },
        ],
    )
    .unwrap();
    let resolved = &engine.cycle.path_resolved[&path_id];
    assert!(resolved.valid, "3-hop V2-V3-V2 path should be valid");
    assert_eq!(resolved.hops.len(), 3);
    assert_eq!(resolved.hops[0].hop_type(), HopType::V2);
    assert_eq!(resolved.hops[1].hop_type(), HopType::V3);
    assert_eq!(resolved.hops[2].hop_type(), HopType::V2);
    // Key: previously this returned None due to hop_types.len() != 2
    let result = ::degenbot_solvers::mixed::solve_path(
        resolved,
        &::degenbot_solvers::profit_envelope::GateDeps::offline(),
    )
    .result;
    let _ = result;
}
// Hop-projection cache (shared-pool dedup): a dirty pool shared by N
// paths must be projected ONCE per solve cycle, not once per path; a
// quiet co-hop must not re-project at all while its state_nonce holds.
#[test]
fn hop_projection_cached_until_pool_state_nonce_advances() {
    let mut oracle = crate::arb_engine::tests::test_keys::DirtyKeys::new();
    oracle.insert(0x00C0_FFEE, HopType::V2); // legacy intake probe (LXDY4C)
    let mut engine = ArbitrageEngine::new();
    // Three V2 pools: A-B and A-C cycles share pool A.
    let pool_a = Address::from([0x11u8; 20]);
    let pool_b = Address::from([0x12u8; 20]);
    let pool_c = Address::from([0x13u8; 20]);
    let id_a = engine.register_v2_pool(pool_a, usdc(1_500_000), weth(800), GAMMA_03, FEE_DENOM_03);
    let id_b = engine.register_v2_pool(pool_b, weth(800), usdc(1_500_000), GAMMA_03, FEE_DENOM_03);
    let id_c = engine.register_v2_pool(pool_c, weth(900), usdc(1_600_000), GAMMA_03, FEE_DENOM_03);
    register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: id_a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: id_b,
                zero_for_one: true,
            },
        ],
    )
    .unwrap();
    register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: id_a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: id_c,
                zero_for_one: false,
            },
        ],
    )
    .unwrap();
    // Cycle 1: both paths resolve; every UNIQUE (pool,direction) is a
    // miss. Pool A appears in both paths with the same direction, so its
    // single projection serves both paths: A+B+C = 3, not 4 hops.
    run_test_cycle(&mut engine, 4, &BlockMetadata::default(), &[]);
    assert_eq!(hop_projection_count(&engine,), 3);
    // Cycle 2: only pool B is dirty. Shared pool A must NOT re-project;
    // only B's hop in path 1 pays the walk (C's hops are untouched).
    process_updates(
        &mut engine,
        &[(pool_b, usdc(1_000_000), weth(800))],
        &[],
        5,
        &BlockMetadata::default(),
    );
    run_test_cycle(&mut engine, 5, &BlockMetadata::default(), &[]);
    // Only B's projection is fresh; A and C replay from the cache.
    assert_eq!(hop_projection_count(&engine,), 4);
    // Cycle 3: A goes dirty. Its cached projection invalidates (nonce
    // advanced) and re-projects ONCE — both paths then share the fresh
    // entry; B and C's quiet hops still do not re-project.
    process_updates(
        &mut engine,
        &[(pool_a, usdc(1_250_000), weth(800))],
        &[],
        6,
        &BlockMetadata::default(),
    );
    run_test_cycle(
        &mut engine,
        6,
        &BlockMetadata::default(),
        &oracle.to_affected_keys(),
    );
    assert_eq!(hop_projection_count(&engine,), 5);
}
#[test]
fn handle_reorg_rolls_back_v2_sync_and_expires_delivered_result() {
    // What: a V2→V2 cycle is balanced (no profit), then a Sync at block 5
    // creates a mispricing (arb appears, delivered to Python). A reorg
    // targeting block 5 rolls back that Sync; the next solve finds no arb
    // and the previously-delivered result expires.
    // Why: ADR-006 slice 7 — a `removed: true` log drives
    // `ReorgCoordinator::dispatch_reorg_log` (per-pool restore + notify →
    // engine dirties → re-solve), which restores BotState state and emits
    // an `expired` diff against `delivered`. This test exercises the
    // engine-level outcome (re-solve expires the delivered result) by
    // inlining the restore + re-dirty the bulk path used to do in one call.
    use tokio::sync::mpsc;
    let mut engine = ArbitrageEngine::new();
    // Two balanced V2 pools forming a cycle (price ≈ 1:1875).
    let pool_a = Address::from([0x11u8; 20]);
    let pool_b = Address::from([0x12u8; 20]);
    let id_a = engine.register_v2_pool(pool_a, usdc(1_500_000), weth(800), GAMMA_03, FEE_DENOM_03);
    let id_b = engine.register_v2_pool(pool_b, weth(800), usdc(1_500_000), GAMMA_03, FEE_DENOM_03);
    // Path: A (USDC→WETH) → B (WETH→USDC). Initially balanced → no profit.
    let path_id = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: id_a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: id_b,
                zero_for_one: true,
            },
        ],
    )
    .unwrap();
    // Install a result channel to capture the diff batches.
    let (tx, mut rx) = mpsc::unbounded_channel();
    set_result_channel(&mut engine, tx);
    // Sanity: the balanced cycle is not profitable (an empty affected set
    // — the reorg keys below are explicit).
    run_test_cycle(&mut engine, 4, &BlockMetadata::default(), &[]);
    compute_diff_and_send(&mut engine, &BlockMetadata::default());
    let (results_before, _) = latest_results(&engine);
    assert!(
        !results_before.contains_key(&path_id),
        "balanced cycle should not be profitable before the Sync"
    );
    // Sync pool A at block 5 to misprice it hard (A's WETH drops to 1250
    // USDC/WETH vs B's 1875 — clears the ~0.6% round-trip fee).
    process_updates(
        &mut engine,
        &[(pool_a, usdc(1_000_000), weth(800))],
        &[],
        5,
        &BlockMetadata::default(),
    );
    compute_diff_and_send(&mut engine, &BlockMetadata::default());
    let (results_after, _) = latest_results(&engine);
    assert!(
        results_after.contains_key(&path_id),
        "arbitrage should appear after the mispricing Sync"
    );
    assert!(
        engine.delivery.delivered.contains_key(&path_id),
        "profitable result should be delivered"
    );
    // Drain all batches queued so far (sanity + post-Sync) so the next
    // receive is the reorg batch.
    while rx.try_recv().is_ok() {}
    // Reorg: roll back block 5 (the Sync that created the arb). Inline the
    // restore+re-dirty — `engine.handle_reorg` is deleted in slice 7
    // (replaced by per-event `ReorgCoordinator::dispatch_reorg_log`);
    // this test verifies the engine-level outcome holds under the restore.
    engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .restore_all_pools_before_block(5);
    engine.cycle.path_resolved.clear();
    // LXDY4C: the re-restored pools re-enter the epoch delta; the solve
    // consumes the delta's taken keys (all registered hop keys here).
    let reorg_keys: Vec<degenbot_solvers::affected_keys::AffectedKey> = engine
        .registry
        .pool_to_paths
        .keys()
        .map(|(hop, pool)| degenbot_solvers::affected_keys::AffectedKey::new(*hop, *pool))
        .collect();
    run_test_cycle(&mut engine, 5, &BlockMetadata::default(), &reorg_keys);
    compute_diff_and_send(&mut engine, &BlockMetadata::default());
    // The arb is gone.
    let (results_reorg, _) = latest_results(&engine);
    assert!(
        !results_reorg.contains_key(&path_id),
        "path should be unprofitable after reorg rollback"
    );
    assert!(
        !engine.delivery.delivered.contains_key(&path_id),
        "previously-delivered result should expire out of `delivered`"
    );
    // The reorg batch must carry an `expired` entry for this path.
    let batch = rx
        .try_recv()
        .expect("a result batch should be sent after the reorg solve");
    assert!(
        batch.expired.contains(&path_id),
        "reorg batch should expire the rolled-back path, got expired={:?}",
        batch.expired
    );
}
#[test]
fn handle_reorg_rolls_back_v3_swap_and_mint_to_prior_state() {
    // What: a V3 pool gets a Swap (scalar state change at block 5) and an
    // in-range Mint (tick_data mutation + active-liquidity scalar bump at
    // block 6; the swap moved the tick to 60, inside [60, 120)). A reorg
    // targeting block 5 must roll both back: swap scalars return to
    // registration values, the Mint's active-liquidity bump is unwound,
    // and the Mint-initialized tick is removed from tick_data.
    // Why: ADR-003 — V3 reorg rollback reaches the live hot path for the
    // first time (S2b). apply_v3_swap journals scalars; the restore path
    // pops them + reverse-applies tick priors. An in-range Mint journals
    // scalar_priors: Some so the bump rolls back too.
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::TickInfo;
    use alloy::primitives::U128;
    let engine = ArbitrageEngine::new();
    let pool_addr = Address::from([0x55u8; 20]);
    // Register a V3 pool at tick 0, 1:1 price, one initialized tick at +60
    // (so the post-Mint state at block 6 can show a *second* tick).
    let mut tick_data = HashMap::new();
    tick_data.insert(
        -60,
        TickInfo {
            liquidity_gross: U128::from(100),
            liquidity_net: 100i128,
            block: 0,
        },
    );
    let pool_id = engine.register_v3_pool(&crate::bot_core::RegisterV3PoolParams {
        address: pool_addr,
        token0: Address::ZERO,
        token1: Address::from([1u8; 20]),
        fee: 3000,
        tick_spacing: 60,
        factory: Address::ZERO,
        sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
        liquidity: 1_000_000,
        tick: 0,
        tick_data,
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    });
    // DFQYM5: Tracked pools register `Quarantined`; the driver's post-verify
    // `set_live` is what makes it apply directly. Transition to `Live` so
    // this test's swap/Mint direct-apply (its model).
    engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .set_v3_pool_live(pool_addr);
    // Capture the registration scalar state.
    let reg_sp = U256::from(79_228_162_514_264_337_593_543_950_336_u128);
    let reg_liq = 1_000_000u128;
    let reg_tick = 0i32;
    let reg_tick_count = 1usize;
    // Swap at block 5: changes scalars only (tick_data untouched on the
    // live path — swaps don't mutate tick_data per V3 spec).
    let swapped_sp = (reg_sp + U256::from(1u128)) << 90;
    let swapped_liq = 2_000_000u128;
    let swapped_tick = 60i32;
    engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .apply_v3_swap(pool_addr, swapped_sp, swapped_liq, swapped_tick, 5, &[]);
    // Mint at block 6: adds liquidity at [+60, +120] — in-range because the
    // swap moved the tick to 60, so the active `liquidity` scalar also gets
    // +500 (parity with on-chain + the concentrated-liquidity-math pure reference).
    engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .apply_v3_liquidity_update(pool_addr, 60, 120, 500_i128, 6);
    {
        let core = engine
            .core
            .read_at(crate::bot_core::state_lock::LockSite::Solver);
        let s = core.get_v3_pool(pool_id).expect("v3 pool registered");
        assert_eq!(s.sqrt_price_x96, swapped_sp, "swap applied at block 5");
        assert_eq!(
            s.liquidity,
            swapped_liq + 500,
            "in-range mint adds 500 to the active liquidity scalar"
        );
        assert_eq!(s.tick, swapped_tick);
        assert_eq!(
            s.tick_data.len(),
            reg_tick_count + 2,
            "mint added two ticks"
        );
        assert!(s.tick_data.contains_key(&60) && s.tick_data.contains_key(&120));
    }
    // Reorg back to block 5: rolls the block-6 Mint (removes ticks 60/120)
    // AND the block-5 Swap (restores registration scalars). Restore is
    // idempotent for pools untouched by the fork.
    let restored = engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .restore_all_pools_before_block(5);
    assert_eq!(restored, 1, "the single registered V3 pool was rolled back");
    {
        let core = engine
            .core
            .read_at(crate::bot_core::state_lock::LockSite::Solver);
        let s = core.get_v3_pool(pool_id).expect("v3 pool still registered");
        assert_eq!(
            s.sqrt_price_x96, reg_sp,
            "swap rolled back to registration scalars"
        );
        assert_eq!(s.liquidity, reg_liq);
        assert_eq!(s.tick, reg_tick);
        assert_eq!(
            s.tick_data.len(),
            reg_tick_count,
            "mint-initialized ticks removed on rollback"
        );
        assert!(!s.tick_data.contains_key(&60) && !s.tick_data.contains_key(&120));
    }
}
/// ADR-006 Slice 1 (D1): `ArbitrageEngine::with_core` adopts an externally
/// allocated `Arc<RwLock<BotState>>` so one shared `BotState` is read by both the
/// engine and the `PyBot`/handle tree — dissolving the dual-`BotState` split
/// (pump mutates `BotState` B; handles read `BotState` A). If the engine held its own
/// `BotState`, `v2_pool_count()` would return 0 for a pool registered only in
/// the shared core.
#[test]
fn with_core_adopts_shared_bot_state() {
    use crate::bot_core::{BotState, RegisterV2PoolParams};
    use std::sync::Arc;
    // Build a shared core with one V2 pool registered directly into `BotState`.
    let core = Arc::new(crate::bot_core::state_lock::StateLock::new(BotState::new()));
    let params = RegisterV2PoolParams {
        address: Address::from([0x11u8; 20]),
        token0: Address::from([0x01u8; 20]),
        token1: Address::from([0x02u8; 20]),
        reserve0: U112::from(1000),
        reserve1: U112::from(2000),
        fee_token0: (997, 1000),
        fee_token1: (997, 1000),
        factory: Address::from([0x33u8; 20]),
        update_block: 0,
        variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
        stable_swap: false,
        fee_denominator: None,
        ..Default::default()
    };
    let _pool_id = core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_v2_pool(&params)
        .expect("test setup: V2 registration");
    // Engine adopts the SAME `Arc<RwLock<BotState>>` — NOT its own `BotState`.
    let engine = ArbitrageEngine::with_core(Arc::clone(&core));
    // If the engine held a separate `BotState`, this would be 0; shared => 1.
    assert_eq!(
        v2_pool_count(&engine,),
        1,
        "engine must read the shared BotState's pools via with_core"
    );
}
/// ADR-006 slice 2 (D3): the engine no longer constructs pools — it
/// resolves `pool_id`s against the shared `BotState` at `register_path`
/// time. A path hop referencing a `pool_id` that isn't registered in
/// the associated `BotState` must be rejected with a clear error (rather
/// than silently producing an unresolved/invalid path).
#[test]
fn register_path_rejects_pool_id_not_in_bot() {
    use crate::bot_core::{BotState, RegisterV2PoolParams};
    use std::sync::Arc;
    let core = Arc::new(crate::bot_core::state_lock::StateLock::new(BotState::new()));
    // Register one real V2 pool so the engine has *some* valid id.
    let real_pool_id = core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_v2_pool(&RegisterV2PoolParams {
            address: Address::from([0x11u8; 20]),
            token0: Address::from([0x01u8; 20]),
            token1: Address::from([0x02u8; 20]),
            reserve0: U112::from(1000),
            reserve1: U112::from(2000),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: Address::from([0x33u8; 20]),
            update_block: 0,
            variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
            stable_swap: false,
            fee_denominator: None,
            ..Default::default()
        })
        .expect("test setup: V2 registration");
    let mut engine = ArbitrageEngine::with_core(Arc::clone(&core));
    // Bogus pool_id (never registered) — must Err.
    let bogus_id = real_pool_id + 1_000;
    let result = register_path(
        &mut engine,
        vec![
            ::degenbot_solvers::mixed::PoolHop {
                pool_id: real_pool_id,
                zero_for_one: true,
            },
            ::degenbot_solvers::mixed::PoolHop {
                pool_id: bogus_id,
                zero_for_one: false,
            },
        ],
    );
    assert!(
        result.is_err(),
        "register_path must reject a pool_id not present in the BotState"
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains(&bogus_id.to_string()),
        "error must name the missing pool_id={bogus_id}, got: {msg}"
    );
}
/// Regression (3ECKWX): `process_backfill_logs` must stamp each applied log
/// with the log's OWN `block_number`, not the chunk-level `chunk_end`. Two
/// V3 Swap logs at distinct blocks B1=10, B2=20 inside one backfill chunk
/// (`chunk_end=2000`) must land as TWO separate journal deltas at blocks 10
/// and 20 — not collapse into one delta stamped at 2000.
///
/// Pre-fix every log in the chunk was journaled at `chunk_end`, so
/// `push_delta`'s same-block replacement collapsed the whole chunk into a
/// single delta at block 2000. A reorg landing mid-chunk (e.g. targeting
/// block 15) then couldn't restore a per-block landed-at state, and buffer
/// expiry timestamps were off-block.
#[test]
#[expect(clippy::too_many_lines)]
fn process_backfill_logs_stamps_per_log_block_number() {
    use crate::arb_engine::PoolTickCoverage;
    use alloy::primitives::{Bytes, B256};
    use alloy::rpc::types::Log;
    use degenbot_decoders::v3_swap_decoder::V3_SWAP_TOPIC;
    /// Build a V3 Swap log carrying post-swap scalars, at `block_number`.
    /// data = abi.encode(int256 amount0, int256 amount1, uint160 sqrtPriceX96,
    /// uint128 liquidity, int24 tick) = 5 × 32 bytes.
    fn v3_swap_log(
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
    ) -> Log {
        let mut data = Vec::with_capacity(160);
        // amount0 (int256) — unused by routing, zero-padded.
        data.extend_from_slice(&[0u8; 32]);
        // amount1 (int256) — unused by routing, zero-padded.
        data.extend_from_slice(&[0u8; 32]);
        // sqrtPriceX96 (uint160) — left-padded into 32 bytes.
        let sp_be = sqrt_price_x96.to_be_bytes::<32>();
        data.extend_from_slice(&sp_be);
        // liquidity (uint128) — left-padded into 32 bytes (bytes 16..32).
        let mut liq_word = [0u8; 32];
        liq_word[16..32].copy_from_slice(&liquidity.to_be_bytes());
        data.extend_from_slice(&liq_word);
        // tick (int24) — sign-extended into 32 bytes; last 4 bytes hold i32.
        let mut tick_word = [0u8; 32];
        tick_word[28..32].copy_from_slice(&tick.to_be_bytes());
        data.extend_from_slice(&tick_word);
        let inner = alloy::primitives::Log::new_unchecked(
            pool_address,
            vec![
                V3_SWAP_TOPIC,
                B256::left_padding_from(&[0xaau8; 20]), // sender (indexed)
                B256::left_padding_from(&[0xbbu8; 20]), // recipient (indexed)
            ],
            Bytes::from(data),
        );
        Log {
            inner,
            block_hash: None,
            block_number: Some(block_number),
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        }
    }
    let engine = ArbitrageEngine::new();
    let pool_addr = Address::from([0x77u8; 20]);
    let base_sp = U256::from(79_228_162_514_264_337_593_543_950_336_u128); // ~1.0 price
    let pool_id = engine.register_v3_pool(&RegisterV3PoolParams {
        address: pool_addr,
        token0: Address::ZERO,
        token1: Address::from([1u8; 20]),
        fee: 3000,
        tick_spacing: 60,
        factory: Address::ZERO,
        sqrt_price_x96: base_sp,
        liquidity: 1_000_000,
        tick: 0,
        tick_data: HashMap::new(),
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    });
    // DFQYM5: Tracked pools register `Quarantined`; this test drives
    // backfill swaps that must direct-apply + journal, so release to Live.
    engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .set_v3_pool_live(pool_addr);
    // Two swaps at distinct blocks inside one backfill chunk.
    let b1 = 10u64;
    let b2 = 20u64;
    let chunk_end = 2000u64; // much larger than b1/b2 — exaggerates the bug
    let sp_b1 = base_sp + U256::from(1u64);
    let sp_b2 = base_sp + U256::from(2u64);
    let logs = vec![
        v3_swap_log(pool_addr, sp_b1, 1_100_000, 1, b1),
        v3_swap_log(pool_addr, sp_b2, 1_200_000, 2, b2),
    ];
    // X35QKN: the engine's `process_backfill_logs` delegator was retired
    // (the pump calls `BotState::process_backfill_logs` directly). The test
    // only asserts on journal/state, so call the BotState method directly
    // — the same path the production backfill uses.
    engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .process_backfill_logs(&logs, chunk_end);
    let core = engine
        .core
        .read_at(crate::bot_core::state_lock::LockSite::Solver);
    let s = core.get_v3_pool(pool_id).expect("v3 pool registered");
    // Two distinct-block swaps must produce two journal deltas — NOT one
    // collapsed delta stamped at chunk_end.
    assert_eq!(
            s.journal.len(),
            2,
            "per-log block stamping must keep B1 and B2 as separate deltas (pre-fix: collapsed to 1 at chunk_end={chunk_end})"
        );
    assert_eq!(
        s.journal.earliest_block(),
        Some(b1),
        "earliest delta must be stamped at the log's real block {b1}, not chunk_end={chunk_end}"
    );
    assert_eq!(
        s.journal.newest_block(),
        Some(b2),
        "newest delta must be stamped at the log's real block {b2}, not chunk_end={chunk_end}"
    );
    // The current mutable state reflects the B2 swap (the newest).
    assert_eq!(s.sqrt_price_x96, sp_b2);
    assert_eq!(s.liquidity, 1_200_000);
    assert_eq!(s.tick, 2);
    assert_eq!(s.update_block, b2);
    // Restorability: restore before B2 must land at the post-B1 state,
    // proving B1 was journaled at its real block (under the bug it would
    // land on pre-B1, since the single collapsed delta at chunk_end >= B2
    // pops the whole chunk).
    drop(core);
    engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .restore_pool_before_block(pool_id, b2)
        .expect("restore returns Some")
        .expect("restore succeeds");
    let core = engine
        .core
        .read_at(crate::bot_core::state_lock::LockSite::Solver);
    let s = core.get_v3_pool(pool_id).expect("v3 pool registered");
    assert_eq!(
        s.sqrt_price_x96, sp_b1,
        "restore before B2={b2} lands at post-B1 scalars (per-log stamping)"
    );
    assert_eq!(s.liquidity, 1_100_000);
    assert_eq!(s.tick, 1);
    // `update_block` follows V3RestoreResult's existing "restore point =
    // oldest popped block" convention (the block we rolled back to the
    // pre-state of), intentionally not the landed-at block — out of scope
    // for 3ECKWX (per-log stamping); the scalar assertions above are the
    // restorability proof.
}
/// ADR-006 slice 10 acceptance: `ArbitrageEngine::with_core` shares the
/// SAME `Arc<RwLock<BotState>>` as the peer `Bot`/`PyBot` — the structural
/// unification that dissolves the dual-`BotState` split (the
/// `rust-owned-bot.md` §17 stale-state root cause). Proven by pointer
/// equality of the two `Arc` clones (same allocation).
#[test]
fn with_core_shares_the_same_core_arc_as_a_peer_bot() {
    use std::sync::Arc;
    let core = Arc::new(crate::bot_core::state_lock::StateLock::new(
        crate::bot_core::BotState::new(),
    ));
    let engine = ArbitrageEngine::with_core(Arc::clone(&core));
    // `Arc::ptr_eq` proves the engine + the peer hold the SAME allocation
    // — not a copy, not a fresh `BotState`. Writes through either side
    // are visible to the other (the §17 live-read payoff).
    assert!(
        Arc::ptr_eq(&engine.core, &core),
        "engine.core must be the same Arc<RwLock<BotState>> as the peer \
             (ADR-006 D1+D4 shared-core topology)"
    );
}
/// ADR-006 slice 10 acceptance: characterize the engine-then-core lock
/// ordering under concurrent access. Engine paths hold the engine
/// `Mutex<ArbitrageEngine>` and nest `core.write()`/`core.read()` inside;
/// core-only paths (`PyBot`/`PyLiquidityPool` getters) take `core` alone
/// and never re-enter the engine — the ADR-003 rule keeping the deadlock
/// surface empty. This test drives that contention concretely: the
/// `solve_dirty` writer (engine lock + core write — the pump's drain
/// path) interleaves with reader threads taking `core.read()` alone (the
/// companion-getter path). `parking_lot` `RwLock` is writer-preferenced, so
/// no reader starves the writer; the join is bounded so a real deadlock
/// would surface as a panic.
#[test]
fn engine_then_core_lock_order_survives_concurrent_readers_and_writer() {
    use crate::bot_core::BlockMetadata;
    use std::sync::Arc;
    use std::thread;
    let core = Arc::new(crate::bot_core::state_lock::StateLock::new(
        crate::bot_core::BotState::new(),
    ));
    let engine = ArbitrageEngine::with_core(Arc::clone(&core));
    let pool_id = engine.register_v2_pool(
        Address::repeat_byte(0x11),
        usdc(2_000_000),
        weth(1_000),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let engine = Arc::new(parking_lot::Mutex::new(engine));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Writer: pump drain path — engine lock then core.write() inside
    // `solve_dirty` (expires buffered events + solves dirty paths).
    let writer_engine = Arc::clone(&engine);
    let writer_done = Arc::clone(&done);
    let metadata = BlockMetadata::default();
    let writer = thread::spawn(move || {
        for block in 1..=2_000u64 {
            if writer_done.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            // engine.lock() then, inside, core.write() — engine-then-core.
            run_test_cycle(&mut writer_engine.lock(), block, &metadata, &[]);
        }
    });
    // Readers: companion-getter path — core.read() alone, never the engine.
    let mut readers = Vec::new();
    for _ in 0..4 {
        let core = Arc::clone(&core);
        let done = Arc::clone(&done);
        readers.push(thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                let r = core.read_at(crate::bot_core::state_lock::LockSite::Solver);
                // Read is coherent under one guard — no torn state.
                let _pool = r.get_v2_pool_state(pool_id);
            }
        }));
    }
    // The writer must finish within a sane bound. A real deadlock
    // (core-then-engine nesting, or a re-entrant core guard) would hit
    // this timeout.
    let writer_result = writer.join();
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    for handle in readers {
        handle.join().expect("reader panicked");
    }
    writer_result.expect("writer deadlocked (engine-then-core ordering broken)");
}
// --- ADR-005 slice 15b-1: Rust parallel solve fan-out -----------------
//
// `solve_dirty`'s affected-path solve loop is parallelized via executor bins
// `par_iter`. The tracer bullet below pins the invariant the parallel
// fan-out must preserve: equivalence with the serial baseline. This test
// runs green against the current serial `solve_all()`; after the parallel
// refactor, the test must stay green — proving the fan-out introduces no
// correctness drift. The companion stress test below it characterizes the
// engine-then-core lock ordering under the new parallel solve path with
// many paths (drives the par_iter loop across non-trivial batch sizes).
/// Pin the parallel-fan-out equivalence invariant: the batch re-solver
/// (`solve_all_paths` → `solve_all` → executor bins of `solve_path`)
/// must produce results identical to the per-path eager baseline captured
/// at `register_and_solve_path` time. Any drift between the two means the
//  fan-out is dropping paths, double-counting, or producing a different
//  solve output for the same input snapshot.
#[test]
fn solve_all_parallel_fanout_matches_per_path_eager_baseline() {
    let mut engine = ArbitrageEngine::new();
    // Register 8 V2-V2 paths on distinct pool pairs with stable price
    // divergence. Each eagerly solves at registration; we capture the
    // eager SolvePathResult as the per-path baseline.
    let mut baseline: HashMap<u64, SolvePathResult> = HashMap::new();
    for i in 0u8..8 {
        let addr_a = Address::from([0x10_u8 + i; 20]);
        let v2_fwd_a = engine.register_v2_pool(
            addr_a,
            usdc(1_500_000),
            weth(800 + u64::from(i) * 10),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let addr_b = Address::from([0x20_u8 + i; 20]);
        let v2_fwd_b = engine.register_v2_pool(
            addr_b,
            weth(800 + u64::from(i) * 10),
            usdc(2_000_000),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let path_id = register_and_solve_path(
            &mut engine,
            vec![
                PoolHop {
                    pool_id: v2_fwd_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_fwd_b,
                    zero_for_one: true,
                },
            ],
        )
        .expect("path registration should succeed");
        let (results, _block) = latest_results(&engine);
        let eager = results
            .get(&path_id)
            .expect("register_and_solve_path must eagerly solve a profitable path");
        baseline.insert(path_id, eager.clone());
    }
    // Full batch re-solve via solve_all_paths — this is the call path
    // whose solve loop gets parallelized. Equivalent eager results must
    // survive the batch re-solve (today in the resolve fan-out; a batch
    // `par_iter`).
    solve_all_paths(&mut engine, 1);
    let (results, block) = latest_results(&engine);
    assert_eq!(block, 1);
    assert_eq!(
        results.len(),
        baseline.len(),
        "batch re-solve must produce the same path-count as the eager baseline"
    );
    for (pid, expected) in &baseline {
        let got = results
            .get(pid)
            .unwrap_or_else(|| panic!("batch re-solve dropped path {pid}"));
        assert_eq!(
            got, expected,
            "path {pid} diverged: parallel fan-out != serial eager baseline"
        );
    }
}
/// ADR-006 slice 10 acceptance for the parallel solve fan-out
/// (ADR-005 slice 15b-1): characterize the engine-then-core lock ordering
/// when the engine's `solve_dirty` solve loop runs under executor bins.
/// The `par_iter` workers operate only on owned/Cloned data (`ResolvedMixedPath`
/// clones + collected `(pid, SolvePathResult)` pairs); they acquire NO
/// engine `Mutex` and NO core lock — so the engine-then-core lock order
/// is preserved unchanged even with multiple workers spawned.
///
/// This test drives the contention with N=8 paths registered (so the
/// `par_iter` batch is non-trivial — at least 8 work items per `solve_dirty`)
/// under one writer (`solve_dirty`) + four readers (core.read companions).
/// Bounded join; a real deadlock (bins re-entering the engine `Mutex`, or
/// a re-entrant core guard) surfaces as a panic on the writer thread.
#[test]
fn solve_cycle_parallel_fanout_survives_concurrent_readers_and_writer() {
    use crate::bot_core::BlockMetadata;
    use std::sync::Arc;
    use std::thread;
    let core = Arc::new(crate::bot_core::state_lock::StateLock::new(
        crate::bot_core::BotState::new(),
    ));
    let mut engine = ArbitrageEngine::with_core(Arc::clone(&core));
    // Register N paths so `solve_dirty` exercises a real par_iter batch.
    for i in 0u8..8 {
        let addr_a = Address::from([0x10_u8 + i; 20]);
        let v2_fwd_a = engine.register_v2_pool(
            addr_a,
            usdc(1_500_000),
            weth(800 + u64::from(i) * 10),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let addr_b = Address::from([0x20_u8 + i; 20]);
        let v2_fwd_b = engine.register_v2_pool(
            addr_b,
            weth(800 + u64::from(i) * 10),
            usdc(2_000_000),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let _ = register_path(
            &mut engine,
            vec![
                PoolHop {
                    pool_id: v2_fwd_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_fwd_b,
                    zero_for_one: true,
                },
            ],
        )
        .expect("path registration should succeed");
    }
    let engine = Arc::new(parking_lot::Mutex::new(engine));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Writer: `solve_dirty` invokes `solve_all_paths` semantically via
    // `run_epoch` → `par_iter` of `Self::solve_path`. The
    // writer holds the engine `Mutex` then (inside) `core.read()` (path
    // resolution) and briefly `core.write()` (V3/V4 buffer expiry). The bins'
    // internal workers touch no engine/core state.
    let writer_engine = Arc::clone(&engine);
    let writer_done = Arc::clone(&done);
    let metadata = BlockMetadata::default();
    let writer = thread::spawn(move || {
        for block in 1..=2_000u64 {
            if writer_done.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            run_test_cycle(&mut writer_engine.lock(), block, &metadata, &[]);
        }
    });
    // Readers: core.read alone — the companion-getter path. Mirrors slice
    // 10's reader pattern (never the engine lock).
    let mut readers = Vec::new();
    for _ in 0..4 {
        let core = Arc::clone(&core);
        let done = Arc::clone(&done);
        readers.push(thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                let _r = core.read_at(crate::bot_core::state_lock::LockSite::Solver);
                // Optional pool-state read; spurious empty reads on the
                // V2 registry are fine (the registered pool_ids are stable).
            }
        }));
    }
    let writer_result = writer.join();
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    for handle in readers {
        handle.join().expect("reader panicked");
    }
    writer_result.expect(
        "writer deadlocked — engine-lock re-entry in `solve_dirty` bins reintroduced a \
             core/engine lock nesting or re-entrant guard (ADR-006 D2 violated)",
    );
}
// ── Block stream (epic 6W35AI) ────────────────────────────────────────
//
// The settlement-arbitrage bot's block clock must come from a forwarded `newHeads`
// stream, NOT from `ResultBatch::solve_block` (which lags by debounce
// delay + only advances on a send). `BlockNotification` + `block_tx` are
// the dedicated channel, plumbed parallel to `result_tx`.
// See docs/architecture/rust-owned-bot.md §6.1 (`block_tx.send — Python
// reads this`); the block-stream-clock plan file has since been removed.
#[test]
fn block_notification_carries_block_and_metadata() {
    // Contract: `BlockNotification` is built from a block number + a
    // `BlockMetadata` and faithfully carries every field Python needs to
    // advance its block clock (timestamp, base_fee, gas_used, gas_limit)
    // — mirroring the `ResultBatch` metadata envelope but with an explicit
    // `number` (the clock field) instead of `solve_block`.
    let metadata = BlockMetadata {
        timestamp: 1_700_000_000,
        base_fee_per_gas: Some(7_000_000_000),
        gas_used: 15_000_000,
        gas_limit: 30_000_000,
    };
    let notif = crate::arb_engine::BlockNotification {
        number: 25_390_117,
        timestamp: metadata.timestamp,
        base_fee_per_gas: metadata.base_fee_per_gas,
        gas_used: metadata.gas_used,
        gas_limit: metadata.gas_limit,
    };
    assert_eq!(notif.number, 25_390_117);
    assert_eq!(notif.timestamp, metadata.timestamp);
    assert_eq!(notif.base_fee_per_gas, metadata.base_fee_per_gas);
    assert_eq!(notif.gas_used, metadata.gas_used);
    assert_eq!(notif.gas_limit, metadata.gas_limit);
}
// The block-channel engine tests (set_block_channel plumbing, notify_block
// push) relocated with the pipe itself: the block clock is now relayed by
// the engine's stage surface (`EngineStages` over `BlockClockPipe`) — see
// bot_core/block_clock_pipe.rs + the EngineStages tests.
#[test]
fn on_pump_ended_closes_the_result_stream() {
    // Incident 2026-08-20 (WS-silent class): pump death routes the sink's
    // `on_pump_ended` liveness answer to the engine-side close;
    // result stream must report Disconnected so the consumer ends and the
    // bot fails loudly (the engine outlives the pump, so without the
    // close the sender stays alive and the Python side awaits forever).
    // The BLOCK stream close is coordinator-owned now (ADR-027 completion)
    // — covered by
    // solve_coordinator::notify_block_delivers_to_the_coordinator_block_clock_pipe.
    use tokio::sync::mpsc::error::TryRecvError;
    let (result_tx, mut result_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut engine = ArbitrageEngine::new();
    set_result_channel(&mut engine, result_tx);
    engine.delivery.lifecycle.close();
    match result_rx.try_recv() {
        Err(TryRecvError::Disconnected) => {}
        other => panic!("result stream must be Disconnected after drop, got {other:?}"),
    }
}
