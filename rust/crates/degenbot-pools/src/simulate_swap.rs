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
use degenbot_balancer_math::fixed_point::{div_down, mul_down};
use degenbot_balancer_math::stable_math;
use degenbot_balancer_math::weighted_math;
use degenbot_balancer_math::PowVersion;
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
        PoolEntry::AerodromeV2(id, state) => {
            if amount_in.is_zero() {
                return Ok(U256::ZERO);
            }
            // Solidly volatile (constant-product) math — direct port via
            // `calc_exact_in_volatile`. The Aerodrome fee is a unidirectional
            // ``(fee_numer, fee_denom)`` fraction (the fee taken from the
            // input), matching `calc_exact_in_volatile`'s convention exactly.
            if !id.stable {
                let token_in: u8 = u8::from(!zero_for_one);
                let (fee_numer, fee_denom) = id.fee;
                return degenbot_solidly_math::calc_exact_in_volatile(
                    amount_in,
                    token_in,
                    state.reserve0.to::<U256>(),
                    state.reserve1.to::<U256>(),
                    U256::from(fee_numer),
                    U256::from(fee_denom),
                )
                .map_err(|_| SimulateSwapError::NotComputable);
            }
            // Stable mode: the Solidly stable invariant (`x^3y+y^3x >= k`)
            // needs per-token decimals, which the Aerodrome state-port
            // slice does not yet carry. The Python companion keeps doing its
            // own math via `swap_fn` until the decimals-ports slice lands,
            // so this Rust core path returns the not-yet-Rust-side sentinel.
            Ok(U256::ZERO)
        }
        PoolEntry::BalancerWeighted(id, state) => {
            simulate_balancer_weighted_swap(id, state, zero_for_one, amount_in)
        }
        PoolEntry::BalancerStable(id, state) => {
            simulate_balancer_stable_swap(id, state, zero_for_one, amount_in)
        }
        // Curve (11a): the stableswap pure math (`stableswap_get_y`) is ported
        // in `degenbot-curve-math` (slice 11c), but the full `get_dy` flow —
        // variant dispatch, rate-multiplier `xp` scaling, A precision, admin-fee
        // split — is non-trivial enough that the Python companion keeps doing
        // its own math via `swap_fn` until the dedicated wiring slice lands. This
        // Rust core path returns the not-yet-Rust-side sentinel.
        PoolEntry::Curve(..) => Ok(U256::ZERO),
    }
}

/// Balancer V2 weighted exact-input swap. Mirrors the Vault's
/// `_swapMinimalInfoGivenIn`: upscale balances + amount (mulDown), subtract
/// swap fee (mulUp on the scaled amount), compute, downscale output (divDown).
fn simulate_balancer_weighted_swap(
    id: &crate::balancer_weighted_state::BalancerWeightedPoolIdentity,
    state: &crate::balancer_weighted_state::BalancerWeightedPoolState,
    zero_for_one: bool,
    amount_in: U256,
) -> Result<U256, SimulateSwapError> {
    if amount_in.is_zero() {
        return Ok(U256::ZERO);
    }
    let (idx_in, idx_out) = if zero_for_one { (0, 1) } else { (1, 0) };
    let sf_in = id.scaling_factors[idx_in];
    let sf_out = id.scaling_factors[idx_out];
    // Fee is subtracted from the RAW amount, then upscaled (matches the
    // Python companion `BalancerV2Pool.calculate_tokens_out_from_tokens_in`).
    let amount_in_less_fee =
        weighted_math::subtract_swap_fee_amount(amount_in, U256::from(id.swap_fee))
            .map_err(|_| SimulateSwapError::NotComputable)?;
    let scaled_balance_in =
        mul_down(state.balances[idx_in], sf_in).map_err(|_| SimulateSwapError::NotComputable)?;
    let scaled_balance_out =
        mul_down(state.balances[idx_out], sf_out).map_err(|_| SimulateSwapError::NotComputable)?;
    let scaled_amount_in =
        mul_down(amount_in_less_fee, sf_in).map_err(|_| SimulateSwapError::NotComputable)?;
    let scaled_amount_out = weighted_math::calc_out_given_in(
        scaled_balance_in,
        id.weights[idx_in],
        scaled_balance_out,
        id.weights[idx_out],
        scaled_amount_in,
        PowVersion::V2,
    )
    .map_err(|_| SimulateSwapError::NotComputable)?;
    let amount_out =
        div_down(scaled_amount_out, sf_out).map_err(|_| SimulateSwapError::NotComputable)?;
    Ok(amount_out)
}

/// Balancer V2 stable exact-input swap. Mirrors the Python companion
/// `BalancerV2StablePool.calculate_tokens_out_from_tokens_in`:
///   1. Subtract swap fee from the RAW amount.
///   2. Upscale balances (drop BPT for `ComposableStable` pools) + amount.
///   3. Compute invariant per `invariant_version` (V1 roundDown `D_P`,
///      V2 roundUp `P_D`).
///   4. `calc_out_given_in` in scaled space with BPT-skipped indices.
///   5. Downscale the output (divDown).
fn simulate_balancer_stable_swap(
    id: &crate::balancer_stable_state::BalancerStablePoolIdentity,
    state: &crate::balancer_stable_state::BalancerStablePoolState,
    zero_for_one: bool,
    amount_in: U256,
) -> Result<U256, SimulateSwapError> {
    if amount_in.is_zero() {
        return Ok(U256::ZERO);
    }
    let (idx_in, idx_out) = if zero_for_one { (0, 1) } else { (1, 0) };
    let sf_in = id.scaling_factors[idx_in];
    let sf_out = id.scaling_factors[idx_out];
    // Step 1: subtract fee from the raw amount.
    let amount_in_less_fee =
        weighted_math::subtract_swap_fee_amount(amount_in, U256::from(id.swap_fee))
            .map_err(|_| SimulateSwapError::NotComputable)?;
    // Step 2: upscale balances + amount; drop BPT for ComposableStable pools.
    let upscaled_balances: Vec<U256> = state
        .balances
        .iter()
        .zip(id.scaling_factors.iter())
        .map(|(&b, &sf)| mul_down(b, sf).map_err(|_| SimulateSwapError::NotComputable))
        .collect::<Result<_, _>>()?;
    let scaled_amount_in =
        mul_down(amount_in_less_fee, sf_in).map_err(|_| SimulateSwapError::NotComputable)?;
    // Adjust indices/balances to skip the BPT token (ComposableStable).
    let (inv_balances, adj_in, adj_out) = skip_bpt(&upscaled_balances, id.bpt_idx, idx_in, idx_out);
    // Step 3: invariant per deployed version (V1 always-roundDown `D_P`,
    // V2 `P_D` with round_up).
    let amp = U256::from(id.amp);
    let invariant = if id.invariant_version == 1 {
        stable_math::calculate_invariant(amp, &inv_balances)
    } else {
        stable_math::calculate_invariant_deployed(amp, &inv_balances, true)
    }
    .map_err(|_| SimulateSwapError::NotComputable)?;
    // Step 4: outGivenIn in scaled space.
    let scaled_amount_out = stable_math::calc_out_given_in(
        amp,
        &inv_balances,
        adj_in,
        adj_out,
        scaled_amount_in,
        invariant,
    )
    .map_err(|_| SimulateSwapError::NotComputable)?;
    // Step 5: downscale.
    let amount_out =
        div_down(scaled_amount_out, sf_out).map_err(|_| SimulateSwapError::NotComputable)?;
    Ok(amount_out)
}

/// Drop the BPT entry and rebase in/out indices to the compressed list.
/// Mirrors `BalancerV2StablePool._skip_bpt_index` / `_non_bpt_indices`.
fn skip_bpt(
    balances: &[U256],
    bpt_idx: Option<usize>,
    idx_in: usize,
    idx_out: usize,
) -> (Vec<U256>, usize, usize) {
    match bpt_idx {
        None => (balances.to_vec(), idx_in, idx_out),
        Some(b) => {
            let adj = |i: usize| if i > b { i - 1 } else { i };
            let v: Vec<U256> = balances
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != b)
                .map(|(_, &x)| x)
                .collect();
            (v, adj(idx_in), adj(idx_out))
        }
    }
}
