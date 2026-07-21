//! `TickMath`: conversions between tick and sqrt-price.
//!
//! Port of `TickMath.sol` from both Uniswap V3 and V4.
//!
//! Both V3 and V4 use the same mathematical formulas with the same
//! valid tick range `[-887272, 887272]` and the same sqrt-ratio range.
//! The V3 naming is `get_sqrt_ratio_at_tick` / `get_tick_at_sqrt_ratio`;
//! the V4 naming is `get_sqrt_price_at_tick` / `get_tick_at_sqrt_price`.
//! Both names are provided as aliases here.
//!
//! See: `contract_reference/uniswap/V3/UniswapV3Factory.sol` (`TickMath` library)
//! See: `contract_reference/uniswap/V4/PoolManager.sol` (`TickMath` library)

use alloy::primitives::{
    aliases::{I24, I256, U160, U256},
    uint,
};

use degenbot_core::errors::TickMathError;

/// Minimum sqrt price ratio (at [`MIN_TICK`]).
pub const MIN_SQRT_RATIO: U160 = uint!(4295128739_U160);
/// Maximum sqrt price ratio (at [`MAX_TICK`]).
pub const MAX_SQRT_RATIO: U160 = uint!(1461446703485210103287273052203988822378723970342_U160);

/// Minimum valid tick value for Uniswap V3 and V4.
pub const MIN_TICK: i32 = -887_272;
/// Maximum valid tick value for Uniswap V3 and V4.
pub const MAX_TICK: i32 = 887_272;

/// Bit masks and ratio multipliers for the 19 most significant bits of the tick.
///
/// Each entry `(bit_mask, multiplier)` corresponds to a power-of-2 bit position.
/// If that bit is set in `abs_tick`, the ratio is multiplied by the corresponding
/// factor and right-shifted by 128 to maintain fixed-point precision.
///
/// These are the pre-computed values of `(2^(1/2^i) - 1) * 2^128` for i = 0..18,
/// derived from the identity: `√(1.0001^tick) ≈ ∏(2^(bit_i / 2^i))`.
/// Reference: Uniswap V3 `TickMath.sol`, lines 50–68.
const TICK_MASKS: [(U256, U256); 19] = uint!([
    (0x2_U256, 0xfff97272373d413259a46990580e213a_U256),
    (0x4_U256, 0xfff2e50f5f656932ef12357cf3c7fdcc_U256),
    (0x8_U256, 0xffe5caca7e10e4e61c3624eaa0941cd0_U256),
    (0x10_U256, 0xffcb9843d60f6159c9db58835c926644_U256),
    (0x20_U256, 0xff973b41fa98c081472e6896dfb254c0_U256),
    (0x40_U256, 0xff2ea16466c96a3843ec78b326b52861_U256),
    (0x80_U256, 0xfe5dee046a99a2a811c461f1969c3053_U256),
    (0x100_U256, 0xfcbe86c7900a88aedcffc83b479aa3a4_U256),
    (0x200_U256, 0xf987a7253ac413176f2b074cf7815e54_U256),
    (0x400_U256, 0xf3392b0822b70005940c7a398e4b70f3_U256),
    (0x800_U256, 0xe7159475a2c29b7443b29c7fa6e889d9_U256),
    (0x1000_U256, 0xd097f3bdfd2022b8845ad8f792aa5825_U256),
    (0x2000_U256, 0xa9f746462d870fdf8a65dc1f90e061e5_U256),
    (0x4000_U256, 0x70d869a156d2a1b890bb3df62baf32f7_U256),
    (0x8000_U256, 0x31be135f97d08fd981231505542fcfa6_U256),
    (0x10000_U256, 0x9aa508b5b7a84e1c677de54f3e99bc9_U256),
    (0x20000_U256, 0x5d6af8dedb81196699c329225ee604_U256),
    (0x40000_U256, 0x2216e584f5fa1ea926041bedfe98_U256),
    (0x80000_U256, 0x48a170391f7dc42444e8fa2_U256),
]);

/// Internal function to calculate sqrt ratio from tick.
///
/// # Errors
///
/// Returns [`TickMathError::InvalidTick`] if the tick is outside [-887272, 887272].
#[inline]
pub fn get_sqrt_ratio_at_tick_internal(tick: i32) -> Result<U160, TickMathError> {
    const INTERMEDIATE_SHIFT: u32 = 128;
    const SQRT_RATIO_SHIFT: u32 = 32;
    const ONE_SHL_32: U256 = uint!(0x100000000_U256);

    // Validate tick is within valid range
    if !(MIN_TICK..=MAX_TICK).contains(&tick) {
        return Err(TickMathError::InvalidTick(tick));
    }

    // SAFETY: Range check above guarantees tick ∈ [-887272, 887272],
    // far from i32::MIN, so unsigned_abs() cannot panic.
    let abs_tick: U256 = U256::from(tick.unsigned_abs());

    let mut ratio: U256 = if abs_tick & U256::ONE == U256::ZERO {
        uint!(0x100000000000000000000000000000000_U256)
    } else {
        uint!(0xfffcb933bd6fad37aa2d162d1a594001_U256)
    };

    for (tick_mask, ratio_multiplier) in TICK_MASKS {
        if (abs_tick & tick_mask) != U256::ZERO {
            ratio = (ratio * ratio_multiplier) >> INTERMEDIATE_SHIFT;
        }
    }

    if tick > 0 {
        ratio = U256::MAX / ratio;
    }

    let mut sqrt_ratio: U256 = ratio >> SQRT_RATIO_SHIFT;
    if (ratio % ONE_SHL_32) != U256::ZERO {
        sqrt_ratio += U256::ONE;
    }

    let result = sqrt_ratio.to::<U160>();
    debug_assert!(U256::from(result) == sqrt_ratio, "ratio overflowed U160");
    Ok(result)
}

/// Internal function to calculate tick from sqrt ratio.
///
/// # Errors
///
/// Returns [`TickMathError::SqrtRatioOutOfBounds`] if the sqrt price is outside
/// the valid [`MIN_SQRT_RATIO`, `MAX_SQRT_RATIO`) range.
#[inline]
pub fn get_tick_at_sqrt_ratio_internal(sqrt_price_x96: U160) -> Result<I24, TickMathError> {
    const FACTOR_SHIFT_VALUES: [(u128, u8); 8] = [
        (0xFFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF, 7),
        (0xFFFF_FFFF_FFFF_FFFF, 6),
        (0xFFFF_FFFF, 5),
        (0xFFFF, 4),
        (0xFF, 3),
        (0xF, 2),
        (0x3, 1),
        (0x1, 0),
    ];
    const LOG_SQRT10001_MULT: I256 = I256::from_raw(uint!(255738958999603826347141_U256));
    const TICK_LOW_OFFSET: I256 = I256::from_raw(uint!(3402992956809132418596140100660247210_U256));
    const TICK_HIGH_OFFSET: I256 =
        I256::from_raw(uint!(291339464771989622907027621153398088495_U256));

    if !(sqrt_price_x96 >= MIN_SQRT_RATIO && sqrt_price_x96 < MAX_SQRT_RATIO) {
        return Err(TickMathError::SqrtRatioOutOfBounds {
            actual: sqrt_price_x96,
            min: MIN_SQRT_RATIO,
            max: MAX_SQRT_RATIO,
        });
    }

    let ratio: U256 = U256::from(sqrt_price_x96) << 32;
    let mut r: U256 = ratio;
    let mut msb: U256 = U256::ZERO;
    let mut f: U256;

    for (factor, shift_bits) in FACTOR_SHIFT_VALUES {
        f = U256::from(r > factor) << shift_bits;
        msb |= f;
        r >>= f;
    }

    let mut log_2: I256;
    {
        let msb_usize: usize = msb.to();

        r = if msb_usize >= 128 {
            ratio >> (msb_usize - 127)
        } else {
            ratio << (127 - msb_usize)
        };

        // SAFETY: msb_usize is a bit position (0–255) which fits in I256.
        // 128 also fits in I256. The difference is in [-128, 127], well within I256 range.
        // Shifting by 64 bits is always safe for I256 (256-bit integer).
        log_2 = (I256::unchecked_from(msb_usize) - I256::unchecked_from(128)).wrapping_shl(64);
    }

    for shift_factor in (51..=63).rev() {
        r = (r * r) >> 127;
        f = r >> 128;
        log_2 |= I256::unchecked_from(f) << shift_factor;
        r >>= f;
    }

    r = (r * r) >> 127;
    f = r >> 128;
    log_2 |= I256::unchecked_from(f) << 50;

    let log_sqrt10001: I256 = log_2 * LOG_SQRT10001_MULT;

    let log_sqrt_low = log_sqrt10001 - TICK_LOW_OFFSET;
    let log_sqrt_high = log_sqrt10001 + TICK_HIGH_OFFSET;

    // asr(128) is arithmetic shift right for signed integers, preserving the sign bit
    let tick_low: I24 = log_sqrt_low.asr(128).to::<I24>();
    let tick_high: I24 = log_sqrt_high.asr(128).to::<I24>();

    if tick_low == tick_high {
        Ok(tick_low)
    } else {
        let high_ratio = get_sqrt_ratio_at_tick_internal(tick_high.as_i32())?;
        if high_ratio <= sqrt_price_x96 {
            Ok(tick_high)
        } else {
            Ok(tick_low)
        }
    }
}

/// Given a tick spacing, compute the maximum usable tick.
///
/// Returns the maximum tick that is a multiple of `tick_spacing`.
#[must_use]
pub const fn max_usable_tick(tick_spacing: i32) -> i32 {
    (MAX_TICK / tick_spacing) * tick_spacing
}

/// Given a tick spacing, compute the minimum usable tick.
///
/// Returns the minimum tick that is a multiple of `tick_spacing`.
/// Uses EVM-style division (truncation toward zero) to match Solidity semantics.
#[must_use]
pub const fn min_usable_tick(tick_spacing: i32) -> i32 {
    // EVM division truncates toward zero for signed integers.
    // Rust's `/` operator also truncates toward zero.
    (MIN_TICK / tick_spacing) * tick_spacing
}

/// Compress a tick by the spacing, rounding toward negative infinity.
///
/// Matches Solidity's `tick / tick_spacing` with rounding toward zero,
/// then adjusting for negative values.
#[must_use]
pub const fn compress(tick: i32, tick_spacing: i32) -> i32 {
    tick.div_euclid(tick_spacing)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::str::FromStr;

    #[test]
    fn test_tick_bounds_validation() {
        assert!(matches!(
            get_sqrt_ratio_at_tick_internal(MIN_TICK - 1),
            Err(TickMathError::InvalidTick(t)) if t == MIN_TICK - 1
        ));
        assert!(matches!(
            get_sqrt_ratio_at_tick_internal(MAX_TICK + 1),
            Err(TickMathError::InvalidTick(t)) if t == MAX_TICK + 1
        ));
    }

    #[test]
    fn test_sqrt_ratio_out_of_bounds() {
        assert!(matches!(
            get_tick_at_sqrt_ratio_internal(U160::ZERO),
            Err(TickMathError::SqrtRatioOutOfBounds { .. })
        ));
        assert!(matches!(
            get_tick_at_sqrt_ratio_internal(MAX_SQRT_RATIO),
            Err(TickMathError::SqrtRatioOutOfBounds { .. })
        ));
    }

    #[test]
    fn test_tickmath() {
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(MIN_TICK).unwrap(),
            U160::from_str("4295128739").unwrap()
        );
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(MAX_TICK).unwrap(),
            U160::from_str("1461446703485210103287273052203988822378723970342").unwrap(),
        );
    }

    #[test]
    fn test_tickmath_mid_values() {
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(0).unwrap(),
            U160::from_str("79228162514264337593543950336").unwrap()
        );
    }

    #[test]
    fn test_tickmath_roundtrip() {
        let ticks = [
            -500_000, -100_000, -10_000, -1_000, -100, -10, -1, 0, 1, 10, 100, 1_000, 10_000,
            100_000, 500_000,
        ];

        for tick in ticks {
            let ratio = get_sqrt_ratio_at_tick_internal(tick).unwrap();
            let tick_back = get_tick_at_sqrt_ratio_internal(ratio).unwrap();
            assert_eq!(tick_back.as_i32(), tick);
        }
    }

    #[test]
    fn test_max_usable_tick() {
        assert_eq!(max_usable_tick(60), 887_220);
        assert_eq!(max_usable_tick(1), 887_272);
    }

    #[test]
    fn test_min_usable_tick() {
        assert_eq!(min_usable_tick(60), -887_220);
        assert_eq!(min_usable_tick(1), -887_272);
    }

    #[test]
    fn test_compress() {
        assert_eq!(compress(100, 60), 1);
        assert_eq!(compress(-100, 60), -2); // -100 // 60 = -2 (floor division)
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn roundtrip_any_valid_tick(tick in -887_272_i32..887_272_i32) {
            let ratio = get_sqrt_ratio_at_tick_internal(tick)?;
            let tick_back = get_tick_at_sqrt_ratio_internal(ratio)?;
            prop_assert_eq!(tick_back.as_i32(), tick);
        }
    }
}
