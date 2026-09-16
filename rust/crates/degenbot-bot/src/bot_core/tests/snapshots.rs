use super::*;

#[test]
fn pool_update_block_tracks_forward_sync_and_returns_zero_for_unknown() {
    // AV42C7 accessor: `pool_update_block` is the per-pool freshness
    // signal the block-boundary FSM  will use to
    // re-solve at block completion. Registers a V2 pool at `update_block=0`,
    // applies a forward Sync, and asserts the accessor advances + returns 0
    // for an unregistered id (the FSM treats 0 as stale: a missing pool
    // defers its path until registered).
    let mut core = BotState::new();
    let pool_id = core
        .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
        .expect("test setup: V2 registration");
    assert_eq!(
        core.pool_update_block(pool_id),
        0,
        "freshly-registered V2 pool is at update_block 0"
    );
    assert_eq!(
        core.pool_update_block(999_999),
        0,
        "an unregistered pool_id reports update_block 0 (stale sentinel)"
    );
    core.apply_v2_sync(make_pool_addr(), U112::from(900), U112::from(2222), 7)
        .expect("forward Sync at block 7 applies");
    assert_eq!(
        core.pool_update_block(pool_id),
        7,
        "forward Sync advances the pool's update_block to the event block"
    );
}

#[test]
fn pool_state_head_is_max_update_block_across_all_pools() {
    // The solve/verify/sim anchor. During a backfill/drain desync the
    // pools are advanced ahead of the pump's header clock, so
    // `pool_state_head()` (max `update_block` across every pool) can
    // exceed the drain `block_number` — that head is the block the live
    // state reflects, and the correct anchor. Unchanged pools have
    // byte-identical EVM state from `update_block` to head, so one head
    // anchor reproduces each path's solver state (B2 collapse).
    let mut core = BotState::new();
    assert_eq!(core.pool_state_head(), 0, "empty state head is 0");

    // Register + advance two pools at different blocks; the head is the max.
    let a = core
        .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
        .expect("setup: pool A");
    core.apply_v2_sync(make_pool_addr(), U112::from(900), U112::from(2222), 5)
        .expect("forward Sync at block 5 applies");
    // Pool B on a distinct address (shared tokens): its update_block 12
    // overtakes pool A's 5, so the head tracks B.
    let b_addr = Address::from([0x12u8; 20]);
    let b = core
        .register_v2_pool(&RegisterV2PoolParams {
            address: b_addr,
            ..make_params(U112::from(3000), U112::from(4000))
        })
        .expect("setup: pool B");
    let _ = b;
    core.apply_v2_sync(b_addr, U112::from(3300), U112::from(4400), 12)
        .expect("forward Sync at block 12 applies");
    assert_eq!(core.pool_update_block(a), 5);
    assert_eq!(core.pool_state_head(), 12, "head = max update_block");

    // De-registering the leading pool (B, at 12) drops the head back to A's 5.
    let _ = a;
    core.unregister_pool(b_addr, None);
    assert_eq!(
        core.pool_state_head(),
        5,
        "head falls back to the next-highest update_block"
    );
}

#[test]
fn pool_tick_data_block_exposes_staged_liquidity_clock() {
    // Two-stamp rule / the `0x5653` staged-clock class: a CL pool whose
    // PRICE clock (`update_block`) is fresh but whose LIQUIDITY clock
    // (`tick_data_block`) lags. The scalar-only ADR-021 diff keys on
    // `update_block` and therefore cannot see this stagger; the new
    // `pool_tick_data_block` accessor makes it observable so a tick-map
    // consumer can key on the right clock.
    let mut core = BotState::new();
    let pool_addr = Address::from([0xabu8; 20]);
    let pool_id = register_v3_on_core(&mut core, pool_addr, 100);
    assert_eq!(
        core.pool_update_block(pool_id),
        100,
        "registered pool: price clock at seed block"
    );
    assert_eq!(
        core.pool_tick_data_block(pool_id),
        100,
        "registered pool: liquidity clock at seed block"
    );
    // Simulate a buggy scalar-only advance that moves the price clock
    // without touching the tick map (direct poke — the two-stamp mutators
    // keep them in lockstep, which is exactly why this class only arises
    // from a bug / non-CL-advancing path).
    if let Some(crate::bot_core::PoolEntry::V3(p)) = core.pools.get_mut(&pool_id) {
        let state = &mut p.1;
        state.update_block = 200;
    }
    assert_eq!(core.pool_update_block(pool_id), 200, "price clock advanced");
    assert_eq!(
        core.pool_tick_data_block(pool_id),
        100,
        "liquidity clock still lags → the stagger is observable"
    );
    // Non-CL families fall back to `update_block` for the total accessor.
    let v2_id = core
        .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
        .expect("test setup: V2 registration");
    assert_eq!(core.pool_tick_data_block(v2_id), 0);
    assert_eq!(
        core.pool_tick_data_block(999_999),
        0,
        "unknown id → stale sentinel"
    );
}

#[test]
fn seed_genesis_anchors_journal_without_advancing_clocks() {
    // The split-seed builder (price at HEAD, tick map at the DB block)
    // replaces its old `apply_swap` genesis with `seed_genesis`: a
    // `before == after` journal delta that makes the journal non-empty
    // (so a mid-window reorg restores instead of a graceful
    // `NoStatePriorToBlock` shutdown) WITHOUT advancing either clock
    // (two-stamp rule).
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::{RegisterV3PoolParams, TickInfo};
    let mut core = BotState::new();
    let pool_addr = Address::from([0xe1u8; 20]);
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
    // Empty journal → not restorable (would be a graceful too-deep shutdown).
    assert!(!core.has_state_prior_to(pool_id, 100));
    // Seed genesis at the DB floor anchor.
    assert_eq!(
        core.seed_genesis_by_pool_id(pool_id, db_block),
        Some(pool_id)
    );
    assert!(core.has_state_prior_to(pool_id, 100), "non-empty journal");
    // The anchor advances NO clock.
    assert_eq!(core.pool_update_block(pool_id), head);
    assert_eq!(core.pool_tick_data_block(pool_id), db_block);
    // Restore below the anchor pops the before==after delta → seeded state.
    let restored = core.restore_pool_before_block(pool_id, 100);
    assert!(restored.unwrap().is_ok());
    assert_eq!(core.pool_update_block(pool_id), head);
    assert_eq!(core.pool_tick_data_block(pool_id), db_block);
    assert_eq!(core.seed_genesis_by_pool_id(999_999, 1), None);
}

/// the snapshot seed must be retained separately from the live
/// `tick_data` so step-1 verify can compare the *seed* against
/// on-chain@snapshot_block, NOT the pump-mutated current. During a rolling
/// start (`resume()` precedes `build_paths`) the pump applies Mint/Burn to
/// `tick_data`; without a pinned seed, step-1 reads engine-current (seed +
/// journal) vs on-chain@snapshot (pre-journal) → false mismatch on every
/// active pool (logs/perm-V2-V3-V2.log). The seed is pinned at registration
/// and never mutated by `apply_v3_liquidity_update`.
#[test]
fn v3_snapshot_seed_survives_pump_liquidity_update() {
    use alloy::primitives::U128;
    let mut core = BotState::new();
    let v3_addr = make_pool_addr();

    // Seed tick_data with one initialized tick (gross=L, net=+L).
    let liq: u128 = 1_000_000;
    let liq_u128 = U256::from(liq).to::<U128>();
    let mut seed = HashMap::new();
    seed.insert(
        -60,
        TickInfo {
            liquidity_gross: liq_u128,
            liquidity_net: i128::try_from(liq).unwrap(),
            block: 0,
        },
    );
    let seed_clone = seed.clone();

    core.register_v3_pool(&RegisterV3PoolParams {
        address: v3_addr,
        token0: make_token0(),
        token1: make_token1(),
        fee: 3_000,
        tick_spacing: 60,
        factory: make_factory(),
        sqrt_price_x96: U256::from(1u64) << 96,
        liquidity: 0,
        tick: 0,
        tick_data: seed,
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    })
    .expect("test setup: V3 registration");
    // Tracked pools register `Quarantined`; transition to `Live`
    // (the driver's post-verify `set_live`) so the pump update below
    // direct-applies as this test models.
    core.set_v3_pool_live(v3_addr);

    // The seed is pinned at registration for Tracked (snapshot) pools.
    assert_eq!(
        core.v3_snapshot_seed(v3_addr).cloned(),
        Some(seed_clone.clone()),
        "Tracked pool must pin its snapshot seed at registration"
    );

    // Pump applies a Mint at block 1, mutating tick -60's gross.
    core.apply_v3_liquidity_update(v3_addr, -60, 60, 500_i128, 1);

    // Live tick_data CHANGED (journal applied) ...
    let live_gross = core
        .get_v3_pool(core.pool_id_by_address(&v3_addr).unwrap())
        .and_then(|s| s.tick_data.get(&-60))
        .map(|t| t.liquidity_gross.to::<u128>());
    assert_ne!(
        live_gross,
        Some(liq),
        "pump Mint must mutate the live tick_data (precondition)"
    );

    // ... but the pinned seed is UNCHANGED — step-1 can still verify the
    // snapshot block against the seed, not the pump-corrupted current.
    assert_eq!(
        core.v3_snapshot_seed(v3_addr).cloned(),
        Some(seed_clone.clone()),
        "snapshot seed must be immutable across pump Mint/Burn (the rolling-start race fix)"
    );

    // `take_v3_snapshot_seed` returns the seed once and clears the slot
    // (memory is freed after step-1 verify; the seed is never needed again).
    let taken = core.take_v3_snapshot_seed(v3_addr);
    assert_eq!(taken, Some(seed_clone), "take returns the pinned seed");
    assert_eq!(
        core.v3_snapshot_seed(v3_addr),
        None,
        "take clears the slot so the seed is verified exactly once"
    );
}

/// Step-2 (post-drain) twin of `v3_snapshot_seed_survives_pump_liquidity_update`.
/// The post-drain pin is captured atomically with `apply_buffer_v3`'s final
/// drain and must be IMMUTABLE across a subsequent pump Mint/Burn — otherwise
/// step-2 (verify post-drain state vs on-chain@backfill) reads engine-current
/// (drain + pump journal) vs on-chain@backfill (pre-journal) → a false
/// mismatch on every active pool during a rolling start
/// (logs/verify-race-hotloop.log: tick 59940, `update_block=25396803 >
/// block=25396790`). The pin is taken once (step-2 verify) then freed.
#[test]
fn v3_post_drain_snapshot_survives_pump_liquidity_update() {
    use alloy::primitives::U128;
    let mut core = BotState::new();
    let v3_addr = make_pool_addr();

    let liq: u128 = 1_000_000;
    let liq_u128 = U256::from(liq).to::<U128>();
    let mut seed = HashMap::new();
    seed.insert(
        -60,
        TickInfo {
            liquidity_gross: liq_u128,
            liquidity_net: i128::try_from(liq).unwrap(),
            block: 0,
        },
    );
    let drain_clone = seed.clone();

    core.register_v3_pool(&RegisterV3PoolParams {
        address: v3_addr,
        token0: make_token0(),
        token1: make_token1(),
        fee: 3_000,
        tick_spacing: 60,
        factory: make_factory(),
        sqrt_price_x96: U256::from(1u64) << 96,
        liquidity: 0,
        tick: 0,
        tick_data: seed,
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    })
    .expect("test setup: V3 registration");
    // Tracked pools register `Quarantined`; transition to `Live` so
    // the pump Mint below direct-applies (this test's model).
    core.set_v3_pool_live(v3_addr);

    // Pin the post-drain state atomically with the drain (no buffer here →
    // pin == current tick_data == the seed at registration). This is what
    // `apply_buffer_v3` does inside its single core.write() hold.
    core.pin_v3_post_drain_snapshot(v3_addr);

    // A pump Mint lands AFTER the drain — mutating the live tick_data.
    core.apply_v3_liquidity_update(v3_addr, -60, 60, 500_i128, 1);

    // Live tick_data CHANGED (journal applied) ...
    let live_gross = core
        .get_v3_pool(core.pool_id_by_address(&v3_addr).unwrap())
        .and_then(|s| s.tick_data.get(&-60))
        .map(|t| t.liquidity_gross.to::<u128>());
    assert_ne!(
        live_gross,
        Some(liq),
        "pump Mint must mutate the live tick_data (precondition)"
    );

    // ... but the pinned post-drain snapshot is UNCHANGED — step-2 verifies
    // the drain-time state, not the pump-corrupted current.
    let taken = core.take_v3_post_drain_snapshot(v3_addr);
    let (tick_data, pinned_block) = taken.expect("Tracked pool pins post-drain");
    assert_eq!(
        tick_data, drain_clone,
        "post-drain pin must be frozen at drain time, not pump-mutated current (step-2 race fix)"
    );
    assert_eq!(
        pinned_block, 0,
        "no buffer events drained → pin's block is the registration update_block (0)"
    );
    assert_eq!(
        core.take_v3_post_drain_snapshot(v3_addr),
        None,
        "take clears the post-drain slot (verified exactly once)"
    );
}

/// `Sparse` pools have no complete `tick_data` to pin — the post-drain pin
/// stays `None` (step-2 verify is a no-op, same as the seed for sparse).
#[test]
fn v3_post_drain_snapshot_is_none_for_sparse_pools() {
    let mut core = BotState::new();
    let v3_addr = make_pool_addr();
    core.register_v3_pool(&RegisterV3PoolParams {
        address: v3_addr,
        token0: make_token0(),
        token1: make_token1(),
        fee: 3_000,
        tick_spacing: 60,
        factory: make_factory(),
        sqrt_price_x96: U256::from(1u64) << 96,
        liquidity: 0,
        tick: 0,
        tick_data: HashMap::new(),
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
        ..Default::default()
    })
    .expect("test setup: V3 registration");
    core.pin_v3_post_drain_snapshot(v3_addr);
    assert_eq!(
        core.take_v3_post_drain_snapshot(v3_addr),
        None,
        "sparse pools must not pin post-drain (no complete tick_data)"
    );
}

/// Regression (post-drain pin block race, 2026-06-29): the post-drain pin
/// must carry the block its `tick_data` was computed at — namely the
/// `update_block` at pin time, which reflects the last drained backfill OR
/// pump event's block. Pre-fix the pin stored `tick_data` only and the
/// step-2 verify compared it against on-chain@`verify_backfill_block`
/// (a start()-time constant). For an active pool on a slow `build_paths`,
/// the pump buffer accumulated Mint/Burn events at blocks PAST the
/// backfill boundary; draining them advanced `tick_data` to a state that
/// matched on-chain at a LATER block, so the verify fabricated a mismatch
/// and crashed the bot.
///
/// This test reproduces the exact shape: a seed at block S, a backfill
/// event at S+1 (within the backfill window), and a pump event at block B
/// (past `verify_backfill_block`). After draining both buffers + pinning,
/// the pin's block must be B (the pump event's block) — the block on-chain
/// tick data actually matches at — NOT `verify_backfill_block`.
#[test]
fn v3_post_drain_snapshot_carries_drained_block_not_backfill_block() {
    use alloy::primitives::U128;
    let mut core = BotState::new();
    let v3_addr = make_pool_addr();

    // Snapshot block S (the registration `update_block`).
    let snapshot_block: u64 = 100;
    // Backfill boundary (start()'s `verify_backfill_block`).
    let backfill_block: u64 = 150;
    // Pump event lands at block B, PAST the backfill boundary — the
    // rolling-start window the bot hit in production (a Mint that fired
    // between `subscribe()` and this pool's `register_v3_pool`).
    let pump_block: u64 = backfill_block + 29;

    let seed_liq: u128 = 1_000_000;
    let liq_u128 = U256::from(seed_liq).to::<U128>();
    let mut seed = HashMap::new();
    seed.insert(
        -60,
        TickInfo {
            liquidity_gross: liq_u128,
            liquidity_net: i128::try_from(seed_liq).unwrap(),
            block: 0,
        },
    );

    // Pre-registration: buffer a backfill Mint at S+1 (within the
    // snapshot→backfill window) and a pump Mint at `pump_block` (past the
    // backfill boundary — the pump was already running during `build_paths`,
    // so the pool's Mint at `pump_block` landed in the unregistered-pool
    // pump buffer via `apply_v3_liquidity_update`).
    core.buffer_backfill_v3_liquidity_update(v3_addr, -60, 60, 500_i128, snapshot_block + 1);
    core.apply_v3_liquidity_update(v3_addr, -60, 60, 750_i128, pump_block);

    core.register_v3_pool(&RegisterV3PoolParams {
        address: v3_addr,
        token0: make_token0(),
        token1: make_token1(),
        fee: 3_000,
        tick_spacing: 60,
        factory: make_factory(),
        sqrt_price_x96: U256::from(1u64) << 96,
        liquidity: 0,
        tick: 0,
        tick_data: seed,
        update_block: snapshot_block,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    })
    .expect("test setup: V3 registration");

    // Drain both buffers + pin — exactly what `apply_buffer_v3` does
    // inside its single `core.write()` hold.
    core.apply_backfill_buffer_v3(&v3_addr);
    // the gated pump drain only yields fully-completed blocks.
    // Mirror the live pump's ADR-008 D1 tombstone (first log of
    // `pump_block`+1 closes `pump_block`) so the drain takes the pump
    // Mint at `pump_block` rather than leaving it behind the gate.
    core.advance_pump_complete_cutoff(pump_block);
    core.apply_pump_buffer_v3(&v3_addr);
    core.pin_v3_post_drain_snapshot(v3_addr);

    // The pin must carry the pump event's block — the block the drained
    // `tick_data` actually matches on-chain at. Pre-fix the pin carried no
    // block at all and the verify used `backfill_block` → false mismatch.
    let taken = core.take_v3_post_drain_snapshot(v3_addr);
    let (tick_data, pinned_block) = taken.expect("Tracked pool pins post-drain");
    assert_eq!(
        pinned_block, pump_block,
        "pin's block must be the last drained event's block (the pump Mint at {pump_block}), \
         not the backfill boundary ({backfill_block}) — pre-fix the verify used the wrong \
         block and fabricated a mismatch on every active pool during a slow build_paths"
    );

    // Sanity: the drained tick_data reflects seed + backfill Mint + pump Mint.
    let t = tick_data.get(&-60).expect("tick -60 present");
    assert_eq!(
        t.liquidity_gross,
        U128::from(seed_liq + 500 + 750),
        "drained tick_data = seed + backfill Mint + pump Mint"
    );

    // Idempotent take: second call returns None (verified exactly once).
    assert_eq!(
        core.take_v3_post_drain_snapshot(v3_addr),
        None,
        "take clears the pin slot (verified exactly once)"
    );
}

/// OVVLGO: the V4 twin of the rolling-start race regression. The V4 seed
/// must survive a pump `ModifyLiquidity` event so step-1 (seed-verify at
/// the snapshot block) is race-free under a rolling start. Mirrors
/// `v3_snapshot_seed_survives_pump_liquidity_update` for the V4
/// `(pool_manager, pool_id)` keying.
#[test]
fn v4_snapshot_seed_survives_pump_modify_liquidity() {
    use crate::bot_core::{RegisterV4PoolParams, V4PoolKey};
    use alloy::primitives::{I256, U128};
    let pool_manager = Address::from([0x44u8; 20]);
    let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0xeeu8; 32];
    let mut core = BotState::new();

    let gross: u128 = 1_000_000;
    let liq_u128 = U256::from(gross).to::<U128>();
    let mut seed = HashMap::new();
    seed.insert(
        -60,
        TickInfo {
            liquidity_gross: liq_u128,
            liquidity_net: i128::try_from(gross).unwrap(),
            block: 0,
        },
    );
    let seed_clone = seed.clone();

    core.register_v4_pool(&RegisterV4PoolParams {
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
        liquidity: 0,
        tick: 0,
        tick_data: seed,
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
    })
    .expect("V4 pool registers");
    // Tracked pools register `Quarantined`; transition to `Live`
    // (the driver's post-verify `set_live`) so the pump update below
    // direct-applies as this test models.
    core.set_v4_pool_live(pool_manager, pool_id_bytes);

    assert_eq!(
        core.v4_snapshot_seed(pool_manager, &pool_id_bytes).cloned(),
        Some(seed_clone.clone()),
        "Tracked V4 pool must pin its snapshot seed at registration"
    );

    // Pump applies a ModifyLiquidity at block 1, mutating tick -60's gross.
    core.apply_v4_liquidity_update(
        pool_manager,
        pool_id_bytes,
        -60,
        60,
        I256::try_from(500_i128).unwrap(),
        1,
    );

    // Live tick_data CHANGED (journal applied) ...
    let live_gross = {
        let pid = core
            .v4_pool_id_by_key(pool_manager, &pool_id_bytes)
            .expect("registered");
        core.get_v4_pool(pid)
            .and_then(|s| s.tick_data.get(&-60))
            .map(|t| t.liquidity_gross.to::<u128>())
    };
    assert_ne!(
        live_gross,
        Some(gross),
        "pump ModifyLiquidity must mutate the live tick_data (precondition)"
    );

    // ... but the pinned seed is UNCHANGED — step-1 verifies seed, not current.
    assert_eq!(
        core.v4_snapshot_seed(pool_manager, &pool_id_bytes).cloned(),
        Some(seed_clone.clone()),
        "V4 snapshot seed must be immutable across pump ModifyLiquidity (rolling-start race fix)"
    );

    // take: returns the seed exactly once then clears.
    let taken = core.take_v4_snapshot_seed(pool_manager, &pool_id_bytes);
    assert_eq!(taken, Some(seed_clone), "take returns the pinned seed");
    assert_eq!(
        core.v4_snapshot_seed(pool_manager, &pool_id_bytes),
        None,
        "take clears the V4 seed slot (verified exactly once)"
    );
}

/// Step-2 (post-drain) V4 twin of
/// `v4_snapshot_seed_survives_pump_modify_liquidity`. The V4 post-drain pin
/// is captured atomically with `apply_buffer_v4`'s final drain and must be
/// IMMUTABLE across a subsequent pump `ModifyLiquidity` — otherwise step-2
/// reads engine-current (drain + pump journal) vs on-chain@backfill
/// (pre-journal) → a false mismatch on every active V4 pool during a
/// rolling start. Pinned for Tracked pools only (Sparse → None, no-op).
#[test]
fn v4_post_drain_snapshot_survives_pump_modify_liquidity() {
    use crate::bot_core::{RegisterV4PoolParams, V4PoolKey};
    use alloy::primitives::{I256, U128};
    let pool_manager = Address::from([0x44u8; 20]);
    let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0xeeu8; 32];
    let mut core = BotState::new();

    let gross: u128 = 1_000_000;
    let liq_u128 = U256::from(gross).to::<U128>();
    let mut seed = HashMap::new();
    seed.insert(
        -60,
        TickInfo {
            liquidity_gross: liq_u128,
            liquidity_net: i128::try_from(gross).unwrap(),
            block: 0,
        },
    );
    let drain_clone = seed.clone();

    core.register_v4_pool(&RegisterV4PoolParams {
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
        liquidity: 0,
        tick: 0,
        tick_data: seed,
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
    })
    .expect("V4 pool registers");
    // Tracked pools register `Quarantined`; transition to `Live` so
    // the pump ModifyLiquidity below direct-applies (this test's model).
    core.set_v4_pool_live(pool_manager, pool_id_bytes);

    // Pin post-drain state atomically with the drain (what apply_buffer_v4
    // does inside its single core.write() hold).
    core.pin_v4_post_drain_snapshot(pool_manager, &pool_id_bytes);

    // Pump ModifyLiquidity lands AFTER the drain.
    core.apply_v4_liquidity_update(
        pool_manager,
        pool_id_bytes,
        -60,
        60,
        I256::try_from(500_i128).unwrap(),
        1,
    );

    // Live tick_data CHANGED ...
    let live_gross = {
        let pid = core
            .v4_pool_id_by_key(pool_manager, &pool_id_bytes)
            .expect("registered");
        core.get_v4_pool(pid)
            .and_then(|s| s.tick_data.get(&-60))
            .map(|t| t.liquidity_gross.to::<u128>())
    };
    assert_ne!(
        live_gross,
        Some(gross),
        "pump ModifyLiquidity must mutate the live tick_data (precondition)"
    );

    // ... but the pinned post-drain snapshot is UNCHANGED — step-2 verifies
    // drain-time state, not pump-corrupted current.
    let taken = core.take_v4_post_drain_snapshot(pool_manager, &pool_id_bytes);
    let (tick_data, pinned_block) = taken.expect("Tracked V4 pool pins post-drain");
    assert_eq!(
        tick_data, drain_clone,
        "V4 post-drain pin must be frozen at drain time (step-2 race fix)"
    );
    assert_eq!(
        pinned_block, 0,
        "no buffer events drained → pin's block is the registration update_block (0)"
    );
    assert_eq!(
        core.take_v4_post_drain_snapshot(pool_manager, &pool_id_bytes),
        None,
        "take clears the V4 post-drain slot (verified exactly once)"
    );
}

/// `Sparse` V4 pools have no complete `tick_data` to pin — post-drain pin
/// stays `None` (step-2 verify is a no-op, same as the V4 seed for sparse).
#[test]
fn v4_post_drain_snapshot_is_none_for_sparse_pools() {
    use crate::bot_core::{RegisterV4PoolParams, V4PoolKey};
    let pool_manager = Address::from([0x44u8; 20]);
    let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0xeeu8; 32];
    let mut core = BotState::new();
    core.register_v4_pool(&RegisterV4PoolParams {
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
        liquidity: 0,
        tick: 0,
        tick_data: HashMap::new(),
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
    })
    .expect("V4 sparse pool registers");
    core.pin_v4_post_drain_snapshot(pool_manager, &pool_id_bytes);
    assert_eq!(
        core.take_v4_post_drain_snapshot(pool_manager, &pool_id_bytes),
        None,
        "sparse V4 pools must not pin post-drain"
    );
}

/// `Bot::load_snapshot_from_db` against the parity
/// fixture DB (`crates/degenbot-db/tests/fixtures/parity.db`) — opens a
/// `SnapshotDb` (held read tx) + records `S = min(newest_update_block(V3),
/// V4)` read INSIDE the held tx. The `SnapshotStore` is NOT populated
/// (the Store is retired; the held tx replaces it).
#[test]
fn load_snapshot_from_db_populates_store_and_seed_block() {
    use degenbot_db::snapshot::TickMapDb;
    use std::path::PathBuf;
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../degenbot-db/tests/fixtures/parity.db");
    if !fixture.exists() {
        // The fixture lives in the sibling crate; skip if absent.
        eprintln!("skipping: parity fixture not at {}", fixture.display());
        return;
    }
    // Pin the ADR-052 D1 heal-at-open killswitch so this read never rewrites
    // the committed fixture.
    std::env::set_var(degenbot_db::AUTO_HEAL_ENV, "0");
    let (snap, _state) = degenbot_db::snapshot_db::SnapshotDb::open(&fixture).unwrap();
    let bot = Bot::new(8453);
    bot.load_snapshot_from_db(&snap, 8453).unwrap();

    let state = bot.state_arc();
    let core = state.read_at(crate::bot_core::state_lock::LockSite::Core);
    // S = min(newest V3, newest V4). The parity fixture records both; the
    // exact S is whatever the fixture DB carries (we assert it's Some and
    // matches the per-family min computed independently inside the SAME
    // held tx).
    let v3 = snap
        .fetch_newest_update_block(8453, degenbot_db::read::ExchangeFamily::V3)
        .unwrap();
    let v4 = snap
        .fetch_newest_update_block(8453, degenbot_db::read::ExchangeFamily::V4)
        .unwrap();
    let expected_s = match (v3, v4) {
        (Some(a), Some(b)) => Some(u64::try_from(a.min(b)).expect("block number non-negative")),
        (Some(a), None) => Some(u64::try_from(a).expect("block number non-negative")),
        (None, Some(b)) => Some(u64::try_from(b).expect("block number non-negative")),
        (None, None) => None,
    };
    assert_eq!(
        core.snapshot_seed_block(),
        expected_s,
        "snapshot_seed_block must be min(newest_update_block(V3), V4)"
    );
}

/// `load_snapshot_from_db` on an empty chain → no snapshot loaded,
/// S = None (cold-start path: the pump will anchor on `first_observed_block`).
#[test]
fn load_snapshot_from_db_empty_chain_is_cold_start() {
    let (snap, _state) = degenbot_db::snapshot_db::SnapshotDb::open_in_memory().unwrap();
    let bot = Bot::new(1);
    bot.load_snapshot_from_db(&snap, 1).unwrap();
    let state = bot.state_arc();
    let core = state.read_at(crate::bot_core::state_lock::LockSite::Core);
    // No pools → no seed block (cold-start path: the pump anchors on
    // `first_observed_block`).
    assert_eq!(
        core.snapshot_seed_block(),
        None,
        "empty chain → no seed block (cold start)"
    );
}

// ── verify-dbg visibility probes (verify_dbg) ───────────────────
//
// Asserts the WIRING the probes rely on: a tracked V3 pool's pump
// Mint/Burn is counted by `v3_buffer.pump_count_at_or_below` through the
// `BotState` field, `advance_pump_complete_cutoff` advances the shared
// pump-completeness cutoff (the StageMachine tombstone, 3M5PO5), and
// `pin_v3_post_drain_snapshot` +
// `set_v3_pool_live` remain behavior-preserving under the buffered
// tail (the apply path executes regardless of the gate — the
// diagnostic branch is pure logging).
#[test]
fn verify_dbg_mark_complete_and_pin_are_behavior_preserving() {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::RegisterV3PoolParams;
    let mut core = BotState::new();
    let pool_addr = Address::from([0xf7u8; 20]);
    let _id = core
        .register_v3_pool(&RegisterV3PoolParams {
            address: pool_addr,
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
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            ..Default::default()
        })
        .expect("test setup: V3 registration");
    // Quarantine so live Mint/Burn route to the pump buffer (the
    // registration-seam posture during drain+pin+verify).
    core.set_v3_pool_quarantined(pool_addr);
    // Pump-buffer a Mint + a same-block Burn for block 100.
    core.apply_v3_liquidity_update(pool_addr, -10, 10, 500_i128, 100);
    core.apply_v3_liquidity_update(pool_addr, -10, 10, -500_i128, 100);
    // Pre-mark: two pump events, no complete block yet.
    assert_eq!(core.v3_buffer.pump_count_at_or_below(&pool_addr, 100), 2);
    assert_eq!(core.pump_complete_cutoff(), 0);
    assert_eq!(core.v3_buffer.pump_total_at_or_below(100), 2);
    // Mark block 100 complete (what the pump does at N+1's tombstone).
    core.advance_pump_complete_cutoff(100);
    assert_eq!(core.pump_complete_cutoff(), 100);
    // Drain + pin: the gated drain yields both events, then the pin
    // captures the post-drain pair. (apply_pump_buffer_v3 + pin are the
    // exact sequence the registration seam runs.)
    core.apply_pump_buffer_v3(&pool_addr);
    core.pin_v3_post_drain_snapshot(pool_addr);
    let (pinned_ticks, pinned_block) = core
        .take_v3_post_drain_snapshot(pool_addr)
        .expect("pin was captured for a Tracked pool");
    assert_eq!(pinned_block, 100, "pin captured the drained update_block");
    // The net-zero Mint+Burn leaves gross=0 on the boundary ticks and
    // creates NO initialized tick (a zero gross is pruned) → the pin carries
    // an empty map. The point: the pin pair is self-consistent with the
    // drain the probe correlates.
    assert_eq!(pinned_ticks.len(), 0, "net-zero Mint+Burn yields no tick");
    // set_live under an empty retained tail is a no-op flush (the probe
    // would log drained_retained_tail=0).
    core.set_v3_pool_live(pool_addr);
}

// ── pin clamp regression (DFQYM5 fabricated-mismatch fix) ──────────────
//
// The pin stores (tick_map, liquidity_clock) and step-2 verify compares
// the map against on-chain @ that block. If the pump has any UNDRAINED
// event at/below the pool's liquidity clock (an in-progress block the
// drain held back at the tombstone cutoff), the map is NOT complete AT
// that clock block — verifying there would compare an incomplete map
// against the full on-chain block and fabricate a mismatch. The pin must
// clamp the verify block down to `pump_complete_cutoff`. Both branches are
// asserted below: the clamp (undrained > 0) and the benign preserve
// (undrained == 0, mod.rs:580 seed).
#[test]
fn pin_clamps_verify_block_to_complete_cutoff_when_pump_undrained() {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::RegisterV3PoolParams;
    let mut core = BotState::new();
    let pool_addr = Address::from([0xf8u8; 20]);
    core.register_v3_pool(&RegisterV3PoolParams {
        address: pool_addr,
        token0: Address::ZERO,
        token1: Address::from([1u8; 20]),
        fee: 500,
        tick_spacing: 10,
        factory: Address::ZERO,
        sqrt_price_x96: U256::from(1u128) << 96,
        liquidity: 1_000_000,
        tick: 0,
        tick_data: HashMap::new(),
        // DB-seeded seed: the liquidity clock is exact at block 100.
        update_block: 100,
        tick_data_block: Some(100),
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    })
    .expect("test setup: V3 registration");
    // Quarantine so live Mint/Burn route to the pump buffer.
    core.set_v3_pool_quarantined(pool_addr);
    // Pump-buffer an UNDRAINED Mint at block 100, but only tombstone the
    // pump through block 99 → the drain (apply_pump_buffer_v3) holds the
    // block-100 event back, so the map is NOT complete at its 100 clock.
    core.apply_v3_liquidity_update(pool_addr, -10, 10, 500_i128, 100);
    core.advance_pump_complete_cutoff(99);
    assert_eq!(core.pump_complete_cutoff(), 99);
    assert_eq!(core.v3_buffer.pump_count_at_or_below(&pool_addr, 100), 1);
    core.apply_pump_buffer_v3(&pool_addr);
    core.pin_v3_post_drain_snapshot(pool_addr);
    let (_ticks, pinned_block) = core
        .take_v3_post_drain_snapshot(pool_addr)
        .expect("pin captured for a Tracked pool");
    assert_eq!(
        pinned_block, 99,
        "fabricated mismatch: pin must clamp the verify block down to the \
         complete cutoff (99), not the seed liquidity clock (100), when the \
         pump has undrained events at/below the clock"
    );
}

#[test]
fn pin_preserves_clock_block_when_no_undrained_events() {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::RegisterV3PoolParams;
    let mut core = BotState::new();
    let pool_addr = Address::from([0xf9u8; 20]);
    core.register_v3_pool(&RegisterV3PoolParams {
        address: pool_addr,
        token0: Address::ZERO,
        token1: Address::from([1u8; 20]),
        fee: 500,
        tick_spacing: 10,
        factory: Address::ZERO,
        sqrt_price_x96: U256::from(1u128) << 96,
        liquidity: 1_000_000,
        tick: 0,
        tick_data: HashMap::new(),
        update_block: 100,
        tick_data_block: Some(100),
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
        ..Default::default()
    })
    .expect("test setup: V3 registration");
    core.set_v3_pool_quarantined(pool_addr);
    // Cutoff is BEHIND the seed clock (99 < 100) and the pump has NO event
    // for this pool at/below the clock → pump_count == 0. This is the
    // mod.rs:580 BENIGN seed case: the DB seed carries the live WS head
    // past the cutoff, so no event could be missing — the clock block is
    // preserved (NOT clamped), and verifying at 100 is correct.
    core.advance_pump_complete_cutoff(99);
    assert_eq!(core.v3_buffer.pump_count_at_or_below(&pool_addr, 100), 0);
    core.apply_pump_buffer_v3(&pool_addr);
    core.pin_v3_post_drain_snapshot(pool_addr);
    let (_ticks, pinned_block) = core
        .take_v3_post_drain_snapshot(pool_addr)
        .expect("pin captured for a Tracked pool");
    assert_eq!(
        pinned_block, 100,
        "benign seed (pump_count==0) must keep the clock block, not clamp"
    );
}
