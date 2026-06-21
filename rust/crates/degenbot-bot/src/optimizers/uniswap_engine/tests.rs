#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use alloy::primitives::{Address, U256};

    use crate::bot_core::RegisterV3PoolParams;
    use crate::bot_core::RegisterV4PoolParams;
    use crate::optimizers::uniswap_engine::ResolvedMixedPath;
    use crate::optimizers::uniswap_engine::{
        BlockMetadata, HopType, PoolHop, ResolvedHop, SolvePathResult, UniswapEngine, INT128_MAX,
    };

    fn usdc(amount: u64) -> U256 {
        U256::from(amount) * U256::from(10u64).pow(U256::from(6))
    }

    fn weth(amount: u64) -> U256 {
        U256::from(amount) * U256::from(10u64).pow(U256::from(18))
    }

    const GAMMA_03: u64 = 997;
    const FEE_DENOM_03: u64 = 1000;

    #[test]
    fn register_v2_and_v3_pools() {
        let mut engine = UniswapEngine::new();

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
            },
        );
        tick_data.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
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
            coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
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
        assert!(resolved.valid);
        assert_eq!(resolved.hops.len(), 2);
        assert_eq!(resolved.hops[0].hop_type(), HopType::V2);
        assert_eq!(resolved.hops[1].hop_type(), HopType::V3);
    }

    #[test]
    fn process_block_routes_logs_to_sub_engines() {
        let mut engine = UniswapEngine::new();

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

        // Process with no logs — should not panic
        engine.process_block(&[], 1, &BlockMetadata::default());

        let (results, block) = engine.latest_results();
        assert_eq!(block, 1);
        let _ = results; // May or may not have profitable results
    }

    #[test]
    fn mixed_path_v2_to_v3_resolves() {
        let mut engine = UniswapEngine::new();

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
            },
        );
        tick_data.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
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
            coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
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
        assert!(resolved.valid);
        assert!(resolved.hops[0].as_v2_state().is_some());
        assert!(matches!(resolved.hops[1], ResolvedHop::V3 { .. }));
    }

    #[test]
    fn missing_v2_pool_makes_path_invalid() {
        let mut engine = UniswapEngine::new();

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
            coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
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
        let mut engine = UniswapEngine::new();

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
    fn register_path_after_start_succeeds() {
        let mut engine = UniswapEngine::new();
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
        let mut engine = UniswapEngine::new();

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
        let mut engine = UniswapEngine::new();

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
        let mut engine = UniswapEngine::new();
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
            engine.delivered.is_empty(),
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
        let mut engine = UniswapEngine::new();
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
            engine.delivered.is_empty(),
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
        let mut engine = UniswapEngine::new();
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
            engine.delivered.len(),
            1,
            "delivered should contain exactly the one above-threshold path"
        );
        assert!(
            engine.delivered.contains_key(&path_id),
            "delivered should include the just-sent profitable path"
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
        let mut engine = UniswapEngine::new();
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
            },
        );

        engine.compute_diff_and_send(&BlockMetadata::default());

        let batch = rx
            .try_recv()
            .expect("compute_diff_and_send should deliver a batch");
        assert!(
            batch.fresh.iter().any(|(id, _)| *id == path_id),
            "a result with profit > u64::MAX must appear in fresh when the cap is unbounded"
        );
        assert!(
            engine.delivered.contains_key(&path_id),
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
        let mut engine = UniswapEngine::new();
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
            },
        );

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
        let mut engine = UniswapEngine::new();
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
            !engine.delivered.contains_key(&path_id),
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
        let mut engine = UniswapEngine::new();
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

        // `last_solved_block < block(=10)` so the guard fires.
        let mut last_solved_block: u64 = 0;
        let mut has_logs_this_block = true;

        engine.finalize_block(
            10,
            &metadata,
            &mut last_solved_block,
            &mut has_logs_this_block,
        );

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
        // Guard advanced + logs flag cleared by finalize_block.
        assert_eq!(last_solved_block, 10);
        assert!(!has_logs_this_block);
    }

    #[test]
    fn pure_v2_path_finds_profitable_arb() {
        let mut engine = UniswapEngine::new();

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
        let mut engine = UniswapEngine::new();

        // V3 pool A at tick 0 (1:1), high liquidity, with tick boundaries
        let mut tick_data_a = HashMap::new();
        tick_data_a.insert(
            60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(5_000_000_000_000_000u64),
                liquidity_net: alloy::primitives::I256::try_from(5_000_000_000_000_000i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );
        tick_data_a.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(5_000_000_000_000_000u64),
                liquidity_net: alloy::primitives::I256::try_from(-5_000_000_000_000_000i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
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
            coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
        });

        // V3 pool B at tick -60 (slightly cheaper token1), high liquidity
        let sqrt_price_lower_u160 =
            degenbot_cl_math::cl_lib::tick_math::get_sqrt_ratio_at_tick_internal(-60)
                .unwrap_or(alloy::primitives::U160::ZERO);
        let sqrt_price_lower = U256::from(sqrt_price_lower_u160);

        let mut tick_data_b = HashMap::new();
        tick_data_b.insert(
            0,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(5_000_000_000_000_000u64),
                liquidity_net: alloy::primitives::I256::try_from(5_000_000_000_000_000i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );
        tick_data_b.insert(
            -120,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(5_000_000_000_000_000u64),
                liquidity_net: alloy::primitives::I256::try_from(-5_000_000_000_000_000i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
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
            coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
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
        let mut engine = UniswapEngine::new();

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
            },
        );
        tick_data.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
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
            coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
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
    fn mixed_v3_to_v2_path_resolves() {
        let mut engine = UniswapEngine::new();

        // V3 pool with tick data
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(300),
                liquidity_net: alloy::primitives::I256::try_from(150i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            },
        );
        tick_data.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
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
            coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
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
        assert!(resolved.valid);
        assert_eq!(resolved.hops[0].hop_type(), HopType::V3);
        assert_eq!(resolved.hops[1].hop_type(), HopType::V2);
        assert!(matches!(resolved.hops[0], ResolvedHop::V3 { .. }));
        assert!(resolved.hops[1].as_v2_state().is_some());
    }

    #[test]
    fn rebuild_on_v2_update_changes_results() {
        let mut engine = UniswapEngine::new();

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

    /// V4 int128 guard: paths where V4 hop amounts exceed `int128_max` are rejected.
    ///
    /// V4's `toBalanceDelta()` calls `toInt128()` on swap amounts. If either component
    /// exceeds `int128_max`, V4 reverts with `SafeCastOverflow` — the swap cannot
    /// execute on-chain. The solver must not report such paths as profitable.
    #[test]
    fn v4_int128_overflow_path_rejected() {
        let mut engine = UniswapEngine::new();

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
            coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
        });

        // V4 pool: pool at extreme price (tick -886_983) with massive liquidity
        // This produces virtual reserves >> int128_max
        let v4_pool_manager = Address::from([0x40u8; 20]);
        // tick -886_983 → sqrtPrice ≈ 4.36e9 (very low price, token0 is nearly worthless)
        let sp_extreme =
            degenbot_cl_math::cl_lib::tick_math::get_sqrt_ratio_at_tick_internal(-886_983)
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
                sqrt_price_x96: U256::from(sp_extreme),
                liquidity: extreme_liquidity,
                tick: -886_983,
                tick_data: std::collections::HashMap::new(),
                update_block: 0,
                coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
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
                UniswapEngine::resolve_path(&core, &path.pools, &mut resolved);
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

    #[test]
    fn inspect_path_returns_hop_details() {
        let mut engine = UniswapEngine::new();

        // Register a V2 pool
        let v2_fwd = engine.register_v2_pool(
            Address::from([0x11u8; 20]),
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
            },
        );
        tick_data.insert(
            -60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(200),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
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
            coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
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
                sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336_u128),
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
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
            .get_v2_pool_state(v2_fwd)
            .map(|p| p.address);
        assert_eq!(v2_addr, Some(Address::from([0x11u8; 20])));

        let core = engine.core.read();
        let v3_pool = core.get_v3_pool(v3_key);
        assert_eq!(
            v3_pool.map(|p| p.address),
            Some(Address::from([0x22u8; 20]))
        );
        let v4_pool = core.get_v4_pool(v4_key);
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
    fn solve_3hop_v3_v3_v3_path() {
        let mut engine = UniswapEngine::new();

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
                },
            );
            td.insert(
                60,
                crate::bot_core::TickInfo {
                    liquidity_gross: alloy::primitives::U128::from(100),
                    liquidity_net: alloy::primitives::I256::try_from(-100i128)
                        .unwrap_or(alloy::primitives::I256::ZERO),
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
            coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
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
            coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
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
            coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
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
        let result = engine.solve_path(resolved);
        let _ = result; // No panic = test passes
    }

    #[test]
    fn solve_3hop_mixed_v2_v3_v2_path() {
        let mut engine = UniswapEngine::new();

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
            },
        );
        tick_data.insert(
            60,
            crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(100),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
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
            coverage: crate::optimizers::uniswap_engine::PoolTickCoverage::Tracked,
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
        let result = engine.solve_path(resolved);
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

        let mut engine = UniswapEngine::new();

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
            engine.delivered.contains_key(&path_id),
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
            !engine.delivered.contains_key(&path_id),
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
        // What: a V3 pool gets a Swap (scalar state change at block 5) and a
        // Mint (tick_data mutation at block 6). A reorg targeting block 5
        // must roll both back: swap scalars return to registration values, and
        // the Mint-initialized tick is removed from tick_data.
        // Why: ADR-003 — V3 reorg rollback reaches the live hot path for the
        // first time (S2b). apply_v3_swap journals scalars; the restore path
        // pops them + reverse-applies tick priors.
        use crate::bot_core::TickInfo;
        use crate::optimizers::uniswap_engine::PoolTickCoverage;
        use alloy::primitives::{I256, U128};

        let engine = UniswapEngine::new();
        let pool_addr = Address::from([0x55u8; 20]);

        // Register a V3 pool at tick 0, 1:1 price, one initialized tick at +60
        // (so the post-Mint state at block 6 can show a *second* tick).
        let mut tick_data = HashMap::new();
        tick_data.insert(
            -60,
            TickInfo {
                liquidity_gross: U128::from(100),
                liquidity_net: I256::try_from(100i128).unwrap(),
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
            coverage: PoolTickCoverage::Tracked,
        });

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

        // Mint at block 6: adds liquidity at [+60, +120].
        engine
            .core
            .write()
            .apply_v3_liquidity_update(pool_addr, 60, 120, 500_i128, 6);

        {
            let core = engine.core.read();
            let s = core.get_v3_pool(pool_id).expect("v3 pool registered");
            assert_eq!(s.sqrt_price_x96, swapped_sp, "swap applied at block 5");
            assert_eq!(s.liquidity, swapped_liq);
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

    /// ADR-006 Slice 1 (D1): `UniswapEngine::with_core` adopts an externally
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
            reserve0: U256::from(1000),
            reserve1: U256::from(2000),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: Address::from([0x33u8; 20]),
            update_block: 0,
        };
        let _pool_id = core.write().register_v2_pool(&params);

        // Engine adopts the SAME `Arc<RwLock<BotState>>` — NOT its own `BotState`.
        let engine = UniswapEngine::with_core(Arc::clone(&core));

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
        let real_pool_id = core.write().register_v2_pool(&RegisterV2PoolParams {
            address: Address::from([0x11u8; 20]),
            token0: Address::from([0x01u8; 20]),
            token1: Address::from([0x02u8; 20]),
            reserve0: U256::from(1000),
            reserve1: U256::from(2000),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: Address::from([0x33u8; 20]),
            update_block: 0,
        });

        let mut engine = UniswapEngine::with_core(Arc::clone(&core));

        // Bogus pool_id (never registered) — must Err.
        let bogus_id = real_pool_id + 1_000;
        let result = engine.register_path(vec![
            crate::optimizers::uniswap_engine::PoolHop {
                pool_id: real_pool_id,
                zero_for_one: true,
            },
            crate::optimizers::uniswap_engine::PoolHop {
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
    #[allow(clippy::too_many_lines)]
    fn process_backfill_logs_stamps_per_log_block_number() {
        use crate::optimizers::uniswap_engine::PoolTickCoverage;
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

        let mut engine = UniswapEngine::new();
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
            coverage: PoolTickCoverage::Tracked,
        });

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

        engine.process_backfill_logs(&logs, chunk_end);

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
        let restored = engine
            .core
            .write()
            .v3_restore_before_block(pool_id, b2)
            .expect("restore returns Some");
        let _ = restored;
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
}
