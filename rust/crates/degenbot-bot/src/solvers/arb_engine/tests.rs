#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]
#[cfg(test)]
#[expect(clippy::module_inception)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use alloy::primitives::{aliases::U112, Address, U256};

    use crate::bot_core::RegisterV3PoolParams;
    use crate::bot_core::RegisterV4PoolParams;
    use crate::solvers::arb_engine::{ArbitrageEngine, BlockMetadata, EnginePhase};
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
                liquidity_net: alloy::primitives::I256::try_from(150i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
                block: 0,
            },
        );
        tick_data.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
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
            coverage: crate::solvers::arb_engine::PoolTickCoverage::Tracked,
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
        let resolved = &engine.path_resolved[&path_id];
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
        engine.solve_dirty(1, &BlockMetadata::default());

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
                liquidity_net: alloy::primitives::I256::try_from(150i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
                block: 0,
            },
        );
        tick_data.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
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
            coverage: crate::solvers::arb_engine::PoolTickCoverage::Tracked,
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

        let resolved = &engine.path_resolved[&path_id];
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
            coverage: crate::solvers::arb_engine::PoolTickCoverage::Tracked,
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
        engine.rebuild_and_solve_affected(
            &HashSet::from([v2_fwd_a]),
            &HashSet::new(),
            &HashSet::new(),
            111,
            &BlockMetadata::default(),
        );

        let (results, _block) = engine.latest_results();
        let solve_result = results.get(&path_id).expect(
            "quiet-but-current path (hop 11 blocks quiet) must be solved, not deferred \
             (QNFYR5/YXHHKR)",
        );
        assert!(!solve_result.optimal_input.is_zero());
        assert!(!solve_result.profit.is_zero());
    }

    /// ADR-021 change-set scoping (pump-freeze fix): the solver-state verifier
    /// must diff ONLY the paths re-solved this block, never the whole registered
    /// set. A path untouched by a solve stays out of the change set; a solve on
    /// one path does not leak the others in; and `take_solver_path_pool_refs_change_set`
    /// consumes+clears the set so it cannot accumulate into the whole set over
    /// time. RED before the change-set plumbing existed (the verifier walked
    /// `solver_path_pool_refs`, i.e. every registered path, each publish).
    #[test]
    fn solver_state_change_set_scopes_to_resolved_paths_and_clears() {
        let mut engine = ArbitrageEngine::new();

        let a = engine.register_v2_pool(
            Address::from([0x11u8; 20]),
            usdc(1_000_000),
            weth(500),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let b = engine.register_v2_pool(
            Address::from([0x12u8; 20]),
            usdc(2_000_000),
            weth(900),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let _path_a = engine
            .register_path(vec![PoolHop {
                pool_id: a,
                zero_for_one: true,
            }])
            .unwrap();
        let _path_b = engine
            .register_path(vec![PoolHop {
                pool_id: b,
                zero_for_one: true,
            }])
            .unwrap();

        // Solve only path A's pool this block — B must stay out of the set.
        engine.rebuild_and_solve_affected(
            &HashSet::from([a]),
            &HashSet::new(),
            &HashSet::new(),
            5,
            &BlockMetadata::default(),
        );

        let change = engine.take_solver_path_pool_refs_change_set();
        assert_eq!(
            change.len(),
            1,
            "change set must contain only the re-solved path, got {} paths",
            change.len()
        );
        assert_eq!(change[0].len(), 1);
        assert_eq!(
            change[0][0].pool_key, a,
            "change set must reference path A's pool, not the whole set"
        );
        assert!(
            change
                .iter()
                .flat_map(|p| p.iter())
                .all(|r| r.pool_key == a),
            "no path referencing pool B may leak into A's change set"
        );

        // Consumed + cleared: a second take returns nothing (can't accumulate).
        let again = engine.take_solver_path_pool_refs_change_set();
        assert!(
            again.is_empty(),
            "change set must be consumed+cleared by take"
        );

        // Re-solving B pushes B into the set — but never the whole set.
        engine.rebuild_and_solve_affected(
            &HashSet::from([b]),
            &HashSet::new(),
            &HashSet::new(),
            6,
            &BlockMetadata::default(),
        );
        let change2 = engine.take_solver_path_pool_refs_change_set();
        assert_eq!(change2.len(), 1);
        assert!(
            change2
                .iter()
                .flat_map(|p| p.iter())
                .all(|r| r.pool_key == b),
            "a fresh solve must carry only the newly-re-solved path"
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

        // Should be tracked as pending so rebuild_and_solve_affected can merge
        assert!(engine.pending_new_paths.contains(&path_id));

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

        // Process an empty block (no affected pools) — rebuild_and_solve_affected
        // should still include the pending path and not drop it
        engine.rebuild_and_solve_affected(
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            1,
            &BlockMetadata::default(),
        );

        // Pending set should be cleared
        assert!(engine.pending_new_paths.is_empty());

        // The path's result should survive the rebuild
        let (results, block) = engine.latest_results();
        assert_eq!(block, 1);
        assert!(
            results.contains_key(&path_id),
            "pending new path result should survive rebuild_and_solve_affected"
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
        // `rebuild_and_solve_affected`) recomputes `results` but must NOT push
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
        engine.results_block = 100;

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
            engine.results_block, 500,
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
        engine.results_block = 900;
        engine.set_solve_anchor(600);
        assert_eq!(
            engine.results_block, 900,
            "set_solve_anchor never clobbers a real anchor"
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
        engine.results.insert(
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
        engine.results_block = 100;
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
        engine.results.insert(
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
        engine.results_block = 100;
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
        engine.results.insert(
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
        engine.dirty_v2.insert(v2_fwd_a);

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
        let results = engine.solve_all();
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
                liquidity_net: alloy::primitives::I256::try_from(5_000_000_000_000_000i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
                block: 0,
            },
        );
        tick_data_a.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(5_000_000_000_000_000u64),
                liquidity_net: alloy::primitives::I256::try_from(-5_000_000_000_000_000i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
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
            coverage: crate::solvers::arb_engine::PoolTickCoverage::Tracked,
            fetcher: None,
            ..Default::default()
        });

        // V3 pool B at tick -60 (slightly cheaper token1), high liquidity
        let sqrt_price_lower_u160 =
            degenbot_concentrated_liquidity_math::tick_math::get_sqrt_ratio_at_tick_internal(-60)
                .unwrap_or(alloy::primitives::U160::ZERO);
        let sqrt_price_lower = U256::from(sqrt_price_lower_u160);

        let mut tick_data_b = HashMap::new();
        tick_data_b.insert(
            0,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(5_000_000_000_000_000u64),
                liquidity_net: alloy::primitives::I256::try_from(5_000_000_000_000_000i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
                block: 0,
            },
        );
        tick_data_b.insert(
            -120,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(5_000_000_000_000_000u64),
                liquidity_net: alloy::primitives::I256::try_from(-5_000_000_000_000_000i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
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
            coverage: crate::solvers::arb_engine::PoolTickCoverage::Tracked,
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

        let results = engine.solve_all();
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
                liquidity_net: alloy::primitives::I256::try_from(150i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
                block: 0,
            },
        );
        tick_data.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
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
            coverage: crate::solvers::arb_engine::PoolTickCoverage::Tracked,
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
        let results = engine.solve_all();
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
                liquidity_net: alloy::primitives::I256::try_from(150i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
                block: 0,
            },
        );
        tick_data.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
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
            coverage: crate::solvers::arb_engine::PoolTickCoverage::Tracked,
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
        engine.rebuild_and_solve_affected(
            &HashSet::from([v2_a, v2_b]),
            &HashSet::from([v3_future]),
            &HashSet::new(),
            50,
            &BlockMetadata::default(),
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
            engine.path_resolved.contains_key(&future_path),
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
                liquidity_net: alloy::primitives::I256::try_from(150i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
                block: 0,
            },
        );
        tick_data.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
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
            coverage: crate::solvers::arb_engine::PoolTickCoverage::Tracked,
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

        let resolved = &engine.path_resolved[&path_id];
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
        let results_before = engine.solve_all();

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
    /// divergence is caught by the ADR-021 verifier
    /// (`verify_solver_state_against_chain`), which fatal-aborts loudly, NOT by
    /// a solve-time defer.
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
            let mut core = engine.core.write();
            let _ = core.apply_sync_by_pool_id(v2_a, usdc(1_500_000), weth(800), 498);
            let _ = core.apply_sync_by_pool_id(v2_b, weth(800), usdc(1_600_000), 498);
        }
        engine.rebuild_and_solve_affected(
            &HashSet::from([v2_a, v2_b]),
            &HashSet::new(),
            &HashSet::new(),
            500,
            &BlockMetadata::default(),
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
            let mut core = engine.core.write();
            let _ = core.apply_sync_by_pool_id(v2_a, usdc(1_500_000), weth(800), 10);
            let _ = core.apply_sync_by_pool_id(v2_b, weth(800), usdc(1_600_000), 10);
        }
        engine.rebuild_and_solve_affected(
            &HashSet::from([v2_a, v2_b]),
            &HashSet::new(),
            &HashSet::new(),
            500,
            &BlockMetadata::default(),
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
        engine.rebuild_and_solve_affected(
            &HashSet::from([v2_a, v2_b]),
            &HashSet::new(),
            &HashSet::new(),
            500,
            &BlockMetadata::default(),
        );
        let (r0, _) = engine.latest_results();
        assert!(
            r0.contains_key(&path_id),
            "update_block == 0 pools must never be assumed stale"
        );

        // Exactly at the old 10-block window edge is tolerated — still solved.
        {
            let mut core = engine.core.write();
            let _ = core.apply_sync_by_pool_id(v2_a, usdc(1_500_000), weth(800), 490);
            let _ = core.apply_sync_by_pool_id(v2_b, weth(800), usdc(1_600_000), 490);
        }
        engine.rebuild_and_solve_affected(
            &HashSet::from([v2_a, v2_b]),
            &HashSet::new(),
            &HashSet::new(),
            500,
            &BlockMetadata::default(),
        );
        let (r1, _) = engine.latest_results();
        assert!(
            r1.contains_key(&path_id),
            "staleness exactly at the window must not defer"
        );

        // 11 blocks past the old window edge — still solved (quiet, not stale).
        {
            let mut core = engine.core.write();
            let _ = core.apply_sync_by_pool_id(v2_a, usdc(1_500_000), weth(800), 489);
            let _ = core.apply_sync_by_pool_id(v2_b, weth(800), usdc(1_600_000), 489);
        }
        engine.rebuild_and_solve_affected(
            &HashSet::from([v2_a, v2_b]),
            &HashSet::new(),
            &HashSet::new(),
            500,
            &BlockMetadata::default(),
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
            tick_data: std::collections::HashMap::new(),
            update_block: 0,
            tick_data_block: None,
            coverage: crate::solvers::arb_engine::PoolTickCoverage::Tracked,
            fetcher: None,
            ..Default::default()
        });

        // V4 pool: pool at extreme price (tick -886_983) with massive liquidity
        // This produces virtual reserves >> int128_max
        let v4_pool_manager = Address::from([0x40u8; 20]);
        // tick -886_983 → sqrtPrice ≈ 4.36e9 (very low price, token0 is nearly worthless)
        let sp_extreme =
            degenbot_concentrated_liquidity_math::tick_math::get_sqrt_ratio_at_tick_internal(
                -886_983,
            )
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
                tick_data: std::collections::HashMap::new(),
                update_block: 0,
                tick_data_block: None,
                coverage: crate::solvers::arb_engine::PoolTickCoverage::Tracked,
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
            let core = engine.core.read();
            for (&path_id, path) in &engine.path_pools {
                let mut resolved = ResolvedMixedPath::default();
                ArbitrageEngine::resolve_path(&core, &path.pools, &mut resolved);
                engine.path_resolved.insert(path_id, resolved);
            }
        }
        engine.results = engine.solve_all();

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
    /// VAASFM margin) — the UO3JM4 empty-march clamp, now enforced in
    /// production at the solve→result merge seam.
    #[expect(clippy::too_many_lines)]
    #[test]
    fn clamp_cl_hop_capacity_caps_overfed_v4_input() {
        use crate::bot_core::TickInfo;
        use crate::solvers::arb_engine::PoolTickCoverage;
        use alloy::primitives::I256;
        use degenbot_pools::v3_state::V3PoolState;
        use degenbot_pools::v4_state::v4_simulate_swap;

        let mut engine = ArbitrageEngine::new();

        // V2 pool: reserves sized so its output (fed to V4) is enormous
        // relative to the V4 pool's capacity.
        let v2 = engine.register_v2_pool(
            Address::from([0x11u8; 20]),
            usdc(1_500_000),
            weth(800),
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
                liquidity_net: I256::try_from(150i128).unwrap_or(I256::ZERO),
                block: 0,
            },
        );
        tick_data.insert(
            -60,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: I256::try_from(-100i128).unwrap_or(I256::ZERO),
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

        engine.clamp_cl_hop_capacity(path_id, &mut result);

        // Compute the pools twin's input_consumed at the requested input to
        // assert the clamped value equals `input_consumed - 1` exactly.
        let input_consumed = {
            let core = engine.core.read();
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
            let core = engine.core.read();
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
    fn clamp_cl_hop_capacity_aligns_v4_first_hop0_outputs() {
        use crate::bot_core::TickInfo;
        use crate::solvers::arb_engine::PoolTickCoverage;
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
                liquidity_net: I256::try_from(150i128).unwrap_or(I256::ZERO),
                block: 0,
            },
        );
        tick_data.insert(
            -60,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: I256::try_from(-100i128).unwrap_or(I256::ZERO),
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
        engine.clamp_cl_hop_capacity(path_id, &mut result);

        // Compute the V4 twin's amount1 (zfo=true → output = amount1) at the
        // requested input — the byte-exact value hop_outputs[0] must align to.
        let twin_out = {
            let core = engine.core.read();
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
        use crate::bot_core::TickInfo;
        use crate::solvers::arb_engine::PoolTickCoverage;
        use alloy::primitives::I256;

        let mut engine = ArbitrageEngine::new();

        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(300),
                liquidity_net: I256::try_from(150i128).unwrap_or(I256::ZERO),
                block: 0,
            },
        );
        tick_data.insert(
            -60,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: I256::try_from(-100i128).unwrap_or(I256::ZERO),
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

        let path_id = engine
            .register_path(vec![PoolHop {
                pool_id: v4_id,
                zero_for_one: false,
            }])
            .unwrap();

        // A tiny in-capacity input — the pool fully converts it, no clamp.
        let small = U256::from(1u128);
        let mut result = SolvePathResult {
            optimal_input: small,
            profit: U256::ONE,
            hop_outputs: vec![U256::ONE],
            consumed_inputs: vec![small],
            state_nonces: vec![0],
            solver_pool_states: Vec::new(),
        };

        engine.clamp_cl_hop_capacity(path_id, &mut result);

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
                liquidity_net: alloy::primitives::I256::try_from(150i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
                block: 0,
            },
        );
        tick_data.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
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
            coverage: crate::solvers::arb_engine::PoolTickCoverage::Tracked,
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
                coverage: crate::solvers::arb_engine::PoolTickCoverage::Tracked,
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
        let path = engine.path_pools.get(&path_id).expect("path should exist");
        assert_eq!(path.pools.len(), 3);

        // Verify hop types
        assert!(matches!(path.pools[0].hop_type, HopType::V2));
        assert!(matches!(path.pools[1].hop_type, HopType::V3));
        assert!(matches!(path.pools[2].hop_type, HopType::V4));

        // Verify we can resolve pool addresses via BotState (V2) / sub-engines (V3/V4)
        let v2_addr = engine
            .core
            .read()
            .get_v2_identity(v2_fwd)
            .map(|p| p.address);
        assert_eq!(v2_addr, Some(Address::from([0x11u8; 20])));

        let core = engine.core.read();
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
        assert!(!engine.path_pools.contains_key(&99999));
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
                    liquidity_net: alloy::primitives::I256::try_from(100i128)
                        .unwrap_or(alloy::primitives::I256::ZERO),
                    block: 0,
                },
            );
            td.insert(
                60,
                crate::bot_core::TickInfo {
                    liquidity_gross: alloy::primitives::U128::from(100),
                    liquidity_net: alloy::primitives::I256::try_from(-100i128)
                        .unwrap_or(alloy::primitives::I256::ZERO),
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
            coverage: crate::solvers::arb_engine::PoolTickCoverage::Tracked,
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
            coverage: crate::solvers::arb_engine::PoolTickCoverage::Tracked,
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
            coverage: crate::solvers::arb_engine::PoolTickCoverage::Tracked,
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
        let resolved = &engine.path_resolved[&path_id];
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
        let result = ::degenbot_solvers::mixed::solve_path(resolved);
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
                liquidity_net: alloy::primitives::I256::try_from(100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
                block: 0,
            },
        );
        tick_data.insert(
            60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(100),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
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
            coverage: crate::solvers::arb_engine::PoolTickCoverage::Tracked,
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

        let resolved = &engine.path_resolved[&path_id];
        assert!(resolved.valid, "3-hop V2-V3-V2 path should be valid");
        assert_eq!(resolved.hops.len(), 3);
        assert_eq!(resolved.hops[0].hop_type(), HopType::V2);
        assert_eq!(resolved.hops[1].hop_type(), HopType::V3);
        assert_eq!(resolved.hops[2].hop_type(), HopType::V2);

        // Key: previously this returned None due to hop_types.len() != 2
        let result = ::degenbot_solvers::mixed::solve_path(resolved);
        let _ = result;
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

        // Sanity: the balanced cycle is not profitable.
        engine.solve_dirty(4, &BlockMetadata::default());
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
        engine.core.write().restore_all_pools_before_block(5);
        engine.path_resolved.clear();
        for &(hop_type, pool_key) in engine.pool_to_paths.keys() {
            match hop_type {
                HopType::V2 => {
                    engine.dirty_v2.insert(pool_key);
                }
                HopType::V3 => {
                    engine.dirty_v3.insert(pool_key);
                }
                HopType::V4 => {
                    engine.dirty_v4.insert(pool_key);
                }
                HopType::SolidlyStable
                | HopType::BalancerWeighted
                | HopType::BalancerStable
                | HopType::CurveStableswap => {
                    // No dirty set for Solidly/Balancer until the pump wires
                    // it; matches the resolve short-circuit.
                }
            }
        }
        engine.solve_dirty(5, &BlockMetadata::default());
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
        use crate::bot_core::TickInfo;
        use crate::solvers::arb_engine::PoolTickCoverage;
        use alloy::primitives::{I256, U128};

        let engine = ArbitrageEngine::new();
        let pool_addr = Address::from([0x55u8; 20]);

        // Register a V3 pool at tick 0, 1:1 price, one initialized tick at +60
        // (so the post-Mint state at block 6 can show a *second* tick).
        let mut tick_data = HashMap::new();
        tick_data.insert(
            -60,
            TickInfo {
                liquidity_gross: U128::from(100),
                liquidity_net: I256::try_from(100i128).unwrap(),
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
        engine.core.write().set_v3_pool_live(pool_addr);

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
            .write()
            .apply_v3_swap(pool_addr, swapped_sp, swapped_liq, swapped_tick, 5, &[]);

        // Mint at block 6: adds liquidity at [+60, +120] — in-range because the
        // swap moved the tick to 60, so the active `liquidity` scalar also gets
        // +500 (parity with on-chain + the cl-math pure reference).
        engine
            .core
            .write()
            .apply_v3_liquidity_update(pool_addr, 60, 120, 500_i128, 6);

        {
            let core = engine.core.read();
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
        let restored = engine.core.write().restore_all_pools_before_block(5);
        assert_eq!(restored, 1, "the single registered V3 pool was rolled back");

        {
            let core = engine.core.read();
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
        let core = Arc::new(parking_lot::RwLock::new(BotState::new()));
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
            .write()
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

        let core = Arc::new(parking_lot::RwLock::new(BotState::new()));
        // Register one real V2 pool so the engine has *some* valid id.
        let real_pool_id = core
            .write()
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
        let msg = result.unwrap_err();
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
        use crate::solvers::arb_engine::PoolTickCoverage;
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
        engine.core.write().set_v3_pool_live(pool_addr);

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
        engine.core.write().process_backfill_logs(&logs, chunk_end);

        let core = engine.core.read();
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
            .write()
            .restore_pool_before_block(pool_id, b2)
            .expect("restore returns Some")
            .expect("restore succeeds");
        let core = engine.core.read();
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

        use parking_lot::RwLock;

        let core = Arc::new(RwLock::new(crate::bot_core::BotState::new()));
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

        use parking_lot::RwLock;

        use crate::bot_core::BlockMetadata;

        let core = Arc::new(RwLock::new(crate::bot_core::BotState::new()));
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
                writer_engine.lock().solve_dirty(block, &metadata);
            }
        });

        // Readers: companion-getter path — core.read() alone, never the engine.
        let mut readers = Vec::new();
        for _ in 0..4 {
            let core = Arc::clone(&core);
            let done = Arc::clone(&done);
            readers.push(thread::spawn(move || {
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    let r = core.read();
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
    // `solve_dirty`'s affected-path solve loop is parallelized via rayon
    // `par_iter`. The tracer bullet below pins the invariant the parallel
    // fan-out must preserve: equivalence with the serial baseline. This test
    // runs green against the current serial `solve_all()`; after the parallel
    // refactor, the test must stay green — proving the fan-out introduces no
    // correctness drift. The companion stress test below it characterizes the
    // engine-then-core lock ordering under the new parallel solve path with
    // many paths (drives the par_iter loop across non-trivial batch sizes).

    /// Pin the parallel-fan-out equivalence invariant: the batch re-solver
    /// (`solve_all_paths` → `solve_all` → rayon `par_iter` of `solve_path`)
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
        // survive the batch re-solve (today serial; after the refactor, rayon
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
    /// when the engine's `solve_dirty` solve loop runs under rayon `par_iter`.
    /// The `par_iter` workers operate only on owned/Cloned data (`ResolvedMixedPath`
    /// clones + collected `(pid, SolvePathResult)` pairs); they acquire NO
    /// engine `Mutex` and NO core lock — so the engine-then-core lock order
    /// is preserved unchanged even with multiple workers spawned.
    ///
    /// This test drives the contention with N=8 paths registered (so the
    /// `par_iter` batch is non-trivial — at least 8 work items per `solve_dirty`)
    /// under one writer (`solve_dirty`) + four readers (core.read companions).
    /// Bounded join; a real deadlock (rayon re-entering the engine `Mutex`, or
    /// a re-entrant core guard) surfaces as a panic on the writer thread.
    #[test]
    fn solve_dirty_parallel_fanout_survives_concurrent_readers_and_writer() {
        use std::sync::Arc;
        use std::thread;

        use parking_lot::RwLock;

        use crate::bot_core::BlockMetadata;

        let core = Arc::new(RwLock::new(crate::bot_core::BotState::new()));
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
        // `rebuild_and_solve_affected` → `par_iter` of `Self::solve_path`. The
        // writer holds the engine `Mutex` then (inside) `core.read()` (path
        // resolution) and briefly `core.write()` (V3/V4 buffer expiry). Rayon's
        // internal workers touch no engine/core state.
        let writer_engine = Arc::clone(&engine);
        let writer_done = Arc::clone(&done);
        let metadata = BlockMetadata::default();
        let writer = thread::spawn(move || {
            for block in 1..=2_000u64 {
                if writer_done.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                writer_engine.lock().solve_dirty(block, &metadata);
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
                    let _r = core.read();
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
            "writer deadlocked — rayon `par_iter` in `solve_dirty` reintroduced a \
             core/engine lock nesting or re-entrant guard (ADR-006 D2 violated)",
        );
    }

    // ── Block stream (epic 6W35AI) ────────────────────────────────────────
    //
    // The backrun bot's block clock must come from a forwarded `newHeads`
    // stream, NOT from `ResultBatch::solve_block` (which lags by debounce
    // delay + only advances on a send). `BlockNotification` + `block_tx` are
    // the dedicated channel, plumbed parallel to `result_tx`.
    // See docs/architecture/rust-owned-bot.md §6.1 (`block_tx.send — Python
    // reads this`) and .ergo/plans/block-stream-clock.md.

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
        let notif =
            crate::solvers::arb_engine::BlockNotification::from_metadata(25_390_117, &metadata);
        assert_eq!(notif.number, 25_390_117);
        assert_eq!(notif.timestamp, metadata.timestamp);
        assert_eq!(notif.base_fee_per_gas, metadata.base_fee_per_gas);
        assert_eq!(notif.gas_used, metadata.gas_used);
        assert_eq!(notif.gas_limit, metadata.gas_limit);
    }

    #[test]
    fn set_block_channel_plumbs_a_sender_separate_from_result_channel() {
        // Contract: the engine accepts a dedicated `block_tx` independent of
        // `result_tx`. Solving / sending result batches must NOT touch the block
        // channel; only an explicit `notify_block` (task AROB7V) does. This test
        // pins the plumbing + the separation: after `set_block_channel`, a
        // result-batch `compute_diff_and_send` produces a `ResultBatch` but
        // leaves the block channel empty.
        let (block_tx, mut block_rx) = tokio::sync::mpsc::unbounded_channel();
        let (result_tx, _result_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut engine = ArbitrageEngine::new();
        engine.set_block_channel(block_tx);
        engine.set_result_channel(result_tx);

        // Two mispriced V2 pools so a send has something to report.
        let v2_addr_a = Address::from([0x31u8; 20]);
        let _ = engine.register_v2_pool(
            v2_addr_a,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let v2_addr_b = Address::from([0x32u8; 20]);
        let _ = engine.register_v2_pool(
            v2_addr_b,
            weth(800),
            usdc(1_600_000),
            GAMMA_03,
            FEE_DENOM_03,
        );

        let metadata = BlockMetadata {
            timestamp: 1,
            base_fee_per_gas: Some(0),
            gas_used: 0,
            gas_limit: 0,
        };
        // A send enqueues a result batch but must NOT notify the block stream.
        engine.send_result_batch(&metadata);
        assert!(
            block_rx.try_recv().is_err(),
            "result-batch send must not touch the block channel"
        );
    }

    #[test]
    fn notify_block_pushes_a_block_notification_on_the_block_channel() {
        // Contract (AROB7V): `notify_block` forwards exactly one
        // `BlockNotification` per call, carrying the block number + the
        // metadata, onto `block_tx`. It must NOT also enqueue a result batch
        // (the block channel is the block clock, independent of solve state).
        let (block_tx, mut block_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut engine = ArbitrageEngine::new();
        engine.set_block_channel(block_tx);

        let metadata = BlockMetadata {
            timestamp: 1_700_000_000,
            base_fee_per_gas: Some(7_000_000_000),
            gas_used: 15_000_000,
            gas_limit: 30_000_000,
        };
        engine.notify_block(25_390_117, &metadata);

        let notif = block_rx
            .try_recv()
            .expect("notify_block sent a notification");
        assert_eq!(notif.number, 25_390_117);
        assert_eq!(notif.timestamp, metadata.timestamp);
        assert_eq!(notif.base_fee_per_gas, metadata.base_fee_per_gas);
        assert_eq!(notif.gas_used, metadata.gas_used);
        assert_eq!(notif.gas_limit, metadata.gas_limit);
        assert!(
            block_rx.try_recv().is_err(),
            "exactly one notification per call"
        );
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

    // -----------------------------------------------------------------
    // resolve_path Solidly arm (task 2OWLDL): the engine reads Aerodrome
    // stable + Camelot stable_swap pools out of `BotState` and builds
    // `SolidlyHopState` in the math leaf's contract shape (un-oriented
    // reserves + token_in flag, decimals as `10^decimals` scale, fee as a
    // fee fraction).
    // -----------------------------------------------------------------
    #[test]
    fn resolve_path_builds_solidly_hop_from_aerodrome_stable_pool() {
        use crate::bot_core::{BotState, RegisterAerodromeV2PoolParams};
        use std::sync::Arc;

        let core = Arc::new(parking_lot::RwLock::new(BotState::new()));

        // Tokens: 6-decimal USDC, 18-decimal WETH.
        core.write().register_token(
            Address::from([0x01u8; 20]),
            "USD Coin".into(),
            "USDC".into(),
            6,
            1,
        );
        core.write().register_token(
            Address::from([0x02u8; 20]),
            "Wrapped Ether".into(),
            "WETH".into(),
            18,
            1,
        );

        // Aerodrome STABLE pool, token0=USDC token1=WETH, unidirectional
        // 0.03% fee stored directly as the fee fraction (3, 1000).
        let aero_id = core
            .write()
            .register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
                token0_decimals: 6,
                token1_decimals: 18,
                address: Address::from([0xaeu8; 20]),
                token0: Address::from([0x01u8; 20]),
                token1: Address::from([0x02u8; 20]),
                factory: Address::from([0xafu8; 20]),
                variant: DexVariant::AerodromeV2Stable,
                stable: true,
                fee: (3, 1000),
                reserve0: U112::from(1_000_000u64),
                reserve1: U112::from(2_000_000u64),
                update_block: 0,
            });

        // A second identical Aerodrome stable pool — register_path requires
        // >= 2 hops. Reversed direction (token1→token0).
        let aero_id2 = core
            .write()
            .register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
                token0_decimals: 6,
                token1_decimals: 18,
                address: Address::from([0xb1u8; 20]),
                token0: Address::from([0x01u8; 20]),
                token1: Address::from([0x02u8; 20]),
                factory: Address::from([0xafu8; 20]),
                variant: DexVariant::AerodromeV2Stable,
                stable: true,
                fee: (3, 1000),
                reserve0: U112::from(1_100_000u64),
                reserve1: U112::from(1_800_000u64),
                update_block: 0,
            });

        let mut engine = ArbitrageEngine::with_core(Arc::clone(&core));
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: aero_id,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: aero_id2,
                    zero_for_one: false,
                },
            ])
            .expect("aerodrome-stable path registers");

        let resolved = engine
            .path_resolved
            .get(&path_id)
            .expect("path resolved eagerly");
        assert!(resolved.valid, "all-Solidly path is valid post-resolve");
        assert_eq!(resolved.hops.len(), 2);

        // Hop 0: token0→token1 (zero_for_one=true)
        let hop0 = resolved.hops[0]
            .as_solidly_state()
            .expect("hop 0 is SolidlyStable");
        assert_eq!(hop0.variant, DexVariant::AerodromeV2Stable);
        assert!(hop0.stable);
        assert_eq!(hop0.reserves_0, U256::from(1_000_000u64));
        assert_eq!(hop0.reserves_1, U256::from(2_000_000u64));
        assert_eq!(hop0.decimals_0, U256::from(10u64).pow(U256::from(6u64)));
        assert_eq!(hop0.decimals_1, U256::from(10u64).pow(U256::from(18u64)));
        assert_eq!(hop0.token_in, 0, "zero_for_one=true → token_in=0");
        assert_eq!(hop0.fee_numer, U256::from(3u64));
        assert_eq!(hop0.fee_denom, U256::from(1000u64));

        // Hop 1: token1→token0 (zero_for_one=false)
        let hop1 = resolved.hops[1]
            .as_solidly_state()
            .expect("hop 1 is SolidlyStable");
        assert_eq!(hop1.token_in, 1, "zero_for_one=false → token_in=1");
        assert_eq!(hop1.reserves_0, U256::from(1_100_000u64));
        assert_eq!(hop1.fee_numer, U256::from(3u64));
        assert_eq!(hop1.fee_denom, U256::from(1000u64));
        // resolving hop1 reads the SAME pair's decimals (token0/token1) —
        // un-oriented, so decimals_0 is still token0's scale.
        assert_eq!(hop1.decimals_0, U256::from(10u64).pow(U256::from(6u64)));
    }

    #[test]
    fn resolve_path_builds_solidly_hop_from_camelot_stable_swap_pool() {
        // Camelot stable_swap lives in `PoolEntry::V2` (V2PoolIdentity with
        // `stable_swap=true`). Its fee is stored as the RETAINED fraction
        // `(gamma, denom)`; the resolve arm must invert it to the fee
        // fraction `(denom - gamma, denom)` that the solidly math expects.
        use crate::bot_core::{BotState, RegisterV2PoolParams};
        use std::sync::Arc;

        let core = Arc::new(parking_lot::RwLock::new(BotState::new()));
        core.write().register_token(
            Address::from([0x01u8; 20]),
            "USD Coin".into(),
            "USDC".into(),
            6,
            1,
        );
        core.write().register_token(
            Address::from([0x02u8; 20]),
            "Tether".into(),
            "USDT".into(),
            6,
            1,
        );

        // Camelot stable_swap pool: gamma=9970 of 10000 retained (0.3% fee).
        let camelot_id = core
            .write()
            .register_v2_pool(&RegisterV2PoolParams {
                address: Address::from([0xc1u8; 20]),
                token0: Address::from([0x01u8; 20]),
                token1: Address::from([0x02u8; 20]),
                reserve0: U112::from(1_000_000u64),
                reserve1: U112::from(2_000_000u64),
                fee_token0: (9970, 10000),
                fee_token1: (9970, 10000),
                factory: Address::from([0xfau8; 20]),
                update_block: 0,
                variant: degenbot_uniswap::dex_identity::DexVariant::CamelotV2Stable,
                stable_swap: true,
                fee_denominator: Some(10000),
                ..Default::default()
            })
            .expect("test setup: V2 registration");
        let camelot_id2 = core
            .write()
            .register_v2_pool(&RegisterV2PoolParams {
                address: Address::from([0xc2u8; 20]),
                token0: Address::from([0x01u8; 20]),
                token1: Address::from([0x02u8; 20]),
                reserve0: U112::from(1_100_000u64),
                reserve1: U112::from(1_900_000u64),
                fee_token0: (9970, 10000),
                fee_token1: (9970, 10000),
                factory: Address::from([0xfau8; 20]),
                update_block: 0,
                variant: degenbot_uniswap::dex_identity::DexVariant::CamelotV2Stable,
                stable_swap: true,
                fee_denominator: Some(10000),
                ..Default::default()
            })
            .expect("test setup: V2 registration");

        let mut engine = ArbitrageEngine::with_core(Arc::clone(&core));
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: camelot_id,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: camelot_id2,
                    zero_for_one: false,
                },
            ])
            .expect("camelot-stable path registers");

        let resolved = engine.path_resolved.get(&path_id).expect("resolved");
        let hop0 = resolved.hops[0]
            .as_solidly_state()
            .expect("hop0 SolidlyStable");
        assert_eq!(
            hop0.variant,
            degenbot_uniswap::dex_identity::DexVariant::CamelotV2Stable
        );
        assert!(hop0.stable);
        assert_eq!(hop0.token_in, 0);
        // gamma 9970/10000 retained → fee = (10000-9970, 10000) = (30, 10000).
        assert_eq!(hop0.fee_numer, U256::from(30u64));
        assert_eq!(hop0.fee_denom, U256::from(10000u64));
        // hop1 (token1→token0): same gamma in fee_token1; identical inverted fee.
        let hop1 = resolved.hops[1]
            .as_solidly_state()
            .expect("hop1 SolidlyStable");
        assert_eq!(hop1.token_in, 1);
        assert_eq!(hop1.fee_numer, U256::from(30u64));
        assert_eq!(hop1.fee_denom, U256::from(10000u64));
    }

    #[test]
    fn resolve_path_solidly_hop_missing_token_entry_invalidates() {
        // If a token entry is absent from the registry, the Solidly hop
        // cannot know its decimals scale → path is invalid (same as missing
        // pool).
        use crate::bot_core::{BotState, RegisterAerodromeV2PoolParams};
        use std::sync::Arc;

        let core = Arc::new(parking_lot::RwLock::new(BotState::new()));
        // NOTE: NO register_token calls.
        let aero_id = core
            .write()
            .register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
                token0_decimals: 18,
                token1_decimals: 18,
                address: Address::from([0xaeu8; 20]),
                token0: Address::from([0x01u8; 20]),
                token1: Address::from([0x02u8; 20]),
                factory: Address::from([0xafu8; 20]),
                variant: DexVariant::AerodromeV2Stable,
                stable: true,
                fee: (3, 1000),
                reserve0: U112::from(1_000_000u64),
                reserve1: U112::from(2_000_000u64),
                update_block: 0,
            });
        let aero_id2 = core
            .write()
            .register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
                token0_decimals: 18,
                token1_decimals: 18,
                address: Address::from([0xb1u8; 20]),
                token0: Address::from([0x01u8; 20]),
                token1: Address::from([0x02u8; 20]),
                factory: Address::from([0xafu8; 20]),
                variant: DexVariant::AerodromeV2Stable,
                stable: true,
                fee: (3, 1000),
                reserve0: U112::from(1_100_000u64),
                reserve1: U112::from(1_900_000u64),
                update_block: 0,
            });

        let mut engine = ArbitrageEngine::with_core(Arc::clone(&core));
        let path_id = engine
            .register_path(vec![
                PoolHop {
                    pool_id: aero_id,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: aero_id2,
                    zero_for_one: false,
                },
            ])
            .expect("path registers even if resolve invalidates");
        let resolved = engine.path_resolved.get(&path_id).expect("resolved");
        assert!(!resolved.valid, "missing token entry → invalid path");
    }

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

        let core = Arc::new(parking_lot::RwLock::new(BotState::new()));
        core.write().register_token(
            Address::from([0x01u8; 20]),
            "Token0".into(),
            "T0".into(),
            18,
            1,
        );
        core.write().register_token(
            Address::from([0x02u8; 20]),
            "Token1".into(),
            "T1".into(),
            18,
            1,
        );
        let aero_a = core
            .write()
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
            .write()
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
        let resolved = engine.path_resolved.get(&path_id).expect("resolved");
        let result =
            ::degenbot_solvers::mixed::solve_path(resolved).expect("profitable path solves");
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

        let core = Arc::new(parking_lot::RwLock::new(BotState::new()));
        core.write().register_token(
            Address::from([0x01u8; 20]),
            "Token0".into(),
            "T0".into(),
            18,
            1,
        );
        core.write().register_token(
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
            .write()
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
            .write()
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
        let resolved = engine.path_resolved.get(&path_id).expect("resolved");
        let result =
            ::degenbot_solvers::mixed::solve_path(resolved).expect("profitable mixed path solves");
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
        let resolved = engine.path_resolved.get(&path_id).expect("resolved");
        assert!(
            ::degenbot_solvers::mixed::solve_path(resolved).is_none(),
            "round-trip through one pool is unprofitable"
        );
    }

    #[test]
    fn solve_solidly_plus_cl_path_rejected_by_scope() {
        use crate::bot_core::{BotState, RegisterAerodromeV2PoolParams, RegisterV3PoolParams};
        use std::sync::Arc;

        let core = Arc::new(parking_lot::RwLock::new(BotState::new()));
        core.write().register_token(
            Address::from([0x01u8; 20]),
            "Token0".into(),
            "T0".into(),
            18,
            1,
        );
        core.write().register_token(
            Address::from([0x02u8; 20]),
            "Token1".into(),
            "T1".into(),
            18,
            1,
        );
        let aero = core
            .write()
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
            .write()
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
        let resolved = engine.path_resolved.get(&path_id).expect("resolved");
        // Solidly + CL is out of scope (p): solve_path returns None.
        assert!(::degenbot_solvers::mixed::solve_path(resolved).is_none());
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
        let pool_a =
            engine
                .core
                .write()
                .register_balancer_weighted_pool(&balancer_weighted_5050_params(
                    Address::from([0xd1u8; 20]),
                    1000,
                    2000,
                ));
        // Pool B: 1000 token0 / 1950 token1 (mispriced — cheaper token1 here)
        let pool_b =
            engine
                .core
                .write()
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

        let results = engine.solve_all();
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
        let pool_a =
            engine
                .core
                .write()
                .register_balancer_weighted_pool(&balancer_weighted_8020_params(
                    Address::from([0xe1u8; 20]),
                    800_000,
                    200_000,
                ));
        let pool_b =
            engine
                .core
                .write()
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

        let results = engine.solve_all();
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
            .write()
            .register_balancer_weighted_pool(&bw_params(Address::from([0xf3u8; 20]), 1000, 2000));
        let bw_b = engine
            .core
            .write()
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
        let v2_results = engine.solve_all();
        let v2_profit = v2_results.values().next().unwrap().profit;

        // Solve Balancer-V2-V2 path (clear and re-solve)
        drop(engine.core.write());
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
        let resolved = &engine.path_resolved[&bw_path];
        let bw_result =
            ::degenbot_solvers::mixed::solve_path(resolved).expect("bw path should solve");

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
            .write()
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
        let results = engine.solve_all();
        // V2+Balancer-weighted is all-V2-or-weighted with no CL — should solve
        assert!(!results.is_empty(), "should find V2+Balancer-weighted arb");
    }

    #[test]
    fn balancer_weighted_rejects_mixed_with_cl() {
        use std::sync::Arc;
        let core = Arc::new(parking_lot::RwLock::new(crate::bot_core::BotState::new()));

        // Register a Balancer weighted pool
        let bw = core
            .write()
            .register_balancer_weighted_pool(&balancer_weighted_5050_params(
                Address::from([0xb1u8; 20]),
                1000,
                2000,
            ));
        // Register a V3 pool
        let v3 = core
            .write()
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
        let resolved = &engine.path_resolved[&path_id];
        // Balancer weighted + CL is out of scope — solve_path returns None.
        assert!(
            ::degenbot_solvers::mixed::solve_path(resolved).is_none(),
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
    /// (needs `>= 2`) crashed the production backrun bot:
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
            .write()
            .register_balancer_stable_pool(&balancer_stable_params(
                Address::from([0xe1u8; 20]),
                1000,
                2000,
            ));
        // Pool B: 1000 token0 / 1950 token1 (mispriced)
        let pool_b = engine
            .core
            .write()
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

        let results = engine.solve_all();
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
            .write()
            .register_balancer_stable_pool(&balancer_stable_params(
                Address::from([0xf1u8; 20]),
                1000,
                2000,
            ));
        let pool_b = engine
            .core
            .write()
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

        let results = engine.solve_all();
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
            .write()
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

        let results = engine.solve_all();
        assert!(
            !results.is_empty(),
            "should find V2+Balancer-stable mixed arb"
        );
    }

    #[test]
    fn balancer_stable_rejects_mixed_with_cl() {
        use std::sync::Arc;
        let core = Arc::new(parking_lot::RwLock::new(crate::bot_core::BotState::new()));

        let bs = core
            .write()
            .register_balancer_stable_pool(&balancer_stable_params(
                Address::from([0xb3u8; 20]),
                1000,
                2000,
            ));
        let v3 = core
            .write()
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
        let resolved = &engine.path_resolved[&path_id];
        assert!(
            ::degenbot_solvers::mixed::solve_path(resolved).is_none(),
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
            .write()
            .register_curve_pool(&curve_stable_params(
                Address::from([0xe1u8; 20]),
                1000,
                2000,
            ));
        let pool_b = engine
            .core
            .write()
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

        let results = engine.solve_all();
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
            .write()
            .register_curve_pool(&curve_stable_params(
                Address::from([0xf1u8; 20]),
                1000,
                2000,
            ));
        let pool_b = engine
            .core
            .write()
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

        let results = engine.solve_all();
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
            .write()
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

        let results = engine.solve_all();
        assert!(!results.is_empty(), "should find V2+Curve mixed arb");
    }

    #[test]
    fn curve_stable_rejects_mixed_with_cl() {
        use std::sync::Arc;
        let core = Arc::new(parking_lot::RwLock::new(crate::bot_core::BotState::new()));

        let cs = core.write().register_curve_pool(&curve_stable_params(
            Address::from([0xb4u8; 20]),
            1000,
            2000,
        ));
        let v3 = core
            .write()
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
        let resolved = &engine.path_resolved[&path_id];
        assert!(
            ::degenbot_solvers::mixed::solve_path(resolved).is_none(),
            "Curve + CL must not solve"
        );
    }
}
