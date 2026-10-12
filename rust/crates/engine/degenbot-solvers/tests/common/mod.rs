//! Shared hop builders for the golden-arm regression binaries
//! (`solidly_golden_parity`, `stable_golden_drift_net`).
//!
//! Each integration-test binary compiles this module separately (`pub mod
//! common;` at its crate root — the standard Rust sharing pattern). The
//! builders construct [`ResolvedHop`]/[`ResolvedMixedPath`] states directly,
//! so the paths they build are byte-identical to the pre-extraction inline
//! definitions the drift-net snapshots were captured from. The module is
//! `pub` so the per-binary dead-code lint stays quiet for a builder one
//! binary does not exercise (each binary would otherwise flag its neighbours).

use alloy::primitives::U256;
use degenbot_math::balancer::constants::PowVersion;
use degenbot_math::curve::stableswap::{DVariant, YVariant};
use degenbot_math::v2::IntHopState;
use degenbot_solvers::mixed::{
    BalancerStableHopState, BalancerWeightedHopState, CurveStableswapHopState, ResolvedHop,
    ResolvedMixedPath, SolidlyHopState,
};
use degenbot_uniswap::dex_identity::DexVariant;

/// 18-decimal scale: the token decimals scale for Solidly hops and the
/// precision/rate unit for the Curve hop.
pub const ONE_E18: u128 = 1_000_000_000_000_000_000;

/// Wrap hops into a resolved path at the resolved-state convention (all
/// nonces `1`, valid, block 0) — the shared intake shape the corpora build.
#[must_use]
pub fn path(hops: Vec<ResolvedHop>) -> ResolvedMixedPath {
    let nonces = vec![1u64; hops.len()];
    ResolvedMixedPath {
        hops,
        valid: true,
        state_nonces: nonces,
        max_update_block: 0,
    }
}

/// A constant-product hop: reserves are already oriented (in, out).
#[must_use]
pub fn v2_hop(
    reserve_in: u128,
    reserve_out: u128,
    gamma_numer: u64,
    fee_denom: u64,
) -> ResolvedHop {
    ResolvedHop::V2 {
        state: IntHopState::new(
            U256::from(reserve_in),
            U256::from(reserve_out),
            gamma_numer,
            fee_denom,
        ),
    }
}

/// A Solidly/Aerodrome stable hop. `scale_*` are the token decimal scales
/// (`10^decimals`), the divisor the leaf normalizes reserves by; `token_in`
/// is the leaf's un-oriented direction flag.
#[must_use]
pub fn solidly_hop(
    reserves_0: u128,
    reserves_1: u128,
    scale_0: u128,
    scale_1: u128,
    token_in: u8,
    fee_numer: u64,
    fee_denom: u64,
) -> ResolvedHop {
    ResolvedHop::SolidlyStable {
        state: SolidlyHopState {
            reserves_0: U256::from(reserves_0),
            reserves_1: U256::from(reserves_1),
            decimals_0: U256::from(scale_0),
            decimals_1: U256::from(scale_1),
            token_in,
            fee_numer: U256::from(fee_numer),
            fee_denom: U256::from(fee_denom),
            stable: true,
            variant: DexVariant::AerodromeV2Stable,
        },
    }
}

/// A balanced 50/50 Balancer weighted hop. `reverse` traverses it output→input;
/// equal balances make the direction numerically irrelevant.
#[must_use]
pub fn weighted_hop(base: u128, swap_fee: u128, reverse: bool) -> ResolvedHop {
    let half = U256::from(ONE_E18) / U256::from(2u64);
    let _ = reverse;
    ResolvedHop::BalancerWeighted {
        state: BalancerWeightedHopState {
            balance_in: U256::from(base),
            balance_out: U256::from(base),
            weight_in: half,
            weight_out: half,
            swap_fee: U256::from(swap_fee),
            pow_version: PowVersion::V2,
            scaling_factor_in: U256::from(1u64),
            scaling_factor_out: U256::from(1u64),
        },
    }
}

/// A balanced Balancer stable hop with its invariant pre-computed at the
/// resolved-state convention.
///
/// # Panics
///
/// Panics if `calculate_invariant_deployed` fails to converge; the balanced
/// positive balances here always converge.
#[must_use]
pub fn stable_hop(base: u128, swap_fee: u128, reverse: bool) -> ResolvedHop {
    let balances = vec![U256::from(base), U256::from(base)];
    let amp = U256::from(200_000u64);
    let invariant =
        degenbot_math::balancer::stable_math::calculate_invariant_deployed(amp, &balances, true)
            .expect("balanced positive balances converge");
    ResolvedHop::BalancerStable {
        state: BalancerStableHopState {
            amp,
            balances,
            token_index_in: usize::from(reverse),
            token_index_out: usize::from(!reverse),
            invariant,
            swap_fee: U256::from(swap_fee),
            scaling_factor_in: U256::from(1u64),
            scaling_factor_out: U256::from(1u64),
        },
    }
}

/// A balanced two-coin Curve stableswap hop (identity rates, standard
/// variants).
#[must_use]
pub fn curve_hop(base: u128, fee: u128, reverse: bool) -> ResolvedHop {
    let precision = U256::from(ONE_E18);
    ResolvedHop::CurveStableswap {
        state: CurveStableswapHopState {
            amp: U256::from(1_000u64),
            a_precision: U256::from(100u64),
            xp: vec![U256::from(base), U256::from(base)],
            token_index_in: usize::from(reverse),
            token_index_out: usize::from(!reverse),
            n_coins: U256::from(2u64),
            fee: U256::from(fee),
            fee_denom: U256::from(10_000_000_000u64),
            precision,
            rate_multiplier_in: precision,
            rate_multiplier_out: precision,
            y_variant: YVariant::Standard,
            d_variant: DVariant::Standard,
        },
    }
}

/// The smallest native reserve whose scaled value clears the Solidly leaf's
/// faithful-arithmetic floor. The leaf normalizes `reserve * ONE / scale`, so
/// a native reserve of `floor * scale / ONE` yields a scaled reserve at the
/// floor — below it the leaf's integer `get_y` degrades and the corpus would
/// measure arithmetic noise instead of the solver's optimality.
#[must_use]
pub fn faithful_base(scale: u128) -> u128 {
    let scaled_floor = 4u128 << 60;
    (scaled_floor.saturating_mul(scale) / ONE_E18).max(2)
}
