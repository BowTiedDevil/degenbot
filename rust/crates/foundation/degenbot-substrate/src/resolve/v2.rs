//! V2 (reserve-pair family) resolve row — the registry-lookup +
//! solver-vocabulary wrap seam (ADR-073 D2).
//!
//! The projection itself is the family read module's door —
//! `degenbot_pools::families::reserve_pair::capture_hop` — a pure function of
//! the family state slice (orientation-selected reserves + the per-direction
//! retained-fee fraction, as the math leaf's hop payload). This file keeps
//! ONLY the state-owner seam: the registry lookups, the nonce, and the one
//! `ResolvedHop` wrap.

use degenbot_pools::families::reserve_pair as reserve_pair_read;
use degenbot_solvers::mixed::{MixedPoolRef, ResolvedHop};

use super::super::BotState;
use super::MissingHopReason;

/// V2 resolve row: read the reserve-pair state + identity off `core`
/// (ADR-003), then the family read module's [`reserve_pair_read::capture_hop`]
/// door derives the orientation-specific hop payload from `zero_for_one`
/// (ADR-003 "Swap Orientation": single `PoolEntry` per address, orientation
/// derived at solve — the engine never mutates this state). The row wraps the
/// math leaf's hop into the solver vocabulary (`ResolvedHop::V2`).
pub(crate) fn project_v2(
    core: &BotState,
    pool_ref: &MixedPoolRef,
) -> Result<(ResolvedHop, u64), MissingHopReason> {
    let state = core
        .get_v2_pool_state(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingState)?;
    let identity = core
        .get_v2_identity(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingIdentity)?;
    let hop_state = reserve_pair_read::capture_hop(state, identity, pool_ref.zero_for_one);
    Ok((ResolvedHop::V2 { state: hop_state }, state.state_nonce))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, clippy::expect_used)]
    use super::super::MissingHopReason;
    use super::project_v2;
    use crate::{BotState, RegisterV2PoolParams};
    use alloy::primitives::{aliases::U112, Address, U256};
    use degenbot_solvers::mixed::{HopType, MixedPoolRef};
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
            core.get_v2_pool_state(v2_id).expect("state").state_nonce
        );
    }

    #[test]
    fn project_v2_unregistered_pool_is_missing_state() {
        let core = BotState::new();
        let reason = project_v2(&core, &v2_ref(333_333, true)).unwrap_err();
        assert_eq!(reason, MissingHopReason::MissingState);
    }
}
