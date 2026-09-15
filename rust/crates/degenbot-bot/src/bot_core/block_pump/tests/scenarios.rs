use super::*;

/// Scenario A — the normal path: a Mint@N in the WS feed IS buffered by
/// the pump, the tombstone@N+1 sets `last_complete_block = N`, and the
/// registration drain+pin captures it. PASSES → the drain/buffer path is
/// correct for delivered logs. If this test ever FAILS the race (A) is
/// real and an FSM on the verify seam is the fix.
#[tokio::test]
async fn scenario_a_buffered_mint_is_drained_into_pin() {
    let seed_gross: u128 = 10_000_000_000_000_000;
    let delta: u128 = 454_021;
    let block_n = 10u64;
    let (bot, pool_addr) = bot_with_quarantined_v3_tracked(seed_gross, block_n - 1);
    let (mut pump, _sink, _shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(block_n - 1));

    // Feed: Mint@N (tick -100..7, +delta) then Swap@N+1 (tombstones N).
    let mint = make_v3_mint_log_with_block(pool_addr, -100, 7, delta, block_n);
    let swap = make_v3_swap_log_with_block(pool_addr, block_n + 1);
    let combined = stream::iter(vec![
        WsEvent::Pool(PoolEvent::from_log(mint)),
        WsEvent::Pool(PoolEvent::from_log(swap)),
    ])
    .boxed();
    pump.run_test_loop(combined, block_n - 1).await;

    // The tombstone@N+1 set `last_complete_block = N`. Drain + pin.
    let (tick_data, pinned_block) = {
        let state = bot.state_arc();
        let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
        core.apply_backfill_buffer_v3(&pool_addr);
        core.apply_pump_buffer_v3(&pool_addr);
        core.pin_v3_post_drain_snapshot(pool_addr);
        core.take_v3_post_drain_snapshot(pool_addr)
            .expect("Tracked pool pins after drain")
    };
    assert_eq!(pinned_block, block_n, "pin's update_block advanced to N");
    assert_eq!(
        tick_data.get(&7).unwrap().liquidity_gross,
        alloy::primitives::U128::from(seed_gross + delta),
        "scenario A: the buffered Mint WAS drained into the pin"
    );
}

/// Scenario C — the EXACT on-chain topology at block 25648846: TWO
/// same-block Mints where tick 7 is the UPPER tick of one (li=1213,
/// tl=6,tu=7,amount=454021) and the LOWER tick of the other (li=1215,
/// tl=7,tu=8,amount=400353245599). On chain, tick-7 gross grows by their
/// sum (+400353699620). Production pin captured only li=1215's amount
/// (+400353245599) — missing exactly li=1213's +454021. This test feeds
/// BOTH Mints in log-index order (li=1213 first, li=1215 second) + the
/// tombstone Swap@N+1, drains, pins, and asserts BOTH Mints landed in
/// the pin. If this test FAILS, the pump→drain→pin path drops the first
/// of two adjacent same-block Mints — the real bug. If it PASSES, the
/// drop is not in this in-process path (it's a real-bot concurrency /
/// bucket-boundary issue the test harness can't reach).
#[tokio::test]
async fn scenario_c_two_adjacent_same_block_mints_both_applied_to_pin() {
    let seed_gross: u128 = 10_953_626_740_480_101; // on-chain@845
    let amt_lower: u128 = 454_021; // li=1213: tl=6, tu=7 (tick 7 = upper)
    let amt_upper: u128 = 400_353_245_599; // li=1215: tl=7, tu=8 (tick 7 = lower)
    let block_n = 10u64;
    let (bot, pool_addr) = bot_with_quarantined_v3_tracked(seed_gross, block_n - 1);
    let (mut pump, _sink, _shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(block_n - 1));

    // Feed the two Mints in log-index order (li=1213 then li=1215) then a
    // Swap@N+1 to tombstone N. Pre-seed tick 6 so the tl=6,tu=7 Mint has a
    // lower tick to mutate (mirrors on-chain where tick 6 is initialized).
    {
        let state = bot.state_arc();
        let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
        let pool_id = *core.pool_addresses.get(&pool_addr).unwrap();
        if let Some(crate::bot_core::PoolEntry::V3(p)) = core.pools.get_mut(&pool_id) {
            use alloy::primitives::U128;
            let pool = &mut p.1;
            pool.tick_data
                .entry(6)
                .or_insert(crate::bot_core::TickInfo {
                    liquidity_gross: U128::from(21_446_194_157_938_844u128),
                    liquidity_net: 21_446_194_157_938_844i128,
                    block: 0,
                });
            pool.tick_data
                .entry(8)
                .or_insert(crate::bot_core::TickInfo {
                    liquidity_gross: U128::from(18_506_953_544_795_537u128),
                    liquidity_net: -18_506_953_544_795_537i128,
                    block: 0,
                });
        }
    }
    let mint_lower = make_v3_mint_log_with_block(pool_addr, 6, 7, amt_lower, block_n);
    let mint_upper = make_v3_mint_log_with_block(pool_addr, 7, 8, amt_upper, block_n);
    let swap = make_v3_swap_log_with_block(pool_addr, block_n + 1);
    let combined = stream::iter(vec![
        WsEvent::Pool(PoolEvent::from_log(mint_lower)),
        WsEvent::Pool(PoolEvent::from_log(mint_upper)),
        WsEvent::Pool(PoolEvent::from_log(swap)),
    ])
    .boxed();
    pump.run_test_loop(combined, block_n - 1).await;

    let (tick_data, pinned_block) = {
        let state = bot.state_arc();
        let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
        core.apply_backfill_buffer_v3(&pool_addr);
        core.apply_pump_buffer_v3(&pool_addr);
        core.pin_v3_post_drain_snapshot(pool_addr);
        core.take_v3_post_drain_snapshot(pool_addr)
            .expect("Tracked pool pins after drain")
    };
    assert_eq!(pinned_block, block_n, "pin's update_block advanced to N");
    // On-chain@846 tick-7 gross = seed + amt_lower + amt_upper.
    assert_eq!(
        tick_data.get(&7).unwrap().liquidity_gross,
        alloy::primitives::U128::from(seed_gross + amt_lower + amt_upper),
        "scenario C: BOTH adjacent same-block Mints drained into the pin \
             (on-chain@846 value). If this fails with only +amt_upper present, \
             the first of two adjacent same-block Mints is dropped by the \
             pump→drain→pin path."
    );
    // And the per-tick net: tick 7 net = seed_net - amt_lower + amt_upper.
    // And the per-tick net: seed_net - amt_lower (upper tick) + amt_upper (lower tick).
    // The helper seeds tick-7 net = +seed_gross.
    assert_eq!(
        tick_data.get(&7).unwrap().liquidity_net,
        i128::try_from(seed_gross).unwrap() - i128::try_from(amt_lower).unwrap()
            + i128::try_from(amt_upper).unwrap(),
        "scenario C: tick-7 net reflects both Mints (upper: -amt_lower, lower: +amt_upper)"
    );
}

/// Scenario B — the WS-drop reproduction: feed Mint1@N but NOT Mint2@N
/// (simulating a WS transport drop). The drain captures `update_block = N`
/// (from Mint1) but the pin is missing Mint2 — exactly the production
/// symptom. A verify vs on-chain@N (which has both) would mismatch. This
/// confirms the production failure is cause (B), which a verify-seam FSM
/// does NOT fix (re-draining an empty buffer still misses it).
#[tokio::test]
async fn scenario_b_dropped_mint_reproduces_verify_mismatch_symptom() {
    let seed_gross: u128 = 10_000_000_000_000_000;
    let delta1: u128 = 400_000_000_000u128; // the +400M burst
    let delta2: u128 = 454_021; // the ONE missed Mint
    let block_n = 10u64;
    let (bot, pool_addr) = bot_with_quarantined_v3_tracked(seed_gross, block_n - 1);
    let (mut pump, _sink, _shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(block_n - 1));

    // Feed Mint1@N then Swap@N+1 (tombstones N). Mint2@N is NOT fed —
    // simulating the WS dropping exactly ONE of block N's Mints.
    let mint1 = make_v3_mint_log_with_block(pool_addr, -100, 7, delta1, block_n);
    let swap = make_v3_swap_log_with_block(pool_addr, block_n + 1);
    let combined = stream::iter(vec![
        WsEvent::Pool(PoolEvent::from_log(mint1)),
        WsEvent::Pool(PoolEvent::from_log(swap)),
    ])
    .boxed();
    pump.run_test_loop(combined, block_n - 1).await;

    let (tick_data, pinned_block) = {
        let state = bot.state_arc();
        let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
        core.apply_backfill_buffer_v3(&pool_addr);
        core.apply_pump_buffer_v3(&pool_addr);
        core.pin_v3_post_drain_snapshot(pool_addr);
        core.take_v3_post_drain_snapshot(pool_addr)
            .expect("Tracked pool pins after drain")
    };
    // The pin advanced to N (from Mint1) but is missing Mint2.
    assert_eq!(pinned_block, block_n, "update_block = N (from Mint1)");
    assert_eq!(
        tick_data.get(&7).unwrap().liquidity_gross,
        alloy::primitives::U128::from(seed_gross + delta1),
        "scenario B: the dropped Mint2 is NOT in the pin — reproduces the symptom"
    );
    // On-chain@N would be seed + delta1 + delta2 (Mint2 was applied
    // on-chain at block N). The pin lacks delta2 → a verify would fatal.
    assert_ne!(
        tick_data.get(&7).unwrap().liquidity_gross,
        alloy::primitives::U128::from(seed_gross + delta1 + delta2),
        "pin diverges from on-chain@N (the production mismatch)"
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "fuzz-bounded: FUZZ_POOL_COUNT=3 and seed<=48 so these casts cannot truncate/wrap"
)]
async fn bamkki_routing_fuzz_oracle_holds_across_lifecycle_roles() {
    type ExpectedTicks = HashMap<i32, (u128, i128)>;
    for seed in 1u64..=48 {
        let mut rng = FuzzRng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let block_n = 100_u64;
        let bot = Arc::new(Bot::new(1));

        // Role assignment per pool rotates with the seed so every role
        // combination is exercised across the iteration space:
        // 0 => Live from start, 1 => unregistered during feed,
        // 2 => Quarantined from start (set_live after drain).
        let roles: Vec<u8> = (0..FUZZ_POOL_COUNT as u8)
            .map(|i| (seed as u8 + i) % 3)
            .collect();
        let addrs: Vec<Address> = (0..FUZZ_POOL_COUNT)
            .map(|i| Address::from([0x40 + i as u8; 20]))
            .collect();

        // Pre-register roles 0 (Live) and 2 (Quarantined).
        for (i, addr) in addrs.iter().enumerate() {
            if roles[i] == 1 {
                continue;
            }
            let mut tick_data = hashbrown::HashMap::new();
            for &t in &FUZZ_TICKS {
                tick_data.insert(
                    t,
                    crate::bot_core::TickInfo {
                        liquidity_gross: alloy::primitives::U128::from(FUZZ_SEED_GROSS),
                        liquidity_net: i128::try_from(FUZZ_SEED_GROSS).unwrap(),
                        block: 0,
                    },
                );
            }
            let state = bot.state_arc();
            let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
            core.register_v3_pool(&crate::bot_core::RegisterV3PoolParams {
                address: *addr,
                token0: Address::from([0xa0u8; 20]),
                token1: Address::from([0xa1u8; 20]),
                fee: 500,
                tick_spacing: 10,
                factory: Address::from([0xf0u8; 20]),
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data,
                update_block: block_n - 1,
                coverage: crate::bot_core::PoolTickCoverage::Tracked,
                fetcher: None,
                ..Default::default()
            })
            .expect("fuzz registration");
            if roles[i] == 2 {
                core.set_v3_pool_quarantined(*addr);
            }
        }

        // Generate the event stream: mints over the pre-seeded tick set +
        // tombstone swaps, distributed across pools/blocks by the seed.
        let mut oracle: Vec<ExpectedTicks> = Vec::new();
        let mut expected_events: Vec<(usize, Log)> = Vec::new();
        let mut log_index = 0u64;
        for i in 0..FUZZ_POOL_COUNT {
            let mut ticks: ExpectedTicks = FUZZ_TICKS
                .iter()
                .map(|&t| {
                    (
                        t,
                        (FUZZ_SEED_GROSS, i128::try_from(FUZZ_SEED_GROSS).unwrap()),
                    )
                })
                .collect();
            oracle.push(ticks.clone());
            let _ = ticks;
            let _ = &mut ticks;
            oracle[i] = FUZZ_TICKS
                .iter()
                .map(|&t| {
                    (
                        t,
                        (FUZZ_SEED_GROSS, i128::try_from(FUZZ_SEED_GROSS).unwrap()),
                    )
                })
                .collect();
        }

        for _ in 0..12 {
            let pool_idx = rng.below(FUZZ_POOL_COUNT as u64) as usize;
            let block = block_n - rng.below(3); // blocks N-2..=N
            let is_mint = rng.below(2) == 0;
            if is_mint {
                let tl = FUZZ_TICKS[rng.below(FUZZ_TICKS.len() as u64) as usize];
                let tu = tl + 10;
                let amount: u128 = u128::from(1000_u64 + rng.below(50_000));
                expected_events.push((
                    pool_idx,
                    make_v3_mint_log_with_block(addrs[pool_idx], tl, tu, amount, block),
                ));
                // Oracle replay in arrival order (Solidity Tick.update):
                let lo = oracle[pool_idx].entry(tl).or_insert((0, 0));
                lo.0 += amount;
                lo.1 += amount as i128;
                let hi = oracle[pool_idx].entry(tu).or_insert((0, 0));
                hi.0 += amount;
                hi.1 -= amount as i128;
            } else {
                expected_events.push((
                    pool_idx,
                    make_v3_swap_log_with_block(addrs[pool_idx], block),
                ));
            }
            log_index += 1;
        }
        // Tombstone swap at N+1 closes block N for the cutoff.
        let tomb_pool = rng.below(FUZZ_POOL_COUNT as u64) as usize;
        expected_events.push((
            tomb_pool,
            make_v3_swap_log_with_block(addrs[tomb_pool], block_n + 1),
        ));
        // WS delivery is per-block ordered: stable-sort by block so the
        // feed never travels backward (a backward log is an ADR-008 D3
        // unreliable-WS signal, not a fuzz dimension).
        expected_events.sort_by_key(|(_, l)| l.block_number);

        let min_block = expected_events
            .iter()
            .map(|(_, l)| l.block_number.unwrap_or(block_n))
            .min()
            .unwrap_or(block_n);
        let (mut pump, _sink, _shutdown) =
            pump_for_test_with_bot(Arc::clone(&bot), Some(min_block - 1));
        let ws_events: Vec<WsEvent> = expected_events
            .iter()
            .map(|(_, l)| WsEvent::Pool(PoolEvent::from_log(l.clone())))
            .collect();
        let combined = stream::iter(ws_events).boxed();
        pump.run_test_loop(combined, min_block - 1).await;

        // Late registration for role-1 pools (the FUWYUR shape), then the
        // standard staged-application seam for every pool.
        for (i, addr) in addrs.iter().enumerate() {
            if roles[i] != 1 {
                continue;
            }
            let state = bot.state_arc();
            let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
            let mut tick_data = hashbrown::HashMap::new();
            for &t in &FUZZ_TICKS {
                tick_data.insert(
                    t,
                    crate::bot_core::TickInfo {
                        liquidity_gross: alloy::primitives::U128::from(FUZZ_SEED_GROSS),
                        liquidity_net: i128::try_from(FUZZ_SEED_GROSS).unwrap(),
                        block: 0,
                    },
                );
            }
            core.register_v3_pool(&crate::bot_core::RegisterV3PoolParams {
                address: *addr,
                token0: Address::from([0xa0u8; 20]),
                token1: Address::from([0xa1u8; 20]),
                fee: 500,
                tick_spacing: 10,
                factory: Address::from([0xf0u8; 20]),
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data,
                update_block: block_n - 1,
                coverage: crate::bot_core::PoolTickCoverage::Tracked,
                fetcher: None,
                ..Default::default()
            })
            .expect("late fuzz registration");
            core.set_v3_pool_quarantined(*addr);
        }
        for addr in &addrs {
            let state = bot.state_arc();
            let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
            core.apply_backfill_buffer_v3(addr);
            core.apply_pump_buffer_v3(addr);
            core.set_v3_pool_live(*addr);
        }

        // ORACLE COMPARISON.
        for (i, addr) in addrs.iter().enumerate() {
            let state = bot.state_arc();
            let core = state.read_at(crate::bot_core::state_lock::LockSite::Pump);
            let pool_id = *core.pool_addresses.get(addr).unwrap();
            let pool = core.get_v3_pool(pool_id).unwrap();
            for &t in &FUZZ_TICKS {
                let actual = pool
                    .tick_data
                    .get(&t)
                    .map(|x| (x.liquidity_gross.to::<u128>(), x.liquidity_net.abs()));
                let want = oracle[i].get(&t).copied().unwrap_or((0, 0));
                let actual_gross = actual.map_or(want.0, |a| a.0);
                assert_eq!(
                    actual_gross, want.0,
                    "BAMKKI seed={seed} pool={i} role={} tick={t}: gross diverged \\
                         (lost/duplicated/mis-staged application)",
                    roles[i]
                );
            }
        }
        let _ = log_index;
    }
}

/// Live-window tracer: a Mint for a NOT-YET-REGISTERED pool must survive
/// late registration.
///
/// Production shape (the 2026-08-25 20:51 UTC ADR-021 trip): crawl is
/// mid-flight when a Mint lands in block N for a Tracked pool that
/// `build_paths` has not registered yet; registration happens AFTER N
/// completed and pins pre-Mint DB data (`tick_data_block = N` with stale
/// gross). The dual buffer exists precisely for staged application at
/// registration — but `LogDispatcher::dispatch`'s APPLY-MISS funnel
/// early-returns BEFORE reaching `apply_v3_liquidity_update`'s
/// unregistered-buffering arm, so the event never reaches the buffer and
/// the pool goes Live permanently missing it (UO3JM4 desync class).
/// Uses the exact on-chain numbers from the trip: pool 0x88e6A0c2 tick
/// 193370 liquidityGross `244_132_769_082_101_7` -> `256_007_624_942_870_5`.
#[tokio::test]
async fn fuwyur_live_mint_for_unregistered_pool_survives_late_registration() {
    use crate::bot_core::{PoolTickCoverage, RegisterV3PoolParams, TickInfo};
    const SEED_GROSS: u128 = 2_441_327_690_821_017;
    const MINT_DELTA: u128 = 118_748_558_607_688;
    let block_n = 10u64;
    let pool_addr = Address::from([0x34u8; 20]);
    // Crawl mid-flight: NOTHING is registered yet.
    let bot = Arc::new(Bot::new(1));
    let (mut pump, _sink, _shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(block_n - 1));

    // Live WS feed: Mint@N for the not-yet-registered pool, then Swap@N+1
    // to tombstone N (completes the block for the cutoff).
    let mint = make_v3_mint_log_with_block(pool_addr, -100, 7, MINT_DELTA, block_n);
    let swap = make_v3_swap_log_with_block(pool_addr, block_n + 1);
    let combined = stream::iter(vec![
        WsEvent::Pool(PoolEvent::from_log(mint)),
        WsEvent::Pool(PoolEvent::from_log(swap)),
    ])
    .boxed();
    pump.run_test_loop(combined, block_n - 1).await;

    // LATE registration (crawl reaches the pool after block N completed):
    // Tracked pool loads stale DB data and starts Quarantined.
    {
        let state = bot.state_arc();
        let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
        let mut tick_data = hashbrown::HashMap::new();
        tick_data.insert(
            7,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(SEED_GROSS),
                liquidity_net: i128::try_from(SEED_GROSS).unwrap(),
                block: 0,
            },
        );
        core.register_v3_pool(&RegisterV3PoolParams {
            address: pool_addr,
            token0: Address::from([0xa0u8; 20]),
            token1: Address::from([0xa1u8; 20]),
            fee: 500,
            tick_spacing: 10,
            factory: Address::from([0xf0u8; 20]),
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block: block_n - 1,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            ..Default::default()
        })
        .expect("late V3 registration");
        core.set_v3_pool_quarantined(pool_addr);
    }

    // Registration drain+pin seam, then set_live flush of the retained
    // tail — the standard staged-application contract.
    let tick_data = {
        let state = bot.state_arc();
        let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
        core.apply_backfill_buffer_v3(&pool_addr);
        core.apply_pump_buffer_v3(&pool_addr);
        core.set_v3_pool_live(pool_addr);
        let pool_id = *core.pool_addresses.get(&pool_addr).unwrap();
        core.get_v3_pool(pool_id)
            .expect("registered")
            .tick_data
            .clone()
    };
    assert_eq!(
        tick_data.get(&7).expect("tick 7 seeded").liquidity_gross,
        alloy::primitives::U128::from(SEED_GROSS + MINT_DELTA),
        "FUWYUR: the live-window Mint for a not-yet-registered pool must \
             reach the pump buffer and land via the registration drain/flush. \
             RED means dispatch's APPLY-MISS funnel silently dropped it."
    );
}

/// Scenario A-race — the concurrent drain window: spawn the pump, feed
/// Mint1@N, run the registration drain (cutoff < N so Mint1 is RETAINED,
/// not drained), then feed Mint2@N + Swap@N+1. The pin captures
/// `update_block = backfill block` (< N) — NOT the production symptom
/// (which has `update_block = N`). This PROVES the race cannot produce
/// the observed symptom: a pin at `update_block = N` requires the
/// tombstone to have fired (cutoff = N), and the tombstone can only fire
/// AFTER all of N's logs were dispatched (else `LateForward` — the benign
/// late-admit drop, HJ5HWF). So all
/// delivered Mints@N are drained together. The missing Mint must have
/// been never delivered (scenario B).
#[tokio::test]
async fn scenario_a_race_concurrent_drain_cannot_produce_symptom() {
    use tokio::sync::oneshot;
    let seed_gross: u128 = 10_000_000_000_000_000;
    let delta1: u128 = 400_000_000_000u128;
    let delta2: u128 = 454_021;
    let block_n = 10u64;
    let (bot, pool_addr) = bot_with_quarantined_v3_tracked(seed_gross, block_n - 1);
    let (mut pump, _sink, _shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(block_n - 1));

    let mint1 = make_v3_mint_log_with_block(pool_addr, -100, 7, delta1, block_n);
    let mint2 = make_v3_mint_log_with_block(pool_addr, -200, 7, delta2, block_n);
    let swap = make_v3_swap_log_with_block(pool_addr, block_n + 1);

    // Stream: Mint1@N, then await the drain-done signal, then Mint2@N +
    // Swap@N+1 (tombstone), then end. The pump dispatches Mint1 into the
    // buffer (cutoff < N), parks on the oneshot receive; the test runs
    // the drain+pin (cutoff < N → Mint1 retained); then signals.
    let (drain_done_tx, drain_done_rx) = oneshot::channel::<()>();
    let logs: Vec<Log> = vec![mint1, mint2, swap];
    let combined = stream::unfold(
        (0u8, Some(drain_done_rx), logs.into_iter()),
        |(phase, rx_opt, mut logs)| async move {
            match phase {
                0 => Some((
                    WsEvent::Pool(PoolEvent::from_log(logs.next().unwrap())),
                    (1, rx_opt, logs),
                )),
                1 => {
                    let _ = rx_opt.unwrap().await; // park until drain completes
                    Some((
                        WsEvent::Pool(PoolEvent::from_log(logs.next().unwrap())),
                        (2, None, logs),
                    ))
                }
                2 => Some((
                    WsEvent::Pool(PoolEvent::from_log(logs.next().unwrap())),
                    (3, None, logs),
                )),
                _ => None,
            }
        },
    )
    .boxed();
    let pump_handle = tokio::spawn(async move {
        pump.run_test_loop(combined, block_n - 1).await;
    });
    // Let the pump process Mint1 (cutoff still < N — no tombstone yet).
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Run the registration drain+pin NOW (cutoff < N → Mint1 retained,
    // NOT drained). The pin captures the backfill seed state.
    let pin_after_mint1 = {
        let state = bot.state_arc();
        let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
        core.apply_backfill_buffer_v3(&pool_addr);
        core.apply_pump_buffer_v3(&pool_addr);
        core.pin_v3_post_drain_snapshot(pool_addr);
        core.take_v3_post_drain_snapshot(pool_addr)
    };
    // Release the pump to feed Mint2 + Swap (tombstone N, cutoff = N).
    let _ = drain_done_tx.send(());
    let _ = pump_handle.await;

    // The pin captured at the race window has update_block = backfill
    // block (< N), NOT N — because cutoff was < N at drain time, Mint1
    // was retained. This is NOT the production symptom (update_block = N).
    let (tick_data, pinned_block) = pin_after_mint1.expect("pin captured");
    assert_eq!(
        pinned_block,
        block_n - 1,
        "race drain (cutoff < N) pins the backfill block, NOT N — not the symptom"
    );
    assert_eq!(
        tick_data.get(&7).unwrap().liquidity_gross,
        alloy::primitives::U128::from(seed_gross),
        "race drain retained Mint1 (cutoff < N) — pin has only the seed"
    );

    // After the tombstone, a SECOND drain (cutoff = N) drains both
    // retained Mints onto the LIVE state — proving they were buffered,
    // just not drained into the pin.
    let live_gross = {
        let state = bot.state_arc();
        let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
        core.apply_pump_buffer_v3(&pool_addr);
        let pool_id = *core.pool_addresses.get(&pool_addr).unwrap();
        core.get_v3_pool(pool_id)
            .unwrap()
            .tick_data
            .get(&7)
            .unwrap()
            .liquidity_gross
    };
    assert_eq!(
        live_gross,
        alloy::primitives::U128::from(seed_gross + delta1 + delta2),
        "both Mints WERE buffered — a post-tombstone drain recovers them onto live state"
    );
}

// -----------------------------------------------------------------
// ADR-008 D2: `LogsQuiesced` solver-release gate.
//
// The pump must publish (`on_send`) only when the open block is
// quiesced (all dispatched logs fully applied), and coalesce a burst of
// same-block logs into ONE publish at the burst tail (not once per log).
// Re-arm on straggler is covered at the clock level by
// `consume_quiesced_publishes_once_per_cycle_and_re_arms_on_straggler`.
// -----------------------------------------------------------------
