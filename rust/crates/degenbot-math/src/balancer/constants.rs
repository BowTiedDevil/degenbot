//! Balancer V2 math constants (18/20/36-decimal fixed-point bases, `PowVersion`).
//!
//! Direct port of `src/degenbot/balancer/libraries/constants.py`.
//! ADR-005 slice 12e — the pure-math port sub-slice of slice 12.

use alloy::primitives::U256;

/// 1.0 in 18-decimal fixed point. The reference unit for all FixedPoint
/// operations (`mul_down`/`div_up`/`pow_down`/`complement` …).
pub const ONE: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);

/// 2.0 in 18-decimal fixed point (`2 * ONE`).
pub const TWO: U256 = U256::from_limbs([2_000_000_000_000_000_000, 0, 0, 0]);

/// 4.0 in 18-decimal fixed point (`4 * ONE`).
pub const FOUR: U256 = U256::from_limbs([4_000_000_000_000_000_000, 0, 0, 0]);

/// `MAX_POW_RELATIVE_ERROR = 10000` — the relative error bound applied by
/// `pow_down`/`pow_up` after the `LogExpMath::pow` approximation. Carries
/// no decimal scaling; multiplied by the raw `pow` output via 18-decimal
/// `mul_up` then ±/subtracted.
pub const MAX_POW_RELATIVE_ERROR: U256 = U256::from_limbs([10_000, 0, 0, 0]);

/// Identifies which `FixedPoint::pow` implementation a deployed pool
/// contract uses.
///
/// Direct port of `balancer.libraries.constants.PowVersion`. Different
/// deployed versions of Balancer V2 `WeightedPool` contracts embed
/// different versions of the `FixedPoint` library:
///
/// - `V1` (no fast paths): older contracts like `WeightedPool2Tokens`
///   use the general `LogExpMath::pow` path with `MAX_POW_RELATIVE_ERROR`
///   error bound for all exponent values.
/// - `V2` (with fast paths): newer `WeightedPool` contracts include fast
///   paths for `y == ONE`, `y == TWO`, `y == FOUR` that bypass the
///   error-bound calculation, producing different rounding behavior.
///
/// The version is detected from the pool's deployed bytecode at build time
/// (the Python `BalancerV2Pool.pow_version` carries it) and threaded into
/// the Rust `WeightedMath` `pow_down`/`pow_up` invocations so swap
/// calculations match the on-chain contract exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowVersion {
    /// General path only (no fast paths) — e.g. `WeightedPool2Tokens`.
    V1,
    /// Fast paths for `y == ONE`, `TWO`, `FOUR` — e.g. newer `WeightedPool`.
    V2,
}

impl PowVersion {
    /// Map the Python-carried `u8` discriminant (1 = V1, 2 = V2) to a
    /// `PowVersion`. Matches `BalancerBuilderBase::pow_version_to_rust`.
    ///
    /// Returns `None` for an unknown discriminant (a builder wiring error).
    #[must_use]
    pub fn from_u8(d: u8) -> Option<Self> {
        match d {
            1 => Some(Self::V1),
            2 => Some(Self::V2),
            _ => None,
        }
    }

    /// The inverse of [`Self::from_u8`] — useful for the FFI seam + tests.
    #[must_use]
    pub fn to_u8(self) -> u8 {
        match self {
            Self::V1 => 1,
            Self::V2 => 2,
        }
    }
}
