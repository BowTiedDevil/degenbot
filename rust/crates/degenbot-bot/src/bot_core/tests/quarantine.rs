use super::*;

/// ADR-040 / PJGMPK: the quarantine seam is idempotent, counts depth,
/// and bumps the pool's `state_nonce` (dirties the pool) on BOTH transitions
/// so cached projections and in-flight solver snapshots invalidate.
#[test]
fn quarantine_pool_seam_is_idempotent_and_dirties_nonce() {
    let mut core = BotState::new();
    let pool_id = core
        .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
        .expect("test setup: V2 registration");

    assert!(
        !core.quarantine_pool(999_999),
        "an unknown pool_id cannot quarantine"
    );
    assert!(!core.is_pool_quarantined(999_999));

    let nonce_before = core.pool_state_nonce(pool_id);
    assert!(core.quarantine_pool(pool_id));
    assert!(core.is_pool_quarantined(pool_id));
    assert_eq!(core.quarantined_pool_count(), 1);

    assert!(!core.quarantine_pool(pool_id), "re-quarantine is a no-op");
    assert_eq!(core.quarantined_pool_count(), 1);
    assert_ne!(
        core.pool_state_nonce(pool_id),
        nonce_before,
        "quarantine dirties the nonce"
    );

    assert!(core.release_pool(pool_id));
    assert!(!core.is_pool_quarantined(pool_id));
    assert_eq!(core.quarantined_pool_count(), 0);
    assert_ne!(
        core.pool_state_nonce(pool_id),
        nonce_before,
        "release also dirties the nonce (stale Invalid cache entries cannot stick)"
    );
    assert!(!core.release_pool(pool_id), "double release is a no-op");
}

// ── 6N7XVR: pool-registration lifecycle FSM (Quarantined→Live) ────────
//
// The rolling-start race the YLYJM2 `drain_pump_completed` buffer gate
// does NOT cover: a registered pool's LIVE direct-apply path advances
// `update_block` past `last_complete_block` during the drain+pin+verify
// window, so the pin captures `(tick_data_without_burn, block_N)` while a
// same-block Burn stays retained in the pump buffer → mismatch by exactly
// the Burn's delta (block 25647112 reproduction). The Quarantined lifecycle
// defers ALL live events (swap + liquidity) to the buffer until
// drain+pin+verify completes; `set_pool_live` flushes the retained tail.
//
// These tests cover the CORE lifecycle + deferral invariants; the
// positional 25647112 reproduction + concurrent-registration stress live
// in the wiring/seam task  and the robust-suite task .
/// Register a V4 pool on `core` with a single tick at 60 (gross/net 100)
/// and `update_block`, returning its `pool_id`. Test helper. `pool_id`
/// distinguishes concurrent registrations (default `[0xee;32]`).
fn register_v4_on_core(core: &mut BotState, update_block: u64) -> u64 {
    register_v4_on_core_with_pid(core, [0xeeu8; 32], update_block)
}

/// `register_v4_on_core` with an explicit V4 `pool_id` (concurrent-
/// registration tests need distinct keys).
fn register_v4_on_core_with_pid(
    core: &mut BotState,
    pool_id_bytes: [u8; 32],
    update_block: u64,
) -> u64 {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::{RegisterV4PoolParams, TickInfo, V4PoolKey};
    use alloy::primitives::U128;
    let mut tick_data = HashMap::new();
    tick_data.insert(
        60,
        TickInfo {
            liquidity_gross: U128::from(100),
            liquidity_net: 100i128,
            block: 0,
        },
    );
    let pool_manager = Address::from([0x44u8; 20]);
    core.register_v4_pool(&RegisterV4PoolParams {
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
        tick_data,
        update_block,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
    })
    .expect("test setup: V4 registration")
}

/// A freshly-registered CL pool's lifecycle is COVERAGE-AWARE (DFQYM5):
/// `Tracked` (complete liquidity map, pins + step-2 verifies) defaults to
/// `Quarantined` so no live event direct-applies before the two-step
/// verify; `Sparse` (no complete map → no pin / step-2 verify) stays
/// `Live`/direct-apply. True for both V3 and V4.
#[test]
fn fresh_pool_lifecycle_is_coverage_aware() {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::{RegisterV3PoolParams, RegisterV4PoolParams, TickInfo, V4PoolKey};
    use alloy::primitives::U128;
    let mut core = BotState::new();

    // Tracked V4 → Quarantined (register_v4_on_core uses Tracked).
    let tracked_v4 = register_v4_on_core(&mut core, 0);
    assert_eq!(
        core.get_v4_pool(tracked_v4).unwrap().registration_lifecycle,
        RegistrationLifecycle::Quarantined
    );

    // Sparse V3 → Live (register_v3_on_core uses Sparse).
    let sparse_v3 = register_v3_on_core(&mut core, Address::from([0x88u8; 20]), 0);
    assert_eq!(
        core.get_v3_pool(sparse_v3).unwrap().registration_lifecycle,
        RegistrationLifecycle::Live
    );

    // Sparse V4 → Live: trim the Tracked helper's params to Sparse, then
    // release the Tracked pool to free its key space isn't needed — use a
    // fresh pool_id.
    let mut tick_data = HashMap::new();
    tick_data.insert(
        60,
        TickInfo {
            liquidity_gross: U128::from(100),
            liquidity_net: 100i128,
            block: 0,
        },
    );
    let pm = Address::from([0x44u8; 20]);
    let pid = [0xabu8; 32];
    let sparse_v4 = core
        .register_v4_pool(&RegisterV4PoolParams {
            pool_manager: pm,
            pool_id: pid,
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
            tick_data,
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
        })
        .expect("sparse V4 registration");
    assert_eq!(
        core.get_v4_pool(sparse_v4).unwrap().registration_lifecycle,
        RegistrationLifecycle::Live
    );

    // Tracked V3 → Quarantined: reuse the V3 Sparse helper's shape but
    // override coverage to Tracked.
    let mut tick_data = HashMap::new();
    tick_data.insert(
        60,
        TickInfo {
            liquidity_gross: U128::from(100),
            liquidity_net: 100i128,
            block: 0,
        },
    );
    let tracked_v3 = core
        .register_v3_pool(&RegisterV3PoolParams {
            address: Address::from([0x99u8; 20]),
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            ..Default::default()
        })
        .expect("tracked V3 registration");
    assert_eq!(
        core.get_v3_pool(tracked_v3).unwrap().registration_lifecycle,
        RegistrationLifecycle::Quarantined
    );
}

/// `set_v3/v4_pool_quarantined` is a no-op for non-`Tracked` pools: a
/// `Sparse` pool has no pin / step-2 verify to protect, so it must stay
/// `Live`/direct-apply (DFQYM5 carve-out). The driver calls `set_*_quarantined`
/// for every registered pool, so this guard is what keeps Sparse out of the
/// `quarantine→buffer→set_live` round trip.
#[test]
fn sparse_pool_ignores_set_quarantined() {
    let mut core = BotState::new();
    let pool_addr = Address::from([0x77u8; 20]);
    // register_v3_on_core uses Sparse coverage.
    register_v3_on_core(&mut core, pool_addr, 0);
    core.set_v3_pool_quarantined(pool_addr);
    assert_eq!(
        core.get_v3_pool(core.pool_id_by_address(&pool_addr).unwrap())
            .unwrap()
            .registration_lifecycle,
        RegistrationLifecycle::Live,
        "Sparse pool must ignore set_quarantined and stay Live"
    );
}

/// `release_all_v3_v4_quarantined` flushes + marks `Live` every pool still
/// `Quarantined` — the orphan sweep that stops a Tracked pool registered
/// but never reaching `set_live` (path skipped before registration) from
/// deferring events to its buffer indefinitely. Already-`Live` (Sparse) and
/// non-CL pools are untouched.
#[test]
fn release_all_quarantined_flushes_and_marks_live() {
    use alloy::primitives::U128;
    let mut core = BotState::new();
    // Two Tracked pools — both register Quarantined under DFQYM5.
    let tracked_v3 = register_v3_on_core(&mut core, Address::from([0x55u8; 20]), 0);
    // register_v3_on_core is Sparse — build a Tracked V3 explicitly.
    let mut tick_data = HashMap::new();
    tick_data.insert(
        60,
        TickInfo {
            liquidity_gross: U128::from(100),
            liquidity_net: 100i128,
            block: 0,
        },
    );
    let tracked_v3b = core
        .register_v3_pool(&crate::bot_core::RegisterV3PoolParams {
            address: Address::from([0x66u8; 20]),
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            ..Default::default()
        })
        .expect("tracked V3");
    let tracked_v4 = register_v4_on_core(&mut core, 0);
    let _ = tracked_v3; // Sparse → Live from the start, not in the sweep.
    assert_eq!(
        core.get_v3_pool(tracked_v3b)
            .unwrap()
            .registration_lifecycle,
        RegistrationLifecycle::Quarantined
    );
    assert_eq!(
        core.get_v4_pool(tracked_v4).unwrap().registration_lifecycle,
        RegistrationLifecycle::Quarantined
    );

    core.release_all_v3_v4_quarantined();

    assert_eq!(
        core.get_v3_pool(tracked_v3b)
            .unwrap()
            .registration_lifecycle,
        RegistrationLifecycle::Live
    );
    assert_eq!(
        core.get_v4_pool(tracked_v4).unwrap().registration_lifecycle,
        RegistrationLifecycle::Live
    );
    // The Sparse V3 (register_v3_on_core) was already Live; release
    // (which would be idempotent) is safe here too.
    assert_eq!(
        core.get_v3_pool(tracked_v3).unwrap().registration_lifecycle,
        RegistrationLifecycle::Live
    );
}

/// A `Quarantined` V4 pool's live `Swap` lands in the pump buffer (NOT
/// applied directly): `tick_data` and scalars are unchanged, but the
/// buffered event count increases and the buffer carries a `Swap` variant.
#[test]
fn quarantined_v4_pool_defers_live_swap_to_pump_buffer() {
    use alloy::primitives::U128;
    let _ = U128::from(0);
    let pool_manager = Address::from([0x44u8; 20]);
    let pool_id_bytes: [u8; 32] = [0xeeu8; 32];
    let mut core = BotState::new();
    let pool_id = register_v4_on_core(&mut core, 10);
    // Quarantine the pool.
    core.set_v4_pool_quarantined(pool_manager, pool_id_bytes);
    assert_eq!(
        core.get_v4_pool(pool_id).unwrap().registration_lifecycle,
        RegistrationLifecycle::Quarantined
    );
    // Snapshot the pre-swap state.
    let pre = core.get_v4_pool(pool_id).unwrap().clone();
    let pre_count = core.buffered_v4_event_count(&(pool_manager, pool_id_bytes));
    // Deliver a live Swap at block 11.
    core.apply_v4_swap(
        &V4SwapUpdate {
            pool_manager,
            pool_id: pool_id_bytes,
            sqrt_price_x96: U256::from(2u128) << 96,
            liquidity: 2_000_000,
            tick: 1,
            tick_priors: Box::default(),
        },
        11,
    );
    // The swap was deferred: scalars + update_block unchanged.
    let s = core.get_v4_pool(pool_id).unwrap();
    assert_eq!(
        s.update_block, pre.update_block,
        "update_block must NOT advance — swap deferred"
    );
    assert_eq!(
        s.sqrt_price_x96, pre.sqrt_price_x96,
        "sqrt_price_x96 unchanged — swap deferred"
    );
    assert_eq!(
        s.liquidity, pre.liquidity,
        "liquidity unchanged — swap deferred"
    );
    assert_eq!(s.tick, pre.tick, "tick unchanged — swap deferred");
    // The pump buffer gained one event.
    assert_eq!(
        core.buffered_v4_event_count(&(pool_manager, pool_id_bytes)),
        pre_count + 1,
        "live swap buffered, not applied"
    );
}

/// A `Quarantined` V4 pool's live `ModifyLiquidity` (Burn) lands in the
/// pump buffer: `tick_data` is unchanged (the Burn is NOT applied).
#[test]
fn quarantined_v4_pool_defers_live_modify_liquidity_to_pump_buffer() {
    use alloy::primitives::U128;
    let _ = U128::from(0);
    let pool_manager = Address::from([0x44u8; 20]);
    let pool_id_bytes: [u8; 32] = [0xeeu8; 32];
    let mut core = BotState::new();
    let pool_id = register_v4_on_core(&mut core, 10);
    core.set_v4_pool_quarantined(pool_manager, pool_id_bytes);
    let pre_t60 = core
        .get_v4_pool(pool_id)
        .unwrap()
        .tick_data
        .get(&60)
        .unwrap()
        .clone();
    let pre_update_block = core.get_v4_pool(pool_id).unwrap().update_block;
    let pre_count = core.buffered_v4_event_count(&(pool_manager, pool_id_bytes));
    // Deliver a live Burn (negative ModifyLiquidity) at block 11.
    core.apply_v4_liquidity_update(
        pool_manager,
        pool_id_bytes,
        60,
        120,
        I256::try_from(-500i128).unwrap(),
        11,
    );
    let s = core.get_v4_pool(pool_id).unwrap();
    assert_eq!(
        *s.tick_data.get(&60).unwrap(),
        pre_t60,
        "tick_data unchanged — Burn deferred"
    );
    assert_eq!(
        s.update_block, pre_update_block,
        "update_block must NOT advance — Burn deferred"
    );
    assert_eq!(
        core.buffered_v4_event_count(&(pool_manager, pool_id_bytes)),
        pre_count + 1,
        "live Burn buffered, not applied"
    );
}

/// The 6N7XVR invariant: while `Quarantined`, the pin's source
/// `update_block` CANNOT outrun `last_complete_block`. A live Swap at
/// block N+1 (in-progress, `last_complete_block == N`) is deferred, so
/// `update_block` stays at N — the gated drain then yields only complete-
/// block events, and the pin captures a self-consistent pair.
///
/// Pre-fix (RED): the live Swap applied directly, advancing `update_block`
/// to N+1 while a same-block buffered Burn stayed retained; `pin_v4_post_
/// drain_snapshot` captured `(tick_data_without_burn, N+1)` → mismatch.
#[test]
fn quarantined_pool_update_block_cannot_outrun_last_complete_block() {
    use alloy::primitives::U128;
    let pool_manager = Address::from([0x44u8; 20]);
    let pool_id_bytes: [u8; 32] = [0xeeu8; 32];
    let mut core = BotState::new();
    // Snapshot seed at S=10; register the pool.
    let _pool_id = register_v4_on_core(&mut core, 10);
    // Quarantine BEFORE any live event lands (the registration seam's job).
    core.set_v4_pool_quarantined(pool_manager, pool_id_bytes);
    // The pump has fully delivered block 10 (tombstone at 11).
    core.advance_pump_complete_cutoff(10);
    // A live Swap lands at block 11 (in-progress; `last_complete_block` is
    // still 10 — no tombstone for 11 yet).
    core.apply_v4_swap(
        &V4SwapUpdate {
            pool_manager,
            pool_id: pool_id_bytes,
            sqrt_price_x96: U256::from(2u128) << 96,
            liquidity: 2_000_000,
            tick: 1,
            tick_priors: Box::default(),
        },
        11,
    );
    // Drain the complete-block tail (block 10 has no events; block 11 is
    // retained by the gate).
    core.apply_pump_buffer_v4(pool_manager, pool_id_bytes);
    // Pin the post-drain pair.
    core.pin_v4_post_drain_snapshot(pool_manager, &pool_id_bytes);
    let (tick_data, pinned_block) = core
        .take_v4_post_drain_snapshot(pool_manager, &pool_id_bytes)
        .expect("a Tracked pool pins a post-drain pair");
    // The pin's `update_block` is 10 (the registration block) — the live
    // Swap at 11 was deferred and the gate retained it. `update_block` did
    // NOT advance to 11 (the in-progress block). This is the invariant
    // YLYJM2's buffer gate alone could NOT guarantee (the live path was
    // ungated).
    assert_eq!(
        pinned_block, 10,
        "pin's update_block cannot outrun last_complete_block"
    );
    // The pinned tick_data matches the registration seed (no live event was
    // applied). tick 60 unchanged.
    let t60 = tick_data.get(&60).expect("tick 60 present");
    assert_eq!(t60.liquidity_gross, U128::from(100));
}

/// `set_v4_pool_live` flushes the retained in-progress-block pump tail
/// (via the unguarded `drain_pump`) in insertion order, then marks `Live`.
/// After the transition, subsequent live events apply directly (the
/// steady-state contract). The flushed events land in arrival order (swap
/// after a buffered Burn if it arrived after).
#[test]
fn set_v4_pool_live_flushes_retained_tail_and_marks_live() {
    use alloy::primitives::U128;
    let pool_manager = Address::from([0x44u8; 20]);
    let pool_id_bytes: [u8; 32] = [0xeeu8; 32];
    let mut core = BotState::new();
    let pool_id = register_v4_on_core(&mut core, 10);
    core.set_v4_pool_quarantined(pool_manager, pool_id_bytes);
    core.advance_pump_complete_cutoff(10);
    // Buffer a Burn (block 11, in-progress) + a Swap (block 11) — both
    // retained by the gate during quarantine.
    core.apply_v4_liquidity_update(
        pool_manager,
        pool_id_bytes,
        60,
        120,
        I256::try_from(-50i128).unwrap(),
        11,
    );
    core.apply_v4_swap(
        &V4SwapUpdate {
            pool_manager,
            pool_id: pool_id_bytes,
            sqrt_price_x96: U256::from(2u128) << 96,
            liquidity: 2_000_000,
            tick: 1,
            tick_priors: Box::default(),
        },
        11,
    );
    assert_eq!(
        core.buffered_v4_event_count(&(pool_manager, pool_id_bytes)),
        2,
        "both events retained"
    );
    // Transition to Live: flush the retained tail.
    core.set_v4_pool_live(pool_manager, pool_id_bytes);
    let s = core.get_v4_pool(pool_id).unwrap();
    assert_eq!(s.registration_lifecycle, RegistrationLifecycle::Live);
    assert_eq!(s.update_block, 11, "flush applied both events at block 11");
    // tick 60: 100 (seed) - 50 (Burn) = 50.
    assert_eq!(
        s.tick_data.get(&60).unwrap().liquidity_gross,
        U128::from(50)
    );
    // scalars reflect the flushed swap.
    assert_eq!(s.sqrt_price_x96, U256::from(2u128) << 96);
    // The buffer is drained.
    assert_eq!(
        core.buffered_v4_event_count(&(pool_manager, pool_id_bytes)),
        0
    );
    // A subsequent live event applies directly (no buffering).
    core.apply_v4_liquidity_update(
        pool_manager,
        pool_id_bytes,
        60,
        120,
        I256::try_from(10i128).unwrap(),
        12,
    );
    assert_eq!(
        core.buffered_v4_event_count(&(pool_manager, pool_id_bytes)),
        0,
        "Live pool applies directly — no buffering"
    );
    let s = core.get_v4_pool(pool_id).unwrap();
    assert_eq!(
        s.tick_data.get(&60).unwrap().liquidity_gross,
        U128::from(60)
    );
}

/// A `Live` (un-quarantined) registered pool applies events directly — the
/// 6N7XVR change does NOT regress the steady-state live-apply path.
#[test]
fn live_pool_applies_modify_liquidity_directly() {
    use alloy::primitives::U128;
    let pool_manager = Address::from([0x44u8; 20]);
    let pool_id_bytes: [u8; 32] = [0xeeu8; 32];
    let mut core = BotState::new();
    let pool_id = register_v4_on_core(&mut core, 10);
    // A Tracked pool registers `Quarantined` under DFQYM5 — transition it
    // to `Live` (the driver's `set_v4_pool_live` is the sole path to the
    // steady-state direct-apply contract).
    core.set_v4_pool_live(pool_manager, pool_id_bytes);
    // Now Live — applies directly, never buffers.
    core.apply_v4_liquidity_update(
        pool_manager,
        pool_id_bytes,
        60,
        120,
        I256::try_from(-50i128).unwrap(),
        11,
    );
    assert_eq!(
        core.buffered_v4_event_count(&(pool_manager, pool_id_bytes)),
        0,
        "Live pool never buffers"
    );
    let s = core.get_v4_pool(pool_id).unwrap();
    // OB7UNY two-stamp: tick-map-only ModifyLiquidity → liquidity clock
    // advances; the price clock stays at the seed block 10.
    assert_eq!(s.tick_data_block, 11, "applied directly (liquidity clock)");
    assert_eq!(
        s.update_block, 10,
        "price clock untouched (out-of-range mint)"
    );
    assert_eq!(
        s.tick_data.get(&60).unwrap().liquidity_gross,
        U128::from(50)
    );
}

// ── 6N7XVR robust suite (BWUHVX) ──────────────────────────────────────
//
// The lifecycle invariant under concurrency, dual-buffer drains, the
// backfill-boundary regression, and the reorg-during-quarantine edge.
/// Dual-buffer drain correctness: a quarantined pool with events in BOTH
/// the backfill buffer (snapshot gap) and the pump buffer (live) drains
/// both in order during `apply_buffer_*`; the pin reflects backfill +
/// complete-block pump events; `set_pool_live` flushes only the retained
/// in-progress pump tail (backfill is always fully drained).
#[test]
fn quarantined_pool_dual_buffer_drain_correctness() {
    use alloy::primitives::U128;
    let pool_manager = Address::from([0x44u8; 20]);
    let pool_id_bytes: [u8; 32] = [0xeeu8; 32];
    let mut core = BotState::new();
    // Backfill-range event (block 8, in the snapshot gap S+1..W-1) for
    // an UNregistered pool → backfill buffer.
    core.buffer_backfill_v4_liquidity_update(
        pool_manager,
        pool_id_bytes,
        60,
        120,
        I256::try_from(30i128).unwrap(),
        8,
    );
    // Register the pool (snapshot seed at block 10, tick 60 gross 100).
    let pool_id = register_v4_on_core(&mut core, 10);
    core.set_v4_pool_quarantined(pool_manager, pool_id_bytes);
    // The pump has tombstoned block 10 (live events at 10 are complete).
    core.advance_pump_complete_cutoff(10);
    // A complete-block pump event (block 10) + an in-progress-block pump
    // event (block 11) — both deferred to the pump buffer.
    core.apply_v4_liquidity_update(
        pool_manager,
        pool_id_bytes,
        60,
        120,
        I256::try_from(20i128).unwrap(),
        10,
    );
    core.apply_v4_liquidity_update(
        pool_manager,
        pool_id_bytes,
        60,
        120,
        I256::try_from(-15i128).unwrap(),
        11,
    );
    // Drain both buffers (the registration `apply_buffer_v4` sequence).
    core.apply_backfill_buffer_v4(pool_manager, pool_id_bytes);
    core.apply_pump_buffer_v4(pool_manager, pool_id_bytes);
    // Pin the post-drain pair.
    core.pin_v4_post_drain_snapshot(pool_manager, &pool_id_bytes);
    let (tick_data, pinned_block) = core
        .take_v4_post_drain_snapshot(pool_manager, &pool_id_bytes)
        .expect("Tracked pool pins");
    // The pin reflects backfill (8) + complete-block pump (10): tick 60
    // gross = 100 (seed) + 30 (backfill) + 20 (pump@10) = 150. The pin's
    // `update_block` is 10 (the highest complete-block event). The
    // in-progress block-11 Burn (-15) was RETAINED by the gate.
    assert_eq!(
        pinned_block, 10,
        "pin reflects backfill + complete pump only"
    );
    assert_eq!(
        tick_data.get(&60).unwrap().liquidity_gross,
        U128::from(150),
        "pin = seed + backfill + complete-pump (block-11 Burn retained)"
    );
    // set_live flushes the retained tail (block-11 Burn).
    core.set_v4_pool_live(pool_manager, pool_id_bytes);
    let s = core.get_v4_pool(pool_id).unwrap();
    // OB7UNY two-stamp: the retained-tail Burn (block 11) is tick-map-only,
    // so it advances the LIQUIDITY clock; the price clock stays at 10.
    assert_eq!(
        s.tick_data_block, 11,
        "flush advanced the retained tail (liquidity clock)"
    );
    assert_eq!(
        s.update_block, 10,
        "price clock untouched by the out-of-range Burn"
    );
    assert_eq!(
        s.tick_data.get(&60).unwrap().liquidity_gross,
        U128::from(135),
        "flush applied the block-11 Burn (150 - 15)"
    );
}

/// Backfill-boundary regression: the FSM does NOT accidentally gate the
/// backfill buffer (it is ungated `drain_backfill`). A quarantined pool
/// whose ONLY events are in the backfill gap still drains them fully at
/// `apply_backfill_buffer_*` — the two-step verify passes because the pin
/// reflects the complete backfill.
#[test]
fn quarantined_pool_backfill_always_fully_drained() {
    use alloy::primitives::U128;
    let pool_manager = Address::from([0x44u8; 20]);
    let pool_id_bytes: [u8; 32] = [0xeeu8; 32];
    let mut core = BotState::new();
    // A backfill event at block 9 (gap S+1..W-1). No pump buffer events.
    core.buffer_backfill_v4_liquidity_update(
        pool_manager,
        pool_id_bytes,
        60,
        120,
        I256::try_from(40i128).unwrap(),
        9,
    );
    let pool_id = register_v4_on_core(&mut core, 10);
    core.set_v4_pool_quarantined(pool_manager, pool_id_bytes);
    // NO cutoff set (no tombstone yet) — the gated pump drain would yield
    // nothing, but the backfill drain is UNGATED and must still apply.
    core.apply_backfill_buffer_v4(pool_manager, pool_id_bytes);
    core.apply_pump_buffer_v4(pool_manager, pool_id_bytes);
    core.pin_v4_post_drain_snapshot(pool_manager, &pool_id_bytes);
    let (tick_data, pinned_block) = core
        .take_v4_post_drain_snapshot(pool_manager, &pool_id_bytes)
        .expect("Tracked pool pins");
    // The backfill event applied (ungated) → the tick (seed 100 + 40) is
    // present. `update_block` is MONOTONIC (no rewind): the pool registered
    // at block 10 and the backfill event is at the older block 9, so the
    // seed block 10 is retained — applying an older event must not rewind
    // the metadata to look stale (AV42C7: the backfill drain rewinding a
    // head-fresh pool's `update_block` to the backfill boundary produced
    // the solver-state false positives).
    assert_eq!(
        pinned_block, 10,
        "update_block must not rewind to an older backfill block"
    );
    assert_eq!(
        tick_data.get(&60).unwrap().liquidity_gross,
        U128::from(140),
        "backfill fully drained (100 seed + 40)"
    );
    let _ = pool_id;
}

/// Concurrent-registration invariant: many pools registered concurrently
/// with a live pump delivering interleaved ModifyLiquidity/Swap across
/// blocks — every pool's pin's `update_block` ≤ `last_complete_block`
/// while Quarantined (the family-level closure of the single-pool fix).
#[test]
#[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn concurrent_registration_lifecycle_invariant() {
    use alloy::primitives::U128;
    let pool_manager = Address::from([0x44u8; 20]);
    let mut core = BotState::new();
    // Register three V4 pools (distinct pool_ids), quarantine each.
    let pool_ids: Vec<([u8; 32], u64)> = (0..3)
        .map(|i| {
            let pid_byte = 0xeeu8 + i as u8;
            let pid_bytes = [pid_byte; 32];
            let pool_id = register_v4_on_core_with_pid(&mut core, pid_bytes, 10);
            core.set_v4_pool_quarantined(pool_manager, pid_bytes);
            (pid_bytes, pool_id)
        })
        .collect();
    // The pump has fully delivered block 10 (tombstone).
    core.advance_pump_complete_cutoff(10);
    // Interleaved live events: a ModifyLiquidity on pool 0, a Swap on
    // pool 1, a ModifyLiquidity on pool 2 — all at the in-progress block
    // 11. All deferred to their respective pump buffers.
    core.apply_v4_liquidity_update(
        pool_manager,
        pool_ids[0].0,
        60,
        120,
        I256::try_from(-10i128).unwrap(),
        11,
    );
    core.apply_v4_swap(
        &V4SwapUpdate {
            pool_manager,
            pool_id: pool_ids[1].0,
            sqrt_price_x96: U256::from(2u128) << 96,
            liquidity: 2_000_000,
            tick: 1,
            tick_priors: Box::default(),
        },
        11,
    );
    core.apply_v4_liquidity_update(
        pool_manager,
        pool_ids[2].0,
        60,
        120,
        I256::try_from(5i128).unwrap(),
        11,
    );
    // Drain + pin each pool. For every pool, the pin's `update_block`
    // MUST be ≤ `last_complete_block` (10) — the in-progress block 11
    // events were retained by the gate, NOT applied.
    for (pid_bytes, _) in &pool_ids {
        core.apply_pump_buffer_v4(pool_manager, *pid_bytes);
        core.pin_v4_post_drain_snapshot(pool_manager, pid_bytes);
        let (_, pinned_block) = core
            .take_v4_post_drain_snapshot(pool_manager, pid_bytes)
            .expect("each Tracked pool pins");
        assert!(
            pinned_block <= 10,
            "pool {pid_bytes:?}: pin update_block {pinned_block} must be ≤ last_complete_block 10"
        );
    }
    // Pool 0's tick 60 unchanged (block-11 Burn retained). Pool 2's
    // tick 60 unchanged (block-11 Mint retained). Pool 1's scalars
    // unchanged (block-11 Swap retained).
    let s0 = core.get_v4_pool(pool_ids[0].1).unwrap();
    assert_eq!(
        s0.tick_data.get(&60).unwrap().liquidity_gross,
        U128::from(100)
    );
    let s2 = core.get_v4_pool(pool_ids[2].1).unwrap();
    assert_eq!(
        s2.tick_data.get(&60).unwrap().liquidity_gross,
        U128::from(100)
    );
    let s1 = core.get_v4_pool(pool_ids[1].1).unwrap();
    assert_eq!(s1.sqrt_price_x96, U256::from(1u128) << 96, "swap deferred");
}

/// Lifecycle invariant property: for any interleaving of (live pump
/// events, drain, pin) over a Quarantined pool, the pin's `update_block`
// ≤ `last_complete_block` AND the pinned `tick_data` excludes any
// in-progress-block liquidity event. Enumerated interleaving ( Swap arrives
// before the Mint, both same-block in-progress — both retained).
#[test]
fn lifecycle_invariant_swap_before_mint_same_inprogress_block() {
    use alloy::primitives::U128;
    let pool_manager = Address::from([0x44u8; 20]);
    let pool_id_bytes: [u8; 32] = [0xeeu8; 32];
    let mut core = BotState::new();
    let _pool_id = register_v4_on_core(&mut core, 10);
    core.set_v4_pool_quarantined(pool_manager, pool_id_bytes);
    core.advance_pump_complete_cutoff(10);
    // Swap arrives FIRST (logIdx 120), then Mint (logIdx 1433) — both at
    // the in-progress block 11. Cross-type arrival order is preserved in
    // the buffer (Swap before Mint in the Vec).
    core.apply_v4_swap(
        &V4SwapUpdate {
            pool_manager,
            pool_id: pool_id_bytes,
            sqrt_price_x96: U256::from(3u128) << 96,
            liquidity: 3_000_000,
            tick: 2,
            tick_priors: Box::default(),
        },
        11,
    );
    core.apply_v4_liquidity_update(
        pool_manager,
        pool_id_bytes,
        60,
        120,
        I256::try_from(25i128).unwrap(),
        11,
    );
    core.apply_pump_buffer_v4(pool_manager, pool_id_bytes);
    core.pin_v4_post_drain_snapshot(pool_manager, &pool_id_bytes);
    let (tick_data, pinned_block) = core
        .take_v4_post_drain_snapshot(pool_manager, &pool_id_bytes)
        .expect("Tracked pool pins");
    assert_eq!(pinned_block, 10, "in-progress block 11 events retained");
    assert_eq!(
        tick_data.get(&60).unwrap().liquidity_gross,
        U128::from(100),
        "Mint at 11 NOT applied — retained"
    );
    // Flush at Live applies BOTH in arrival order (Swap, then Mint).
    core.set_v4_pool_live(pool_manager, pool_id_bytes);
}

/// Reorg-during-quarantine edge (documented): a reorg that changes
/// on-chain@`pinned_block` surfaces as a step-2 `VerificationMismatchError`
/// (fail-fast) — the pinned pair reflects pre-reorg state, on-chain
/// reflects post-reorg. This test documents that the pin (the verified
/// pair) is a SEPARATE clone, so a reorg's `restore_before_block` on the
/// live pool state does NOT corrupt the already-consumed pin; the
/// reorg's effect on the retained tail is a known gap (the flush-at-Live
/// would re-apply reorged-block events) that is mitigation-gated to the
/// reorg coordinator (out of scope: 6N7XVR does not rewrite the reorg
/// path). Here we assert the pin-independence property.
#[test]
fn reorg_during_quarantine_pin_is_independent_of_live_rollback() {
    use alloy::primitives::U128;
    let pool_manager = Address::from([0x44u8; 20]);
    let pool_id_bytes: [u8; 32] = [0xeeu8; 32];
    let mut core = BotState::new();
    let pool_id = register_v4_on_core(&mut core, 10);
    core.set_v4_pool_quarantined(pool_manager, pool_id_bytes);
    core.advance_pump_complete_cutoff(10);
    // A complete-block Mint at 10 (applied at drain), then pin.
    core.apply_v4_liquidity_update(
        pool_manager,
        pool_id_bytes,
        60,
        120,
        I256::try_from(50i128).unwrap(),
        10,
    );
    core.apply_pump_buffer_v4(pool_manager, pool_id_bytes);
    core.pin_v4_post_drain_snapshot(pool_manager, &pool_id_bytes);
    let (tick_data, pinned_block) = core
        .take_v4_post_drain_snapshot(pool_manager, &pool_id_bytes)
        .expect("Tracked pool pins");
    assert_eq!(pinned_block, 10);
    assert_eq!(tick_data.get(&60).unwrap().liquidity_gross, U128::from(150));
    // Now a reorg rolls back block 10 on the LIVE pool state. The
    // already-consumed pin (a clone) is UNAFFECTED — verify @ pinned_block
    // 10 would compare this frozen pair against post-reorg on-chain@10
    // (which lacks the Mint) → mismatch → fail-fast (the reorg surfaces).
    core.restore_pool_before_block(pool_id, 10);
    let s = core.get_v4_pool(pool_id).unwrap();
    assert_eq!(
        s.tick_data.get(&60).unwrap().liquidity_gross,
        U128::from(100),
        "live state rolled back the Mint"
    );
    // The pin (consumed above) is independent — it still holds (150, 10),
    // a frozen snapshot the verify compares against post-reorg on-chain.
    // (No re-assertion of the pin value: it was moved out by take_*.)
}
