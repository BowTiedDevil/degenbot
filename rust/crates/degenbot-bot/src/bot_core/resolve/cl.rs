//! CL family projections (V3 + V4) — two SELF-CONTAINED entries.
//!
//! GUARDRAIL (load-bearing, do not fuse): `project_v3` and `project_v4` share
//! only this file, the thin `ResolvedHop::V3/V4` wrap, and the nonce return.
//! The `build_int_v*_sequence` builders in `degenbot-pools` own the three
//! load-bearing differences — fee convention (V3 `gamma = 1e6 − lp_fee` vs V4
//! combined `swapFee`), current-tick drain framing (V3 leading hop vs V4
//! `base_liquidity` fold), and per-range net-sign direction (V4 only).

use std::sync::Arc;

use degenbot_solvers::mixed::{MixedPoolRef, ResolvedHop};
use degenbot_solvers::mobius_v3_int::ClWordProfileCache;

use super::super::BotState;
use super::MissingHopReason;
use crate::solvers::arb_engine::PoolTickCoverage;

/// V3 projection: read pool state + identity off `core` (ADR-003) and build
/// the integer tick-range sequence the CL solver consumes lock-free.
pub(crate) fn project_v3(
    core: &BotState,
    pool_ref: &MixedPoolRef,
    profile_cache: &mut ClWordProfileCache,
) -> Result<(ResolvedHop, u64), MissingHopReason> {
    let pool_state = core
        .get_v3_pool(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingState)?;
    // Directional viability gate (archived-Python port): reject a dead
    // direction with an O(1) extremes check BEFORE the O(tick_data) walk in
    // `build_int_v3_sequence`. Sparse-coverage pools default viable (no data
    // to judge — mirrors Python's `sparse_liquidity_map` early-True).
    if pool_state.coverage == PoolTickCoverage::Tracked
        && !pool_state.swap_is_viable(pool_ref.zero_for_one)
    {
        return Err(MissingHopReason::NotViable);
    }
    let identity = core
        .get_v3_identity(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingIdentity)?;
    let int_seq = pool_state
        .build_int_v3_sequence(
            identity.tick_spacing,
            identity.fee,
            pool_ref.zero_for_one,
            // 24 = the active-set walk feed depth. The enumeration-era
            // value was 10 (tuple cap); the walk has no tuple cap, so depth
            // is bounded by data availability (the range cache stores 24).
            24,
        )
        .ok_or(MissingHopReason::SequenceUnavailable)?;

    let word_profiles = profile_cache.prepare(&int_seq);
    Ok((
        ResolvedHop::V3 {
            int_seq,
            word_profiles: Arc::new(word_profiles),
        },
        pool_state.state_nonce,
    ))
}

/// V4 projection: identical CL math as V3 (BotState-owned, ADR-003), with
/// V4's own fee/protocol-fee convention handled inside
/// `build_int_v4_sequence`.
pub(crate) fn project_v4(
    core: &BotState,
    pool_ref: &MixedPoolRef,
    profile_cache: &mut ClWordProfileCache,
) -> Result<(ResolvedHop, u64), MissingHopReason> {
    let pool_state = core
        .get_v4_pool(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingState)?;
    // Directional viability gate — V4 twin of the V3 site above.
    if pool_state.coverage == PoolTickCoverage::Tracked
        && !pool_state.swap_is_viable(pool_ref.zero_for_one)
    {
        return Err(MissingHopReason::NotViable);
    }
    let identity = core
        .get_v4_identity(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingIdentity)?;
    let int_seq = pool_state
        .build_int_v4_sequence(
            identity.pool_key.tick_spacing,
            identity.pool_key.fee,
            pool_ref.zero_for_one,
            // 24 = the active-set walk feed depth (twin of the V3
            // site above).
            24,
        )
        .ok_or(MissingHopReason::SequenceUnavailable)?;

    let word_profiles = profile_cache.prepare(&int_seq);
    Ok((
        ResolvedHop::V4 {
            int_seq,
            word_profiles: Arc::new(word_profiles),
        },
        pool_state.state_nonce,
    ))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, clippy::expect_used)]
    use std::collections::HashMap;

    use super::super::MissingHopReason;
    use super::{project_v3, project_v4};
    use crate::bot_core::{
        BotState, RegisterV3PoolParams, RegisterV4PoolParams, TickInfo, V4PoolKey,
    };
    use crate::solvers::arb_engine::PoolTickCoverage;
    use alloy::primitives::{Address, I256, U128, U256};
    use degenbot_solvers::mixed::{HopType, MixedPoolRef, ResolvedHop};
    use degenbot_solvers::mobius_v3_int::ClWordProfileCache;

    /// Projection with a throwaway per-pool profile cache. Tests here don't need
    /// cross-call reuse (the per-range reuse property lives in the solvers cache
    /// tests); these are just the valid per-pool-cache stand-in.
    fn proj3(core: &BotState, r: &MixedPoolRef) -> Result<(ResolvedHop, u64), MissingHopReason> {
        project_v3(core, r, &mut ClWordProfileCache::default())
    }
    fn proj4(core: &BotState, r: &MixedPoolRef) -> Result<(ResolvedHop, u64), MissingHopReason> {
        project_v4(core, r, &mut ClWordProfileCache::default())
    }

    // -----------------------------------------------------------------
    // Per-family projection tests. CL guardrail: V3 and
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
                liquidity_net: I256::try_from(5_000i128).unwrap(),
                block: 0,
            },
        );
        t.insert(
            -120,
            TickInfo {
                liquidity_gross: U128::from(8_000),
                liquidity_net: I256::try_from(-4_000i128).unwrap(),
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
            let seq = hop.as_int_sequence().expect("hop is a CL sequence");
            assert!(
                !seq.ranges.is_empty(),
                "initialized ticks -> non-empty walk"
            );
            assert_eq!(nonce, core.get_v3_pool(v3_id).expect("state").state_nonce);
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
        core.register_v4_pool(&RegisterV4PoolParams {
            pool_manager: Address::from([0x44u8; 20]),
            pool_id: [0xabu8; 32],
            pool_key: V4PoolKey {
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
            update_block: 42,
            tick_data_block: None,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
        })
        .expect("v4 registration")
    }

    #[test]
    fn project_v4_builds_tick_range_sequence() {
        let mut core = BotState::new();
        let v4_id = register_v4(&mut core, two_ticks());

        let (hop, nonce) = proj4(&core, &v4_ref(v4_id, true)).unwrap();
        let seq = hop.as_int_sequence().expect("hop is a CL sequence");
        assert!(
            !seq.ranges.is_empty(),
            "initialized ticks -> non-empty walk"
        );
        assert_eq!(nonce, core.get_v4_pool(v4_id).expect("state").state_nonce);
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
