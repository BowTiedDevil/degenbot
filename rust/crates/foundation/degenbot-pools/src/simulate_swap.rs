//! Stateless swap-simulator dispatch — computes a swap outcome purely from
//! in-memory pool state (V2 constant-product, V3/V4 concentrated-liquidity),
//! with no chain / registry / async / tokio.
//!
//! Returns [`SimulateSwapError::MissingTickWord(word)`] as **data** for V3/V4
//! sparse-map misses (Pattern B); the fetch+retry shell lives in `degenbot-bot`
//! (it catches the value error + calls the pools-defined `TickWordFetcher`
//! trait to fill the word, then retries).
//!
//! The **not-yet-Rust-side** sentinel (`Ok(U256::ZERO)`) survives only for the
//! families whose invariant math the Rust core does not yet own: non-`STANDARD`
//! Curve swap styles (crypto, live-admin, metapool `get_dy` base-pool dispatch).
//! V2, V3/V4 CL, Balancer weighted+stable, Aerodrome V2 volatile **and** stable,
//! and Curve standard stableswap are all ported — the Python companions
//! for those delegate to this core rather than doing their own math. The
//! per-family design arms live behind the family read modules' read doors
//! (ADR-073 D1); this dispatch keeps ONE row per family and no family math.
//!
//! **Relocated** (the value core) from
//! `degenbot-bot/src/bot_core/mod.rs::BotState::calculate_tokens_out_miss_aware`;
//! the `BotState` method is now a thin shell that resolves the `pool_id` →
//! `&PoolEntry` then delegates here.

use alloy::primitives::{I256, U256};
use degenbot_math::solidly;

use crate::families::concentrated_liquidity::{
    ConcentratedLiquidityState, DesignScalars, FetchMode, SwapReadOutcome, SwapReadRequest,
    SwapReadSource,
};
use crate::families::{
    balancer_stable, balancer_weighted, concentrated_liquidity, curve_stableswap, reserve_pair,
};
use crate::registry::PoolEntry;
use crate::v3_state::SimulateSwapError;

/// Exact-input swap over a [`PoolEntry`]: returns the output amount, or
/// [`SimulateSwapError::MissingTickWord(word)`] / [`NotComputable`](SimulateSwapError::NotComputable).
///
/// `Ok(U256::ZERO)` covers the non-fetchable zeros (zero amount, V2 with zero
/// reserves, and the remaining not-yet-Rust-side sentinel: non-standard Curve
/// swap styles). The V3/V4 arms surface a
/// sparse-map miss as [`SimulateSwapError::MissingTickWord`] (the caller —
/// [`BotState::calculate_tokens_out_with_fetch`] in `degenbot-bot` — fetches +
/// retries); arithmetic overflow / non-positive amount yields
/// [`SimulateSwapError::NotComputable`].
///
/// # Errors
///
/// Returns [`SimulateSwapError::MissingTickWord(word)`] when a V3/V4 sparse
/// pool's walk enters an unfetched tick-bitmap word (the caller fetches +
/// retries), or [`SimulateSwapError::NotComputable`] on arithmetic overflow /
/// invariant violation / non-positive amount.
pub fn simulate_swap(
    entry: &PoolEntry,
    zero_for_one: bool,
    amount_in: U256,
) -> Result<U256, SimulateSwapError> {
    match entry {
        // Reserve-pair rows (ADR-073 D2): the constant-product read + the
        // orientation/fee conventions live behind the family read module's
        // read_swap door (the zero-amount sentinel rides with the arm).
        PoolEntry::V2(p) => reserve_pair::read_swap(
            reserve_pair::ReservePairState::V2 {
                identity: &p.0,
                state: &p.1,
            },
            reserve_pair::SwapReadRequest::ExactIn {
                zero_for_one,
                amount_in,
            },
        ),
        // V3 concentrated-liquidity math. Exact-input swap: the unsigned
        // amount enters as a canonical NEGATIVE request (exact input); the
        // design's fee + sign conventions live behind the family read
        // module's door. Output is token1 for zfo, token0 for ofz (matches
        // the V3 Swap callback: zfo pays token0, receives token1).
        PoolEntry::V3(p) => {
            let (identity, state) = (&p.0, &p.1);
            if amount_in.is_zero() {
                return Ok(U256::ZERO);
            }
            let Ok(spec) = I256::try_from(amount_in) else {
                return Err(SimulateSwapError::NotComputable);
            };
            cl_stateless_row(
                ConcentratedLiquidityState::V3 {
                    state,
                    scalars: DesignScalars::new(identity.fee, identity.tick_spacing),
                },
                zero_for_one,
                -spec,
            )
        }
        // V4 concentrated-liquidity math. Same CL math as V3, one dispatch
        // row: the V4-native sign convention (`amountSpecified < 0` for
        // exact input) is the design mapping behind the door — the canonical
        // request is the SAME negative exact-input amount.
        PoolEntry::V4(p) => {
            let (identity, state) = (&p.0, &p.1);
            if amount_in.is_zero() {
                return Ok(U256::ZERO);
            }
            let Ok(spec) = I256::try_from(amount_in) else {
                return Err(SimulateSwapError::NotComputable);
            };
            cl_stateless_row(
                ConcentratedLiquidityState::V4 {
                    state,
                    scalars: DesignScalars::new(
                        identity.pool_key.fee,
                        identity.pool_key.tick_spacing,
                    ),
                    hooks: identity.pool_key.hooks,
                },
                zero_for_one,
                -spec,
            )
        }
        PoolEntry::AerodromeV2(p) => {
            let (id, state) = (&p.0, &p.1);
            // Volatile mode: the reserve-pair family's read door (the
            // fee-fraction convention + the zero-amount sentinel live there).
            if !id.stable {
                return reserve_pair::read_swap(
                    reserve_pair::ReservePairState::Aerodrome {
                        identity: id,
                        state,
                    },
                    reserve_pair::SwapReadRequest::ExactIn {
                        zero_for_one,
                        amount_in,
                    },
                );
            }
            // Stable mode: Solidly stable invariant (`x^3y + y^3x >= k`)
            // via `calc_exact_in_stable_solidly` (decimals carried on identity).
            // The Solidly family's read arm — not the reserve-pair module's.
            simulate_aerodrome_stable_swap(id, state, zero_for_one, amount_in)
        }
        // One balance-vector row per family (ADR-073 D2): the exact-input
        // read goes through the family read module's read_swap door — the
        // invariant math, fee conventions, and variant dispatch all live
        // behind the doors.
        PoolEntry::BalancerWeighted(p) => balancer_weighted::read_swap(
            &p.1,
            &p.0,
            balancer_weighted::SwapReadRequest::ExactIn {
                zero_for_one,
                amount_in,
            },
        ),
        PoolEntry::BalancerStable(p) => balancer_stable::read_swap(
            &p.1,
            &p.0,
            balancer_stable::SwapReadRequest::ExactIn {
                zero_for_one,
                amount_in,
            },
        ),
        PoolEntry::Curve(p) => curve_stableswap::read_swap(&p.1, &p.0, zero_for_one, amount_in),
    }
}

/// The ONE concentrated-liquidity dispatch row of the stateless swap
/// dispatch (ADR-073 D2): the arm hands the family read module the design's
/// state slice, and the V3/V4 fee + sign conventions live behind the
/// [`concentrated_liquidity::read_swap`] door. Stateless Pattern-B contract
/// preserved: a sparse-map miss surfaces as
/// [`SimulateSwapError::MissingTickWord`] data for the caller's fetch + retry
/// shell.
fn cl_stateless_row(
    design: ConcentratedLiquidityState<'_>,
    zero_for_one: bool,
    amount_specified: alloy::primitives::I256,
) -> Result<U256, SimulateSwapError> {
    match concentrated_liquidity::read_swap(
        SwapReadSource::Shared(design),
        SwapReadRequest {
            zero_for_one,
            amount_specified,
            sqrt_price_limit: None,
        },
        FetchMode::Stateless,
    ) {
        SwapReadOutcome::Stateless(Ok(outcome)) => Ok(if zero_for_one {
            outcome.amount1
        } else {
            outcome.amount0
        }),
        SwapReadOutcome::Stateless(Err(err)) => Err(err),
        _ => unreachable!("stateless read returns the stateless verdict"),
    }
}

/// Aerodrome V2 **stable**-mode exact-input swap (Solidly invariant
/// `x^3*y + y^3*x >= k`) via `calc_exact_in_stable_solidly`.
///
/// The identity carries token0/token1 decimal counts as `u8`; they are
/// converted to the `10**decimals` scale factors the Solidly math expects
/// (mirrors the Python `AerodromeV2Pool` companion, which uses
/// `10**token.decimals`).
fn simulate_aerodrome_stable_swap(
    id: &crate::aerodrome_v2_state::AerodromeV2PoolIdentity,
    state: &crate::aerodrome_v2_state::AerodromeV2PoolState,
    zero_for_one: bool,
    amount_in: U256,
) -> Result<U256, SimulateSwapError> {
    if amount_in.is_zero() {
        return Ok(U256::ZERO);
    }
    let token_in: u8 = u8::from(!zero_for_one);
    let (fee_numer, fee_denom) = id.fee;
    solidly::calc_exact_in_stable_solidly(
        amount_in,
        token_in,
        state.reserve0.to::<U256>(),
        state.reserve1.to::<U256>(),
        U256::from(10u64).pow(U256::from(id.token0_decimals)),
        U256::from(10u64).pow(U256::from(id.token1_decimals)),
        U256::from(fee_numer),
        U256::from(fee_denom),
    )
    .map_err(|_| SimulateSwapError::NotComputable)
}
