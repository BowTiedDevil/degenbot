//! Pure-Rust EVM arithmetic — gas math and value validation.
//!
//! A pyo3-free *core leaf* that ports pure-integer EVM math from the Python
//! reference (`src/degenbot/calculations/evm_math.py`). The Python module is
//! retained as the **parity oracle** (ADR-005 §4.2); this crate is the canonical
//! Rust implementation consumed cross-epic by Submission (fee finalization) and
//! Simulation (`base_fee_next` net-profit) via the umbrella re-export.
//!
//! # Integer discipline
//!
//! All math is integer-only — no floats — matching the EVM's truncating
//! division. The EIP-1559 base-fee delta multiplies `parent_base_fee *
//! gas_used_delta`, which can exceed `u128` range for the full `u128` input
//! domain; the intermediate is therefore computed in `alloy::primitives::U256`
//! and narrowed back to `u128` for the result (base-fee values always fit
//! `u128`).

use alloy::primitives::U256;
use std::cmp::Ordering;

pub mod wad_ray_math;
pub use wad_ray_math::{
    ray_div, ray_div_ceil, ray_div_floor, ray_mul, ray_mul_ceil, ray_mul_floor, ray_to_wad,
    wad_div, wad_mul, wad_to_ray, RayRounding, WadRayError, HALF_RAY, HALF_WAD, MAX_UINT256, RAY,
    WAD, WAD_RAY_RATIO,
};

pub mod percentage_math;
pub use percentage_math::{
    percent_div, percent_div_ceil, percent_mul, percent_mul_ceil, percent_mul_floor, PercentError,
    HALF_PERCENTAGE_FACTOR, PERCENTAGE_FACTOR,
};

/// EIP-1559 base-fee maximum change denominator (`BASE_FEE_MAX_CHANGE_DENOMINATOR`).
///
/// Per [EIP-1559], the base fee may change by at most `1/8` per block.
///
/// [EIP-1559]: https://eips.ethereum.org/EIPS/eip-1559
pub const DEFAULT_BASE_FEE_MAX_CHANGE_DENOMINATOR: u128 = 8;

/// EIP-1559 elasticity multiplier (`ELASTICITY_MULTIPLIER`).
///
/// The gas target is `parent_gas_limit // elasticity_multiplier` — per
/// [EIP-1559] the target is half the limit (multiplier = 2). Non-Ethereum
/// EIP-1559 chains may differ.
///
/// [EIP-1559]: https://eips.ethereum.org/EIPS/eip-1559
pub const DEFAULT_ELASTICITY_MULTIPLIER: u128 = 2;

/// Calculate the next EIP-1559 base fee for a compatible blockchain.
///
/// Ports `degenbot.calculations.evm_math.next_base_fee` (integer-exact, no
/// float). The formula is taken from the example code in the [EIP-1559]
/// proposal. The default `base_fee_max_change_denominator` (8) and
/// `elasticity_multiplier` (2) are taken from EIP-1559; non-Ethereum EIP-1559
/// chains (e.g. some L2s) may override either.
///
/// # Branch logic (matches the Python oracle exactly)
///
/// - `parent_gas_used == target` → base fee is unchanged.
/// - `parent_gas_used > target`  → base fee increases by `max(delta, 1)`
///   (the `max(_, 1)` floor guarantees the fee never stagnates above target).
/// - `parent_gas_used < target`  → base fee decreases by `delta`.
///
/// where `target = parent_gas_limit // elasticity_multiplier` and
/// `delta = parent_base_fee * gas_used_delta // target //
/// base_fee_max_change_denominator`.
///
/// # `min_base_fee` clamp
///
/// When `Some(min)`, the result is `max(min, working_base_fee)`. `None` applies
/// no clamp. This preserves the Python oracle's `min_base_fee` argument, which
/// supports non-Ethereum EIP-1559 chains that floor the base fee (e.g. Linea).
///
/// # Arguments
///
/// - `parent_base_fee` — the parent block's base fee (wei).
/// - `parent_gas_used` — gas consumed by the parent block.
/// - `parent_gas_limit` — the parent block's gas limit (must be ≥
///   `elasticity_multiplier` so the target is non-zero; a zero target mirrors
///   the Python oracle, which raises `ZeroDivisionError`).
/// - `min_base_fee` — optional floor applied to the result.
/// - `base_fee_max_change_denominator` — max per-block change denominator
///   (default 8).
/// - `elasticity_multiplier` — gas-target divisor (default 2).
///
/// # Returns
///
/// The computed next base fee as a `u128`.
///
/// [EIP-1559]: https://eips.ethereum.org/EIPS/eip-1559
#[must_use]
pub fn next_base_fee(
    parent_base_fee: u128,
    parent_gas_used: u128,
    parent_gas_limit: u128,
    min_base_fee: Option<u128>,
    base_fee_max_change_denominator: u128,
    elasticity_multiplier: u128,
) -> u128 {
    let last_gas_target = parent_gas_limit / elasticity_multiplier;

    let working_base_fee = match parent_gas_used.cmp(&last_gas_target) {
        Ordering::Equal => parent_base_fee,
        Ordering::Greater => {
            let gas_used_delta = parent_gas_used - last_gas_target;
            // U256 intermediate: parent_base_fee * gas_used_delta can exceed u128.
            let delta = U256::from(parent_base_fee) * U256::from(gas_used_delta)
                / U256::from(last_gas_target)
                / U256::from(base_fee_max_change_denominator);
            let base_fee_delta = std::cmp::max(delta.to::<u128>(), 1);
            parent_base_fee + base_fee_delta
        }
        Ordering::Less => {
            let gas_used_delta = last_gas_target - parent_gas_used;
            let delta = U256::from(parent_base_fee) * U256::from(gas_used_delta)
                / U256::from(last_gas_target)
                / U256::from(base_fee_max_change_denominator);
            let base_fee_delta = delta.to::<u128>();
            parent_base_fee - base_fee_delta
        }
    };

    match min_base_fee {
        Some(min) => working_base_fee.max(min),
        None => working_base_fee,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::too_many_lines)]

    use super::*;

    /// A parity-test fixture row:
    /// `(parent_base_fee, parent_gas_used, parent_gas_limit, min_base_fee,
    ///   base_fee_max_change_denominator, elasticity_multiplier)` → oracle result.
    type Case = (u128, u128, u128, Option<u128>, u128, u128, u128);

    /// Convenience wrapper using the EIP-1559 defaults and no `min_base_fee`,
    /// matching the Python oracle's most common call shape.
    fn nbf(bf: u128, gu: u128, gl: u128) -> u128 {
        next_base_fee(bf, gu, gl, None, 8, 2)
    }

    // ---------------------------------------------------------------------------
    // Parity fixtures: generated from the Python oracle
    // `src/degenbot/calculations/evm_math.py` `next_base_fee`. Each row is
    // (parent_base_fee, parent_gas_used, parent_gas_limit, min_base_fee,
    //  denom, elasticity) => oracle result.
    // ---------------------------------------------------------------------------
    #[test]
    fn parity_vs_python_oracle() {
        // (bf, gu, gl, min, d, e, want) — all values as u128 literals.
        let cases: &[Case] = &[
            // --- flat (gas_used == target) ---
            (
                1_000_000_000,
                15_000_000,
                30_000_000,
                None,
                8,
                2,
                1_000_000_000,
            ),
            (7, 5, 10, None, 8, 2, 7),
            // --- increase (gas_used > target): +max(delta,1) ---
            (
                1_000_000_000,
                30_000_000,
                30_000_000,
                None,
                8,
                2,
                1_125_000_000,
            ),
            (
                1_000_000_000,
                20_000_000,
                30_000_000,
                None,
                8,
                2,
                1_041_666_666,
            ),
            (875_000_000, 29_999_999, 30_000_000, None, 8, 2, 984_374_992),
            // flat edge (target==1==used)
            (1_000_000, 1, 2, None, 8, 2, 1_000_000),
            (1, 1, 2, None, 8, 2, 1),
            (0, 1, 2, None, 8, 2, 0),
            // --- decrease (gas_used < target): −delta ---
            (
                1_000_000_000,
                10_000_000,
                30_000_000,
                None,
                8,
                2,
                958_333_334,
            ),
            (1_000_000_000, 0, 30_000_000, None, 8, 2, 875_000_000),
            (100, 1, 10, None, 8, 2, 90),
            (1_000_000, 0, 2, None, 8, 2, 875_000),
            // --- min_base_fee clamp ---
            (
                1_000_000_000,
                0,
                30_000_000,
                Some(900_000_000),
                8,
                2,
                900_000_000,
            ),
            (1_000_000_000, 0, 30_000_000, Some(0), 8, 2, 875_000_000),
            (
                1_000_000_000,
                30_000_000,
                30_000_000,
                Some(2_000_000_000),
                8,
                2,
                2_000_000_000,
            ),
            // --- non-default denominators / elasticity (non-Ethereum chains) ---
            (
                1_000_000_000,
                10_000_000,
                30_000_000,
                None,
                17,
                2,
                980_392_157,
            ),
            (
                1_000_000_000,
                10_000_000,
                20_000_000,
                None,
                8,
                2,
                1_000_000_000,
            ),
            (
                1_000_000_000,
                15_000_000,
                30_000_000,
                None,
                8,
                3,
                1_062_500_000,
            ),
            (500_000_000, 9_000_000, 30_000_000, None, 8, 2, 475_000_000),
            // --- large values (force u256 intermediate) ---
            (
                100_000_000_000_000_000_000,
                30_000_000,
                30_000_000,
                None,
                8,
                2,
                112_500_000_000_000_000_000,
            ),
            (
                1_000_000_000_000_000_000_000_000_000_000,
                30_000_000,
                30_000_000,
                None,
                8,
                2,
                1_125_000_000_000_000_000_000_000_000_000,
            ),
            // 2^120 — near the u128 high end; flat path.
            (
                1_329_227_995_784_915_872_903_807_060_280_344_576,
                15_000_000,
                30_000_000,
                None,
                8,
                2,
                1_329_227_995_784_915_872_903_807_060_280_344_576,
            ),
            // 2^120 — increase path; exercises u256 multiply + narrow.
            (
                1_329_227_995_784_915_872_903_807_060_280_344_576,
                30_000_000,
                30_000_000,
                None,
                8,
                2,
                1_495_381_495_258_030_357_016_782_942_815_387_648,
            ),
        ];

        for &(bf, gu, gl, min, d, e, want) in cases {
            let got = next_base_fee(bf, gu, gl, min, d, e);
            assert_eq!(
                got, want,
                "parity mismatch: next_base_fee(bf={bf}, gu={gu}, gl={gl}, min={min:?}, d={d}, e={e})"
            );
        }
    }

    // ---------------------------------------------------------------------------
    // Property tests (proptest): invariants of the EIP-1559 formula.
    // ---------------------------------------------------------------------------

    /// Invariant: at the gas target, the base fee is unchanged (round-trip flat).
    #[test]
    fn property_target_is_flat() {
        use proptest::prelude::*;
        proptest!(|(bf in 0u128..u128::MAX / 2, gl in 2u128..u128::MAX / 2, e in 2u128..=4)| {
            let target = gl / e;
            if target > 0 {
                prop_assert_eq!(next_base_fee(bf, target, gl, None, 8, e), bf);
            }
        });
    }

    /// Invariant: when `gas_used` > `target`, the base fee strictly increases (by at
    /// least 1, per the `max(delta, 1)` floor).
    #[test]
    fn property_over_target_increases() {
        use proptest::prelude::*;
        proptest!(|(bf in 1u128..u128::MAX / 2, gl in 4u128..u128::MAX / 2, e in 2u128..=4, over in 1u128..1000)| {
            let target = gl / e;
            if target > 0 && target + over <= gl && bf < u128::MAX / 4 {
                let next = next_base_fee(bf, target + over, gl, None, 8, e);
                prop_assert!(next > bf, "next={next} bf={bf}");
            }
        });
    }

    /// Invariant: when `gas_used` < `target`, the base fee never increases (it
    /// decreases or stays flat at `min_base_fee`).
    #[test]
    fn property_under_target_decreases() {
        use proptest::prelude::*;
        proptest!(|(bf in 1u128..u128::MAX / 2, gl in 4u128..u128::MAX / 2, e in 2u128..=4, under in 0u128..1000)| {
            let target = gl / e;
            if target > 0 && under < target {
                let next = next_base_fee(bf, target - under, gl, None, 8, e);
                prop_assert!(next <= bf, "next={next} bf={bf}");
            }
        });
    }

    /// Invariant: the result is always >= `min_base_fee` when provided, and never
    /// exceeds `max(parent_base_fee, min)` on the decrease path.
    #[test]
    fn property_min_clamp_floor() {
        use proptest::prelude::*;
        proptest!(|(bf in 100u128..u128::MAX / 4, gl in 4u128..u128::MAX / 2, min in 0u128..u128::MAX / 4)| {
            let target = gl / 2;
            if target > 0 {
                // empty block → maximal decrease; the clamp must still hold.
                let next = next_base_fee(bf, 0, gl, Some(min), 8, 2);
                prop_assert!(next >= min, "next={next} < min={min}");
            }
        });
    }

    /// Invariant: the per-block change is bounded by the maximal-possible delta
    /// (`gas_used` at the limit) plus the `max(_, 1)` floor. EIP-1559
    /// `BASE_FEE_MAX_CHANGE_DENOMINATOR` bounds the change to ~1/denominator, but
    /// when `gas_limit` is odd and `gas_used == gas_limit`, the floor-divided
    /// target makes `gas_used_delta = gl - (gl//2)` exceed `target` by one — so
    /// the correct bound is `bf * (gl - target) / target / denom + 1`, not
    /// `bf/denom + 1`.
    #[expect(clippy::manual_checked_ops)] // explicit `target > 0` guard above each division
    #[test]
    fn property_max_change_bounded() {
        use proptest::prelude::*;
        // Ranges kept modest so `bf * (gl - target)` fits `u128` in the oracle calc.
        proptest!(|(bf in 1u128..10u128.pow(30), gl in 4u128..100_000_000, denom in 2u128..=64)| {
            let target = gl / 2;
            if target > 0 {
                let delta_max_up = bf * (gl - target) / target / denom;
                let up = next_base_fee(bf, gl, gl, None, denom, 2); // full block (max over)
                prop_assert!(up <= bf + delta_max_up + 1, "up={up} > bf+{delta_max_up}+1 (bf={bf},gl={gl},d={denom})");
                // decrease path: max decrease is the empty-block delta = bf/denom exactly.
                let delta_max_down = bf / denom;
                let down = next_base_fee(bf, 0, gl, None, denom, 2); // empty block (max under)
                prop_assert!(down >= bf - delta_max_down, "down={down} < bf-{delta_max_down} (bf={bf},gl={gl},d={denom})");
            }
        });
    }

    // ---------------------------------------------------------------------------
    // Edge cases.
    // ---------------------------------------------------------------------------

    /// `min_base_fee = Some(0)` is a no-op clamp (matches the Python oracle,
    /// where `min_base_fee=0` is falsy and skipped — and `max(0, x) == x` for
    /// unsigned x regardless).
    #[test]
    fn min_fee_zero_is_noop() {
        let none = next_base_fee(1_000_000_000, 0, 30_000_000, None, 8, 2);
        let zero = next_base_fee(1_000_000_000, 0, 30_000_000, Some(0), 8, 2);
        assert_eq!(none, zero);
        assert_eq!(zero, 875_000_000);
    }

    /// The increase-floor: above target with a tiny `parent_base_fee`, the `max(_,1)`
    /// floor bumps the fee by exactly 1 (mirrors the Python oracle).
    #[test]
    fn increase_floor_bumps_by_one() {
        // bf=1, target=1; gu=2>1 → gas_used_delta=1, delta=1*1//1//8=0 → max(0,1)=1 → 1+1=2
        assert_eq!(next_base_fee(1, 2, 2, None, 8, 2), 2);
        // bf=0, gu=2>1 → delta=max(0,1)=1 → 0+1=1
        assert_eq!(next_base_fee(0, 2, 2, None, 8, 2), 1);
    }

    /// Defaults constants match EIP-1559.
    #[test]
    fn eip1559_defaults() {
        assert_eq!(DEFAULT_BASE_FEE_MAX_CHANGE_DENOMINATOR, 8);
        assert_eq!(DEFAULT_ELASTICITY_MULTIPLIER, 2);
    }

    /// Smoke: nbf convenience matches the explicit-defaults call.
    #[test]
    fn convenience_matches_explicit_defaults() {
        assert_eq!(nbf(1_000_000_000, 0, 30_000_000), 875_000_000);
    }
}
