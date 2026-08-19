//! Balancer weighted-pool projection.

use alloy::primitives::U256;

use degenbot_solvers::mixed::{BalancerWeightedHopState, MixedPoolRef, ResolvedHop};

use super::super::BotState;
use super::MissingHopReason;

/// Balancer-weighted projection: upscale the pairwise balances to
/// 18-decimal fixed point (Balancer convention: the math leaf operates at
/// ONE = 1e18 scale; `scaling_factors[i] = 10^(18 - token_decimals_i)`).
pub(crate) fn project_balancer_weighted(
    core: &BotState,
    pool_ref: &MixedPoolRef,
) -> Result<(ResolvedHop, u64), MissingHopReason> {
    let id = core
        .get_balancer_weighted_identity(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingIdentity)?;
    let state = core
        .get_balancer_weighted_pool(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingState)?;
    // N-token pool: zero_for_one selects token[0]→token[1]
    // (i=0, j=1) or token[1]→token[0] (i=1, j=0). The engine
    // only handles the pairwise (0/1) case; N>2 pair selection
    // is a Python-side concern (BalancerPairView) that fixes
    // the pair before registration.
    if id.n_tokens() < 2 {
        return Err(MissingHopReason::TooFewTokens); // Can't form a pairwise hop
    }
    let (balance_in, balance_out, weight_in, weight_out, sf_in, sf_out) = if pool_ref.zero_for_one {
        (
            state.balances[0].saturating_mul(id.scaling_factors[0]),
            state.balances[1].saturating_mul(id.scaling_factors[1]),
            id.weights[0],
            id.weights[1],
            id.scaling_factors[0],
            id.scaling_factors[1],
        )
    } else {
        (
            state.balances[1].saturating_mul(id.scaling_factors[1]),
            state.balances[0].saturating_mul(id.scaling_factors[0]),
            id.weights[1],
            id.weights[0],
            id.scaling_factors[1],
            id.scaling_factors[0],
        )
    };
    let pow_version = degenbot_balancer_math::PowVersion::from_u8(id.pow_version)
        .ok_or(MissingHopReason::UnknownVariant)?; // Unknown pow_version → invalid
    Ok((
        ResolvedHop::BalancerWeighted {
            state: BalancerWeightedHopState {
                balance_in,
                balance_out,
                weight_in,
                weight_out,
                swap_fee: U256::from(id.swap_fee),
                pow_version,
                scaling_factor_in: sf_in,
                scaling_factor_out: sf_out,
            },
        },
        state.state_nonce,
    ))
}

#[cfg(test)]
mod tests {
    use super::super::MissingHopReason;
    use super::project_balancer_weighted;
    use crate::bot_core::{BotState, RegisterBalancerWeightedPoolParams};
    use alloy::primitives::{Address, U256};
    use degenbot_balancer_math::PowVersion;
    use degenbot_solvers::mixed::{HopType, MixedPoolRef, ResolvedHop};

    // -----------------------------------------------------------------
    // Per-family projection tests (T3, epic MKRKNB): `project_
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
            core.get_balancer_weighted_pool(bw_id)
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
