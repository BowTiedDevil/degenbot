use super::*;

#[test]
fn register_v2_and_v3_pools() {
    let mut engine = ArbitrageEngine::new();
    // Register a V2 pool
    let v2_fwd = engine.register_v2_pool(
        Address::ZERO,
        usdc(1_500_000),
        weth(800),
        GAMMA_03,
        FEE_DENOM_03,
    );
    // Register a V3 pool
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
    assert_eq!(v2_pool_count(&engine,), 1);
    assert_eq!(v3_pool_count(&engine,), 1);
    // Register a mixed V2→V3 path
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
        ],
    )
    .unwrap();
    assert_eq!(path_id, 1);
    assert_eq!(path_count(&engine,), 1);
    // Path should be resolved
    let resolved = &engine.cycle.path_resolved[&path_id];
    assert_eq!(resolved.hops.len(), 2);
    assert_eq!(resolved.hops[0].hop_type(), HopType::V2);
    assert_eq!(resolved.hops[1].hop_type(), HopType::V3);
}
#[test]
fn process_block_routes_logs_to_sub_engines() {
    let mut engine = ArbitrageEngine::new();
    // Register V2 pools
    let v2_addr = Address::ZERO;
    let v2_fwd =
        engine.register_v2_pool(v2_addr, usdc(1_500_000), weth(800), GAMMA_03, FEE_DENOM_03);
    let v2_addr1 = Address::from([1u8; 20]);
    let v2_fwd1 =
        engine.register_v2_pool(v2_addr1, weth(800), usdc(1_600_000), GAMMA_03, FEE_DENOM_03);
    // Register a pure V2 path
    register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v2_fwd,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v2_fwd1,
                zero_for_one: true,
            },
        ],
    )
    .unwrap();
    // Process with no logs — should not panic. X35QKN: process_block was
    // retired (the parallel log-routing API); an empty-log process is just
    // solve_dirty over empty dirty sets + the last_processed_block stamp.
    run_test_cycle(&mut engine, 1, &BlockMetadata::default(), &[]);
    let (results, block) = latest_results(&engine);
    assert_eq!(block, 1);
    let _ = results; // May or may not have profitable results
}
#[test]
fn mixed_path_v2_to_v3_resolves() {
    let mut engine = ArbitrageEngine::new();
    // V2 pool
    let v2_fwd = engine.register_v2_pool(
        Address::ZERO,
        usdc(1_500_000),
        weth(800),
        GAMMA_03,
        FEE_DENOM_03,
    );
    // V3 pool
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
        token0: Address::from([0u8; 20]),
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
        ],
    )
    .unwrap();
    let resolved = &engine.cycle.path_resolved[&path_id];
    assert!(resolved.hops[0].as_v2_state().is_some());
    assert!(matches!(resolved.hops[1], ResolvedHop::V3 { .. }));
}
#[test]
fn missing_v2_pool_makes_path_invalid() {
    let mut engine = ArbitrageEngine::new();
    // Only register V3 pool
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
        tick_data: HashMap::new(),
        update_block: 0,
        tick_data_block: None,
        coverage: crate::arb_engine::PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    });
    // Reference a non-existent V2 pool — ADR-006 D3: register_path
    // rejects a pool_id not present in the BotState rather than silently
    // producing an unresolved/invalid path.
    let result = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: 999, // Non-existent
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v3_key,
                zero_for_one: false,
            },
        ],
    );
    assert!(
        result.is_err(),
        "register_path must reject a pool_id not registered in the BotState"
    );
}
#[test]
fn process_updates_applies_both_types() {
    let mut engine = ArbitrageEngine::new();
    // Register V2 pools
    let v2_addr = Address::from([0x11u8; 20]);
    let v2_fwd =
        engine.register_v2_pool(v2_addr, usdc(1_500_000), weth(800), GAMMA_03, FEE_DENOM_03);
    let v2_addr1 = Address::from([0x12u8; 20]);
    let v2_fwd1 =
        engine.register_v2_pool(v2_addr1, weth(800), usdc(1_600_000), GAMMA_03, FEE_DENOM_03);
    // Register V2-only path
    register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v2_fwd,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v2_fwd1,
                zero_for_one: true,
            },
        ],
    )
    .unwrap();
    // Process updates
    process_updates(
        &mut engine,
        &[(v2_addr, usdc(1_400_000), weth(750))],
        &[],
        42,
        &BlockMetadata::default(),
    );
    let (_, block) = latest_results(&engine);
    assert_eq!(block, 42);
}
#[test]
fn quiet_pool_that_swapped_11_blocks_ago_is_still_solved() {
    // A pool that swapped once (update_block = 100) then
    // went quiet has stored reserves byte-identical to on-chain (V2 semantics:
    // unchanged until the next Sync). Solving it at block 111 is therefore
    // legitimate — it is "quiet-but-current", NOT stale. The
    // `hop_is_too_stale` pre-gate defers the whole path on any co-hop trailing
    // > MAX_SOLVE_STALENESS(10) blocks — the quiet-pool false positive
    // proved live (3,550 defers, gap 11-16, 0 genuine) while the gate
    // existed. The gate is deleted with the fix; the ADR-021 verifier is
    // the sole chain/solver-mismatch guard.
    let mut engine = ArbitrageEngine::new();
    let v2_addr_a = Address::from([0x21u8; 20]);
    let v2_fwd_a = engine.register_v2_pool(
        v2_addr_a,
        usdc(1_500_000),
        weth(800),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let v2_addr_b = Address::from([0x22u8; 20]);
    let v2_fwd_b = engine.register_v2_pool(
        v2_addr_b,
        weth(1000),
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
    .unwrap();
    // Pool A swaps at block 100 (advancing update_block to 100), then goes quiet.
    process_updates(
        &mut engine,
        &[(v2_addr_a, usdc(1_400_000), weth(750))],
        &[],
        100,
        &BlockMetadata::default(),
    );
    // Re-solve at block 111: pool A trails by 11 blocks — quiet, not stale.
    engine.cycle.run_epoch(
        &crate::arb_engine::tests::test_keys::affected_keys(
            &HashSet::from([v2_fwd_a]),
            &HashSet::new(),
            &HashSet::new(),
        ),
        111,
        &BlockMetadata::default(),
        &engine.registry,
        &mut engine.delivery,
    );
    let (results, _block) = latest_results(&engine);
    let solve_result = results.get(&path_id).expect(
        "quiet-but-current path (hop 11 blocks quiet) must be solved, not deferred \
            ",
    );
    assert!(!solve_result.optimal_input.is_zero());
    assert!(!solve_result.profit.is_zero());
}
/// The invalid-path container recheck: an invalid path re-checks ONLY when
/// a responsible pool goes dirty (and leaves Invalid as long as the pool
/// stays empty); unrelated co-hop dirt does not re-derive it. Observable
/// via `resolved_update_snapshot` (a re-derive stamps it; a skipped path
/// never does).
#[test]
fn invalid_path_skips_unrelated_dirty_but_rechecks_own_pool() {
    use crate::arb_engine::path_lifecycle::PathSolveStatus;
    let mut engine = ArbitrageEngine::new();
    // Empty V3 (Tracked coverage, no initialized ticks → NotViable).
    let empty_v3 = engine.register_v3_pool(&RegisterV3PoolParams {
        address: Address::from([0x55u8; 20]),
        token0: Address::from([0u8; 20]),
        token1: Address::from([1u8; 20]),
        fee: 3000,
        tick_spacing: 60,
        sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
        liquidity: 0,
        tick: 0,
        tick_data: HashMap::new(),
        update_block: 0,
        tick_data_block: None,
        coverage: crate::arb_engine::PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    });
    let v2 = engine.register_v2_pool(
        Address::from([0x56u8; 20]),
        usdc(1_000_000),
        weth(500),
        GAMMA_03,
        FEE_DENOM_03,
    );
    // 2-hop V3(empty) → V2. Registering succeeds (NotViable is recoverable,
    // not structural), but the path is Invalid with responsible={empty_v3}.
    let path_id = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: empty_v3,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v2,
                zero_for_one: true,
            },
        ],
    )
    .unwrap();
    match &engine.cycle.path_status[&path_id] {
        PathSolveStatus::Invalid { responsible } => {
            assert_eq!(responsible.len(), 1);
            assert!(responsible.contains(&(HopType::V3, empty_v3)));
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    // Unrelated dirty (the V2 co-hop) must NOT re-resolve the invalid path:
    // clear the resolve stamp and prove the cycle does not re-derive the
    // path (no snapshot re-insertion).
    engine.cycle.resolved_update_snapshot.clear();
    engine.cycle.run_epoch(
        &crate::arb_engine::tests::test_keys::affected_keys(
            &HashSet::from([v2]),
            &HashSet::new(),
            &HashSet::new(),
        ),
        5,
        &BlockMetadata::default(),
        &engine.registry,
        &mut engine.delivery,
    );
    assert!(
        !engine.cycle.resolved_update_snapshot.contains_key(&path_id),
        "unrelated dirty co-hop must not re-derive the invalid path"
    );
    match &engine.cycle.path_status[&path_id] {
        PathSolveStatus::Invalid { responsible } => {
            assert_eq!(responsible.len(), 1);
            assert!(responsible.contains(&(HopType::V3, empty_v3)));
        }
        other => panic!("expected still Invalid, got {other:?}"),
    }
    // Dirtying the path's OWN responsible empty pool clears the container
    // and re-checks it (still empty → Invalid again, but it WAS re-checked).
    engine.cycle.run_epoch(
        &crate::arb_engine::tests::test_keys::affected_keys(
            &HashSet::new(),
            &HashSet::from([empty_v3]),
            &HashSet::new(),
        ),
        6,
        &BlockMetadata::default(),
        &engine.registry,
        &mut engine.delivery,
    );
    assert!(
        engine.cycle.resolved_update_snapshot.contains_key(&path_id),
        "dirtying the path's own responsible pool must re-derive (re-check) it"
    );
}
#[test]
fn register_path_after_start_succeeds() {
    let mut engine = ArbitrageEngine::new();
    let v2_addr = Address::from([0x11u8; 20]);
    let v2_fwd =
        engine.register_v2_pool(v2_addr, usdc(1_500_000), weth(800), GAMMA_03, FEE_DENOM_03);
    let v2_addr2 = Address::from([0x12u8; 20]);
    let v2_fwd2 = engine.register_v2_pool(
        v2_addr2,
        weth(1000),
        usdc(2_000_000),
        GAMMA_03,
        FEE_DENOM_03,
    );
    register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v2_fwd,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v2_fwd2,
                zero_for_one: true,
            },
        ],
    )
    .unwrap();
    // Registration is always-on; this should not panic
    register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v2_fwd,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v2_fwd2,
                zero_for_one: true,
            },
        ],
    )
    .unwrap();
}
use crate::arb_engine::lifecycle::PathRegistrationError;
/// PRG-4: the engine path registry owns the registered-path
/// cap. At the cap, a NEW path registration is refused with the typed
/// benign-stop refusal (`RegistryFull`) — no Python counters involve —
/// while a DUPLICATE registration still answers with the existing id
/// (dedup is by construction; the cap only gates growth).
#[test]
fn register_path_refuses_new_paths_at_the_cap() {
    let mut engine = ArbitrageEngine::new();
    let v2_addr = Address::from([0x11u8; 20]);
    let v2_fwd =
        engine.register_v2_pool(v2_addr, usdc(1_500_000), weth(800), GAMMA_03, FEE_DENOM_03);
    let v2_addr2 = Address::from([0x12u8; 20]);
    let v2_fwd2 = engine.register_v2_pool(
        v2_addr2,
        weth(1000),
        usdc(2_000_000),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let v2_addr3 = Address::from([0x13u8; 20]);
    let _v2_fwd3 = engine.register_v2_pool(
        v2_addr3,
        weth(3000),
        usdc(3_000_000),
        GAMMA_03,
        FEE_DENOM_03,
    );
    set_path_cap(&mut engine, Some(1));
    // Two-hop path (usdc→weth then weth→usdc), per the dedicated
    // reversed-direction registration test above.
    let hops = vec![
        PoolHop {
            pool_id: v2_fwd,
            zero_for_one: true,
        },
        PoolHop {
            pool_id: v2_fwd2,
            zero_for_one: true,
        },
    ];
    let id1 = register_path(&mut engine, hops.clone()).expect("first registration fits the cap");
    // A duplicate at-cap still answers (dedup precedes the cap check).
    let dup = register_path(&mut engine, hops.clone()).expect("dedup is not capped");
    assert_eq!(dup, id1, "duplicate registration returns the existing id");
    // A NEW path at the cap (same pools, reversed direction): the typed
    // benign-stop refusal.
    let hops2 = vec![
        PoolHop {
            pool_id: v2_fwd2,
            zero_for_one: true,
        },
        PoolHop {
            pool_id: v2_fwd,
            zero_for_one: true,
        },
    ];
    let err = register_path(&mut engine, hops2.clone()).expect_err("cap reached");
    assert_eq!(
        err,
        PathRegistrationError::RegistryFull {
            cap: 1,
            registered: 1
        },
        "the refusal carries cap + registered counts"
    );
    // Raising the cap admits the queued registration.
    set_path_cap(&mut engine, Some(2));
    let id2 = register_path(&mut engine, hops2).expect("registry grown by the operator");
    assert_ne!(id1, id2);
}
/// PRG-4: dedup hits are counted engine-side (the `dup` skip telemetry
/// no longer has a Python witness — the duplicate never crosses the FFI
/// as a skip).
#[test]
fn register_path_dedup_is_counted_for_the_skip_family() {
    let mut engine = ArbitrageEngine::new();
    let v2_addr = Address::from([0x11u8; 20]);
    let v2_fwd =
        engine.register_v2_pool(v2_addr, usdc(1_500_000), weth(800), GAMMA_03, FEE_DENOM_03);
    let v2_addr2 = Address::from([0x12u8; 20]);
    let v2_fwd2 = engine.register_v2_pool(
        v2_addr2,
        weth(1000),
        usdc(2_000_000),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let hops = vec![
        PoolHop {
            pool_id: v2_fwd,
            zero_for_one: true,
        },
        PoolHop {
            pool_id: v2_fwd2,
            zero_for_one: true,
        },
    ];
    let _ = register_path(&mut engine, hops.clone()).expect("first register");
    assert_eq!(
        path_dedups(&engine,),
        0,
        "first registration is not a dedup"
    );
    let _ = register_path(&mut engine, hops).expect("dedup hit");
    assert_eq!(path_dedups(&engine,), 1, "the duplicate was counted");
}
/// FPGOYX: registering the same path (same pools + directions) twice
/// must be idempotent — return the SAME `path_id`, not a new one.
/// Unbounded registration growth (8.7k -> 107k in 25 min) caused OOM kills
/// and multi-second CPU-bound solves because every dirty-pool fan-out
/// re-solved an ever-growing duplicate set.
#[test]
fn register_path_dedup_returns_same_id() {
    let mut engine = ArbitrageEngine::new();
    let v2_addr = Address::from([0x11u8; 20]);
    let v2_fwd =
        engine.register_v2_pool(v2_addr, usdc(1_500_000), weth(800), GAMMA_03, FEE_DENOM_03);
    let v2_addr2 = Address::from([0x12u8; 20]);
    let v2_fwd2 = engine.register_v2_pool(
        v2_addr2,
        weth(1000),
        usdc(2_000_000),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let hops = vec![
        PoolHop {
            pool_id: v2_fwd,
            zero_for_one: true,
        },
        PoolHop {
            pool_id: v2_fwd2,
            zero_for_one: true,
        },
    ];
    let id1 = register_path(&mut engine, hops.clone()).expect("first register");
    let id2 = register_path(&mut engine, hops).expect("second register (dedup)");
    assert_eq!(
        id1, id2,
        "duplicate path registration must return the same path_id"
    );
    assert_eq!(
        path_count(&engine,),
        1,
        "engine must not grow on duplicate registration"
    );
}
/// FPGOYX: a path with the same pools but reversed directions is a
/// different path and must get its own id.
#[test]
fn register_path_reversed_direction_is_distinct() {
    let mut engine = ArbitrageEngine::new();
    let v2_addr = Address::from([0x11u8; 20]);
    let v2_fwd =
        engine.register_v2_pool(v2_addr, usdc(1_500_000), weth(800), GAMMA_03, FEE_DENOM_03);
    let v2_addr2 = Address::from([0x12u8; 20]);
    let v2_fwd2 = engine.register_v2_pool(
        v2_addr2,
        weth(1000),
        usdc(2_000_000),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let id_fwd = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v2_fwd,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v2_fwd2,
                zero_for_one: true,
            },
        ],
    )
    .expect("fwd register");
    let id_rev = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v2_fwd,
                zero_for_one: false,
            },
            PoolHop {
                pool_id: v2_fwd2,
                zero_for_one: false,
            },
        ],
    )
    .expect("rev register");
    assert_ne!(id_fwd, id_rev, "reversed-direction path must be distinct");
    assert_eq!(
        path_count(&engine,),
        2,
        "two distinct paths should be registered"
    );
}
#[test]
fn register_and_solve_path_eagerly_solves() {
    let mut engine = ArbitrageEngine::new();
    // Two V2 pools with price divergence
    let v2_addr_a = Address::from([0x11u8; 20]);
    let v2_fwd_a = engine.register_v2_pool(
        v2_addr_a,
        usdc(1_500_000),
        weth(800),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let v2_addr_b = Address::from([0x12u8; 20]);
    let v2_fwd_b = engine.register_v2_pool(
        v2_addr_b,
        weth(1000),
        usdc(2_000_000),
        GAMMA_03,
        FEE_DENOM_03,
    );
    // register_and_solve_path should eagerly solve and append to results
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
    .unwrap();
    // Should be tracked as pending so run_epoch can merge
    assert!(engine.cycle.pending_new_paths.contains(&path_id));
    // Results should already contain the eagerly-solved path
    let (results, _block) = latest_results(&engine);
    let solve_result = results.get(&path_id);
    assert!(
        solve_result.is_some(),
        "register_and_solve_path should eagerly solve and add to results"
    );
    let solve_result = solve_result.unwrap();
    assert!(!solve_result.optimal_input.is_zero());
    assert!(!solve_result.profit.is_zero());
}
#[test]
fn pending_new_paths_survive_rebuild() {
    let mut engine = ArbitrageEngine::new();
    // Two V2 pools with price divergence
    let v2_addr_a = Address::from([0x11u8; 20]);
    let v2_fwd_a = engine.register_v2_pool(
        v2_addr_a,
        usdc(1_500_000),
        weth(800),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let v2_addr_b = Address::from([0x12u8; 20]);
    let v2_fwd_b = engine.register_v2_pool(
        v2_addr_b,
        weth(1000),
        usdc(2_000_000),
        GAMMA_03,
        FEE_DENOM_03,
    );
    // Register path eagerly
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
    .unwrap();
    // Process an empty block (no affected pools) — run_epoch
    // should still include the pending path and not drop it
    engine.cycle.run_epoch(
        &crate::arb_engine::tests::test_keys::affected_keys(
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        ),
        1,
        &BlockMetadata::default(),
        &engine.registry,
        &mut engine.delivery,
    );
    // Pending set should be cleared
    assert!(engine.cycle.pending_new_paths.is_empty());
    // The path's result should survive the rebuild
    let (results, block) = latest_results(&engine);
    assert_eq!(block, 1);
    assert!(
        results.contains_key(&path_id),
        "pending new path result should survive run_epoch"
    );
}
#[test]
fn solve_all_paths_does_not_advance_delivered_without_channel() {
    // Contract: `solve_all_paths` is solve-only. It populates `results`
    // but must NOT advance `delivered` — `delivered`'s invariant is
    // "what Python has actually received via the result channel," and
    // with no channel set Python has received nothing. Advancing it here
    // would poison the `fresh`/`expired` computation for the first real
    // pump-driven send (any path falsely marked "delivered" gets
    // silently omitted from the next batch's `fresh` list).
    let mut engine = ArbitrageEngine::new();
    // No set_result_channel call — mirrors `solve_all_paths`'s real
    // callers (every one in tests/ builds an engine and reads
    // `latest_results()`, none sets a channel).
    let v2_addr_a = Address::from([0x11u8; 20]);
    let v2_fwd_a = engine.register_v2_pool(
        v2_addr_a,
        usdc(1_500_000),
        weth(800),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let v2_addr_b = Address::from([0x12u8; 20]);
    let v2_fwd_b = engine.register_v2_pool(
        v2_addr_b,
        weth(1000),
        usdc(2_000_000),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let path_id = register_path(
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
    solve_all_paths(&mut engine, 1);
    // Solve actually ran: results populated with a profitable path.
    let (results, block) = latest_results(&engine);
    assert_eq!(block, 1);
    let solve_result = results
        .get(&path_id)
        .expect("solve_all_paths should populate results");
    assert!(!solve_result.optimal_input.is_zero());
    assert!(!solve_result.profit.is_zero());
    // Delivered untouched — Python has not received anything.
    assert!(
        engine.delivery.delivered.is_empty(),
        "solve_all_paths must not advance `delivered` without a channel"
    );
}
#[test]
fn solve_does_not_send_result_batch_only_send_does() {
    // Contract (lock granularity, 3HYYGQ): solving
    // (`solve_all_paths` / `solve_dirty` / `process_updates` — all through
    // `run_epoch`) recomputes `results` but must NOT push
    // a batch onto the result channel. Only `send_result_batch`
    // (→ `compute_diff_and_send`) sends.
    //
    // This separation is load-bearing for lock granularity: the pump holds
    // the engine `Mutex` for the solve window only, releases it, then
    // re-acquires briefly for the channel send (an unbounded, non-blocking
    // `mpsc::UnboundedSender::send`). Python's hot loop reads results via
    // `result_rx.recv().await` — never a locked `latest_results()` — so it
    // never contends with a solve-held lock. Re-coupling the send into the
    // solve path would reintroduce exactly the "Mutex held for entire
    // solve including the (now-blocking) channel send" concern this task
    // exists to prevent. The cold-start half of this invariant is pinned
    // by `solve_all_paths_does_not_advance_delivered_without_channel`
    // (no channel set); this test pins the live half (channel set, solve
    // still must not fire it).
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut engine = ArbitrageEngine::new();
    set_result_channel(&mut engine, tx);
    // Two mispriced V2 pools → a profitable V2→V2 arb at solve time.
    let v2_addr_a = Address::from([0x11u8; 20]);
    let v2_fwd_a = engine.register_v2_pool(
        v2_addr_a,
        usdc(1_500_000),
        weth(800),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let v2_addr_b = Address::from([0x12u8; 20]);
    let v2_fwd_b = engine.register_v2_pool(
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
    // Solve with a live channel — must NOT send.
    solve_all_paths(&mut engine, 1);
    assert!(
        matches!(
            rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "solve must not push a result batch onto the channel; \
             only send_result_batch sends"
    );
    // Solving did run: a profitable result is present but undelivered.
    let (results, block) = latest_results(&engine);
    assert_eq!(block, 1);
    let solve_result = results
        .get(&path_id)
        .expect("solve should populate a profitable result");
    assert!(!solve_result.profit.is_zero());
    assert!(
        engine.delivery.delivered.is_empty(),
        "solve must not advance `delivered` (Python has received nothing)"
    );
    // Only the explicit send drives the channel.
    compute_diff_and_send(&mut engine, &BlockMetadata::default());
    let batch = rx
        .try_recv()
        .expect("send_result_batch must deliver the batch");
    assert!(
        batch.fresh.iter().any(|(id, _)| *id == path_id),
        "the solved path should arrive in the `fresh` list"
    );
}
#[test]
fn send_result_batch_advances_delivered_to_above_threshold() {
    // Contract: after a real `send_result_batch` (channel live + send
    // fires), `delivered` equals the above-threshold subset of `results`.
    //
    // Note the asymmetry this test documents: `compute_diff_and_send`
    // advances `delivered` *unconditionally* and only guards the actual
    // channel send with `if let Some(ref tx)`. That is correct WHEN a
    // channel exists and the send fires — the advance truthfully records
    // "Python now knows these." It is only a bug when the send does NOT
    // fire (the case the previous test guards). This test pins the live
    // invariant; the previous test pins the cold-start one.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut engine = ArbitrageEngine::new();
    set_result_channel(&mut engine, tx);
    // Defaults already min_profit=0, max_profit=MAX (window fully open).
    let v2_addr_a = Address::from([0x11u8; 20]);
    let v2_fwd_a = engine.register_v2_pool(
        v2_addr_a,
        usdc(1_500_000),
        weth(800),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let v2_addr_b = Address::from([0x12u8; 20]);
    let v2_fwd_b = engine.register_v2_pool(
        v2_addr_b,
        weth(1000),
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
    .unwrap();
    // Eagerly solved → path is in `results` and above-threshold.
    let (results_before, _) = latest_results(&engine);
    let solve_result = results_before
        .get(&path_id)
        .expect("eagerly solved path present");
    assert!(!solve_result.profit.is_zero());
    // A real solve has anchored `results_block` (the delivery-policy
    // solve-anchor guard defers candidates while it is still 0 — see
    // `diff_and_send_with_zero_anchor_defers_candidates_and_does_not_commit`).
    engine.cycle.cursor.set_results_block_for_test(100);
    // send_result_batch computes the diff, sends it, and advances
    // `delivered` to the above-threshold subset.
    compute_diff_and_send(&mut engine, &BlockMetadata::default());
    // Batch was actually delivered to the channel.
    let batch = rx
        .try_recv()
        .expect("send_result_batch should deliver a batch");
    assert!(
        batch.fresh.iter().any(|(id, _)| *id == path_id),
        "profitable path should appear in fresh"
    );
    // `delivered` now equals the above-threshold subset of `results`.
    assert_eq!(
        engine.delivery.delivered.len(),
        1,
        "delivered should contain exactly the one above-threshold path"
    );
    assert!(
        engine.delivery.delivered.contains_key(&path_id),
        "delivered should include the just-sent profitable path"
    );
}
/// Cold-start solved-state anchor (closes the deferral gap SAFELY):
/// backfill brings pool state to the chain tip (persisting, so capturable
/// in the next block), registration eager-solves over that live state, but
/// backfill doesn't solve and `register_and_solve_path` doesn't advance
/// `results_block`. The pump seeds `set_solve_anchor(resume_boundary)` at
/// resume — a SETTLED, in-backfill-window block — so these candidates
/// deliver immediately at a valid, verification-safe `solve_block` instead of
/// block 0 (sim panic) or a deferred deferral. It must NOT anchor to the
/// pool-state head (which a partially-applied live event can race past the
/// backfill window → premature verification failures).
#[test]
fn set_solve_anchor_seeds_cold_start_results_for_immediate_delivery() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut engine = ArbitrageEngine::new();
    set_result_channel(&mut engine, tx);
    // Pump seeds the settled resume boundary (block 500) at resume.
    engine.cycle.cursor.advance_solved(500);
    assert_eq!(
        engine.cycle.cursor.results_block(),
        500,
        "cold-start anchor seeded to settled resume block"
    );
    // Register two V2 pools + an eager-solved (profitable) path.
    let v2_addr_a = Address::from([0x11u8; 20]);
    let v2_fwd_a = engine.register_v2_pool(
        v2_addr_a,
        usdc(1_500_000),
        weth(800),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let v2_addr_b = Address::from([0x12u8; 20]);
    let v2_fwd_b = engine.register_v2_pool(
        v2_addr_b,
        weth(1000),
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
    .unwrap();
    let (results_before, _) = latest_results(&engine);
    assert!(!results_before.get(&path_id).unwrap().profit.is_zero());
    // Delivery uses the seeded settled anchor — immediate, no deferral.
    compute_diff_and_send(&mut engine, &BlockMetadata::default());
    let batch = rx
        .try_recv()
        .expect("compute_diff_and_send should deliver a batch");
    assert_eq!(
        batch.solve_block, 500,
        "cold-start candidates deliver at the settled resume anchor"
    );
    assert!(
        batch.fresh.iter().any(|(id, _)| *id == path_id),
        "capturable cold-start candidate must be delivered at the settled anchor"
    );
    assert!(engine.delivery.delivered.contains_key(&path_id));
    // Never regress a real solve anchor: a later seed must not lower it.
    engine.cycle.cursor.set_results_block_for_test(900);
    engine.cycle.cursor.advance_solved(600);
    assert_eq!(
        engine.cycle.cursor.results_block(),
        900,
        "set_solve_anchor never clobbers a real anchor"
    );
}
/// Pin (the review's Q6 strengthening): the solve-stamp path is
/// MONOTONE - a late/stale stamp can no longer regress the results
/// anchor. Both stamps below go through the REAL solve-stamp path
/// (`solve_dirty` -> `run_epoch`'s anchor re-stamp):
/// block 10 anchors first, then a stale block-5 cycle (a lagging drain
/// entry, a re-fired boundary, a detached straggler) must NOT pull the
/// anchor backwards - delivery would re-emit at a regressed
/// `solve_block`. RED before the block cursor: the stamp was an
/// unconditional `self.results_block = solve_block` write.
#[test]
fn late_solve_stamp_cannot_regress_results_anchor() {
    let mut engine = ArbitrageEngine::new();
    // First solve cycle anchors at block 10.
    run_test_cycle(&mut engine, 10, &BlockMetadata::default(), &[]);
    assert_eq!(
        engine.cycle.cursor.results_block(),
        10,
        "the solve-stamp path anchors results_block at the cycle's solve block"
    );
    // A late/stale stamp through the same path must not regress it.
    run_test_cycle(&mut engine, 5, &BlockMetadata::default(), &[]);
    assert_eq!(
        engine.cycle.cursor.results_block(),
        10,
        "a stale solve stamp must never regress the results anchor"
    );
}
#[test]
fn profit_threshold_includes_results_above_u64_max_when_unbounded() {
    // Contract: profits above `u64::MAX` (~1.84e19) are reachable for
    // 18-decimal tokens with large reserves, and the V4 int128 guard
    // permits up to 2^127-1. With the default unbounded cap
    // (`max_profit == U256::MAX`), such a result must surface in `fresh` —
    // the previous `< max_profit` filter using a u64-truncated binding
    // would silently drop everything above `u64::MAX`.
    //
    // We inject a synthetic `SolvePathResult` directly into `results`
    // (the filter reads from there; the solver path is irrelevant to
    // this bound) and drive `compute_diff_and_send`.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut engine = ArbitrageEngine::new();
    set_result_channel(&mut engine, tx);
    // Defaults: min_profit = 0, max_profit = U256::MAX (cap fully open).
    let huge_profit = U256::from(u64::MAX) + U256::from(1u64);
    let path_id = 7u64;
    engine.cycle.results.insert(
        path_id,
        SolvePathResult {
            optimal_input: U256::from(1_000u64),
            profit: huge_profit,
            hop_outputs: vec![U256::from(1u64), huge_profit],
            consumed_inputs: vec![U256::from(1_000u64)],
            state_nonces: vec![],
            solver_pool_states: vec![],
        },
    );
    // Anchor the solve at a real block: candidates are only deliverable
    // once `results_block` is non-zero (solve-anchor delivery guard — a 0
    // anchor would sim at block 0, the 0x841820 code-less panic).
    engine.cycle.cursor.set_results_block_for_test(100);
    compute_diff_and_send(&mut engine, &BlockMetadata::default());
    let batch = rx
        .try_recv()
        .expect("compute_diff_and_send should deliver a batch");
    assert!(
        batch.fresh.iter().any(|(id, _)| *id == path_id),
        "a result with profit > u64::MAX must appear in fresh when the cap is unbounded"
    );
    assert!(
        engine.delivery.delivered.contains_key(&path_id),
        "a result with profit > u64::MAX must be delivered"
    );
}
#[test]
fn profit_threshold_max_bound_is_inclusive() {
    // Contract: the max bound is inclusive (`profit <= max_profit`), so a
    // result whose profit exactly equals `max_profit` is included in
    // `fresh`. This is what makes `None` / `U256::MAX` (the only safe
    // unbounded value under the old u64 binding) reachable as an open cap.
    //
    // Same injection strategy as the above-u64-max test.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut engine = ArbitrageEngine::new();
    set_result_channel(&mut engine, tx);
    let profit = U256::from(1_000_000u64);
    set_profit_thresholds(&mut engine, U256::ZERO, profit);
    let path_id = 7u64;
    engine.cycle.results.insert(
        path_id,
        SolvePathResult {
            optimal_input: U256::from(1_000u64),
            profit,
            hop_outputs: vec![U256::from(1u64), profit],
            consumed_inputs: vec![U256::from(1_000u64)],
            state_nonces: vec![],
            solver_pool_states: vec![],
        },
    );
    // Anchor the solve at a real block (solve-anchor delivery guard — see
    // `diff_and_send_with_zero_anchor_defers_candidates_and_does_not_commit`).
    engine.cycle.cursor.set_results_block_for_test(100);
    compute_diff_and_send(&mut engine, &BlockMetadata::default());
    let batch = rx
        .try_recv()
        .expect("compute_diff_and_send should deliver a batch");
    assert!(
        batch.fresh.iter().any(|(id, _)| *id == path_id),
        "a result with profit == max_profit must be included under the inclusive (`<=`) max bound"
    );
}
#[test]
fn profit_threshold_min_bound_is_exclusive() {
    // Contract guard: the min bound stays strict (`profit > min_profit`),
    // unchanged by the max-bound inclusive fix. A result equal to
    // `min_profit` must be excluded.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut engine = ArbitrageEngine::new();
    set_result_channel(&mut engine, tx);
    let profit = U256::from(1_000_000u64);
    set_profit_thresholds(&mut engine, profit, U256::MAX);
    let path_id = 7u64;
    engine.cycle.results.insert(
        path_id,
        SolvePathResult {
            optimal_input: U256::from(1_000u64),
            profit,
            hop_outputs: vec![U256::from(1u64), profit],
            consumed_inputs: vec![U256::from(1_000u64)],
            state_nonces: vec![],
            solver_pool_states: vec![],
        },
    );
    compute_diff_and_send(&mut engine, &BlockMetadata::default());
    let batch = rx
        .try_recv()
        .expect("compute_diff_and_send should deliver a batch");
    assert!(
        batch.fresh.is_empty(),
        "a result with profit == min_profit must be excluded under the strict (`>`) min bound"
    );
    assert!(
        !engine.delivery.delivered.contains_key(&path_id),
        "a result with profit == min_profit must not be delivered"
    );
}
#[test]
fn finalize_block_threads_metadata_into_send() {
    let mut oracle = crate::arb_engine::tests::test_keys::DirtyKeys::new();
    // Contract guard for the metadata-threading fix: when the pump's
    // `finalize_if_dirty` guard fires on a dirty profitable path, the
    // emitted `ResultBatch` must carry the caller's real `BlockMetadata` —
    // not `BlockMetadata::default()` (which would make the Python consumer
    // compute `base_fee_next = next_base_fee(0,0,0) = 0` and broadcast an
    // underpriced transaction).
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut engine = ArbitrageEngine::new();
    set_result_channel(&mut engine, tx);
    // Two V2 pools with price divergence → a profitable pure-V2 path
    // (same setup as `register_and_solve_path_eagerly_solves`).
    let v2_addr_a = Address::from([0x11u8; 20]);
    let v2_fwd_a = engine.register_v2_pool(
        v2_addr_a,
        usdc(1_500_000),
        weth(800),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let v2_addr_b = Address::from([0x12u8; 20]);
    let v2_fwd_b = engine.register_v2_pool(
        v2_addr_b,
        weth(1000),
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
    .unwrap();
    // Mark a pool dirty so `has_dirty_paths()` is true (mirrors a WS log
    // having arrived). The eagerly-solved result is already in `results`.
    oracle.insert(v2_fwd_a, HopType::V2);
    // Non-default metadata — every field non-zero and distinct from default.
    let metadata = BlockMetadata {
        timestamp: 1_700_000_000,
        base_fee_per_gas: Some(1_000_000_000),
        gas_used: 5_000_000,
        gas_limit: 30_000_000,
    };
    // `last_solved_block < block(=10)` so the guard fires. The engine
    // now OWNS this bookkeeping — drive it through the engine's own
    // accessor so the test exercises the same path the pump uses.
    engine.cycle.cursor.record_logs();
    finalize_for_test(&mut engine, 10, &metadata);
    // The emitted batch must carry the passed metadata, not default.
    let batch = rx
        .try_recv()
        .expect("finalize_block should emit a result batch");
    assert_eq!(batch.solve_block, 10);
    assert_eq!(
        batch.timestamp, 1_700_000_000,
        "batch must carry the caller's timestamp"
    );
    assert_eq!(batch.base_fee_per_gas, Some(1_000_000_000));
    assert_eq!(batch.gas_used, 5_000_000);
    assert_eq!(batch.gas_limit, 30_000_000);
    // The profitable path should surface in fresh/updated.
    assert!(
        batch.fresh.iter().any(|(id, _)| *id == path_id)
            || batch.updated.iter().any(|(id, _)| *id == path_id),
        "expected the profitable path in fresh/updated"
    );
    // Guard advanced + logs flag cleared — now read from the engine
    // itself (the pump out-params were retired in).
    assert_eq!(last_solved_block(&engine,), 10);
    assert!(!has_logs_this_block(&engine,));
}
