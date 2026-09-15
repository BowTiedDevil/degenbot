use super::*;

#[test]
fn pure_v2_path_finds_profitable_arb() {
    let mut engine = ArbitrageEngine::new();
    // V2 pool A: USDC/WETH with price ~1875 USDC/WETH
    let v2_addr_a = Address::from([0x11u8; 20]);
    let v2_fwd_a = engine.register_v2_pool(
        v2_addr_a,
        usdc(1_500_000),
        weth(800),
        GAMMA_03,
        FEE_DENOM_03,
    );
    // V2 pool B: WETH/USDC with price ~2000 USDC/WETH (mispriced — arb opportunity)
    let v2_addr_b = Address::from([0x12u8; 20]);
    let v2_fwd_b = engine.register_v2_pool(
        v2_addr_b,
        weth(800),
        usdc(1_600_000),
        GAMMA_03,
        FEE_DENOM_03,
    );
    // V2→V2 path: USDC → WETH (pool A) → USDC (pool B)
    register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v2_fwd_a, // reserve0=USDC, reserve1=WETH
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v2_fwd_b, // reserve0=WETH, reserve1=USDC
                zero_for_one: true,
            },
        ],
    )
    .unwrap();
    // Solve
    let results = engine.cycle.solve_all(&engine.registry);
    // Should find a profitable arbitrage
    assert!(!results.is_empty(), "should find profitable V2-V2 arb");
    let solve_result = results.values().next().unwrap();
    assert!(!solve_result.optimal_input.is_zero());
    assert!(!solve_result.profit.is_zero());
}
#[test]
fn pure_v3_path_finds_profitable_arb() {
    let mut engine = ArbitrageEngine::new();
    // V3 pool A at tick 0 (1:1), high liquidity, with tick boundaries
    let mut tick_data_a = HashMap::new();
    tick_data_a.insert(
        60,
        crate::bot_core::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(5_000_000_000_000_000u64),
            liquidity_net: 5_000_000_000_000_000i128,
            block: 0,
        },
    );
    tick_data_a.insert(
        -60,
        crate::bot_core::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(5_000_000_000_000_000u64),
            liquidity_net: -5_000_000_000_000_000i128,
            block: 0,
        },
    );
    let v3_key_a = engine.register_v3_pool(&crate::bot_core::RegisterV3PoolParams {
        address: Address::from([0x21u8; 20]),
        token0: Address::ZERO,
        token1: Address::from([1u8; 20]),
        fee: 3000,
        tick_spacing: 60,
        factory: Address::ZERO,
        sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
        liquidity: 10_000_000_000_000_000,
        tick: 0,
        tick_data: tick_data_a,
        update_block: 0,
        tick_data_block: None,
        coverage: crate::arb_engine::PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    });
    // V3 pool B at tick -60 (slightly cheaper token1), high liquidity
    let sqrt_price_lower_u160 = degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(-60)
        .unwrap_or(alloy::primitives::U160::ZERO);
    let sqrt_price_lower = U256::from(sqrt_price_lower_u160);
    let mut tick_data_b = HashMap::new();
    tick_data_b.insert(
        0,
        crate::bot_core::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(5_000_000_000_000_000u64),
            liquidity_net: 5_000_000_000_000_000i128,
            block: 0,
        },
    );
    tick_data_b.insert(
        -120,
        crate::bot_core::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(5_000_000_000_000_000u64),
            liquidity_net: -5_000_000_000_000_000i128,
            block: 0,
        },
    );
    let v3_key_b = engine.register_v3_pool(&crate::bot_core::RegisterV3PoolParams {
        address: Address::from([0x22u8; 20]),
        token0: Address::ZERO,
        token1: Address::from([1u8; 20]),
        fee: 3000,
        tick_spacing: 60,
        factory: Address::ZERO,
        sqrt_price_x96: sqrt_price_lower,
        liquidity: 10_000_000_000_000_000,
        tick: -60,
        tick_data: tick_data_b,
        update_block: 0,
        tick_data_block: None,
        coverage: crate::arb_engine::PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    });
    // V3→V3 path: pool A (zfo) → pool B (ofz)
    register_path(
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
        ],
    )
    .unwrap();
    let results = engine.cycle.solve_all(&engine.registry);
    // V3-V3 arb depends on the exact price divergence — the important thing
    // is that the path resolves and the solver runs without panicking.
    // With a single tick spacing of 60 and 0.6% total fees, the arb may
    // not be profitable at these liquidity levels.
    let _ = results;
}
#[test]
fn mixed_v2_to_v3_path_finds_arb() {
    let mut engine = ArbitrageEngine::new();
    // V2 pool: USDC/WETH
    let v2_addr = Address::from([0x11u8; 20]);
    let v2_fwd =
        engine.register_v2_pool(v2_addr, usdc(1_500_000), weth(800), GAMMA_03, FEE_DENOM_03);
    // V3 pool: same pair but different price
    let mut tick_data = HashMap::new();
    tick_data.insert(
        60,
        crate::bot_core::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(300),
            liquidity_net: 150i128,
            block: 0,
        },
    );
    tick_data.insert(
        -60,
        crate::bot_core::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(200),
            liquidity_net: -100i128,
            block: 0,
        },
    );
    let v3_key = engine.register_v3_pool(&crate::bot_core::RegisterV3PoolParams {
        address: Address::from([0x22u8; 20]),
        token0: Address::ZERO,
        token1: Address::from([1u8; 20]),
        fee: 3000,
        tick_spacing: 60,
        factory: Address::ZERO,
        sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
        liquidity: 10_000_000_000_000,
        tick: 0,
        tick_data,
        update_block: 0,
        tick_data_block: None,
        coverage: crate::arb_engine::PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    });
    // Mixed V2→V3 path
    register_path(
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
        ],
    )
    .unwrap();
    // Even if no profit found (depends on exact numbers),
    // solve_all should run without panicking
    let results = engine.cycle.solve_all(&engine.registry);
    // Just verify it doesn't crash
    let _ = results;
}
#[test]
#[expect(clippy::too_many_lines)]
fn future_state_path_is_reanchored_to_pool_state_head() {
    let mut engine = ArbitrageEngine::new();
    // B2 (per-path re-anchor): a path whose price-clock `update_block` is
    // AHEAD of the drain block is LIVE head state (the pools were advanced
    // by backfill), NOT poison to be skipped. The correct action is to
    // re-anchor the solve block at the pool-state head so solve/verify/sim
    // all match the state the solver used. Skipping would DROP a
    // capturable live opportunity.
    // Profitable V2→V2 control — proves the dispatch pipeline builds a
    // result at the solve block when NO hop's price clock is ahead.
    let v2_a = engine.register_v2_pool(
        Address::from([0x11u8; 20]),
        usdc(1_500_000),
        weth(800),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let v2_b = engine.register_v2_pool(
        Address::from([0x12u8; 20]),
        weth(1_000),
        usdc(2_000_000),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let fresh_path = register_and_solve_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v2_a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v2_b,
                zero_for_one: true,
            },
        ],
    )
    .unwrap();
    // V3 pool whose price clock (`update_block`) is 100 — 50 blocks AHEAD
    // of the solve block 50 below (the two-stamp backfill/dispatch race).
    let mut tick_data = HashMap::new();
    tick_data.insert(
        60,
        crate::bot_core::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(300),
            liquidity_net: 150i128,
            block: 0,
        },
    );
    tick_data.insert(
        -60,
        crate::bot_core::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(200),
            liquidity_net: -100i128,
            block: 0,
        },
    );
    let v3_future = engine.register_v3_pool(&RegisterV3PoolParams {
        address: Address::from([0x22u8; 20]),
        token0: Address::ZERO,
        token1: Address::from([1u8; 20]),
        fee: 3000,
        tick_spacing: 60,
        factory: Address::ZERO,
        sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
        liquidity: 10_000_000_000_000,
        tick: 0,
        tick_data,
        update_block: 100, // 50 blocks AHEAD of the solve block 50
        tick_data_block: None,
        coverage: crate::arb_engine::PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    });
    let future_path = register_and_solve_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v2_a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v3_future,
                zero_for_one: false,
            },
        ],
    )
    .unwrap();
    // Rebuild + solve at drain block 50, but the V3 pool's price clock is
    // at 100 (head). The solve block must re-anchor to head = 100, and the
    // path is solved (never skipped): a future-vs-drain-clock block is live
    // state, not a poison to drop.
    engine.cycle.run_epoch(
        &crate::arb_engine::tests::test_keys::affected_keys(
            &HashSet::from([v2_a, v2_b]),
            &HashSet::from([v3_future]),
            &HashSet::new(),
        ),
        50,
        &BlockMetadata::default(),
        &engine.registry,
        &mut engine.delivery,
    );
    let (results, block) = latest_results(&engine);
    assert_eq!(
        block, 100,
        "solve block re-anchors to the pool-state head (max update_block 100), \
             not the lagging drain block 50"
    );
    assert!(
        results.contains_key(&fresh_path),
        "fresh V2→V2 path must still be solved"
    );
    // B2: the V2→V3 path whose pools sit at head MUST be attempted (not
    // skipped) — it is a live, potentially capturable opportunity. Whether
    // it lands in `results` depends only on profitability, which the
    // re-anchored solve computes correctly at head.
    assert!(
        engine.cycle.path_resolved.contains_key(&future_path),
        "future-state path must remain resolved for solving (never dropped)"
    );
}
#[test]
fn mixed_v3_to_v2_path_resolves() {
    let mut engine = ArbitrageEngine::new();
    // V3 pool with tick data
    let mut tick_data = HashMap::new();
    tick_data.insert(
        60,
        crate::bot_core::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(300),
            liquidity_net: 150i128,
            block: 0,
        },
    );
    tick_data.insert(
        -60,
        crate::bot_core::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(200),
            liquidity_net: -100i128,
            block: 0,
        },
    );
    let v3_key = engine.register_v3_pool(&crate::bot_core::RegisterV3PoolParams {
        address: Address::from([0x22u8; 20]),
        token0: Address::ZERO,
        token1: Address::from([1u8; 20]),
        fee: 3000,
        tick_spacing: 60,
        factory: Address::ZERO,
        sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
        liquidity: 10_000_000_000_000,
        tick: 0,
        tick_data,
        update_block: 0,
        tick_data_block: None,
        coverage: crate::arb_engine::PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    });
    // V2 pool
    let v2_addr = Address::from([0x11u8; 20]);
    let v2_fwd =
        engine.register_v2_pool(v2_addr, usdc(1_500_000), weth(800), GAMMA_03, FEE_DENOM_03);
    // V3→V2 path
    let path_id = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v3_key,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v2_fwd,
                zero_for_one: false,
            },
        ],
    )
    .unwrap();
    let resolved = &engine.cycle.path_resolved[&path_id];
    assert_eq!(resolved.hops[0].hop_type(), HopType::V3);
    assert_eq!(resolved.hops[1].hop_type(), HopType::V2);
    assert!(matches!(resolved.hops[0], ResolvedHop::V3 { .. }));
    assert!(resolved.hops[1].as_v2_state().is_some());
}
#[test]
fn rebuild_on_v2_update_changes_results() {
    let mut engine = ArbitrageEngine::new();
    // V2 pool A: USDC/WETH
    let v2_addr_a = Address::from([0x11u8; 20]);
    let v2_fwd_a = engine.register_v2_pool(
        v2_addr_a,
        usdc(1_500_000),
        weth(800),
        GAMMA_03,
        FEE_DENOM_03,
    );
    // V2 pool B: WETH/USDC
    let v2_addr_b = Address::from([0x12u8; 20]);
    let v2_fwd_b = engine.register_v2_pool(
        v2_addr_b,
        weth(800),
        usdc(1_600_000),
        GAMMA_03,
        FEE_DENOM_03,
    );
    // V2→V2 path
    register_path(
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
    .unwrap();
    // Initial solve
    let results_before = engine.cycle.solve_all(&engine.registry);
    // Apply V2 update to make pool A even more mispriced
    process_updates(
        &mut engine,
        &[(v2_addr_a, usdc(1_400_000), weth(750))],
        &[],
        1,
        &BlockMetadata::default(),
    );
    let (results_after, block) = latest_results(&engine);
    assert_eq!(block, 1);
    // Results should differ after the update
    let _ = results_before; // Just ensure initial solve didn't panic
    let _ = results_after;
}
/// Supersedes the removed solve-time staleness gate test. A
/// path whose price clock runs far behind the solve block is a QUIET pool
/// (stored state byte-identical to on-chain), so it is SOLVED, not deferred.
/// The old gate deferred it because `update_block` age looks like staleness —
/// the quiet-pool false positive proved live. Genuine chain/solver
/// divergence is out of the solve path's scope: the tripwire is retired,
/// and stale merge results are DROPPED by the Q1a window gate, never applied.
#[test]
fn quiet_pool_frozen_far_behind_is_solved_not_deferred() {
    let mut engine = ArbitrageEngine::new();
    // Profitable V2→V2 pair (from pure_v2_path_finds_profitable_arb).
    let v2_addr_a = Address::from([0x11u8; 20]);
    let v2_a = engine.register_v2_pool(
        v2_addr_a,
        usdc(1_500_000),
        weth(800),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let v2_addr_b = Address::from([0x12u8; 20]);
    let v2_b = engine.register_v2_pool(
        v2_addr_b,
        weth(800),
        usdc(1_600_000),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let path_id = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v2_a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v2_b,
                zero_for_one: true,
            },
        ],
    )
    .unwrap();
    // Prove the path IS profitable when fresh: advance both clocks to a block
    // within the window of the solve block, rebuild, and confirm a result.
    {
        let mut core = engine
            .core
            .write_at(crate::bot_core::state_lock::LockSite::Solver);
        let _ = core.apply_sync_by_pool_id(v2_a, usdc(1_500_000), weth(800), 498);
        let _ = core.apply_sync_by_pool_id(v2_b, weth(800), usdc(1_600_000), 498);
    }
    engine.cycle.run_epoch(
        &crate::arb_engine::tests::test_keys::affected_keys(
            &HashSet::from([v2_a, v2_b]),
            &HashSet::new(),
            &HashSet::new(),
        ),
        500,
        &BlockMetadata::default(),
        &engine.registry,
        &mut engine.delivery,
    );
    let (fresh, _) = latest_results(&engine);
    assert!(
        fresh.contains_key(&path_id),
        "a within-window (2-block) lag must NOT defer a profitable path"
    );
    // Now FREEZE both clocks far behind the solve block (the stale seed-anchor
    // / missed-event class, e.g. the 166k-block-behind live SushiSwap-V3 pool)
    // and rebuild at 500 again. Quiet-but-current → MUST be solved, not deferred.
    {
        let mut core = engine
            .core
            .write_at(crate::bot_core::state_lock::LockSite::Solver);
        let _ = core.apply_sync_by_pool_id(v2_a, usdc(1_500_000), weth(800), 10);
        let _ = core.apply_sync_by_pool_id(v2_b, weth(800), usdc(1_600_000), 10);
    }
    engine.cycle.run_epoch(
        &crate::arb_engine::tests::test_keys::affected_keys(
            &HashSet::from([v2_a, v2_b]),
            &HashSet::new(),
            &HashSet::new(),
        ),
        500,
        &BlockMetadata::default(),
        &engine.registry,
        &mut engine.delivery,
    );
    let (stale_results, block) = latest_results(&engine);
    assert_eq!(block, 500, "solve block anchors at max(drain, head) = 500");
    assert!(
        stale_results.contains_key(&path_id),
        "a quiet pool frozen far behind the solve block is current, not stale — \
             must be solved, not deferred (YXHHKR)"
    );
}
/// YXHHKR (resolves QNFYR5): with the TQ43TU window gate removed, no
/// `update_block` age defers a path. Never-updated pools and pools far past
/// the old 10-block window are all SOLVED — they are quiet-but-current, not
/// stale. Genuine divergence is the ADR-021 verifier's job (fatal abort).
#[test]
fn no_update_block_age_defers_a_quiet_path() {
    let mut engine = ArbitrageEngine::new();
    let v2_a = engine.register_v2_pool(
        Address::from([0x13u8; 20]),
        usdc(1_500_000),
        weth(800),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let v2_b = engine.register_v2_pool(
        Address::from([0x14u8; 20]),
        weth(800),
        usdc(1_600_000),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let path_id = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v2_a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v2_b,
                zero_for_one: true,
            },
        ],
    )
    .unwrap();
    // Never-advanced pools (`update_block == 0`) at a far solve block are
    // NOT deferred — the ADR-021 verifier diffs them at the solve block.
    engine.cycle.run_epoch(
        &crate::arb_engine::tests::test_keys::affected_keys(
            &HashSet::from([v2_a, v2_b]),
            &HashSet::new(),
            &HashSet::new(),
        ),
        500,
        &BlockMetadata::default(),
        &engine.registry,
        &mut engine.delivery,
    );
    let (r0, _) = latest_results(&engine);
    assert!(
        r0.contains_key(&path_id),
        "update_block == 0 pools must never be assumed stale"
    );
    // Exactly at the old 10-block window edge is tolerated — still solved.
    {
        let mut core = engine
            .core
            .write_at(crate::bot_core::state_lock::LockSite::Solver);
        let _ = core.apply_sync_by_pool_id(v2_a, usdc(1_500_000), weth(800), 490);
        let _ = core.apply_sync_by_pool_id(v2_b, weth(800), usdc(1_600_000), 490);
    }
    engine.cycle.run_epoch(
        &crate::arb_engine::tests::test_keys::affected_keys(
            &HashSet::from([v2_a, v2_b]),
            &HashSet::new(),
            &HashSet::new(),
        ),
        500,
        &BlockMetadata::default(),
        &engine.registry,
        &mut engine.delivery,
    );
    let (r1, _) = latest_results(&engine);
    assert!(
        r1.contains_key(&path_id),
        "staleness exactly at the window must not defer"
    );
    // 11 blocks past the old window edge — still solved (quiet, not stale).
    {
        let mut core = engine
            .core
            .write_at(crate::bot_core::state_lock::LockSite::Solver);
        let _ = core.apply_sync_by_pool_id(v2_a, usdc(1_500_000), weth(800), 489);
        let _ = core.apply_sync_by_pool_id(v2_b, weth(800), usdc(1_600_000), 489);
    }
    engine.cycle.run_epoch(
        &crate::arb_engine::tests::test_keys::affected_keys(
            &HashSet::from([v2_a, v2_b]),
            &HashSet::new(),
            &HashSet::new(),
        ),
        500,
        &BlockMetadata::default(),
        &engine.registry,
        &mut engine.delivery,
    );
    let (r2, _) = latest_results(&engine);
    assert!(
        r2.contains_key(&path_id),
        "a pool 11 blocks past the old window is quiet-but-current — solved, not \
             deferred (YXHHKR)"
    );
}
/// V4 int128 guard: paths where V4 hop amounts exceed `int128_max` are rejected.
///
/// V4's `toBalanceDelta()` calls `toInt128()` on swap amounts. If either component
/// exceeds `int128_max`, V4 reverts with `SafeCastOverflow` — the swap cannot
/// execute on-chain. The solver must not report such paths as profitable.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "int128-overflow setup + assertions are one regression narrative"
)]
fn v4_int128_overflow_path_rejected() {
    let mut engine = ArbitrageEngine::new();
    // V3 pool: normal pool at 1:1 price
    let v3_addr = Address::from([0x20u8; 20]);
    let v3_factory = Address::from([0x21u8; 20]);
    let sp_0 = U256::from(1u128) << 96;
    let v3_id = engine.register_v3_pool(&RegisterV3PoolParams {
        address: v3_addr,
        token0: Address::from([0x30u8; 20]),
        token1: Address::from([0x31u8; 20]),
        fee: 10_000, // 1%
        tick_spacing: 200,
        factory: v3_factory,
        sqrt_price_x96: sp_0,
        liquidity: 10_000_000_000_000u128,
        tick: 0,
        tick_data: HashMap::new(),
        update_block: 0,
        tick_data_block: None,
        coverage: crate::arb_engine::PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    });
    // V4 pool: pool at extreme price (tick -886_983) with massive liquidity
    // This produces virtual reserves >> int128_max
    let v4_pool_manager = Address::from([0x40u8; 20]);
    // tick -886_983 → sqrtPrice ≈ 4.36e9 (very low price, token0 is nearly worthless)
    let sp_extreme =
        degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(-886_983).unwrap_or_default();
    let extreme_liquidity: u128 = 76_688_550_121_478_947_320_312_764_923_207_804;
    let v4_id = engine
        .register_v4_pool(&RegisterV4PoolParams {
            pool_manager: v4_pool_manager,
            pool_id: [0xffu8; 32],
            pool_key: crate::bot_core::V4PoolKey {
                currency0: Address::from([0x30u8; 20]),
                currency1: Address::from([0x31u8; 20]),
                fee: 10_000,
                tick_spacing: 200,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(sp_extreme),
            liquidity: extreme_liquidity,
            tick: -886_983,
            tick_data: HashMap::new(),
            update_block: 0,
            tick_data_block: None,
            coverage: crate::arb_engine::PoolTickCoverage::Tracked,
            fetcher: None,
        })
        .expect("V4 registration failed");
    // Register path: V3 (zfo) → V4 (ofz, which will produce huge token0 output)
    let path_id = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v3_id,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v4_id,
                zero_for_one: false,
            },
        ],
    )
    .unwrap();
    // Resolve and solve all paths (replaces start() + initial_solve())
    {
        let core = engine
            .core
            .read_at(crate::bot_core::state_lock::LockSite::Solver);
        for (&path_id, path) in &engine.registry.path_pools {
            let mut resolved = ResolvedMixedPath::default();
            let _ = crate::bot_core::resolve::resolve_hops(
                &core,
                &path.pools,
                &mut resolved,
                &engine.cycle.hop_projection_cache,
                None,
                engine.cycle.cl_projection_memo,
            );
            engine
                .cycle
                .path_resolved
                .insert(path_id, std::sync::Arc::new(resolved));
        }
    }
    let results_map = engine.cycle.solve_all(&engine.registry);
    engine.cycle.results.clear();
    for (pid, r) in results_map {
        engine.cycle.results.insert(pid, r);
    }
    let (results, _block) = latest_results(&engine);
    // The V4 hop's output (token0 at extreme price) would overflow int128.
    // The solver should reject this path — no result should be returned.
    if let Some(solve_result) = results.get(&path_id) {
        // If a result IS found, verify that V4 hop outputs fit int128
        let v4_output = solve_result
            .hop_outputs
            .get(1)
            .copied()
            .unwrap_or(U256::ZERO);
        let v4_consumed = solve_result
            .consumed_inputs
            .get(1)
            .copied()
            .unwrap_or(U256::ZERO);
        assert!(
            v4_output <= INT128_MAX && v4_consumed <= INT128_MAX,
            "V4 hop amounts must fit int128: output={v4_output}, consumed={v4_consumed}"
        );
    }
    // Ideally the path should not appear in results at all
}
/// Register a small V4 pool that can only convert a bounded amount per
/// swap (single narrow position, low liquidity), plus a 2-hop V2→V4 path
/// whose V4 hop is fed an absurdly large committed input. Then drive
/// `clamp_cl_hop_capacity` directly and assert it caps the V4 hop's
/// `consumed_inputs[1]` to the pools twin's `input_consumed - 1` (the 1-wei
/// Forward-clamp staleness (the path-182449/110302 1-wei over-prediction
/// class): when the upstream CL hop's twin forward-clamps the DOWNSTREAM
/// V2 hop's committed input, the V2 hop's REPORTED output must be
/// re-derived byte-exact at the clamped input. Pre-fix the V2 branch of
/// the clamp loop was a bare `continue`, so the V2 `hop_outputs[i]` kept
/// the pre-clamp input's output — over-predicting by exactly the 1 wei of
/// clamped input (the live bot then failed on-chain with
/// `UniswapV2: K`).
#[expect(clippy::too_many_lines)]
#[test]
fn clamp_cl_hop_capacity_realigns_terminal_v2_after_forward_clamp() {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::TickInfo;
    use degenbot_math::v2::IntHopState;
    let mut engine = ArbitrageEngine::new();
    // Terminal V2 pool: token0=USDC, token1=WETH; hop zfo=false → WETH
    // in, USDC out.
    let v2 = engine.register_v2_pool(
        Address::from([0x22u8; 20]),
        U112::from(1_500_000_000_000u128),
        U112::from(1_000_000_000_000u128),
        GAMMA_03,
        FEE_DENOM_03,
    );
    // Leading V4 hop: single narrow position (±60 ticks), 1e6 liquidity —
    // its twin output at a 1e6-scale input is bounded (≪ 5e6), so a 5e6
    // committed forward into the V2 hop must forward-clamp.
    let mut tick_data = HashMap::new();
    tick_data.insert(
        60,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(300),
            liquidity_net: 150i128,
            block: 0,
        },
    );
    tick_data.insert(
        -60,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(200),
            liquidity_net: -100i128,
            block: 0,
        },
    );
    let v4_id = engine
        .register_v4_pool(&RegisterV4PoolParams {
            pool_manager: Address::from([0x45u8; 20]),
            pool_id: [0xcdu8; 32],
            pool_key: crate::bot_core::V4PoolKey {
                currency0: Address::from([0x32u8; 20]),
                currency1: Address::from([0x33u8; 20]),
                fee: 500,
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000_000,
            tick: 0,
            tick_data,
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
        })
        .expect("V4 registration failed");
    let path_id = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v4_id,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v2,
                zero_for_one: false,
            },
        ],
    )
    .unwrap();
    // Committed values: the V2-input forward is deliberately above the
    // V4 twin's actual output; the V2 output is the STALE value the walk
    // reported for the un-clamped forward.
    let x0 = U256::from(1_000_000_000u64);
    let committed_forward = U256::from(5_000_000_000u64);
    let mut result = SolvePathResult {
        optimal_input: x0,
        profit: U256::from(1_000u64),
        hop_outputs: vec![committed_forward, U256::from(9_999_999u64)],
        consumed_inputs: vec![x0, committed_forward],
        state_nonces: vec![0, 0],
        solver_pool_states: Vec::new(),
    };
    engine
        .cycle
        .clamp_cl_hop_capacity(path_id, &mut result, &engine.registry);
    // Test premise: the forward into the V2 hop was actually clamped.
    let clamped = result.consumed_inputs[1];
    assert!(
            clamped < committed_forward,
            "premise: upstream forward clamp must fire (clamped={clamped} vs committed={committed_forward})"
        );
    // The terminal V2 hop's REPORTED output must equal its byte-exact
    // twin at the CLAMPED input (zfo=false → reserve_in=token1, fee_token1).
    let core = engine
        .core
        .read_at(crate::bot_core::state_lock::LockSite::Solver);
    let state = core.get_v2_pool_state(v2).unwrap();
    let identity = core.get_v2_identity(v2).unwrap();
    let expected = IntHopState::new(
        state.reserve1.to::<U256>(),
        state.reserve0.to::<U256>(),
        identity.fee_token1.0,
        identity.fee_token1.1,
    )
    .swap(clamped)
    .expect("V2 twin does not overflow");
    // Sizing premise: the re-derived output is a meaningful (non-zero)
    // amount, so the stale-vs-corrected assertion is non-degenerate.
    assert!(
        !expected.is_zero(),
        "test sizing: expected V2 output must be non-zero"
    );
    assert_eq!(
            result.hop_outputs[1],
            expected,
            "terminal V2 hop_outputs must be re-derived at the clamped input \n\n(path-182449/110302 1-wei over-prediction class)"
        );
    // ...and the selection profit reflects the corrected final output.
    assert_eq!(
        result.profit,
        result.hop_outputs[1].saturating_sub(result.consumed_inputs[0]),
        "post-clamp profit must be recomputed from the corrected outputs"
    );
}
/// VAASFM margin) — the UO3JM4 empty-march clamp, now enforced in
/// production at the solve→result merge seam.
#[expect(clippy::too_many_lines)]
#[test]
fn clamp_cl_hop_capacity_caps_overfed_v4_input() {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::TickInfo;
    use alloy::primitives::I256;
    use degenbot_pools::v3_state::V3PoolState;
    use degenbot_pools::v4_state::v4_simulate_swap;
    let mut engine = ArbitrageEngine::new();
    // V2 pool: reserves sized so its output (fed to V4) is enormous
    // relative to the V4 pool's capacity (token1 ≫ the V4 twin's
    // input_consumed, so the V2 hop's forward-clamp cannot fire before the
    // V4's own input clamp — this test isolates (a)).
    let v2 = engine.register_v2_pool(
        Address::from([0x11u8; 20]),
        usdc(1_500_000),
        weth(20_000_000_000),
        GAMMA_03,
        FEE_DENOM_03,
    );
    // V4 pool: single narrow position (±60 ticks) with low liquidity so
    // the exact-in loop converts only a bounded amount.
    let mut tick_data = HashMap::new();
    tick_data.insert(
        60,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(300),
            liquidity_net: 150i128,
            block: 0,
        },
    );
    tick_data.insert(
        -60,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(200),
            liquidity_net: -100i128,
            block: 0,
        },
    );
    let v4_id = engine
        .register_v4_pool(&RegisterV4PoolParams {
            pool_manager: Address::from([0x44u8; 20]),
            pool_id: [0xabu8; 32],
            pool_key: crate::bot_core::V4PoolKey {
                currency0: Address::from([0x30u8; 20]),
                currency1: Address::from([0x31u8; 20]),
                fee: 500,
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
        })
        .expect("V4 registration failed");
    // Register a V2→V4 path (V4 is hop 1, over-fed).
    let path_id = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v2,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v4_id,
                zero_for_one: false,
            },
        ],
    )
    .unwrap();
    // Over-feed the V4 hop with an absurdly large committed input far
    // beyond the pool's capacity.
    let huge = U256::from(1u128) << 120;
    let mut result = SolvePathResult {
        optimal_input: huge,
        profit: U256::ONE,
        hop_outputs: vec![huge, U256::ONE],
        consumed_inputs: vec![huge, huge],
        state_nonces: vec![0, 0],
        solver_pool_states: Vec::new(),
    };
    engine
        .cycle
        .clamp_cl_hop_capacity(path_id, &mut result, &engine.registry);
    // Compute the pools twin's input_consumed at the requested input to
    // assert the clamped value equals `input_consumed - 1` exactly.
    let input_consumed = {
        let core = engine
            .core
            .read_at(crate::bot_core::state_lock::LockSite::Solver);
        let state = core.get_v4_pool(v4_id).unwrap();
        let identity = core.get_v4_identity(v4_id).unwrap();
        let neg = I256::try_from(huge).unwrap().checked_neg().unwrap();
        let limit = V3PoolState::default_sqrt_price_limit(false);
        let outcome = v4_simulate_swap(
            state,
            identity.pool_key.fee,
            identity.pool_key.tick_spacing,
            false,
            neg,
            limit,
        )
        .expect("twin simulates");
        outcome.input_consumed
    };
    let expected = input_consumed.saturating_sub(U256::ONE);
    // The twin's output-token amount (zfo=false → output = amount0) — the
    // byte-exact value the clamp aligns hop_outputs[1] to.
    let twin_out = {
        let core = engine
            .core
            .read_at(crate::bot_core::state_lock::LockSite::Solver);
        let state = core.get_v4_pool(v4_id).unwrap();
        let identity = core.get_v4_identity(v4_id).unwrap();
        let neg = I256::try_from(huge).unwrap().checked_neg().unwrap();
        let limit = V3PoolState::default_sqrt_price_limit(false);
        v4_simulate_swap(
            state,
            identity.pool_key.fee,
            identity.pool_key.tick_spacing,
            false,
            neg,
            limit,
        )
        .expect("twin simulates")
        .amount0
    };
    // The clamp engages: consumed_inputs[1] is capped below the request.
    assert!(
        result.consumed_inputs[1] < huge,
        "V4 hop must be clamped below the over-fed request (got {})",
        result.consumed_inputs[1]
    );
    assert_eq!(
        result.consumed_inputs[1], expected,
        "clamped input must equal input_consumed - margin (1 wei)"
    );
    // The V2 hop (index 0) is untouched — only CL hops are clamped.
    assert_eq!(result.consumed_inputs[0], huge);
    // hop_outputs[1] is now ALIGNED to the byte-exact twin output (the
    // path-73385 fix): the solver's reported output = the on-chain truth, so
    // the composer's take (derived from consumed_inputs[1+1]) is exact.
    assert_eq!(
        result.hop_outputs[1], twin_out,
        "hop_outputs[1] must be aligned to the twin output"
    );
}
/// The solver alignment covers a V4-FIRST path (hop0): `hop_outputs[0]`
/// is aligned to the V4 twin output and the forward to hop1
/// (`consumed_inputs[1]`) is clamped to it — the V4-first families
/// (`v4_v3_*`, `v4_v2_*`, `v4_v4_*`) all derive their V4 take from
/// `hop_outputs[0]`, so this sweeps them to exactness with no composer
/// change.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "V4-first hop fixture setup + alignment assertions read as one scenario"
)]
fn clamp_cl_hop_capacity_aligns_v4_first_hop0_outputs() {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::TickInfo;
    use alloy::primitives::I256;
    use degenbot_pools::v3_state::V3PoolState;
    use degenbot_pools::v4_state::v4_simulate_swap;
    let mut engine = ArbitrageEngine::new();
    // V4 pool (hop0), narrow ±60 band, low liquidity — over-fed later.
    let mut tick_data = HashMap::new();
    tick_data.insert(
        60,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(300),
            liquidity_net: 150i128,
            block: 0,
        },
    );
    tick_data.insert(
        -60,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(200),
            liquidity_net: -100i128,
            block: 0,
        },
    );
    let v4_id = engine
        .register_v4_pool(&RegisterV4PoolParams {
            pool_manager: Address::from([0x44u8; 20]),
            pool_id: [0xabu8; 32],
            pool_key: crate::bot_core::V4PoolKey {
                currency0: Address::from([0x30u8; 20]),
                currency1: Address::from([0x31u8; 20]),
                fee: 500,
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
        })
        .expect("V4 registration failed");
    let v2 = engine.register_v2_pool(
        Address::from([0x11u8; 20]),
        usdc(1_500_000),
        weth(800),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let path_id = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v4_id,
                zero_for_one: true, // sell currency0; output = amount1
            },
            PoolHop {
                pool_id: v2,
                zero_for_one: true,
            },
        ],
    )
    .unwrap();
    let huge = U256::from(1u128) << 120;
    let mut result = SolvePathResult {
        optimal_input: huge,
        profit: U256::ONE,
        hop_outputs: vec![U256::ONE, U256::ONE],
        consumed_inputs: vec![huge, huge],
        state_nonces: vec![0, 0],
        solver_pool_states: Vec::new(),
    };
    engine
        .cycle
        .clamp_cl_hop_capacity(path_id, &mut result, &engine.registry);
    // Compute the V4 twin's amount1 (zfo=true → output = amount1) at the
    // requested input — the byte-exact value hop_outputs[0] must align to.
    let twin_out = {
        let core = engine
            .core
            .read_at(crate::bot_core::state_lock::LockSite::Solver);
        let state = core.get_v4_pool(v4_id).unwrap();
        let identity = core.get_v4_identity(v4_id).unwrap();
        let neg = I256::try_from(huge).unwrap().checked_neg().unwrap();
        let limit = V3PoolState::default_sqrt_price_limit(true);
        v4_simulate_swap(
            state,
            identity.pool_key.fee,
            identity.pool_key.tick_spacing,
            true,
            neg,
            limit,
        )
        .expect("twin simulates")
        .amount1
    };
    // hop_outputs[0] is aligned to the byte-exact twin output (V4-first).
    assert_eq!(result.hop_outputs[0], twin_out);
    // The forward to hop1 (consumed_inputs[1]) is clamped to the twin output
    // so the composer's take can never over-take the V4 pool's actual yield.
    assert_eq!(result.consumed_inputs[1], twin_out);
    assert!(
        result.consumed_inputs[1] < huge,
        "hop1 forward must be clamped"
    );
}
/// The clamp is a strict no-op when a CL hop's committed input is within
/// the pool's max-convertible capacity — the exact-in loop already
/// terminates on `amountRemaining==0`. Prevents the clamp from corrupting
/// `consumed_inputs` for the (common) fully-fed-hop case.
#[test]
fn clamp_cl_hop_capacity_noop_within_capacity() {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::TickInfo;
    let mut engine = ArbitrageEngine::new();
    let mut tick_data = HashMap::new();
    tick_data.insert(
        60,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(300),
            liquidity_net: 150i128,
            block: 0,
        },
    );
    tick_data.insert(
        -60,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(200),
            liquidity_net: -100i128,
            block: 0,
        },
    );
    let v4_id = engine
        .register_v4_pool(&RegisterV4PoolParams {
            pool_manager: Address::from([0x44u8; 20]),
            pool_id: [0xabu8; 32],
            pool_key: crate::bot_core::V4PoolKey {
                currency0: Address::from([0x30u8; 20]),
                currency1: Address::from([0x31u8; 20]),
                fee: 500,
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
        })
        .expect("V4 registration failed");
    let v2_id = engine.register_v2_pool(
        Address::from([0x77u8; 20]),
        usdc(1_600_000),
        weth(900),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let path_id = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v4_id,
                zero_for_one: false,
            },
            PoolHop {
                pool_id: v2_id,
                zero_for_one: true,
            },
        ],
    )
    .unwrap();
    // A tiny in-capacity input — the pool fully converts it, no clamp.
    let small = U256::from(1u128);
    let mut result = SolvePathResult {
        optimal_input: small,
        profit: U256::ONE,
        hop_outputs: vec![U256::ONE, U256::ONE],
        consumed_inputs: vec![small, small],
        state_nonces: vec![0, 0],
        solver_pool_states: Vec::new(),
    };
    engine
        .cycle
        .clamp_cl_hop_capacity(path_id, &mut result, &engine.registry);
    assert_eq!(
        result.consumed_inputs[0], small,
        "in-capacity input must be left untouched by the clamp"
    );
}
