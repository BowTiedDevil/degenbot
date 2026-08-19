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
