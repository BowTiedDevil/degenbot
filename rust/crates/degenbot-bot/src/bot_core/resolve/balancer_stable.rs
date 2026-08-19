//! Balancer stable-pool projection.

use alloy::primitives::U256;

use degenbot_solvers::mixed::{BalancerStableHopState, MixedPoolRef, ResolvedHop};

use super::super::BotState;
use super::MissingHopReason;

/// Balancer-stable projection: pairwise (0/1) hop over the BPT-skipped,
/// 18-decimal-upscaled balances with the pre-computed `invariant`.
pub(crate) fn project_balancer_stable(
    core: &BotState,
    pool_ref: &MixedPoolRef,
) -> Result<(ResolvedHop, u64), MissingHopReason> {
    let id = core
        .get_balancer_stable_identity(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingIdentity)?;
    let state = core
        .get_balancer_stable_pool(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingState)?;
    if id.n_tokens() < 2 {
        return Err(MissingHopReason::TooFewTokens); // Can't form a pairwise hop
    }
    let (raw_idx_in, raw_idx_out) = if pool_ref.zero_for_one {
        (0, 1)
    } else {
        (1, 0)
    };
    let skip_bpt = |idx: usize| -> usize {
        match id.bpt_idx {
            Some(bpt) if idx >= bpt => idx - 1,
            _ => idx,
        }
    };
    let token_index_in = skip_bpt(raw_idx_in);
    let token_index_out = skip_bpt(raw_idx_out);
    let upscaled_balances: Vec<U256> = {
        let mut ub = Vec::with_capacity(id.n_tokens());
        for (i, &bal) in state.balances.iter().enumerate() {
            if id.bpt_idx.is_some_and(|bpt| bpt == i) {
                continue;
            }
            ub.push(bal.saturating_mul(id.scaling_factors[i]));
        }
        ub
    };
    if token_index_in >= upscaled_balances.len() || token_index_out >= upscaled_balances.len() {
        return Err(MissingHopReason::OutOfRange);
    }
    let amp_u256 = U256::from(id.amp);
    let invariant = if id.invariant_version == 1 {
        degenbot_balancer_math::stable_math::calculate_invariant(amp_u256, &upscaled_balances)
    } else {
        degenbot_balancer_math::stable_math::calculate_invariant_deployed(
            amp_u256,
            &upscaled_balances,
            true,
        )
    };
    let invariant = invariant.map_err(|_| MissingHopReason::InvariantError)?;
    Ok((
        ResolvedHop::BalancerStable {
            state: BalancerStableHopState {
                amp: amp_u256,
                balances: upscaled_balances,
                token_index_in,
                token_index_out,
                invariant,
                swap_fee: U256::from(id.swap_fee),
                scaling_factor_in: id.scaling_factors[raw_idx_in],
                scaling_factor_out: id.scaling_factors[raw_idx_out],
            },
        },
        state.state_nonce,
    ))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::super::MissingHopReason;
    use super::project_balancer_stable;
    use crate::bot_core::{BotState, RegisterBalancerStablePoolParams};
    use alloy::primitives::{Address, U256};
    use degenbot_solvers::mixed::{HopType, MixedPoolRef, ResolvedHop};

    fn bs_ref(pool_key: u64, zero_for_one: bool) -> MixedPoolRef {
        MixedPoolRef {
            hop_type: HopType::BalancerStable,
            pool_key,
            zero_for_one,
        }
    }

    fn register_balancer_stable(
        core: &mut BotState,
        addr_byte: u8,
        balance0: u128,
        balance1: u128,
    ) -> u64 {
        let one_e18 = U256::from(10u64).pow(U256::from(18u64));
        core.register_balancer_stable_pool(&RegisterBalancerStablePoolParams {
            address: Address::from([addr_byte; 20]),
            vault: Address::repeat_byte(0xba),
            pool_id: [0u8; 32],
            tokens: vec![Address::repeat_byte(0x01), Address::repeat_byte(0x02)],
            // amp=200_000 = raw_amp(200) * AMP_PRECISION(1000).
            amp: 200_000,
            scaling_factors: vec![U256::from(1u64), U256::from(1u64)],
            swap_fee: 10_000_000_000_000u128, // 0.01% of 1e18
            bpt_idx: None,
            invariant_version: 2,
            balances: vec![
                U256::from(balance0) * one_e18,
                U256::from(balance1) * one_e18,
            ],
            update_block: 0,
            rate_provider: None,
        })
    }

    #[test]
    fn project_balancer_stable_builds_hop_with_invariant() {
        let mut core = BotState::new();
        let bs_id = register_balancer_stable(&mut core, 0xe1, 1000, 2000);

        let (hop, _) = project_balancer_stable(&core, &bs_ref(bs_id, true)).unwrap();
        let ResolvedHop::BalancerStable { state: s } = hop else {
            panic!("hop is not BalancerStable");
        };
        let one_e18 = U256::from(10u64).pow(U256::from(18u64));
        assert_eq!(s.amp, U256::from(200_000u64));
        assert_eq!(s.token_index_in, 0);
        assert_eq!(s.token_index_out, 1);
        assert_eq!(
            s.balances,
            vec![U256::from(1000u64) * one_e18, U256::from(2000u64) * one_e18]
        );
        assert!(s.invariant > U256::ZERO, "invariant of positive balances");
        assert_eq!(s.swap_fee, U256::from(10_000_000_000_000u64));

        // Orientation flips the pairwise indices.
        let (hop, nonce) = project_balancer_stable(&core, &bs_ref(bs_id, false)).unwrap();
        let ResolvedHop::BalancerStable { state: s } = hop else {
            panic!("hop is not BalancerStable");
        };
        assert_eq!(s.token_index_in, 1);
        assert_eq!(s.token_index_out, 0);
        assert_eq!(
            nonce,
            core.get_balancer_stable_pool(bs_id)
                .expect("state")
                .state_nonce
        );
    }

    #[test]
    fn project_balancer_stable_reason_qa() {
        // QA of the reachable failure -> variant mapping.
        // Unregistered pool: identity is checked FIRST in this arm ->
        // MissingIdentity.
        // Not constructible via the registration API (documented here so the
        // QA stays honest): TooFewTokens (registration validates the vector
        // shapes), OutOfRange (the bpt-skip bounds track the upscaled length
        // for a well-formed pool), InvariantError (the invariant math does
        // not err on sane positive balances). Those three remain the
        // defensive mappings for malformed or future state.
        let core = BotState::new();
        let reason = project_balancer_stable(&core, &bs_ref(888_888, true)).unwrap_err();
        assert_eq!(reason, MissingHopReason::MissingIdentity);
    }
}
