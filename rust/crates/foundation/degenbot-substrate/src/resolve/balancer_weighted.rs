//! Balancer weighted-pool resolve row — the registry-lookup + solver-vocabulary
//! wrap seam (ADR-073 D2).
//!
//! The projection itself is the family read module's door —
//! `degenbot_pools::families::balancer_weighted::capture_hop` — a pure
//! function of the family state slice (pairwise upscale to 18-decimal fixed
//! point + pow-version decode). This file keeps ONLY the state-owner seam:
//! the registry lookups, the refusal mapping, the nonce, and the one
//! `ResolvedHop` wrap.

use degenbot_pools::families::balancer_weighted as weighted_read;
use degenbot_pools::solve::{BalancerWeightedHopState, MixedPoolRef, ResolvedHop};

use super::super::BotState;
use super::MissingHopReason;

/// Balancer-weighted resolve row: read pool state + identity off `core`
/// (ADR-003), then the family read module's [`weighted_read::capture_hop`]
/// door derives the upscaled pairwise hop payload the weighted solve branch
/// consumes. The row wraps the solver-vocabulary hop
/// (`ResolvedHop::BalancerWeighted`).
pub(crate) fn project_balancer_weighted(
    core: &BotState,
    pool_ref: &MixedPoolRef,
) -> Result<(ResolvedHop, u64), MissingHopReason> {
    let id = core
        .pool_slice::<degenbot_pools::BalancerWeightedPoolIdentity>(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingIdentity)?;
    let state = core
        .pool_slice::<degenbot_pools::BalancerWeightedPoolState>(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingState)?;
    let hop = weighted_read::capture_hop(state, id, pool_ref.zero_for_one)
        .map_err(balancer_weighted_refusal)?;
    Ok((
        ResolvedHop::BalancerWeighted {
            state: BalancerWeightedHopState {
                balance_in: hop.balance_in,
                balance_out: hop.balance_out,
                weight_in: hop.weight_in,
                weight_out: hop.weight_out,
                swap_fee: hop.swap_fee,
                pow_version: hop.pow_version,
                scaling_factor_in: hop.scaling_factor_in,
                scaling_factor_out: hop.scaling_factor_out,
            },
        },
        state.state_nonce,
    ))
}

/// Map the family read module's capture refusal onto the resolve
/// invalidation vocabulary — the row's one mapping site.
fn balancer_weighted_refusal(refusal: weighted_read::CaptureRefusal) -> MissingHopReason {
    match refusal {
        weighted_read::CaptureRefusal::TooFewTokens => MissingHopReason::TooFewTokens,
        weighted_read::CaptureRefusal::UnknownVariant => MissingHopReason::UnknownVariant,
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::super::MissingHopReason;
    use super::project_balancer_weighted;
    use crate::{BotState, RegisterBalancerWeightedPoolParams};
    use alloy::primitives::{Address, U256};
    use degenbot_math::balancer::PowVersion;
    use degenbot_pools::solve::{HopType, MixedPoolRef, ResolvedHop};

    // -----------------------------------------------------------------
    // Per-family projection tests: `project_
    // balancer_weighted` crosses the seam directly — 18-decimal upscaling
    // + pairwise (0/1) selection + pow-version decode included.
    // -----------------------------------------------------------------

    fn bw_ref(pool_key: u64, zero_for_one: bool) -> MixedPoolRef {
        MixedPoolRef {
            hop_type: HopType::BalancerWeighted,
            pool_key,
            zero_for_one,
        }
    }

    fn register_balancer_weighted(
        core: &mut BotState,
        addr_byte: u8,
        balance0: u128,
        balance1: u128,
        pow_version: u8,
    ) -> u64 {
        core.register_balancer_weighted_pool(&RegisterBalancerWeightedPoolParams {
            address: Address::from([addr_byte; 20]),
            vault: Address::repeat_byte(0xba),
            pool_id: [0u8; 32],
            tokens: vec![Address::repeat_byte(0x01), Address::repeat_byte(0x02)],
            weights: vec![
                U256::from(500_000_000_000_000_000u128),
                U256::from(500_000_000_000_000_000u128),
            ],
            scaling_factors: vec![U256::from(1u64), U256::from(1u64)],
            swap_fee: 1_000_000_000_000_000u128, // 0.1% of 1e18
            pow_version,
            balances: vec![
                U256::from(balance0) * U256::from(10u64).pow(U256::from(18u64)),
                U256::from(balance1) * U256::from(10u64).pow(U256::from(18u64)),
            ],
            update_block: 0,
        })
        .expect("balancer weighted registration")
    }

    #[test]
    fn project_balancer_weighted_builds_hop_with_upscaled_pairwise_state() {
        let mut core = BotState::new();
        let bw_id = register_balancer_weighted(&mut core, 0xe1, 1000, 2000, 2);

        let (hop, _) = project_balancer_weighted(&core, &bw_ref(bw_id, true)).unwrap();
        let ResolvedHop::BalancerWeighted { state: s } = hop else {
            panic!("hop is not BalancerWeighted");
        };
        let one_e18 = U256::from(10u64).pow(U256::from(18u64));
        // sf = [1, 1]: balances pass through at 18-decimal scale.
        assert_eq!(s.balance_in, U256::from(1000u64) * one_e18);
        assert_eq!(s.balance_out, U256::from(2000u64) * one_e18);
        assert_eq!(s.weight_in, U256::from(500_000_000_000_000_000u128));
        assert_eq!(s.weight_out, U256::from(500_000_000_000_000_000u128));
        assert_eq!(s.swap_fee, U256::from(1_000_000_000_000_000u64));
        assert_eq!(s.pow_version, PowVersion::from_u8(2).expect("pow 2"));

        // Orientation flips the pairwise (0/1) selection.
        let (hop, nonce) = project_balancer_weighted(&core, &bw_ref(bw_id, false)).unwrap();
        let ResolvedHop::BalancerWeighted { state: s } = hop else {
            panic!("hop is not BalancerWeighted");
        };
        assert_eq!(s.balance_in, U256::from(2000u64) * one_e18);
        assert_eq!(s.balance_out, U256::from(1000u64) * one_e18);
        assert_eq!(
            nonce,
            core.pool_slice::<degenbot_pools::BalancerWeightedPoolState>(bw_id)
                .expect("state")
                .state_nonce
        );
    }

    #[test]
    fn project_balancer_weighted_unknown_pow_version_is_unknown_variant() {
        let mut core = BotState::new();
        let bw_id = register_balancer_weighted(&mut core, 0xe2, 1000, 2000, 9);
        let reason = project_balancer_weighted(&core, &bw_ref(bw_id, true)).unwrap_err();
        assert_eq!(reason, MissingHopReason::UnknownVariant);
    }

    #[test]
    fn project_balancer_weighted_reason_qa() {
        // QA of the reachable failure -> variant mapping.
        // Unregistered pool: identity is checked FIRST in this arm ->
        // MissingIdentity. MissingState (state absent, identity present) is
        // not constructible: registration is atomic.
        let core = BotState::new();
        let reason = project_balancer_weighted(&core, &bw_ref(999_999, true)).unwrap_err();
        assert_eq!(reason, MissingHopReason::MissingIdentity);
    }
}
