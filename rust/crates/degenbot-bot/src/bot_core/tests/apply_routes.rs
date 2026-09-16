use super::*;

/// Staged-router contract (`cl_route)`: `route_v3_event` is THE decision
/// point — a live-phase tick mutation for an unregistered pool must
/// stage into the pump buffer (not drop), and the buffered event lands
/// in `v3_buffer` keyed by address for the registration drain+pin seam.
#[test]
fn fuwyur_router_stages_unregistered_live_liquidity_into_pump_buffer() {
    use crate::bot_core::cl_route::{ApplyOutcome, BufferKind};
    let mut core = BotState::new();
    let addr = Address::from([0x66; 20]);
    let outcome = core.route_v3_event(
        crate::bot_core::cl_route::Phase::Live,
        addr,
        BufferedV3PoolEvent::Liquidity(BufferedV3LiquidityUpdate {
            tick_lower: -100,
            tick_upper: 7,
            liquidity_delta: 118_748_558_607_688,
            block_number: 10,
        }),
        &[],
    );
    assert_eq!(outcome, ApplyOutcome::Buffered(BufferKind::Pump));
    assert_eq!(core.buffered_v3_event_count(&addr), 1);
    // a buffered event is engine-witnessed activity — the
    // event horizon advances at ARRIVAL time (parity with V4) so the
    // pin's stamp-provenance verdict sees the true witnessed span after
    // the staged drain, not just ApplyDirect-routed events.
    assert_eq!(core.v3_event_horizon(&addr), 10);
}

/// the event horizon tracks the MAX block across multiple buffered
/// events for the same (still-unregistered) pool. The pin's
/// `SeedTrustOnly{witnessed_horizon>0}` classification (the re-seed-after-
/// activity tripwire) depends on this being the true high-water mark.
#[test]
fn v3_event_horizon_tracks_max_block_across_buffered_events() {
    let mut core = BotState::new();
    let addr = Address::from([0x67; 20]);
    let mk = |block, delta| {
        BufferedV3PoolEvent::Liquidity(BufferedV3LiquidityUpdate {
            tick_lower: -100,
            tick_upper: 7,
            liquidity_delta: delta,
            block_number: block,
        })
    };
    core.route_v3_event(crate::bot_core::cl_route::Phase::Live, addr, mk(10, 1), &[]);
    core.route_v3_event(crate::bot_core::cl_route::Phase::Live, addr, mk(50, 2), &[]);
    core.route_v3_event(crate::bot_core::cl_route::Phase::Live, addr, mk(30, 3), &[]);
    assert_eq!(core.buffered_v3_event_count(&addr), 3);
    assert_eq!(core.v3_event_horizon(&addr), 50);
}

#[test]
fn v3_split_clock_seed_prices_at_head_ticks_at_db_block() {
    // Two-stamp rule / the fresh-read builder: seed the PRICE clock at
    // HEAD (`update_block` — a cheap slot0 read) while the LIQUIDITY
    // clock stays at the DB liquidity snapshot block (`tick_data_block`).
    // The historical-replay guard must key on the PRICE seed block
    // (`initial_state_block == update_block`): the head-seeded slot0
    // `liquidity` scalar already reflects every in-range event up to head,
    // so a backfilled in-range replay below head must not adjust it.
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::{RegisterV3PoolParams, TickInfo};
    let mut core = BotState::new();
    let pool_addr = Address::from([0xccu8; 20]);
    let head = 1_000u64;
    let db_block = 950u64;
    let mut tick_data = HashMap::new();
    tick_data.insert(
        60,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(100),
            liquidity_net: 100i128,
            block: 0,
        },
    );
    let pool_id = core
        .register_v3_pool(&RegisterV3PoolParams {
            address: pool_addr,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block: head,
            tick_data_block: Some(db_block),
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
            ..Default::default()
        })
        .expect("test setup: V3 split-clock registration");
    let s = core.get_v3_pool(pool_id).expect("registered");
    assert_eq!(
        s.update_block, head,
        "price clock stamped at HEAD (fresh slot0 read)"
    );
    assert_eq!(
        s.tick_data_block, db_block,
        "liquidity clock anchored at the DB snapshot block"
    );
    assert_eq!(
        s.initial_state_block, head,
        "replay guard keys on the PRICE seed block (head-seeded scalar)"
    );
    assert_eq!(core.pool_update_block(pool_id), head);
    assert_eq!(core.pool_tick_data_block(pool_id), db_block);
}

#[test]
fn aerodrome_reserve_mutation_and_reorg_rollback() {
    // Aerodrome V2 reserves + reorg journal live in Rust (ADR-005
    // Aerodrome state port): `apply_sync_by_pool_id` journals
    // the prior reserves then lands the new; `aerodrome_restore_before_block`
    // pops back to the landed-at state at the target block.
    use crate::bot_core::RegisterAerodromeV2PoolParams;

    let mut core = BotState::new();
    let pool_id = core.register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
        address: Address::from([0xaeu8; 20]),
        token0: Address::ZERO,
        token1: Address::from([0x01u8; 20]),
        factory: Address::from([0xafu8; 20]),
        variant: degenbot_uniswap::dex_identity::DexVariant::AerodromeV2Volatile,
        stable: false,
        fee: (3, 1000),
        token0_decimals: 18,
        token1_decimals: 18,
        reserve0: U112::from(1_000u64),
        reserve1: U112::from(2_000u64),
        update_block: 10,
    });

    // Identity survives.
    let identity = core
        .get_aerodrome_identity(pool_id)
        .expect("aerodrome identity");
    assert_eq!(identity.fee, (3, 1000));
    assert!(!identity.stable);

    // Initial registration state (genesis anchor at block 10).
    let state = core.get_aerodrome_pool(pool_id).expect("aerodrome state");
    assert_eq!(state.reserve0, U112::from(1_000u64));
    assert_eq!(state.reserve1, U112::from(2_000u64));
    assert_eq!(state.update_block, 10);
    assert_eq!(state.journal.len(), 1);

    // Apply a Sync at block 20 (journals prior reserves, lands new).
    let applied =
        core.apply_sync_by_pool_id(pool_id, U112::from(1_500u64), U112::from(2_500u64), 20);
    assert_eq!(applied, Some(pool_id));
    let state = core.get_aerodrome_pool(pool_id).expect("aerodrome state");
    assert_eq!(state.reserve0, U112::from(1_500u64));
    assert_eq!(state.reserve1, U112::from(2_500u64));
    assert_eq!(state.update_block, 20);
    assert_eq!(state.journal.len(), 2);

    // Reorg to before block 20 → restores registration state (genesis at 10).
    core.restore_pool_before_block(pool_id, 20)
        .expect("restore returns Some")
        .expect("restore succeeds");
    let state = core.get_aerodrome_pool(pool_id).expect("aerodrome state");
    assert_eq!(state.reserve0, U112::from(1_000u64));
    assert_eq!(state.reserve1, U112::from(2_000u64));
    assert_eq!(state.update_block, 10);

    // With ADR-017 slice 5 the Aerodrome + V2 `apply_sync` paths are
    // one dispatcher (`apply_sync_by_pool_id` across both reserve-pair
    // families), so a V2 pool_id now lands too — the cross-family isolation
    // that the old per-family method provided is gone by design.
    let v2_id = core
        .register_v2_pool(&make_params(U112::from(100), U112::from(200)))
        .expect("test setup: V2 registration");
    assert_eq!(
        core.apply_sync_by_pool_id(v2_id, U112::ZERO, U112::ZERO, 99),
        Some(v2_id)
    );
    // The no-mutate-on-wrong-family guard for restore moved to the PyO3
    // wrapper layer (ADR-016); BotState's unified restore dispatches
    // across all families, so a V2 pool_id is restored as V2.
}

#[test]
fn calculate_tokens_out_reverse_direction() {
    let mut core = BotState::new();
    let pool_id = core
        .register_v2_pool(&make_params(U112::from(2000), U112::from(1000)))
        .expect("test setup: V2 registration");

    // Python reference: constant_product_calc_exact_in(100, 1000, 2000, 3/1000) = 181
    let amount_out = tokens_out(&mut core, pool_id, false, U256::from(100));
    assert_eq!(amount_out, U256::from(181));
}

#[test]
fn update_v2_pool_changes_calculation_result() {
    let mut core = BotState::new();
    let pool_id = core
        .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
        .expect("test setup: V2 registration");

    // Before update: swap 100 token0 → 181 token1
    let before = tokens_out(&mut core, pool_id, true, U256::from(100));
    assert_eq!(before, U256::from(181));

    // Update reserves: now reserve0=2000, reserve1=1000
    core.update_v2_pool(make_pool_addr(), U112::from(2000), U112::from(1000), 42);

    // After update: Python: constant_product_calc_exact_in(100, 2000, 1000, 3/1000) = 47
    let after = tokens_out(&mut core, pool_id, true, U256::from(100));
    assert_eq!(after, U256::from(47));
}

/// Required-input read through the swap-simulation gate (ADR-037) — the
/// replacement for the deleted `calculate_tokens_in` seam. Exact-output
/// request (positive user-perspective); the required input is the
/// magnitude of the consumed delta.
fn tokens_in(core: &mut BotState, pool_id: u64, zero_for_one: bool, amount_out: U256) -> U256 {
    match core.swap_simulation(
        0,
        pool_id,
        SwapRequest {
            zero_for_one,
            amount_specified: I256::try_from(amount_out).unwrap(),
            sqrt_price_limit: None,
        },
    ) {
        SwapRead::Computed(outcome) => (-match &outcome {
            SwapOutcome::V2(o) => o.consumed,
            SwapOutcome::V3(o) | SwapOutcome::V4(o) => o.consumed,
        })
        .into_raw(),
        f => panic!("exact-out calc must not fail on this fixture: {f:?}"),
    }
}

#[test]
fn calculate_tokens_in_for_v2_pool() {
    let mut core = BotState::new();
    let pool_id = core
        .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
        .expect("test setup: V2 registration");

    // Python: constant_product_calc_exact_out(50, 1000, 2000, 3/1000) = 26
    let amount_in = tokens_in(&mut core, pool_id, true, U256::from(50));
    assert_eq!(amount_in, U256::from(26));

    // Reverse: Python: constant_product_calc_exact_out(10, 2000, 1000, 3/1000) = 21
    let amount_in_rev = tokens_in(&mut core, pool_id, false, U256::from(10));
    assert_eq!(amount_in_rev, U256::from(21));
}

#[test]
fn calculate_tokens_out_realistic_amounts() {
    let mut core = BotState::new();

    // Realistic: 1.5M USDC / 800 WETH, 0.3% fee
    let reserve0 = U112::from(1_500_000_000_000u64); // 1.5M USDC (6dp)
    let reserve1 = U112::from(800u128) * U112::from(10u64).pow(U112::from(18)); // 800 WETH

    let params = RegisterV2PoolParams {
        address: make_pool_addr(),
        token0: make_token0(),
        token1: make_token1(),
        reserve0,
        reserve1,
        fee_token0: FEE_03,
        fee_token1: FEE_03,
        factory: make_factory(),
        update_block: 0,
        variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
        stable_swap: false,
        fee_denominator: None,
        ..Default::default()
    };
    let pool_id = core
        .register_v2_pool(&params)
        .expect("test setup: V2 registration");

    // Swap 1000 USDC for WETH
    // Python reference: 531380142665175213
    let amount_in = U256::from(1_000_000_000u64); // 1000 USDC (6dp)
    let amount_out = tokens_out(&mut core, pool_id, true, amount_in);
    assert_eq!(amount_out, U256::from(531_380_142_665_175_213_u64));
}

/// Bug-A regression (path-142603): a **backfill→Live** in-range
/// `ModifyLiquidity` with block > seed must adjust the in-range `liquidity()`
/// scalar. Pre-fix the backfill→Live branch applied the tick map via the
/// low-level `apply_liquidity_to_tick_range` and NEVER adjusted the scalar,
/// producing the staged-clock desync (fresh tick map, stale in-range
/// liquidity). Now it routes through the shared, in-range-aware
/// `apply_liquidity_update`. (Sparse → Live registration per DFQYM5.)
#[test]
fn backfill_live_in_range_post_seed_adjusts_in_range_liquidity() {
    let mut core = BotState::new();
    let pool_id = register_v3(&mut core, 5); // seed block 5, liq 0, tick 0 (Sparse -> Live)
                                             // Post-seed in-range Mint (tick 0 in [-60,60), block 6 > seed 5):
    core.buffer_backfill_v3_liquidity_update(make_pool_addr(), -60, 60, 123_456_i128, 6);
    let Some(state) = core
        .pools
        .get(&pool_id)
        .and_then(PoolEntry::v3)
        .map(|(_, s)| s)
    else {
        panic!("pool missing")
    };
    assert_eq!(
        state.liquidity, 123_456,
        "in-range post-seed Mint must adjust the active scalar"
    );
    assert_eq!(state.tick_data_block, 6, "liquidity clock advanced");
    assert_eq!(
        state.update_block, 6,
        "price clock advanced (in-range post-seed)"
    );
    assert!(
        state.tick_data.contains_key(&-60) && state.tick_data.contains_key(&60),
        "both boundary ticks mutated"
    );
}

/// Bug-A companion: a backfill→Live in-range event at/before the seed block
/// is a historical replay — the seed's `liquidity` already reflects every
/// on-chain event <= seed, so the tick map mutates but the scalar must NOT
/// change (the guard in the shared `apply_liquidity_update` survives the
/// unification).
#[test]
fn backfill_live_in_range_pre_seed_does_not_adjust_scalar() {
    let mut core = BotState::new();
    let pool_id = register_v3(&mut core, 5); // seed block 5
                                             // Pre-seed in-range Mint (block 4 <= seed 5):
    core.buffer_backfill_v3_liquidity_update(make_pool_addr(), -60, 60, 250_000_i128, 4);
    let Some(state) = core
        .pools
        .get(&pool_id)
        .and_then(PoolEntry::v3)
        .map(|(_, s)| s)
    else {
        panic!("pool missing")
    };
    assert_eq!(
        state.liquidity, 0,
        "pre-seed replay must NOT adjust the scalar"
    );
    assert!(
        state.tick_data.contains_key(&-60),
        "pre-seed replay still mutates the tick map"
    );
    assert_eq!(
        state.tick_data_block, 5,
        "pre-seed replay is a monotonic no-op: clock stays at the seed block (5 > 4)"
    );
}

/// Regression (WZWKKU): `v3_restore_before_block(B)` with `B` strictly past
/// the journal's newest delta must leave the pool's current state
/// UNTOUCHED. This is the per-pool path `dispatch_reorg_log` hits for a
/// `removed: true` log on an unrelated pool — totally normal reorg traffic.
///
/// Pre-fix the journal returned the newest delta's own `scalar_priors` and
/// `tick_priors` (the PRE-newest state) and `v3_restore_before_block`
/// reverse-applied them, silently rolling back the newest delta's swap and
/// deleting its freshly-initialized ticks. The engine then re-solved off
/// the corrupted scalars + `tick_data`.
#[test]
fn v3_restore_before_block_past_newest_leaves_state_untouched() {
    let mut core = BotState::new();
    let pool_id = register_v3(&mut core, 5);

    // Forward Swap at block 10 moves scalars + initializes tick 100.
    let new_sqrt = U256::from(2u64) << 96;
    core.apply_v3_swap_by_pool_id(
        pool_id,
        new_sqrt,
        1_000,
        100,
        10,
        &[(
            100,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(500),
                liquidity_net: 500i128,
                block: 0,
            },
        )],
    );

    // Snapshot the landed-at (post-block-10) state.
    let landed_sqrt;
    let landed_liq;
    let landed_tick;
    let landed_update_block;
    let tick_present;
    {
        let Some(state) = core
            .pools
            .get(&pool_id)
            .and_then(PoolEntry::v3)
            .map(|(_, s)| s)
        else {
            panic!("pool missing");
        };
        landed_sqrt = state.sqrt_price_x96;
        landed_liq = state.liquidity;
        landed_tick = state.tick;
        landed_update_block = state.update_block;
        tick_present = state.tick_data.contains_key(&100);
    }
    assert_eq!(landed_sqrt, new_sqrt);
    assert_eq!(landed_liq, 1_000);
    assert_eq!(landed_tick, 100);
    assert_eq!(landed_update_block, 10);
    assert!(tick_present, "block-10 swap initialized tick 100");

    // Reorg: a removed log arrives for an unrelated pool whose newest
    // journal delta (block 10) is BELOW the reorg target (block 12).
    // `has_state_prior_to` returns true (V3 journal non-empty), so the
    // coordinator proceeds to `restore_pool_before_block` →
    // `v3_restore_before_block`.
    assert!(core.has_state_prior_to(pool_id, 12));
    let result = core.restore_pool_before_block(pool_id, 12);
    assert!(
        result.is_some(),
        "restore returns Some even on the no-op path"
    );

    // The landed-at state must survive unchanged.
    let Some(state) = core
        .pools
        .get(&pool_id)
        .and_then(PoolEntry::v3)
        .map(|(_, s)| s)
    else {
        panic!("pool missing post-restore");
    };
    assert_eq!(
        state.sqrt_price_x96, landed_sqrt,
        "scalars must not roll back"
    );
    assert_eq!(state.liquidity, landed_liq);
    assert_eq!(state.tick, landed_tick);
    assert_eq!(state.update_block, landed_update_block);
    assert!(
        state.tick_data.contains_key(&100),
        "tick 100 must survive — pre-fix it was deleted via the newest delta's tick_priors"
    );
    assert_eq!(
        state.journal.len(),
        1,
        "no deltas popped on the no-op path (only the block-10 swap delta; V3 registration pushes no genesis)"
    );
}

/// Regression (HO3GWT, V3 backfill buffer): a Mint buffered during the
/// backfill phase, then applied at registration via
/// `apply_backfill_buffer_v3`, must (1) push a tick-only journal delta,
/// (2) advance `state.update_block` to the event's block, and (3) be
/// reversible by `restore_before_block` (roll the tick state back to the
/// pre-buffer registration snapshot).
///
/// Pre-fix `apply_backfill_buffer_v3` called `apply_liquidity_to_tick_range`
/// and `invalidate_tick_range_cache()` and stopped — no journal, no
/// `update_block` bump. A reorg landing inside the buffered range
/// couldn't reverse the buffered events, and `v3_snapshot`/diagnostics
/// reported a stale last-update block.
#[test]
fn apply_backfill_buffer_v3_journals_and_advances_update_block() {
    let pool_addr = Address::from([0x88u8; 20]);
    let block_b = 5u64;

    // 1. Pre-registration: buffer a backfill Mint at [60, 120], block B=5.
    let mut core = BotState::new();
    core.buffer_backfill_v3_liquidity_update(pool_addr, 60, 120, 500_i128, block_b);

    // 2. Register on the SAME core (tick 60 gross=100, tick 120 absent).
    let pool_id = register_v3_on_core(&mut core, pool_addr, 0);

    // 3. Apply the backfill buffer (the registration-staged application).
    core.apply_backfill_buffer_v3(&pool_addr);

    {
        let s = core.get_v3_pool(pool_id).expect("registered");
        // Two-stamp rule: a Mint mutates the TICK MAP (liquidity clock
        // advances) but, being out of range, leaves the slot0 head
        // untouched (price clock stays at registration block 0).
        assert_eq!(
            s.tick_data_block, block_b,
            "the liquidity clock advances to the buffered event's block"
        );
        assert_eq!(
            s.update_block, 0,
            "the price clock is untouched (out-of-range mint)"
        );
        assert_eq!(
            s.journal.len(),
            1,
            "buffered Mint must push one tick-only journal delta (pre-fix: 0)"
        );
        let t60 = s.tick_data.get(&60).expect("tick 60 present");
        assert_eq!(t60.liquidity_gross, alloy::primitives::U128::from(600));
        assert_eq!(t60.liquidity_net, 600i128);
        let t120 = s.tick_data.get(&120).expect("tick 120 newly initialized");
        assert_eq!(t120.liquidity_gross, alloy::primitives::U128::from(500));
        assert_eq!(
            t120.liquidity_net, -500i128,
            "upper tick net -= delta (V3/`apply_liquidity_to_tick_range` convention)"
        );
    }

    // 4. Restore before block B → rolls back the buffered Mint to the
    //    registration snapshot.
    core.restore_pool_before_block(pool_id, block_b);
    let s = core.get_v3_pool(pool_id).expect("registered");
    let t60 = s.tick_data.get(&60).expect("tick 60 still present");
    assert_eq!(
        t60.liquidity_gross,
        alloy::primitives::U128::from(100),
        "tick 60 reverts to registration snapshot (gross 100) on rollback"
    );
    assert_eq!(t60.liquidity_net, 100i128);
    assert!(
        !s.tick_data.contains_key(&120),
        "newly-initialized tick 120 removed on rollback"
    );
}

/// Regression (HO3GWT, V3 pump buffer): same invariants as the backfill
/// path, but the event is buffered via the WS-pump path (`apply_v3_
/// liquidity_update` while unregistered routes to the pump buffer) and
/// applied via `apply_pump_buffer_v3`.
#[test]
fn apply_pump_buffer_v3_journals_and_advances_update_block() {
    let pool_addr = Address::from([0x99u8; 20]);
    let block_b = 7u64;

    // 1. Pre-registration: pump the Mint (unregistered → pump buffer).
    let mut core = BotState::new();
    core.apply_v3_liquidity_update(pool_addr, 60, 120, 500_i128, block_b);

    // 2. Register on the SAME core + 3. apply pump buffer.
    let pool_id = register_v3_on_core(&mut core, pool_addr, 0);
    // the gated drain only yields fully-completed blocks. The
    // live pump marks `block_b` complete at its ADR-008 D1 tombstone (the
    // first log of block_b+1); mirror that here so the drain takes the
    // buffered Mint instead of leaving it pinned behind the gate.
    core.advance_pump_complete_cutoff(block_b);
    core.apply_pump_buffer_v3(&pool_addr);

    {
        let s = core.get_v3_pool(pool_id).expect("registered");
        // Two-stamp rule: tick-map-only mint → liquidity clock advances,
        // price clock untouched.
        assert_eq!(
            s.tick_data_block, block_b,
            "the liquidity clock advances to pump-buffer event block"
        );
        assert_eq!(
            s.update_block, 0,
            "the price clock is untouched (out-of-range mint)"
        );
        assert_eq!(
            s.journal.len(),
            1,
            "pump-buffer Mint pushes one journal delta"
        );
        assert_eq!(
            s.tick_data.get(&60).expect("t60").liquidity_gross,
            alloy::primitives::U128::from(600)
        );
        assert!(s.tick_data.contains_key(&120));
    }

    core.restore_pool_before_block(pool_id, block_b);
    let s = core.get_v3_pool(pool_id).expect("registered");
    assert_eq!(
        s.tick_data.get(&60).expect("t60").liquidity_gross,
        alloy::primitives::U128::from(100),
        "pump-buffer Mint rolls back to registration snapshot"
    );
    assert!(
        !s.tick_data.contains_key(&120),
        "newly-initialized tick 120 removed on pump-buffer rollback"
    );
}

/// Regression (HO3GWT, V4 backfill buffer): mirror of the V3 backfill
/// test for `apply_backfill_buffer_v4` — a `ModifyLiquidity` buffered
/// during backfill must journal + advance `update_block` + be reversible
/// via `v4_restore_before_block`.
#[test]
fn apply_backfill_buffer_v4_journals_and_advances_update_block() {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::{RegisterV4PoolParams, TickInfo, V4PoolKey};
    use alloy::primitives::{I256, U128};

    let pool_manager = Address::from([0x44u8; 20]);
    let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0xeeu8; 32];
    let block_b = 9u64;

    // 1. Pre-registration: buffer a backfill ModifyLiquidity at [60,120].
    let mut core = BotState::new();
    core.buffer_backfill_v4_liquidity_update(
        pool_manager,
        pool_id_bytes,
        60,
        120,
        I256::try_from(500i128).unwrap(),
        block_b,
    );

    // 2. Register (tick 60 gross=100, tick 120 absent, update_block=0).
    let mut tick_data = HashMap::new();
    tick_data.insert(
        60,
        TickInfo {
            liquidity_gross: U128::from(100),
            liquidity_net: 100i128,
            block: 0,
        },
    );
    let pool_id = core
        .register_v4_pool(&RegisterV4PoolParams {
            pool_manager,
            pool_id: pool_id_bytes,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 10_000,
                tick_spacing: 60,
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
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
        })
        .expect("V4 pool registers");

    // 3. Apply the backfill buffer.
    core.apply_backfill_buffer_v4(pool_manager, pool_id_bytes);

    {
        let s = core.get_v4_pool(pool_id).expect("registered");
        // Two-stamp rule: tick-map-only ModifyLiquidity → liquidity clock
        // advances, price clock untouched.
        assert_eq!(
            s.tick_data_block, block_b,
            "V4 liquidity clock advances to buffered event block"
        );
        assert_eq!(
            s.update_block, 0,
            "V4 price clock untouched (out-of-range mint)"
        );
        assert_eq!(
            s.journal.len(),
            1,
            "V4 buffered ModifyLiquidity pushes one journal delta"
        );
        assert_eq!(
            s.tick_data.get(&60).expect("t60").liquidity_gross,
            U128::from(600)
        );
        assert!(s.tick_data.contains_key(&120));
    }

    // 4. Restore before block B → rolls back the ModifyLiquidity.
    core.restore_pool_before_block(pool_id, block_b);
    let s = core.get_v4_pool(pool_id).expect("registered");
    assert_eq!(
        s.tick_data.get(&60).expect("t60").liquidity_gross,
        U128::from(100),
        "V4 tick 60 reverts to registration snapshot on rollback"
    );
    assert!(
        !s.tick_data.contains_key(&120),
        "V4 newly-initialized tick 120 removed on rollback"
    );
}

/// Regression: `PyLiquidityPool.apply_swap` routed V4 pools into
/// `apply_v3_swap_by_pool_id`, which matches `PoolEntry::V3` only and
/// silently no-op'd on `PoolEntry::V4` — a Python-side V4 update path
/// (snapshots, regression tests, manual `external_update`) dropped every
/// update. The fix is a family-dispatching `apply_swap_by_pool_id` on
/// `BotState` that `PyLiquidityPool.apply_swap` calls (the preferred
/// "existing methods do family dispatch internally" option). This test
/// pins both halves of the AC: V4 scalars actually change (not a no-op),
/// and they match a direct `apply_v4_swap` on the same scalar inputs.
#[test]
fn apply_swap_by_pool_id_routes_to_v4_and_matches_apply_v4_swap() {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::{RegisterV4PoolParams, V4PoolKey, V4SwapUpdate};

    let pool_manager = Address::from([0x44u8; 20]);
    let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0x66u8; 32];
    let block_b = 7u64;

    let make_params = || RegisterV4PoolParams {
        pool_manager,
        pool_id: pool_id_bytes,
        pool_key: V4PoolKey {
            currency0: Address::ZERO,
            currency1: Address::from([1u8; 20]),
            fee: 10_000,
            tick_spacing: 60,
            hooks: Address::ZERO,
        },
        hook_flags: 0,
        protocol_fee: 0,
        sqrt_price_x96: U256::from(1u128) << 96,
        liquidity: 1_000_000,
        tick: 0,
        tick_data: HashMap::new(),
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
    };

    // Twin pools: A updated via the family dispatcher, B via the
    // dedicated V4 path (`apply_v4_swap`). Both start identical.
    let mut core_a = BotState::new();
    let id_a = core_a
        .register_v4_pool(&make_params())
        .expect("V4 pool A registers");
    let mut core_b = BotState::new();
    let id_b = core_b
        .register_v4_pool(&make_params())
        .expect("V4 pool B registers");

    // Before the fix, this call was a silent no-op on A (routed to the
    // V3-only method). Assert it now applies.
    let _ =
        core_a.apply_swap_by_pool_id(id_a, U256::from(2u128) << 96, 2_000_000, -100, block_b, &[]);
    let s_a = core_a.get_v4_pool(id_a).expect("V4 pool A registered");
    assert_eq!(s_a.sqrt_price_x96, U256::from(2u128) << 96);
    assert_eq!(s_a.liquidity, 2_000_000);
    assert_eq!(s_a.tick, -100);
    assert_eq!(s_a.update_block, block_b);
    assert_eq!(
        s_a.journal.len(),
        1,
        "the dispatcher must journal a scalar delta like apply_v4_swap"
    );

    // AC parity: same scalar inputs via `apply_v4_swap` produce identical
    // post-state. The dispatcher's V4 branch mirrors `apply_v4_swap`'s
    // body (same tick_priors=[], same journal shape).
    let _ = core_b.apply_v4_swap(
        &V4SwapUpdate {
            pool_manager,
            pool_id: pool_id_bytes,
            sqrt_price_x96: U256::from(2u128) << 96,
            liquidity: 2_000_000,
            tick: -100,
            tick_priors: Box::default(),
        },
        block_b,
    );
    let s_b = core_b.get_v4_pool(id_b).expect("V4 pool B registered");
    assert_eq!(s_b.sqrt_price_x96, s_a.sqrt_price_x96);
    assert_eq!(s_b.liquidity, s_a.liquidity);
    assert_eq!(s_b.tick, s_a.tick);
    assert_eq!(s_b.update_block, s_a.update_block);
    assert_eq!(s_b.journal.len(), s_a.journal.len());
}

/// Regression (RAJ3PP, V4 `apply_liquidity_update` half): the liquidity
/// update previously routed V4 pools into `apply_v3_liquidity_update_by_pool
/// _id` (V3-only, no-op on V4). The family dispatcher must apply a V4
/// `ModifyLiquidity` to the tick range and journal it, matching
/// `apply_v4_liquidity_update` on the same inputs.
#[test]
fn apply_liquidity_update_by_pool_id_routes_to_v4_and_applies_ticks() {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::{RegisterV4PoolParams, TickInfo, V4PoolKey};
    use alloy::primitives::{I256, U128};

    let pool_manager = Address::from([0x55u8; 20]);
    let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0x77u8; 32];
    let block_b = 5u64;

    // Pre-seed tick 60 (gross=100, net=+100); tick 120 absent.
    let mut tick_data = HashMap::new();
    tick_data.insert(
        60,
        TickInfo {
            liquidity_gross: U128::from(100),
            liquidity_net: 100i128,
            block: 0,
        },
    );
    let params = RegisterV4PoolParams {
        pool_manager,
        pool_id: pool_id_bytes,
        pool_key: V4PoolKey {
            currency0: Address::ZERO,
            currency1: Address::from([1u8; 20]),
            fee: 10_000,
            tick_spacing: 60,
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
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
    };

    // Twin pools: A via the dispatcher, B via apply_v4_liquidity_update.
    let mut core_a = BotState::new();
    let id_a = core_a.register_v4_pool(&params).expect("V4 A registers");
    let mut core_b = BotState::new();
    let id_b = core_b.register_v4_pool(&params).expect("V4 B registers");

    // Before the fix this no-op'd on A. Assert it now applies.
    assert_eq!(
        core_a.apply_liquidity_update_by_pool_id(id_a, 60, 120, 500, block_b),
        Some(id_a),
        "dispatcher must report an applied V4 liquidity update"
    );
    let s_a = core_a.get_v4_pool(id_a).expect("registered A");
    // Two-stamp rule: out-of-range mint → liquidity clock advances, price
    // clock untouched.
    assert_eq!(s_a.tick_data_block, block_b);
    assert_eq!(s_a.update_block, 0);
    assert_eq!(s_a.journal.len(), 1);
    assert_eq!(
        s_a.tick_data.get(&60).expect("t60").liquidity_gross,
        U128::from(600),
        "tick 60 gross += delta (ModifyLiquidity) via the dispatcher"
    );
    assert!(s_a.tick_data.contains_key(&120), "tick 120 initialized");
    // slot0 scalars unchanged (tick-only event per ADR-004).
    assert_eq!(s_a.sqrt_price_x96, U256::from(1u128) << 96);

    // Parity: direct apply_v4_liquidity_update on B produces the same state.
    assert_eq!(
        core_b.apply_v4_liquidity_update(
            pool_manager,
            pool_id_bytes,
            60,
            120,
            I256::try_from(500i128).unwrap(),
            block_b
        ),
        Some(id_b)
    );
    let s_b = core_b.get_v4_pool(id_b).expect("registered B");
    assert_eq!(
        s_b.tick_data.get(&60).expect("t60").liquidity_gross,
        s_a.tick_data.get(&60).expect("t60").liquidity_gross
    );
    assert_eq!(s_b.journal.len(), s_a.journal.len());
    assert_eq!(s_b.update_block, s_a.update_block);
}

/// Regression (J63J3N, scalar read half): `PyLiquidityPool.snapshot_v3`
/// and the per-field scalar getters all routed through `get_v3_pool`,
/// which returns `None` for `PoolEntry::V4` — silently dropping V4 reads
/// (the read-side twin of the RAJ3PP write-side bug). The fix is the
/// family-dispatching `BotState::get_v3_or_v4_pool` accessor returning a
/// `&dyn ConcentratedLiquidityPool`. This pins both halves of the AC: a V4 pool
/// returns a non-`None` scalar view, and the scalars match a direct
/// `apply_v4_swap` on the same inputs.
#[test]
fn get_v3_or_v4_pool_reads_v4_scalars_matching_apply_v4_swap() {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::{RegisterV4PoolParams, V4PoolKey, V4SwapUpdate};

    let pool_manager = Address::from([0x88u8; 20]);
    let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0x99u8; 32];
    let block_b = 11u64;

    let make_params = || RegisterV4PoolParams {
        pool_manager,
        pool_id: pool_id_bytes,
        pool_key: V4PoolKey {
            currency0: Address::ZERO,
            currency1: Address::from([1u8; 20]),
            fee: 500,
            tick_spacing: 10,
            hooks: Address::ZERO,
        },
        hook_flags: 0,
        protocol_fee: 0,
        sqrt_price_x96: U256::from(1u128) << 96,
        liquidity: 1_000_000,
        tick: 0,
        tick_data: HashMap::new(),
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
    };

    // Twin V4 pools: A read via the family accessor after a dispatcher
    // apply; B updated via the dedicated `apply_v4_swap`. Both start
    // identical.
    let mut core_a = BotState::new();
    let id_a = core_a
        .register_v4_pool(&make_params())
        .expect("V4 pool A registers");
    let mut core_b = BotState::new();
    let id_b = core_b
        .register_v4_pool(&make_params())
        .expect("V4 pool B registers");

    // Before the fix, `get_v3_or_v4_pool` did not exist and the Python
    // reader used `get_v3_pool` (None for V4). Assert the accessor now
    // returns a non-None view of the post-apply state.
    let _ =
        core_a.apply_swap_by_pool_id(id_a, U256::from(3u128) << 96, 9_000_000, -240, block_b, &[]);
    let view_a = core_a
        .get_v3_or_v4_pool(id_a)
        .expect("V4 pool must surface a non-None reader view");
    assert_eq!(view_a.sqrt_price_x96(), U256::from(3u128) << 96);
    assert_eq!(view_a.liquidity(), 9_000_000);
    assert_eq!(view_a.tick(), -240);
    assert_eq!(view_a.update_block(), block_b);
    // Immutable V4 key fields surface from `pool_key` (the ConcentratedLiquidityPool
    // reader trait was slimmed to mutable-only scalars in the V3/V4
    // identity/state split; identity reads go through the family-specific
    // getter rather than the dyn-dispatch view).
    let v4_id_a = core_a
        .get_v4_identity(id_a)
        .expect("registered V4 pool surfaces an identity via get_v4_identity");
    assert_eq!(v4_id_a.pool_key.fee, 500);
    assert_eq!(v4_id_a.pool_key.tick_spacing, 10);

    // AC parity: identical scalar inputs via `apply_v4_swap` produce the
    // same values read through the accessor.
    let _ = core_b.apply_v4_swap(
        &V4SwapUpdate {
            pool_manager,
            pool_id: pool_id_bytes,
            sqrt_price_x96: U256::from(3u128) << 96,
            liquidity: 9_000_000,
            tick: -240,
            tick_priors: Box::default(),
        },
        block_b,
    );
    let view_b = core_b
        .get_v3_or_v4_pool(id_b)
        .expect("V4 pool B surfaces a reader view");
    assert_eq!(view_b.sqrt_price_x96(), view_a.sqrt_price_x96());
    assert_eq!(view_b.liquidity(), view_a.liquidity());
    assert_eq!(view_b.tick(), view_a.tick());
    assert_eq!(view_b.update_block(), view_a.update_block());
}

/// Regression (J63J3N, tick-data read half): `tick_data_snapshot` and
/// `tick_bitmap_snapshot` routed through `get_v3_pool`, returning an
/// empty dict for V4 pools. The family `get_v3_or_v4_pool` accessor's
/// `tick_data()` must surface a V4 pool's tick map (post-Mint/Burn) — and
/// it must match `apply_v4_liquidity_update` on the same inputs.
#[test]
fn get_v3_or_v4_pool_reads_v4_tick_data_matching_apply_v4_liquidity_update() {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::{RegisterV4PoolParams, TickInfo, V4PoolKey};
    use alloy::primitives::{I256, U128};

    let pool_manager = Address::from([0xaau8; 20]);
    let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0xbbu8; 32];
    let block_b = 13u64;

    // Pre-seed tick 60 (gross=100, net=+100); tick 120 absent.
    let mut tick_data = HashMap::new();
    tick_data.insert(
        60,
        TickInfo {
            liquidity_gross: U128::from(100),
            liquidity_net: 100i128,
            block: 0,
        },
    );
    let params = RegisterV4PoolParams {
        pool_manager,
        pool_id: pool_id_bytes,
        pool_key: V4PoolKey {
            currency0: Address::ZERO,
            currency1: Address::from([1u8; 20]),
            fee: 3_000,
            tick_spacing: 60,
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
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
    };

    // Twin pools: A via the dispatcher, B via apply_v4_liquidity_update.
    let mut core_a = BotState::new();
    let id_a = core_a.register_v4_pool(&params).expect("V4 A registers");
    let mut core_b = BotState::new();
    let id_b = core_b.register_v4_pool(&params).expect("V4 B registers");

    let _ = core_a.apply_liquidity_update_by_pool_id(id_a, 60, 120, 700, block_b);
    let _ = core_b.apply_v4_liquidity_update(
        pool_manager,
        pool_id_bytes,
        60,
        120,
        I256::try_from(700i128).unwrap(),
        block_b,
    );

    // Before the fix, the V4 view came back None → empty dict. Assert the
    // accessor now yields the V4 tick map (non-empty, mutated) and the
    // view matches the dedicated-path twin.
    let view_a = core_a
        .get_v3_or_v4_pool(id_a)
        .expect("V4 A reader view non-None");
    let view_b = core_b
        .get_v3_or_v4_pool(id_b)
        .expect("V4 B reader view non-None");
    assert!(!view_a.tick_data().is_empty(), "V4 tick map surfaced");
    assert_eq!(
        view_a.tick_data().get(&60).expect("t60").liquidity_gross,
        U128::from(800),
        "tick 60 gross reflects the +700 ModifyLiquidity"
    );
    assert_eq!(
        view_a.tick_data().get(&60).expect("t60").liquidity_gross,
        view_b.tick_data().get(&60).expect("t60").liquidity_gross,
        "dispatcher and dedicated path produce identical V4 tick maps"
    );
    assert_eq!(view_a.update_block(), view_b.update_block());
}

/// Regression: same-block multi-Swap reorg rollback.
///
/// `push_delta` collapsed same-block deltas ("same-block replacement"):
/// the second Swap at block B replaced the first, so the recorded
/// `scalar_priors` became post-first-Swap, not pre-block. On
/// `restore_before_block(B)` the popped delta then returned post-first-Swap
/// scalars, landing the pool on post-first-Swap instead of the true pre-B
/// state. (Two same-block swaps on mainnet V3/V4 are common — multi-hop
/// arb bots, MEV activity.)
///
/// This test pins the AC: register a V3 pool, push two Swap deltas at
/// block B with different scalars, then `restore_before_block(B)` and
/// assert the pool scalars match the pre-B (registration) state, not
/// post-first-Swap.
///
/// Note: the AC text says `restore_before_block(B+1)`, but that is the
/// no-op case (newest at B < B+1 returns current state = post-both-swaps,
/// correct for "before B+1"). The bug manifests at `restore_before_block(B)`,
/// which pops the block-B delta — the trigger exercised here.
#[test]
fn v3_restore_before_block_after_same_block_multi_swap_lands_on_pre_block() {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::RegisterV3PoolParams;

    let mut core = BotState::new();
    let pool_id = core
        .register_v3_pool(&RegisterV3PoolParams {
            address: Address::from([0xf7u8; 20]),
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 500,
            tick_spacing: 10,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
            ..Default::default()
        })
        .expect("test setup: V3 registration");

    let block_b = 9u64;
    // Two same-block Swaps with distinct scalars.
    let _ = core.apply_v3_swap_by_pool_id(
        pool_id,
        U256::from(2u128) << 96,
        2_000_000,
        -10,
        block_b,
        &[],
    );
    let _ = core.apply_v3_swap_by_pool_id(
        pool_id,
        U256::from(3u128) << 96,
        3_000_000,
        -20,
        block_b,
        &[],
    );

    // Sanity: current state reflects the second swap.
    {
        let s = core.get_v3_pool(pool_id).expect("registered");
        assert_eq!(s.sqrt_price_x96, U256::from(3u128) << 96);
    }

    // Roll back block B. Pre-fix this returned post-first-Swap scalars
    // (2<<96, 2_000_000, -10); the fix must land on the pre-B (registration)
    // state (1<<96, 1_000_000, 0).
    let _ = core.restore_pool_before_block(pool_id, block_b);
    let s = core.get_v3_pool(pool_id).expect("registered after restore");
    assert_eq!(
        s.sqrt_price_x96,
        U256::from(1u128) << 96,
        "same-block multi-swap restore lands on pre-B sqrt_price, not post-first-Swap"
    );
    assert_eq!(s.liquidity, 1_000_000);
    assert_eq!(s.tick, 0);
}
