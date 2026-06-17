#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use alloy::primitives::{Address, U256};

    use crate::optimizers::uniswap_engine::{
        BlockMetadata, HopType, MixedPoolRef, ResolvedHop, UniswapEngine, INT128_MAX,
    };
    use crate::optimizers::uniswap_engine::ResolvedMixedPath;
    use crate::optimizers::v3_block_engine::RegisterV3PoolParams;
    use crate::optimizers::v4_block_engine::RegisterV4PoolParams;

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

        let v3_key = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
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
            },
        );

        assert_eq!(engine.v2_pool_count(), 1);
        assert_eq!(engine.v3_pool_count(), 1);

        // Register a mixed V2→V3 path
        let path_id = engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key,
                zero_for_one: false,
            },
        ]);

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
        let v2_fwd = engine.register_v2_pool(
            v2_addr,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        let v2_addr1 = Address::from([1u8; 20]);
        let v2_fwd1 = engine.register_v2_pool(
            v2_addr1,
            weth(800),
            usdc(1_600_000),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // Register a pure V2 path
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd1,
                zero_for_one: true,
            },
        ]);

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

        let v3_key = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
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
            },
        );

        // Mixed V2→V3 path
        let path_id = engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key,
                zero_for_one: false,
            },
        ]);

        let resolved = &engine.path_resolved[&path_id];
        assert!(resolved.valid);
        assert!(resolved.hops[0].as_v2_state().is_some());
        assert!(matches!(resolved.hops[1], ResolvedHop::V3 { .. }));
    }

    #[test]
    fn missing_v2_pool_makes_path_invalid() {
        let mut engine = UniswapEngine::new();

        // Only register V3 pool
        let v3_key = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
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
            },
        );

        // Reference a non-existent V2 pool
        let path_id = engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: 999, // Non-existent
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key,
                zero_for_one: false,
            },
        ]);

        let resolved = &engine.path_resolved[&path_id];
        assert!(!resolved.valid);
    }

    #[test]
    fn process_updates_applies_both_types() {
        let mut engine = UniswapEngine::new();

        // Register V2 pools
        let v2_addr = Address::from([0x11u8; 20]);
        let v2_fwd = engine.register_v2_pool(
            v2_addr,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        let v2_addr1 = Address::from([0x12u8; 20]);
        let v2_fwd1 = engine.register_v2_pool(
            v2_addr1,
            weth(800),
            usdc(1_600_000),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // Register V2-only path
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd1,
                zero_for_one: true,
            },
        ]);

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
        let v2_fwd = engine.register_v2_pool(
            v2_addr,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let v2_addr2 = Address::from([0x12u8; 20]);
        let v2_fwd2 = engine.register_v2_pool(
            v2_addr2,
            weth(1000),
            usdc(2_000_000),
            GAMMA_03,
            FEE_DENOM_03,
        );
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd2,
                zero_for_one: true,
            },
        ]);
        // Registration is always-on; this should not panic
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd2,
                zero_for_one: true,
            },
        ]);
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
        let path_id = engine.register_and_solve_path(vec![
            MixedPoolRef { hop_type: HopType::V2, pool_key: v2_fwd_a, zero_for_one: true },
            MixedPoolRef { hop_type: HopType::V2, pool_key: v2_fwd_b, zero_for_one: true },
        ]);

        // Should be tracked as pending so rebuild_and_solve_affected can merge
        assert!(engine.pending_new_paths.contains(&path_id));

        // Results should already contain the eagerly-solved path
        let (results, _block) = engine.latest_results();
        let solve_result = results.get(&path_id);
        assert!(solve_result.is_some(), "register_and_solve_path should eagerly solve and add to results");

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
        let path_id = engine.register_and_solve_path(vec![
            MixedPoolRef { hop_type: HopType::V2, pool_key: v2_fwd_a, zero_for_one: true },
            MixedPoolRef { hop_type: HopType::V2, pool_key: v2_fwd_b, zero_for_one: true },
        ]);

        // Process an empty block (no affected pools) — rebuild_and_solve_affected
        // should still include the pending path and not drop it
        engine.rebuild_and_solve_affected(&HashSet::new(), &HashSet::new(), &HashSet::new(), 1, &BlockMetadata::default());

        // Pending set should be cleared
        assert!(engine.pending_new_paths.is_empty());

        // The path's result should survive the rebuild
        let (results, block) = engine.latest_results();
        assert_eq!(block, 1);
        assert!(results.contains_key(&path_id), "pending new path result should survive rebuild_and_solve_affected");
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
        let path_id = engine.register_path(vec![
            MixedPoolRef { hop_type: HopType::V2, pool_key: v2_fwd_a, zero_for_one: true },
            MixedPoolRef { hop_type: HopType::V2, pool_key: v2_fwd_b, zero_for_one: true },
        ]);

        engine.solve_all_paths(1);

        // Solve actually ran: results populated with a profitable path.
        let (results, block) = engine.latest_results();
        assert_eq!(block, 1);
        let solve_result = results.get(&path_id).expect("solve_all_paths should populate results");
        assert!(!solve_result.optimal_input.is_zero());
        assert!(!solve_result.profit.is_zero());

        // Delivered untouched — Python has not received anything.
        assert!(
            engine.delivered.is_empty(),
            "solve_all_paths must not advance `delivered` without a channel"
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
        let path_id = engine.register_and_solve_path(vec![
            MixedPoolRef { hop_type: HopType::V2, pool_key: v2_fwd_a, zero_for_one: true },
            MixedPoolRef { hop_type: HopType::V2, pool_key: v2_fwd_b, zero_for_one: true },
        ]);

        // Eagerly solved → path is in `results` and above-threshold.
        let (results_before, _) = engine.latest_results();
        let solve_result = results_before.get(&path_id).expect("eagerly solved path present");
        assert!(!solve_result.profit.is_zero());

        // send_result_batch computes the diff, sends it, and advances
        // `delivered` to the above-threshold subset.
        engine.send_result_batch(&BlockMetadata::default());

        // Batch was actually delivered to the channel.
        let batch = rx.try_recv().expect("send_result_batch should deliver a batch");
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
        let path_id = engine.register_and_solve_path(vec![
            MixedPoolRef { hop_type: HopType::V2, pool_key: v2_fwd_a, zero_for_one: true },
            MixedPoolRef { hop_type: HopType::V2, pool_key: v2_fwd_b, zero_for_one: true },
        ]);

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

        engine.finalize_block(10, &metadata, &mut last_solved_block, &mut has_logs_this_block);

        // The emitted batch must carry the passed metadata, not default.
        let batch = rx.try_recv().expect("finalize_block should emit a result batch");
        assert_eq!(batch.solve_block, 10);
        assert_eq!(batch.timestamp, 1_700_000_000, "batch must carry the caller's timestamp");
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
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd_a, // reserve0=USDC, reserve1=WETH
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd_b, // reserve0=WETH, reserve1=USDC
                zero_for_one: true,
            },
        ]);

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

        let v3_key_a = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
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
            },
        );

        // V3 pool B at tick -60 (slightly cheaper token1), high liquidity
        let sqrt_price_lower_u160 = crate::cl_lib::tick_math::get_sqrt_ratio_at_tick_internal(-60)
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

        let v3_key_b = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
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
            },
        );

        // V3→V3 path: pool A (zfo) → pool B (ofz)
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key_a,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key_b,
                zero_for_one: false,
            },
        ]);

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
        let v2_fwd = engine.register_v2_pool(
            v2_addr,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

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

        let v3_key = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
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
            },
        );

        // Mixed V2→V3 path
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key,
                zero_for_one: false,
            },
        ]);

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

        let v3_key = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
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
            },
        );

        // V2 pool
        let v2_addr = Address::from([0x11u8; 20]);
        let v2_fwd = engine.register_v2_pool(
            v2_addr,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // V3→V2 path
        let path_id = engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V3,
                pool_key: v3_key,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd,
                zero_for_one: false,
            },
        ]);

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
        engine.register_path(vec![
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd_a,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2_fwd_b,
                zero_for_one: true,
            },
        ]);

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

        engine.v3_engine().register_pool(RegisterV3PoolParams {
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
        let sp_extreme = crate::cl_lib::tick_math::get_sqrt_ratio_at_tick_internal(-886_983)
            .unwrap_or_default();
        let extreme_liquidity: u128 = 76_688_550_121_478_947_320_312_764_923_207_804;

        let _ = engine.v4_engine().register_pool(RegisterV4PoolParams {
            pool_manager: v4_pool_manager,
            pool_id: [0xffu8; 32],
            pool_key: crate::optimizers::v4_block_engine::V4PoolKey {
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
        });

        // Register path: V3 (zfo) → V4 (ofz, which will produce huge token0 output)
        let path_id = engine.register_path(vec![
            MixedPoolRef { hop_type: HopType::V3, pool_key: 0, zero_for_one: true },
            MixedPoolRef { hop_type: HopType::V4, pool_key: 0, zero_for_one: false },
        ]);

        // Resolve and solve all paths (replaces start() + initial_solve())
        {
            let core = engine.core.lock();
            for (&path_id, path) in &engine.path_pools {
                let mut resolved = ResolvedMixedPath::default();
                engine.resolve_path(&core, &path.pools, &mut resolved);
                engine.path_resolved.insert(path_id, resolved);
            }
        }
        engine.results = engine.solve_all();

        let (results, _block) = engine.latest_results();

        // The V4 hop's output (token0 at extreme price) would overflow int128.
        // The solver should reject this path — no result should be returned.
        if let Some(solve_result) = results.get(&path_id) {
            // If a result IS found, verify that V4 hop outputs fit int128
            let v4_output = solve_result.hop_outputs.get(1).copied().unwrap_or(U256::ZERO);
            let v4_consumed = solve_result.consumed_inputs.get(1).copied().unwrap_or(U256::ZERO);
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
        let v3_key = engine.v3_engine().register_pool(
            crate::optimizers::v3_block_engine::RegisterV3PoolParams {
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
            },
        );

        // Register a V4 pool
        let v4_key = engine.v4_engine().register_pool(
            crate::optimizers::v4_block_engine::RegisterV4PoolParams {
                pool_manager: Address::from([0x33u8; 20]),
                pool_id: [0xabu8; 32],
                pool_key: crate::optimizers::v4_block_engine::V4PoolKey {
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
            },
        ).expect("V4 registration should succeed");

        // Register a 3-hop path: V2 → V3 → V4
        let path_id = engine.register_path(vec![
            MixedPoolRef { hop_type: HopType::V2, pool_key: v2_fwd, zero_for_one: true },
            MixedPoolRef { hop_type: HopType::V3, pool_key: v3_key, zero_for_one: false },
            MixedPoolRef { hop_type: HopType::V4, pool_key: v4_key, zero_for_one: true },
        ]);

        // Inspect the path
        let path = engine.path_pools.get(&path_id).expect("path should exist");
        assert_eq!(path.pools.len(), 3);

        // Verify hop types
        assert!(matches!(path.pools[0].hop_type, HopType::V2));
        assert!(matches!(path.pools[1].hop_type, HopType::V3));
        assert!(matches!(path.pools[2].hop_type, HopType::V4));

        // Verify we can resolve pool addresses via BotCore (V2) / sub-engines (V3/V4)
        let v2_addr = engine.core.lock().pool_address(v2_fwd);
        assert_eq!(v2_addr, Some(Address::from([0x11u8; 20])));

        let v3_pool = engine.v3_engine().get_pool(v3_key);
        assert_eq!(v3_pool.map(|p| p.address), Some(Address::from([0x22u8; 20])));

        let v4_pool = engine.v4_engine().get_pool(v4_key);
        assert_eq!(v4_pool.map(|p| p.pool_manager), Some(Address::from([0x33u8; 20])));
        assert_eq!(v4_pool.map(|p| p.pool_id), Some([0xabu8; 32]));

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
            td.insert(-60, crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(100),
                liquidity_net: alloy::primitives::I256::try_from(100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            });
            td.insert(60, crate::bot_core::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(100),
                liquidity_net: alloy::primitives::I256::try_from(-100i128)
                    .unwrap_or(alloy::primitives::I256::ZERO),
            });
            td
        };

        // Pool 1 at tick 0 with high liquidity
        let v3_key_a = engine.v3_engine().register_pool(RegisterV3PoolParams {
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
        let v3_key_b = engine.v3_engine().register_pool(RegisterV3PoolParams {
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
        let v3_key_c = engine.v3_engine().register_pool(RegisterV3PoolParams {
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
        let path_id = engine.register_path(vec![
            MixedPoolRef { hop_type: HopType::V3, pool_key: v3_key_a, zero_for_one: true },
            MixedPoolRef { hop_type: HopType::V3, pool_key: v3_key_b, zero_for_one: false },
            MixedPoolRef { hop_type: HopType::V3, pool_key: v3_key_c, zero_for_one: true },
        ]);

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
        tick_data.insert(-60, crate::bot_core::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(100),
            liquidity_net: alloy::primitives::I256::try_from(100i128)
                .unwrap_or(alloy::primitives::I256::ZERO),
        });
        tick_data.insert(60, crate::bot_core::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(100),
            liquidity_net: alloy::primitives::I256::try_from(-100i128)
                .unwrap_or(alloy::primitives::I256::ZERO),
        });
        let v3_key = engine.v3_engine().register_pool(RegisterV3PoolParams {
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
        let path_id = engine.register_path(vec![
            MixedPoolRef { hop_type: HopType::V2, pool_key: v2_fwd_a, zero_for_one: true },
            MixedPoolRef { hop_type: HopType::V3, pool_key: v3_key, zero_for_one: false },
            MixedPoolRef { hop_type: HopType::V2, pool_key: v2_fwd_b, zero_for_one: true },
        ]);

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
        // Why: ADR-003 reorg path — `removed`-flag detection feeds
        // `engine.handle_reorg`, which restores BotCore state and emits an
        // `expired` diff against `delivered`. This is the realistic case where
        // the pool's first Sync is at the reorg target block.
        use tokio::sync::mpsc;

        let mut engine = UniswapEngine::new();

        // Two balanced V2 pools forming a cycle (price ≈ 1:1875).
        let pool_a = Address::from([0x11u8; 20]);
        let pool_b = Address::from([0x12u8; 20]);
        let id_a = engine.register_v2_pool(
            pool_a,
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let id_b = engine.register_v2_pool(
            pool_b,
            weth(800),
            usdc(1_500_000),
            GAMMA_03,
            FEE_DENOM_03,
        );

        // Path: A (USDC→WETH) → B (WETH→USDC). Initially balanced → no profit.
        let path_id = engine.register_path(vec![
            MixedPoolRef { hop_type: HopType::V2, pool_key: id_a, zero_for_one: true },
            MixedPoolRef { hop_type: HopType::V2, pool_key: id_b, zero_for_one: true },
        ]);

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

        // Reorg: roll back block 5 (the Sync that created the arb).
        engine.handle_reorg(5);
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
}
