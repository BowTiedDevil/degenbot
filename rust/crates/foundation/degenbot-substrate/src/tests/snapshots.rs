use super::*;
use crate::concentrated_liquidity_orchestration::PostDrainPin;
use crate::concentrated_liquidity_route::{ApplyOutcome, BufferKind, PinProvenance};
use degenbot_pools::PoolLiquidityUpdate;

#[test]
fn pool_update_block_tracks_forward_sync_and_returns_zero_for_unknown() {
    // The `pool_update_block` accessor is the per-pool freshness
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
    core.apply_sync_by_address(make_pool_addr(), U112::from(900), U112::from(2222), 7)
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
    core.apply_sync_by_address(make_pool_addr(), U112::from(900), U112::from(2222), 5)
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
    core.apply_sync_by_address(b_addr, U112::from(3300), U112::from(4400), 12)
        .expect("forward Sync at block 12 applies");
    assert_eq!(core.pool_update_block(a), 5);
    assert_eq!(core.pool_state_head(), 12, "head = max update_block");

    // De-registering the leading pool (B, at 12) drops the head back to A's 5.
    let _ = a;
    core.unregister_pool(PoolLookup::from_address(b_addr));
    assert_eq!(
        core.pool_state_head(),
        5,
        "head falls back to the next-highest update_block"
    );
}

#[test]
fn pool_tick_data_block_exposes_staged_liquidity_clock() {
    // Two-stamp rule / the `0x5653` staged-clock class: a concentrated liquidity pool whose
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
    // from a bug / non-concentrated-liquidity-advancing path).
    if let Some(crate::PoolEntry::V3(p)) = core.pools.get_mut(pool_id) {
        let state = &mut p.1;
        state.update_block = 200;
    }
    assert_eq!(core.pool_update_block(pool_id), 200, "price clock advanced");
    assert_eq!(
        core.pool_tick_data_block(pool_id),
        100,
        "liquidity clock still lags → the stagger is observable"
    );
    // Non-concentrated-liquidity families fall back to `update_block` for the total accessor.
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
    use crate::PoolTickCoverage;
    use crate::{RegisterV3PoolParams, TickInfo};
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
    assert_eq!(core.seed_genesis_by_pool_id(pool_id, db_block), Ok(pool_id));
    assert!(core.has_state_prior_to(pool_id, 100), "non-empty journal");
    // The anchor advances NO clock.
    assert_eq!(core.pool_update_block(pool_id), head);
    assert_eq!(core.pool_tick_data_block(pool_id), db_block);
    // Restore below the anchor pops the before==after delta → seeded state.
    let restored = core.restore_pool_before_block(pool_id, 100);
    assert!(restored.unwrap().is_ok());
    assert_eq!(core.pool_update_block(pool_id), head);
    assert_eq!(core.pool_tick_data_block(pool_id), db_block);
    assert!(matches!(
        core.seed_genesis_by_pool_id(999_999, 1),
        Err(crate::ConcentratedLiquidityApplyError::NotRegistered { pool_id: 999_999 })
    ));
}

/// the snapshot seed must be retained separately from the live
/// `tick_data` so step-1 verify can compare the *seed* against
/// on-chain@snapshot_block, NOT the pump-mutated current. During a rolling
/// start (`resume()` precedes `build_paths`) the pump applies Mint/Burn to
/// `tick_data`; without a pinned seed, step-1 reads engine-current (seed +
/// journal) vs on-chain@snapshot (pre-journal) → false mismatch on every
/// active pool (logs/perm-V2-V3-V2.log). The seed is pinned at registration
/// and never mutated by `apply_liquidity_update`.
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
    PoolLookup::from_address(v3_addr).mark_registration_live(&mut core);

    // The seed is pinned at registration for Tracked (snapshot) pools.
    assert_eq!(
        core.snapshot_seed(PoolLookup::from_address(v3_addr))
            .cloned(),
        Some(seed_clone.clone()),
        "Tracked pool must pin its snapshot seed at registration"
    );

    // Pump applies a Mint at block 1, mutating tick -60's gross.
    applied_directly(core.apply_liquidity_update(
        PoolLookup::from_address(v3_addr),
        PoolLiquidityUpdate::new(-60, 60, 500_i128, 1),
    ));

    // Live tick_data CHANGED (journal applied) ...
    let live_gross = core
        .pool_slice::<degenbot_pools::V3PoolState>(core.pool_id_by_address(&v3_addr).unwrap())
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
        core.snapshot_seed(PoolLookup::from_address(v3_addr))
            .cloned(),
        Some(seed_clone.clone()),
        "snapshot seed must be immutable across pump Mint/Burn (the rolling-start race fix)"
    );

    // `take_v3_snapshot_seed` returns the seed once and clears the slot
    // (memory is freed after step-1 verify; the seed is never needed again).
    let taken = PoolLookup::from_address(v3_addr).take_seed(&mut core);
    assert_eq!(taken, Some(seed_clone), "take returns the pinned seed");
    assert_eq!(
        core.snapshot_seed(PoolLookup::from_address(v3_addr)),
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
    PoolLookup::from_address(v3_addr).mark_registration_live(&mut core);

    // Pin the post-drain state atomically with the drain (no buffer here →
    // pin == current tick_data == the seed at registration). This is what
    // `apply_buffer_v3` does inside its single core.write() hold.
    PoolLookup::from_address(v3_addr).pin_post_drain(&mut core);

    // A pump Mint lands AFTER the drain — mutating the live tick_data.
    applied_directly(core.apply_liquidity_update(
        PoolLookup::from_address(v3_addr),
        PoolLiquidityUpdate::new(-60, 60, 500_i128, 1),
    ));

    // Live tick_data CHANGED (journal applied) ...
    let live_gross = core
        .pool_slice::<degenbot_pools::V3PoolState>(core.pool_id_by_address(&v3_addr).unwrap())
        .and_then(|s| s.tick_data.get(&-60))
        .map(|t| t.liquidity_gross.to::<u128>());
    assert_ne!(
        live_gross,
        Some(liq),
        "pump Mint must mutate the live tick_data (precondition)"
    );

    // ... but the pinned post-drain snapshot is UNCHANGED — step-2 verifies
    // the drain-time state, not the pump-corrupted current.
    let taken = PoolLookup::from_address(v3_addr).take_post_drain(&mut core);
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
        PoolLookup::from_address(v3_addr).take_post_drain(&mut core),
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
    PoolLookup::from_address(v3_addr).pin_post_drain(&mut core);
    assert_eq!(
        PoolLookup::from_address(v3_addr).take_post_drain(&mut core),
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
    // pump buffer via `apply_liquidity_update`).
    core.buffer_backfill_liquidity_update(
        PoolLookup::from_address(v3_addr),
        PoolLiquidityUpdate::new(-60, 60, 500_i128, snapshot_block + 1),
    );
    assert_eq!(
        core.apply_liquidity_update(
            PoolLookup::from_address(v3_addr),
            PoolLiquidityUpdate::new(-60, 60, 750_i128, pump_block),
        ),
        Ok(ApplyOutcome::Buffered(BufferKind::Pump)),
        "the door stages the pre-registration Mint into the pump buffer"
    );

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
    PoolLookup::from_address(*v3_addr).backfill(&mut core);
    // the gated pump drain only yields fully-completed blocks.
    // Mirror the live pump's ADR-008 D1 tombstone (first log of
    // `pump_block`+1 closes `pump_block`) so the drain takes the pump
    // Mint at `pump_block` rather than leaving it behind the gate.
    core.concentrated_liquidity_mut()
        .advance_pump_complete_cutoff(pump_block);
    PoolLookup::from_address(*v3_addr).pump(&mut core);
    PoolLookup::from_address(v3_addr).pin_post_drain(&mut core);

    // The pin must carry the pump event's block — the block the drained
    // `tick_data` actually matches on-chain at. Pre-fix the pin carried no
    // block at all and the verify used `backfill_block` → false mismatch.
    let taken = PoolLookup::from_address(v3_addr).take_post_drain(&mut core);
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
        PoolLookup::from_address(v3_addr).take_post_drain(&mut core),
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
    use crate::{RegisterV4PoolParams, V4PoolKey};
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
        state_view: Address::ZERO,
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
        known_bitmap_words: Vec::new(),
    })
    .expect("V4 pool registers");
    // Tracked pools register `Quarantined`; transition to `Live`
    // (the driver's post-verify `set_live`) so the pump update below
    // direct-applies as this test models.
    PoolLookup::V4 {
        pool_manager,
        pool_id: pool_id_bytes,
    }
    .mark_registration_live(&mut core);

    assert_eq!(
        core.snapshot_seed(PoolLookup::V4 {
            pool_manager,
            pool_id: pool_id_bytes
        })
        .cloned(),
        Some(seed_clone.clone()),
        "Tracked V4 pool must pin its snapshot seed at registration"
    );

    // Pump applies a ModifyLiquidity at block 1, mutating tick -60's gross.
    applied_directly(core.apply_liquidity_update(
        PoolLookup::V4 {
            pool_manager,
            pool_id: pool_id_bytes,
        },
        PoolLiquidityUpdate::from_v4_wire(-60, 60, I256::try_from(500_i128).unwrap(), 1).unwrap(),
    ));

    // Live tick_data CHANGED (journal applied) ...
    let live_gross = {
        let pid = core
            .pool_id_by_key(pool_manager, &pool_id_bytes)
            .expect("registered");
        core.pool_slice::<degenbot_pools::V4PoolState>(pid)
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
        core.snapshot_seed(PoolLookup::V4 {
            pool_manager,
            pool_id: pool_id_bytes
        })
        .cloned(),
        Some(seed_clone.clone()),
        "V4 snapshot seed must be immutable across pump ModifyLiquidity (rolling-start race fix)"
    );

    // take: returns the seed exactly once then clears.
    let taken = PoolLookup::V4 {
        pool_manager,
        pool_id: pool_id_bytes,
    }
    .take_seed(&mut core);
    assert_eq!(taken, Some(seed_clone), "take returns the pinned seed");
    assert_eq!(
        core.snapshot_seed(PoolLookup::V4 {
            pool_manager,
            pool_id: pool_id_bytes
        }),
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
    use crate::{RegisterV4PoolParams, V4PoolKey};
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
        state_view: Address::ZERO,
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
        known_bitmap_words: Vec::new(),
    })
    .expect("V4 pool registers");
    // Tracked pools register `Quarantined`; transition to `Live` so
    // the pump ModifyLiquidity below direct-applies (this test's model).
    PoolLookup::V4 {
        pool_manager,
        pool_id: pool_id_bytes,
    }
    .mark_registration_live(&mut core);

    // Pin post-drain state atomically with the drain (what apply_buffer_v4
    // does inside its single core.write() hold).
    PoolLookup::V4 {
        pool_manager,
        pool_id: pool_id_bytes,
    }
    .pin_post_drain(&mut core);

    // Pump ModifyLiquidity lands AFTER the drain.
    applied_directly(core.apply_liquidity_update(
        PoolLookup::V4 {
            pool_manager,
            pool_id: pool_id_bytes,
        },
        PoolLiquidityUpdate::from_v4_wire(-60, 60, I256::try_from(500_i128).unwrap(), 1).unwrap(),
    ));

    // Live tick_data CHANGED ...
    let live_gross = {
        let pid = core
            .pool_id_by_key(pool_manager, &pool_id_bytes)
            .expect("registered");
        core.pool_slice::<degenbot_pools::V4PoolState>(pid)
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
    let taken = PoolLookup::V4 {
        pool_manager,
        pool_id: pool_id_bytes,
    }
    .take_post_drain(&mut core);
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
        PoolLookup::V4 {
            pool_manager,
            pool_id: pool_id_bytes
        }
        .take_post_drain(&mut core),
        None,
        "take clears the V4 post-drain slot (verified exactly once)"
    );
}

/// `Sparse` V4 pools have no complete `tick_data` to pin — post-drain pin
/// stays `None` (step-2 verify is a no-op, same as the V4 seed for sparse).
#[test]
fn v4_post_drain_snapshot_is_none_for_sparse_pools() {
    use crate::{RegisterV4PoolParams, V4PoolKey};
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
        state_view: Address::ZERO,
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
        known_bitmap_words: Vec::new(),
    })
    .expect("V4 sparse pool registers");
    PoolLookup::V4 {
        pool_manager,
        pool_id: pool_id_bytes,
    }
    .pin_post_drain(&mut core);
    assert_eq!(
        PoolLookup::V4 {
            pool_manager,
            pool_id: pool_id_bytes
        }
        .take_post_drain(&mut core),
        None,
        "sparse V4 pools must not pin post-drain"
    );
}

// ── verify-dbg visibility probes (verify_dbg) ───────────────────
//
// Asserts the WIRING the probes rely on: a tracked V3 pool's pump
// Mint/Burn is counted by `v3_buffer.pump_count_at_or_below` through the
// `BotState` field, `advance_pump_complete_cutoff` advances the shared
// pump-completeness cutoff (the StageMachine tombstone), and
// `pin_v3_post_drain_snapshot` +
// `set_v3_pool_live` remain behavior-preserving under the buffered
// tail (the apply path executes regardless of the gate — the
// diagnostic branch is pure logging).
#[test]
fn verify_dbg_mark_complete_and_pin_are_behavior_preserving() {
    use crate::PoolTickCoverage;
    use crate::RegisterV3PoolParams;
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
    PoolLookup::from_address(pool_addr).begin_registration_quarantine(&mut core);
    // Pump-buffer a Mint + a same-block Burn for block 100.
    assert_eq!(
        core.apply_liquidity_update(
            PoolLookup::from_address(pool_addr),
            PoolLiquidityUpdate::new(-10, 10, 500_i128, 100),
        ),
        Ok(ApplyOutcome::Buffered(BufferKind::Pump)),
        "a quarantined pool's Mint is staged, not dropped"
    );
    assert_eq!(
        core.apply_liquidity_update(
            PoolLookup::from_address(pool_addr),
            PoolLiquidityUpdate::new(-10, 10, -500_i128, 100),
        ),
        Ok(ApplyOutcome::Buffered(BufferKind::Pump)),
        "a quarantined pool's Burn is staged, not dropped"
    );
    // Pre-mark: two pump events, no complete block yet.
    assert_eq!(
        core.concentrated_liquidity
            .v3_buffer
            .pump_count_at_or_below(&pool_addr, 100),
        2
    );
    assert_eq!(core.concentrated_liquidity().pump_complete_cutoff(), 0);
    assert_eq!(
        core.concentrated_liquidity
            .v3_buffer
            .pump_total_at_or_below(100),
        2
    );
    // Mark block 100 complete (what the pump does at N+1's tombstone).
    core.concentrated_liquidity_mut()
        .advance_pump_complete_cutoff(100);
    assert_eq!(core.concentrated_liquidity().pump_complete_cutoff(), 100);
    // Drain + pin: the gated drain yields both events, then the pin
    // captures the post-drain pair. (apply_pump_buffer_v3 + pin are the
    // exact sequence the registration seam runs.)
    PoolLookup::from_address(*pool_addr).pump(&mut core);
    PoolLookup::from_address(pool_addr).pin_post_drain(&mut core);
    let (pinned_ticks, pinned_block) = PoolLookup::from_address(pool_addr)
        .take_post_drain(&mut core)
        .expect("pin was captured for a Tracked pool");
    assert_eq!(pinned_block, 100, "pin captured the drained update_block");
    // The net-zero Mint+Burn leaves gross=0 on the boundary ticks and
    // creates NO initialized tick (a zero gross is pruned) → the pin carries
    // an empty map. The point: the pin pair is self-consistent with the
    // drain the probe correlates.
    assert_eq!(pinned_ticks.len(), 0, "net-zero Mint+Burn yields no tick");
    // set_live under an empty retained tail is a no-op flush (the probe
    // would log drained_retained_tail=0).
    PoolLookup::from_address(pool_addr).mark_registration_live(&mut core);
}

// ── post-drain pin: one atomic (map, clock) claim ───────────────
//
// The pin freezes the drained tick map WITH the block that map reflects, and
// that pair is a completeness claim: step-2 verify compares it against
// on-chain state at the carried block. The claim holds only when the drain
// released every buffered pump event at or below the map's own liquidity
// clock. When one is held back (an in-progress block the tombstone cutoff has
// not closed), the map omits a KNOWN event at/below its own stamp, so no
// block exists the pair matches: the clock is contradicted by the drain's
// held-back tail, and the tombstone cutoff is a stamp no snapshot was ever
// taken at. The pin refuses, stores nothing, and the consumer rebuilds after
// the next drain.

/// The V4 twin of [`register_v3_tracked_at`].
fn register_v4_tracked_at(core: &mut BotState, clock: u64) -> PoolLookup {
    let pool_manager = Address::from([0x44u8; 20]);
    let v4_pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId = [0xeeu8; 32];
    core.register_v4_pool(&RegisterV4PoolParams {
        pool_manager,
        pool_id: v4_pool_id,
        pool_key: V4PoolKey {
            currency0: Address::ZERO,
            currency1: Address::from([1u8; 20]),
            fee: 10_000,
            tick_spacing: 60,
            hooks: Address::ZERO,
        },
        state_view: Address::ZERO,
        hook_flags: 0,
        protocol_fee: 0,
        sqrt_price_x96: U256::from(1u128) << 96,
        liquidity: 0,
        tick: 0,
        tick_data: HashMap::new(),
        update_block: clock,
        tick_data_block: Some(clock),
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
        known_bitmap_words: Vec::new(),
    })
    .expect("test setup: V4 tracked registration");
    PoolLookup::V4 {
        pool_manager,
        pool_id: v4_pool_id,
    }
}

/// The pool's registration lifecycle, resolved through its key — the observable
/// a deferral changes.
fn lifecycle_of(core: &BotState, key: PoolLookup) -> RegistrationLifecycle {
    let pool_id = key.resolve(core).expect("registered pool");
    match core.pools.get(pool_id).expect("pool entry") {
        crate::PoolEntry::V3(p) => p.1.registration_lifecycle,
        crate::PoolEntry::V4(p) => p.1.registration_lifecycle,
        _ => panic!("only a concentrated-liquidity pool has a registration lifecycle"),
    }
}

/// Quarantine `key`, stage one liquidity event, tombstone block 99 only, and
/// drain: the event stays buffered because the drain's horizon stops at the
/// tombstone. A pool whose map is stamped at 100 lands in the shape the old
/// clamp papered over.
fn quarantine_buffer_and_drain_behind_the_clock(
    core: &mut BotState,
    key: PoolLookup,
    update: PoolLiquidityUpdate,
) {
    key.begin_registration_quarantine(core);
    assert_eq!(
        core.apply_liquidity_update(key, update),
        Ok(ApplyOutcome::Buffered(BufferKind::Pump)),
        "a quarantined pool's liquidity event is staged, not dropped"
    );
    core.concentrated_liquidity_mut()
        .advance_pump_complete_cutoff(99);
    key.pump(core);
}

/// The prior mismatch shape: a seed map stamped at 100 with the pump holding a
/// Mint@100 the tombstone cutoff (99) has not released. Pre-fix the pin
/// lowered the stamp to 99 — a block the map was never captured at. The map
/// omits an event at its own clock, so no stamp is complete and the pin
/// refuses.
#[test]
fn pin_refuses_incomplete_when_pump_has_undrained_events_at_the_clock() {
    let mut core = BotState::new();
    let pool_addr = Address::from([0xf8u8; 20]);
    register_v3_tracked_at(&mut core, pool_addr, 100);
    let key = PoolLookup::from_address(pool_addr);
    quarantine_buffer_and_drain_behind_the_clock(
        &mut core,
        key,
        PoolLiquidityUpdate::new(-10, 10, 500_i128, 100),
    );
    assert_eq!(
        core.concentrated_liquidity()
            .buffered_v3_event_count(&pool_addr),
        1,
        "the drain's horizon stops at the tombstone (precondition)"
    );

    assert_eq!(
        key.pin_post_drain(&mut core),
        PostDrainPin::Incomplete {
            clock: 100,
            undrained: 1,
            cutoff: 99,
            verdict: PinProvenance::WitnessedBeyondCutoff,
        },
        "an event at the map's own clock is missing from it: no block is complete"
    );
    assert_eq!(
        key.take_post_drain(&mut core),
        None,
        "an incomplete drain stores no pair (step-2 has nothing to verify)"
    );
    assert_eq!(
        lifecycle_of(&core, key),
        RegistrationLifecycle::VerificationDeferred,
        "an incomplete pin parks the pool as still owing a verification"
    );
}

/// The consumer's rebuild path: the incomplete outcome is not terminal. The
/// tombstone for block 100 closes, the next drain applies the held-back Mint,
/// and the re-pin claims the (now complete) map at 100.
#[test]
fn pin_incomplete_is_rebuilt_by_the_next_drain() {
    let mut core = BotState::new();
    let pool_addr = Address::from([0xf9u8; 20]);
    register_v3_tracked_at(&mut core, pool_addr, 100);
    let key = PoolLookup::from_address(pool_addr);
    quarantine_buffer_and_drain_behind_the_clock(
        &mut core,
        key,
        PoolLiquidityUpdate::new(-10, 10, 500_i128, 100),
    );
    assert!(matches!(
        key.pin_post_drain(&mut core),
        PostDrainPin::Incomplete { .. }
    ));
    assert_eq!(
        lifecycle_of(&core, key),
        RegistrationLifecycle::VerificationDeferred,
        "the incomplete pin parks the pool (precondition for the retry)"
    );

    // The retry starts the way the registration lifecycle starts: re-quarantine,
    // which is the one transition a deferred pool takes back into the machine.
    key.begin_registration_quarantine(&mut core);
    assert_eq!(
        lifecycle_of(&core, key),
        RegistrationLifecycle::Quarantined,
        "begin_registration_quarantine moves a deferred pool back to Quarantined"
    );

    // The pump's next tombstone lands: block 100 is complete, so its event
    // drains into the map.
    core.concentrated_liquidity_mut()
        .advance_pump_complete_cutoff(100);
    key.pump(&mut core);
    assert_eq!(
        core.concentrated_liquidity()
            .buffered_v3_event_count(&pool_addr),
        0,
        "the completed block's event drained (precondition)"
    );

    assert_eq!(
        key.pin_post_drain(&mut core),
        PostDrainPin::Pinned {
            clock: 100,
            tick_count: 2,
            verdict: PinProvenance::CorroboratedByDelivery,
        },
        "the rebuilt map covers its clock, and the tombstone now corroborates it"
    );
    let (tick_data, pinned_block) = key
        .take_post_drain(&mut core)
        .expect("the rebuilt pin is stored for step-2");
    assert_eq!(pinned_block, 100, "the pin carries the map's own clock");
    assert_eq!(
        tick_data.get(&-10).map(|t| t.liquidity_gross.to::<u128>()),
        Some(500),
        "the rebuilt pin carries the drained Mint"
    );
    assert_eq!(
        lifecycle_of(&core, key),
        RegistrationLifecycle::Quarantined,
        "a completed pin returns the pool to Quarantined; only step-2 promotes it to Live"
    );
}

/// A pool with no tombstone at all (`cutoff == 0`) proves nothing about a
/// seeded map's clock, so a held-back event still refuses the pin. Pre-fix the
/// clamp's `cutoff > 0` guard skipped the clamp here and stamped the map at
/// its clock — the same contradiction, unwarned.
#[test]
fn pin_refuses_incomplete_with_no_tombstone_yet() {
    let mut core = BotState::new();
    let pool_addr = Address::from([0xfbu8; 20]);
    register_v3_tracked_at(&mut core, pool_addr, 100);
    let key = PoolLookup::from_address(pool_addr);
    key.begin_registration_quarantine(&mut core);
    assert_eq!(
        core.apply_liquidity_update(key, PoolLiquidityUpdate::new(-10, 10, 500_i128, 100)),
        Ok(ApplyOutcome::Buffered(BufferKind::Pump)),
        "a quarantined pool's Mint is staged, not dropped"
    );
    assert_eq!(
        core.concentrated_liquidity().pump_complete_cutoff(),
        0,
        "no tombstone yet (precondition)"
    );
    key.pump(&mut core);

    assert_eq!(
        key.pin_post_drain(&mut core),
        PostDrainPin::Incomplete {
            clock: 100,
            undrained: 1,
            cutoff: 0,
            verdict: PinProvenance::WitnessedBeyondCutoff,
        },
        "a missing delivery horizon cannot certify a map the drain contradicts"
    );
    assert_eq!(key.take_post_drain(&mut core), None);
    assert_eq!(
        lifecycle_of(&core, key),
        RegistrationLifecycle::VerificationDeferred,
        "a refusal always parks the deferral state, with no tombstone or not"
    );
}

/// An EMPTY map is a complete map when nothing is undrained, and its stamp
/// stays its own clock: the delivery horizon is corroboration, never a
/// promotion target.
#[test]
fn pin_stamps_an_empty_map_at_its_own_clock() {
    let mut core = BotState::new();
    let pool_addr = Address::from([0xfcu8; 20]);
    register_v3_tracked_at(&mut core, pool_addr, 100);
    core.concentrated_liquidity_mut()
        .advance_pump_complete_cutoff(150);
    let key = PoolLookup::from_address(pool_addr);

    assert_eq!(
        key.pin_post_drain(&mut core),
        PostDrainPin::Pinned {
            clock: 100,
            tick_count: 0,
            verdict: PinProvenance::CorroboratedByDelivery,
        }
    );
    let (tick_data, pinned_block) = key
        .take_post_drain(&mut core)
        .expect("an empty map with nothing undrained still pins");
    assert!(tick_data.is_empty(), "the pin mirrors the drained map");
    assert_eq!(pinned_block, 100, "the stamp is the map's clock, not 150");
}

/// Only a `Tracked` pool carries a complete map, so only it can be pinned.
#[test]
fn pin_reports_not_pinnable_for_sparse_and_unregistered_pools() {
    let mut core = BotState::new();
    let _ = register_v3(&mut core, 0);
    assert_eq!(
        PoolLookup::from_address(make_pool_addr()).pin_post_drain(&mut core),
        PostDrainPin::NotPinnable,
        "a sparse pool has no complete map to pin"
    );
    assert_eq!(
        PoolLookup::from_address(Address::from([0x99u8; 20])).pin_post_drain(&mut core),
        PostDrainPin::NotPinnable,
        "an unregistered pool has no map at all"
    );
}

/// A stale pair is dropped, never left behind: a complete pin followed by a
/// drain that holds an event back must not leave the earlier pair for step-2
/// to verify as if it were this drain's state.
#[test]
fn incomplete_pin_drops_a_stale_pair() {
    let mut core = BotState::new();
    let pool_addr = Address::from([0xfeu8; 20]);
    register_v3_tracked_at(&mut core, pool_addr, 100);
    let key = PoolLookup::from_address(pool_addr);
    key.begin_registration_quarantine(&mut core);
    assert!(matches!(
        key.pin_post_drain(&mut core),
        PostDrainPin::Pinned { .. }
    ));

    assert_eq!(
        core.apply_liquidity_update(key, PoolLiquidityUpdate::new(-10, 10, 500_i128, 100)),
        Ok(ApplyOutcome::Buffered(BufferKind::Pump)),
        "a quarantined pool's Mint is staged, not dropped"
    );
    core.concentrated_liquidity_mut()
        .advance_pump_complete_cutoff(99);
    key.pump(&mut core);

    assert!(matches!(
        key.pin_post_drain(&mut core),
        PostDrainPin::Incomplete { .. }
    ));
    assert_eq!(
        key.take_post_drain(&mut core),
        None,
        "the stale pair from the earlier drain is dropped, not verified"
    );
}

/// The benign counterpart: a seed stamped BEYOND the tombstone with nothing
/// undrained keeps its own clock. Nothing the drain knows contradicts the
/// map, so the pin is minted; the seed-trust provenance is a diagnostic
/// (`SeedTrustOnly { witnessed_horizon: 0 }`), not a veto.
#[test]
fn pin_preserves_clock_block_when_no_undrained_events() {
    let mut core = BotState::new();
    let pool_addr = Address::from([0xf3u8; 20]);
    register_v3_tracked_at(&mut core, pool_addr, 100);
    let key = PoolLookup::from_address(pool_addr);
    key.begin_registration_quarantine(&mut core);
    core.concentrated_liquidity_mut()
        .advance_pump_complete_cutoff(99);
    key.pump(&mut core);

    assert_eq!(
        key.pin_post_drain(&mut core),
        PostDrainPin::Pinned {
            clock: 100,
            tick_count: 0,
            verdict: PinProvenance::SeedTrustOnly {
                witnessed_horizon: 0
            },
        }
    );
    let (_ticks, pinned_block) = key
        .take_post_drain(&mut core)
        .expect("pin captured for a Tracked pool");
    assert_eq!(
        pinned_block, 100,
        "an undrained-free drain keeps the map's own clock"
    );
}

/// Re-seed after activity: the engine witnessed this pool's Mint@90 (still
/// undrained) and a DB row then re-seeded the map at 105. The seed-trust
/// verdict stays visible for diagnostics, but it is NOT completeness: the pin
/// refuses rather than trusting the fresher stamp.
#[test]
fn pin_refuses_incomplete_after_a_re_seed_over_witnessed_activity() {
    let mut core = BotState::new();
    let pool_addr = Address::from([0xfdu8; 20]);
    register_v3_tracked_at(&mut core, pool_addr, 90);
    let key = PoolLookup::from_address(pool_addr);
    key.begin_registration_quarantine(&mut core);
    assert_eq!(
        core.apply_liquidity_update(key, PoolLiquidityUpdate::new(-10, 10, 500_i128, 90)),
        Ok(ApplyOutcome::Buffered(BufferKind::Pump)),
        "the engine witnesses this pool's Mint@90 and stages it"
    );
    // The tombstone closes block 85 only, so the drain holds the Mint back.
    core.concentrated_liquidity_mut()
        .advance_pump_complete_cutoff(85);
    key.pump(&mut core);

    // The re-seed: one wholesale sync moves both clocks to 105, past anything
    // the engine witnessed for this pool.
    core.sync_pool_state(
        key,
        PoolStateUpdate::V3(V3StateSync {
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 0,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 105,
        }),
    )
    .expect("the re-seed applies to a registered pool");
    key.pump(&mut core);

    assert_eq!(
        key.pin_post_drain(&mut core),
        PostDrainPin::Incomplete {
            clock: 105,
            undrained: 1,
            cutoff: 85,
            verdict: PinProvenance::SeedTrustOnly {
                witnessed_horizon: 90
            },
        },
        "a fresher seed stamp cannot certify a map the witnessed tail contradicts"
    );
    assert_eq!(key.take_post_drain(&mut core), None);
}

/// The V4 twin of [`pin_refuses_incomplete_when_pump_has_undrained_events_at_the_clock`]:
/// one rule for both designs.
#[test]
fn v4_pin_refuses_incomplete_when_pump_has_undrained_events_at_the_clock() {
    let mut core = BotState::new();
    let key = register_v4_tracked_at(&mut core, 100);
    quarantine_buffer_and_drain_behind_the_clock(
        &mut core,
        key,
        PoolLiquidityUpdate::from_v4_wire(
            -60,
            60,
            I256::try_from(500_i128).expect("500 fits i128"),
            100,
        )
        .expect("valid V4 ModifyLiquidity wire update"),
    );

    assert_eq!(
        key.pin_post_drain(&mut core),
        PostDrainPin::Incomplete {
            clock: 100,
            undrained: 1,
            cutoff: 99,
            verdict: PinProvenance::WitnessedBeyondCutoff,
        }
    );
    assert_eq!(key.take_post_drain(&mut core), None);
    assert_eq!(
        lifecycle_of(&core, key),
        RegistrationLifecycle::VerificationDeferred,
        "the V4 refusal defers exactly like the V3 one"
    );
}

/// The V4 twin of [`pin_incomplete_is_rebuilt_by_the_next_drain`].
#[test]
fn v4_pin_incomplete_is_rebuilt_by_the_next_drain() {
    let mut core = BotState::new();
    let key = register_v4_tracked_at(&mut core, 100);
    quarantine_buffer_and_drain_behind_the_clock(
        &mut core,
        key,
        PoolLiquidityUpdate::from_v4_wire(
            -60,
            60,
            I256::try_from(500_i128).expect("500 fits i128"),
            100,
        )
        .expect("valid V4 ModifyLiquidity wire update"),
    );
    assert!(matches!(
        key.pin_post_drain(&mut core),
        PostDrainPin::Incomplete { .. }
    ));
    assert_eq!(
        lifecycle_of(&core, key),
        RegistrationLifecycle::VerificationDeferred
    );
    key.begin_registration_quarantine(&mut core);

    core.concentrated_liquidity_mut()
        .advance_pump_complete_cutoff(100);
    key.pump(&mut core);

    assert_eq!(
        key.pin_post_drain(&mut core),
        PostDrainPin::Pinned {
            clock: 100,
            tick_count: 2,
            verdict: PinProvenance::CorroboratedByDelivery,
        }
    );
    let (tick_data, pinned_block) = key
        .take_post_drain(&mut core)
        .expect("the rebuilt V4 pin is stored for step-2");
    assert_eq!(pinned_block, 100);
    assert_eq!(
        tick_data.get(&-60).map(|t| t.liquidity_gross.to::<u128>()),
        Some(500)
    );
    assert_eq!(
        lifecycle_of(&core, key),
        RegistrationLifecycle::Quarantined,
        "the completed V4 retry re-quarantined the pool; step-2 owns the promotion"
    );
}
