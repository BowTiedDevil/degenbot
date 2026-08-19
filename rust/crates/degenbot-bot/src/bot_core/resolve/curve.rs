//! Curve stableswap projection.

use alloy::primitives::U256;

use degenbot_solvers::mixed::{CurveStableswapHopState, MixedPoolRef, ResolvedHop};

use super::super::BotState;
use super::MissingHopReason;

/// Curve-stableswap projection: rate-adjusted XP + the pairwise (0/1) variant
/// bytes, all read off `core` (ADR-003).
pub(crate) fn project_curve(
    core: &BotState,
    pool_ref: &MixedPoolRef,
) -> Result<(ResolvedHop, u64), MissingHopReason> {
    let id = core
        .get_curve_identity(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingIdentity)?;
    let state = core
        .get_curve_pool(pool_ref.pool_key)
        .ok_or(MissingHopReason::MissingState)?;
    if id.tokens.len() < 2 {
        return Err(MissingHopReason::TooFewTokens); // Can't form a pairwise hop
    }
    let (raw_idx_in, raw_idx_out) = if pool_ref.zero_for_one {
        (0, 1)
    } else {
        (1, 0)
    };
    // Curve constants
    let precision = U256::from(10u64).pow(U256::from(18u64));
    let fee_denom = U256::from(10u64).pow(U256::from(10u64));
    let a_precision = U256::from(100u64);
    let amp = U256::from(id.a_coefficient).saturating_mul(a_precision);
    let n_coins = U256::from(id.tokens.len() as u64);
    // Build rate-adjusted XP: xp[i] = balances[i] * rate_multipliers[i] / PRECISION
    let xp: Vec<U256> = state
        .balances
        .iter()
        .zip(id.rate_multipliers.iter())
        .map(|(b, rm)| b.saturating_mul(*rm) / precision)
        .collect();
    if raw_idx_in >= xp.len() || raw_idx_out >= xp.len() {
        return Err(MissingHopReason::OutOfRange);
    }
    let y_variant = degenbot_curve_math::stableswap::YVariant::try_from_u8(id.y_variant)
        .ok_or(MissingHopReason::UnknownVariant)?;
    let d_variant = degenbot_curve_math::stableswap::DVariant::try_from_u8(id.d_variant)
        .ok_or(MissingHopReason::UnknownVariant)?;
    Ok((
        ResolvedHop::CurveStableswap {
            state: CurveStableswapHopState {
                amp,
                a_precision,
                xp,
                token_index_in: raw_idx_in,
                token_index_out: raw_idx_out,
                n_coins,
                fee: U256::from(id.fee),
                fee_denom,
                precision,
                rate_multiplier_in: id.rate_multipliers[raw_idx_in],
                rate_multiplier_out: id.rate_multipliers[raw_idx_out],
                y_variant,
                d_variant,
            },
        },
        state.state_nonce,
    ))
}
