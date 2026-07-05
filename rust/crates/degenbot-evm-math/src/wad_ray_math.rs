//! Aave V3 `WadRayMath` — 18/27-decimal fixed-point arithmetic.
//!
//! Verbatim port of `src/degenbot/aave/libraries/wad_ray_math.py` (which is
//! itself a port of the [Aave V3 Solidity `WadRayMath`][sol] library). The
//! §4.2 (U5YIBG) parity cross-check compares these against the Python oracle
//! byte-for-byte, so the rounding + overflow semantics MUST match exactly.
//!
//! [sol]: https://github.com/aave/aave-v3-core/blob/master/contracts/protocol/libraries/math/WadRayMath.sol
//!
//! # Constants
//!
//! - `WAD` — 18-decimal fixed-point (1e18).
//! - `RAY` — 27-decimal fixed-point (1e27); Aave V3 uses ray for indices/rates.
//! - `WAD_RAY_RATIO` — 1e9 (ray ÷ wad); the conversion factor between the two.
//!
//! # Rounding
//!
//! The Python library exposes three rounding modes per op:
//! - **Half-up** (the default; `(a*b + HALF_RAY) // RAY` — rounds to nearest,
//!   ties go up).
//! - **Floor** (`(a*b) // RAY` — truncates toward zero).
//! - **Ceil** (`(a*b) // RAY + ((a*b) % RAY != 0)` — rounds toward +∞).
//!
//! `ray_mul` / `ray_div` take an optional [`RayRounding`] mode; the
//! revision-aware token processors (`degenbot-aave-updater::processors`)
//! select floor/ceil/half-up per the token revision's strategy (see the
//! Python `processors/strategies.py::RoundingStrategy`).
//!
//! # Overflow
//!
//! The Python oracle raises `EVMRevertError` on overflow (`a*b > MAX_UINT256`).
//! The Rust port returns [`WadRayError::Overflow`] (a `thiserror::Error`); the
//! apply layer surfaces it via `?`. Division by zero returns
//! [`WadRayError::ZeroDivision`].

use alloy::primitives::U256;
use thiserror::Error;

/// 18-decimal fixed-point precision (`1e18`).
#[allow(clippy::unreadable_literal)] // u64 limb — group separator porly groups
pub const WAD: U256 = U256::from_limbs([0xde0b6b3a7640000, 0, 0, 0]);

/// Half of [`WAD`] (`5e17`).
#[allow(clippy::unreadable_literal)]
pub const HALF_WAD: U256 = U256::from_limbs([0x6f05b59d3b20000, 0, 0, 0]);

/// 27-decimal fixed-point precision (`1e27`).
#[allow(clippy::unreadable_literal)]
pub const RAY: U256 = U256::from_limbs([0x9fd0803ce8000000, 0x33b2e3c, 0, 0]);

/// Half of [`RAY`] (`5e26`).
#[allow(clippy::unreadable_literal)]
pub const HALF_RAY: U256 = U256::from_limbs([0x4fe8401e74000000, 0x19d971e, 0, 0]);

/// The ratio between ray and wad (`1e9`); `ray_to_wad` divides by this,
/// `wad_to_ray` multiplies.
#[allow(clippy::unreadable_literal)]
pub const WAD_RAY_RATIO: U256 = U256::from_limbs([0x3b9aca00, 0, 0, 0]);

/// `MAX_UINT256` (mirrors `degenbot.constants.MAX_UINT256`).
pub const MAX_UINT256: U256 = U256::MAX;

/// Errors from `WadRayMath` operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WadRayError {
    /// The product `a * b` (or `a * RAY` for `ray_div`) exceeds `MAX_UINT256`.
    #[error("wad/ray overflow: product exceeds MAX_UINT256")]
    Overflow,
    /// A division by zero was attempted.
    #[error("wad/ray division by zero")]
    ZeroDivision,
}

/// The rounding mode for a `ray_mul` / `ray_div` operation. Mirrors the
/// Python `wad_ray_math.Rounding` enum (`HALF_UP` / `FLOOR` / `CEIL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RayRounding {
    /// Default: round half up to nearest (`(a*b + HALF_RAY) // RAY`).
    #[default]
    HalfUp,
    /// Truncate toward zero (`(a*b) // RAY`).
    Floor,
    /// Round toward +∞ (`(a*b) // RAY + ((a*b) % RAY != 0)`).
    Ceil,
}

/// Multiply two wad values, rounding half up to the nearest wad.
/// Port of `wad_ray_math.wad_mul`.
///
/// # Errors
///
/// Returns [`WadRayError::Overflow`] if `a * b + HALF_WAD > MAX_UINT256`.
pub fn wad_mul(a: U256, b: U256) -> Result<U256, WadRayError> {
    // `a * b + HALF_WAD` cannot overflow if `a*b + HALF_WAD <= MAX_UINT256`
    // (the guard checked above). Use checked arithmetic to detect overflow.
    let ab = a.checked_mul(b).ok_or(WadRayError::Overflow)?;
    let numerator = ab.checked_add(HALF_WAD).ok_or(WadRayError::Overflow)?;
    if numerator > MAX_UINT256 {
        return Err(WadRayError::Overflow);
    }
    Ok(numerator / WAD)
}

/// Divide two wad values, rounding half up to the nearest wad.
/// Port of `wad_ray_math.wad_div`.
///
/// # Errors
///
/// Returns [`WadRayError::ZeroDivision`] if `b == 0`, or
/// [`WadRayError::Overflow`] if `a * WAD + b // 2 > MAX_UINT256`.
pub fn wad_div(a: U256, b: U256) -> Result<U256, WadRayError> {
    if b.is_zero() {
        return Err(WadRayError::ZeroDivision);
    }
    let half_b = b / U256::from(2u8);
    let numerator = a.checked_mul(WAD).ok_or(WadRayError::Overflow)?;
    let numerator = numerator.checked_add(half_b).ok_or(WadRayError::Overflow)?;
    if numerator > MAX_UINT256 {
        return Err(WadRayError::Overflow);
    }
    Ok(numerator / b)
}

/// Multiply two ray values with the specified [`RayRounding`] mode.
/// Port of `wad_ray_math.ray_mul`.
///
/// # Errors
///
/// Returns [`WadRayError::Overflow`] if the product (or `product + HALF_RAY`
/// for half-up) exceeds `MAX_UINT256`.
pub fn ray_mul(a: U256, b: U256, rounding: Option<RayRounding>) -> Result<U256, WadRayError> {
    match rounding {
        Some(RayRounding::Floor) => ray_mul_floor(a, b),
        Some(RayRounding::Ceil) => ray_mul_ceil(a, b),
        None | Some(RayRounding::HalfUp) => ray_mul_half_up(a, b),
    }
}

/// Multiply two ray values, rounding half up. Port of `wad_ray_math.ray_mul`
/// (the default no-rounding-arg path).
fn ray_mul_half_up(a: U256, b: U256) -> Result<U256, WadRayError> {
    let ab = a.checked_mul(b).ok_or(WadRayError::Overflow)?;
    let numerator = ab.checked_add(HALF_RAY).ok_or(WadRayError::Overflow)?;
    if numerator > MAX_UINT256 {
        return Err(WadRayError::Overflow);
    }
    Ok(numerator / RAY)
}

/// Multiply two ray values, truncating toward zero. Port of `ray_mul_floor`.
///
/// # Errors
///
/// Returns [`WadRayError::Overflow`] if `a * b > MAX_UINT256`.
pub fn ray_mul_floor(a: U256, b: U256) -> Result<U256, WadRayError> {
    let ab = a.checked_mul(b).ok_or(WadRayError::Overflow)?;
    if ab > MAX_UINT256 {
        return Err(WadRayError::Overflow);
    }
    Ok(ab / RAY)
}

/// Multiply two ray values, rounding toward +∞. Port of `ray_mul_ceil`.
///
/// # Errors
///
/// Returns [`WadRayError::Overflow`] if `a * b > MAX_UINT256`.
pub fn ray_mul_ceil(a: U256, b: U256) -> Result<U256, WadRayError> {
    let ab = a.checked_mul(b).ok_or(WadRayError::Overflow)?;
    if ab > MAX_UINT256 {
        return Err(WadRayError::Overflow);
    }
    let base = ab / RAY;
    let rem = ab % RAY;
    if rem.is_zero() {
        Ok(base)
    } else {
        Ok(base + U256::from(1u8))
    }
}

/// Divide two ray values with the specified [`RayRounding`] mode.
/// Port of `wad_ray_math.ray_div`.
///
/// # Errors
///
/// Returns [`WadRayError::ZeroDivision`] if `b == 0`, or
/// [`WadRayError::Overflow`] if `a * RAY + b // 2 > MAX_UINT256`.
pub fn ray_div(a: U256, b: U256, rounding: Option<RayRounding>) -> Result<U256, WadRayError> {
    match rounding {
        Some(RayRounding::Floor) => ray_div_floor(a, b),
        Some(RayRounding::Ceil) => ray_div_ceil(a, b),
        None | Some(RayRounding::HalfUp) => ray_div_half_up(a, b),
    }
}

/// Divide two ray values, rounding half up. Port of `ray_div` (default).
fn ray_div_half_up(a: U256, b: U256) -> Result<U256, WadRayError> {
    if b.is_zero() {
        return Err(WadRayError::ZeroDivision);
    }
    let half_b = b / U256::from(2u8);
    let numerator = a.checked_mul(RAY).ok_or(WadRayError::Overflow)?;
    let numerator = numerator.checked_add(half_b).ok_or(WadRayError::Overflow)?;
    if numerator > MAX_UINT256 {
        return Err(WadRayError::Overflow);
    }
    Ok(numerator / b)
}

/// Divide two ray values, truncating toward zero. Port of `ray_div_floor`.
///
/// # Errors
///
/// Returns [`WadRayError::ZeroDivision`] if `b == 0`, or
/// [`WadRayError::Overflow`] if `a * RAY > MAX_UINT256`.
pub fn ray_div_floor(a: U256, b: U256) -> Result<U256, WadRayError> {
    if b.is_zero() {
        return Err(WadRayError::ZeroDivision);
    }
    let numerator = a.checked_mul(RAY).ok_or(WadRayError::Overflow)?;
    if numerator > MAX_UINT256 {
        return Err(WadRayError::Overflow);
    }
    Ok(numerator / b)
}

/// Divide two ray values, rounding toward +∞. Port of `ray_div_ceil`.
///
/// # Errors
///
/// Returns [`WadRayError::ZeroDivision`] if `b == 0`, or
/// [`WadRayError::Overflow`] if `a * RAY > MAX_UINT256`.
pub fn ray_div_ceil(a: U256, b: U256) -> Result<U256, WadRayError> {
    if b.is_zero() {
        return Err(WadRayError::ZeroDivision);
    }
    let numerator = a.checked_mul(RAY).ok_or(WadRayError::Overflow)?;
    if numerator > MAX_UINT256 {
        return Err(WadRayError::Overflow);
    }
    let base = numerator / b;
    let rem = numerator % b;
    if rem.is_zero() {
        Ok(base)
    } else {
        Ok(base + U256::from(1u8))
    }
}

/// Cast a ray value down to wad, rounding half up to the nearest wad.
/// Port of `wad_ray_math.ray_to_wad`.
#[must_use]
pub fn ray_to_wad(a: U256) -> U256 {
    let quotient = a / WAD_RAY_RATIO;
    let rem = a % WAD_RAY_RATIO;
    let half_ratio = WAD_RAY_RATIO / U256::from(2u8);
    let tie_break = if rem > half_ratio {
        U256::from(1u8)
    } else {
        U256::ZERO
    };
    quotient + tie_break
}

/// Convert a wad value up to ray. Port of `wad_ray_math.wad_to_ray`.
///
/// # Errors
///
/// Returns [`WadRayError::Overflow`] if `a * WAD_RAY_RATIO > MAX_UINT256`.
pub fn wad_to_ray(a: U256) -> Result<U256, WadRayError> {
    a.checked_mul(WAD_RAY_RATIO).ok_or(WadRayError::Overflow)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    // ── constants ────────────────────────────────────────────────────────────

    #[test]
    fn constants_match_python_oracle() {
        assert_eq!(WAD, U256::from(10u128.pow(18)));
        assert_eq!(HALF_WAD, U256::from(5u128 * 10u128.pow(17)));
        assert_eq!(RAY, U256::from(10u128.pow(27)));
        assert_eq!(HALF_RAY, U256::from(5u128 * 10u128.pow(26)));
        assert_eq!(WAD_RAY_RATIO, U256::from(10u128.pow(9)));
        assert_eq!(MAX_UINT256, U256::MAX);
    }

    // ── ray_mul ──────────────────────────────────────────────────────────────

    #[test]
    fn ray_mul_half_up_basic() {
        // ray_mul(RAY, 2 RAY) = 2 RAY (result in ray scale: a*b*RAY/RAY = a*b).
        assert_eq!(
            ray_mul(RAY, U256::from(2u8) * RAY, None).unwrap(),
            U256::from(2u8) * RAY,
            "ray_mul(1 RAY, 2 RAY) = 2 RAY"
        );
        // ray_mul(1.5 RAY, 2 RAY) = 3 RAY.
        let one_half_ray = RAY + RAY / U256::from(2u8);
        assert_eq!(
            ray_mul(one_half_ray, U256::from(2u8) * RAY, None).unwrap(),
            U256::from(3u8) * RAY
        );
    }

    #[test]
    fn ray_mul_floor_truncates() {
        // ray_mul_floor(1.5, 1) where 1.5 is 1.5 RAY → 1 (truncates 1.5 → 1).
        let one_half_ray = RAY + RAY / U256::from(2u8);
        assert_eq!(
            ray_mul_floor(one_half_ray, U256::ONE),
            Ok(U256::ONE),
            "ray_mul_floor(1.5, 1) should truncate to 1"
        );
    }

    #[test]
    fn ray_mul_ceil_rounds_up() {
        // ray_mul_ceil(1.5, 1) → 2 (rounds 1.5 up to 2).
        let one_half_ray = RAY + RAY / U256::from(2u8);
        assert_eq!(
            ray_mul_ceil(one_half_ray, U256::ONE),
            Ok(U256::from(2u8)),
            "ray_mul_ceil(1.5, 1) should round up to 2"
        );
        // exact: ray_mul_ceil(2, 2) = 4 (no remainder → no round-up).
        assert_eq!(
            ray_mul_ceil(U256::from(2u8) * RAY, U256::from(2u8)),
            Ok(U256::from(4u8))
        );
    }

    // ── ray_div ──────────────────────────────────────────────────────────────

    #[test]
    fn ray_div_half_up_basic() {
        // ray_div(2 RAY, 2 RAY) = RAY (result in ray scale: 2/2 = 1).
        assert_eq!(
            ray_div(U256::from(2u8) * RAY, U256::from(2u8) * RAY, None).unwrap(),
            RAY
        );
    }

    #[test]
    fn ray_div_zero_returns_err() {
        assert_eq!(
            ray_div(RAY, U256::ZERO, None),
            Err(WadRayError::ZeroDivision)
        );
        assert_eq!(
            ray_div_floor(RAY, U256::ZERO),
            Err(WadRayError::ZeroDivision)
        );
        assert_eq!(
            ray_div_ceil(RAY, U256::ZERO),
            Err(WadRayError::ZeroDivision)
        );
    }

    #[test]
    fn ray_div_ceil_vs_floor_on_remainder() {
        // 10 RAY / 3 has a remainder (10 % 3 == 1); floor truncates, ceil bumps.
        let ten_ray = U256::from(10u8) * RAY;
        let three = U256::from(3u8);
        let floor = ray_div_floor(ten_ray, three).unwrap();
        let ceil = ray_div_ceil(ten_ray, three).unwrap();
        assert_eq!(
            ceil,
            floor + U256::from(1u8),
            "ceil bumps when there is a remainder"
        );
        // No-remainder case: floor == ceil (exact division).
        let six_ray = U256::from(6u8) * RAY;
        assert_eq!(
            ray_div_floor(six_ray, three).unwrap(),
            ray_div_ceil(six_ray, three).unwrap(),
            "no remainder → floor == ceil"
        );
    }

    // ── ray_to_wad / wad_to_ray ─────────────────────────────────────────────

    #[test]
    fn ray_to_wad_rounds_half_up() {
        // 1 RAY = 1e27 → 1e18 WAD (exact).
        assert_eq!(ray_to_wad(RAY), WAD);
        // RAY + WAD_RAY_RATIO/2: the remainder is EXACTLY half → does NOT round
        // up (Python uses strict `>`, so a tie stays down). Result = WAD.
        let exact_half = RAY + WAD_RAY_RATIO / U256::from(2u8);
        assert_eq!(
            ray_to_wad(exact_half),
            WAD,
            "exact half (tie) does NOT round up (strict `>` in the oracle)"
        );
        // RAY + WAD_RAY_RATIO/2 + 1: the remainder is just above half → rounds up to WAD+1.
        let just_above_half = RAY + WAD_RAY_RATIO / U256::from(2u8) + U256::from(1u8);
        assert_eq!(
            ray_to_wad(just_above_half),
            WAD + U256::from(1u8),
            "just above half rounds up to WAD+1"
        );
        // RAY + WAD_RAY_RATIO/2 - 1: below the tie → rounds down to WAD.
        let just_below_half = RAY + WAD_RAY_RATIO / U256::from(2u8) - U256::from(1u8);
        assert_eq!(
            ray_to_wad(just_below_half),
            WAD,
            "just below half rounds down to WAD"
        );
    }

    #[test]
    fn wad_to_ray_exact() {
        assert_eq!(wad_to_ray(WAD).unwrap(), RAY);
        assert_eq!(
            wad_to_ray(U256::from(2u8) * WAD).unwrap(),
            U256::from(2u8) * RAY
        );
        // overflow: MAX * WAD_RAY_RATIO overflows.
        assert_eq!(wad_to_ray(MAX_UINT256), Err(WadRayError::Overflow));
    }

    // ── wad_mul / wad_div ────────────────────────────────────────────────────

    #[test]
    fn wad_mul_basic() {
        // wad_mul(WAD, 2 WAD) = 2 WAD (both in wad scale).
        assert_eq!(
            wad_mul(WAD, U256::from(2u8) * WAD).unwrap(),
            U256::from(2u8) * WAD
        );
        assert_eq!(wad_mul(U256::from(0u8), WAD).unwrap(), U256::ZERO);
    }

    #[test]
    fn wad_div_basic() {
        // wad_div(2 WAD, 2 WAD) = WAD (result in wad scale: 2/2 = 1).
        assert_eq!(
            wad_div(U256::from(2u8) * WAD, U256::from(2u8) * WAD).unwrap(),
            WAD
        );
    }

    #[test]
    fn wad_operations_overflow() {
        assert_eq!(
            wad_mul(MAX_UINT256, MAX_UINT256),
            Err(WadRayError::Overflow)
        );
        assert_eq!(
            wad_div(MAX_UINT256, U256::from(1u8)),
            Err(WadRayError::Overflow)
        );
    }

    // ── overflow on ray ops ──────────────────────────────────────────────────

    #[test]
    fn ray_mul_overflow() {
        assert_eq!(
            ray_mul(MAX_UINT256, MAX_UINT256, None),
            Err(WadRayError::Overflow)
        );
        assert_eq!(
            ray_mul_floor(MAX_UINT256, MAX_UINT256),
            Err(WadRayError::Overflow)
        );
        assert_eq!(
            ray_mul_ceil(MAX_UINT256, MAX_UINT256),
            Err(WadRayError::Overflow)
        );
    }

    #[test]
    fn ray_div_overflow() {
        assert_eq!(
            ray_div(MAX_UINT256, U256::from(1u8), None),
            Err(WadRayError::Overflow)
        );
        assert_eq!(
            ray_div_floor(MAX_UINT256, U256::from(1u8)),
            Err(WadRayError::Overflow)
        );
        assert_eq!(
            ray_div_ceil(MAX_UINT256, U256::from(1u8)),
            Err(WadRayError::Overflow)
        );
    }

    /// Helper: why does the test use `ray_mul_floor` with a literal that should
    /// truncate? Computes `ray_mul(a, b, Some(Floor))` (= `ray_mul_floor(a, b)`).
    fn _floor_call(a: U256, b: U256) -> U256 {
        ray_mul(a, b, Some(RayRounding::Floor)).unwrap()
    }
}
