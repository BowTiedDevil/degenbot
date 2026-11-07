//! Signed 256-bit arithmetic helpers.

use crate::interpreter::Word;
use core::cmp::Ordering;

/// Compares two EVM words as signed 256-bit integers.
#[inline]
pub fn i256_cmp(first: &Word, second: &Word) -> Ordering {
    flip_sign(first).cmp(&flip_sign(second))
}

/// Returns whether `first < second` as signed 256-bit integers.
#[inline]
pub(super) fn i256_lt(first: &Word, second: &Word) -> bool {
    flip_sign(first) < flip_sign(second)
}

/// Flips the sign bit, which maps two's complement order onto unsigned order. Unsigned comparisons
/// compile to a branchless subtraction chain.
#[inline]
const fn flip_sign(value: &Word) -> Word {
    let mut limbs = *value.as_limbs();
    limbs[3] ^= 1 << 63;
    Word::from_limbs(limbs)
}

/// Divides two EVM words as signed 256-bit integers.
#[inline]
pub fn i256_div(first: Word, second: Word) -> Word {
    if second.is_zero() {
        return Word::ZERO;
    }
    let (first_sign, second_sign) = (sign_mask(&first), sign_mask(&second));
    // `MIN / -1` and `MIN / 1` need no special case: the unsigned quotient `2^255` is `MIN`.
    let quotient = negate_if(first, first_sign) / negate_if(second, second_sign);
    negate_if(quotient, first_sign ^ second_sign)
}

/// Computes signed 256-bit remainder.
#[inline]
pub fn i256_mod(first: Word, second: Word) -> Word {
    if second.is_zero() {
        return Word::ZERO;
    }
    let (first_sign, second_sign) = (sign_mask(&first), sign_mask(&second));
    let remainder = negate_if(first, first_sign) % negate_if(second, second_sign);
    negate_if(remainder, first_sign)
}

/// Returns all ones if `value` is negative and zero otherwise.
#[inline]
const fn sign_mask(value: &Word) -> u64 {
    ((value.as_limbs()[3] as i64) >> 63) as u64
}

/// Negates `value` if `mask` is all ones, without branching: `(value ^ mask) - mask`.
#[inline]
const fn negate_if(value: Word, mask: u64) -> Word {
    let [l0, l1, l2, l3] = value.into_limbs();
    Word::from_limbs([l0 ^ mask, l1 ^ mask, l2 ^ mask, l3 ^ mask])
        .wrapping_sub(Word::from_limbs([mask; 4]))
}
