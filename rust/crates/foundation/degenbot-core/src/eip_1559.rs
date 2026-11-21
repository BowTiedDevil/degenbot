//! EIP-1559 base-fee math.
//!
//! The delta formula is the integer form of the example code in the
//! [EIP-1559] proposal; this module is the canonical Rust implementation of it,
//! exposed to the Python driver as `degenbot._ffi.eip_1559` and to standalone
//! consumers via the umbrella re-export (`degenbot::eip_1559`).
//!
//! [EIP-1559]: https://eips.ethereum.org/EIPS/eip-1559
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
use std::num::NonZeroU128;

use crate::errors::{BaseFeeParamsError, NextBaseFeeError};

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

/// The EIP-1559 parameters one call needs: the two divisors and the optional
/// base-fee floor.
///
/// Both divisors are [`NonZeroU128`], so the zero they would divide by in the
/// raw formula is unrepresentable here. Untrusted values (the Python boundary,
/// configuration) go through [`BaseFeeParams::try_new`], which refuses a zero
/// with [`BaseFeeParamsError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BaseFeeParams {
    base_fee_max_change_denominator: NonZeroU128,
    elasticity_multiplier: NonZeroU128,
    min_base_fee: Option<u128>,
}

impl BaseFeeParams {
    /// The EIP-1559 mainnet parameters: denominator 8, elasticity 2, no floor.
    pub const DEFAULT: Self = Self {
        base_fee_max_change_denominator: NonZeroU128::new(DEFAULT_BASE_FEE_MAX_CHANGE_DENOMINATOR)
            .expect("the EIP-1559 default max-change denominator is non-zero"),
        elasticity_multiplier: NonZeroU128::new(DEFAULT_ELASTICITY_MULTIPLIER)
            .expect("the EIP-1559 default elasticity multiplier is non-zero"),
        min_base_fee: None,
    };

    /// Build the parameters from untrusted values.
    ///
    /// # Errors
    ///
    /// [`BaseFeeParamsError::ZeroMaxChangeDenominator`] or
    /// [`BaseFeeParamsError::ZeroElasticityMultiplier`] when either divisor is
    /// zero.
    pub fn try_new(
        base_fee_max_change_denominator: u128,
        elasticity_multiplier: u128,
        min_base_fee: Option<u128>,
    ) -> Result<Self, BaseFeeParamsError> {
        let base_fee_max_change_denominator = NonZeroU128::new(base_fee_max_change_denominator)
            .ok_or(BaseFeeParamsError::ZeroMaxChangeDenominator)?;
        let elasticity_multiplier = NonZeroU128::new(elasticity_multiplier)
            .ok_or(BaseFeeParamsError::ZeroElasticityMultiplier)?;
        Ok(Self {
            base_fee_max_change_denominator,
            elasticity_multiplier,
            min_base_fee,
        })
    }

    /// The maximum per-block change, as a `1/n` fraction of the parent fee.
    #[must_use]
    pub const fn base_fee_max_change_denominator(&self) -> NonZeroU128 {
        self.base_fee_max_change_denominator
    }

    /// The gas-limit-to-gas-target multiplier.
    #[must_use]
    pub const fn elasticity_multiplier(&self) -> NonZeroU128 {
        self.elasticity_multiplier
    }

    /// The optional base-fee floor.
    #[must_use]
    pub const fn min_base_fee(&self) -> Option<u128> {
        self.min_base_fee
    }

    /// The gas target of `parent_gas_limit` — the delta's divisor, which is zero
    /// only for a gas limit below the elasticity multiplier.
    #[must_use]
    pub const fn gas_target(&self, parent_gas_limit: u128) -> u128 {
        parent_gas_limit / self.elasticity_multiplier.get()
    }
}

/// Calculate the next EIP-1559 base fee for a compatible blockchain.
///
/// Integer-exact (no float) evaluation of the formula in the [EIP-1559]
/// proposal's example code. [`BaseFeeParams::DEFAULT`] carries EIP-1559's
/// `base_fee_max_change_denominator` (8) and `elasticity_multiplier` (2);
/// non-Ethereum EIP-1559 chains (e.g. some L2s) may override either.
///
/// # Branch logic
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
/// no clamp. `min_base_fee` supports non-Ethereum EIP-1559 chains that floor
/// the base fee (e.g. Linea).
///
/// # Totality
///
/// Never panics: both divisors are [`NonZeroU128`] by construction, the delta
/// only divides by a gas target proven non-zero above, the [`U256`] narrowing
/// saturates, and the increase path saturates at `u128::MAX`.
///
/// # Errors
///
/// [`NextBaseFeeError::ZeroGasTarget`] when `parent_gas_limit` is below
/// [`BaseFeeParams::elasticity_multiplier`] while `parent_gas_used` is
/// non-zero: the gas target is zero, so the delta has no divisor. The same zero
/// target with zero usage takes the unchanged branch and stays total.
///
/// [EIP-1559]: https://eips.ethereum.org/EIPS/eip-1559
pub fn next_base_fee(
    parent_base_fee: u128,
    parent_gas_used: u128,
    parent_gas_limit: u128,
    params: BaseFeeParams,
) -> Result<u128, NextBaseFeeError> {
    let last_gas_target = params.gas_target(parent_gas_limit);
    if last_gas_target == 0 && parent_gas_used != 0 {
        return Err(NextBaseFeeError::ZeroGasTarget {
            parent_gas_limit,
            elasticity_multiplier: params.elasticity_multiplier().get(),
        });
    }

    let delta = |gas_used_delta: u128| {
        // U256 intermediate: parent_base_fee * gas_used_delta can exceed u128.
        U256::from(parent_base_fee) * U256::from(gas_used_delta)
            / U256::from(last_gas_target)
            / U256::from(params.base_fee_max_change_denominator().get())
    };

    let working_base_fee = match parent_gas_used.cmp(&last_gas_target) {
        Ordering::Equal => parent_base_fee,
        Ordering::Greater => {
            let base_fee_delta = std::cmp::max(
                delta(parent_gas_used - last_gas_target).saturating_to::<u128>(),
                1,
            );
            parent_base_fee.saturating_add(base_fee_delta)
        }
        Ordering::Less => {
            parent_base_fee - delta(last_gas_target - parent_gas_used).saturating_to::<u128>()
        }
    };

    Ok(match params.min_base_fee() {
        Some(min) => working_base_fee.max(min),
        None => working_base_fee,
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::expect_used, clippy::too_many_lines)]

    use super::*;
    use crate::errors::{BaseFeeParamsError, NextBaseFeeError};

    /// A parity-test fixture row:
    /// `(parent_base_fee, parent_gas_used, parent_gas_limit, min_base_fee,
    ///   base_fee_max_change_denominator, elasticity_multiplier)` → expected result.
    type Case = (u128, u128, u128, Option<u128>, u128, u128, u128);

    /// The params of one fixture row, built through the refusing constructor.
    fn params(min_base_fee: Option<u128>, denominator: u128, elasticity: u128) -> BaseFeeParams {
        BaseFeeParams::try_new(denominator, elasticity, min_base_fee)
            .expect("the fixtures use the non-zero divisors the type requires")
    }

    /// The fee for an in-domain row: no fixture asks with a zero gas target.
    fn fee(bf: u128, gu: u128, gl: u128, params: BaseFeeParams) -> u128 {
        next_base_fee(bf, gu, gl, params).expect("the fixtures keep the gas target non-zero")
    }

    /// Convenience wrapper using the EIP-1559 defaults and no `min_base_fee`
    /// — the common mainnet call shape.
    fn nbf(bf: u128, gu: u128, gl: u128) -> u128 {
        fee(bf, gu, gl, BaseFeeParams::DEFAULT)
    }

    // Parity fixtures recorded from the Python reference implementation.
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
            let got = fee(bf, gu, gl, params(min, d, e));
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
                prop_assert_eq!(fee(bf, target, gl, params(None, 8, e)), bf);
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
                let next = fee(bf, target + over, gl, params(None, 8, e));
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
                let next = fee(bf, target - under, gl, params(None, 8, e));
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
                let next = fee(bf, 0, gl, params(Some(min), 8, 2));
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
        // Ranges kept modest so `bf * (gl - target)` fits `u128` in the bound check.
        proptest!(|(bf in 1u128..10u128.pow(30), gl in 4u128..100_000_000, denom in 2u128..=64)| {
            let target = gl / 2;
            if target > 0 {
                let delta_max_up = bf * (gl - target) / target / denom;
                let up = fee(bf, gl, gl, params(None, denom, 2)); // full block (max over)
                prop_assert!(up <= bf + delta_max_up + 1, "up={up} > bf+{delta_max_up}+1 (bf={bf},gl={gl},d={denom})");
                // decrease path: max decrease is the empty-block delta = bf/denom exactly.
                let delta_max_down = bf / denom;
                let down = fee(bf, 0, gl, params(None, denom, 2)); // empty block (max under)
                prop_assert!(down >= bf - delta_max_down, "down={down} < bf-{delta_max_down} (bf={bf},gl={gl},d={denom})");
            }
        });
    }

    // ---------------------------------------------------------------------------
    // Edge cases.
    // ---------------------------------------------------------------------------

    /// `min_base_fee = Some(0)` is a no-op clamp (`max(0, x) == x` for unsigned
    /// `x`).
    #[test]
    fn min_fee_zero_is_noop() {
        let none = fee(1_000_000_000, 0, 30_000_000, BaseFeeParams::DEFAULT);
        let zero = fee(1_000_000_000, 0, 30_000_000, params(Some(0), 8, 2));
        assert_eq!(none, zero);
        assert_eq!(zero, 875_000_000);
    }

    /// The increase-floor: above target with a tiny `parent_base_fee`, the `max(_,1)`
    /// floor bumps the fee by exactly 1.
    #[test]
    fn increase_floor_bumps_by_one() {
        // bf=1, target=1; gu=2>1 → gas_used_delta=1, delta=1*1//1//8=0 → max(0,1)=1 → 1+1=2
        assert_eq!(nbf(1, 2, 2), 2);
        // bf=0, gu=2>1 → delta=max(0,1)=1 → 0+1=1
        assert_eq!(nbf(0, 2, 2), 1);
    }

    /// Defaults constants match EIP-1559, and the default params carry them.
    #[test]
    fn eip1559_defaults() {
        assert_eq!(DEFAULT_BASE_FEE_MAX_CHANGE_DENOMINATOR, 8);
        assert_eq!(DEFAULT_ELASTICITY_MULTIPLIER, 2);
        assert_eq!(
            BaseFeeParams::DEFAULT
                .base_fee_max_change_denominator()
                .get(),
            DEFAULT_BASE_FEE_MAX_CHANGE_DENOMINATOR
        );
        assert_eq!(
            BaseFeeParams::DEFAULT.elasticity_multiplier().get(),
            DEFAULT_ELASTICITY_MULTIPLIER
        );
        assert_eq!(BaseFeeParams::DEFAULT.min_base_fee(), None);
    }

    /// Smoke: nbf convenience matches the explicit-defaults call.
    #[test]
    fn convenience_matches_explicit_defaults() {
        assert_eq!(nbf(1_000_000_000, 0, 30_000_000), 875_000_000);
    }

    // ---------------------------------------------------------------------------
    // The zero divisors are unrepresentable: refused at construction, or a typed
    // refusal where the divisor is relational.
    // ---------------------------------------------------------------------------

    /// A zero max-change denominator never reaches the delta division.
    #[test]
    fn zero_denominator_is_refused_at_params_construction() {
        assert_eq!(
            BaseFeeParams::try_new(0, DEFAULT_ELASTICITY_MULTIPLIER, None),
            Err(BaseFeeParamsError::ZeroMaxChangeDenominator)
        );
    }

    /// A zero elasticity never reaches the gas-target division.
    #[test]
    fn zero_elasticity_is_refused_at_params_construction() {
        assert_eq!(
            BaseFeeParams::try_new(DEFAULT_BASE_FEE_MAX_CHANGE_DENOMINATOR, 0, None),
            Err(BaseFeeParamsError::ZeroElasticityMultiplier)
        );
    }

    /// A zero gas target with non-zero usage has no divisor for the delta, so a
    /// block that used gas is a typed refusal (the pre-cutover panic path).
    #[test]
    fn zero_gas_target_with_nonzero_usage_is_a_typed_refusal() {
        assert_eq!(
            next_base_fee(1_000_000_000, 1, 0, BaseFeeParams::DEFAULT),
            Err(NextBaseFeeError::ZeroGasTarget {
                parent_gas_limit: 0,
                elasticity_multiplier: DEFAULT_ELASTICITY_MULTIPLIER,
            })
        );
        assert_eq!(
            next_base_fee(1_000_000_000, 2, 1, BaseFeeParams::DEFAULT),
            Err(NextBaseFeeError::ZeroGasTarget {
                parent_gas_limit: 1,
                elasticity_multiplier: DEFAULT_ELASTICITY_MULTIPLIER,
            })
        );
    }

    /// The same zero target with zero usage divides nothing: the unchanged
    /// branch stays total, so a pre-EIP-1559-shaped header still prices.
    #[test]
    fn zero_gas_target_with_zero_usage_stays_the_unchanged_branch() {
        assert_eq!(nbf(1_000_000_000, 0, 0), 1_000_000_000);
        assert_eq!(nbf(1_000_000_000, 0, 1), 1_000_000_000);
    }

    /// The increase path saturates at the `u128` ceiling instead of overflowing,
    /// both when the add carries past it and when the `U256` delta itself exceeds
    /// `u128` (a wide elasticity widens the delta up to `e - 1` times the parent
    /// fee).
    #[test]
    fn increase_saturates_at_the_u128_ceiling() {
        assert_eq!(nbf(u128::MAX, 2, 2), u128::MAX);
        assert_eq!(
            fee(
                u128::MAX,
                64,
                64,
                params(None, DEFAULT_BASE_FEE_MAX_CHANGE_DENOMINATOR, 64)
            ),
            u128::MAX
        );
    }
}
