//! Balancer V2 `LogExpMath` — fixed-point exponentiation via Taylor
//! series. Direct port of `src/degenbot/balancer/libraries/log_exp_math.py`.
//!
//! Internally uses signed 256-bit (`[`alloy::primitives::I256`]`) arithmetic
//! because the deployed Solidity treats these quantities as `int256` —
//! `ln(x)` can return negative values (when `x < ONE`), `pow` takes a
//! "signed range" `x` (the high bit asserted clear), and intermediate
//! `y * ln(x)` products can be negative.
//!
//! `_truncated_div` is the Solidity `/` (truncates toward zero). Python's
//! `//` floors toward -∞ for negative dividends — the Python oracle has a
//! helper to wrap that. Rust's `I256::Div` truncates toward zero (two's
//! complement signed division), so the helper is a no-op kept for the 1:1
//! port correspondence + single panic-on-overflow seam.

use alloy::primitives::{I256, U256};

use crate::BalancerMathError;

/// `Result` alias for the LogExpMath port.
pub type Result<T> = std::result::Result<T, BalancerMathError>;

// ---------------------------------------------------------------------------
// Decimal fixed-point bases
// ---------------------------------------------------------------------------

/// 1.0 in 18-decimal fixed point (the unit of `pow`/`exp`/`ln` inputs and
/// outputs). Matches Python `ONE_18`.
pub const ONE_18: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);

/// 1.0 in 18-decimal fixed point as I256 (used by `ln`/`pow` boundary
/// comparisons). `[1e18, 0, 0, 0]` reinterpreted as two's complement
/// (high bit clear → positive 1e18).
pub const ONE_18_I: I256 = I256::from_raw(U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]));

/// 10^20 spans two limbs (10^20 > 2^64). Computed via Python:
/// `limbs_u256(10**20) = [7766279631452241920, 5, 0, 0]`.
const ONE_20: I256 = I256::from_raw(U256::from_limbs([7_766_279_631_452_241_920, 5, 0, 0]));

/// 10^36 spans two limbs. Computed via Python:
/// `limbs_u256(10**36) = [12919594847110692864, 54210108624275221, 0, 0]`.
const ONE_36: I256 = I256::from_raw(U256::from_limbs([
    12_919_594_847_110_692_864,
    54_210_108_624_275_221,
    0,
    0,
]));

// ---------------------------------------------------------------------------
// Domain bounds
// ---------------------------------------------------------------------------

/// `MAX_NATURAL_EXPONENT = 130e18` — the upper bound on `exp` arguments.
///
/// Internally the result is stored using 20 decimals, so the largest
/// representable result is `(2^255-1) / 10^20`; the largest exponent whose
/// `exp` fits is `ln((2^255-1) / 10^20) ≈ 130.700829182905140221`. We use
/// `130.0` for safety margin. Two limbs (130e18 > 2^64).
const MAX_NATURAL_EXPONENT: I256 =
    I256::from_raw(U256::from_limbs([872_791_484_033_138_688, 7, 0, 0]));

/// `MIN_NATURAL_EXPONENT = -41e18` — the lower bound on `exp` arguments.
///
/// The smallest representable result is `10^(-18)`; the largest negative
/// argument is `ln(10^(-18)) ≈ -41.446531673892822312`. We use `-41.0` for
/// safety margin. Stored as the two's-complement bit pattern (high bit set).
const MIN_NATURAL_EXPONENT: I256 = I256::from_raw(U256::from_limbs([
    14_340_232_221_128_654_848,
    18_446_744_073_709_551_613,
    18_446_744_073_709_551_615,
    18_446_744_073_709_551_615,
]));

/// `LN_36_LOWER_BOUND = ONE_18 - 1e17 = 0.9e18` — bounds for `ln_36`'s
/// argument (both `ln(0.9)` and `ln(1.1)` are representable with 36
/// decimals in a 256-bit int).
const LN_36_LOWER_BOUND: U256 = U256::from_limbs([900_000_000_000_000_000, 0, 0, 0]);

/// `LN_36_UPPER_BOUND = ONE_18 + 1e17 = 1.1e18`.
const LN_36_UPPER_BOUND: U256 = U256::from_limbs([1_100_000_000_000_000_000, 0, 0, 0]);

/// `MILD_EXPONENT_BOUND = 2^254 / ONE_20` — guards `y` against overflow when
/// computing `y * ln(x)` in the `pow` entry. Computed via Python:
/// `limbs_u256(2**254 // 10**20) = [4720311721447089458, 12146009947018874712, 850705917302346158, 0]`.
const MILD_EXPONENT_BOUND: U256 = U256::from_limbs([
    4_720_311_721_447_089_458,
    12_146_009_947_018_874_712,
    850_705_917_302_346_158,
    0,
]);

/// `2^255` — used as the `x_int256` range bound on `pow`'s `x` argument.
const TWO_255: U256 = U256::from_limbs([0, 0, 0, 9_223_372_036_854_775_808]);

// ---------------------------------------------------------------------------
// Precomputed `e^(2^n)` table — 0 decimals for a0/a1, 20 decimals for
// a2..a11. x0..x11 are the `2^n` exponents they correspond to.
// ---------------------------------------------------------------------------

// 18-decimal exponents (a0, a1 are 0-decimal magnitudes; x0, x1 are 18-dp).
const X0: I256 = I256::from_raw(U256::from_limbs([17_319_535_557_742_690_304, 6, 0, 0]));
const X1: I256 = I256::from_raw(U256::from_limbs([8_659_767_778_871_345_152, 3, 0, 0]));

// a0, a1 are plain integers (0 decimals), e^(2^7) / e^(2^6).
const A0: I256 = I256::from_raw(U256::from_limbs([
    171_843_153_341_448_192,
    17_670_479_068_478_958_691,
    114_249_481_722_274_167,
    0,
]));
const A1: I256 = I256::from_raw(U256::from_limbs([
    17_696_838_799_657_497_472,
    338_008_108,
    0,
    0,
]));

// 20-decimal exponents + magnitudes (x2..x11 / a2..a11).
const X2: I256 = I256::from_raw(U256::from_limbs([8_713_275_248_247_570_432, 173, 0, 0]));
const X3: I256 = I256::from_raw(U256::from_limbs([13_580_009_660_978_561_024, 86, 0, 0]));
const X4: I256 = I256::from_raw(U256::from_limbs([6_790_004_830_489_280_512, 43, 0, 0]));
const X5: I256 = I256::from_raw(U256::from_limbs([12_618_374_452_099_416_064, 21, 0, 0]));
const X6: I256 = I256::from_raw(U256::from_limbs([15_532_559_262_904_483_840, 10, 0, 0]));
const X7: I256 = I256::from_raw(U256::from_limbs([7_766_279_631_452_241_920, 5, 0, 0]));
const X8: I256 = I256::from_raw(U256::from_limbs([13_106_511_852_580_896_768, 2, 0, 0]));
const X9: I256 = I256::from_raw(U256::from_limbs([6_553_255_926_290_448_384, 1, 0, 0]));
const X10: I256 = I256::from_raw(U256::from_limbs([12_500_000_000_000_000_000, 0, 0, 0]));
const X11: I256 = I256::from_raw(U256::from_limbs([6_250_000_000_000_000_000, 0, 0, 0]));

const A2: I256 = I256::from_raw(U256::from_limbs([
    17_871_857_890_508_685_312,
    428_059_064_879_743,
    0,
    0,
]));
const A3: I256 = I256::from_raw(U256::from_limbs([
    12_108_528_782_385_981_184,
    48_171_701,
    0,
    0,
]));
const A4: I256 = I256::from_raw(U256::from_limbs([14_861_217_100_182_911_056, 16_159, 0, 0]));
const A5: I256 = I256::from_raw(U256::from_limbs([18_025_501_570_106_181_090, 295, 0, 0]));
const A6: I256 = I256::from_raw(U256::from_limbs([1_035_846_944_682_958_083, 40, 0, 0]));
const A7: I256 = I256::from_raw(U256::from_limbs([13_573_765_813_970_800_912, 14, 0, 0]));
const A8: I256 = I256::from_raw(U256::from_limbs([17_298_174_480_336_401_757, 8, 0, 0]));
const A9: I256 = I256::from_raw(U256::from_limbs([17_722_077_226_516_838_711, 6, 0, 0]));
const A10: I256 = I256::from_raw(U256::from_limbs([2_634_380_864_425_321_987, 6, 0, 0]));
const A11: I256 = I256::from_raw(U256::from_limbs([14_215_725_523_238_184_876, 5, 0, 0]));

const ONE_18_SIGNED: I256 = ONE_18_I;

// ---------------------------------------------------------------------------
// signed truncated division (Solidity `/` semantics)
// ---------------------------------------------------------------------------

/// Integer division that matches Solidity's truncation-toward-zero
/// semantics.
///
/// Solidity's signed `/` truncates toward zero. Rust's `I256::Div` does
/// the same (two's complement signed division). Python's `//` floors
/// toward -∞ for negative dividends — the Python oracle wraps it in
/// `_truncated_div`. In Rust, `a / b` already truncates toward zero.
///
/// # Panics
///
/// Panics on `MIN / -1` (would overflow `I256::MAX`) — matching the
/// Solidity revert. Unreachable for the bounded log/exp inputs.
#[inline]
pub(crate) fn truncated_div(a: I256, b: I256) -> I256 {
    a / b
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// `pow(x, y) = x ^ y` raising `x` to the power `y` using fixed-point
/// exponentiation. Returns a non-negative `U256` (the product).
///
/// Instead of computing `x^y` directly, this uses `ln`/`exp` identities:
/// `x^y = exp(y * ln(x))`.
///
/// # Errors
///
/// - [`BalancerMathError::XOutOfBounds`] if `x >= 2^255` (the signed-256
///   range).
/// - [`BalancerMathError::YOutOfBounds`] if `y >= MILD_EXPONENT_BOUND`.
/// - [`BalancerMathError::ProductOutOfBounds`] if `y * ln(x)` is outside
///   `[MIN_NATURAL_EXPONENT, MAX_NATURAL_EXPONENT]`.
#[allow(clippy::fn_params_excessive_bools)]
pub fn pow(x: U256, y: I256) -> Result<U256> {
    if y.is_zero() {
        // 0^0 indeterminate: defined as 1 (matching deployed contract).
        return Ok(ONE_18);
    }
    if x.is_zero() {
        return Ok(U256::ZERO);
    }

    // `x` must fit in signed 256-bit range.
    if x >= TWO_255 {
        return Err(BalancerMathError::XOutOfBounds);
    }
    let x_int256 = I256::from_raw(x);

    // `y` must fit in "mild" range to prevent `y * ln(x)` overflow AND
    // guarantee `y` fits in signed range. The check is asymmetric
    // (Python: `if y >= MILD_EXPONENT_BOUND` — only rejects large
    // POSITIVE y; very-negative y is rejected later via the
    // MIN_NATURAL_EXPONENT bound on `logx_times_y`). Compare as I256 —
    // `into_raw()` would reinterpret negative y as a huge unsigned and
    // spuriously raise.
    let mild_signed = I256::from_raw(MILD_EXPONENT_BOUND);
    if y >= mild_signed {
        return Err(BalancerMathError::YOutOfBounds);
    }
    let y_int256 = y;

    // Compute `y * ln(x)`, leaving the `/ONE_18` (fp-mult correction) to
    // the end. Two paths depending on x's proximity to ONE (ln_36 is the
    // high-precision variant for near-ONE arguments).
    let logx_times_y = if LN_36_LOWER_BOUND < x && x < LN_36_UPPER_BOUND {
        let ln_36_x = ln_36(x_int256);
        // ln_36_x has 36 decimals; the Solidity splits the mult into two
        // 18-decimal mults to avoid 256-bit overflow. Python uses arbitrary
        // precision to multiply directly. Rust's I256 has the headroom.
        truncated_div(ln_36_x * y_int256, ONE_18_SIGNED)
    } else {
        ln_internal(x_int256) * y_int256
    };
    let logx_times_y = truncated_div(logx_times_y, ONE_18_SIGNED);

    if !(logx_times_y >= MIN_NATURAL_EXPONENT && logx_times_y <= MAX_NATURAL_EXPONENT) {
        return Err(BalancerMathError::ProductOutOfBounds);
    }

    let result = exp_internal(logx_times_y)?;
    // Result is non-negative: convert back to U256 via raw reinterpret.
    Ok(result.into_raw())
}

/// `exp(x) = e^x`. Public entry: domain-checks `x` against
/// `[MIN_NATURAL_EXPONENT, MAX_NATURAL_EXPONENT]`.
///
/// # Errors
///
/// [`BalancerMathError::OutOfBounds`] if `x` is outside the natural
/// exponent domain.
pub fn exp(x: I256) -> Result<U256> {
    if !(x >= MIN_NATURAL_EXPONENT && x <= MAX_NATURAL_EXPONENT) {
        return Err(BalancerMathError::OutOfBounds);
    }
    let result = exp_internal(x)?;
    Ok(result.into_raw())
}

/// `ln(a) = ln(a)` (natural logarithm). Returns the signed 18-decimal
/// fixed-point result.
///
/// # Errors
///
/// [`BalancerMathError::OutOfBounds`] if `a <= 0` (ln undefined).
pub fn ln(a: U256) -> Result<I256> {
    // The natural logarithm is undefined for non-positive arguments.
    if a.is_zero() {
        return Err(BalancerMathError::OutOfBounds);
    }
    let a_int256 = I256::from_raw(a);
    if a_int256.is_negative() {
        return Err(BalancerMathError::OutOfBounds);
    }

    if LN_36_LOWER_BOUND < a && a < LN_36_UPPER_BOUND {
        return Ok(truncated_div(ln_36(a_int256), ONE_18_SIGNED));
    }
    Ok(ln_internal(a_int256))
}

/// `log(arg, base) = log(arg) / log(base)` — base-change via `ln`.
///
/// Both `log_base` and `log_arg` are computed as 36-decimal fixed point
/// (using `ln_36` where possible, or `ln` upscaled by `ONE_18`).
pub fn log(arg: U256, base: U256) -> Result<I256> {
    let log_base = if LN_36_LOWER_BOUND < base && base < LN_36_UPPER_BOUND {
        ln_36(I256::from_raw(base))
    } else {
        ln_internal(I256::from_raw(base)) * ONE_18_SIGNED
    };
    let log_arg = if LN_36_LOWER_BOUND < arg && arg < LN_36_UPPER_BOUND {
        ln_36(I256::from_raw(arg))
    } else {
        ln_internal(I256::from_raw(arg)) * ONE_18_SIGNED
    };
    // The `/ log_base` (fp-div) corrects the 18-decimal scaling introduced
    // by the multiplications above.
    Ok(truncated_div(log_arg * ONE_18_SIGNED, log_base))
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// `exp` core (no domain check — assumes `x` is within bounds).
fn exp_internal(x: I256) -> Result<I256> {
    debug_assert!(
        x >= MIN_NATURAL_EXPONENT && x <= MAX_NATURAL_EXPONENT,
        "exp_internal domain violation"
    );

    if x.is_negative() {
        // e^(-x) computed as 1 / e^x. `x` is in signed range (it's >=
        // MIN_NATURAL_EXPONENT, which fits). Fixed-point division requires
        // `* ONE_18`.
        let positive = -x;
        let denom = exp_internal(positive)?;
        // `(ONE_18 * ONE_18) / denom` — both non-negative; result non-negative.
        let product = ONE_18_SIGNED
            .checked_mul(ONE_18_SIGNED)
            .ok_or(BalancerMathError::MulOverflow)?;
        return Ok(truncated_div(product, denom));
    }

    // Decompose x into a sum of precomputed powers of two (e^x_n = a_n), using
    // the largest possible first term so the remainder is small. The first
    // two a_n (a0, a1) are stored with 0 decimals because they're too large
    // for 18-decimal 256-bit; x0+x1 > MAX_NATURAL_EXPONENT so only one of
    // them appears in the decomposition.
    let mut x = x;
    let first_an: I256;
    if x >= X0 {
        x -= X0;
        first_an = A0;
    } else if x >= X1 {
        x -= X1;
        first_an = A1;
    } else {
        first_an = I256::try_from(1i64).expect("1 fits");
    }

    // Promote x to 20-decimal fixed point for higher precision on the
    // remaining small terms.
    x *= I256::try_from(100i64).expect("100 fits");

    // `product` accumulates the 20-decimal-fixed-point a_n product.
    let mut product = ONE_20;
    if x >= X2 {
        x -= X2;
        product = truncated_div(product * A2, ONE_20);
    }
    if x >= X3 {
        x -= X3;
        product = truncated_div(product * A3, ONE_20);
    }
    if x >= X4 {
        x -= X4;
        product = truncated_div(product * A4, ONE_20);
    }
    if x >= X5 {
        x -= X5;
        product = truncated_div(product * A5, ONE_20);
    }
    if x >= X6 {
        x -= X6;
        product = truncated_div(product * A6, ONE_20);
    }
    if x >= X7 {
        x -= X7;
        product = truncated_div(product * A7, ONE_20);
    }
    if x >= X8 {
        x -= X8;
        product = truncated_div(product * A8, ONE_20);
    }
    if x >= X9 {
        x -= X9;
        product = truncated_div(product * A9, ONE_20);
    }

    // x10, x11 are unnecessary — 20-decimal precision is already enough.

    // Taylor series: e^x = 1 + x + x^2/2! + x^3/3! + ... + x^12/12!.
    // 12 terms suffice for 18-decimal precision. Each recurrence divides
    // by ONE_20 first (the fp mult), then by n (the factorial) — two
    // integer divisions, matching the Python's `((term * x) // ONE_20) // n`
    // order (chained truncation toward zero; for the all-non-negative exp
    // path this equals `term * x / (ONE_20 * n)` but we keep the order for
    // port fidelity).
    let mut series_sum = ONE_20;
    let mut term = x; // First term: x itself.
    series_sum += term;
    for n in 2..=12 {
        let prod = term * x;
        term = truncated_div(
            truncated_div(prod, ONE_20),
            I256::try_from(n).expect("n in 2..=12"),
        );
        series_sum += term;
    }

    // Multiply all three parts (product * series_sum, both 20-dp, then *
    // first_an), and drop two decimal places to return 18-dp result. The
    // first_an multiplication is plain integer (no /ONE_20 division).
    Ok(truncated_div(
        truncated_div(product * series_sum, ONE_20) * first_an,
        I256::try_from(100i64).expect("100 fits"),
    ))
}

/// `_ln(a)` — the 18-decimal natural-log core (no domain check; assumes
/// `a > 0`). Recursive on the `a < ONE_18` branch.
fn ln_internal(a: I256) -> I256 {
    if a < ONE_18_SIGNED {
        // ln(a) = ln((1/a)^(-1)) = -ln(1/a). For a < 1, 1/a > 1, so the
        // recursive call enters the else branch.
        let reciprocal = truncated_div(ONE_18_SIGNED * ONE_18_SIGNED, a);
        return -ln_internal(reciprocal);
    }

    // Decompose ln(a) = sum of ln(a_n) = x_n where each a_n divides a_n out
    // of `a`. a0, a1 are 0-decimal integers (multiplied by ONE_18 for the
    // comparison); a2..a11 are 20-decimal fixed-point constants.
    let mut a = a;
    let mut working_sum: I256 = I256::ZERO;
    if a >= A0 * ONE_18_SIGNED {
        a = truncated_div(a, A0); // integer division (no fp correction)
        working_sum += X0;
    }
    if a >= A1 * ONE_18_SIGNED {
        a = truncated_div(a, A1);
        working_sum += X1;
    }

    // Promote to 20-decimal precision for the smaller terms.
    working_sum *= I256::try_from(100i64).expect("100 fits");
    a *= I256::try_from(100i64).expect("100 fits");

    // For the remaining a_n (20-dp fixed-point), division requires
    // multiplying by ONE_20.
    if a >= A2 {
        a = truncated_div(a * ONE_20, A2);
        working_sum += X2;
    }
    if a >= A3 {
        a = truncated_div(a * ONE_20, A3);
        working_sum += X3;
    }
    if a >= A4 {
        a = truncated_div(a * ONE_20, A4);
        working_sum += X4;
    }
    if a >= A5 {
        a = truncated_div(a * ONE_20, A5);
        working_sum += X5;
    }
    if a >= A6 {
        a = truncated_div(a * ONE_20, A6);
        working_sum += X6;
    }
    if a >= A7 {
        a = truncated_div(a * ONE_20, A7);
        working_sum += X7;
    }
    if a >= A8 {
        a = truncated_div(a * ONE_20, A8);
        working_sum += X8;
    }
    if a >= A9 {
        a = truncated_div(a * ONE_20, A9);
        working_sum += X9;
    }
    if a >= A10 {
        a = truncated_div(a * ONE_20, A10);
        working_sum += X10;
    }
    if a >= A11 {
        a = truncated_div(a * ONE_20, A11);
        working_sum += X11;
    }

    // `x` is now small (< a_11 ≈ 1.06). Taylor series:
    //   let z = (a - 1) / (a + 1)
    //   ln(a) = 2 * (z + z^3/3 + z^5/5 + ... + z^(2n+1)/(2n+1))
    // 6 terms suffice for 36-decimal precision.
    let z = truncated_div((a - ONE_20) * ONE_20, a + ONE_20);
    let z_squared = truncated_div(z * z, ONE_20);

    let mut num = z;
    let mut series_sum = num;

    for n in [3, 5, 7, 9, 11] {
        num = truncated_div(num * z_squared, ONE_20);
        series_sum += truncated_div(num, I256::try_from(n).expect("odd n"));
    }

    // Multiply by 2 (non-fixed-point), then drop the two extra decimal places.
    let series_sum = series_sum * I256::try_from(2i64).expect("2 fits");
    truncated_div(
        working_sum + series_sum,
        I256::try_from(100i64).expect("100 fits"),
    )
}

/// `_ln_36(x)` — the 36-decimal precision ln variant for arguments close to
/// ONE (where the result is small enough that 36 decimals improve precision).
///
/// Uses a Taylor series `ln(x) = 2 * (z + z^3/3 + ... + z^15/15)` (8 terms,
/// for 36-decimal precision), with `z = (x-1)/(x+1)`.
fn ln_36(x: I256) -> I256 {
    let x = x * ONE_18_SIGNED; // Promote to 36 decimals.

    let z = truncated_div((x - ONE_36) * ONE_36, x + ONE_36);
    let z_squared = truncated_div(z * z, ONE_36);

    let mut num = z;
    let mut series_sum = num;

    for n in [3, 5, 7, 9, 11, 13, 15] {
        num = truncated_div(num * z_squared, ONE_36);
        series_sum += truncated_div(num, I256::try_from(n).expect("odd n"));
    }

    // 8 terms suffice for 36-decimal precision. Multiply by 2 (non-fp).
    series_sum * I256::try_from(2i64).expect("2 fits")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncated_div_truncates_toward_zero_like_solidity_not_floor() {
        // Solidity: -7 / 2 = -3 (truncate toward 0). Python `//` floors to -4.
        // alloy's I256 Div truncates toward zero, matching Solidity.
        let neg7 = I256::try_from(-7i64).unwrap();
        let two = I256::try_from(2i64).unwrap();
        assert_eq!(truncated_div(neg7, two), I256::try_from(-3i64).unwrap());
        // Positive case unchanged.
        assert_eq!(
            truncated_div(I256::try_from(7i64).unwrap(), two),
            I256::try_from(3i64).unwrap()
        );
    }

    #[test]
    fn pow_zero_exponent_returns_one() {
        assert_eq!(pow(U256::from(5u64), I256::ZERO).unwrap(), ONE_18);
        // 0^0 also defined as 1.
        assert_eq!(pow(U256::ZERO, I256::ZERO).unwrap(), ONE_18);
    }

    #[test]
    fn pow_x_zero_returns_zero() {
        assert_eq!(
            pow(U256::ZERO, I256::try_from(5u64).unwrap()).unwrap(),
            U256::ZERO
        );
    }

    #[test]
    fn pow_x_out_of_bounds() {
        // x >= 2^255 reverts.
        let r = pow(TWO_255, ONE_18_I);
        assert_eq!(r, Err(BalancerMathError::XOutOfBounds));
    }

    #[test]
    fn pow_y_out_of_bounds() {
        // y >= MILD_EXPONENT_BOUND reverts.
        let r = pow(ONE_18, I256::from_raw(MILD_EXPONENT_BOUND));
        assert_eq!(r, Err(BalancerMathError::YOutOfBounds));
    }

    #[test]
    fn exp_out_of_bounds() {
        let too_big = MAX_NATURAL_EXPONENT + I256::try_from(1i64).unwrap();
        assert_eq!(exp(too_big), Err(BalancerMathError::OutOfBounds));
    }

    #[test]
    fn ln_of_zero_is_error() {
        assert_eq!(ln(U256::ZERO), Err(BalancerMathError::OutOfBounds));
    }

    #[test]
    fn u256_round_trips_through_i256_for_signed_range() {
        // Values < 2^255 round-trip U256 -> I256::from_raw -> into_raw.
        let v = U256::from(1_000_000_000_000_000_000u128);
        let signed = I256::from_raw(v);
        assert!(!signed.is_negative());
        assert_eq!(signed.into_raw(), v);
    }
}
