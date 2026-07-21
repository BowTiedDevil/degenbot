//! Spec-bound validation for pool registration admission.
//!
//! The on-chain Solidity types constrain every field a Uniswap V2 / V3 / V4
//! pool can hold. `BotState::register_v{2,3,4}_pool` admits pools by
//! verifying these widths up-front and returning a typed [`SpecViolation`]
//! on a violation; downstream swap math (`u512_to_u256_internal`, the CL
//! step math) can then rely on spec-bound admission as an upstream guarantee
//! (cf. the `# Panics` section on the V2/V3 U512→U256 narrowing helpers
//! committed in `19218a2c`).
//!
//! Bounds are derived from Uniswap v2-core / v3-core / v4-core (researched
//! against the contract source trees):
//!
//! | Field | On-chain type | Bound |
//! |---|---|---|
//! | V2 reserves (reserve0 / reserve1) | `uint112` | `≤ 2^112 − 1` (v2-core `UniswapV2Pair._update`: `require(balance0 <= uint112(-1))`) |
//! | V3/V4 `sqrtPriceX96` | `uint160` | `[MIN_SQRT_RATIO, MAX_SQRT_RATIO)` (`TickMath` rejects out-of-range) |
//! | V3/V4 liquidity | `uint128` | `≤ 2^128 − 1` (already enforced by the Rust `u128` type — no check) |
//! | V3 `fee` | `uint24` | `< 1_000_000` (V3 factory `require(fee < 1000000)`) |
//! | V4 `lpFee` | `uint24` | `< 1 << 24` (static-fee path reserves the `0x800000` bit for the dynamic-fee flag) |
//! | V3/V4 `tick` | `int24` | `[MIN_TICK = −887_272, MAX_TICK = 887_272]` |
//! | V3/V4 `tickSpacing` | `int24` | `[1, 32_767]` (V4 spec: `MIN_TICK_SPACING=1, MAX_TICK_SPACING=type(int16).max`) |
//!
//! The V3/V4 bounds reuse `degenbot_cl_math::cl_lib::tick_math`'s constants
//! (`MIN_SQRT_RATIO` / `MAX_SQRT_RATIO` / `MIN_TICK` / `MAX_TICK`) — both V3
//! and V4 share the same `TickMath`, so the validators are family-agnostic.

use alloy::primitives::{aliases::U112, uint, U256};
use degenbot_cl_math::cl_lib::tick_math::{MAX_SQRT_RATIO, MAX_TICK, MIN_SQRT_RATIO, MIN_TICK};

/// `uint112(-1)` — `2^112 − 1`. The `uint112` storage width v2-core asserts
/// at `UniswapV2Pair._update` (`require(balance0 <= uint112(-1))`). Held as
/// `U256` for direct comparison with `RegisterV2PoolParams.reserve{0,1}`,
/// which are upcast to `U256` for ergonomic arithmetic elsewhere in the
/// solver path.
pub const UINT112_MAX: U256 = uint!(5192296858534827628530496329220095_U256);

/// V3 factory's `< 1_000_000` fee bound — `UniswapV3Factory.createPool`:
/// `require(fee < 1000000)`.
pub const V3_FEE_MAX: u32 = 1_000_000;

/// V4 `lpFee` static-fee bound — the high bit (`0x800000`) is reserved for
/// the dynamic-fee flag (`V4_DYNAMIC_FEE_FLAG`); static fees must lie below
/// `1 << 24 = 16_777_216`.
pub const V4_FEE_MAX: u32 = 1 << 24;

/// Minimum allowed `tickSpacing` (V4 spec: `MIN_TICK_SPACING = 1`).
pub const MIN_TICK_SPACING: i32 = 1;
/// Maximum allowed `tickSpacing` (V4 spec: `MAX_TICK_SPACING =
/// type(int16).max = 32_767`).
pub const MAX_TICK_SPACING: i32 = 32_767;

/// A typed carrier for a single out-of-spec field at registration.
///
/// `field` names the offending `RegisterV2PoolParams` field (e.g.
/// `"reserve0"`); `value` carries the actual value as a [`SpecValue`] (so
/// the enum is uniform across `u32` / `i32` / `u128` / `U256` field types);
/// `bound` is a human-readable bound string (e.g. `"uint112 (≤ 2^112 − 1)"`)
/// for diagnostic surfacing at the `PyO3` seam.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpecViolation {
    /// The offending param-field name (matches the struct-field identifier).
    pub field: &'static str,
    /// The actual value that violated the bound.
    pub value: SpecValue,
    /// Human-readable bound string, for diagnostics.
    pub bound: &'static str,
}

/// A type-erased primitive value carried by [`SpecViolation`] across the
/// `u32` / `i32` / `u128` / `U256` field types a pool registration can hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpecValue {
    /// `U256`-typed field (V2 reserves, V3/V4 `sqrtPriceX96`).
    U256(U256),
    /// `u128`-typed field (V3/V4 liquidity — guarded by the Rust type, no
    /// runtime check; kept for completeness).
    U128(u128),
    /// `u32`-typed field (V3/V4 fee).
    U32(u32),
    /// `i32`-typed field (V3/V4 tick / tickSpacing).
    I32(i32),
}

impl std::fmt::Display for SpecValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::U256(v) => std::fmt::Display::fmt(&v, f),
            Self::U128(v) => std::fmt::Display::fmt(&v, f),
            Self::U32(v) => std::fmt::Display::fmt(&v, f),
            Self::I32(v) => std::fmt::Display::fmt(&v, f),
        }
    }
}

impl std::fmt::Display for SpecViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "field `{}` value {} is out of bounds: {}",
            self.field, self.value, self.bound,
        )
    }
}

// ---------------------------------------------------------------------------
// Validators — pure functions returning `Result<(), SpecViolation>`.
//
// The family `RegisterV{2,3,4}PoolError` enums each wrap a `SpecViolation`
// (added in MSTAT2 / 24KNGF / K3IICB) via `SpecViolation(SpecViolation)` —
// the register fns `.map_err(RegisterV*PoolError::SpecViolation)` when
// calling these.
// ---------------------------------------------------------------------------

/// Validate a V2 reserve value (`uint112` on-chain).
///
/// `field` is the offending struct-field name (`"reserve0"` / `"reserve1"`)
/// used in the resulting [`SpecViolation`] diagnostic.
///
/// # Errors
///
/// Returns [`Err`] with a [`SpecViolation`] bound `"uint112 (≤ 2^112 − 1)"`
/// when `value > UINT112_MAX` (i.e. `> 2^112 − 1`).
pub fn validate_v2_reserve(value: U112, field: &'static str) -> Result<(), SpecViolation> {
    if U256::from(value) > UINT112_MAX {
        return Err(SpecViolation {
            field,
            value: SpecValue::U256(U256::from(value)),
            bound: "uint112 (≤ 2^112 − 1)",
        });
    }
    Ok(())
}

/// Narrow a `U256`-sourced reserve to `U112` with a spec-bound check.
///
/// The ingestion seam for reserves that arrive as `U256` from Python / RPC
/// decoding (the `PyO3` `sync_reserves` path, the V2 Sync decoder's
/// ABI-word decode): validates `value ≤ uint112::MAX` before narrowing to
/// `U112` via `.to::<U112>()` (which panics on overflow — this fn runs the
/// check first, so the `.to` is infallible here).
///
/// Post-ZPHT6X the `V2PoolState` / `V2BlockDelta` fields are typed `U112`,
/// so any `U256`-sourced reserve must narrow through this seam before
/// reaching typed storage.
///
/// # Errors
///
/// Returns [`Err`] with a [`SpecViolation`] bound `"uint112 (≤ 2^112 − 1)"`
/// when `value > UINT112_MAX` (i.e. `> 2^112 − 1`).
pub fn narrow_v2_reserve(value: U256, field: &'static str) -> Result<U112, SpecViolation> {
    if value > UINT112_MAX {
        return Err(SpecViolation {
            field,
            value: SpecValue::U256(value),
            bound: "uint112 (≤ 2^112 − 1)",
        });
    }
    Ok(value.to::<U112>())
}

/// Validate a V3/V4 `sqrtPriceX96` (`TickMath`-bounded `uint160`).
///
/// On-chain invariant: `MIN_SQRT_RATIO <= sqrtPriceX96 < MAX_SQRT_RATIO`
/// (the maximum is strict — `TickMath` rejects `sqrtPriceX96 >= MAX`).
///
/// # Errors
///
/// Returns [`Err`] with a [`SpecViolation`] when `sp < MIN_SQRT_RATIO` or
/// `sp >= MAX_SQRT_RATIO`.
pub fn validate_sqrt_price(sp: U256) -> Result<(), SpecViolation> {
    if sp < U256::from(MIN_SQRT_RATIO) || sp >= U256::from(MAX_SQRT_RATIO) {
        return Err(SpecViolation {
            field: "sqrtPriceX96",
            value: SpecValue::U256(sp),
            bound: "sqrtPriceX96 (∈ [MIN_SQRT_RATIO, MAX_SQRT_RATIO))",
        });
    }
    Ok(())
}

/// Validate a V3/V4 tick (`int24`, `TickMath`-bounded).
///
/// On-chain invariant: `MIN_TICK <= tick <= MAX_TICK` (inclusive both ends).
///
/// # Errors
///
/// Returns [`Err`] with a [`SpecViolation`] when `tick < MIN_TICK` or
/// `tick > MAX_TICK`.
pub fn validate_tick(tick: i32) -> Result<(), SpecViolation> {
    if !(MIN_TICK..=MAX_TICK).contains(&tick) {
        return Err(SpecViolation {
            field: "tick",
            value: SpecValue::I32(tick),
            bound: "tick (∈ [MIN_TICK = −887_272, MAX_TICK = 887_272])",
        });
    }
    Ok(())
}

/// Validate a V3 `fee` (`uint24`, V3 factory-bounded).
///
/// On-chain invariant: `fee < 1_000_000` (V3 factory `require(fee <
/// 1000000)`).
///
/// # Errors
///
/// Returns [`Err`] with a [`SpecViolation`] when `fee >= V3_FEE_MAX`
/// (i.e. `>= 1_000_000`).
pub fn validate_v3_fee(fee: u32) -> Result<(), SpecViolation> {
    if fee >= V3_FEE_MAX {
        return Err(SpecViolation {
            field: "fee",
            value: SpecValue::U32(fee),
            bound: "V3 fee (< 1_000_000)",
        });
    }
    Ok(())
}

/// Validate a V4 `lpFee` (`uint24`, static-fee path).
///
/// On-chain invariant: `fee < 1 << 24` (the `0x800000` high bit flags a
/// dynamic-fee pool, which `register_v4_pool` rejects upstream as
/// `DynamicFee` — this validator is the static-fee floor).
///
/// # Errors
///
/// Returns [`Err`] with a [`SpecViolation`] when `fee >= V4_FEE_MAX`
/// (i.e. `>= 1 << 24`).
pub fn validate_v4_fee(fee: u32) -> Result<(), SpecViolation> {
    if fee >= V4_FEE_MAX {
        return Err(SpecViolation {
            field: "fee",
            value: SpecValue::U32(fee),
            bound: "V4 lp_fee (< 1 << 24)",
        });
    }
    Ok(())
}

/// Validate a V3/V4 `tickSpacing` (`int24`).
///
/// On-chain invariant: `MIN_TICK_SPACING (1) <= spacing <= MAX_TICK_SPACING
/// (32_767)`.
///
/// # Errors
///
/// Returns [`Err`] with a [`SpecViolation`] when `spacing <
/// MIN_TICK_SPACING` or `spacing > MAX_TICK_SPACING`.
pub fn validate_tick_spacing(spacing: i32) -> Result<(), SpecViolation> {
    if !(MIN_TICK_SPACING..=MAX_TICK_SPACING).contains(&spacing) {
        return Err(SpecViolation {
            field: "tickSpacing",
            value: SpecValue::I32(spacing),
            bound: "tickSpacing (∈ [1, 32_767])",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::uint;

    // ── accept boundaries ─────────────────────────────────────────
    // Each validator's `Ok` boundary is the largest value the spec accepts;
    // the reject test below asserts the very next step out-of-range fires.

    #[test]
    fn v2_reserve_accepts_uint112_max() {
        // Post-ZPHT6X the field is typed `U112`, so `U112::MAX` *is*
        // `UINT112_MAX`; the type-level bound supersedes the runtime check.
        assert_eq!(validate_v2_reserve(U112::MAX, "reserve0"), Ok(()));
    }
    #[test]
    fn v2_reserve_accepts_zero() {
        assert_eq!(validate_v2_reserve(U112::ZERO, "reserve1"), Ok(()));
    }
    // Note: the pre-ZPHT6X `v2_reserve_rejects_uint112_max_plus_one` test is
    // removed — a value `> UINT112_MAX` cannot be constructed as a `U112`
    // (the type's `MAX` *is* `UINT112_MAX`), so the runtime reject branch is
    // unreachable for `U112`-typed input. The branch remains live for the
    // `U256`-ingestion seams (PyO3 `sync_reserves`, the Sync decoder's
    // high-bits-zero check) that narrow to `U112` after validation; those
    // paths exercise the bound in their own crate's tests.

    #[test]
    fn sqrt_price_accepts_min() {
        assert_eq!(validate_sqrt_price(U256::from(MIN_SQRT_RATIO)), Ok(()));
    }
    #[test]
    fn sqrt_price_rejects_below_min() {
        let sp = U256::from(MIN_SQRT_RATIO) - uint!(1_U256);
        assert!(matches!(
            validate_sqrt_price(sp),
            Err(SpecViolation { bound, .. }) if bound.contains("sqrtPriceX96"),
        ));
    }
    #[test]
    fn sqrt_price_accepts_just_below_max() {
        // MAX_SQRT_RATIO is the strict upper bound — MAX - 1 is the highest
        // value the spec accepts.
        let sp = U256::from(MAX_SQRT_RATIO) - uint!(1_U256);
        assert_eq!(validate_sqrt_price(sp), Ok(()));
    }
    #[test]
    fn sqrt_price_rejects_at_max() {
        assert!(matches!(
            validate_sqrt_price(U256::from(MAX_SQRT_RATIO)),
            Err(SpecViolation { bound, .. }) if bound.contains("sqrtPriceX96"),
        ));
    }

    #[test]
    fn tick_accepts_min() {
        assert_eq!(validate_tick(MIN_TICK), Ok(()));
    }
    #[test]
    fn tick_accepts_max() {
        assert_eq!(validate_tick(MAX_TICK), Ok(()));
    }
    #[test]
    fn tick_rejects_below_min() {
        assert!(matches!(
            validate_tick(MIN_TICK - 1),
            Err(SpecViolation { bound, .. }) if bound.contains("tick"),
        ));
    }
    #[test]
    fn tick_rejects_above_max() {
        assert!(matches!(
            validate_tick(MAX_TICK + 1),
            Err(SpecViolation { bound, .. }) if bound.contains("tick"),
        ));
    }

    #[test]
    fn v3_fee_accepts_zero() {
        assert_eq!(validate_v3_fee(0), Ok(()));
    }
    #[test]
    fn v3_fee_accepts_just_below_max() {
        assert_eq!(validate_v3_fee(V3_FEE_MAX - 1), Ok(()));
    }
    #[test]
    fn v3_fee_rejects_at_max() {
        assert!(matches!(
            validate_v3_fee(V3_FEE_MAX),
            Err(SpecViolation { bound, .. }) if bound.contains("fee"),
        ));
    }

    #[test]
    fn v4_fee_accepts_zero() {
        assert_eq!(validate_v4_fee(0), Ok(()));
    }
    #[test]
    fn v4_fee_accepts_just_below_max() {
        assert_eq!(validate_v4_fee(V4_FEE_MAX - 1), Ok(()));
    }
    #[test]
    fn v4_fee_rejects_at_max() {
        assert!(matches!(
            validate_v4_fee(V4_FEE_MAX),
            Err(SpecViolation { bound, .. }) if bound.contains("fee"),
        ));
    }

    #[test]
    fn tick_spacing_accepts_min() {
        assert_eq!(validate_tick_spacing(MIN_TICK_SPACING), Ok(()));
    }
    #[test]
    fn tick_spacing_accepts_max() {
        assert_eq!(validate_tick_spacing(MAX_TICK_SPACING), Ok(()));
    }
    #[test]
    fn tick_spacing_rejects_zero() {
        assert!(matches!(
            validate_tick_spacing(0),
            Err(SpecViolation { bound, .. }) if bound.contains("tickSpacing"),
        ));
    }
    #[test]
    fn tick_spacing_rejects_above_max() {
        assert!(matches!(
            validate_tick_spacing(MAX_TICK_SPACING + 1),
            Err(SpecViolation { bound, .. }) if bound.contains("tickSpacing"),
        ));
    }
}
