//! V2 (reserve-pair family) resolve row — the registry-lookup +
//! solver-vocabulary wrap seam (ADR-073 D2).
//!
//! The projection itself is the family read module's door —
//! `degenbot_pools::families::reserve_pair::capture_hop` (V2 design) /
//! `capture_aerodrome_hop` (Aerodrome volatile design) — a pure function of
//! the family state slice (orientation-selected reserves + the per-direction
//! fee fraction, as the math leaf's hop payload). This file keeps ONLY the
//! state-owner seam: the registry lookups, the nonce, and the one
//! `ResolvedHop` wrap. The row is ONE dispatch row with TWO entry arms
//! (ADR-059 D6): `PoolEntry::V2` and the Aerodrome VOLATILE design — the
//! engine's `derive_hop_type` routes Aerodrome volatile here, and a `stable`
//! identity (either design) refuses: it is the `SolidlyStable` lane's pool.

use degenbot_pools::families::reserve_pair as reserve_pair_read;
use degenbot_solvers::mixed::{MixedPoolRef, ResolvedHop};

use super::super::BotState;
use super::MissingHopReason;

/// V2 resolve row — ONE dispatch row, TWO entry arms (ADR-059 D6): read the
/// reserve-pair state + identity off `core` (ADR-003), then the family read
/// module's [`reserve_pair_read::capture_hop`] (V2 design) or
/// [`reserve_pair_read::capture_aerodrome_hop`] (Aerodrome volatile design)
/// door derives the orientation-specific hop payload from `zero_for_one`
/// (ADR-003 "Swap Orientation": single `PoolEntry` per address, orientation
/// derived at solve — the engine never mutates this state). The row wraps the
/// math leaf's hop into the solver vocabulary (`ResolvedHop::V2`). A `stable`
/// identity in either design refuses: the stable invariant is the
/// `SolidlyStable` lane's computation, never this row's.
pub(crate) fn project_v2(
    core: &BotState,
    pool_ref: &MixedPoolRef,
) -> Result<(ResolvedHop, u64), MissingHopReason> {
    let v2_state = core.pool_slice::<degenbot_pools::v2_state::V2PoolState>(pool_ref.pool_key);
    if let Some(v2_state) = v2_state {
        let identity = core
            .pool_slice::<degenbot_pools::v2_state::V2PoolIdentity>(pool_ref.pool_key)
            .ok_or(MissingHopReason::MissingIdentity)?;
        let hop_state = reserve_pair_read::capture_hop(v2_state, identity, pool_ref.zero_for_one);
        return Ok((ResolvedHop::V2 { state: hop_state }, v2_state.state_nonce));
    }
    if let Some(state) = core
        .pool_slice::<degenbot_pools::aerodrome_v2_state::AerodromeV2PoolState>(pool_ref.pool_key)
    {
        let identity = core
            .pool_slice::<degenbot_pools::aerodrome_v2_state::AerodromeV2PoolIdentity>(
                pool_ref.pool_key,
            )
            .ok_or(MissingHopReason::MissingIdentity)?;
        if identity.stable {
            // The stable invariant is the `SolidlyStable` lane's; this row's
            // volatile constant product would mis-price it. Refuse rather
            // than project with the wrong math.
            return Err(MissingHopReason::MissingIdentity);
        }
        let hop_state =
            reserve_pair_read::capture_aerodrome_hop(state, identity, pool_ref.zero_for_one);
        return Ok((ResolvedHop::V2 { state: hop_state }, state.state_nonce));
    }
    // Neither reserve-pair state slice present → the unregistered-pool
    // reason (the deficit reason vocabulary is pinned downstream).
    Err(MissingHopReason::MissingState)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, clippy::expect_used)]
    use super::super::{resolve_hops, HopProjectionCache, MissingHopReason};
    use super::project_v2;
    use crate::{BotState, RegisterAerodromeV2PoolParams, RegisterV2PoolParams};
    use alloy::primitives::{aliases::U112, Address, U256};
    use degenbot_solvers::mixed::{HopType, MixedPoolRef, ResolvedMixedPath};
    use degenbot_uniswap::dex_identity::DexVariant;

    // -----------------------------------------------------------------
    // Per-family projection tests: `project_v2` crosses
    // the seam directly — orientation selection + fee routing included.
    // -----------------------------------------------------------------

    fn v2_ref(pool_key: u64, zero_for_one: bool) -> MixedPoolRef {
        MixedPoolRef {
            hop_type: HopType::V2,
            pool_key,
            zero_for_one,
        }
    }

    fn register_v2(core: &mut BotState, addr_byte: u8, r0: u64, r1: u64) -> u64 {
        core.register_v2_pool(&RegisterV2PoolParams {
            address: Address::from([addr_byte; 20]),
            token0: Address::from([0x01u8; 20]),
            token1: Address::from([0x02u8; 20]),
            reserve0: U112::from(r0),
            reserve1: U112::from(r1),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: Address::from([0xfau8; 20]),
            update_block: 7,
            variant: DexVariant::UniswapV2,
            stable_swap: false,
            fee_denominator: None,
            ..Default::default()
        })
        .expect("v2 registration")
    }

    #[test]
    fn project_v2_selects_orientation_specific_reserves_and_fees() {
        let mut core = BotState::new();
        let v2_id = register_v2(&mut core, 0x11, 1_000_000, 2_000_000);

        // token0 in (zero_for_one): input reserve = reserve0.
        let (hop, _) = project_v2(&core, &v2_ref(v2_id, true)).unwrap();
        let s = hop.as_v2_state().expect("hop is V2");
        assert_eq!(s.reserve_in, U256::from(1_000_000u64));
        assert_eq!(s.reserve_out, U256::from(2_000_000u64));
        assert_eq!(s.gamma_numer, U256::from(997u64));
        assert_eq!(s.fee_denom, U256::from(1000u64));

        // token1 in: orientation flips.
        let (hop, nonce) = project_v2(&core, &v2_ref(v2_id, false)).unwrap();
        let s = hop.as_v2_state().expect("hop is V2");
        assert_eq!(s.reserve_in, U256::from(2_000_000u64));
        assert_eq!(s.reserve_out, U256::from(1_000_000u64));
        assert_eq!(s.gamma_numer, U256::from(997u64));
        assert_eq!(
            nonce,
            core.pool_slice::<degenbot_pools::v2_state::V2PoolState>(v2_id)
                .expect("state")
                .state_nonce
        );
    }

    #[test]
    fn project_v2_unregistered_pool_is_missing_state() {
        // No reserve-pair state slice at all → the pinned unregistered-pool
        // reason (the deficit reason vocabulary is pinned downstream).
        let core = BotState::new();
        let reason = project_v2(&core, &v2_ref(333_333, true)).unwrap_err();
        assert_eq!(reason, MissingHopReason::MissingState);
    }

    fn register_aerodrome_volatile(core: &mut BotState, addr_byte: u8, r0: u64, r1: u64) -> u64 {
        core.register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
            token0_decimals: 6,
            token1_decimals: 18,
            address: Address::from([addr_byte; 20]),
            token0: Address::from([0x01u8; 20]),
            token1: Address::from([0x02u8; 20]),
            factory: Address::from([0xafu8; 20]),
            variant: DexVariant::AerodromeV2Volatile,
            stable: false,
            fee: (3, 1000),
            reserve0: U112::from(r0),
            reserve1: U112::from(r1),
            update_block: 0,
        })
        .expect("test setup: Aerodrome registration")
    }

    fn register_aerodrome_stable(core: &mut BotState, addr_byte: u8) -> u64 {
        core.register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
            token0_decimals: 18,
            token1_decimals: 18,
            address: Address::from([addr_byte; 20]),
            token0: Address::from([0x01u8; 20]),
            token1: Address::from([0x02u8; 20]),
            factory: Address::from([0xafu8; 20]),
            variant: DexVariant::AerodromeV2Stable,
            stable: true,
            fee: (3, 1000),
            reserve0: U112::from(1_000_000u64),
            reserve1: U112::from(2_000_000u64),
            update_block: 0,
        })
        .expect("test setup: Aerodrome registration")
    }

    #[test]
    fn project_v2_admits_the_aerodrome_volatile_lane_in_a_mixed_path() {
        // What: the row's second entry arm (ADR-059 D6) — an Aerodrome
        // VOLATILE pool (the engine's `derive_hop_type` routes it to
        // HopType::V2) resolves-and-projects through the V2 lane alongside a
        // V2 pool in a mixed path; its hop payload carries the
        // fee-fraction → retained-gamma conversion.
        let mut core = BotState::new();
        let aero = register_aerodrome_volatile(&mut core, 0xae, 1_000_000, 2_000_000);
        let v2 = register_v2(&mut core, 0x11, 1_200_000, 1_000_000);

        // Per-hop: the Aerodrome arm's payload.
        let (hop, _) = project_v2(&core, &v2_ref(aero, true)).unwrap();
        let s = hop.as_v2_state().expect("hop is V2");
        assert_eq!(s.reserve_in, U256::from(1_000_000u64));
        assert_eq!(s.reserve_out, U256::from(2_000_000u64));
        // fee (3, 1000) → retained (997, 1000).
        assert_eq!(s.gamma_numer, U256::from(997u64));
        assert_eq!(s.fee_denom, U256::from(1000u64));

        // The mixed path resolves: both V2-lane hops project.
        let refs = [v2_ref(aero, true), v2_ref(v2, true)];
        let cache = HopProjectionCache::new();
        let mut resolved = ResolvedMixedPath::default();
        let deficits = resolve_hops(&core, &refs, &mut resolved, &cache, None, true);
        assert!(deficits.is_empty(), "mixed path must resolve: {deficits:?}");
        assert!(resolved.valid);
        assert_eq!(resolved.hops.len(), 2);
        assert!(resolved.hops.iter().all(|h| h.as_v2_state().is_some()));
    }

    #[test]
    fn project_v2_refuses_a_stable_aerodrome_identity() {
        // What: a `stable` Aerodrome identity routed at the V2 lane refuses —
        // the stable invariant is the SolidlyStable lane's; the volatile
        // constant product would mis-price it.
        let mut core = BotState::new();
        let aero = register_aerodrome_stable(&mut core, 0xae);
        let reason = project_v2(&core, &v2_ref(aero, true)).unwrap_err();
        assert_eq!(reason, MissingHopReason::MissingIdentity);
    }

    #[test]
    fn reserve_pair_pool_count_names_the_v2_lanes_admitted_entries() {
        // What: the lane count == the entries the V2 solve lane admits — the
        // volatile reserve-pair designs (ADR-059 D6). A Camelot `stable_swap`
        // V2 entry and a stable Aerodrome entry are the SolidlyStable lane's
        // designs and are NOT counted.
        let mut core = BotState::new();
        let _v2 = register_v2(&mut core, 0x11, 1_200, 1_000);
        let _aero = register_aerodrome_volatile(&mut core, 0xae, 1_000, 2_000);
        assert_eq!(core.reserve_pair_pool_count(), 2);

        // The SolidlyStable lane's two reserve-pair designs are not counted.
        let _stable_aero = register_aerodrome_stable(&mut core, 0xa5);
        core.register_v2_pool(&RegisterV2PoolParams {
            address: Address::from([0xc1u8; 20]),
            token0: Address::from([0x01u8; 20]),
            token1: Address::from([0x02u8; 20]),
            reserve0: U112::from(1_000u64),
            reserve1: U112::from(2_000u64),
            fee_token0: (9970, 10000),
            fee_token1: (9970, 10000),
            factory: Address::from([0xfau8; 20]),
            update_block: 0,
            variant: DexVariant::CamelotV2Stable,
            stable_swap: true,
            fee_denominator: Some(10_000),
            ..Default::default()
        })
        .expect("test setup: Camelot stable registration");
        assert_eq!(
            core.reserve_pair_pool_count(),
            2,
            "stable designs stay out of the V2 lane's count"
        );
    }
}
