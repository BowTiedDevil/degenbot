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
