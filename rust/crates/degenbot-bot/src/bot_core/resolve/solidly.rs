//! Solidly-stable (Aerodrome stable / Camelot `stable_swap`) projection.
//!
//! Reads reserves + identity off the per-family `PoolEntry` arm, then fetches
//! token decimals via the token registry (never stored on the identity —
//! ADR-003 single source of truth). Dedup is ONLY the token-decimals fetch
//! shared across the two variants; the fee math stays two distinct paths
//! (Aerodrome fee-fraction direct; Camelot retained-gamma inverted to fee).

use alloy::primitives::{Address, U256};

use degenbot_solvers::mixed::{MixedPoolRef, ResolvedHop, SolidlyHopState};

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

/// Solidly-stable projection. Aerodrome identity wins when present; otherwise
/// a V2 identity (`stable_swap=true`) is the Camelot arm; neither present
/// means the pool is not a Solidly-stable pool.
pub(crate) fn project_solidly(
    core: &BotState,
    pool_ref: &MixedPoolRef,
) -> Result<(ResolvedHop, u64), MissingHopReason> {
    if let Some(id) = core.get_aerodrome_identity(pool_ref.pool_key) {
        let state = core
            .get_aerodrome_pool(pool_ref.pool_key)
            .ok_or(MissingHopReason::MissingState)?;
        let (decimals_0, decimals_1) = token_decimals(core, &id.token0, &id.token1)?;
        // Aerodrome fee is stored as the fee fraction directly
        // (cf. Camelot below).
        Ok((
            ResolvedHop::SolidlyStable {
                state: SolidlyHopState {
                    reserves_0: state.reserve0.to::<U256>(),
                    reserves_1: state.reserve1.to::<U256>(),
                    decimals_0,
                    decimals_1,
                    token_in: u8::from(!pool_ref.zero_for_one),
                    fee_numer: U256::from(id.fee.0),
                    fee_denom: U256::from(id.fee.1),
                    stable: id.stable,
                    variant: id.variant,
                },
            },
            state.state_nonce,
        ))
    } else if let Some(id) = core.get_v2_identity(pool_ref.pool_key) {
        // Camelot stable_swap path (V2PoolIdentity with
        // `stable_swap=true`).
        let state = core
            .get_v2_pool_state(pool_ref.pool_key)
            .ok_or(MissingHopReason::MissingState)?;
        let (decimals_0, decimals_1) = token_decimals(core, &id.token0, &id.token1)?;
        // Camelot stores the per-direction RETAINED fraction
        // `(gamma_numer, fee_denom)`; the solidly math takes the
        // FEE fraction, so invert: `fee_numer = denom - gamma`,
        // `fee_denom = denom`. Selected by `zero_for_one`
        // (token0 in → fee_token0; token1 in → fee_token1).
        let (gamma, denom) = if pool_ref.zero_for_one {
            id.fee_token0
        } else {
            id.fee_token1
        };
        Ok((
            ResolvedHop::SolidlyStable {
                state: SolidlyHopState {
                    reserves_0: state.reserve0.to::<U256>(),
                    reserves_1: state.reserve1.to::<U256>(),
                    decimals_0,
                    decimals_1,
                    token_in: u8::from(!pool_ref.zero_for_one),
                    fee_numer: U256::from(denom.saturating_sub(gamma)),
                    fee_denom: U256::from(denom),
                    stable: id.stable_swap,
                    variant: id.variant,
                },
            },
            state.state_nonce,
        ))
    } else {
        // Not an Aerodrome/Camelot pool → invalid.
        Err(MissingHopReason::MissingIdentity)
    }
}
