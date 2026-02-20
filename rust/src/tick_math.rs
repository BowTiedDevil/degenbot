//! Tick math calculations for Uniswap V3.
//!
//! This module provides high-performance implementations of:
//! - `get_sqrt_ratio_at_tick`: Converts tick to sqrt price (X96)
//! - `get_tick_at_sqrt_ratio`: Converts sqrt price (X96) to tick
//!
//! # Performance
//!
//! All constants are evaluated at compile time for zero runtime overhead.
//!
//! # Error Handling
//!
//! Functions return `Result<T, TickMathError>` for proper error handling.

use crate::errors::TickMathError;
use alloy_primitives::{
    aliases::{I24, I256, U160, U256},
    uint,
};
use num_bigint::BigUint;
use pyo3::{exceptions::PyTypeError, exceptions::PyValueError, prelude::*, types::PyAny};

/// Extract a U160 from a Python object (accepts int or bytes).
#[inline]
fn extract_u160(obj: &Bound<'_, PyAny>) -> PyResult<U160> {
    /// Number of bytes in a 160-bit word (U160).
    const BYTES_PER_WORD: usize = 20;

    if let Ok(bytes) = obj.extract::<&[u8]>() {
        if bytes.len() > BYTES_PER_WORD {
            return Err(PyErr::new::<PyValueError, _>(
                "Sqrt price X96 is too large (exceeds 20 bytes)",
            ));
        }
        return U160::try_from_be_slice(bytes).ok_or_else(|| {
            PyErr::new::<PyValueError, _>("Failed to parse sqrt_price_x96 from bytes")
        });
    }

    if let Ok(biguint) = obj.extract::<BigUint>() {
        if biguint.bits() > 160 {
            return Err(PyErr::new::<PyValueError, _>(
                "Sqrt price X96 is too large (exceeds 160 bits)",
            ));
        }
        let digits = biguint.to_u64_digits();
        let mut limbs = [0u64; 3];
        limbs[..digits.len()].copy_from_slice(&digits);
        return Ok(U160::from_limbs(limbs));
    }

    Err(PyErr::new::<PyTypeError, _>(
        "sqrt_price_x96 must be int or bytes",
    ))
}

/// Sqrt ratio constants and utilities for Uniswap V3 tick math.
pub struct SqrtRatio;

impl SqrtRatio {
    /// Minimum sqrt price ratio (at `MIN_TICK`).
    pub const MIN: U160 = uint!(4295128739_U160);
    /// Maximum sqrt price ratio (at `MAX_TICK`).
    pub const MAX: U160 = uint!(1461446703485210103287273052203988822378723970342_U160);
}

/// Minimum valid tick value for Uniswap V3.
pub const MIN_TICK: i32 = -887_272;
/// Maximum valid tick value for Uniswap V3.
pub const MAX_TICK: i32 = 887_272;

/// Tick mask lookup table for sqrt ratio calculation.
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

/// Converts a tick value to its corresponding sqrt price (X96 format).
///
/// This function calculates the sqrt price for a given tick value using the
/// Uniswap V3 tick math formula. The result is returned as a native Python int.
///
/// # Arguments
///
/// * `tick` - The tick value in range [-887272, 887272]
///
/// # Returns
///
/// A Python int representing the sqrt price X96 value
///
/// # Errors
///
/// Returns `PyValueError` if the tick value is invalid
///
/// # Example
///
/// ```
/// use degenbot_rs::tick_math::get_sqrt_ratio_at_tick_internal;
///
/// let ratio = get_sqrt_ratio_at_tick_internal(0).expect("Valid tick");
/// println!("Tick 0 ratio: {}", ratio);
/// ```
#[pyfunction(signature = (tick))]
pub fn get_sqrt_ratio_at_tick(py: Python<'_>, tick: i32) -> PyResult<Py<PyAny>> {
    let result = py.detach(|| get_sqrt_ratio_at_tick_internal(tick))?;
    let bytes: Vec<u8> = result.to_be_bytes::<20>().to_vec();

    let py_bytes = pyo3::types::PyBytes::new(py, &bytes);
    let int_class = py.get_type::<pyo3::types::PyInt>();
    let result = int_class.call_method1("from_bytes", (py_bytes, "big"))?;
    Ok(result.unbind())
}

/// Internal function to calculate sqrt ratio from tick.
#[inline]
pub fn get_sqrt_ratio_at_tick_internal(tick: i32) -> Result<U160, TickMathError> {
    const INTERMEDIATE_SHIFT: u32 = 128;
    const SQRT_RATIO_SHIFT: u32 = 32;
    const ONE_SHL_32: U256 = uint!(0x100000000_U256);

    // Validate tick is within valid range
    if !(MIN_TICK..=MAX_TICK).contains(&tick) {
        return Err(TickMathError::InvalidTick(tick));
    }

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

    Ok(sqrt_ratio.to::<U160>())
}

/// Converts a sqrt price (X96 format) to its corresponding tick value.
///
/// This function calculates the tick for a given sqrt price using the
/// Uniswap V3 tick math formula. The result is returned as a Python `i32`.
///
/// # Arguments
///
/// * `sqrt_price_x96` - The sqrt price X96 value as a Python `int` or `bytes`
///
/// # Returns
///
/// The tick value corresponding to the given sqrt price
///
/// # Errors
///
/// Returns `PyValueError` if:
/// - The input is too large (exceeds 20 bytes)
/// - The sqrt price is outside the valid [`MIN_SQRT_RATIO`, `MAX_SQRT_RATIO`) range
///
/// Returns `PyTypeError` if the input is not an int or bytes
///
/// # Example
///
/// ```
/// use degenbot_rs::tick_math::get_tick_at_sqrt_ratio_internal;
/// use alloy_primitives::U160;
/// use std::str::FromStr;
///
/// let sqrt_price = U160::from_str("79228162514264337593543950336").unwrap();
/// let tick = get_tick_at_sqrt_ratio_internal(sqrt_price).expect("Valid price");
/// println!("Calculated tick: {}", tick);
/// ```
#[pyfunction(signature = (sqrt_price_x96))]
pub fn get_tick_at_sqrt_ratio(py: Python<'_>, sqrt_price_x96: &Bound<'_, PyAny>) -> PyResult<i32> {
    let sqrt_price = extract_u160(sqrt_price_x96)?;
    let tick = py.detach(|| get_tick_at_sqrt_ratio_internal(sqrt_price))?;
    Ok(tick.as_i32())
}

// Internal function to calculate tick from sqrt ratio.
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

    if !(sqrt_price_x96 >= SqrtRatio::MIN && sqrt_price_x96 < SqrtRatio::MAX) {
        return Err(TickMathError::SqrtRatioOutOfBounds);
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

        // SAFETY: msb is a bit position (0-255), so msb - 128 is in [-128, 127].
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;
    use std::str::FromStr;

    #[test]
    fn test_tick_bounds_validation() -> Result<(), Box<dyn Error>> {
        // Test invalid ticks below minimum
        if let Err(TickMathError::InvalidTick(tick)) = get_sqrt_ratio_at_tick_internal(MIN_TICK - 1)
        {
            if tick == MIN_TICK - 1 {
                // pass
            } else {
                return Err("Wrong tick value returned".into());
            }
        } else {
            return Err("Should have rejected tick below MIN_TICK".into());
        }

        // Test invalid ticks above maximum
        if let Err(TickMathError::InvalidTick(tick)) = get_sqrt_ratio_at_tick_internal(MAX_TICK + 1)
        {
            if tick == MAX_TICK + 1 {
                // pass
            } else {
                return Err("Wrong tick value returned".into());
            }
        } else {
            return Err("Should have rejected tick above MAX_TICK".into());
        }

        // Test extreme invalid values
        if let Err(TickMathError::InvalidTick(tick)) = get_sqrt_ratio_at_tick_internal(i32::MIN) {
            if tick == i32::MIN {
                // pass
            } else {
                return Err("Wrong tick value returned".into());
            }
        } else {
            return Err("Should have rejected i32::MIN".into());
        }

        if let Err(TickMathError::InvalidTick(tick)) = get_sqrt_ratio_at_tick_internal(i32::MAX) {
            if tick == i32::MAX {
                // pass
            } else {
                return Err("Wrong tick value returned".into());
            }
        } else {
            return Err("Should have rejected i32::MAX".into());
        }

        Ok(())
    }

    #[test]
    fn test_tickmath() -> Result<(), Box<dyn Error>> {
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(MIN_TICK)?,
            U160::from_str("4295128739")?
        );
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(MIN_TICK + 1)?,
            U160::from_str("4295343490")?
        );
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(MAX_TICK - 1)?,
            U160::from_str("1461373636630004318706518188784493106690254656249")?
        );
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(MAX_TICK)?,
            U160::from_str("1461446703485210103287273052203988822378723970342")?,
        );
        Ok(())
    }

    #[test]
    fn test_tickmath_mid_values() -> Result<(), Box<dyn Error>> {
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(0)?,
            U160::from_str("79228162514264337593543950336")?
        );
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(1)?,
            U160::from_str("79232123823359799118286999568")?
        );
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(-1)?,
            U160::from_str("79224201403219477170569942574")?
        );
        Ok(())
    }

    #[test]
    fn test_tickmath_negative_values() -> Result<(), Box<dyn Error>> {
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(-100000)?,
            U160::from_str("533968626430936354154228408")?
        );
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(-500000)?,
            U160::from_str("1101692437043807371")?
        );
        Ok(())
    }

    #[test]
    fn test_tickmath_positive_values() -> Result<(), Box<dyn Error>> {
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(100000)?,
            U160::from_str("11755562826496067164730007768450")?
        );
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(500000)?,
            U160::from_str("5697689776495288729098254600827762987878")?
        );
        Ok(())
    }

    #[test]
    fn test_tickmath_additional_values() -> Result<(), Box<dyn Error>> {
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(10000)?,
            U160::from_str("130621891405341611593710811006")?
        );
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(-10000)?,
            U160::from_str("48055510970269007215549348797")?
        );
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(1000)?,
            U160::from_str("83290069058676223003182343270")?
        );
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(-1000)?,
            U160::from_str("75364347830767020784054125655")?
        );
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(100)?,
            U160::from_str("79625275426524748796330556128")?
        );
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(-100)?,
            U160::from_str("78833030112140176575862854579")?
        );
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(10)?,
            U160::from_str("79267784519130042428790663799")?
        );
        assert_eq!(
            get_sqrt_ratio_at_tick_internal(-10)?,
            U160::from_str("79188560314459151373725315960")?
        );
        Ok(())
    }

    #[test]
    fn test_tickmath_roundtrip() -> Result<(), Box<dyn Error>> {
        let ticks = [
            -500000, -100000, -10000, -1000, -100, -10, -1, 0, 1, 10, 100, 1000, 10000, 100000,
            500000,
        ];

        for tick in ticks {
            let ratio = get_sqrt_ratio_at_tick_internal(tick)?;
            let tick_back = get_tick_at_sqrt_ratio_internal(ratio)?;
            assert_eq!(tick_back.as_i32(), tick);
        }
        Ok(())
    }

    #[test]
    fn test_tickmath_boundary_roundtrip() -> Result<(), Box<dyn Error>> {
        let min_ratio = get_sqrt_ratio_at_tick_internal(-887272)?;
        assert_eq!(
            get_tick_at_sqrt_ratio_internal(min_ratio)?,
            I24::unchecked_from(-887272)
        );

        let max_ratio = get_sqrt_ratio_at_tick_internal(887272)?;
        let max_ratio_minus_one = U256::from(max_ratio) - U256::ONE;
        assert_eq!(
            get_tick_at_sqrt_ratio_internal(U160::from(max_ratio_minus_one))?,
            I24::unchecked_from(887271)
        );
        Ok(())
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn roundtrip_any_valid_tick(tick in -887272i32..=887272i32) {
            let ratio = get_sqrt_ratio_at_tick_internal(tick)?;
            let tick_back = get_tick_at_sqrt_ratio_internal(ratio)?;
            prop_assert_eq!(tick_back.as_i32(), tick);
        }

        #[test]
        fn tick_produces_monotonically_increasing_prices(tick_a in -887272i32..=887272i32, tick_b in -887272i32..=887272i32) {
            let ratio_a = get_sqrt_ratio_at_tick_internal(tick_a)?;
            let ratio_b = get_sqrt_ratio_at_tick_internal(tick_b)?;

            if tick_a < tick_b {
                prop_assert!(ratio_a < ratio_b, "Price should increase with tick");
            } else if tick_a > tick_b {
                prop_assert!(ratio_a > ratio_b, "Price should decrease with tick");
            } else {
                prop_assert_eq!(ratio_a, ratio_b, "Same tick should produce same price");
            }
        }

        #[test]
        fn tick_0_produces_correct_price(tick in Just(0i32)) {
            let ratio = get_sqrt_ratio_at_tick_internal(tick)?;
            // sqrt(1.0001^0) * 2^96 = 1 * 2^96 = 2^96
            let expected = U160::from(1u128) << 96;
            prop_assert_eq!(ratio, expected);
        }
    }
}
