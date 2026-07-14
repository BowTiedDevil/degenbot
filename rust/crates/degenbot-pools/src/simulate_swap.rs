//! Stateless swap-simulator dispatch — computes a swap outcome purely from
//! in-memory pool state (V2 constant-product, V3/V4 concentrated-liquidity),
//! with no chain / registry / async / tokio.
//!
//! Returns [`SimulateSwapError::MissingTickWord(word)`] as **data** for V3/V4
//! sparse-map misses (Pattern B); the fetch+retry shell lives in `degenbot-bot`
//! (it catches the value error + calls the pools-defined `TickWordFetcher`
//! trait to fill the word, then retries).
#![allow(clippy::doc_markdown)]
//!
//! Curve / Balancer / AerodromeV2 return `Ok(U256::ZERO)` — their invariant
//! math is not yet ported to the Rust core (the Python companions keep doing
//! their own math via `swap_fn`); this mirrors the prior "not-yet-Rust-side"
//! sentinel.
//!
//! **Relocated** (the value core) from
//! `degenbot-bot/src/bot_core/mod.rs::BotState::calculate_tokens_out_miss_aware`;
//! the `BotState` method is now a thin shell that resolves the `pool_id` →
//! `&PoolEntry` then delegates here.

use alloy::primitives::{I256, U256};
use degenbot_v2_math::IntHopState;

use crate::registry::PoolEntry;
use crate::v3_state::{v3_simulate_swap, SimulateSwapError, V3PoolState};
use crate::v4_state::v4_simulate_swap;

/// Exact-input swap over a [`PoolEntry`]: returns the output amount, or
/// [`SimulateSwapError::MissingTickWord(word)`] / [`NotComputable`](SimulateSwapError::NotComputable).
///
/// `Ok(U256::ZERO)` covers the non-fetchable zeros (zero amount, V2 with zero
/// reserves, Curve/Balancer/AerodromeV2 sentinels). The V3/V4 arms surface a
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
        PoolEntry::V2(identity, state) => {
            if amount_in.is_zero() {
                return Ok(U256::ZERO);
            }
            let (reserve_in, reserve_out, gamma_numer, fee_denom) = if zero_for_one {
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
            let hop = IntHopState::new(reserve_in, reserve_out, gamma_numer, fee_denom);
            hop.swap(amount_in)
                .map_err(|_| SimulateSwapError::NotComputable)
        }
        // V3 concentrated-liquidity math. Exact-input swap: amount_specified
        // > 0 (V3 convention). Output is token1 for zfo, token0 for ofz
        // (matches the V3 Swap callback: zfo pays token0, receives token1).
        PoolEntry::V3(identity, state) => {
            if amount_in.is_zero() {
                return Ok(U256::ZERO);
            }
            let Some(spec) = I256::try_from(amount_in).ok() else {
                return Err(SimulateSwapError::NotComputable);
            };
            let outcome = v3_simulate_swap(
                state,
                identity.fee,
                identity.tick_spacing,
                zero_for_one,
                spec,
                V3PoolState::default_sqrt_price_limit(zero_for_one),
            )?;
            Ok(if zero_for_one {
                outcome.amount1
            } else {
                outcome.amount0
            })
        }
        // V4 concentrated-liquidity math. Same CL math as V3; sign
        // convention: V4 exact-input is `amountSpecified < 0` (negative),
        // opposite to V3. The caller flips so the simulator sees the
        // V4-native sign.
        PoolEntry::V4(identity, state) => {
            if amount_in.is_zero() {
                return Ok(U256::ZERO);
            }
            let Some(spec) = I256::try_from(amount_in).ok() else {
                return Err(SimulateSwapError::NotComputable);
            };
            let outcome = v4_simulate_swap(
                state,
                identity.pool_key.fee,
                identity.pool_key.tick_spacing,
                zero_for_one,
                -spec,
                V3PoolState::default_sqrt_price_limit(zero_for_one),
            )?;
            Ok(if zero_for_one {
                outcome.amount1
            } else {
                outcome.amount0
            })
        }
        // Curve (11a) + Balancer weighted (12a) + Balancer stable (12c): the
        // stableswap / weighted-product / stable-invariant math is NOT ported in
        // their state-port sub-slices. The Python companions keep doing their
        // own math via `DyCalculator` / `WeightedMath` / `StableMath` through
        // the `swap_fn` returned by `to_hop_state`; this Rust core path
        // returns 0 (the "not-yet-Rust-side" sentinel — same as an
        // unregistered pool). Curve ported in 11c; Balancer weighted stable in 12e.
        PoolEntry::Curve(..)
        | PoolEntry::BalancerWeighted(..)
        | PoolEntry::BalancerStable(..)
        | PoolEntry::AerodromeV2(..) => Ok(U256::ZERO),
    }
}
