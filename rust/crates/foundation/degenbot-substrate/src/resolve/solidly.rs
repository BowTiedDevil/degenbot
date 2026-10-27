//! Solidly-stable (Aerodrome stable / Camelot `stable_swap`) resolve row —
//! the registry-lookup + solver-vocabulary wrap seam (ADR-073 D2).
//!
//! The projection itself is the family read module's door —
//! `degenbot_pools::families::solidly::capture_hop` — a pure function of the
//! family state slice plus the registry scales. This row keeps ONLY the
//! state-owner seams: the registry lookups (the dedup'd token-decimals fetch,
//! ADR-003 single source of truth), and the one `ResolvedHop` wrap + nonce.
//! The fee conventions (Aerodrome fee-fraction direct; Camelot retained-gamma
//! inverted to fee) are design-private behind the door.

use alloy::primitives::{Address, U256};

use degenbot_pools::families::solidly::{self, SolidlyHopProjection, SolidlyState};
use degenbot_pools::solve::{MixedPoolRef, ResolvedHop, SolidlyHopState};

use super::super::BotState;
use super::MissingHopReason;

/// Decimal powers for the pair's tokens from the token registry.
fn token_decimals(
    core: &BotState,
    token0: &Address,
    token1: &Address,
) -> Result<(U256, U256), MissingHopReason> {
    match (core.token_entry(token0), core.token_entry(token1)) {
        (Some(t0), Some(t1)) => Ok((
            U256::from(10u64).pow(U256::from(t0.decimals)),
            U256::from(10u64).pow(U256::from(t1.decimals)),
        )),
        _ => Err(MissingHopReason::MissingTokenPair), // Missing token entry → invalid
    }
}

/// The solver-vocabulary wrap — the row's named seam: the family projection
/// into the solver's hop payload, field by field.
fn wrap_hop(hop: SolidlyHopProjection) -> ResolvedHop {
    ResolvedHop::SolidlyStable {
        state: SolidlyHopState {
            reserves_0: hop.reserves_0,
            reserves_1: hop.reserves_1,
            decimals_0: hop.decimals_0,
            decimals_1: hop.decimals_1,
            token_in: hop.token_in,
            fee_numer: hop.fee_numer,
            fee_denom: hop.fee_denom,
            stable: hop.stable,
            variant: hop.variant,
        },
    }
}

/// Solidly-stable resolve row. Aerodrome identity wins when present; otherwise
/// a V2 identity (`stable_swap=true`) is the Camelot arm; neither present
/// means the pool is not a Solidly-stable pool. The projection is the family
/// read module's [`solidly::capture_hop`] door — the registry scales this row
/// fetches are the door's caller-supplied input (both designs).
pub(crate) fn project_solidly(
    core: &BotState,
    pool_ref: &MixedPoolRef,
) -> Result<(ResolvedHop, u64), MissingHopReason> {
    if let Some(id) = core.pool_slice::<degenbot_pools::AerodromeV2PoolIdentity>(pool_ref.pool_key)
    {
        let state = core
            .pool_slice::<degenbot_pools::AerodromeV2PoolState>(pool_ref.pool_key)
            .ok_or(MissingHopReason::MissingState)?;
        let (decimals_0, decimals_1) = token_decimals(core, &id.token0, &id.token1)?;
        let hop = solidly::capture_hop(
            SolidlyState::Aerodrome {
                identity: id,
                state,
            },
            (decimals_0, decimals_1),
            pool_ref.zero_for_one,
        );
        Ok((wrap_hop(hop), state.state_nonce))
    } else if let Some(id) = core.pool_slice::<degenbot_pools::V2PoolIdentity>(pool_ref.pool_key) {
        let state = core
            .pool_slice::<degenbot_pools::V2PoolState>(pool_ref.pool_key)
            .ok_or(MissingHopReason::MissingState)?;
        let (decimals_0, decimals_1) = token_decimals(core, &id.token0, &id.token1)?;
        let hop = solidly::capture_hop(
            SolidlyState::Camelot {
                identity: id,
                state,
                decimals: (decimals_0, decimals_1),
            },
            (decimals_0, decimals_1),
            pool_ref.zero_for_one,
        );
        Ok((wrap_hop(hop), state.state_nonce))
    } else {
        // Not an Aerodrome/Camelot pool → invalid.
        Err(MissingHopReason::MissingIdentity)
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, clippy::expect_used)]
    use super::super::MissingHopReason;
    use super::project_solidly;
    use crate::{BotState, RegisterAerodromeV2PoolParams, RegisterV2PoolParams};
    use alloy::primitives::{aliases::U112, Address, U256};
    use degenbot_pools::solve::{HopType, MixedPoolRef};
    use degenbot_uniswap::dex_identity::DexVariant;

    // -----------------------------------------------------------------
    // Per-family projection tests: assert against the
    // `project_solidly` seam directly — the interface IS the test
    // surface. Ported from the engine-level `resolve_path_*solidly*` tests
    // in `arb_engine/tests.rs`; assertions unchanged, harness simplified
    // to one pool per module (no engine, no 2-hop path).
    // -----------------------------------------------------------------

    fn solidly_ref(pool_key: u64, zero_for_one: bool) -> MixedPoolRef {
        MixedPoolRef {
            hop_type: HopType::SolidlyStable,
            pool_key,
            zero_for_one,
        }
    }

    fn register_aerodrome(core: &mut BotState, addr_byte: u8, r0: u64, r1: u64) -> u64 {
        core.register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
            token0_decimals: 6,
            token1_decimals: 18,
            address: Address::from([addr_byte; 20]),
            token0: Address::from([0x01u8; 20]),
            token1: Address::from([0x02u8; 20]),
            factory: Address::from([0xafu8; 20]),
            variant: DexVariant::AerodromeV2Stable,
            stable: true,
            fee: (3, 1000),
            reserve0: U112::from(r0),
            reserve1: U112::from(r1),
            update_block: 0,
        })
        .expect("test setup: Aerodrome registration")
    }

    fn tokens_usdc_weth(core: &mut BotState) {
        // Tokens: 6-decimal USDC, 18-decimal WETH.
        core.register_token(
            Address::from([0x01u8; 20]),
            "USD Coin".into(),
            "USDC".into(),
            6,
            1,
        );
        core.register_token(
            Address::from([0x02u8; 20]),
            "Wrapped Ether".into(),
            "WETH".into(),
            18,
            1,
        );
    }

    #[test]
    fn project_solidly_builds_aerodrome_stable_hop() {
        let mut core = BotState::new();
        tokens_usdc_weth(&mut core);
        let aero_id = register_aerodrome(&mut core, 0xae, 1_000_000, 2_000_000);

        for (zero_for_one, token_in) in [(true, 0u8), (false, 1u8)] {
            let (hop, nonce) = project_solidly(&core, &solidly_ref(aero_id, zero_for_one)).unwrap();
            let hop0 = hop.as_solidly_state().expect("hop is SolidlyStable");
            assert_eq!(hop0.variant, DexVariant::AerodromeV2Stable);
            assert!(hop0.stable);
            // Aerodrome fee is stored as the fee fraction directly.
            assert_eq!(hop0.fee_numer, U256::from(3u64));
            assert_eq!(hop0.fee_denom, U256::from(1000u64));
            assert_eq!(hop0.reserves_0, U256::from(1_000_000u64));
            assert_eq!(hop0.reserves_1, U256::from(2_000_000u64));
            assert_eq!(hop0.decimals_0, U256::from(10u64).pow(U256::from(6u64)));
            assert_eq!(hop0.decimals_1, U256::from(10u64).pow(U256::from(18u64)));
            assert_eq!(hop0.token_in, token_in, "zero_for_one → token_in = !zfo");
            assert_eq!(
                nonce,
                core.pool_slice::<degenbot_pools::AerodromeV2PoolState>(aero_id)
                    .expect("pool state")
                    .state_nonce
            );
        }
    }

    #[test]
    fn project_solidly_builds_camelot_stable_swap_hop_with_inverted_fee() {
        // Camelot stable_swap lives in `PoolEntry::V2` (V2PoolIdentity with
        // `stable_swap=true`). Its fee is stored as the RETAINED fraction
        // `(gamma, denom)`; the projection must invert it to the fee fraction
        // `(denom - gamma, denom)` that the solidly math expects.
        let mut core = BotState::new();
        core.register_token(
            Address::from([0x01u8; 20]),
            "USD Coin".into(),
            "USDC".into(),
            6,
            1,
        );
        core.register_token(
            Address::from([0x02u8; 20]),
            "Tether".into(),
            "USDT".into(),
            6,
            1,
        );

        // gamma 9970/10000 retained (0.3% fee), both directions.
        let camelot_id = core
            .register_v2_pool(&RegisterV2PoolParams {
                address: Address::from([0xc1u8; 20]),
                token0: Address::from([0x01u8; 20]),
                token1: Address::from([0x02u8; 20]),
                reserve0: U112::from(1_000_000u64),
                reserve1: U112::from(2_000_000u64),
                fee_token0: (9970, 10000),
                fee_token1: (9970, 10000),
                factory: Address::from([0xfau8; 20]),
                update_block: 0,
                variant: DexVariant::CamelotV2Stable,
                stable_swap: true,
                fee_denominator: Some(10000),
                ..Default::default()
            })
            .expect("test setup: V2 registration");

        // token1→token0: fee comes from `fee_token1`.
        let (hop, _) = project_solidly(&core, &solidly_ref(camelot_id, false)).unwrap();
        let hop1 = hop.as_solidly_state().expect("hop is SolidlyStable");
        assert_eq!(hop1.variant, DexVariant::CamelotV2Stable);
        assert!(hop1.stable);
        assert_eq!(hop1.token_in, 1);
        // gamma 9970/10000 retained → fee = (10000-9970, 10000) = (30, 10000).
        assert_eq!(hop1.fee_numer, U256::from(30u64));
        assert_eq!(hop1.fee_denom, U256::from(10000u64));
    }

    #[test]
    fn project_solidly_missing_token_entry_is_missing_token_pair() {
        // No register_token calls: the Solidly hop cannot know its decimals
        // scale.
        let mut core = BotState::new();
        let aero_id = register_aerodrome(&mut core, 0xae, 1_000_000, 2_000_000);
        let reason = project_solidly(&core, &solidly_ref(aero_id, true)).unwrap_err();
        assert_eq!(reason, MissingHopReason::MissingTokenPair);
    }

    #[test]
    fn project_solidly_reason_qa() {
        // QA of the reachable failure → variant mapping.
        // Unregistered pool: no Aerodrome identity AND no V2 identity → the
        // fall-through arm fires → MissingIdentity.
        // MissingState (identity present, state absent) is not constructible
        // through the registration API: identity + state register atomically
        // (`register_aerodrome_pool` / `register_v2_pool`).
        let core = BotState::new();
        let reason = project_solidly(&core, &solidly_ref(987_654, true)).unwrap_err();
        assert_eq!(reason, MissingHopReason::MissingIdentity);
    }
}
