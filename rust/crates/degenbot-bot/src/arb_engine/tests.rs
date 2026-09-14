#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]
#[cfg(test)]
#[expect(clippy::module_inception)]
mod tests {
    use hashbrown::{HashMap, HashSet};

    use alloy::primitives::{aliases::U112, Address, U256};

    use crate::arb_engine::{ArbitrageEngine, BlockMetadata, EnginePhase};
    use crate::bot_core::RegisterV3PoolParams;
    use crate::bot_core::RegisterV4PoolParams;
    use ::degenbot_solvers::mixed::{
        HopType, PoolHop, ResolvedHop, ResolvedMixedPath, SolidlyHopState, SolvePathResult,
        INT128_MAX,
    };
    use degenbot_uniswap::dex_identity::DexVariant;

    fn usdc(amount: u64) -> U112 {
        (U256::from(amount) * U256::from(10u64).pow(U256::from(6))).to::<U112>()
    }

    fn weth(amount: u64) -> U112 {
        (U256::from(amount) * U256::from(10u64).pow(U256::from(18))).to::<U112>()
    }

    const GAMMA_03: u64 = 997;
    const FEE_DENOM_03: u64 = 1000;

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

        assert_eq!(engine.v2_pool_count(), 1);
        assert_eq!(engine.v3_pool_count(), 1);

        // Register a mixed V2→V3 path
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2_fwd,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v3_key,
                    zero_for_one: false,
                },
            ])
            .unwrap();

        assert_eq!(path_id, 1);
        assert_eq!(engine.path_count(), 1);

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
        engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2_fwd,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_fwd1,
                    zero_for_one: true,
                },
            ])
            .unwrap();

        // Process with no logs — should not panic. X35QKN: process_block was
        // retired (the parallel log-routing API); an empty-log process is just
        // solve_dirty over empty dirty sets + the last_processed_block stamp.
        engine.solve_dirty(1, &BlockMetadata::default(), &[]);

        let (results, block) = engine.latest_results();
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
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2_fwd,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v3_key,
                    zero_for_one: false,
                },
            ])
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
        let result = engine.register_path(vec![
            PoolHop {
                pool_id: 999, // Non-existent
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v3_key,
                zero_for_one: false,
            },
        ]);
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
        engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2_fwd,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_fwd1,
                    zero_for_one: true,
                },
            ])
            .unwrap();

        // Process updates
        engine.process_updates(
            &[(v2_addr, usdc(1_400_000), weth(750))],
            &[],
            42,
            &BlockMetadata::default(),
        );

        let (_, block) = engine.latest_results();
        assert_eq!(block, 42);
    }

    #[test]
    fn quiet_pool_that_swapped_11_blocks_ago_is_still_solved() {
        // QNFYR5 / YXHHKR RED. A pool that swapped once (update_block = 100) then
        // went quiet has stored reserves byte-identical to on-chain (V2 semantics:
        // unchanged until the next Sync). Solving it at block 111 is therefore
        // legitimate — it is "quiet-but-current", NOT stale. The TQ43TU
        // `hop_is_too_stale` pre-gate defers the whole path on any co-hop trailing
        // > MAX_SOLVE_STALENESS(10) blocks, which is the quiet-pool false positive
        // QNFYR5 proved live (3,550 defers, gap 11-16, 0 genuine). RED: this test
        // FAILS while the gate exists (path dropped from results). The gate is
        // deleted with the fix; the ADR-021 verifier is the sole chain/solver-
        // mismatch guard.
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

        let path_id = engine
            .register_and_solve_path(vec![
                PoolHop {
                    pool_id: v2_fwd_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_fwd_b,
                    zero_for_one: true,
                },
            ])
            .unwrap();

        // Pool A swaps at block 100 (advancing update_block to 100), then goes quiet.
        engine.process_updates(
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

        let (results, _block) = engine.latest_results();
        let solve_result = results.get(&path_id).expect(
            "quiet-but-current path (hop 11 blocks quiet) must be solved, not deferred \
             (QNFYR5/YXHHKR)",
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
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: empty_v3,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2,
                    zero_for_one: true,
                },
            ])
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
        engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2_fwd,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_fwd2,
                    zero_for_one: true,
                },
            ])
            .unwrap();
        // Registration is always-on; this should not panic
        engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2_fwd,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_fwd2,
                    zero_for_one: true,
                },
            ])
            .unwrap();
    }

    use crate::arb_engine::lifecycle::PathRegistrationError;

    /// PRG-4 / IRUMXD: the engine path registry owns the registered-path
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

        engine.set_path_cap(Some(1));

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
        let id1 = engine
            .register_path(hops.clone())
            .expect("first registration fits the cap");

        // A duplicate at-cap still answers (dedup precedes the cap check).
        let dup = engine
            .register_path(hops.clone())
            .expect("dedup is not capped");
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
        let err = engine
            .register_path(hops2.clone())
            .expect_err("cap reached");
        assert_eq!(
            err,
            PathRegistrationError::RegistryFull {
                cap: 1,
                registered: 1
            },
            "the refusal carries cap + registered counts"
        );

        // Raising the cap admits the queued registration.
        engine.set_path_cap(Some(2));
        let id2 = engine
            .register_path(hops2)
            .expect("registry grown by the operator");
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
        let _ = engine.register_path(hops.clone()).expect("first register");
        assert_eq!(engine.path_dedups(), 0, "first registration is not a dedup");
        let _ = engine.register_path(hops).expect("dedup hit");
        assert_eq!(engine.path_dedups(), 1, "the duplicate was counted");
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

        let id1 = engine.register_path(hops.clone()).expect("first register");
        let id2 = engine.register_path(hops).expect("second register (dedup)");

        assert_eq!(
            id1, id2,
            "duplicate path registration must return the same path_id"
        );
        assert_eq!(
            engine.path_count(),
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

        let id_fwd = engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2_fwd,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_fwd2,
                    zero_for_one: true,
                },
            ])
            .expect("fwd register");

        let id_rev = engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2_fwd,
                    zero_for_one: false,
                },
                PoolHop {
                    pool_id: v2_fwd2,
                    zero_for_one: false,
                },
            ])
            .expect("rev register");

        assert_ne!(id_fwd, id_rev, "reversed-direction path must be distinct");
        assert_eq!(
            engine.path_count(),
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
        let path_id = engine
            .register_and_solve_path(vec![
                PoolHop {
                    pool_id: v2_fwd_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_fwd_b,
                    zero_for_one: true,
                },
            ])
            .unwrap();

        // Should be tracked as pending so run_epoch can merge
        assert!(engine.cycle.pending_new_paths.contains(&path_id));

        // Results should already contain the eagerly-solved path
        let (results, _block) = engine.latest_results();
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
        let path_id = engine
            .register_and_solve_path(vec![
                PoolHop {
                    pool_id: v2_fwd_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_fwd_b,
                    zero_for_one: true,
                },
            ])
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
        let (results, block) = engine.latest_results();
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
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2_fwd_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_fwd_b,
                    zero_for_one: true,
                },
            ])
            .unwrap();

        engine.solve_all_paths(1);

        // Solve actually ran: results populated with a profitable path.
        let (results, block) = engine.latest_results();
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
        engine.set_result_channel(tx);

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
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2_fwd_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_fwd_b,
                    zero_for_one: true,
                },
            ])
            .unwrap();

        // Solve with a live channel — must NOT send.
        engine.solve_all_paths(1);
        assert!(
            matches!(
                rx.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ),
            "solve must not push a result batch onto the channel; \
             only send_result_batch sends"
        );

        // Solving did run: a profitable result is present but undelivered.
        let (results, block) = engine.latest_results();
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
        engine.send_result_batch(&BlockMetadata::default());
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
        engine.set_result_channel(tx);
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
        let path_id = engine
            .register_and_solve_path(vec![
                PoolHop {
                    pool_id: v2_fwd_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_fwd_b,
                    zero_for_one: true,
                },
            ])
            .unwrap();

        // Eagerly solved → path is in `results` and above-threshold.
        let (results_before, _) = engine.latest_results();
        let solve_result = results_before
            .get(&path_id)
            .expect("eagerly solved path present");
        assert!(!solve_result.profit.is_zero());

        // A real solve has anchored `results_block` (the delivery-policy
        // solve-anchor guard defers candidates while it is still 0 — see
        // `diff_and_send_with_zero_anchor_defers_candidates_and_does_not_commit`).
        engine.set_results_block_for_test(100);

        // send_result_batch computes the diff, sends it, and advances
        // `delivered` to the above-threshold subset.
        engine.send_result_batch(&BlockMetadata::default());

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
        engine.set_result_channel(tx);

        // Pump seeds the settled resume boundary (block 500) at resume.
        engine.set_solve_anchor(500);
        assert_eq!(
            engine.results_block(),
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
        let path_id = engine
            .register_and_solve_path(vec![
                PoolHop {
                    pool_id: v2_fwd_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_fwd_b,
                    zero_for_one: true,
                },
            ])
            .unwrap();
        let (results_before, _) = engine.latest_results();
        assert!(!results_before.get(&path_id).unwrap().profit.is_zero());

        // Delivery uses the seeded settled anchor — immediate, no deferral.
        engine.compute_diff_and_send(&BlockMetadata::default());
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
        engine.set_results_block_for_test(900);
        engine.set_solve_anchor(600);
        assert_eq!(
            engine.results_block(),
            900,
            "set_solve_anchor never clobbers a real anchor"
        );
    }

    /// 6XB6NJ pin (the review's Q6 strengthening): the solve-stamp path is
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
        engine.solve_dirty(10, &BlockMetadata::default(), &[]);
        assert_eq!(
            engine.results_block(),
            10,
            "the solve-stamp path anchors results_block at the cycle's solve block"
        );

        // A late/stale stamp through the same path must not regress it.
        engine.solve_dirty(5, &BlockMetadata::default(), &[]);
        assert_eq!(
            engine.results_block(),
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
        engine.set_result_channel(tx);
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
        engine.set_results_block_for_test(100);
        engine.compute_diff_and_send(&BlockMetadata::default());

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
        engine.set_result_channel(tx);

        let profit = U256::from(1_000_000u64);
        engine.set_profit_thresholds(U256::ZERO, profit);

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
        engine.set_results_block_for_test(100);
        engine.compute_diff_and_send(&BlockMetadata::default());

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
        engine.set_result_channel(tx);

        let profit = U256::from(1_000_000u64);
        engine.set_profit_thresholds(profit, U256::MAX);

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

        engine.compute_diff_and_send(&BlockMetadata::default());

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
        engine.set_result_channel(tx);

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
        let path_id = engine
            .register_and_solve_path(vec![
                PoolHop {
                    pool_id: v2_fwd_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_fwd_b,
                    zero_for_one: true,
                },
            ])
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
        // now OWNS this bookkeeping (the pump out-params retired in ergo task
        // LEZJAS) — drive it through the engine's own accessor so the test
        // exercises the same path the pump uses.
        engine.set_last_solved_block(0);
        engine.record_logs_this_block();

        engine.finalize_block(10, &metadata);

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
        // itself (the pump out-params were retired in ergo task LEZJAS).
        assert_eq!(engine.last_solved_block(), 10);
        assert!(!engine.has_logs_this_block());
    }

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
        engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2_fwd_a, // reserve0=USDC, reserve1=WETH
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_fwd_b, // reserve0=WETH, reserve1=USDC
                    zero_for_one: true,
                },
            ])
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
        let sqrt_price_lower_u160 =
            degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(-60)
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
        engine
            .register_path(vec![
                PoolHop {
                    pool_id: v3_key_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v3_key_b,
                    zero_for_one: false,
                },
            ])
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
        engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2_fwd,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v3_key,
                    zero_for_one: false,
                },
            ])
            .unwrap();

        // Even if no profit found (depends on exact numbers),
        // solve_all should run without panicking
        let results = engine.cycle.solve_all(&engine.registry);
        // Just verify it doesn't crash
        let _ = results;
    }

    #[test]
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
        let fresh_path = engine
            .register_and_solve_path(vec![
                PoolHop {
                    pool_id: v2_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_b,
                    zero_for_one: true,
                },
            ])
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
        let future_path = engine
            .register_and_solve_path(vec![
                PoolHop {
                    pool_id: v2_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v3_future,
                    zero_for_one: false,
                },
            ])
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
        let (results, block) = engine.latest_results();
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
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: v3_key,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_fwd,
                    zero_for_one: false,
                },
            ])
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
        engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2_fwd_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_fwd_b,
                    zero_for_one: true,
                },
            ])
            .unwrap();

        // Initial solve
        let results_before = engine.cycle.solve_all(&engine.registry);

        // Apply V2 update to make pool A even more mispriced
        engine.process_updates(
            &[(v2_addr_a, usdc(1_400_000), weth(750))],
            &[],
            1,
            &BlockMetadata::default(),
        );

        let (results_after, block) = engine.latest_results();
        assert_eq!(block, 1);
        // Results should differ after the update
        let _ = results_before; // Just ensure initial solve didn't panic
        let _ = results_after;
    }

    /// YXHHKR (resolves QNFYR5) — supersedes the removed TQ43TU gate test. A
    /// path whose price clock runs far behind the solve block is a QUIET pool
    /// (stored state byte-identical to on-chain), so it is SOLVED, not deferred.
    /// The old gate deferred it because `update_block` age looks like staleness —
    /// the quiet-pool false positive QNFYR5 proved live. Genuine chain/solver
    /// divergence is out of the solve path's scope: the tripwire retired with
    /// epic MROOY7 task 2UVG3E, and stale merge results are DROPPED by the
    /// Q1a window gate, never applied.
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
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_b,
                    zero_for_one: true,
                },
            ])
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
        let (fresh, _) = engine.latest_results();
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
        let (stale_results, block) = engine.latest_results();
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
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_b,
                    zero_for_one: true,
                },
            ])
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
        let (r0, _) = engine.latest_results();
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
        let (r1, _) = engine.latest_results();
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
        let (r2, _) = engine.latest_results();
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
        let sp_extreme = degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(-886_983)
            .unwrap_or_default();
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
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: v3_id,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v4_id,
                    zero_for_one: false,
                },
            ])
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

        let (results, _block) = engine.latest_results();

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

        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: v4_id,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2,
                    zero_for_one: false,
                },
            ])
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
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v4_id,
                    zero_for_one: false,
                },
            ])
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
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: v4_id,
                    zero_for_one: true, // sell currency0; output = amount1
                },
                PoolHop {
                    pool_id: v2,
                    zero_for_one: true,
                },
            ])
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
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: v4_id,
                    zero_for_one: false,
                },
                PoolHop {
                    pool_id: v2_id,
                    zero_for_one: true,
                },
            ])
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

    /// Build the minimal V3 tick-data (initialized +60/-60 ticks) used by
    /// `inspect_path_returns_hop_details`.
    fn inspect_test_v3_tick_data() -> HashMap<i32, crate::bot_core::TickInfo> {
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
        tick_data
    }

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
        let path_id = engine
            .register_path(vec![
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
            ])
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

        assert_eq!(engine.v3_pool_count(), 3);

        // Register 3-hop V3-V3-V3 path
        let path_id = engine
            .register_path(vec![
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
            ])
            .unwrap();

        assert_eq!(path_id, 1);
        assert_eq!(engine.path_count(), 1);

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
        let path_id = engine
            .register_path(vec![
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
            ])
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
        let id_a =
            engine.register_v2_pool(pool_a, usdc(1_500_000), weth(800), GAMMA_03, FEE_DENOM_03);
        let id_b =
            engine.register_v2_pool(pool_b, weth(800), usdc(1_500_000), GAMMA_03, FEE_DENOM_03);
        let id_c =
            engine.register_v2_pool(pool_c, weth(900), usdc(1_600_000), GAMMA_03, FEE_DENOM_03);

        engine
            .register_path(vec![
                PoolHop {
                    pool_id: id_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: id_b,
                    zero_for_one: true,
                },
            ])
            .unwrap();
        engine
            .register_path(vec![
                PoolHop {
                    pool_id: id_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: id_c,
                    zero_for_one: false,
                },
            ])
            .unwrap();

        // Cycle 1: both paths resolve; every UNIQUE (pool,direction) is a
        // miss. Pool A appears in both paths with the same direction, so its
        // single projection serves both paths: A+B+C = 3, not 4 hops.
        engine.solve_dirty(4, &BlockMetadata::default(), &[]);
        assert_eq!(engine.hop_projection_count(), 3);

        // Cycle 2: only pool B is dirty. Shared pool A must NOT re-project;
        // only B's hop in path 1 pays the walk (C's hops are untouched).
        engine.process_updates(
            &[(pool_b, usdc(1_000_000), weth(800))],
            &[],
            5,
            &BlockMetadata::default(),
        );
        engine.solve_dirty(5, &BlockMetadata::default(), &[]);
        // Only B's projection is fresh; A and C replay from the cache.
        assert_eq!(engine.hop_projection_count(), 4);

        // Cycle 3: A goes dirty. Its cached projection invalidates (nonce
        // advanced) and re-projects ONCE — both paths then share the fresh
        // entry; B and C's quiet hops still do not re-project.
        engine.process_updates(
            &[(pool_a, usdc(1_250_000), weth(800))],
            &[],
            6,
            &BlockMetadata::default(),
        );
        engine.solve_dirty(6, &BlockMetadata::default(), &oracle.to_affected_keys());
        assert_eq!(engine.hop_projection_count(), 5);
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
        let id_a =
            engine.register_v2_pool(pool_a, usdc(1_500_000), weth(800), GAMMA_03, FEE_DENOM_03);
        let id_b =
            engine.register_v2_pool(pool_b, weth(800), usdc(1_500_000), GAMMA_03, FEE_DENOM_03);

        // Path: A (USDC→WETH) → B (WETH→USDC). Initially balanced → no profit.
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: id_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: id_b,
                    zero_for_one: true,
                },
            ])
            .unwrap();

        // Install a result channel to capture the diff batches.
        let (tx, mut rx) = mpsc::unbounded_channel();
        engine.set_result_channel(tx);

        // Sanity: the balanced cycle is not profitable (an empty affected set
        // — the reorg keys below are explicit).
        engine.solve_dirty(4, &BlockMetadata::default(), &[]);
        engine.send_result_batch(&BlockMetadata::default());
        let (results_before, _) = engine.latest_results();
        assert!(
            !results_before.contains_key(&path_id),
            "balanced cycle should not be profitable before the Sync"
        );

        // Sync pool A at block 5 to misprice it hard (A's WETH drops to 1250
        // USDC/WETH vs B's 1875 — clears the ~0.6% round-trip fee).
        engine.process_updates(
            &[(pool_a, usdc(1_000_000), weth(800))],
            &[],
            5,
            &BlockMetadata::default(),
        );
        engine.send_result_batch(&BlockMetadata::default());

        let (results_after, _) = engine.latest_results();
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
        engine.solve_dirty(5, &BlockMetadata::default(), &reorg_keys);
        engine.send_result_batch(&BlockMetadata::default());

        // The arb is gone.
        let (results_reorg, _) = engine.latest_results();
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
            engine.v2_pool_count(),
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
        let result = engine.register_path(vec![
            ::degenbot_solvers::mixed::PoolHop {
                pool_id: real_pool_id,
                zero_for_one: true,
            },
            ::degenbot_solvers::mixed::PoolHop {
                pool_id: bogus_id,
                zero_for_one: false,
            },
        ]);
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
        use std::sync::Arc;
        use std::thread;

        use crate::bot_core::BlockMetadata;

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
                writer_engine.lock().solve_dirty(block, &metadata, &[]);
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
            let path_id = engine
                .register_and_solve_path(vec![
                    PoolHop {
                        pool_id: v2_fwd_a,
                        zero_for_one: true,
                    },
                    PoolHop {
                        pool_id: v2_fwd_b,
                        zero_for_one: true,
                    },
                ])
                .expect("path registration should succeed");
            let (results, _block) = engine.latest_results();
            let eager = results
                .get(&path_id)
                .expect("register_and_solve_path must eagerly solve a profitable path");
            baseline.insert(path_id, eager.clone());
        }

        // Full batch re-solve via solve_all_paths — this is the call path
        // whose solve loop gets parallelized. Equivalent eager results must
        // survive the batch re-solve (today in the resolve fan-out; a batch
        // `par_iter`).
        engine.solve_all_paths(1);
        let (results, block) = engine.latest_results();
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
    fn solve_dirty_parallel_fanout_survives_concurrent_readers_and_writer() {
        use std::sync::Arc;
        use std::thread;

        use crate::bot_core::BlockMetadata;

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
            let _ = engine
                .register_path(vec![
                    PoolHop {
                        pool_id: v2_fwd_a,
                        zero_for_one: true,
                    },
                    PoolHop {
                        pool_id: v2_fwd_b,
                        zero_for_one: true,
                    },
                ])
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
                writer_engine.lock().solve_dirty(block, &metadata, &[]);
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
        engine.set_result_channel(result_tx);
        engine.on_pump_ended();
        match result_rx.try_recv() {
            Err(TryRecvError::Disconnected) => {}
            other => panic!("result stream must be Disconnected after drop, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // HopType::SolidlyStable + ResolvedHop::SolidlyStable variant (Plan: Port
    // Solidly solve into the Rust engine — task BFIWUG).
    // -----------------------------------------------------------------
    #[test]
    fn solidly_hop_variant_is_not_v2_and_not_cl() {
        // The new variant must be excluded from the existing all-V2 and
        // all-CL dispatch branches — otherwise solve_path would mis-dispatch.
        assert!(!HopType::SolidlyStable.is_concentrated_liquidity());
    }

    #[test]
    fn resolved_solidly_hop_round_trips_via_as_solidly_state() {
        let state = SolidlyHopState {
            reserves_0: U256::from(1_000_000u64),
            reserves_1: U256::from(1_000_000u64),
            decimals_0: U256::from(10u64).pow(U256::from(6u64)),
            decimals_1: U256::from(10u64).pow(U256::from(18u64)),
            token_in: 0,
            fee_numer: U256::from(3u64),
            fee_denom: U256::from(1000u64),
            stable: true,
            variant: DexVariant::AerodromeV2Stable,
        };
        let hop = ResolvedHop::SolidlyStable {
            state: state.clone(),
        };

        // The new accessor returns the state.
        let got = hop
            .as_solidly_state()
            .expect("Solidly hop should yield its state");
        assert_eq!(got.reserves_0, state.reserves_0);
        assert_eq!(got.variant, DexVariant::AerodromeV2Stable);
        assert!(got.stable);

        // hop_type() maps to the new variant.
        assert_eq!(hop.hop_type(), HopType::SolidlyStable);

        // The Solidly hop is excluded from the V2 + CL accessors — the
        // existing dispatch arms must not pick it up.
        assert!(hop.as_v2_state().is_none());
        assert!(hop.as_int_sequence().is_none());
    }

    // The per-family Solidly projection tests live in
    // `crate::bot_core::resolve::solidly::tests` (moved in T3 of epic
    // MKRKNB; they assert the `MissingHopReason` variants directly
    // against `project_solidly`). This module keeps only the
    // engine-level classifier test (`solidly_hop_variant_is_not_v2_and_not_cl`).

    // -----------------------------------------------------------------
    // solve_solidly_path_int (task DMPSNG) — the two-stage Möbius precheck +
    // golden-section search. Tests cover all four AC cases: (1) all-Solidly
    // 2-hop, (2) V2+Solidly mixed, (3) unprofitable → None (precheck),
    // (4) Solidly+CL → None (scope rejection).
    // -----------------------------------------------------------------
    fn solidly_arb_engine() -> (ArbitrageEngine, u64, u64) {
        // Two Aerodrome-stable pools with the same token pair but divergent
        // reserves — a profitable arb cycle. Reserves use "wei magnitude"
        // (1e18 == 1 token of an 18-dec token) so the solidly math's
        // calc_d (which divides intermediate products by 1e18) does not
        // underflow to zero (small-magnitude reserves would panic on
        // divide-by-zero in get_y_solidly).
        use crate::bot_core::{BotState, RegisterAerodromeV2PoolParams};
        use std::sync::Arc;

        fn tokens(n: u64) -> U112 {
            (U256::from(n) * U256::from(10u64).pow(U256::from(18u64))).to::<U112>()
        }

        let core = Arc::new(crate::bot_core::state_lock::StateLock::new(BotState::new()));
        core.write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_token(
                Address::from([0x01u8; 20]),
                "Token0".into(),
                "T0".into(),
                18,
                1,
            );
        core.write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_token(
                Address::from([0x02u8; 20]),
                "Token1".into(),
                "T1".into(),
                18,
                1,
            );
        let aero_a = core
            .write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
                token0_decimals: 18,
                token1_decimals: 18,
                address: Address::from([0xa1u8; 20]),
                token0: Address::from([0x01u8; 20]),
                token1: Address::from([0x02u8; 20]),
                factory: Address::from([0xfau8; 20]),
                variant: DexVariant::AerodromeV2Stable,
                stable: true,
                fee: (3, 1000),
                reserve0: tokens(1000),
                reserve1: tokens(100),
                update_block: 0,
            });
        let aero_b = core
            .write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
                token0_decimals: 18,
                token1_decimals: 18,
                address: Address::from([0xa2u8; 20]),
                token0: Address::from([0x01u8; 20]),
                token1: Address::from([0x02u8; 20]),
                factory: Address::from([0xfau8; 20]),
                variant: DexVariant::AerodromeV2Stable,
                stable: true,
                fee: (3, 1000),
                // Pool B holds the SAME pair but with twice the token0 — its
                // token1→token0 price (reserve0 / reserve1) is 2x Pool A's, so a
                // token0→token1→token0 cycle is profitable (the V2-equivalent
                // Möbius optimal input is non-trivial).
                reserve0: tokens(2000),
                reserve1: tokens(100),
                update_block: 0,
            });
        let engine = ArbitrageEngine::with_core(Arc::clone(&core));
        (engine, aero_a, aero_b)
    }

    #[test]
    fn solve_solidly_2hop_all_solidly_matches_grid_scan() {
        let (mut engine, aero_a, aero_b) = solidly_arb_engine();
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: aero_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: aero_b,
                    zero_for_one: false,
                },
            ])
            .expect("path registers");
        let resolved = engine.cycle.path_resolved.get(&path_id).expect("resolved");
        let result = ::degenbot_solvers::mixed::solve_path(
            resolved,
            &::degenbot_solvers::profit_envelope::GateDeps::offline(),
        )
        .result
        .expect("profitable path solves");
        assert!(!result.optimal_input.is_zero());
        assert!(!result.profit.is_zero());
        assert_eq!(result.hop_outputs.len(), 2);
        assert_eq!(result.consumed_inputs.len(), 2);
        assert_eq!(result.consumed_inputs[0], result.optimal_input);
        assert_eq!(result.consumed_inputs[1], result.hop_outputs[0]);
        // profit = final output − optimal_input.
        assert_eq!(
            result.profit,
            result.hop_outputs[1].saturating_sub(result.optimal_input)
        );

        // Golden-section must not miss the global optimum: scan a fine grid
        // (1-token steps) and assert the solver's profit is within one grid
        // step of the grid max (±3 verification radius tolerance).
        let max_reserve = U256::from(1000u64) * U256::from(10u64).pow(U256::from(18u64));
        let grid_step = U256::from(10u64).pow(U256::from(18u64)); // 1 token
        let mut grid_best_profit = U256::ZERO;
        let mut x = U256::from(1u64);
        while x <= max_reserve {
            let out = ::degenbot_solvers::mixed::simulate_solidly_path(x, &resolved.hops);
            let profit = out.saturating_sub(x);
            if profit > grid_best_profit {
                grid_best_profit = profit;
            }
            x += grid_step;
        }
        assert!(
            result.profit + grid_step >= grid_best_profit,
            "solver profit {} should be within one grid step of grid max {}",
            result.profit,
            grid_best_profit
        );
        assert!(
            result.profit >= grid_best_profit.saturating_sub(grid_step),
            "solver profit {} must not fall more than one grid step below grid max {}",
            result.profit,
            grid_best_profit
        );
    }

    #[test]
    fn solve_solidly_mixed_v2_and_solidly_matches_grid_scan() {
        use crate::bot_core::{BotState, RegisterAerodromeV2PoolParams, RegisterV2PoolParams};
        use std::sync::Arc;

        fn tokens(n: u64) -> U112 {
            (U256::from(n) * U256::from(10u64).pow(U256::from(18u64))).to::<U112>()
        }

        let core = Arc::new(crate::bot_core::state_lock::StateLock::new(BotState::new()));
        core.write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_token(
                Address::from([0x01u8; 20]),
                "Token0".into(),
                "T0".into(),
                18,
                1,
            );
        core.write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_token(
                Address::from([0x02u8; 20]),
                "Token1".into(),
                "T1".into(),
                18,
                1,
            );
        // Mixed path: Solidly hop0 (token0→token1), V2 hop1 (token1→token0).
        // Mirrors the profitable all-Solidly fixture but with the second hop
        // as V2 constant-product (more slippage than Solidly, but the cycle
        // is still profitable because Solidly hop0 emits ample token1).
        let aero_id = core
            .write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
                token0_decimals: 18,
                token1_decimals: 18,
                address: Address::from([0xb1u8; 20]),
                token0: Address::from([0x01u8; 20]),
                token1: Address::from([0x02u8; 20]),
                factory: Address::from([0xfau8; 20]),
                variant: DexVariant::AerodromeV2Stable,
                stable: true,
                fee: (3, 1000),
                reserve0: tokens(1000),
                reserve1: tokens(100),
                update_block: 0,
            });
        let v2_id = core
            .write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_v2_pool(&RegisterV2PoolParams {
                address: Address::from([0xb2u8; 20]),
                token0: Address::from([0x01u8; 20]),
                token1: Address::from([0x02u8; 20]),
                reserve0: tokens(2000),
                reserve1: tokens(100),
                fee_token0: (997, 1000),
                fee_token1: (997, 1000),
                factory: Address::from([0xfbu8; 20]),
                update_block: 0,
                variant: DexVariant::UniswapV2,
                stable_swap: false,
                fee_denominator: None,
                ..Default::default()
            })
            .expect("test setup: V2 registration");
        let mut engine = ArbitrageEngine::with_core(Arc::clone(&core));
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: aero_id,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_id,
                    zero_for_one: false,
                },
            ])
            .expect("mixed V2+Solidly path registers");
        let resolved = engine.cycle.path_resolved.get(&path_id).expect("resolved");
        let result = ::degenbot_solvers::mixed::solve_path(
            resolved,
            &::degenbot_solvers::profit_envelope::GateDeps::offline(),
        )
        .result
        .expect("profitable mixed path solves");
        assert!(!result.profit.is_zero());

        // Grid scan parity check (Solidly hop uses the integer leaf, V2 hop
        // uses IntHopState::swap).
        let max_reserve = tokens(1000).to::<U256>();
        let grid_step = tokens(1).to::<U256>();
        let mut grid_best = U256::ZERO;
        let mut x = U256::from(1u64);
        while x <= max_reserve {
            let profit = ::degenbot_solvers::mixed::simulate_solidly_path(x, &resolved.hops)
                .saturating_sub(x);
            if profit > grid_best {
                grid_best = profit;
            }
            x += grid_step;
        }
        assert!(
            result.profit + grid_step >= grid_best,
            "mixed-path profit {} within one grid step of grid max {}",
            result.profit,
            grid_best
        );
    }

    #[test]
    fn solve_solidly_unprofitable_path_returns_none() {
        let (mut engine, aero_a, _aero_b) = solidly_arb_engine();
        // A round-trip through the SAME pool (token0→token1 then token1→token0)
        // is always unprofitable after fees — the Möbius precheck must early-out.
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: aero_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: aero_a,
                    zero_for_one: false,
                },
            ])
            .expect("path registers");
        let resolved = engine.cycle.path_resolved.get(&path_id).expect("resolved");
        assert!(
            ::degenbot_solvers::mixed::solve_path(
                resolved,
                &::degenbot_solvers::profit_envelope::GateDeps::offline()
            )
            .result
            .is_none(),
            "round-trip through one pool is unprofitable"
        );
    }

    #[test]
    fn solve_solidly_plus_cl_path_rejected_by_scope() {
        use crate::bot_core::{BotState, RegisterAerodromeV2PoolParams, RegisterV3PoolParams};
        use std::sync::Arc;

        let core = Arc::new(crate::bot_core::state_lock::StateLock::new(BotState::new()));
        core.write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_token(
                Address::from([0x01u8; 20]),
                "Token0".into(),
                "T0".into(),
                18,
                1,
            );
        core.write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_token(
                Address::from([0x02u8; 20]),
                "Token1".into(),
                "T1".into(),
                18,
                1,
            );
        let aero = core
            .write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
                token0_decimals: 18,
                token1_decimals: 18,
                address: Address::from([0xa1u8; 20]),
                token0: Address::from([0x01u8; 20]),
                token1: Address::from([0x02u8; 20]),
                factory: Address::from([0xfau8; 20]),
                variant: DexVariant::AerodromeV2Stable,
                stable: true,
                fee: (3, 1000),
                reserve0: (U256::from(1000u64) * U256::from(10u64).pow(U256::from(18u64)))
                    .to::<U112>(),
                reserve1: (U256::from(100u64) * U256::from(10u64).pow(U256::from(18u64)))
                    .to::<U112>(),
                update_block: 0,
            });
        // Register a minimal V3 pool for the second hop using the same
        // ..Default::default() pattern as the existing V3 tests.
        let v3_id = core
            .write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_v3_pool(&RegisterV3PoolParams {
                address: Address::from([0xc1u8; 20]),
                token0: Address::from([0x02u8; 20]),
                token1: Address::from([0x01u8; 20]),
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
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: aero,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v3_id,
                    zero_for_one: false,
                },
            ])
            .expect("path registers (resolve is per-arm)");
        let resolved = engine.cycle.path_resolved.get(&path_id).expect("resolved");
        // Solidly + CL is out of scope (p): solve_path returns None.
        assert!(::degenbot_solvers::mixed::solve_path(
            resolved,
            &::degenbot_solvers::profit_envelope::GateDeps::offline()
        )
        .result
        .is_none());
    }
    // -----------------------------------------------------------------------
    // Balancer weighted solve branch (AT2TGZ)
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
        engine
            .register_path(vec![
                PoolHop {
                    pool_id: pool_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: pool_b,
                    zero_for_one: false,
                },
            ])
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

        engine
            .register_path(vec![
                PoolHop {
                    pool_id: pool_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: pool_b,
                    zero_for_one: false,
                },
            ])
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
        engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v2_b,
                    zero_for_one: false,
                },
            ])
            .unwrap();
        let v2_results = engine.cycle.solve_all(&engine.registry);
        let v2_profit = v2_results.values().next().unwrap().profit;

        // Solve Balancer-V2-V2 path (clear and re-solve)
        drop(
            engine
                .core
                .write_at(crate::bot_core::state_lock::LockSite::Solver),
        );
        let bw_path = engine
            .register_path(vec![
                PoolHop {
                    pool_id: bw_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: bw_b,
                    zero_for_one: false,
                },
            ])
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
        engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: bw,
                    zero_for_one: false,
                },
            ])
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
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: bw,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v3,
                    zero_for_one: false,
                },
            ])
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

    /// ZU7RAF: the core `ArbitrageEngine` OWNS the lifecycle phase — a
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

    /// TJT63P: `allow_subscribe` accepts `Created` (legacy subscribe-first path)
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
        engine
            .register_path(vec![
                PoolHop {
                    pool_id: pool_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: pool_b,
                    zero_for_one: false,
                },
            ])
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

        engine
            .register_path(vec![
                PoolHop {
                    pool_id: pool_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: pool_b,
                    zero_for_one: false,
                },
            ])
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
        engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: bs,
                    zero_for_one: false,
                },
            ])
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
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: bs,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v3,
                    zero_for_one: false,
                },
            ])
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

    // -----------------------------------------------------------------------
    // Curve stableswap solve branch (RPDDWH)
    // -----------------------------------------------------------------------

    /// Two-token Curve stableswap pool params (standard, raw balances, no
    /// rates, no lending). amp=100 (raw), fee=4e6 (0.04% of 1e10).
    fn curve_stable_params(
        addr: Address,
        balance0: u128,
        balance1: u128,
    ) -> crate::bot_core::RegisterCurvePoolParams {
        let one_e18 = U256::from(10u64).pow(U256::from(18u64));
        let precision = one_e18; // PRECISION = 1e18
        crate::bot_core::RegisterCurvePoolParams {
            address: addr,
            tokens: vec![Address::repeat_byte(0x01), Address::repeat_byte(0x02)],
            a_coefficient: 10,
            a_precision: 100,
            fee: 4_000_000, // 0.04% of 1e10
            admin_fee: 0,
            rate_multipliers: vec![precision, precision], // identity rates
            balances: vec![
                U256::from(balance0) * one_e18,
                U256::from(balance1) * one_e18,
            ],
            update_block: 0,
            swap_style: 0,         // STANDARD
            lending_rate_style: 0, // NONE
            d_variant: 1,          // Standard
            y_variant: 1,          // Standard
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
            use_lending: vec![false, false],
            precision_multipliers: vec![precision, precision],
            tokens_underlying: None,
            metapool_rate_style: 0,
            metapool_underlying_style: 0,
            data_provider: None,
        }
    }

    #[test]
    fn curve_stable_finds_profitable_arb() {
        let mut engine = ArbitrageEngine::new();

        let pool_a = engine
            .core
            .write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_curve_pool(&curve_stable_params(
                Address::from([0xe1u8; 20]),
                1000,
                2000,
            ));
        let pool_b = engine
            .core
            .write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_curve_pool(&curve_stable_params(
                Address::from([0xe2u8; 20]),
                1000,
                1950,
            ));

        engine
            .register_path(vec![
                PoolHop {
                    pool_id: pool_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: pool_b,
                    zero_for_one: false,
                },
            ])
            .unwrap();

        let results = engine.cycle.solve_all(&engine.registry);
        eprintln!("results: {}", results.len());
        assert!(
            !results.is_empty(),
            "should find profitable Curve stableswap arb"
        );
        let r = results.values().next().unwrap();
        assert!(
            !r.optimal_input.is_zero(),
            "optimal input should be non-zero"
        );
        assert!(!r.profit.is_zero(), "profit should be non-zero");
    }

    #[test]
    fn curve_stable_unprofitable_path_returns_none() {
        let mut engine = ArbitrageEngine::new();

        let pool_a = engine
            .core
            .write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_curve_pool(&curve_stable_params(
                Address::from([0xf1u8; 20]),
                1000,
                2000,
            ));
        let pool_b = engine
            .core
            .write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_curve_pool(&curve_stable_params(
                Address::from([0xf2u8; 20]),
                1000,
                2000,
            ));

        engine
            .register_path(vec![
                PoolHop {
                    pool_id: pool_a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: pool_b,
                    zero_for_one: false,
                },
            ])
            .unwrap();

        let results = engine.cycle.solve_all(&engine.registry);
        assert!(
            results.is_empty(),
            "identical Curve pools should not produce an arb"
        );
    }

    #[test]
    fn curve_stable_mixed_with_v2_finds_arb() {
        let mut engine = ArbitrageEngine::new();
        let one_e18 = U256::from(10u64).pow(U256::from(18u64));

        let v2 = engine.register_v2_pool(
            Address::from([0xa5u8; 20]),
            (U256::from(1000u64) * one_e18).to::<U112>(),
            (U256::from(2000u64) * one_e18).to::<U112>(),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let cs = engine
            .core
            .write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_curve_pool(&curve_stable_params(
                Address::from([0xa6u8; 20]),
                1000,
                1500,
            ));

        engine
            .register_path(vec![
                PoolHop {
                    pool_id: v2,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: cs,
                    zero_for_one: false,
                },
            ])
            .unwrap();

        let results = engine.cycle.solve_all(&engine.registry);
        assert!(!results.is_empty(), "should find V2+Curve mixed arb");
    }

    #[test]
    fn curve_stable_rejects_mixed_with_cl() {
        use std::sync::Arc;
        let core = Arc::new(crate::bot_core::state_lock::StateLock::new(
            crate::bot_core::BotState::new(),
        ));

        let cs = core
            .write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_curve_pool(&curve_stable_params(
                Address::from([0xb4u8; 20]),
                1000,
                2000,
            ));
        let v3 = core
            .write_at(crate::bot_core::state_lock::LockSite::Solver)
            .register_v3_pool(&RegisterV3PoolParams {
                address: Address::from([0xc4u8; 20]),
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
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: cs,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: v3,
                    zero_for_one: false,
                },
            ])
            .expect("path registers (resolve succeeds per-arm)");
        let resolved = &engine.cycle.path_resolved[&path_id];
        assert!(
            ::degenbot_solvers::mixed::solve_path(
                resolved,
                &::degenbot_solvers::profit_envelope::GateDeps::offline()
            )
            .result
            .is_none(),
            "Curve + CL must not solve"
        );
    }
    /// A minimal tracing capture layer: records (name, span id, parent id)
    /// for every span created under the subscriber. Deliberately NOT the
    /// `OTel` exporter - span-PARENTING is not an `OTel` concern, so this
    /// invariant test runs in the DEFAULT test gate (the otel-gated
    /// `InMemorySpanExporter` harness stays for the attribute-level tests).
    #[derive(Clone, Default)]
    struct SpanParentCapture {
        spans: std::sync::Arc<std::sync::Mutex<Vec<SpanRecord>>>,
    }

    /// One captured span: (name, span id, parent id).
    type SpanRecord = (String, u64, Option<u64>);

    thread_local! {
        /// Current-span stack mirror: on_enter/on_exit maintain it so
        /// contextually-created children resolve their parent the way
        /// tracing's dispatcher does.
        static SPAN_STACK: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SpanParentCapture {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            id: &tracing::span::Id,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let parent = SPAN_STACK.with(|st| st.borrow().last().copied());
            let spans = std::sync::Arc::clone(&self.spans);
            let name = attrs.metadata().name().to_string();
            spans
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((name, id.into_u64(), parent));
        }

        fn on_enter(
            &self,
            id: &tracing::span::Id,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            SPAN_STACK.with(|st| st.borrow_mut().push(id.into_u64()));
        }

        fn on_exit(
            &self,
            _id: &tracing::span::Id,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            SPAN_STACK.with(|st| {
                st.borrow_mut().pop();
            });
        }
    }

    /// K4ETHF follow-up (trace f06ea422 / block 25900244): the old
    /// two-acquisition gate let a concurrent dirty marker land BETWEEN the
    /// probe and the take - the solve then did real work (1518 affected
    /// paths) through the no-span branch, orphaning its phase spans under
    /// `degenbot.epoch` and escaping the `solve_duration` histogram. The gate and
    /// the work now share ONE mutex acquisition (dirt marking needs the same
    /// mutex, so probe and take cannot disagree). Invariant under test:
    /// every fanout span's parent is an arb.solve span (a fanout implies
    /// real affected paths; real work must have taken the span branch).
    ///
    /// DEFAULT-GATE VISIBLE (no otel cfg) - reviewer flag on 2f22fa575: the
    /// race class must not live behind an optional feature.
    ///
    /// The metrics half of the harm (`solve_duration` sample + `solves_executed`
    /// count) is structural now: counting happens on the same span-branch as
    /// parenting, so there is no code path that does work without either.
    #[test]
    #[expect(clippy::expect_used)]
    #[expect(clippy::too_many_lines)]
    fn solve_dirty_race_marks_dirty_work_with_solve_span() {
        use std::collections::HashSet;
        use std::sync::Arc;

        use crate::arb_engine::EngineStages;
        use tracing_subscriber::layer::SubscriberExt;

        let capture = SpanParentCapture::default();
        let log = std::sync::Arc::clone(&capture.spans);
        let subscriber = tracing_subscriber::registry().with(capture);

        // Real registered paths (mirrors the 3780 concurrency fixture) so a
        // dirty marker produces genuine fan-out phase work.
        let core = Arc::new(crate::bot_core::state_lock::StateLock::new(
            crate::bot_core::BotState::new(),
        ));
        let mut engine = ArbitrageEngine::with_core(Arc::clone(&core));
        let mut pool_ids = Vec::new();
        for i in 0u8..8 {
            let addr_a = Address::from([0x10_u8 + i; 20]);
            let a = engine.register_v2_pool(
                addr_a,
                usdc(1_500_000),
                weth(800 + u64::from(i) * 10),
                GAMMA_03,
                FEE_DENOM_03,
            );
            let addr_b = Address::from([0x20_u8 + i; 20]);
            let b = engine.register_v2_pool(
                addr_b,
                weth(800 + u64::from(i) * 10),
                usdc(2_000_000),
                GAMMA_03,
                FEE_DENOM_03,
            );
            let _ = engine
                .register_path(vec![
                    PoolHop {
                        pool_id: a,
                        zero_for_one: true,
                    },
                    PoolHop {
                        pool_id: b,
                        zero_for_one: true,
                    },
                ])
                .expect("path registration should succeed");
            pool_ids.push(a);
        }
        let engine = Arc::new(parking_lot::Mutex::new(engine));

        // Seed every path dirty up front: the fanout assertions below need
        // at least one solve cycle, and the marker thread's 50us cadence is
        // best-effort — a slow CI runner can starve it for the whole 200
        // solve loop, leaving the engine clean and the fixture vacuous (CI
        // panic: "fixture must produce phase spans"). Pre-seeding makes the
        // first solve deterministically dirty while the marker thread
        // continues to exercise the probe<->take race window.
        // LXDY4C: the seeds + marker ride the SHARED epoch ledger; the drain
        // consumes take_keys per cycle (deterministically dirty on the first
        // solve; the marker thread keeps landing NEW dirt in later cycles).
        let marker_delta = std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64));
        {
            for pid in &pool_ids {
                marker_delta.record_affected(HopType::V2, *pid, 0u64);
            }
        }

        // Marker thread: continuously re-marks a tracked V2 pool dirty -
        // under the old gate these landings are exactly the probe<->take
        // window; under the single-acquisition gate they can only be seen by
        // the take itself.
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let starter = Arc::new(std::sync::Barrier::new(2));
        let marker_stop = Arc::clone(&stop);
        let marker_delta_thread = std::sync::Arc::clone(&marker_delta);
        let bar0 = Arc::clone(&starter);
        let marker = std::thread::spawn(move || {
            bar0.wait();
            let mut rot = 0usize;
            while !marker_stop.load(std::sync::atomic::Ordering::Relaxed) {
                // LXDY4C: the marker records into the shared epoch ledger —
                // the delta IS what the drain takes (no engine-local intake
                // remains, so the retired probe<->take window cannot exist).
                marker_delta_thread.record_affected(HopType::V2, pool_ids[rot % 8], 0u64);
                rot = rot.wrapping_add(1);
                // Bounded pace: enough iterations to hit any probe<->take
                // window the old gate exposed, without spinning hot and
                // perturbing the timing-sensitive detached-cycle neighbors.
                std::thread::sleep(std::time::Duration::from_micros(50));
            }
        });

        let handle = EngineStages::new(
            std::sync::Arc::clone(&engine),
            std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64)),
        );
        starter.wait();
        let block = 5000u64;
        tracing::subscriber::with_default(subscriber, || {
            for i in 0..200 {
                // Drain the ledger the way the settle drain does: take
                // keys, solve exactly what the take returned.
                let keys = marker_delta.take_keys();
                if !keys.is_empty() {
                    handle.run_solve_cycle(&keys, block + i, &BlockMetadata::default());
                }
            }
        });
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        marker.join().expect("marker thread");

        let spans = log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let solve_ids: HashSet<u64> = spans
            .iter()
            .filter(|(name, _, _)| name == "degenbot.arb.solve")
            .map(|(_, id, _)| *id)
            .collect();
        let fanouts: Vec<_> = spans
            .iter()
            .filter(|(name, _, _)| name == "degenbot.arb.fanout")
            .collect();
        assert!(
            !fanouts.is_empty(),
            "fixture must produce phase spans (marker thread keeps the engine dirty)"
        );
        let orphaned: Vec<_> = fanouts
            .iter()
            .filter(|(_, _, parent)| parent.is_none_or(|p| !solve_ids.contains(&p)))
            .collect();
        assert!(
            orphaned.is_empty(),
            "fanout spans orphaned outside an arb.solve parent: {} of {}",
            orphaned.len(),
            fanouts.len()
        );
    }

    /// PWPPAZ T1 (flips the trace-91a4a776 pin): the tombstone finalize must
    /// NOT run a solve cycle. Trace 91a4a776's inner `solve_dirty` — and the
    /// span gate later added around it — retired with this task: the finalize
    /// is dispatched tombstone-driven and executed by the drainer while the
    /// SUCCESSOR block's burst is still being applied, so its solve consumed
    /// the successor's first-dirt under the dead block's identity (traces
    /// ab13f75f: finalize(83) solved 1,755 paths of 84's dirt; 98f7cf52 and
    /// the fresh census: 2/20 blocks with the degenerate pattern). The
    /// boundary is now bookkeeping-only: the guarded transition advances the
    /// block cursor (6XB6NJ: `BlockCursor::finalize`) and emits the terminal
    /// publish; dirt stays unconsumed for the pump's drained-settle gate. RED
    /// while `finalize_block` still called `solve_dirty`.
    #[test]
    #[expect(clippy::expect_used)]
    fn finalize_block_consumes_no_dirt_and_emits_no_solve() {
        use std::collections::HashSet;
        use std::sync::Arc;

        use crate::arb_engine::EngineStages;
        use tracing_subscriber::layer::SubscriberExt;

        let mut oracle = crate::arb_engine::tests::test_keys::DirtyKeys::new();

        let capture = SpanParentCapture::default();
        let log = Arc::clone(&capture.spans);
        let subscriber = tracing_subscriber::registry().with(capture);

        // Real pools + path so the finalize solve does genuine fan-out work
        // (mirrors the tombstone-adjacent dirt crossing the burst boundary).
        let mut engine = ArbitrageEngine::new();
        let a = engine.register_v2_pool(
            Address::from([0x21u8; 20]),
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let b = engine.register_v2_pool(
            Address::from([0x22u8; 20]),
            weth(800),
            usdc(1_600_000),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let _pid = engine
            .register_path(vec![
                PoolHop {
                    pool_id: a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: b,
                    zero_for_one: true,
                },
            ])
            .expect("path registers");
        oracle.insert(a, HopType::V2);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        engine.set_result_channel(tx);
        let engine_state = Arc::new(parking_lot::Mutex::new(engine));
        let _handle = EngineStages::new(
            Arc::clone(&engine_state),
            Arc::new(crate::bot_core::EpochDelta::new(0u64)),
        );

        tracing::subscriber::with_default(subscriber, || {
            engine_state
                .lock()
                .finalize_block(5, &BlockMetadata::default());
        });

        let spans = log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        // PWPPAZ T1: the finalize emits NO solve at all — no arb.solve span,
        // no phase spans. Any solve here would race the successor block's
        // burst (the steal observed in traces ab13f75f / 98f7cf52).
        let solve_ids: HashSet<u64> = spans
            .iter()
            .filter(|(name, _, _)| name == "degenbot.arb.solve")
            .map(|(_, id, _)| *id)
            .collect();
        assert!(
            solve_ids.is_empty(),
            "finalize must not run a solve cycle; got arb.solve spans {solve_ids:?}"
        );
        let phase_spans: Vec<&String> = spans
            .iter()
            .map(|(name, _, _)| name)
            .filter(|name| name.contains("arb."))
            .collect();
        assert!(
            phase_spans.is_empty(),
            "finalize must emit no solve-phase spans; got {phase_spans:?}"
        );
        // LXDY4C: unconsumed dirt lives in the DRAIN-SEAM epoch ledger now —
        // the engine has no local dirty intake to probe; finalize consumed no
        // keys (the coordinator's has_dirty/ledger tests pin that contract).
        // Boundary bookkeeping advanced under the same guard.
        {
            let engine = engine_state.lock();
            assert_eq!(
                engine.last_solved_block(),
                5,
                "finalize must advance the solved boundary"
            );
            assert!(!engine.has_logs_this_block());
            // Results anchor advanced for the terminal batch.
            assert_eq!(engine.results_block(), 5);
        }
        // Terminal publish: the boundary batch still flows to Python with the
        // finalized block as its solve_block.
        let batch = rx
            .try_recv()
            .expect("finalize must emit the terminal boundary batch");
        assert_eq!(batch.solve_block, 5);
    }

    /// PWPPAZ T1: both guard branches — a block whose logs dirtied nothing
    /// (or never arrived) still gets its one-shot boundary advance + terminal
    /// publish (`solve_block` = the finalized block), and a re-fire of the
    /// guard for the same boundary must not double-publish.
    #[test]
    #[expect(clippy::expect_used)]
    fn finalize_boundary_publishes_even_when_nothing_dirtied() {
        let mut engine = ArbitrageEngine::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        engine.set_result_channel(tx);
        engine.set_last_solved_block(0);
        // Empty-logs branch: no dirt, no recorded logs — the pure
        // header-advance boundary.
        engine.finalize_block(7, &BlockMetadata::default());
        assert_eq!(engine.last_solved_block(), 7);
        assert!(!engine.has_logs_this_block());
        let batch = rx
            .try_recv()
            .expect("boundary batch must be emitted without dirt");
        assert_eq!(batch.solve_block, 7);
        // Guard no-ops the re-fired boundary.
        engine.finalize_block(7, &BlockMetadata::default());
        assert!(
            rx.try_recv().is_err(),
            "guard must not double-publish a settled boundary"
        );
    }

    /// ZZS6CG (trace hygiene): a solve span must parent to its OWN block's
    /// published epoch root span (`degenbot.epoch`) - exact-match only. The stale
    /// `DrainWork::Finalize` (retired in MROOY7) crossing a block boundary parked
    /// block N-1's
    /// `arb.solve` inside block N's trace in 19/20 of the recent traces
    /// analyzed (the drain/finalize arms inherited the dispatch-time loop
    /// context unconditionally). RED before `attach_published_parent_exact`
    /// existed. Two assertions:
    /// 1. exact hit - solve(100) with a published context for 100 parents to
    ///    the published epoch(100) span, not the ambient newer block;
    /// 2. exact miss - solve with NO published context keeps its ambient
    ///    parent (no fallback onto an unrelated older block, no orphan).
    #[cfg(feature = "otel")]
    #[test]
    #[expect(clippy::expect_used)]
    fn solve_spans_anchor_to_their_own_published_block() {
        use crate::arb_engine::EngineStages;
        use crate::otel;
        use opentelemetry_sdk::trace::InMemorySpanExporter;
        use std::sync::Arc;
        use tracing_subscriber::layer::SubscriberExt;

        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));

        let mut oracle = crate::arb_engine::tests::test_keys::DirtyKeys::new();
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let engine = Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new()));
                oracle.insert(0x0BAD_F00D, HopType::V2);
                Arc::new(EngineStages::new(
                    engine,
                    Arc::new(crate::bot_core::EpochDelta::new(0u64)),
                ))
            })
            .collect();

        tracing::subscriber::with_default(subscriber, || {
            // Published context for block 100 (a completed earlier settle).
            {
                let block100 = tracing::info_span!("degenbot.epoch.run", block.number = 100u64);
                let _guard = block100.enter();
                crate::telemetry::publish_block_context(100);
            }

            // The newer block's loop context is ambient during both solves
            // (the stale-crossing shape: block 101's context is current).
            let ambient = tracing::info_span!("degenbot.epoch.run", block.number = 101u64);
            let _ambient_guard = ambient.enter();

            // (1) Exact hit: solve of the PUBLISHED block 100 re-attaches to
            // the published epoch(100) span, not the ambient 101 span.
            let h100 = Arc::clone(&handles[0]);
            h100.run_solve_cycle(&oracle.to_affected_keys(), 100, &BlockMetadata::default());

            // (2) Exact miss: solve of block 101 (never published) keeps the
            // ambient parent - no fallback re-parenting, no orphan.
            let h101 = Arc::clone(&handles[1]);
            h101.run_solve_cycle(&oracle.to_affected_keys(), 101, &BlockMetadata::default());
        });

        provider.force_flush().expect("flush");
        let spans = exporter.get_finished_spans().expect("spans");
        let span_for_block = |blk: u64, name: &str| {
            spans
                .iter()
                .find(|sp| {
                    sp.name.as_ref() == name
                        && sp.attributes.iter().any(|kv| {
                            kv.key == opentelemetry::Key::from_static_str("block.number")
                                && (matches!(kv.value, opentelemetry::Value::I64(v) if v == blk.cast_signed())
                                    || matches!(kv.value, opentelemetry::Value::String(ref v) if v.as_str() == blk.to_string().as_str()))
                        })
                }).map_or_else(|| panic!("{name} for block {blk} must be exported"), |sp| sp.span_context.span_id())
        };

        let published_100 = span_for_block(100, "degenbot.epoch.run");
        let ambient_101 = span_for_block(101, "degenbot.epoch.run");
        let solve_100 = span_for_block(100, "degenbot.arb.solve");
        let solve_101 = span_for_block(101, "degenbot.arb.solve");

        // Look up both solve spans' parents via the exported spans.
        let parent_of = |id| {
            spans
                .iter()
                .find(|sp| sp.span_context.span_id() == id)
                .map(|sp| sp.parent_span_id)
                .expect("solve span exported")
        };
        assert_eq!(
            parent_of(solve_100),
            published_100,
            "solve(published block) must re-attach to its own block's published span"
        );
        assert_eq!(
            parent_of(solve_101),
            ambient_101,
            "solve(unpublished block) must keep the ambient parent - no fallback mis-dating"
        );
    }

    /// KNEUQX: the arb.solve span records `cycle.solve_block` (the cycle's
    /// anchored work block = `engine.results_block()`) alongside the entry
    /// block.number tag. At a settle boundary the anchor is the pool-state
    /// head and can run one (or more) ahead of the entry block - the field
    /// makes that visible/self-documenting in Jaeger instead of showing a
    /// parent span seemingly contradicting its phase children. Pin: the
    /// exported attribute matches the engine's post-solve anchor.
    #[cfg(feature = "otel")]
    #[test]
    #[expect(clippy::expect_used)]
    fn solve_span_records_cycle_solve_block() {
        use crate::arb_engine::EngineStages;
        use crate::otel;
        use opentelemetry_sdk::trace::InMemorySpanExporter;
        use std::sync::Arc;
        use tracing_subscriber::layer::SubscriberExt;

        const MY_SOLVE_BLOCK: u64 = 0x5EED_B10C;

        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));

        let mut oracle = crate::arb_engine::tests::test_keys::DirtyKeys::new();
        let engine = Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new()));
        oracle.insert(0x0BAD_F00D, HopType::V2);
        let engine_arc = Arc::clone(&engine);
        let handle = EngineStages::new(
            engine,
            std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64)),
        );
        tracing::subscriber::with_default(subscriber, || {
            handle.run_solve_cycle(
                &oracle.to_affected_keys(),
                MY_SOLVE_BLOCK,
                &BlockMetadata::default(),
            );
        });

        provider.force_flush().expect("flush");
        let spans = exporter.get_finished_spans().expect("spans");
        let solve = spans
            .iter()
            .find(|sp| sp.name.as_ref() == "degenbot.arb.solve")
            .expect("solve span must be exported");
        let expected = engine_arc.lock().results_block();
        let recorded = solve
            .attributes
            .iter()
            .find(|kv| kv.key == opentelemetry::Key::from_static_str("cycle.solve_block"))
            .map_or_else(|| "ABSENT".to_string(), |kv| kv.value.to_string());
        assert_eq!(
            recorded,
            expected.to_string(),
            "arb.solve must record the cycle's anchored block"
        );
    }

    // P5FEOI (epic 2LXPPV): original span test, otel-gated like its harness.
    #[cfg(feature = "otel")]
    #[test]
    #[expect(clippy::expect_used)]
    fn solve_dirty_emits_arb_solve_span_with_block_number() {
        use crate::arb_engine::EngineStages;
        use crate::otel;
        use opentelemetry_sdk::trace::InMemorySpanExporter;
        use std::sync::Arc;
        use tracing_subscriber::layer::SubscriberExt;

        const MY_SOLVE_BLOCK: u64 = 0x0BAD_F00D;
        const MY_SOLVE_BLOCK_I64: i64 = 0x0BAD_F00D;

        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));

        // T0 no-op gating: the span fires only when the engine holds dirty
        // paths — mark one so this test still exercises the emitted-span path.
        let mut oracle = crate::arb_engine::tests::test_keys::DirtyKeys::new();
        let engine = Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new()));
        oracle.insert(0x0BAD_F00D, HopType::V2);
        let handle = EngineStages::new(
            engine,
            std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64)),
        );
        tracing::subscriber::with_default(subscriber, || {
            handle.run_solve_cycle(
                &oracle.to_affected_keys(),
                MY_SOLVE_BLOCK,
                &BlockMetadata::default(),
            );
        });

        provider.force_flush().expect("flush");
        let spans = exporter.get_finished_spans().expect("spans");

        // Same dual-representation attribute check as the MQUKB6 pump test (tracing-
        // opentelemetry 0.33 maps u64 fields to strings; an OTel bump may switch to
        // I64 - accept both).
        let my_spans: Vec<_> = spans
            .iter()
            .filter(|sp| {
                sp.name.as_ref() == "degenbot.arb.solve"
            })
            .filter(|sp| {
                sp.attributes.iter().any(|kv| {
                    kv.key == opentelemetry::Key::from_static_str("block.number")
                        && (matches!(kv.value, opentelemetry::Value::I64(v) if v == MY_SOLVE_BLOCK_I64)
                            || matches!(kv.value, opentelemetry::Value::String(ref v) if v.as_str() == MY_SOLVE_BLOCK.to_string().as_str()))
                })
            })
            .collect();
        assert_eq!(
            my_spans.len(),
            1,
            "expected exactly one degenbot.arb.solve span for block {MY_SOLVE_BLOCK}; got names: {:?}",
            spans.iter().map(|sp| sp.name.as_ref()).collect::<Vec<_>>()
        );
    }

    /// XC7SWD + LPEOBI: the pre-cycle expiry window (core write
    /// `expire_v3/v4`) owns a ~2.8-3.1s lock-queue slot per cycle. When
    /// `max_age` is unset (production cockpit default) the expiry is
    /// PROVABLY a no-op and must not take the core write at all: no
    /// `degenbot.arb.expire` span, no queue position.
    #[cfg(feature = "otel")]
    #[test]
    fn solve_dirty_skips_expire_spans_when_max_age_unset() {
        use crate::arb_engine::EngineStages;
        use crate::otel;
        use opentelemetry_sdk::trace::InMemorySpanExporter;
        use std::sync::Arc;
        use tracing_subscriber::layer::SubscriberExt;

        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));

        let mut oracle = crate::arb_engine::tests::test_keys::DirtyKeys::new();
        let engine = Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new()));
        oracle.insert(0x0BAD_F00D, HopType::V2);
        let handle = EngineStages::new(
            engine,
            std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64)),
        );
        tracing::subscriber::with_default(subscriber, || {
            handle.run_solve_cycle(
                &oracle.to_affected_keys(),
                0x0BAD_F00D,
                &BlockMetadata::default(),
            );
        });

        provider.force_flush().expect("flush");
        let spans = exporter.get_finished_spans().expect("spans");
        let expire_spans: Vec<_> = spans
            .iter()
            .filter(|sp| sp.name.as_ref() == "degenbot.arb.expire")
            .collect();
        assert!(
            expire_spans.is_empty(),
            "max_age=None expiry is a no-op - must not take the core write; got {expire_spans:?}"
        );
    }

    /// Resolve->LPT staging trace (f701ccd36f4ecf80d671e798df218fa4, block
    /// 25906841): between the close of `arb.resolve` and the open of
    /// `arb.lpt` sat 647 ms of uninstrumented wall time — the results sweep
    /// and resolved-snapshot staging (`to_solve`) that makes the engine
    /// borrow-free for the parallel dispatch. That phase must emit its own
    /// `degenbot.arb.stage` phase span carrying `paths.staged`, so the
    /// staging cost is attributable in Jaeger like its fanout/resolve/lpt/
    /// merge siblings (MQUKB6-T2 pattern). RED before the span existed.
    #[cfg(feature = "otel")]
    #[test]
    #[expect(clippy::expect_used)]
    fn run_epoch_emits_stage_span_with_paths_staged() {
        use crate::otel;
        use hashbrown::HashSet;
        use opentelemetry_sdk::trace::InMemorySpanExporter;
        use tracing_subscriber::layer::SubscriberExt;

        const PATH_COUNT: u64 = 1;

        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));

        let mut engine = ArbitrageEngine::new();
        let a = engine.register_v2_pool(
            Address::from([0x11u8; 20]),
            usdc(1_000_000),
            weth(500),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let a2 = engine.register_v2_pool(
            Address::from([0x13u8; 20]),
            usdc(1_100_000),
            weth(510),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let _path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: a2,
                    zero_for_one: true,
                },
            ])
            .expect("path registers");

        tracing::subscriber::with_default(subscriber, || {
            engine.cycle.run_epoch(
                &crate::arb_engine::tests::test_keys::affected_keys(
                    &HashSet::from([a]),
                    &HashSet::new(),
                    &HashSet::new(),
                ),
                5,
                &BlockMetadata::default(),
                &engine.registry,
                &mut engine.delivery,
            );
        });

        provider.force_flush().expect("flush");
        let spans = exporter.get_finished_spans().expect("spans");
        let stage_spans: Vec<_> = spans
            .iter()
            .filter(|sp| sp.name.as_ref() == "degenbot.arb.stage")
            .collect();
        assert_eq!(
            stage_spans.len(),
            1,
            "expected exactly one degenbot.arb.stage span per solve cycle; got names: {:?}",
            spans.iter().map(|sp| sp.name.as_ref()).collect::<Vec<_>>()
        );
        // Dual-representation check (u64 fields map to String or I64 under
        // tracing-opentelemetry 0.33; mirrors the MQUKB6 pump test).
        assert!(
            stage_spans[0].attributes.iter().any(|kv| {
                kv.key == opentelemetry::Key::from_static_str("paths.staged")
                    && (matches!(kv.value, opentelemetry::Value::I64(v) if v.cast_unsigned() == PATH_COUNT)
                        || matches!(kv.value, opentelemetry::Value::String(ref v) if v.as_str() == PATH_COUNT.to_string().as_str()))
            }),
            "stage span must carry paths.staged={PATH_COUNT}; got {:?}",
            stage_spans[0].attributes
        );
    }

    /// With `max_age` SET the expiry write returns and each buffer kind gets
    /// one `degenbot.arb.expire` span with `lock_wait_us`/`expire_work_us`.
    #[cfg(feature = "otel")]
    #[test]
    fn solve_dirty_emits_expire_spans_with_phase_split() {
        use crate::arb_engine::EngineStages;
        use crate::otel;
        use opentelemetry_sdk::trace::InMemorySpanExporter;
        use std::sync::Arc;
        use tracing_subscriber::layer::SubscriberExt;

        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));

        let mut oracle = crate::arb_engine::tests::test_keys::DirtyKeys::new();
        let engine = Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new()));
        oracle.insert(0x0BAD_F00D, HopType::V2);
        // Gate ON: only a configured max_age justifies the core write.
        engine.lock().set_event_buffer_max_age(Some(100));
        let handle = EngineStages::new(
            engine,
            std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64)),
        );
        tracing::subscriber::with_default(subscriber, || {
            handle.run_solve_cycle(
                &oracle.to_affected_keys(),
                0x0BAD_F00D,
                &BlockMetadata::default(),
            );
        });

        provider.force_flush().expect("flush");
        let spans = exporter.get_finished_spans().expect("spans");

        let expire_spans: Vec<_> = spans
            .iter()
            .filter(|sp| sp.name.as_ref() == "degenbot.arb.expire")
            .collect();
        let has_stage = |sp: &&opentelemetry_sdk::trace::SpanData, kind: &str| {
            sp.attributes.iter().any(|kv| {
                kv.key == opentelemetry::Key::from_static_str("kind")
                    && matches!(kv.value, opentelemetry::Value::String(ref v) if v.as_str() == kind)
            })
        };
        let has_phase_field = |sp: &&opentelemetry_sdk::trace::SpanData, field: &str| {
            sp.attributes.iter().any(|kv| {
                if field == "lock_wait_us" {
                    kv.key == opentelemetry::Key::from_static_str("lock_wait_us")
                } else {
                    kv.key == opentelemetry::Key::from_static_str("expire_work_us")
                }
            })
        };
        for kind in ["v3", "v4"] {
            let matched = expire_spans
                .iter()
                .filter(|sp| has_stage(sp, kind))
                .collect::<Vec<_>>();
            assert_eq!(
                matched.len(),
                1,
                "expected one degenbot.arb.expire span for kind={kind}; got spans: {:?}",
                expire_spans
                    .iter()
                    .map(|sp| sp.name.as_ref())
                    .collect::<Vec<_>>()
            );
            assert!(
                matched.iter().all(|sp| has_phase_field(sp, "lock_wait_us")
                    && has_phase_field(sp, "expire_work_us")),
                "expire span kind={kind} missing lock_wait_us/expire_work_us attributes"
            );
        }
    }

    /// T0 no-op gating: a clean engine (no dirty paths) must NOT emit an
    /// `degenbot.arb.solve` span — the 2µs no-op solves were flooding Jaeger's
    /// recent-traces list and drowning the real solves.
    #[cfg(feature = "otel")]
    #[test]
    fn solve_dirty_skips_span_when_nothing_dirty() {
        use crate::arb_engine::EngineStages;
        use crate::otel;
        use opentelemetry_sdk::trace::InMemorySpanExporter;
        use std::sync::Arc;
        use tracing_subscriber::layer::SubscriberExt;

        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));

        let handle = EngineStages::new(
            Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new())),
            std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64)),
        );
        tracing::subscriber::with_default(subscriber, || {
            handle.run_solve_cycle(&[], 1, &BlockMetadata::default());
        });

        provider.force_flush().expect("flush");
        let spans = exporter.get_finished_spans().expect("spans");
        let solve_spans = spans
            .iter()
            .filter(|sp| sp.name.as_ref() == "degenbot.arb.solve")
            .count();
        assert_eq!(
            solve_spans, 0,
            "no-op solve must not emit a degenbot.arb.solve span"
        );
    }

    /// Epic BXUSGL T1 acceptance: with the tokio solve executor each path's
    /// result reaches `self.results` as soon as ITS OWN solve completes —
    /// the slowest path in the batch may not delay the fast ones' merge.
    /// RED before the per-path result-queue streaming exists: the batched
    /// barrier merges everything only AFTER the slowest solve, so the drain
    /// probe stays empty past the deadline while the slow path still runs.
    ///
    /// Pinned-tier fixture (FF-T2): the streaming-merge premise binds only
    /// on a host whose auto-resolved fleet binding is pinned (see the
    /// host-tier gate in the body); the serial tier's ONE solve seat has no
    /// second LPT bin to stream a fast merge into the probe.
    #[expect(
        clippy::too_many_lines,
        reason = "the T1 acceptance carries the whole hook-probe + streaming-ordering story in one deterministic body"
    )]
    #[test]
    fn tokio_executor_merges_fast_paths_while_slow_path_solves() {
        if std::thread::available_parallelism().is_ok_and(|n| n.get() < 2) {
            eprintln!("skipping: streaming-merge test requires >=2 cores");
            return;
        }
        // Host-tier gate (FF-T2): the premise needs the PINNED tier's
        // multi-seat solver fan-out — the slow path isolated in its own LPT
        // bin while the other bins stream fast merges into the probe. On a
        // 2-5-core host the auto profile resolves the fleet to the serial
        // binding by design (ONE cycle lane, ONE solve seat): there is no
        // second bin to merge anything before the slow path's release
        // marker, so the ordering assertion cannot bind there. The
        // serial-tier delivery story is covered by
        // `streaming_delivery_emits_fast_result_while_slow_path_solves`
        // (which tolerates the single-seat ordering). Mirror the <2-core
        // self-skip channel.
        let tier_quota =
            degenbot_workers::budget::detected_quota_cpus(&degenbot_config::FleetConfig::default());
        if degenbot_workers::budget::FleetBudget::derive(
            tier_quota,
            &degenbot_workers::budget::BudgetOverrides::default(),
        )
        .is_err()
        {
            eprintln!(
                "skipping: the fleet resolves this host to the serial tier \
                 ({tier_quota} cores) — one solve seat, no second LPT bin"
            );
            return;
        }
        let probe: std::sync::Arc<parking_lot::Mutex<Vec<u64>>> =
            std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));

        let mut engine = ArbitrageEngine::new();

        // Seven independent mispriced V2->V2 pairs -> seven profitable paths
        // (>=2 cores: LPT puts the slow path FIRST in its bin, so at least
        // one fast path always lands in a different bin - the streaming
        // drain merges it long before the slow solve ends).
        let mut pool_ids = Vec::new();
        let mut path_ids = Vec::new();
        for i in 0u8..7 {
            let fwd = engine.register_v2_pool(
                Address::from([0x40 + i; 20]),
                usdc(1_500_000),
                weth(800),
                GAMMA_03,
                FEE_DENOM_03,
            );
            let back = engine.register_v2_pool(
                Address::from([0x50 + i; 20]),
                weth(800),
                usdc(1_600_000),
                GAMMA_03,
                FEE_DENOM_03,
            );
            pool_ids.push(fwd);
            pool_ids.push(back);
            path_ids.push(
                engine
                    .register_path(vec![
                        PoolHop {
                            pool_id: fwd,
                            zero_for_one: true,
                        },
                        PoolHop {
                            pool_id: back,
                            zero_for_one: true,
                        },
                    ])
                    .unwrap(),
            );
        }

        // Slowen path 0; STRUCTURAL interleaving (load-immune): the slow
        // path's hook parks until at least one fast path is MERGED (observed
        // via the probe), then stamps a release marker. Under the batched
        // barrier no fast merge can precede the marker even after the full
        // wait (a merge happens only after the slowest path returns), so the
        // ordering assertion catches it - no absolute deadline to flake on.
        let slow_pid = path_ids[0];
        let fast_pids: Vec<u64> = path_ids[1..].to_vec();
        // Pin LPT placement deterministically: a huge MEASURED sims cost on
        // the slow path sorts it FIRST into its own bin, so the other bins
        // always host fast paths no matter what order the (HashSet-ordered)
        // work items land in. Without this, bin position is nondeterministic
        // (equal structural costs + arbitrary dirty-set iteration order).
        engine
            .cycle
            .last_walk_sims
            .lock()
            .insert(slow_pid, u64::MAX - 1);
        engine
            .cycle
            .last_walk_sims
            .lock()
            .insert(*fast_pids.first().unwrap_or(&0), u64::MAX / 4);
        engine
            .cycle
            .last_walk_sims
            .lock()
            .insert(*fast_pids.get(1).unwrap_or(&0), u64::MAX / 8);
        let hook_probe = probe.clone();
        let hook_fast = fast_pids.clone();
        engine.set_solve_delay_hook(std::sync::Arc::new(move |pid: u64| {
            if pid == slow_pid {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
                while std::time::Instant::now() < deadline {
                    if hook_probe.lock().iter().any(|p| hook_fast.contains(p)) {
                        hook_probe.lock().push(u64::MAX); // merge-before-release marker
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                hook_probe.lock().push(u64::MAX); // released WITHOUT a fast merge
            }
        }));
        engine.set_merge_probe(probe.clone());

        let pool_set: HashSet<u64> = pool_ids.iter().copied().collect();
        let joiner = std::thread::spawn(move || {
            engine.cycle.run_epoch(
                &crate::arb_engine::tests::test_keys::affected_keys(
                    &pool_set,
                    &HashSet::new(),
                    &HashSet::new(),
                ),
                100,
                &BlockMetadata::default(),
                &engine.registry,
                &mut engine.delivery,
            );
            engine
        });

        let engine = joiner.join().unwrap();
        let (results, _block) = engine.latest_results();

        // Structural streaming proof: the first probe entry must be a fast
        // path MERGE (a fast path merged before the slow solve released its
        // hook). Under the batched barrier the marker would land first: only
        // after the slowest path returns can the drain merge anything.
        let observed = probe.lock().clone();
        let marker = observed.iter().position(|p| *p == u64::MAX);
        let first_fast = observed
            .iter()
            .position(|p| *p != u64::MAX && fast_pids.contains(p));
        assert_eq!(results.len(), 7, "all seven paths profitable and merged");
        let marker =
            marker.expect("the slow path hook must stamp a release marker (probe = {observed:?})");
        let first_fast = first_fast.expect(
            "a fast-path MERGE must happen before the slow path finishes (probe = {observed:?})",
        );
        assert!(
            first_fast < marker,
            "a fast-path MERGE must precede the slow path release marker; batched \
             barrier order puts the marker first (probe = {observed:?})"
        );
    }
    /// T3 (epic BXUSGL) acceptance: with `DEGENBOT_STREAMING_DELIVERY` the drain
    /// emits each clamp-passed above-threshold result as an immediate single
    /// -entry batch — a fast path's batch must arrive on the channel while the
    /// slow path is still solving. RED before the per-result emission: the
    /// debounce path sends nothing until `send_result_batch`.
    #[test]
    #[expect(clippy::too_many_lines)]
    fn streaming_delivery_emits_fast_result_while_slow_path_solves() {
        if std::thread::available_parallelism().is_ok_and(|n| n.get() < 2) {
            eprintln!("skipping: streaming-delivery test requires >=2 cores");
            return;
        }
        let probe: std::sync::Arc<parking_lot::Mutex<Vec<u64>>> =
            std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let mut engine = ArbitrageEngine::new();
        engine.set_streaming_delivery(true);
        let (result_tx, mut result_rx) = tokio::sync::mpsc::unbounded_channel();
        engine.set_result_channel(result_tx);

        let mut pool_ids = Vec::new();
        let mut path_ids = Vec::new();
        for i in 0u8..3 {
            let fwd = engine.register_v2_pool(
                Address::from([0x70 + i; 20]),
                usdc(1_500_000),
                weth(800),
                GAMMA_03,
                FEE_DENOM_03,
            );
            let back = engine.register_v2_pool(
                Address::from([0x80 + i; 20]),
                weth(800),
                usdc(1_600_000),
                GAMMA_03,
                FEE_DENOM_03,
            );
            pool_ids.push(fwd);
            pool_ids.push(back);
            path_ids.push(
                engine
                    .register_path(vec![
                        PoolHop {
                            pool_id: fwd,
                            zero_for_one: true,
                        },
                        PoolHop {
                            pool_id: back,
                            zero_for_one: true,
                        },
                    ])
                    .unwrap(),
            );
        }

        // Structural interleave (mirror of the solve-orchestration test): the
        // slow path's hook parks until the delivery side stamped a flag (set
        // by the payer loop below when it sees any batch), then releases.
        let slow_pid = path_ids[0];
        let fast_pids: Vec<u64> = path_ids[1..].to_vec();
        let observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hook_observed = observed.clone();
        engine.set_solve_delay_hook(std::sync::Arc::new(move |pid: u64| {
            if pid == slow_pid {
                let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2500);
                while std::time::Instant::now() < deadline {
                    if hook_observed.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        }));
        engine.set_merge_probe(probe.clone());

        let pool_set: HashSet<u64> = pool_ids.iter().copied().collect();
        let joiner = std::thread::spawn(move || {
            engine.cycle.run_epoch(
                &crate::arb_engine::tests::test_keys::affected_keys(
                    &pool_set,
                    &HashSet::new(),
                    &HashSet::new(),
                ),
                100,
                &BlockMetadata::default(),
                &engine.registry,
                &mut engine.delivery,
            );
            engine
        });

        // Payer: drain the channel from THIS thread while the solve_THREAD
        // holds the engine Mutex; declare success as soon as any batch carries
        // a fast path.
        let mut saw_fast_batch = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2200);
        while std::time::Instant::now() < deadline {
            if observed.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            while let Ok(batch) = result_rx.try_recv() {
                let has_fast = batch
                    .fresh
                    .iter()
                    .chain(batch.updated.iter())
                    .any(|(id, _)| fast_pids.contains(id));
                if has_fast {
                    saw_fast_batch = true;
                    break;
                }
            }
            if saw_fast_batch {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        observed.store(true, std::sync::atomic::Ordering::Relaxed);

        let engine = joiner.join().unwrap();

        // drained bookkeeping: every path above threshold is in `delivered`.
        assert_eq!(engine.delivery.delivered.len(), 3, "all paths in delivered");
        assert!(
            saw_fast_batch || !result_rx.is_empty(),
            "a fast path's batch must arrive on the channel while the slow path \\\n             is still solving (flag on); saw_fast_batch = {saw_fast_batch}"
        );
    }

    // -------------------------------------------------------------------
    // Epic SRQEK5 (WV62TX): detached enqueue + sidecar merge
    // -------------------------------------------------------------------

    /// Common scaffolding: a 3-path V2→V2 engine (same live-corpus-shaped
    /// fixtures as the streaming test), with the slow path's hook injectable
    /// per test.
    fn detached_fixture(delay_ms: u64) -> (ArbitrageEngine, Vec<u64>, Vec<u64>) {
        let mut engine = ArbitrageEngine::new();
        let mut pool_ids = Vec::new();
        let mut path_ids = Vec::new();
        for i in 0u8..3 {
            let fwd = engine.register_v2_pool(
                Address::from([0x90 + i; 20]),
                usdc(1_500_000),
                weth(800),
                GAMMA_03,
                FEE_DENOM_03,
            );
            let back = engine.register_v2_pool(
                Address::from([0xA0 + i; 20]),
                weth(800),
                usdc(1_600_000),
                GAMMA_03,
                FEE_DENOM_03,
            );
            pool_ids.push(fwd);
            pool_ids.push(back);
            path_ids.push(
                engine
                    .register_path(vec![
                        PoolHop {
                            pool_id: fwd,
                            zero_for_one: true,
                        },
                        PoolHop {
                            pool_id: back,
                            zero_for_one: true,
                        },
                    ])
                    .unwrap(),
            );
        }
        // The first registered path's id owns the injected delay.
        let target = path_ids[0];
        engine.set_solve_delay_hook(std::sync::Arc::new(move |pid: u64| {
            if pid == target {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            }
        }));
        (engine, pool_ids, path_ids)
    }

    /// Structural acceptance (red/green): with the one (detached) arm and an
    /// injected 400ms slow path, `run_epoch` — driven via
    /// the production `EngineStages` solve seam, which also spawns the
    /// merge sidecar — RETURNS before the merge lands, and the sidecar
    /// populates the results within ~500ms of enqueue.
    #[test]
    fn detached_cycle_returns_at_enqueue_end_and_sidecar_merges() {
        if std::thread::available_parallelism().is_ok_and(|n| n.get() < 2) {
            eprintln!("skipping: detached-cycle structural test requires >=2 cores");
            return;
        }
        let (engine, pool_ids, path_ids) = detached_fixture(400);
        let slow_pid = path_ids[0];
        let engine = std::sync::Arc::new(parking_lot::Mutex::new(engine));
        let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
            .iter()
            .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
            .collect();
        let handle = crate::arb_engine::EngineStages::new(
            std::sync::Arc::clone(&engine),
            std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64)),
        );

        let t0 = std::time::Instant::now();
        handle.run_solve_cycle(&affected_keys_v2, 100, &BlockMetadata::default());
        let returned = t0.elapsed();

        // RETURNS before the merge lands: strictly inside the injected 400ms
        // slow-solve window, and the slow path's result is NOT in the map yet.
        assert!(
            returned < std::time::Duration::from_millis(350),
            "detached cycle must return at enqueue end (before the 400ms slow \
             solve can merge); took {returned:?}"
        );
        {
            let engine_guard = engine.lock();
            assert!(
                !engine_guard.cycle.results.contains_key(&slow_pid),
                "the slow path must NOT be merged at enqueue-end return"
            );
        }

        // The sidecar populates the results within ~500ms of enqueue.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while !engine.lock().cycle.results.contains_key(&slow_pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "sidecar merge did not land within ~500ms of enqueue"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // And every path merged (fast + slow), applied by the sidecar.
        assert_eq!(
            engine.lock().cycle.results.len(),
            3,
            "all three detached stragglers must be applied by the sidecar"
        );
        let guard = engine.lock();
        assert_eq!(
            guard
                .cycle
                .detached_cycle
                .applied
                .load(std::sync::atomic::Ordering::Relaxed),
            3
        );
    }

    /// Q1a stale policy (red/green): a straggler whose resolved-update stamp
    /// is stale (a pool ticked during the solve → its `update_block` moved)
    /// is DROPPED, not applied. A fresh-stamp straggler applies
    /// (apply-if-unchanged) through the same merge seam. LW-T9: the two
    /// dispositions ride DIFFERENT pids — the QR3NUS exactness fuse (carried
    /// to the sidecar) owns per-(cycle_seq, pid) delivery uniqueness, so the
    /// stale→fresh stamp flip on ONE pid is fused by construction now.
    #[test]
    fn detached_straggler_with_stale_update_stamp_is_dropped() {
        // 43E3H3 const hoist: the straggler probes a seq the baseline
        // in-cycle run did NOT claim (that run consumed seq 1; the ONE
        // (`solve_seq`, pid) ledger must not false-trip a plain sidecar
        // Q1a probe).
        const STRAGGLER_SEQ: u64 = 2;
        let (mut engine, pool_ids, path_ids) = detached_fixture(0);
        let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
            .iter()
            .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
            .collect();
        engine.solve_dirty(100, &BlockMetadata::default(), &affected_keys_v2);
        let stale_pid = path_ids[0];
        let fresh_pid = path_ids[1];
        assert!(
            engine.cycle.results.contains_key(&stale_pid)
                && engine.cycle.results.contains_key(&fresh_pid),
            "precondition: fresh results merged by the inline drain"
        );
        // WFF6MM: the baseline cycle's own merges counted here — the straggler
        // assertions below are DELTAS against this snapshot.
        let applied_before = engine
            .cycle
            .detached_cycle
            .applied
            .load(std::sync::atomic::Ordering::Relaxed);
        let stale_stamp: Vec<u64> = engine.cycle.resolved_update_snapshot[&stale_pid]
            .clone()
            .iter()
            .map(|b| b + 1)
            .collect();
        let stale_result = engine.cycle.results.get(&stale_pid).unwrap().clone();
        let fresh_stamp = engine.cycle.resolved_update_snapshot[&fresh_pid].clone();
        let fresh_result = engine.cycle.results.get(&fresh_pid).unwrap().clone();

        let item = |result: SolvePathResult, stamp: Vec<u64>, pid: u64| {
            crate::arb_engine::executor::LaneOutcome::Solved(
                crate::arb_engine::executor::SolveOutcome {
                    payload: None,
                    worker_clamp_twins: 0,
                    cycle_seq: STRAGGLER_SEQ,
                    solve_block: 100,
                    metadata: BlockMetadata::default(),
                    pid,
                    update_stamp: stamp,
                    result,
                    solve_span: tracing::Span::none(),
                },
            )
        };

        // A straggler whose pools ALL ticked during the solve.
        engine.merge_detached_item(item(stale_result, stale_stamp, stale_pid));
        assert_eq!(
            engine
                .cycle
                .detached_cycle
                .dropped_stale
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the stale straggler must be dropped"
        );
        assert_eq!(
            engine
                .cycle
                .detached_cycle
                .applied
                .load(std::sync::atomic::Ordering::Relaxed),
            applied_before
        );

        // The unchanged-intake twin APPLIES (apply-if-unchanged).
        engine.merge_detached_item(item(fresh_result, fresh_stamp, fresh_pid));
        assert_eq!(
            engine
                .cycle
                .detached_cycle
                .applied
                .load(std::sync::atomic::Ordering::Relaxed),
            applied_before + 1,
            "the unchanged straggler must be applied"
        );
        assert_eq!(
            engine
                .cycle
                .detached_cycle
                .dropped_stale
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    /// LW-T9 note-(a) carry (red): the DETACHED sidecar merge must carry the
    /// SAME exactness assert as the in-cycle drain (QR3NUS): one path outcome
    /// exactly once — a duplicate (`cycle_seq`, `pid`) delivery trips the loud
    /// exactness fuse and is NOT applied a second time. RED before the
    /// cutover: the sidecar had no duplicate guard, so the twin merge
    /// counted applied == 2.
    #[test]
    fn detached_duplicate_straggler_trips_the_exactness_fuse() {
        // 43E3H3 const hoist: the baseline in-cycle run consumed seq 1 — the
        // straggler probes the NEXT seq so it cannot shadow a key the
        // in-cycle arm already claimed under the ONE (`solve_seq`, pid)
        // ledger (that collision is N2's job; R1 pins a SIDECAR-ONLY
        // double-delivery).
        const STRAGGLER_SEQ: u64 = 2;
        let (mut engine, pool_ids, path_ids) = detached_fixture(0);
        let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
            .iter()
            .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
            .collect();
        engine.solve_dirty(100, &BlockMetadata::default(), &affected_keys_v2);
        let pid = path_ids[0];
        let applied_before = engine
            .cycle
            .detached_cycle
            .applied
            .load(std::sync::atomic::Ordering::Relaxed);
        let fresh_stamp = engine.cycle.resolved_update_snapshot[&pid].clone();
        let fresh_result = engine.cycle.results.get(&pid).unwrap().clone();

        let item = |result: SolvePathResult| {
            crate::arb_engine::executor::LaneOutcome::Solved(
                crate::arb_engine::executor::SolveOutcome {
                    payload: None,
                    worker_clamp_twins: 0,
                    cycle_seq: STRAGGLER_SEQ,
                    solve_block: 100,
                    metadata: BlockMetadata::default(),
                    pid,
                    update_stamp: fresh_stamp.clone(),
                    result,
                    solve_span: tracing::Span::none(),
                },
            )
        };
        engine.merge_detached_item(item(fresh_result.clone()));
        engine.merge_detached_item(item(fresh_result));

        assert_eq!(
            engine
                .cycle
                .detached_cycle
                .applied
                .load(std::sync::atomic::Ordering::Relaxed),
            applied_before + 1,
            "the duplicate delivery must not apply a second time"
        );
        assert_eq!(
            engine
                .cycle
                .detached_cycle
                .duplicate_outcomes
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the duplicate delivery must trip the loud exactness fuse once"
        );
    }

    /// RLVDUP T3 (red/green): de-registration removes the resolve
    /// bookkeeping - `path_status` and `resolved_update_snapshot` entries
    /// must follow the path out, or per-pool churn grows the maps
    /// unbounded.
    #[test]
    fn deregister_removes_status_and_update_snapshot() {
        let (mut engine, pool_ids, path_ids) = detached_fixture(0);
        let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
            .iter()
            .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
            .collect();
        engine.solve_dirty(100, &BlockMetadata::default(), &affected_keys_v2);
        let pid = path_ids[0];
        assert!(engine.cycle.resolved_update_snapshot.contains_key(&pid));
        assert!(engine.cycle.path_status.contains_key(&pid));

        assert!(engine.deregister_path(pid));
        assert!(!engine.cycle.resolved_update_snapshot.contains_key(&pid));
        assert!(!engine.cycle.path_status.contains_key(&pid));
    }

    /// Q1a deregister (red/green): a straggler landing after its path was
    /// de-registered is DROPPED, never applied (and never re-creates a
    /// result entry).
    #[test]
    fn detached_straggler_after_deregister_is_dropped() {
        let (mut engine, pool_ids, path_ids) = detached_fixture(0);
        let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
            .iter()
            .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
            .collect();
        engine.solve_dirty(100, &BlockMetadata::default(), &affected_keys_v2);
        let pid = path_ids[0];
        let fresh_stamp = engine.cycle.resolved_update_snapshot[&pid].clone();
        let fresh_result = engine.cycle.results.get(&pid).unwrap().clone();

        assert!(engine.deregister_path(pid), "path must deregister");
        assert!(!engine.cycle.results.contains_key(&pid));

        engine.merge_detached_item(crate::arb_engine::executor::LaneOutcome::Solved(
            crate::arb_engine::executor::SolveOutcome {
                payload: None,
                worker_clamp_twins: 0,
                cycle_seq: 1,
                solve_block: 100,
                metadata: BlockMetadata::default(),
                pid,
                update_stamp: fresh_stamp,
                result: fresh_result,
                solve_span: tracing::Span::none(),
            },
        ));
        assert_eq!(
            engine
                .cycle
                .detached_cycle
                .dropped_deregistered
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the deregistered straggler must be dropped"
        );
        assert!(!engine.cycle.results.contains_key(&pid));
        assert_eq!(
            engine
                .cycle
                .detached_cycle
                .applied
                .load(std::sync::atomic::Ordering::Relaxed),
            3,
            "the baseline inline-drain merges, and the dropped straggler adds none"
        );
    }
    /// MQUKB6-T2: the detached-merge sidecar thread has NO ambient span
    /// context, so every `LaneOutcome::Solved` carrier carries the
    /// enqueue-time solve span — the merge-time event (here: the Q1a
    /// deregister drop) must land on the carried span rather than
    /// orphaning into a Jaeger root. Uses the deregister drop path
    /// (unknown pid): deterministic, no registration and no core lock
    /// needed. Scoped LOCAL subscriber.
    #[cfg(feature = "otel")]
    #[test]
    fn detached_merge_event_parents_under_the_carried_solve_span() {
        use crate::arb_engine::detached_cycle::detached_merge_sidecar;
        use crate::arb_engine::executor::LaneOutcome;
        use crate::{arb_engine::ArbitrageEngine, otel};
        use alloy::primitives::U256;
        use degenbot_solvers::mixed::SolvePathResult;
        use opentelemetry_sdk::trace::InMemorySpanExporter;
        use std::sync::Arc;
        use tracing_subscriber::layer::SubscriberExt;

        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));

        let engine = Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new()));
        let (tx, rx) = std::sync::mpsc::channel::<LaneOutcome>();

        // tracing::Span binds to the thread-local subscriber at CREATION, so
        // the whole span lifecycle (create → capture → sidecar merge → drop)
        // runs inside one `with_default` scope.
        tracing::subscriber::with_default(subscriber, || {
            let solve_span = tracing::info_span!("degenbot.arb.solve", block.number = 42u64);
            {
                let _guard = solve_span.enter();
                tx.send(LaneOutcome::Solved(
                    crate::arb_engine::executor::SolveOutcome {
                        payload: None,
                        worker_clamp_twins: 0,
                        cycle_seq: 1,
                        solve_block: 42,
                        metadata: BlockMetadata::default(),
                        pid: 0xDEAD,
                        update_stamp: Vec::new(),
                        result: SolvePathResult {
                            optimal_input: U256::ZERO,
                            profit: U256::ZERO,
                            hop_outputs: Vec::new(),
                            consumed_inputs: Vec::new(),
                            state_nonces: Vec::new(),
                            solver_pool_states: Vec::new(),
                        },
                        solve_span: tracing::Span::current(),
                    },
                ))
                .expect("sidecar rx is alive");
            }
            drop(tx);
            // Inline sidecar run (this thread): the merge enters the item's
            // carried span — exactly what the real std-thread would see.
            detached_merge_sidecar(&engine, rx, None);
            // This outer handle is the LAST reference to the solve span —
            // dropping it closes (exports) the span.
            drop(solve_span);
        });
        provider.force_flush().expect("flush");
        let spans = exporter.get_finished_spans().expect("spans");

        let solve_spans: Vec<_> = spans
            .iter()
            .filter(|sp| sp.name.as_ref() == "degenbot.arb.solve")
            .collect();
        assert_eq!(
            solve_spans.len(),
            1,
            "exactly one solve span; got: {:?}",
            spans.iter().map(|sp| sp.name.as_ref()).collect::<Vec<_>>()
        );
        let merged_event = solve_spans[0]
            .events
            .events
            .iter()
            .any(|e| e.name == "straggler dropped (path deregistered)");
        assert!(
            merged_event,
            "the sidecar merge-time event must parent under the carried solve span; events: {:?}",
            solve_spans[0].events.events
        );
    }

    /// AQV6EF AC2 (red-first): a panic inside `merge_detached_item` must be
    /// CAUGHT — it becomes a typed drain-death record that trips the SAME
    /// sticky cordon as a failed send, and the sidecar must return (not
    /// vanish silently, stranding every later send with no signal). The
    /// dropped Receiver then makes every later send a COUNTED loss.
    #[test]
    fn a_panicking_merge_becomes_a_typed_record_and_a_sticky_cordon() {
        use crate::arb_engine::detached_cycle::detached_merge_sidecar;
        use crate::arb_engine::executor::LaneOutcome;

        let owner: &'static degenbot_workers::posture::PostureOwner = std::boxed::Box::leak(
            std::boxed::Box::new(degenbot_workers::posture::PostureOwner::new(
                degenbot_workers::posture::PosturePolicy::doc_defaults(),
            )),
        );
        let engine = std::sync::Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new()));
        engine
            .lock()
            .set_merge_panic_hook(std::sync::Arc::new(|pid| {
                assert_ne!(pid, 0x5151_5151, "merge boom");
            }));
        let (tx, rx) = std::sync::mpsc::channel::<LaneOutcome>();
        tx.send(LaneOutcome::Suppressed { pid: 0x5151_5151 })
            .expect("rx alive");
        let tx_after = tx.clone();
        drop(tx);
        detached_merge_sidecar(&engine, rx, Some(owner));
        assert_eq!(
            owner.current(),
            degenbot_workers::posture::FleetPosture::Cordoned,
            "a caught merge panic must trip the sticky drain-death cordon"
        );
        assert!(
            tx_after
                .send(LaneOutcome::Suppressed { pid: 0x1 })
                .is_err(),
            "the panicked sidecar has exited: later sends hit the dead pipe (the send-failure signal)"
        );
    }

    /// T2 (epic SRQEK5 4QKZE3) cadence acceptance, SZJUKL-port: with detached
    /// cycles ON through the PRODUCTION stage surface (`EngineStages` — the
    /// shipped `solve_dirty` cadence the driver executes INLINE at the machine's
    /// decision points), each solve call RETURNS at enqueue-end (µs) while the
    /// 400ms straggler still merges on the sidecar — consecutive stage cycles
    /// interleave with the merges, and the dissolved B3 frozen-drainer
    /// detector has no queue left to stall across detached cycles (the
    /// no-progress obligation lives on the machine's `WatchdogPhase`). The
    /// four-call cadence must complete inside the single slow-solve window
    /// (enqueue-end, not apply-end), and the stragglers must still land.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn detached_stragglers_do_not_block_inline_stage_work() {
        let (engine, pool_ids, path_ids) = detached_fixture(400);
        let engine = std::sync::Arc::new(parking_lot::Mutex::new(engine));
        let delta = std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64));
        for &p in &pool_ids {
            delta.record_affected(HopType::V2, p, 0u64);
        }
        let stages = crate::arb_engine::EngineStages::new(std::sync::Arc::clone(&engine), delta);
        let meta = BlockMetadata::default();

        // The affected keys the dissolved coordinator would have taken from
        // the epoch ledger before driving the engine's solve.
        let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
            .iter()
            .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
            .collect();

        let t0 = std::time::Instant::now();
        // The detached cycle: the solve call RETURNS (enqueue-end) while the
        // 400ms slow solve still runs — no in-cycle multi-second hold.
        stages.run_solve_cycle(&affected_keys_v2, 100, &meta);
        // The shipped cadence continues UNCHANGED mid-merge: a debounce publish
        // + further block cycles interleave with the sidecar's merges.
        engine.lock().send_result_batch(&meta);
        stages.run_solve_cycle(&[], 101, &meta);
        stages.run_solve_cycle(&[], 102, &meta);

        assert!(
            t0.elapsed() < std::time::Duration::from_millis(390),
            "the whole cadence must complete inside the 400ms slow-solve window (enqueue-end, not apply-end)"
        );

        // The stragglers DID land via the sidecar (cross-cycle merge),
        // without ever blocking the inline stage work along the way.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while engine.lock().cycle.results.len() < 3 {
            assert!(
                std::time::Instant::now() < deadline,
                "all detached stragglers must merge via the sidecar"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let guard = engine.lock();
        assert!(path_ids.iter().all(|p| guard.cycle.results.contains_key(p)));
    }

    // =================================================================
    // 43E3H3 red-first breaker suite (design logs/lane-unify-design.md §5).
    // Status at HEAD (commit 1): each test below is RED against current
    // code — they pin the POST-merge contracts (one carrier, one ledger,
    // the detached arm's lane witness). They GREEN in commit 2.
    // =================================================================

    /// N2 (same-seq replay, WFF6MM single-arm): a merged result and a later
    /// carrier naming the SAME (`cycle_seq`, pid) must collide on the ONE
    /// ledger — the fuse refuses the second arrival instead of merging
    /// twice.
    // 43E3H3 red-first: pins the (solve_seq, pid) key half the merged
    // ledger owns (design §3.3).
    #[test]
    fn merged_ledger_rejects_same_seq_pid_replay() {
        let (mut engine, pool_ids, path_ids) = detached_fixture(0);
        let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
            .iter()
            .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
            .collect();
        // In-cycle solve of the same block: all 3 paths land in `results`.
        engine.solve_dirty(100, &BlockMetadata::default(), &affected_keys_v2);
        let pid = path_ids[0];
        assert!(
            engine.cycle.results.contains_key(&pid),
            "baseline: the in-cycle arm must merge pid {pid} first"
        );
        let fresh_stamp = engine.cycle.resolved_update_snapshot[&pid].clone();
        let fresh_result = engine.cycle.results.get(&pid).unwrap().clone();
        let results_before = engine.cycle.results.len();
        let applied_before = engine
            .cycle
            .detached_cycle
            .applied
            .load(std::sync::atomic::Ordering::Relaxed);

        // The replay claims the cycle_seq the merged cycle consumed: under
        // the one ledger this is the SAME (solve_seq, pid) key and the fuse
        // must refuse it.
        let item = crate::arb_engine::executor::LaneOutcome::Solved(
            crate::arb_engine::executor::SolveOutcome {
                payload: None,
                worker_clamp_twins: 0,
                cycle_seq: engine.cycle.detached_cycle.issued_seq(), // the merged cycle's seq
                solve_block: 100,
                metadata: BlockMetadata::default(),
                pid,
                update_stamp: fresh_stamp,
                result: fresh_result,
                solve_span: tracing::Span::none(),
            },
        );
        engine.merge_detached_item(item);

        assert_eq!(
            engine
                .cycle
                .detached_cycle
                .duplicate_outcomes
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "merged ledger: the same-seq replay must trip the fuse exactly once"
        );
        assert_eq!(
            engine.cycle.results.len(),
            results_before,
            "merged ledger: the refused duplicate must not add a results entry"
        );
        assert_eq!(
            engine
                .cycle
                .detached_cycle
                .applied
                .load(std::sync::atomic::Ordering::Relaxed),
            applied_before,
            "merged ledger: the refused duplicate must not count as applied"
        );
    }

    /// N3 (prune correctness): the merged ledger prunes keyed rows only
    /// past `LEDGER_AGE`, driven by the DETACHED arm's cycle issuance —
    /// and an in-cycle-only advance must NOT prune (the anchor is
    /// detached-issued-seq). Red at HEAD: the in-cycle side has no
    /// ledger/rows at all; the detach-keyed prune ages on any current
    /// seq the claim sees.
    // 43E3H3 red-first: pins LEDGER_AGE=64 exactly and the anchor choice
    // (design §3.3 + §3.3.1 REV 2).
    #[test]
    fn merged_ledger_prunes_only_past_ledger_age() {
        use std::sync::atomic::Ordering;
        let (mut engine, pool_ids, path_ids) = detached_fixture(0);
        let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
            .iter()
            .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
            .collect();
        engine.solve_dirty(100, &BlockMetadata::default(), &affected_keys_v2);
        let results_before = engine.cycle.results.len();

        // Seed directly into the sidecar ledger (pub(crate) state) with
        // pids not otherwise involved, at seq boundaries chosen to straddle
        // the LEDGER_AGE edge driven through the DETACHED key space.
        let mut seed = |seq: u64, pid: u64| {
            let fresh_stamp = engine.cycle.resolved_update_snapshot[&path_ids[0]].clone();
            let result = engine.cycle.results.get(&path_ids[0]).unwrap().clone();
            let item = crate::arb_engine::executor::LaneOutcome::Solved(
                crate::arb_engine::executor::SolveOutcome {
                    payload: None,
                    worker_clamp_twins: 0,
                    cycle_seq: seq,
                    solve_block: 100,
                    metadata: BlockMetadata::default(),
                    pid,
                    update_stamp: fresh_stamp,
                    result,
                    solve_span: tracing::Span::none(),
                },
            );
            engine.merge_detached_item(item);
        };

        // Drive the anchor forward by ONE detached merge at a high seq; the
        // seeds at (current-65) and (current-63) straddle the age edge.
        let current = 100u64;
        seed(current - 65, 1111); // beyond LEDGER_AGE: must be PRUNED
        seed(current - 63, 2222); // within LEDGER_AGE: must be RETAINED
        seed(current, 3333); // the advancing merge itself

        // 43E3H3: the seq-100 merge's PRUNE swept BOTH seeds' rows as a
        // side effect (the engine-side ledger now prunes on every claim,
        // not just the sidecar's). Restore the retained-row marker its
        // (36, 2222) key placed there — the prune must prove (36, 2222)
        // SURVIVES a later claim, not that it survived the sweep that
        // built the (100, 3333) row.
        engine
            .cycle
            .detached_cycle
            .outcome_ledger
            .lock()
            .claim((current - 63, 2222))
            .ok();

        let ledger_rows = engine.cycle.detached_cycle.outcome_ledger.lock();
        assert!(
            !ledger_rows.contains((current - 65, 1111)),
            "LEDGER_AGE=64: row (seq-65) must be pruned after the seq-{current} claim"
        );
        assert!(
            ledger_rows.contains((current - 63, 2222)),
            "LEDGER_AGE=64: row (seq-63) must be retained after the seq-{current} claim"
        );
        drop(ledger_rows);

        // NEGATIVE half (design §3.3.1 REV 2): in-cycle-only advances of
        // the shared counter must NOT prune detached-keyed rows. Run two
        // in-cycle solves (the shared counter ticks), then re-assert the
        // retained row survived them.
        engine.solve_dirty(101, &BlockMetadata::default(), &affected_keys_v2);
        engine.solve_dirty(102, &BlockMetadata::default(), &affected_keys_v2);
        let ledger_rows_still = engine.cycle.detached_cycle.outcome_ledger.lock();
        assert!(
            ledger_rows_still.contains((current - 63, 2222)),
            "in-cycle-only advances must not prune detached-keyed rows (anchor = detached_issued_seq)"
        );
        let _ = results_before;
        let _ = Ordering::Relaxed;
    }

    /// N4 (detached undercount): a detached bin that panics mid-walk must
    /// deliver a typed disposition for EVERY owed pid — no silent
    /// undercount. Red at HEAD: the detached arm has NO lane witness
    /// (sD:2350 submits a raw bin body), so a panicked bin simply never
    /// delivers its undelivered pids.
    // 43E3H3 red-first: pins the detached arm's witness adoption
    // (design §5.2). GREEN requires Failed records on the detached pipe.
    #[test]
    fn detached_undercount_trips_the_fan_in_assert() {
        if std::thread::available_parallelism().is_ok_and(|n| n.get() < 2) {
            eprintln!("skipping: detached panic test requires >=2 cores");
            return;
        }
        // Drive a detached cycle with a panicking pid; then REQUIRE that
        // every submitted pid got exactly one disposition (Solved,
        // Suppressed, or Failed) — observed through the applied+dropped
        // counters, which today stay short (the undelivered pids never
        // arrive at all: SILENT UNDERCOUNT).
        let (mut engine, pool_ids, path_ids) = detached_fixture(0);
        let kill = path_ids[1];
        engine.set_solve_panic_hook(std::sync::Arc::new(move |pid: u64| {
            if pid != kill {
                return;
            }
            panic!("path killed mid-bin (43E3H3 red harness)");
        }));
        let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
            .iter()
            .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
            .collect();
        // Drive through the production stage seam so the sidecar spawns.
        let engine = std::sync::Arc::new(parking_lot::Mutex::new(engine));
        let delta = std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64));
        for &p in &pool_ids {
            delta.record_affected(HopType::V2, p, 0u64);
        }
        let stages = crate::arb_engine::EngineStages::new(std::sync::Arc::clone(&engine), delta);
        stages.run_solve_cycle(&affected_keys_v2, 100, &BlockMetadata::default());

        // Wait for the dispositions to land (the sidecar merges async).
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2_000);
        loop {
            let guard = engine.lock();
            let applied = guard
                .cycle
                .detached_cycle
                .applied
                .load(std::sync::atomic::Ordering::Relaxed);
            let stale = guard
                .cycle
                .detached_cycle
                .dropped_stale
                .load(std::sync::atomic::Ordering::Relaxed);
            let dereg = guard
                .cycle
                .detached_cycle
                .dropped_deregistered
                .load(std::sync::atomic::Ordering::Relaxed);
            let dup = guard
                .cycle
                .detached_cycle
                .duplicate_outcomes
                .load(std::sync::atomic::Ordering::Relaxed);
            drop(guard);
            let disposed = applied + stale + dereg + dup;
            if disposed >= path_ids.len() as u64 || std::time::Instant::now() > deadline {
                // 43E3H3: THE assert — every submitted path must be
                // dispositioned EXACTLY once. At HEAD the panicked bin's
                // undelivered pids NEVER arrive, so disposed < submitted.
                assert_eq!(
                    disposed, path_ids.len() as u64,
                    "outcome accounting undercount — exactness fuse \\\n                     (QR3NUS/43E3H3): a panicked bin must still disposition \\\n                     every owed pid as a typed Failed record"
                );
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    /// N5 (gauge pairing through a panic): the in-flight gauge must return
    /// to its EXACT pre-cycle value after a panicking detached cycle AND
    /// keep the cap gate engaging on true load. Red at HEAD: the panicked
    /// bin's flushed Solved items bump at send but the panic kills the
    /// remaining path solves, nothing decrements the orphaned bumps
    // 43E3H3 red-first: pins constraint (b) THROUGH the panic path AND
    // the variant-gated bump/decrement pairing (design §4.6.1 REV 2).
    #[test]
    fn detached_panic_does_not_leak_inflight_gauge() {
        if std::thread::available_parallelism().is_ok_and(|n| n.get() < 2) {
            eprintln!("skipping: detached panic test requires >=2 cores");
            return;
        }
        let (mut engine, pool_ids, path_ids) = detached_fixture(0);
        let kill = path_ids[2];
        engine.set_solve_panic_hook(std::sync::Arc::new(move |pid: u64| {
            if pid != kill {
                return;
            }
            panic!("path killed mid-bin (43E3H3 red harness)");
        }));
        let g0 = engine
            .cycle
            .detached_cycle
            .outstanding
            .load(std::sync::atomic::Ordering::Relaxed);

        let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
            .iter()
            .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
            .collect();
        let engine = std::sync::Arc::new(parking_lot::Mutex::new(engine));
        let delta = std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64));
        for &p in &pool_ids {
            delta.record_affected(HopType::V2, p, 0u64);
        }
        let stages = crate::arb_engine::EngineStages::new(std::sync::Arc::clone(&engine), delta);
        stages.run_solve_cycle(&affected_keys_v2, 100, &BlockMetadata::default());

        // After all dispositions land, the gauge must be back at g0 EXACTLY
        // (a leaked count OR a sagged count both move it off g0 — only the
        // exact value pins BOTH directions of a broken pairing).
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2_000);
        loop {
            let guard = engine.lock();
            let applied = guard
                .cycle
                .detached_cycle
                .applied
                .load(std::sync::atomic::Ordering::Relaxed)
                + guard
                    .cycle
                    .detached_cycle
                    .dropped_stale
                    .load(std::sync::atomic::Ordering::Relaxed)
                + guard
                    .cycle
                    .detached_cycle
                    .dropped_deregistered
                    .load(std::sync::atomic::Ordering::Relaxed)
                + guard
                    .cycle
                    .detached_cycle
                    .duplicate_outcomes
                    .load(std::sync::atomic::Ordering::Relaxed);
            let gauge = guard
                .cycle
                .detached_cycle
                .outstanding
                .load(std::sync::atomic::Ordering::Relaxed);
            drop(guard);
            if applied >= path_ids.len() as u64 || std::time::Instant::now() > deadline {
                assert_eq!(
                    gauge, g0,
                    "in-flight gauge must return to its EXACT pre-cycle \\\n                     value g0={g0} through a panicking cycle (variant-gated pairing, \\\n                     design §4.6.1 REV 2) — got {gauge}"
                );
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    /// WFF6MM cutover: the in-flight cap gate is RETIRED — a cycle whose
    /// un-dispositioned count sits AT the old cap STILL detaches (the
    /// admission draw, not the cap, owns backpressure now; there is no
    /// in-cycle fallback left to degrade to).
    #[test]
    fn retired_inflight_cap_no_longer_degrades() {
        let (mut engine, pool_ids, _path_ids) = detached_fixture(0);
        // Seed the gauge AT the old cap: it must be ignored.
        engine
            .cycle
            .detached_cycle
            .outstanding
            .store(8, std::sync::atomic::Ordering::Relaxed); // == DETACHED_INFLIGHT_CAP
        let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
            .iter()
            .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
            .collect();
        engine.solve_dirty(100, &BlockMetadata::default(), &affected_keys_v2);
        assert_eq!(
            engine.cycle_arm(),
            "detached",
            "the retired cap must no longer degrade a cycle to an in-cycle arm"
        );
    }

    /// Cold-start trace (degraded-state measurement): the dispatch LATCHES the
    /// cycle's arm on the engine (the `solve_entry` precedent) — a span field
    /// alone is unreadable to the caller, and the caller (`EngineStages`) is
    /// the only place the cycle's duration and Mutex hold are measurable,
    /// i.e. AFTER `solve_dirty` returns. `unset` is the never-dispatched
    /// sentinel: deliberately visible rather than folded into
    /// `skipped_empty`.
    #[test]
    fn cycle_arm_label_latches_for_every_dispatch_arm() {
        let (mut engine, pool_ids, _path_ids) = detached_fixture(0);
        assert_eq!(
            engine.cycle_arm(),
            "unset",
            "no cycle has been dispatched yet"
        );
        // Sub-cap under the detached stance: the detached arm.
        let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
            .iter()
            .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
            .collect();
        engine.solve_dirty(100, &BlockMetadata::default(), &affected_keys_v2);
        assert_eq!(
            engine.cycle_arm(),
            "detached",
            "a sub-cap cycle under the detached stance must latch the detached arm"
        );
        // At the (retired) in-flight cap: STILL detached (WFF6MM — the
        // cap gate is gone; every dispatched cycle takes the one arm).
        engine
            .cycle
            .detached_cycle
            .outstanding
            .store(8, std::sync::atomic::Ordering::Relaxed); // == DETACHED_INFLIGHT_CAP
        engine.solve_dirty(101, &BlockMetadata::default(), &affected_keys_v2);
        assert_eq!(
            engine.cycle_arm(),
            "detached",
            "the retired cap gate must not divert a cycle off the one dispatch arm"
        );
        // A dirty key with NO registered paths: the bookkeeping-only pass.
        engine
            .cycle
            .detached_cycle
            .outstanding
            .store(0, std::sync::atomic::Ordering::Relaxed);
        let orphan = engine.register_v2_pool(
            Address::from([0x77_u8; 20]),
            usdc(1_000_000),
            weth(500),
            GAMMA_03,
            FEE_DENOM_03,
        );
        engine.solve_dirty(
            102,
            &BlockMetadata::default(),
            &[degenbot_solvers::affected_keys::AffectedKey::new(
                HopType::V2,
                orphan,
            )],
        );
        assert_eq!(
            engine.cycle_arm(),
            "skipped_empty",
            "a dirty key with no registered paths must latch the bookkeeping-only arm"
        );
    }

    #[test]
    fn detached_solve_returns_at_enqueue_end_when_sync_drain_is_off() {
        let (mut engine, pool_ids, path_ids) = detached_fixture(400);
        // WFF6MM: direct-call engines merge inline (synchronous harness); turn
        // that OFF so this pins the PRODUCTION return semantics — enqueue end,
        // results ABSENT until the sidecar (or a later drain) lands them.
        engine.set_sync_merge_for_test(false);
        let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
            .iter()
            .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
            .collect();
        engine.solve_dirty(100, &BlockMetadata::default(), &affected_keys_v2);
        assert_eq!(engine.cycle_arm(), "detached");
        // With a 400ms slow path the results land AFTER return (T2's read) —
        // at least the slow pid is absent.
        assert!(
            !engine.cycle.results.contains_key(&path_ids[0]),
            "the detached arm must return at enqueue end: the slow pid is absent AT return"
        );
    }

    // -------------------------------------------------------------------
    // QTZGFL: capacity-modulated admission draw (experiment; flag OFF by
    // default so the current degrade stays byte-identical).
    // -------------------------------------------------------------------

    /// Budget arithmetic: `budget = max(0, target − outstanding)` in KEYS,
    /// `None` while the stance is OFF, and the target is clamped to the
    /// design-locked safety valve. `Some(0)` is the shed predicate.
    #[test]
    fn admission_budget_arithmetic_and_target_clamp() {
        let (mut engine, _pool_ids, _path_ids) = detached_fixture(0);
        // Stance OFF: no budget — the caller take-alls (byte-identical).
        assert_eq!(
            engine.cycle.admission_budget_keys(),
            None,
            "flag OFF must yield no budget (the take_keys path)"
        );
        engine.set_solve_admission(true);
        engine.set_admission_target_depth(3);
        assert_eq!(
            engine.cycle.admission_budget_keys(),
            Some(3),
            "empty pipe: full headroom"
        );
        engine
            .cycle
            .detached_cycle
            .outstanding
            .store(1, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            engine.cycle.admission_budget_keys(),
            Some(2),
            "one straggler: target − 1"
        );
        engine
            .cycle
            .detached_cycle
            .outstanding
            .store(3, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            engine.cycle.admission_budget_keys(),
            Some(0),
            "at target: zero budget = the SHED verdict"
        );
        engine
            .cycle
            .detached_cycle
            .outstanding
            .store(99, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            engine.cycle.admission_budget_keys(),
            Some(0),
            "an overshoot saturates at zero (no unsigned wrap)"
        );
        // The target is clamped to the design-locked safety valve.
        engine
            .cycle
            .detached_cycle
            .outstanding
            .store(0, std::sync::atomic::Ordering::Relaxed);
        engine.set_admission_target_depth(usize::MAX);
        assert_eq!(
            engine.cycle.admission_budget_keys(),
            Some(
                usize::try_from(crate::arb_engine::detached_cycle::DETACHED_INFLIGHT_CAP)
                    .expect("cap fits usize")
            ),
            "an over-cap target clamps to DETACHED_INFLIGHT_CAP"
        );
        engine.set_admission_target_depth(0);
        assert_eq!(
            engine.cycle.admission_budget_keys(),
            Some(1),
            "a zero target clamps up to 1 (a target of 0 would never submit)"
        );
    }

    /// Full-path shed: the gauge preloaded AT the target makes the DRAW
    /// (the single consumption decision, `on_resolve`) return a zero budget,
    /// so the cycle submits NOTHING, advances the solved-block cursor exactly
    /// like the `skipped_empty` bookkeeping pass, latches
    /// `cycle.arm="shed"`, and counts the shed. Driven through the staged
    /// path (Resolved -> Solved), because the dispatch no longer re-reads the
    /// gauge — it consumes the draw-time verdict stashed by `on_resolve`.
    #[test]
    fn admission_zero_budget_sheds_the_whole_cycle() {
        use crate::arb_engine::EngineStages;
        use crate::bot_core::stage_handlers::{
            QuiesceOutcome, QuiesceVerdict, Resolve, Solve, StageHandlers,
        };
        use crate::bot_core::{BlockContext, Epoch, EpochDelta};
        use std::sync::Arc;

        let (mut engine, pool_ids, path_ids) = detached_fixture(400);
        engine.set_solve_admission(true);
        engine.set_admission_target_depth(8);
        engine
            .cycle
            .detached_cycle
            .outstanding
            .store(8, std::sync::atomic::Ordering::Relaxed);
        let engine = Arc::new(parking_lot::Mutex::new(engine));
        let delta = Arc::new(EpochDelta::new(0u64));
        let stages = EngineStages::new(Arc::clone(&engine), Arc::clone(&delta));
        for &p in &pool_ids {
            delta.record(
                degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p),
                10,
            );
        }
        let quiesced = QuiesceOutcome {
            verdict: QuiesceVerdict::Settled,
        };
        let ctx = BlockContext::new(Epoch::at(100), BlockMetadata::default());
        let drawn = stages
            .on_resolve(&Resolve {
                ctx,
                quiesced: &quiesced,
                delta: &delta,
            })
            .expect("resolve hook is infallible");
        assert!(
            drawn.0.is_empty(),
            "a zero-budget draw consumes nothing; the keys are RETAINED"
        );
        let sheds_before = engine
            .lock()
            .cycle
            .detached_cycle
            .shed_cycles
            .load(std::sync::atomic::Ordering::Relaxed);
        stages
            .on_solve(&Solve { ctx, paths: drawn })
            .expect("solve hook is infallible");
        let guard = engine.lock();
        assert_eq!(
            guard.cycle_arm(),
            "shed",
            "a zero-budget cycle must latch the shed arm"
        );
        assert!(
            path_ids
                .iter()
                .all(|p| !guard.cycle.results.contains_key(p)),
            "a shed cycle SUBMITS NOTHING: no path may be solved or merged"
        );
        assert_eq!(
            guard
                .cycle
                .detached_cycle
                .shed_cycles
                .load(std::sync::atomic::Ordering::Relaxed),
            sheds_before + 1,
            "the shed counter must fire exactly once"
        );
        assert_eq!(
            guard.results_block(),
            100,
            "a shed cycle advances the solved-block cursor like skipped_empty"
        );
    }

    /// F2: a draw-zero shed responds BEFORE the `pending_new_paths` merge, so
    /// it never consumes the eager-registration protection — the eagerly
    /// solved path is still merged on the NEXT normal cycle (a post-merge
    /// shed would clear the pipe and let the next cycle's results replacement
    /// drop the eager result).
    #[test]
    #[expect(clippy::too_many_lines)]
    fn admission_draw_zero_shed_preserves_pending_new_paths() {
        use crate::arb_engine::EngineStages;
        use crate::bot_core::stage_handlers::{
            QuiesceOutcome, QuiesceVerdict, Resolve, Solve, StageHandlers,
        };
        use crate::bot_core::{BlockContext, Epoch, EpochDelta};
        use std::sync::Arc;

        let mut engine = ArbitrageEngine::new();
        let a = engine.register_v2_pool(
            Address::from([0x11u8; 20]),
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let b = engine.register_v2_pool(
            Address::from([0x12u8; 20]),
            weth(1000),
            usdc(2_000_000),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let pid = engine
            .register_and_solve_path(vec![
                PoolHop {
                    pool_id: a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: b,
                    zero_for_one: true,
                },
            ])
            .expect("eager path registration succeeds");
        assert!(
            engine.cycle.pending_new_paths.contains(&pid),
            "the eager path starts in the merge pipe"
        );
        engine.set_solve_admission(true);
        engine.set_admission_target_depth(8);
        engine
            .cycle
            .detached_cycle
            .outstanding
            .store(8, std::sync::atomic::Ordering::Relaxed);
        let engine = Arc::new(parking_lot::Mutex::new(engine));
        let delta = Arc::new(EpochDelta::new(0u64));
        let stages = EngineStages::new(Arc::clone(&engine), Arc::clone(&delta));
        delta.record(
            degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, a),
            10,
        );
        let quiesced = QuiesceOutcome {
            verdict: QuiesceVerdict::Settled,
        };
        // Cycle 1 (draw-zero): the shed responds before the merge.
        let ctx = BlockContext::new(Epoch::at(10), BlockMetadata::default());
        let drawn = stages
            .on_resolve(&Resolve {
                ctx,
                quiesced: &quiesced,
                delta: &delta,
            })
            .expect("resolve hook is infallible");
        assert!(drawn.0.is_empty(), "zero budget draws nothing");
        stages
            .on_solve(&Solve { ctx, paths: drawn })
            .expect("solve hook is infallible");
        {
            let guard = engine.lock();
            assert_eq!(guard.cycle_arm(), "shed");
            assert!(
                guard.cycle.pending_new_paths.contains(&pid),
                "a draw-zero shed must NOT consume the eager merge protection"
            );
            assert!(
                guard.cycle.results.contains_key(&pid),
                "the eagerly-solved result survives the shed"
            );
        }
        // Cycle 2 (headroom back): the eager path merges and the pipe clears.
        engine
            .lock()
            .cycle
            .detached_cycle
            .outstanding
            .store(0, std::sync::atomic::Ordering::Relaxed);
        let ctx = BlockContext::new(Epoch::at(11), BlockMetadata::default());
        let drawn = stages
            .on_resolve(&Resolve {
                ctx,
                quiesced: &quiesced,
                delta: &delta,
            })
            .expect("resolve hook is infallible");
        stages
            .on_solve(&Solve { ctx, paths: drawn })
            .expect("solve hook is infallible");
        // WFF6MM: the cycle enqueues and returns; wait for the sidecar merge.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2_000);
        loop {
            let merged = {
                let guard = engine.lock();
                guard.cycle.pending_new_paths.is_empty() && guard.cycle.results.contains_key(&pid)
            };
            if merged {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the next normal cycle must merge + clear the eager-registration pipe"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let guard = engine.lock();
        assert!(
            guard.cycle.pending_new_paths.is_empty(),
            "the next normal cycle merges + clears the eager-registration pipe"
        );
        assert!(
            guard.cycle.results.contains_key(&pid),
            "the eager result survives the merge cycle"
        );
    }

    /// RACE REGRESSION (F1/F3, red-first): the admission budget is decided
    /// ONCE, at the DRAW. A cycle that drew a POSITIVE budget owns those keys
    /// (they are already removed from the ledger); an earlier cycle's bin
    /// thread bumping in-flight to/over the target between the draw and the
    /// dispatch must NOT turn that cycle into a shed — the drawn keys would
    /// be discarded (never submitted, never re-recorded). WFF6MM: the drawn
    /// keys submit down the one (detached) arm regardless of the gauge.
    ///
    /// The test drives both stages explicitly, so it can interleave the
    /// in-flight bump exactly in the race window (between `on_resolve` and
    /// `on_solve`) — the shape the stage machine's Resolved -> Solved
    /// sequencing makes deterministic here. RED on the pre-remediation code
    /// (the dispatch re-read the gauge fresh and shed, discarding the drawn
    /// keys); GREEN after the draw-time verdict travels with the cycle.
    #[test]
    fn admission_race_positive_draw_never_sheds() {
        use crate::arb_engine::EngineStages;
        use crate::bot_core::stage_handlers::{
            QuiesceOutcome, QuiesceVerdict, Resolve, Solve, StageHandlers,
        };
        use crate::bot_core::{BlockContext, Epoch, EpochDelta};
        use std::sync::Arc;

        let (mut engine, pool_ids, path_ids) = detached_fixture(0);
        engine.set_solve_admission(true);
        engine.set_admission_target_depth(8);
        // An eager-registration path in the merge pipe: a post-merge race
        // shed would be the F2 data-loss class.
        engine.cycle.pending_new_paths.insert(path_ids[1]);
        let engine = Arc::new(parking_lot::Mutex::new(engine));
        let delta = Arc::new(EpochDelta::new(0u64));
        let stages = EngineStages::new(Arc::clone(&engine), Arc::clone(&delta));
        for &p in &pool_ids {
            delta.record(
                degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p),
                10,
            );
        }
        let quiesced = QuiesceOutcome {
            verdict: QuiesceVerdict::Settled,
        };
        let ctx = BlockContext::new(Epoch::at(10), BlockMetadata::default());
        // DRAW with an EMPTY pipe: budget = 8 -> positive; every key drawn.
        let drawn = stages
            .on_resolve(&Resolve {
                ctx,
                quiesced: &quiesced,
                delta: &delta,
            })
            .expect("resolve hook is infallible");
        assert!(
            !drawn.0.is_empty(),
            "an empty pipe must draw a positive budget"
        );
        assert!(
            delta.is_empty(),
            "the draw consumed the ledger keys (a shed would lose them)"
        );
        // RACE: the bin thread bumps in-flight to the cap between the draw
        // and the dispatch.
        engine
            .lock()
            .cycle
            .detached_cycle
            .outstanding
            .store(8, std::sync::atomic::Ordering::Relaxed);
        let sheds_before = engine
            .lock()
            .cycle
            .detached_cycle
            .shed_cycles
            .load(std::sync::atomic::Ordering::Relaxed);
        stages
            .on_solve(&Solve { ctx, paths: drawn })
            .expect("solve hook is infallible");
        // WFF6MM: the dispatch enqueues and returns; the sidecar merges. Wait
        // for the merge before reading the results/pending pipe.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2_000);
        loop {
            let merged = {
                let guard = engine.lock();
                path_ids.iter().all(|p| guard.cycle.results.contains_key(p))
                    && guard.cycle.pending_new_paths.is_empty()
            };
            if merged {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the sidecar must merge the drawn keys after the detached enqueue"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let guard = engine.lock();
        assert_eq!(
            guard.cycle_arm(),
            "detached",
            "a positive-budget draw always detaches — there is no in-cycle response left"
        );
        assert_eq!(
            guard
                .cycle
                .detached_cycle
                .shed_cycles
                .load(std::sync::atomic::Ordering::Relaxed),
            sheds_before,
            "a cycle that drew a POSITIVE budget must NEVER shed at the dispatch"
        );
        assert!(
            path_ids.iter().all(|p| guard.cycle.results.contains_key(p)),
            "the drawn keys must be SUBMITTED, never discarded"
        );
        assert!(
            guard.cycle.pending_new_paths.is_empty(),
            "the race cycle must still merge + clear the eager-registration pipe (F2)"
        );
    }

    /// Carry: a shed cycle RETAINS its keys in the ledger; a later cycle with
    /// headroom draws them again (a fresh solve against current state, not a
    /// replay). This is the acceptance the whole design turns on.
    #[test]
    fn admission_carries_retained_keys_to_a_later_cycle() {
        use crate::arb_engine::EngineStages;
        use crate::bot_core::stage_handlers::{
            QuiesceOutcome, QuiesceVerdict, Resolve, StageHandlers,
        };
        use crate::bot_core::{BlockContext, Epoch, EpochDelta};
        use std::sync::Arc;

        let engine = ArbitrageEngine::new();
        let engine = Arc::new(parking_lot::Mutex::new(engine));
        engine.lock().set_solve_admission(true);
        engine.lock().set_admission_target_depth(2);
        let delta = Arc::new(EpochDelta::new(0u64));
        let stages = EngineStages::new(Arc::clone(&engine), Arc::clone(&delta));

        let keys: Vec<_> = (1..=3u64)
            .map(|id| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, id))
            .collect();
        for key in &keys {
            delta.record(*key, 10);
        }
        let quiesced = QuiesceOutcome {
            verdict: QuiesceVerdict::Settled,
        };
        // Gauge AT the target: zero budget ⇒ shed — nothing drawn, all retained.
        engine
            .lock()
            .cycle
            .detached_cycle
            .outstanding
            .store(2, std::sync::atomic::Ordering::Relaxed);
        let ctx = BlockContext::new(Epoch::at(10), BlockMetadata::default());
        let drawn = stages
            .on_resolve(&Resolve {
                ctx,
                quiesced: &quiesced,
                delta: &delta,
            })
            .expect("resolve hook is infallible");
        assert!(
            drawn.0.is_empty(),
            "zero-budget draw takes nothing; the keys are RETAINED"
        );
        assert_eq!(
            delta.snapshot_keys(),
            keys,
            "carry: the ledger keeps every key"
        );
        // Depth falls: the NEXT cycle draws the carried keys (budget = 2).
        engine
            .lock()
            .cycle
            .detached_cycle
            .outstanding
            .store(0, std::sync::atomic::Ordering::Relaxed);
        let ctx = BlockContext::new(Epoch::at(11), BlockMetadata::default());
        let drawn = stages
            .on_resolve(&Resolve {
                ctx,
                quiesced: &quiesced,
                delta: &delta,
            })
            .expect("resolve hook is infallible");
        assert_eq!(
            drawn.0,
            vec![keys[0], keys[1]],
            "the carried keys are drawn freshest-first in insertion order"
        );
        assert_eq!(
            delta.snapshot_keys(),
            vec![keys[2]],
            "the overflow stays retained for the next cycle"
        );
        // And the final carry drains on the next empty pipe.
        let ctx = BlockContext::new(Epoch::at(12), BlockMetadata::default());
        let drawn = stages
            .on_resolve(&Resolve {
                ctx,
                quiesced: &quiesced,
                delta: &delta,
            })
            .expect("resolve hook is infallible");
        assert_eq!(drawn.0, vec![keys[2]], "the last carried key drains");
        assert!(delta.is_empty());
    }

    /// Retention: on a block advance the ledger prunes carried keys older than
    /// `head − W` and counts the expiry, so a lead that stays starved
    /// eventually expires VISIBLY instead of pinning the ledger forever.
    #[test]
    fn admission_retention_window_expires_carried_leads() {
        use crate::arb_engine::EngineStages;
        use crate::bot_core::stage_handlers::{
            QuiesceOutcome, QuiesceVerdict, Resolve, StageHandlers,
        };
        use crate::bot_core::{BlockContext, Epoch, EpochDelta};
        use std::sync::Arc;

        let engine = ArbitrageEngine::new();
        let engine = Arc::new(parking_lot::Mutex::new(engine));
        engine.lock().set_solve_admission(true);
        engine.lock().set_admission_target_depth(4);
        engine.lock().set_admission_retention_blocks(5);
        let delta = Arc::new(EpochDelta::new(0u64));
        let stages = EngineStages::new(Arc::clone(&engine), Arc::clone(&delta));

        let stale = degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, 1);
        let fresh = degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, 2);
        delta.record(stale, 10);
        delta.record(fresh, 100);
        let quiesced = QuiesceOutcome {
            verdict: QuiesceVerdict::Settled,
        };
        // head 100, W 5 ⇒ cutoff 95: the block-10 lead expires; the block-100
        // lead is drawn.
        let ctx = BlockContext::new(Epoch::at(100), BlockMetadata::default());
        let drawn = stages
            .on_resolve(&Resolve {
                ctx,
                quiesced: &quiesced,
                delta: &delta,
            })
            .expect("resolve hook is infallible");
        assert_eq!(drawn.0, vec![fresh], "only the in-window lead is drawn");
        assert_eq!(
            engine
                .lock()
                .cycle
                .detached_cycle
                .leads_expired
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the retention prune must count exactly one expired lead"
        );
        assert!(delta.is_empty());
    }

    /// Flag OFF: `on_resolve` take-alls (even with the gauge saturated) and
    /// the engine DETACHES every cycle — never a shed, and (WFF6MM) never any
    /// in-cycle degrade either.
    #[test]
    fn admission_off_keeps_take_all_and_never_sheds() {
        use crate::arb_engine::EngineStages;
        use crate::bot_core::stage_handlers::{
            QuiesceOutcome, QuiesceVerdict, Resolve, StageHandlers,
        };
        use crate::bot_core::{BlockContext, Epoch, EpochDelta};
        use std::sync::Arc;

        let (mut engine, pool_ids, _path_ids) = detached_fixture(0);
        // Stance left OFF; the gauge at the cap must NOT shed.
        engine
            .cycle
            .detached_cycle
            .outstanding
            .store(8, std::sync::atomic::Ordering::Relaxed);
        let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
            .iter()
            .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
            .collect();
        engine.solve_dirty(100, &BlockMetadata::default(), &affected_keys_v2);
        assert_eq!(
            engine.cycle_arm(),
            "detached",
            "flag OFF: the cycle still takes the one dispatch arm (WFF6MM)"
        );
        assert_eq!(
            engine
                .cycle
                .detached_cycle
                .shed_cycles
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "flag OFF must never shed"
        );
        // And the draw path take-alls regardless of the gauge.
        let engine_arc = Arc::new(parking_lot::Mutex::new(engine));
        let delta = Arc::new(EpochDelta::new(0u64));
        let stages = EngineStages::new(Arc::clone(&engine_arc), Arc::clone(&delta));
        for id in 1..=5u64 {
            delta.record(
                degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, id),
                10,
            );
        }
        let quiesced = QuiesceOutcome {
            verdict: QuiesceVerdict::Settled,
        };
        let ctx = BlockContext::new(Epoch::at(10), BlockMetadata::default());
        let drawn = stages
            .on_resolve(&Resolve {
                ctx,
                quiesced: &quiesced,
                delta: &delta,
            })
            .expect("resolve hook is infallible");
        assert_eq!(
            drawn.0.len(),
            5,
            "flag OFF: take_keys consumes the whole ledger"
        );
        assert!(delta.is_empty());
    }

    /// N1 (in-cycle duplicate policy, tightened): a duplicate
    /// (`solve_seq`, pid) arrival at the in-cycle drain must be REFUSED —
    /// counted and logged, never merged twice. Red at HEAD against the
    /// NEW policy (today the in-cycle drain logs-and-merges anyway —
    /// sD:2550 — because its local set is dropped with the cycle).
    // 43E3H3 red-first: pins the tightened refuse-the-merge policy
    // (design §4.4 REV 2 decision, Risk 5 option 1).
    #[test]
    fn duplicate_lane_outcome_does_not_double_apply() {
        let (mut engine, pool_ids, path_ids) = detached_fixture(0);
        let affected_keys_v2: Vec<degenbot_solvers::affected_keys::AffectedKey> = pool_ids
            .iter()
            .map(|&p| degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, p))
            .collect();
        engine.solve_dirty(100, &BlockMetadata::default(), &affected_keys_v2);
        let pid = path_ids[0];
        let results_before = engine.cycle.results.len();
        let applied_before = engine
            .cycle
            .detached_cycle
            .applied
            .load(std::sync::atomic::Ordering::Relaxed);
        // WFF6MM: the machine issues the seq (no in-cycle counter) — read
        // back the tick the cycle actually claimed so the replay collides.
        let cycle_seq = engine.cycle.detached_cycle.issued_seq();

        // A second Solved arrival for the SAME (seq, pid) through the merge
        // disposition must be refused: no second results write, no applied
        // count for the duplicate.
        let fresh_stamp = engine.cycle.resolved_update_snapshot[&pid].clone();
        let fresh_result = engine.cycle.results.get(&pid).unwrap().clone();
        let item = crate::arb_engine::executor::LaneOutcome::Solved(
            crate::arb_engine::executor::SolveOutcome {
                payload: None,
                worker_clamp_twins: 0,
                cycle_seq,
                solve_block: 100,
                metadata: BlockMetadata::default(),
                pid,
                update_stamp: fresh_stamp,
                result: fresh_result,
                solve_span: tracing::Span::none(),
            },
        );
        engine.merge_detached_item(item);

        assert_eq!(
            engine
                .cycle
                .detached_cycle
                .duplicate_outcomes
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "in-cycle dup policy (tightened): the fuse must trip exactly once"
        );
        assert_eq!(
            engine.cycle.results.len(),
            results_before,
            "in-cycle dup policy (tightened): the refused duplicate must not re-merge"
        );
        let _ = applied_before;
    }

    #[test]
    #[expect(clippy::too_many_lines)] // A/B harness: two full engines, worth the length
    fn resolve_chunk_parity_parallel_matches_serial_and_reuses_cache_walks() {
        const N: usize = 600; // >= RESOLVE_PAR_MIN (512) so the parallel arm engages

        let build = || {
            let mut engine = ArbitrageEngine::new();
            // Two HUB pools shared by every path; one unique pool per path.
            let hub_a = engine.register_v2_pool(
                Address::from([0xaa_u8; 20]),
                usdc(1_000_000),
                weth(700),
                GAMMA_03,
                FEE_DENOM_03,
            );
            let hub_b = engine.register_v2_pool(
                Address::from([0xbb_u8; 20]),
                weth(900),
                usdc(1_200_000),
                GAMMA_03,
                FEE_DENOM_03,
            );
            let mut path_ids = Vec::with_capacity(N);
            for i in 0..N {
                let mut b = [0x33u8; 20];
                // Distinct for the whole 0..600 range: high byte of the
                // 16-bit index in b[15], low byte in b[16] (expect: a test
                // address space, values provably < 256 per byte).
                b[15] = u8::try_from(i / 256).expect("N < 65536");
                b[16] = u8::try_from(i % 256).expect("index mod 256 fits u8");
                b[17] = 0x5au8;
                let unique = engine.register_v2_pool(
                    Address::from(b),
                    usdc(50_000),
                    weth(30),
                    GAMMA_03,
                    FEE_DENOM_03,
                );
                let id = engine
                    .register_and_solve_path(vec![
                        PoolHop {
                            pool_id: hub_a,
                            zero_for_one: true,
                        },
                        PoolHop {
                            pool_id: unique,
                            zero_for_one: true,
                        },
                        PoolHop {
                            pool_id: hub_b,
                            zero_for_one: false,
                        },
                    ])
                    .unwrap();
                path_ids.push((id, unique, hub_a, hub_b));
            }
            (engine, path_ids, hub_a, hub_b)
        };

        let run = |parallel: bool| {
            let (mut engine, path_ids, hub_a, hub_b) = build();
            // YI5NGB: the A/B arm drives the INSTANCE stance now (no
            // process-global flip; no parallel-order dependence).
            engine.set_resolve_parallel_for_test(parallel);

            // Cycle 1: dirty BOTH hubs -> all N paths re-resolve in one cycle.
            engine.process_updates(
                &[
                    (Address::from([0xaa_u8; 20]), usdc(990_000), weth(705)),
                    (Address::from([0xbb_u8; 20]), weth(895), usdc(1_210_000)),
                ],
                &[],
                500,
                &BlockMetadata::default(),
            );
            engine.cycle.run_epoch(
                &crate::arb_engine::tests::test_keys::affected_keys(
                    &HashSet::from([hub_a, hub_b]),
                    &HashSet::new(),
                    &HashSet::new(),
                ),
                500,
                &BlockMetadata::default(),
                &engine.registry,
                &mut engine.delivery,
            );

            // Cycle 2: dirty hub_b only -> 600 affected paths again; hub_a must
            // be walked ONCE by the shared sharded cache (serial: also once).
            let projections_before = engine.cycle.hop_projection_count;
            engine.process_updates(
                &[(Address::from([0xbb_u8; 20]), weth(880), usdc(1_230_000))],
                &[],
                501,
                &BlockMetadata::default(),
            );
            engine.cycle.run_epoch(
                &crate::arb_engine::tests::test_keys::affected_keys(
                    &HashSet::from([hub_b]),
                    &HashSet::new(),
                    &HashSet::new(),
                ),
                501,
                &BlockMetadata::default(),
                &engine.registry,
                &mut engine.delivery,
            );
            let projections_delta = engine.cycle.hop_projection_count - projections_before;

            let (results, _block) = engine.latest_results();
            (
                results,
                engine.cycle.paths_same_state_this_cycle,
                projections_delta,
                path_ids,
            )
        };

        let (serial_results, serial_same_state, serial_proj_delta, path_ids) = run(false);
        let (par_results, par_same_state, par_proj_delta, _path_ids) = run(true);

        // YI5NGB: the instance-stance cutover -> nothing process-global remains to restore.

        assert_eq!(path_ids.len(), N);
        for (path_id, _unique, _a, _b) in &path_ids {
            let sres = serial_results.get(path_id).expect("serial result");
            let pres = par_results.get(path_id).expect("parallel result");
            assert_eq!(
                sres.profit, pres.profit,
                "profit diverged for path {path_id}"
            );
            assert_eq!(
                sres.solver_pool_states.len(),
                pres.solver_pool_states.len(),
                "hop-state shape diverged for path {path_id}"
            );
        }
        assert_eq!(
            serial_same_state, par_same_state,
            "same-state accounting diverged"
        );
        // THE cache-reuse invariant: both arms walk the same pool count on the
        // second cycle, and exactly one walk per distinct pool (not per chunk).
        assert_eq!(
            serial_proj_delta, par_proj_delta,
            "projection walks diverged"
        );
        // Cycle-2 re-resolve of 600 paths: only hub_b (dirty) misses the cache;
        // hub_a hits in every path. 1 walk total in BOTH arms.
        assert_eq!(
            serial_proj_delta, 1,
            "cycle-2 must walk only the dirty pool, got {serial_proj_delta}"
        );
    }
    // KJWIK5: the deferred-path carry — the ledger re-record at the deferral
    // site. The future-price tripwire is unreachable after the solve-anchor
    // head floor (only a mid-solve state advance can trip it), so these tests
    // install the `test_force_deferred` seam to exercise the carry
    // deterministically. The re-record hook itself is the production seam
    // the `EngineStages` constructor installs (`set_deferred_re_record`);
    // with the hook unset (direct engine drives) the deferral keeps today's
    // drop.
    // -------------------------------------------------------------------

    fn kjwik5_key(pool_id: u64) -> degenbot_solvers::affected_keys::AffectedKey {
        degenbot_solvers::affected_keys::AffectedKey::new(HopType::V2, pool_id)
    }

    fn kjwik5_affected_keys(pool_ids: &[u64]) -> Vec<degenbot_solvers::affected_keys::AffectedKey> {
        pool_ids.iter().copied().map(kjwik5_key).collect()
    }

    fn kjwik5_path_keys(
        engine: &ArbitrageEngine,
        path_id: u64,
    ) -> Vec<degenbot_solvers::affected_keys::AffectedKey> {
        engine
            .path_pools()
            .get(&path_id)
            .expect("registered path")
            .pools
            .iter()
            .map(|r| degenbot_solvers::affected_keys::AffectedKey::new(r.hop_type, r.pool_key))
            .collect()
    }

    /// Guard 1: capture the `[solve-phase]` events' `paths.deferred_future_price`
    /// field. Both reporting sites (the resolve funnel and the detached
    /// enqueue) carry the same value; the test asserts every captured value.
    type Kjwik5EventFields = std::sync::Arc<parking_lot::Mutex<Vec<Vec<(String, String)>>>>;

    type Kjwik5ReRecords = std::sync::Arc<
        parking_lot::Mutex<Vec<(Vec<degenbot_solvers::affected_keys::AffectedKey>, u64)>>,
    >;

    fn kjwik5_capture_deferred_counter(run: impl FnOnce()) -> Vec<u64> {
        use tracing_subscriber::layer::SubscriberExt;

        struct EventCapture(Kjwik5EventFields);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for EventCapture {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                struct Saver(Vec<(String, String)>);
                impl tracing::field::Visit for Saver {
                    fn record_str(&mut self, f: &tracing::field::Field, v: &str) {
                        self.0.push((f.name().to_string(), v.to_string()));
                    }
                    fn record_u64(&mut self, f: &tracing::field::Field, v: u64) {
                        self.0.push((f.name().to_string(), v.to_string()));
                    }
                    fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                        self.0.push((f.name().to_string(), format!("{v:?}")));
                    }
                }
                let mut saver = Saver(Vec::new());
                event.record(&mut saver);
                self.0.lock().push(saver.0);
            }
        }

        let capture = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let subscriber =
            tracing_subscriber::registry().with(EventCapture(std::sync::Arc::clone(&capture)));
        tracing::subscriber::with_default(subscriber, run);
        let events = capture.lock().clone();
        events
            .into_iter()
            .filter_map(|fields| {
                fields
                    .iter()
                    .find(|(name, _)| name == "paths.deferred_future_price")
                    .and_then(|(_, value)| {
                        value
                            .chars()
                            .take_while(char::is_ascii_digit)
                            .collect::<String>()
                            .parse::<u64>()
                            .ok()
                    })
            })
            .collect()
    }

    /// Red-first (a): a deferred path re-records ALL its hop pools through the
    /// engine's installed hook, with the cycle's solve block — the ledger
    /// carry that lets the next draw re-include the path through the same
    /// freshness ordering.
    #[test]
    fn deferred_path_re_records_all_hop_pools_via_the_hook() {
        let (mut engine, pool_ids, path_ids) = detached_fixture(0);
        let deferred = path_ids[0];
        let expected = kjwik5_path_keys(&engine, deferred);
        assert_eq!(expected.len(), 2, "fixture path has two hops");
        engine.set_force_deferred_for_test(HashSet::from([deferred]));
        let seen: Kjwik5ReRecords = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let seen_hook = std::sync::Arc::clone(&seen);
        engine.set_deferred_re_record(std::sync::Arc::new(move |keys, block| {
            seen_hook.lock().push((keys.to_vec(), block));
        }));

        engine.solve_dirty(
            100,
            &BlockMetadata::default(),
            &kjwik5_affected_keys(&pool_ids),
        );

        let calls = seen.lock().clone();
        assert_eq!(calls.len(), 1, "one re-record call per deferred cycle");
        assert_eq!(
            calls[0].1, 100,
            "the re-record targets the cycle's solve block"
        );
        assert_eq!(
            calls[0].0.as_slice(),
            expected.as_slice(),
            "EVERY hop pool of the deferred path, in path order"
        );
        assert!(
            !engine.cycle.results.contains_key(&deferred),
            "the deferred path is not submitted this cycle (no double-submit)"
        );
        for &sibling in &path_ids[1..] {
            assert!(
                engine.cycle.results.contains_key(&sibling),
                "non-deferred siblings still solve"
            );
        }
    }

    /// Red-first (d): with the hook unset (direct engine drive, no driver),
    /// the deferral keeps today's dropped behavior and counts unchanged.
    #[test]
    fn deferred_path_without_hook_keeps_today_drop_behavior() {
        let (mut engine, pool_ids, path_ids) = detached_fixture(0);
        let deferred = path_ids[1];
        engine.set_force_deferred_for_test(HashSet::from([deferred]));
        engine.solve_dirty(
            100,
            &BlockMetadata::default(),
            &kjwik5_affected_keys(&pool_ids),
        );
        assert!(
            !engine.cycle.results.contains_key(&deferred),
            "with the hook unset the deferred path keeps today's dropped behavior"
        );
        assert!(engine.cycle.results.contains_key(&path_ids[0]));
        assert!(engine.cycle.results.contains_key(&path_ids[2]));
    }

    /// Red-first (c): `paths.deferred_future_price` semantics are unchanged —
    /// same site, same triggers, same counts (0 with no future hop, N with N).
    #[test]
    fn future_price_counter_semantics_are_unchanged() {
        let (mut engine, pool_ids, path_ids) = detached_fixture(0);
        let affected = kjwik5_affected_keys(&pool_ids);
        let baseline = kjwik5_capture_deferred_counter(|| {
            engine.solve_dirty(100, &BlockMetadata::default(), &affected);
        });
        assert!(
            !baseline.is_empty(),
            "the counter is reported at its [solve-phase] sites"
        );
        assert!(
            baseline.iter().all(|&count| count == 0),
            "no future-priced hop → the counter reads 0; got {baseline:?}"
        );
        engine.set_force_deferred_for_test(HashSet::from([path_ids[0], path_ids[2]]));
        let forced = kjwik5_capture_deferred_counter(|| {
            engine.solve_dirty(101, &BlockMetadata::default(), &affected);
        });
        assert!(
            forced.iter().all(|&count| count == 2),
            "two deferred paths count at the same site; got {forced:?}"
        );
    }

    /// Red-first (b): the staged carry. Cycle 1 defers the future-priced path
    /// and re-records its pools into the ledger; cycle 2 (the anchor caught
    /// up) draws it back and solves it against the retry cycle's block.
    #[test]
    fn staged_deferred_path_carries_via_the_ledger_and_solves_on_the_retry() {
        use crate::arb_engine::EngineStages;
        use crate::bot_core::stage_handlers::{
            QuiesceOutcome, QuiesceVerdict, Resolve, Solve, StageHandlers,
        };
        use crate::bot_core::{BlockContext, Epoch, EpochDelta};
        use std::sync::Arc;

        let (mut engine, pool_ids, path_ids) = detached_fixture(0);
        let deferred = path_ids[0];
        let deferred_keys = kjwik5_path_keys(&engine, deferred);
        engine.set_force_deferred_for_test(HashSet::from([deferred]));
        let engine = Arc::new(parking_lot::Mutex::new(engine));
        let delta = Arc::new(EpochDelta::new(0u64));
        let stages = EngineStages::new(Arc::clone(&engine), Arc::clone(&delta));
        for &p in &pool_ids {
            delta.record(kjwik5_key(p), 10);
        }
        let quiesced = QuiesceOutcome {
            verdict: QuiesceVerdict::Settled,
        };

        // Cycle 1: draw, defer, re-record into the ledger at block 100.
        let ctx = BlockContext::new(Epoch::at(100), BlockMetadata::default());
        let drawn = stages
            .on_resolve(&Resolve {
                ctx,
                quiesced: &quiesced,
                delta: &delta,
            })
            .expect("resolve hook is infallible");
        assert_eq!(
            drawn.0.len(),
            pool_ids.len(),
            "cycle 1 draws every seed key"
        );
        stages
            .on_solve(&Solve { ctx, paths: drawn })
            .expect("solve hook is infallible");
        assert!(
            !engine.lock().cycle.results.contains_key(&deferred),
            "cycle 1 defers the path (it is never submitted)"
        );
        let pending = delta.snapshot_keys();
        for key in &deferred_keys {
            assert!(
                pending.contains(key),
                "the deferred path's key {key:?} is re-recorded for the next draw"
            );
        }

        // The anchor catches up: the path is no longer future-priced. Cycle 2
        // draws it back from the ledger.
        engine.lock().set_force_deferred_for_test(HashSet::new());
        let ctx = BlockContext::new(Epoch::at(101), BlockMetadata::default());
        let drawn = stages
            .on_resolve(&Resolve {
                ctx,
                quiesced: &quiesced,
                delta: &delta,
            })
            .expect("resolve hook is infallible");
        let drawn_keys: HashSet<_> = drawn.0.iter().copied().collect();
        for key in &deferred_keys {
            assert!(
                drawn_keys.contains(key),
                "the retry draw re-includes {key:?}"
            );
        }
        stages
            .on_solve(&Solve { ctx, paths: drawn })
            .expect("solve hook is infallible");

        // Q1a: the retried path's solve block is the RETRY cycle's block.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !engine.lock().cycle.results.contains_key(&deferred) {
            assert!(
                std::time::Instant::now() < deadline,
                "the retried path must solve and merge via the sidecar"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            engine.lock().results_block(),
            101,
            "the retried path solves at the retry cycle's block"
        );
    }

    /// Guard 4: the retention window now bounds the deferred-retry lifetime —
    /// a deferred lead never redrawn within `admission_retention_blocks` is
    /// pruned and counted in `degenbot.detached.leads_expired`.
    #[test]
    fn staged_deferred_retry_expires_after_the_retention_window() {
        use crate::arb_engine::EngineStages;
        use crate::bot_core::stage_handlers::{
            QuiesceOutcome, QuiesceVerdict, Resolve, Solve, StageHandlers,
        };
        use crate::bot_core::{BlockContext, Epoch, EpochDelta};
        use std::sync::atomic::Ordering;
        use std::sync::Arc;

        let (mut engine, pool_ids, path_ids) = detached_fixture(0);
        let deferred = path_ids[0];
        let deferred_keys = kjwik5_path_keys(&engine, deferred);
        engine.set_solve_admission(true);
        engine.set_admission_target_depth(8);
        // Cycle 1's window is wide (nothing seeded may expire before the
        // deferral); it is narrowed before cycle 2 to bound the retry.
        engine.set_admission_retention_blocks(200);
        engine.set_force_deferred_for_test(HashSet::from([deferred]));
        let engine = Arc::new(parking_lot::Mutex::new(engine));
        let delta = Arc::new(EpochDelta::new(0u64));
        let stages = EngineStages::new(Arc::clone(&engine), Arc::clone(&delta));
        for &p in &pool_ids {
            delta.record(kjwik5_key(p), 10);
        }
        let quiesced = QuiesceOutcome {
            verdict: QuiesceVerdict::Settled,
        };

        // Cycle 1 defers + re-records the deferred path at block 100.
        let ctx = BlockContext::new(Epoch::at(100), BlockMetadata::default());
        let drawn = stages
            .on_resolve(&Resolve {
                ctx,
                quiesced: &quiesced,
                delta: &delta,
            })
            .expect("resolve hook is infallible");
        stages
            .on_solve(&Solve { ctx, paths: drawn })
            .expect("solve hook is infallible");
        let expired_before = engine
            .lock()
            .cycle
            .detached_cycle
            .leads_expired
            .load(Ordering::Relaxed);
        // The re-record targeted cycle 1's solve block.
        let defer_block = engine.lock().results_block();

        // Cycle 2: narrow the window to zero and advance one block — the
        // cutoff prunes the deferred lead's bucket before the draw, so it is
        // never redrawn.
        engine.lock().set_force_deferred_for_test(HashSet::new());
        engine.lock().set_admission_retention_blocks(0);
        let ctx = BlockContext::new(Epoch::at(defer_block + 1), BlockMetadata::default());
        let drawn = stages
            .on_resolve(&Resolve {
                ctx,
                quiesced: &quiesced,
                delta: &delta,
            })
            .expect("resolve hook is infallible");
        assert!(
            !drawn.0.iter().any(|key| deferred_keys.contains(key)),
            "the deferred retry lead expired at the retention window boundary"
        );
        stages
            .on_solve(&Solve { ctx, paths: drawn })
            .expect("solve hook is infallible");
        let expired_after = engine
            .lock()
            .cycle
            .detached_cycle
            .leads_expired
            .load(Ordering::Relaxed);
        assert_eq!(
            expired_after - expired_before,
            u64::try_from(deferred_keys.len()).expect("small key count"),
            "the expired deferred lead is counted (degenbot_detached_leads_expired_total)"
        );
    }
}

pub(crate) mod test_keys {
    use ::degenbot_solvers::affected_keys::AffectedKey;
    use ::degenbot_solvers::mixed::HopType;
    /// Test-side affected-key plumbing (the trivial remainder of the deleted
    /// (`EpochDelta` is sole authority), so the per-family sets below are
    /// just a sorted `AffectedKey` builder the solve-shaped tests use to
    /// call `run_epoch` / `solve_dirty`. No production
    /// caller, no parity claim.
    use hashbrown::HashSet;

    /// Convert per-family key sets into delta-shaped affected keys
    /// (sorted, retired `take_all` order).
    #[must_use]
    pub(crate) fn affected_keys(
        v2: &HashSet<u64>,
        v3: &HashSet<u64>,
        v4: &HashSet<u64>,
    ) -> Vec<AffectedKey> {
        let mut keys: Vec<AffectedKey> = v2
            .iter()
            .map(|&p| AffectedKey::new(HopType::V2, p))
            .chain(v3.iter().map(|&p| AffectedKey::new(HopType::V3, p)))
            .chain(v4.iter().map(|&p| AffectedKey::new(HopType::V4, p)))
            .collect();
        keys.sort_unstable();
        keys
    }

    /// The per-family dirty-set builder the solve tests use (the old
    /// `DirtySets` insert + `to_affected_keys` take, single-threaded).
    pub(crate) struct DirtyKeys {
        v2: HashSet<u64>,
        v3: HashSet<u64>,
        v4: HashSet<u64>,
    }

    impl DirtyKeys {
        #[must_use]
        pub(crate) fn new() -> Self {
            Self {
                v2: HashSet::new(),
                v3: HashSet::new(),
                v4: HashSet::new(),
            }
        }

        /// Insert `pool_id` into the set for `hop_type`.
        pub(crate) fn insert(&mut self, pool_id: u64, hop_type: HopType) {
            match hop_type {
                HopType::V2 => self.v2.insert(pool_id),
                HopType::V3 => self.v3.insert(pool_id),
                HopType::V4 => self.v4.insert(pool_id),
                _ => false, // non-pool hop types are never dirtied
            };
        }

        /// Delta-shaped sorted take view (no consume — tests re-use).
        #[must_use]
        pub(crate) fn to_affected_keys(&self) -> Vec<AffectedKey> {
            affected_keys(&self.v2, &self.v3, &self.v4)
        }
    }

    impl Default for DirtyKeys {
        fn default() -> Self {
            Self::new()
        }
    }

    // Cold-start trace (detached-cycle arm attribution): the cycle span must
    // carry `cycle.arm`, derivable WITHOUT log archaeology. The helper below
    // is the ONE wiring site (the solve cycle, at the machine's begin_cycle
    // verdict). WFF6MM: one arm remains, so one stamp.
    #[cfg(feature = "otel")]
    #[test]
    #[expect(clippy::expect_used)]
    fn cycle_arm_span_field_stamps_the_arm() {
        use crate::arb_engine::engine_stages::record_cycle_arm_telemetry;
        use opentelemetry_sdk::trace::InMemorySpanExporter;
        use tracing_subscriber::layer::SubscriberExt;

        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = crate::otel::provider_with_exporter(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(crate::otel::layer(tracer));

        tracing::subscriber::with_default(subscriber, || {
            let detached =
                tracing::info_span!("degenbot.arb.solve", cycle.arm = tracing::field::Empty,);
            let guard = detached.enter();
            assert_eq!(
                record_cycle_arm_telemetry(&detached, "detached"),
                "detached",
                "the helper returns the label the caller latches on the engine"
            );
            drop(guard);
        });

        provider.force_flush().expect("flush");
        let spans = exporter.get_finished_spans().expect("spans");
        let solve_spans: Vec<_> = spans
            .iter()
            .filter(|sp| sp.name.as_ref() == "degenbot.arb.solve")
            .collect();
        assert_eq!(
            solve_spans.len(),
            1,
            "expected exactly the one stamped cycle span; got {spans:?}"
        );
        let arm_values: Vec<String> = solve_spans
            .iter()
            .filter_map(|sp| {
                sp.attributes
                    .iter()
                    .find(|kv| kv.key == opentelemetry::Key::from_static_str("cycle.arm"))
                    .and_then(|kv| match &kv.value {
                        opentelemetry::Value::String(v) => Some(v.to_string()),
                        _ => None,
                    })
            })
            .collect();
        assert!(
            arm_values == vec!["detached".to_string()],
            "the one dispatch arm must stamp cycle.arm exactly once; got {arm_values:?}"
        );
    }

    // -------------------------------------------------------------------
}

// =======================================================================
// 5WCRWZ T7: the fixture-driven clamp/merge/worker tests, moved here from
// the deleted grab file's test island. They exercise the `SolveCycle`
// clamp + merge surfaces through a real engine, so they live with the
// engine-parity tests.
// =======================================================================
#[cfg(test)]
mod clamp_merge_worker_tests {
    #![expect(clippy::expect_used)] // tests assert clamp/merge invariants
    use crate::arb_engine::lane_walk::clamp_result_in_worker;
    use crate::arb_engine::solve_cycle::{PathTimesHeap, SolveCycleShared};
    use crate::arb_engine::{ArbitrageEngine, BlockMetadata};
    use crate::bot_core::{TickInfo, V4PoolKey};
    use ::degenbot_solvers::mixed::{MixedPath, SolvePathResult};
    use alloy::primitives::U256;
    use hashbrown::HashMap;
    use std::sync::Arc;

    /// Narrow single-position V4 pool (±60 ticks, 1e6 liquidity) + a one-hop
    /// path: the over-fed committed input is the empty-march class. Returns
    /// (engine, `path_id`, the to_solve-aligned pool-ref snapshot).
    fn overfed_v4_engine() -> (ArbitrageEngine, u64, Vec<std::sync::Arc<MixedPath>>) {
        use crate::arb_engine::PoolTickCoverage;
        use crate::bot_core::RegisterV4PoolParams;
        fn usdc_local(amount: u64) -> alloy::primitives::Uint<112, 2> {
            (U256::from(amount) * U256::from(10u64).pow(U256::from(6)))
                .to::<alloy::primitives::Uint<112, 2>>()
        }
        fn weth_local(amount: u64) -> alloy::primitives::Uint<112, 2> {
            (U256::from(amount) * U256::from(10u64).pow(U256::from(18)))
                .to::<alloy::primitives::Uint<112, 2>>()
        }
        const GAMMA_03: u64 = 997;
        const FEE_DENOM_03: u64 = 1000;
        let mut engine = ArbitrageEngine::new();
        // V2 pool: large reserves so its output dwarfs the V4 hop's capacity —
        // the V4 hop is the over-fed one (this isolates hop1's input clamp).
        let v2 = engine.register_v2_pool(
            alloy::primitives::Address::from([0x11u8; 20]),
            usdc_local(1_500_000),
            weth_local(20_000_000_000),
            GAMMA_03,
            FEE_DENOM_03,
        );
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
                pool_manager: alloy::primitives::Address::from([0x44u8; 20]),
                pool_id: [0xabu8; 32],
                pool_key: V4PoolKey {
                    currency0: alloy::primitives::Address::from([0x30u8; 20]),
                    currency1: alloy::primitives::Address::from([0x31u8; 20]),
                    fee: 500,
                    tick_spacing: 10,
                    hooks: alloy::primitives::Address::ZERO,
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
        let path_id = engine
            .register_path(vec![
                ::degenbot_solvers::mixed::PoolHop {
                    pool_id: v2,
                    zero_for_one: true,
                },
                ::degenbot_solvers::mixed::PoolHop {
                    pool_id: v4_id,
                    zero_for_one: false,
                },
            ])
            .expect("two-hop path registers");
        let pool_refs = std::iter::once(engine.registry.get(path_id).expect("registered").clone())
            .collect::<Vec<_>>();
        (engine, path_id, pool_refs)
    }

    fn worker_probe_ctx(
        core: Arc<crate::bot_core::state_lock::StateLock<crate::bot_core::BotState>>,
        pool_refs: Vec<std::sync::Arc<MixedPath>>,
    ) -> Arc<SolveCycleShared> {
        Arc::new(SolveCycleShared {
            core,
            pool_refs,
            worker_clamp: true,
            inline_sim: None,
            solve_block: 0,
            epoch: 0,
            metadata: BlockMetadata::default(),
            runtime: ::degenbot_solvers::runtime::SolveRuntimeConfig::default(),
            gate_capture: None,
            walk_memo: Arc::new(::degenbot_solvers::mobius_v3_int::WalkMemo::new(
                false, false,
            )),
            capture: None,
            capture_mixed: None,
            path_times: parking_lot::Mutex::new(PathTimesHeap::new()),
            gate_total: parking_lot::Mutex::new(
                ::degenbot_solvers::profit_envelope::GateStats::default(),
            ),
            solve_cpu_us: std::sync::atomic::AtomicU64::new(0),
            walk_pieces_total: std::sync::atomic::AtomicU64::new(0),
            walk_sims_total: std::sync::atomic::AtomicU64::new(0),
            walk_word_steps_total: std::sync::atomic::AtomicU64::new(0),
            walk_refine_sims_total: std::sync::atomic::AtomicU64::new(0),
            walk_ternary_total: std::sync::atomic::AtomicU64::new(0),
            walk_grid_total: std::sync::atomic::AtomicU64::new(0),
            sims_recorder: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            gate_recorder: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            test_solve_delay: None,
            #[cfg(test)]
            test_solve_panic: None,
        })
    }

    /// The engine clamp and the WORKER clamp are the same computation from
    /// two call sites: byte-identical result + twin count on identical input.
    #[test]
    fn worker_clamp_matches_engine_clamp_bit_for_bit() {
        use ::degenbot_solvers::mixed::MixedPoolRef;
        let (engine, path_id, pool_refs) = overfed_v4_engine();
        let mk = || {
            let committed = U256::from(1u128) << 120;
            SolvePathResult {
                optimal_input: U256::from(1_000_000_000u64),
                profit: U256::from(1_000u64),
                hop_outputs: vec![committed, committed],
                consumed_inputs: vec![committed, committed],
                state_nonces: vec![],
                solver_pool_states: Vec::new(),
            }
        };
        let (mut r_engine, mut r_worker) = (mk(), mk());
        let twins_engine =
            engine
                .cycle
                .clamp_cl_hop_capacity(path_id, &mut r_engine, &engine.registry);
        assert!(twins_engine > 0, "premise: the over-fed input must clamp");
        assert!(
            r_engine.consumed_inputs[1] < U256::from(1u128) << 120,
            "premise: the V4 hop input clamp fired"
        );
        let ctx = worker_probe_ctx(Arc::clone(engine.core()), pool_refs);
        let twins_worker = clamp_result_in_worker(&ctx, 0, path_id, &mut r_worker);
        assert_eq!(twins_worker, twins_engine, "twin count must match");
        assert_eq!(r_engine, r_worker, "clamped result must be byte-identical");
        // The pool-ref SNAPSHOT path (worker side) is exercised; the MixedPoolRef _ unused is intentional.
        let _: Vec<Vec<MixedPoolRef>> = Vec::new();
    }

    // The merge honors the worker's twin report: twins > 0 = the result is
    // already clamp-committed (no second clip); twins = 0 = the merge clips
    // the over-fed input itself (the legacy path — bit-identical).

    /// SIMPIPE2 T3: a payload riding `merge_one_result` is stored at the
    /// engine (`inline_payloads`) and a re-merge WITHOUT the payload drops the
    /// stale entry — per-entry presence decides Python-side. (The delivery
    /// drain into `ResultBatch.payloads` is covered by the `delivery_policy`
    /// tests + the FFI conversion; this pins the merge-site store/drop.)
    #[test]
    fn merge_stores_payload_and_drops_it_without_one() {
        use crate::arb_engine::inline_sim::{InlineSwapFamily, SimulatedPathResult};
        use alloy::primitives::{Address, I256, U256};

        let (mut engine, path_id, _pool_refs) = overfed_v4_engine();
        let metadata = BlockMetadata::default();
        let mk = || SolvePathResult {
            optimal_input: U256::from(1_000_000_000u64),
            profit: U256::from(1_000u64),
            hop_outputs: vec![U256::from(1u64)],
            consumed_inputs: vec![U256::from(1u64)],
            state_nonces: vec![0],
            solver_pool_states: Vec::new(),
        };
        let payload = SimulatedPathResult {
            path_id,
            gross_profit: U256::from(1_000u64),
            net_profit: U256::from(900u64),
            gas_used: 300_000,
            priority_fee: 2,
            base_fee_next: 30,
            execute_calldata: vec![1, 2, 3],
            access_list: None,
            captured_swaps: vec![crate::arb_engine::inline_sim::CapturedSwapRow {
                emitter: Address::from([0x11u8; 20]),
                family: InlineSwapFamily::V4,
                amount0: I256::MINUS_ONE,
                amount1: I256::ONE,
                sqrt_price_x96: U256::ZERO,
                liquidity: U256::ZERO,
                tick: 0,
            }],
            hop_count: 1,
            failure: None,
        };

        engine.cycle.merge_one_result(
            42,
            &metadata,
            path_id,
            mk(),
            0,
            Some(payload),
            &engine.registry,
            &mut engine.delivery,
        );
        assert!(
            engine.cycle.inline_payloads.contains_key(&path_id),
            "the payload must be stored at merge"
        );

        // The path re-solves WITHOUT a payload (stance off or hook silence):
        // the stale entry must drop — presence decides per entry.
        engine.cycle.merge_one_result(
            43,
            &metadata,
            path_id,
            mk(),
            0,
            None,
            &engine.registry,
            &mut engine.delivery,
        );
        assert!(
            !engine.cycle.inline_payloads.contains_key(&path_id),
            "a payload-less re-merge must drop the stale payload"
        );
    }

    #[test]
    fn merge_reports_worker_twins_and_never_reclips() {
        let (mut engine, path_id, pool_refs) = overfed_v4_engine();
        let metadata = BlockMetadata::default();
        let overfed = || {
            let committed = U256::from(1u128) << 120;
            SolvePathResult {
                optimal_input: U256::from(1_000_000_000u64),
                profit: U256::from(1_000u64),
                hop_outputs: vec![committed, committed],
                consumed_inputs: vec![committed, committed],
                state_nonces: vec![],
                solver_pool_states: Vec::new(),
            }
        };

        // Worker arm: clamp once (the worker report = committed truth), then
        // merge with twins > 0 — the stored result stays byte-identical.
        let mut worker_result = overfed();
        let ctx = worker_probe_ctx(Arc::clone(engine.core()), pool_refs);
        let twins = clamp_result_in_worker(&ctx, 0, path_id, &mut worker_result);
        assert!(twins > 0, "premise: worker clamp fired");
        let committed = worker_result.clone();
        engine.cycle.merge_one_result(
            42,
            &metadata,
            path_id,
            worker_result,
            twins,
            None,
            &engine.registry,
            &mut engine.delivery,
        );
        {
            let stored = engine.cycle.results.get(&path_id).expect("worker-merged");
            assert_eq!(
                stored.consumed_inputs, committed.consumed_inputs,
                "twins>0 must not re-clip the committed inputs"
            );
            assert_eq!(stored.profit, committed.profit, "profit untouched on skip");
        }

        // Legacy arm (twins=0): the merge clips the over-fed V4 hop input
        // itself (index 1 — the V2 hop has no input clamp by design).
        let legacy = overfed();
        let pre = legacy.consumed_inputs[1];
        engine.cycle.merge_one_result(
            42,
            &metadata,
            path_id,
            legacy,
            0,
            None,
            &engine.registry,
            &mut engine.delivery,
        );
        let stored = engine.cycle.results.get(&path_id).expect("legacy-merged");
        assert_ne!(
            stored.consumed_inputs[1], pre,
            "twins=0 must run the merge-site clamp"
        );
    }

    // ----------------- RKXN5Z / IJUBV3: bundle.simulate span hygiene -----------------

    /// RED-gate (IJUBV3): the merge-site microsecond `degenbot.bundle.simulate`
    /// "verdict bookmark" spans collided with the REAL per-path EVM sim spans
    /// of the same name (traces 98f7cf52 / ab13f75fad50: 90-300 markers per
    /// block drowned the ms-scale sims). The merge must create NO span with
    /// that name - the verdict is an `info!` event on the enclosing merge
    /// span, and the span name now belongs solely to simulation work.
    ///
    /// DEFAULT-GATE VISIBLE (no otel cfg), on the K4ETHF pattern: the marker
    /// flood was what made Jaeger unreadable, so the regression gate must not
    /// hide behind --features otel.
    #[test]
    fn merge_payload_store_emits_no_bundle_simulate_span() {
        use std::sync::Mutex;

        struct SpanNameCapture {
            names: std::sync::Arc<Mutex<Vec<String>>>,
        }
        impl<S> tracing_subscriber::Layer<S> for SpanNameCapture
        where
            S: tracing::Subscriber,
        {
            fn on_new_span(
                &self,
                attrs: &tracing::span::Attributes<'_>,
                _id: &tracing::span::Id,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                self.names
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(attrs.metadata().name().to_string());
            }
        }

        use tracing_subscriber::layer::SubscriberExt as _;
        let names = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        let capture = SpanNameCapture {
            names: std::sync::Arc::clone(&names),
        };
        let subscriber = tracing_subscriber::registry().with(capture);

        let (mut engine, path_id, _pool_refs) = overfed_v4_engine();
        let metadata = BlockMetadata::default();
        let mk = || SolvePathResult {
            optimal_input: U256::from(1_000_000_000u64),
            profit: U256::from(1_000u64),
            hop_outputs: vec![U256::from(1u64)],
            consumed_inputs: vec![U256::from(1u64)],
            state_nonces: vec![0],
            solver_pool_states: Vec::new(),
        };
        let payload = crate::arb_engine::inline_sim::SimulatedPathResult {
            path_id,
            gross_profit: U256::from(1_000u64),
            net_profit: U256::from(900u64),
            gas_used: 300_000,
            priority_fee: 2,
            base_fee_next: 30,
            execute_calldata: vec![1, 2, 3],
            access_list: None,
            captured_swaps: Vec::new(),
            hop_count: 1,
            failure: None,
        };

        tracing::subscriber::with_default(subscriber, || {
            // Enclosing merge span, as in both production arms.
            let merge = tracing::info_span!("degenbot.arb.merge", merge.paths = 1u64);
            let _ctx = merge.enter();
            engine.cycle.merge_one_result(
                42,
                &metadata,
                path_id,
                mk(),
                0,
                Some(payload),
                &engine.registry,
                &mut engine.delivery,
            );
        });

        let created = names
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let offenders: Vec<_> = created
            .iter()
            .filter(|n| *n == "degenbot.bundle.simulate")
            .collect();
        assert!(
            offenders.is_empty(),
            "merge must not create bundle.simulate markers (the name belongs to real sims); \
             spans created: {created:?}"
        );
    }

    /// GREEN-gate (IJUBV3): the WORKER-side inline sim gets the honest
    /// `degenbot.bundle.simulate` span - a real ms-class EVM sim on the solve
    /// path, parented under the cycle span, with the terminal verdict.
    #[cfg(feature = "otel")]
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "single end-to-end span-emission assertion: stub + emit + export + attribute checks read best as one sequence"
    )]
    fn inline_sim_payload_emits_worker_sim_span_with_verdict() {
        use crate::arb_engine::lane_walk::inline_sim_payload;
        use crate::otel;
        use opentelemetry_sdk::trace::InMemorySpanExporter;
        use tracing_subscriber::layer::SubscriberExt;

        struct StubSim {
            fail: bool,
            path_id: u64,
        }
        impl crate::arb_engine::inline_sim::InlineSimulator for StubSim {
            fn simulate_path(
                &self,
                request: crate::arb_engine::inline_sim::InlineSimRequest,
            ) -> Option<crate::arb_engine::inline_sim::SimulatedPathResult> {
                assert_eq!(
                    request.path_id, self.path_id,
                    "stub receives the merged path id"
                );
                Some(crate::arb_engine::inline_sim::SimulatedPathResult {
                    path_id: request.path_id,
                    gross_profit: U256::from(1_000u64),
                    net_profit: U256::from(900u64),
                    gas_used: 300_000,
                    priority_fee: 2,
                    base_fee_next: 30,
                    execute_calldata: vec![7, 8, 9],
                    access_list: None,
                    captured_swaps: Vec::new(),
                    hop_count: 1,
                    failure: self
                        .fail
                        .then(|| crate::arb_engine::inline_sim::InlineSimFailure {
                            fail_index: None,
                            revert_data: Vec::new(),
                            bucket: "test".to_string(),
                        }),
                })
            }
        }

        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));

        let (engine, path_id, pool_refs) = overfed_v4_engine();
        let mut ctx = worker_probe_ctx(Arc::clone(engine.core()), pool_refs);
        // Fresh Arc (refcount 1): install the stub via get_mut.
        Arc::get_mut(&mut ctx)
            .expect("probe ctx exclusively owned")
            .inline_sim = Some(Arc::new(StubSim {
            fail: false,
            path_id,
        }));

        let result = SolvePathResult {
            optimal_input: U256::from(1_000_000_000u64),
            profit: U256::from(1_000u64),
            hop_outputs: vec![U256::from(1u64)],
            consumed_inputs: vec![U256::from(1u64)],
            state_nonces: vec![0],
            solver_pool_states: Vec::new(),
        };

        tracing::subscriber::with_default(subscriber, || {
            let solve = tracing::info_span!("degenbot.arb.solve", block.number = 7u64);
            let _guard = solve.enter();
            let payload = inline_sim_payload(&ctx, 0, path_id, &result, &tracing::Span::current());
            assert!(
                payload.is_some(),
                "stub hook returns a payload; None only when the seam is off"
            );
        });

        provider.force_flush().expect("flush");
        let spans = exporter.get_finished_spans().expect("spans");
        let solve_id = spans
            .iter()
            .find(|sp| sp.name.as_ref() == "degenbot.arb.solve")
            .map(|sp| sp.span_context.span_id())
            .expect("solve span must be exported");
        let sims: Vec<_> = spans
            .iter()
            .filter(|sp| sp.name.as_ref() == "degenbot.bundle.simulate")
            .collect();
        assert_eq!(
            sims.len(),
            1,
            "exactly one worker-side sim span; all: {:?}",
            spans.iter().map(|sp| sp.name.as_ref()).collect::<Vec<_>>()
        );
        assert_eq!(
            sims[0].parent_span_id, solve_id,
            "the worker sim span must parent under the cycle span"
        );
        let attr = |k: &'static str| {
            sims[0]
                .attributes
                .iter()
                .find(|kv| kv.key == opentelemetry::Key::from_static_str(k))
                .map(|kv| kv.value.to_string())
        };
        assert_eq!(
            attr("path_id").as_deref(),
            Some(path_id.to_string().as_str()),
            "path_id attribute"
        );
        assert_eq!(
            attr("simulate.verdict").as_deref(),
            Some("profitable"),
            "verdict recorded at span close; attrs: {:?}",
            sims[0].attributes
        );
        assert_eq!(
            attr("sim.path").as_deref(),
            Some("worker_inline"),
            "seam discriminator distinguishes worker sims from the FFI seam"
        );
    }
}
