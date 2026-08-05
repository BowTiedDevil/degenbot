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
//! The **not-yet-Rust-side** sentinel (`Ok(U256::ZERO)`) survives only for the
//! families whose invariant math the Rust core does not yet own: Aerodrome V2
//! **stable** mode (needs per-token decimals) and non-`STANDARD` Curve swap
//! styles (crypto, live-admin, metapool `get_dy` base-pool dispatch). V2,
//! V3/V4 CL, Balancer weighted+stable, Aerodrome V2 volatile, and Curve
//! standard stableswap are all ported here — the Python companions for those
//! delegate to this core rather than doing their own math.
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
use degenbot_curve_math::stableswap::{stableswap_get_y, DVariant, YVariant};
use degenbot_v2_math::IntHopState;

use crate::registry::PoolEntry;
use crate::v3_state::{v3_simulate_swap, SimulateSwapError, V3PoolState};
use crate::v4_state::v4_simulate_swap;

// Curve stableswap structural constants (Vyper contract literal values).
/// Curve native precision scale (1e18).
const CURVE_PRECISION: u64 = 1_000_000_000_000_000_000;
/// Curve fee denominator (1e10).
const CURVE_FEE_DENOMINATOR: u64 = 10_000_000_000;

/// Exact-input swap over a [`PoolEntry`]: returns the output amount, or
/// [`SimulateSwapError::MissingTickWord(word)`] / [`NotComputable`](SimulateSwapError::NotComputable).
///
/// `Ok(U256::ZERO)` covers the non-fetchable zeros (zero amount, V2 with zero
/// reserves, and the remaining not-yet-Rust-side sentinels: Aerodrome V2 stable
/// mode and non-standard Curve swap styles). The V3/V4 arms surface a
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
        // in `degenbot-curve-math` (slice 11c). The full `get_dy` flow —
        // variant dispatch, rate-multiplier `xp` scaling, A precision, admin-fee
        // split — is non-trivial enough that the Python companion keeps doing
        // its own math via `swap_fn` until the dedicated wiring slice lands. This
        // Rust core path returns the not-yet-Rust-side sentinel.
        PoolEntry::Curve(id, state) => {
            simulate_curve_stableswap_swap(id, state, zero_for_one, amount_in)
        }
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

/// Curve standard stableswap exact-input swap.
///
/// Mirrors the Python companion `StandardStableswapCalculator.calculate` for
/// the **standard** stableswap path (`swap_style == 1`): rate-adjust balances
/// into `xp`, add the rate-scaled input, solve `get_y`, then apply
/// `dy = xp[j] - y - 1`, deduct the swap fee, and descale by the output token's
/// rate multiplier (`ConversionStyle.FEE_THEN_RATE`).
///
/// Non-`STANDARD` swap styles (crypto, live-admin, etc.) are not yet ported to
/// the Rust core; those return the `Ok(U256::ZERO)` not-yet-Rust-side sentinel
/// (the Python companion keeps doing its own math via `swap_fn` for them).
fn simulate_curve_stableswap_swap(
    id: &crate::curve_state::CurvePoolIdentity,
    state: &crate::curve_state::CurvePoolState,
    zero_for_one: bool,
    amount_in: U256,
) -> Result<U256, SimulateSwapError> {
    // Only the standard stableswap path is ported; advertise the missing ones
    // as the pre-port sentinel (matches the previous `Ok(U256::ZERO)` arm).
    if id.swap_style != 1 {
        return Ok(U256::ZERO);
    }
    if amount_in.is_zero() {
        return Ok(U256::ZERO);
    }
    let n_coins = id.n_coins();
    if n_coins < 2 {
        return Ok(U256::ZERO);
    }
    // 2-token structural convention: zero_for_one → coin (0, 1), else (1, 0).
    let (coin_in, coin_out) = if zero_for_one {
        (0usize, 1usize)
    } else {
        (1usize, 0usize)
    };

    let a_precision = U256::from(id.a_precision);
    let n_u = U256::from(n_coins);

    // Resolve A. Python `_a()` returns `a_coefficient * A_PRECISION`; the
    // stableswap `amp` is that raw product for the standard path (only
    // `VARIANT_0` divides by A_PRECISION).
    let amp = U256::from(id.a_coefficient)
        .checked_mul(a_precision)
        .ok_or(SimulateSwapError::NotComputable)?;

    // Rate-adjust balances into xp (Python: `rate * balance // PRECISION`).
    let xp: Vec<U256> = state
        .balances
        .iter()
        .zip(&id.rate_multipliers)
        .map(|(&balance, &rate_mult)| {
            rate_mult
                .checked_mul(balance)
                .ok_or(SimulateSwapError::NotComputable)
                .map(|v| v / U256::from(CURVE_PRECISION))
        })
        .collect::<Result<_, _>>()?;
    if xp.len() != n_coins {
        return Ok(U256::ZERO);
    }

    // x = xp[coin_in] + (dx * rates[coin_in] // PRECISION)
    let dx_scaled = amount_in
        .checked_mul(id.rate_multipliers[coin_in])
        .ok_or(SimulateSwapError::NotComputable)?
        / U256::from(CURVE_PRECISION);
    let x = xp[coin_in]
        .checked_add(dx_scaled)
        .ok_or(SimulateSwapError::NotComputable)?;

    // Variant dispatch: map the opaque u8 discriminants (1-based auto(); the
    // standard path defaults to STANDARD when an unrecognised/zero value is
    // carried, as the slice fixtures do).
    let y_variant = YVariant::try_from_u8(id.y_variant).unwrap_or(YVariant::Standard);
    let d_variant = DVariant::try_from_u8(id.d_variant).unwrap_or(DVariant::Standard);

    let y = stableswap_get_y(
        coin_in,
        coin_out,
        x,
        &xp,
        amp,
        n_u,
        a_precision,
        y_variant,
        d_variant,
    )
    .map_err(|_| SimulateSwapError::NotComputable)?;

    // dy = xp[coin_out] - y - 1
    let one = U256::from(1u8);
    let raw_dy = xp[coin_out]
        .checked_sub(y)
        .ok_or(SimulateSwapError::NotComputable)?;
    let raw_dy = if raw_dy.is_zero() {
        raw_dy
    } else {
        raw_dy
            .checked_sub(one)
            .ok_or(SimulateSwapError::NotComputable)?
    };

    // fee = fee * dy // FEE_DENOMINATOR; return (dy - fee) * PRECISION // rate_out
    let fee = U256::from(id.fee)
        .checked_mul(raw_dy)
        .ok_or(SimulateSwapError::NotComputable)?
        / U256::from(CURVE_FEE_DENOMINATOR);
    let dy_less_fee = raw_dy
        .checked_sub(fee)
        .ok_or(SimulateSwapError::NotComputable)?;
    let out = dy_less_fee
        .checked_mul(U256::from(CURVE_PRECISION))
        .ok_or(SimulateSwapError::NotComputable)?
        / id.rate_multipliers[coin_out];
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::skip_bpt;
    use alloy::primitives::U256;

    // Distinct magnitudes per token so a mis-rebase swapping one balance
    // so a mis-rebase that swapped one balance for another is caught). …

    fn b(i: u64) -> U256 {
        U256::from(i)
    }

    /// RPSW4Z: the MetaStable pass-through. `bpt_idx = None` leaves balances +
    /// indices untouched — the ComposableStable rebase machinery is a no-op.
    #[test]
    fn skip_bpt_metastable_none_is_passthrough() {
        let balances = [b(10), b(20)];
        let (v, idx_in, idx_out) = skip_bpt(&balances, None, 0, 1);
        assert_eq!(
            v,
            vec![b(10), b(20)],
            "Metastable balances must be unchanged"
        );
        assert_eq!(
            (idx_in, idx_out),
            (0, 1),
            "Metastable indices must be unchanged"
        );
    }

    /// RPSW4Z: ComposableStable `bpt_idx = Some(2)` (BPT at the END). The BPT
    /// is dropped and neither swap index is past `bpt_idx`, so `adj_in`/`adj_out`
    /// do NOT rebase — this is the BPT-drop branch exercisable end-to-end via
    /// `simulate_balancer_stable_swap` (covered by the parity fixture in
    /// `rust/crates/degenbot/tests/parity_balancer_stable_swap.rs`). Pinned here
    /// at the unit level so the BPT-drop + no-rebase combination is directly
    /// attributable.
    #[test]
    fn skip_bpt_composable_bpt_at_end_drops_bpt_no_rebase() {
        // [token0=10, token1=20, BPT=999] → drop index 2 → [10, 20].
        let balances = [b(10), b(20), b(999)];
        let (v, idx_in, idx_out) = skip_bpt(&balances, Some(2), 0, 1);
        assert_eq!(v, vec![b(10), b(20)], "BPT (index 2) must be dropped");
        assert_eq!(
            (idx_in, idx_out),
            (0, 1),
            "indices below bpt_idx must not rebase"
        );
    }

    /// RPSW4Z: ComposableStable `bpt_idx = Some(1)` (BPT in the MIDDLE), swap
    /// `token0 → token2` (idx_in=0, idx_out=2). `idx_out` is PAST `bpt_idx`, so
    /// it rebases to `2 - 1 = 1`; `idx_in` (0, below bpt_idx) stays `0`. The BPT
    /// at index 1 is dropped, leaving [token0, token2] with adj_in=0, adj_out=1.
    ///
    /// This is the index-rebase branch the end-to-end `simulate_swap` fixture
    /// CANNOT reach (the dispatch is `zero_for_one`-based and hardcodes token
    /// positions `0 ↔ 1`, so a `bpt_idx = 1` pool would swap token0 ↔ BPT —
    /// not a valid asset-pair swap). It is the core correctness claim of the
    /// full RPSW4Z scenario and is pinned directly here so the rebase is
    /// verified independently of the dispatch limitation. The end-to-end wiring
    /// of arbitrary (idx_in, idx_out) through `simulate_swap` is the broader
    /// VQ4OHX multi-token-API extension (sibling to `7D34LW` / `U2K6FN`).
    #[test]
    fn skip_bpt_composable_bpt_in_middle_rebases_index_past_bpt() {
        // [token0=10, BPT=999, token2=30] → drop index 1 → [10, 30].
        let balances = [b(10), b(999), b(30)];
        let (v, idx_in, idx_out) = skip_bpt(&balances, Some(1), 0, 2);
        assert_eq!(v, vec![b(10), b(30)], "BPT (index 1) must be dropped");
        assert_eq!(idx_in, 0, "idx_in (0, below bpt_idx=1) must not rebase");
        assert_eq!(
            idx_out, 1,
            "idx_out (2, past bpt_idx=1) must rebase to 2-1=1"
        );
    }

    /// RPSW4Z: symmetric rebase — `idx_in` past `bpt_idx`, `idx_out` below.
    /// `bpt_idx = Some(1)`, swap `token2 → token0` (idx_in=2, idx_out=0).
    /// Confirms the rebase applies to EITHER side, not just `idx_out`.
    #[test]
    fn skip_bpt_composable_rebases_idx_in_past_bpt() {
        let balances = [b(10), b(999), b(30)];
        let (v, idx_in, idx_out) = skip_bpt(&balances, Some(1), 2, 0);
        assert_eq!(v, vec![b(10), b(30)], "BPT (index 1) must be dropped");
        assert_eq!(idx_in, 1, "idx_in (2, past bpt_idx=1) must rebase to 2-1=1");
        assert_eq!(idx_out, 0, "idx_out (0, below bpt_idx=1) must not rebase");
    }

    /// RPSW4Z: both indices past `bpt_idx` (`bpt_idx = Some(0)`, BPT at start,
    /// swap `token1 → token2`). Both rebase by -1. Confirms the rebase is
    /// applied uniformly to both sides when both are past the BPT.
    #[test]
    fn skip_bpt_composable_rebases_both_indices_past_bpt() {
        // [BPT=999, token1=20, token2=30] → drop index 0 → [20, 30].
        let balances = [b(999), b(20), b(30)];
        let (v, idx_in, idx_out) = skip_bpt(&balances, Some(0), 1, 2);
        assert_eq!(v, vec![b(20), b(30)], "BPT (index 0) must be dropped");
        assert_eq!(
            (idx_in, idx_out),
            (0, 1),
            "both past bpt_idx=0 rebase by -1"
        );
    }
}
