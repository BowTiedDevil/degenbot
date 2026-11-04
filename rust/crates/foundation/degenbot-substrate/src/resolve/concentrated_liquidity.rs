//! concentrated liquidity family resolve rows (V3 + V4) — the registry-lookup + solver-vocabulary
//! wrap seam. ONE dispatch row in [`super::resolve_hops`]
//! (`HopType::V3 | HopType::V4`) routes here; the two entries below share
//! only that row, the thin `ResolvedHop::V3/V4` wrap, and the nonce return.
//!
//! GUARDRAIL (load-bearing, do not fuse): the projections themselves are the
//! family read module's door —
//! `degenbot_pools::families::concentrated_liquidity::capture_hop` — where the
//! design-private halves (`families/concentrated_liquidity/{v3.rs,v4.rs}`)
//! deliberately share NO code. The `build_int_v*_sequence` builders own the
//! load-bearing differences — fee convention (V3 `gamma = 1e6 − lp_fee` vs V4
//! combined `swapFee`) and per-range net-sign direction (V4 only).
//! This file keeps ONLY the state-owner seam: the registry lookups, the nonce,
//! and the one `ResolvedHop` wrap per variant.
//!
//! TABLE DERIVATION (shared, still downstream of the per-family sequence
//! builders): the promoted fused-epoch pair (cache-lab winner S1) lives in
//! [`PreparedConcentratedLiquidityHop::derive`] — ONE crossing build derives BOTH the crossing
//! table and the word profiles from it, and folds the sequence's derivation
//! fingerprint once. It reinterprets nothing about fee convention or net-sign
//! direction; only the (identical for both families) table derivation is
//! shared.

use degenbot_pools::families::concentrated_liquidity as concentrated_liquidity_read;
use degenbot_pools::solve::{HopType, MixedPoolRef, ResolvedHop};
use degenbot_pools::PreparedConcentratedLiquidityHop;

use super::super::BotState;
use super::MissingHopReason;

/// The ONE concentrated liquidity resolve row (ADR-073 D2): both designs route here from the
/// dispatcher; the design match happens on the hop type and each arm keeps
/// its own thin wrap.
pub(crate) fn project(
    core: &BotState,
    pool_ref: &MixedPoolRef,
) -> Result<(ResolvedHop, u64), MissingHopReason> {
    match pool_ref.hop_type {
        HopType::V3 => project_v3(core, pool_ref),
        HopType::V4 => project_v4(core, pool_ref),
        _ => Err(MissingHopReason::MissingState),
    }
}

/// V3 resolve row: read pool state + identity off `core` (ADR-003), then the
/// family read module's [`concentrated_liquidity_read::capture_hop`] door builds the integer
/// tick-range sequence the concentrated liquidity solver consumes lock-free. The row wraps the
/// solver-vocabulary hop (`ResolvedHop::V3`) — the hop is solver vocabulary,
/// so the wrapper stays at the row.
pub(crate) fn project_v3(
    core: &BotState,
    pool_ref: &MixedPoolRef,
) -> Result<(ResolvedHop, u64), MissingHopReason> {
    let pool_state = core
        .pool_slice::<degenbot_pools::V3PoolState>(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingState)?;
    let identity = core
        .pool_slice::<degenbot_pools::V3PoolIdentity>(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingIdentity)?;
    // Build the sequence ONCE through the family door and Arc-share it -
    // every path reusing this (pool, direction) clones an Arc, not the
    // ranges Vec.
    let int_seq = concentrated_liquidity_read::capture_hop(
        concentrated_liquidity_read::ConcentratedLiquidityState::V3 {
            state: pool_state,
            scalars: concentrated_liquidity_read::DesignScalars::new(
                identity.fee,
                identity.tick_spacing,
            ),
        },
        pool_ref.zero_for_one,
    )
    .map_err(concentrated_liquidity_refusal)?;

    // Derive the crossing table + word profiles + fingerprint ONCE (the fused
    // S1 derivation) — the row constructs the carried solver value here.
    let prepared = PreparedConcentratedLiquidityHop::derive(int_seq);
    Ok((ResolvedHop::V3 { prepared }, pool_state.state_nonce))
}

/// V4 resolve row: identical concentrated liquidity math as V3 (BotState-owned, ADR-003), with
/// V4's own fee/protocol-fee convention handled behind the door (inside
/// `build_int_v4_sequence`). An amount-modifying hook no longer refuses the
/// projection (ADR-090): the row projects the standard-math walk and
/// `resolve_hops` unions `Caveats::HOOKED_POOL` onto the path, where the
/// finalist re-quote tier reads it.
///
/// F2 — the search-time fee. A pool whose charge its hook carries
/// ([`charge_is_hook_carried`]: keyed at the `DYNAMIC_FEE_FLAG` sentinel, or
/// keyed at zero with an amount-modifying hook) has no usable fee in its key, so
/// the walk is built at the LAST MEASURED charge of THIS direction — the
/// record the black-box twin writes when the finalist re-quote prices the leg.
/// Two exclusions follow, and both are the point:
///
/// - **unmeasured** (`HookFeeUnmeasured`) — the direction has no record, so the
///   pool is NOT SEARCHABLE. Searching it at fee 0 is the systematic
///   overestimate the census measured at 1.0–1.5% for 44.7% of the population
///   and 0–42% across the long tail, against arb edges of a few bps.
/// - **refused** (`HookFeeRefused`) — the chain refuses the direction outright
///   (44% of the census's pool-direction walks), so no fee could make it a hop.
///
/// Both are CLEARABLE (a measurement writes the record and bumps the pool's
/// state nonce, which re-walks the memo entry), so neither is structurally
/// unroutable. A pool whose charge is its key's fee — every hookless pool, and
/// every static-fee pool, hooked or not — projects exactly as before.
pub(crate) fn project_v4(
    core: &BotState,
    pool_ref: &MixedPoolRef,
) -> Result<(ResolvedHop, u64), MissingHopReason> {
    let pool_state = core
        .pool_slice::<degenbot_pools::V4PoolState>(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingState)?;
    let identity = core
        .pool_slice::<degenbot_pools::V4PoolIdentity>(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingIdentity)?;
    // F2: the walk's fee field — the key's own fee for a pool whose charge its
    // key carries, the recorded per-direction hook charge otherwise.
    let fee = if degenbot_pools::charge_is_hook_carried(&identity.pool_key) {
        match core.recorded_hook_fee(pool_ref.pool_key, pool_ref.zero_for_one) {
            None => return Err(MissingHopReason::HookFeeUnmeasured),
            Some(record) => record
                .rankable_pips()
                .ok_or(MissingHopReason::HookFeeRefused)?,
        }
    } else {
        identity.pool_key.fee
    };

    let int_seq = concentrated_liquidity_read::capture_hop(
        concentrated_liquidity_read::ConcentratedLiquidityState::V4 {
            state: pool_state,
            scalars: concentrated_liquidity_read::DesignScalars::new(
                fee,
                identity.pool_key.tick_spacing,
            ),
            hooks: identity.pool_key.hooks,
        },
        pool_ref.zero_for_one,
    )
    .map_err(concentrated_liquidity_refusal)?;

    // Derive as the V3 row: ONE crossing build, word profiles from it, the
    // sequence's fingerprint folded once at derivation.
    let prepared = PreparedConcentratedLiquidityHop::derive(int_seq);
    Ok((ResolvedHop::V4 { prepared }, pool_state.state_nonce))
}

/// Map the family read module's capture refusal onto the resolve
/// invalidation vocabulary — the row's one mapping site.
fn concentrated_liquidity_refusal(
    refusal: concentrated_liquidity_read::CaptureRefusal,
) -> MissingHopReason {
    match refusal {
        concentrated_liquidity_read::CaptureRefusal::NotViable => MissingHopReason::NotViable,
        concentrated_liquidity_read::CaptureRefusal::SequenceUnavailable => {
            MissingHopReason::SequenceUnavailable
        }
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, clippy::expect_used)]
    use hashbrown::HashMap;

    use super::super::MissingHopReason;
    use super::{project_v3, project_v4};
    use crate::PoolTickCoverage;
    use crate::{BotState, RegisterV3PoolParams, RegisterV4PoolParams, TickInfo, V4PoolKey};
    use alloy::primitives::{Address, U128, U256};
    use degenbot_pools::solve::{HopType, MixedPoolRef, ResolvedHop};

    /// Projection stand-in for tests: the word-profile table is built fresh per
    /// call (`build_concentrated_liquidity_word_profiles`, no cross-call reuse).
    fn proj3(core: &BotState, r: &MixedPoolRef) -> Result<(ResolvedHop, u64), MissingHopReason> {
        project_v3(core, r)
    }
    fn proj4(core: &BotState, r: &MixedPoolRef) -> Result<(ResolvedHop, u64), MissingHopReason> {
        project_v4(core, r)
    }

    // -----------------------------------------------------------------
    // Per-family projection tests. concentrated liquidity guardrail: V3 and
    // V4 are two adapters behind the shared `ConcentratedLiquidityPool`
    // interface with genuinely distinct builders — the tests here stay
    // per-adapter and never share a construction path.
    // -----------------------------------------------------------------

    fn v3_ref(pool_key: u64, zero_for_one: bool) -> MixedPoolRef {
        MixedPoolRef {
            hop_type: HopType::V3,
            pool_key,
            zero_for_one,
        }
    }

    fn v4_ref(pool_key: u64, zero_for_one: bool) -> MixedPoolRef {
        MixedPoolRef {
            hop_type: HopType::V4,
            pool_key,
            zero_for_one,
        }
    }

    /// Two initialized ticks straddling the current tick — enough positions
    /// for a non-empty active-set walk in both directions.
    fn two_ticks() -> HashMap<i32, TickInfo> {
        let mut t = HashMap::new();
        t.insert(
            120,
            TickInfo {
                liquidity_gross: U128::from(10_000),
                liquidity_net: 5_000i128,
                block: 0,
            },
        );
        t.insert(
            -120,
            TickInfo {
                liquidity_gross: U128::from(8_000),
                liquidity_net: -4_000i128,
                block: 0,
            },
        );
        t
    }

    fn register_v3(core: &mut BotState, tick_data: HashMap<i32, TickInfo>) -> u64 {
        core.register_v3_pool(&RegisterV3PoolParams {
            address: Address::from([0x22u8; 20]),
            token0: Address::from([0x30u8; 20]),
            token1: Address::from([0x31u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336u128),
            liquidity: 10_000_000_000_000,
            tick: 0,
            tick_data,
            update_block: 42,
            tick_data_block: None,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            ..Default::default()
        })
        .expect("v3 registration")
    }

    // --- directional viability gate (ported from the archived Python
    // swap_is_viable pattern): non-viable hops fail with MissingHopReason::NotViable
    // BEFORE any tick-range walk ---

    #[test]
    fn project_v3_one_sided_liquidity_is_not_viable_in_dead_direction() {
        // What: all initialized ticks ABOVE the current price → a zfo
        // projection (walking DOWN) is NotViable, not SequenceUnavailable.
        // Why: the viability gate must reject the ~65% dead population with an
        // O(1) extremes check before the O(tick_data) range walk runs.
        let mut core = BotState::new();
        let mut ticks = two_ticks();
        ticks.remove(&-120); // leave only the tick ABOVE current price (120)
        let v3_id = register_v3(&mut core, ticks);

        let reason = proj3(&core, &v3_ref(v3_id, true)).unwrap_err();
        assert_eq!(reason, MissingHopReason::NotViable);
        // The live direction still projects.
        assert!(proj3(&core, &v3_ref(v3_id, false)).is_ok());
    }

    #[test]
    fn project_v3_empty_tick_map_is_not_viable() {
        // What: no initialized ticks at all → NotViable in both directions.
        let mut core = BotState::new();
        let v3_id = register_v3(&mut core, HashMap::new());

        assert_eq!(
            proj3(&core, &v3_ref(v3_id, true)).unwrap_err(),
            MissingHopReason::NotViable
        );
        assert_eq!(
            proj3(&core, &v3_ref(v3_id, false)).unwrap_err(),
            MissingHopReason::NotViable
        );
    }

    #[test]
    fn project_v4_one_sided_liquidity_is_not_viable_in_dead_direction() {
        // What: V4 twin of the one-sided check.
        let mut core = BotState::new();
        let mut ticks = two_ticks();
        ticks.remove(&120); // only the tick BELOW price remains; ofz walks UP → dead
        let v4_id = register_v4(&mut core, ticks);

        let reason = proj4(&core, &v4_ref(v4_id, false)).unwrap_err();
        assert_eq!(reason, MissingHopReason::NotViable);
        assert!(proj4(&core, &v4_ref(v4_id, true)).is_ok());
    }

    #[test]
    fn project_v3_builds_tick_range_sequence_in_both_directions() {
        let mut core = BotState::new();
        let v3_id = register_v3(&mut core, two_ticks());

        for zero_for_one in [true, false] {
            let (hop, nonce) = proj3(&core, &v3_ref(v3_id, zero_for_one)).unwrap();
            let seq = hop
                .as_int_sequence()
                .expect("hop is a concentrated liquidity sequence");
            assert!(
                !seq.ranges.is_empty(),
                "initialized ticks -> non-empty walk"
            );
            assert_eq!(
                nonce,
                core.pool_slice::<degenbot_pools::V3PoolState>(v3_id)
                    .expect("state")
                    .state_nonce
            );
        }
    }

    #[test]
    fn project_v3_unregistered_pool_is_missing_state() {
        let core = BotState::new();
        let reason = proj3(&core, &v3_ref(111_111, true)).unwrap_err();
        assert_eq!(reason, MissingHopReason::MissingState);
    }

    #[test]
    fn project_v3_without_tick_data_is_not_viable() {
        // Empty tick map → the viability gate rejects before any walk. (This
        // case used to surface as SequenceUnavailable; NotViable is strictly
        // earlier and O(1).)
        let mut core = BotState::new();
        let v3_id = register_v3(&mut core, HashMap::new());
        let reason = proj3(&core, &v3_ref(v3_id, true)).unwrap_err();
        assert_eq!(reason, MissingHopReason::NotViable);
    }

    fn register_v4(core: &mut BotState, tick_data: HashMap<i32, TickInfo>) -> u64 {
        register_v4_with_hooks(core, tick_data, 0)
    }

    fn register_v4_with_hooks(
        core: &mut BotState,
        tick_data: HashMap<i32, TickInfo>,
        hook_flags: u16,
    ) -> u64 {
        register_v4_with(core, tick_data, 500, hook_flags)
    }

    /// Register a V4 pool with an explicit KEY fee — the F2 tests need a pool
    /// keyed at the `DYNAMIC_FEE_FLAG` sentinel and one keyed at zero.
    fn register_v4_with(
        core: &mut BotState,
        tick_data: HashMap<i32, TickInfo>,
        fee: u32,
        hook_flags: u16,
    ) -> u64 {
        // The hook ADDRESS's low 16 bits carry the permission bitmask — the
        // builder derives hook_flags from it, so craft the address to match.
        let mut hooks = [0u8; 20];
        hooks[18] = (hook_flags >> 8) as u8;
        hooks[19] = (hook_flags & 0xFF) as u8;
        core.register_v4_pool(&RegisterV4PoolParams {
            pool_manager: Address::from([0x44u8; 20]),
            pool_id: [0xabu8; 32],
            pool_key: V4PoolKey {
                currency0: Address::from([0x30u8; 20]),
                currency1: Address::from([0x31u8; 20]),
                fee,
                tick_spacing: 10,
                hooks: Address::new(hooks),
            },
            state_view: Address::ZERO,
            hook_flags,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block: 42,
            tick_data_block: None,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            known_bitmap_words: Vec::new(),
        })
        .expect("v4 registration")
    }

    #[test]
    fn project_v4_builds_tick_range_sequence() {
        let mut core = BotState::new();
        let v4_id = register_v4(&mut core, two_ticks());

        let (hop, nonce) = proj4(&core, &v4_ref(v4_id, true)).unwrap();
        let seq = hop
            .as_int_sequence()
            .expect("hop is a concentrated liquidity sequence");
        assert!(
            !seq.ranges.is_empty(),
            "initialized ticks -> non-empty walk"
        );
        assert_eq!(
            nonce,
            core.pool_slice::<degenbot_pools::V4PoolState>(v4_id)
                .expect("state")
                .state_nonce
        );
    }

    #[test]
    fn project_v4_amount_modifying_hook_projects_the_standard_walk() {
        // ADR-090: an amount-modifying hook no longer refuses projection — the
        // row projects the standard-math walk and the path-level caveat
        // (`resolve_hops`) tells the finalist re-quote tier the leg is
        // approximate.
        let mut core = BotState::new();
        let v4_id = register_v4_with_hooks(&mut core, two_ticks(), 0x80);
        let (hop, _nonce) = proj4(&core, &v4_ref(v4_id, true)).expect("hooked pool projects");
        let seq = hop
            .as_int_sequence()
            .expect("hop is a concentrated liquidity sequence");
        assert!(
            !seq.ranges.is_empty(),
            "a hooked pool's standard-math walk still reaches the solver"
        );
    }

    // -----------------------------------------------------------------
    // F2: the recorded hook charge drives the walk's fee field
    // -----------------------------------------------------------------

    /// The census's reading for the biggest hook family's pools (149.22 bps,
    /// all ten sampled pools) — the record a measurement writes.
    const CENSUS_PIPS: u32 = 14_922;

    /// 1 bp = 100 pips of 1e6.
    const BP_PIPS: u32 = 100;

    /// The walk's fee the projection committed, read back off the sequence's
    /// first range (`gamma_numer = 1e6 − swap_fee`, `fee_denom = 1e6`; the
    /// fixture pools carry no protocol fee, so the swap fee IS the fee input).
    fn projected_fee_pips(core: &BotState, pool_id: u64, zero_for_one: bool) -> u32 {
        let (hop, _nonce) = proj4(core, &v4_ref(pool_id, zero_for_one)).expect("projection");
        let seq = hop
            .as_int_sequence()
            .expect("concentrated liquidity sequence");
        let fee_denom = seq.ranges[0].fee_denom;
        assert_eq!(fee_denom, 1_000_000, "the walk's fee denominator is pips");
        1_000_000 - u32::try_from(seq.ranges[0].gamma_numer).expect("gamma fits u32")
    }

    #[test]
    fn an_unmeasured_hook_carried_pool_is_not_searchable() {
        // The sentinel pool from F1 and the census's hooked zero-fee population:
        // no record ⇒ NO hop. Never fee 0 — the census measured 1.0–1.5% for
        // 44.7% of the population and 0–42% on the long tail, against arb edges
        // of a few bps.
        for (fee, hook_flags) in [(crate::V4_DYNAMIC_FEE_FLAG, 0x80), (0, 0x80)] {
            let mut core = BotState::new();
            let v4_id = register_v4_with(&mut core, two_ticks(), fee, hook_flags);
            for zero_for_one in [true, false] {
                assert_eq!(
                    proj4(&core, &v4_ref(v4_id, zero_for_one)).unwrap_err(),
                    MissingHopReason::HookFeeUnmeasured,
                    "key fee {fee:#x} / flags {hook_flags:#x} with no record"
                );
            }
        }
    }

    #[test]
    fn a_measured_hook_charge_ranks_the_pool_at_the_recorded_fee() {
        // The recorded last-measured charge is the walk's fee field — composed
        // with the pool's protocol fee exactly like a static key fee.
        let mut core = BotState::new();
        let v4_id = register_v4_with(&mut core, two_ticks(), crate::V4_DYNAMIC_FEE_FLAG, 0x80);

        assert!(core.record_hook_fee(
            v4_id,
            true,
            degenbot_pools::HookFeeReading::Measured { pips: CENSUS_PIPS },
            26_320_000,
        ));
        assert_eq!(
            projected_fee_pips(&core, v4_id, true),
            CENSUS_PIPS,
            "the zfo walk charges the record"
        );
        assert_eq!(
            proj4(&core, &v4_ref(v4_id, false)).unwrap_err(),
            MissingHopReason::HookFeeUnmeasured,
            "the other direction is still unmeasured"
        );
    }

    #[test]
    fn a_refused_direction_is_excluded_in_that_direction_only() {
        // 44% of the census's pool-direction walks refuse outright; a refused
        // leg must never enter a path, and the twin direction is unaffected.
        let mut core = BotState::new();
        let v4_id = register_v4_with(&mut core, two_ticks(), crate::V4_DYNAMIC_FEE_FLAG, 0x80);
        assert!(core.record_hook_fee(
            v4_id,
            true,
            degenbot_pools::HookFeeReading::Refused,
            26_320_000,
        ));
        assert!(core.record_hook_fee(
            v4_id,
            false,
            degenbot_pools::HookFeeReading::Measured { pips: 12_435 },
            26_320_000,
        ));

        assert_eq!(
            proj4(&core, &v4_ref(v4_id, true)).unwrap_err(),
            MissingHopReason::HookFeeRefused
        );
        assert_eq!(projected_fee_pips(&core, v4_id, false), 12_435);
    }

    #[test]
    fn the_hookless_zero_fee_pools_keep_their_fee_0_walk() {
        // The census database's 330 genuine zero-fee pools: no hook, key fee 0,
        // and 0 IS the truth. They must stay searchable and unchanged.
        let mut core = BotState::new();
        let v4_id = register_v4_with(&mut core, two_ticks(), 0, 0);
        for zero_for_one in [true, false] {
            assert_eq!(
                projected_fee_pips(&core, v4_id, zero_for_one),
                0,
                "a hookless zero-fee pool walks at fee 0"
            );
        }
    }

    #[test]
    fn a_hooked_static_fee_pool_walks_at_its_key_fee() {
        // A hooked pool whose KEY carries the charge is priced by that key fee:
        // the record governs only hook-carried charges, so no measurement is
        // needed for it (ADR-090's re-quote remains its exactness backstop).
        let mut core = BotState::new();
        let v4_id = register_v4_with(&mut core, two_ticks(), 3_000, 0x80);
        for zero_for_one in [true, false] {
            assert_eq!(projected_fee_pips(&core, v4_id, zero_for_one), 3_000);
        }
    }

    #[test]
    fn measuring_a_direction_bumps_the_pool_nonce_and_re_walks_the_projection() {
        // The record's WRITE is what clears the exclusion: the projection memo
        // is nonce-validated, so the cached `Invalid(HookFeeUnmeasured)` entry
        // must not outlive the measurement.
        let mut core = BotState::new();
        let v4_id = register_v4_with(&mut core, two_ticks(), 0, 0x80);
        let cache = super::super::HopProjectionCache::new();

        let mut resolved = degenbot_pools::solve::ResolvedMixedPath::default();
        let refs = [v4_ref(v4_id, true), v4_ref(v4_id, true)];
        let deficits = super::super::resolve_hops(&core, &refs, &mut resolved, &cache, None, true);
        assert_eq!(deficits.len(), 2, "both hops are unmeasured");
        assert_eq!(deficits[0].reason, MissingHopReason::HookFeeUnmeasured);

        let before = core.pool_state_nonce(v4_id);
        assert!(core.record_hook_fee(
            v4_id,
            true,
            degenbot_pools::HookFeeReading::Measured { pips: BP_PIPS },
            42,
        ));
        assert_eq!(
            core.pool_state_nonce(v4_id),
            before + 1,
            "a record that changes the fact bumps the pool's nonce"
        );

        let deficits = super::super::resolve_hops(&core, &refs, &mut resolved, &cache, None, true);
        assert!(deficits.is_empty(), "the measured direction projects");
        assert!(resolved.valid);

        // An AGREEING reading (inside the refresh horizon) is a no-op — no
        // nonce churn, no re-walk.
        let settled = core.pool_state_nonce(v4_id);
        assert!(!core.record_hook_fee(
            v4_id,
            true,
            degenbot_pools::HookFeeReading::Measured { pips: BP_PIPS },
            43,
        ));
        assert_eq!(core.pool_state_nonce(v4_id), settled);

        // A DISAGREEING reading supersedes it and re-walks.
        assert!(core.record_hook_fee(
            v4_id,
            true,
            degenbot_pools::HookFeeReading::Measured { pips: 2 * BP_PIPS },
            44,
        ));
        assert_eq!(projected_fee_pips(&core, v4_id, true), 2 * BP_PIPS);
    }

    #[test]
    fn a_record_for_an_unregistered_pool_is_not_stored() {
        let mut core = BotState::new();
        assert!(!core.record_hook_fee(999, true, degenbot_pools::HookFeeReading::Refused, 1,));
        assert_eq!(core.hook_fee_pool_count(), 0);
    }

    #[test]
    fn project_v4_unregistered_pool_is_missing_state() {
        let core = BotState::new();
        let reason = proj4(&core, &v4_ref(222_222, true)).unwrap_err();
        assert_eq!(reason, MissingHopReason::MissingState);
    }

    #[test]
    fn project_v4_without_tick_data_is_not_viable() {
        // V4 twin of the empty-map gate rejection.
        let mut core = BotState::new();
        let v4_id = register_v4(&mut core, HashMap::new());
        let reason = proj4(&core, &v4_ref(v4_id, true)).unwrap_err();
        assert_eq!(reason, MissingHopReason::NotViable);
    }
}
