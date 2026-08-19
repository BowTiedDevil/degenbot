//! V2 (reserve-pair family) projection.

use alloy::primitives::U256;

use degenbot_solvers::mixed::{MixedPoolRef, ResolvedHop};

use super::super::BotState;
use super::MissingHopReason;

/// V2 projection: read the reserve-pair state + identity off `core` (ADR-003)
/// and build the orientation-specific `IntHopState` at resolve time from
/// `zero_for_one` (ADR-003 "Swap Orientation": single `PoolEntry` per address,
/// orientation derived at solve — the engine never mutates this state).
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
    let (reserve_in, reserve_out, gamma_numer, fee_denom) = if pool_ref.zero_for_one {
        (
            state.reserve0.to::<U256>(),
            state.reserve1.to::<U256>(),
            identity.fee_token0.0,
            identity.fee_token0.1,
        )
    } else {
        (
            state.reserve1.to::<U256>(),
            state.reserve0.to::<U256>(),
            identity.fee_token1.0,
            identity.fee_token1.1,
        )
    };
    let hop_state =
        degenbot_v2_math::IntHopState::new(reserve_in, reserve_out, gamma_numer, fee_denom);
    Ok((ResolvedHop::V2 { state: hop_state }, state.state_nonce))
}
