//! Minimal exact signed 256-bit arithmetic, used only for the hull **cross
//! product**.
//!
//! Why this module exists — see the spike at
//! `docs/ergo-results/order-index-integer-repr-spike.md`: the index stores
//! `gas` / `gross` / `X` / `net` as `i128`, which is exact for `net =
//! gross - gas * X` even at a trillion-token `gross` (~1e30, eight orders of
//! magnitude under `i128::MAX`). But the hull's `cross(a, b, c)` multiplies two
//! **differences** of `i128` values; two large-but-legal diffs can produce a
//! product that overflows `i128`. To keep hull geometry exact *unconditionally*
//! (the crate's exactness requirement), the cross is computed here in exact
//! signed 256-bit and only its sign is used.
//!
//! Representation: two's complement with a signed `hi` (bits 128..256) and an
//! unsigned `lo` (bits 0..128): `value = hi * 2^128 + lo`.

use std::cmp::Ordering;

/// Exact signed 256-bit value as `hi * 2^128 + lo`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct I256 {
    hi: i128,
    lo: u128,
}

impl I256 {
    #[inline]
    fn add(self, o: Self) -> Self {
        let (lo, carry) = self.lo.overflowing_add(o.lo);
        Self {
            hi: self.hi.wrapping_add(o.hi).wrapping_add(i128::from(carry)),
            lo,
        }
    }

    /// Two's complement negation.
    #[inline]
    fn neg(self) -> Self {
        let lo = !self.lo;
        let (lo, carry) = lo.overflowing_add(1);
        Self {
            hi: !self.hi.wrapping_add(i128::from(carry)),
            lo,
        }
    }

    #[inline]
    fn sub(self, o: Self) -> Self {
        self.add(o.neg())
    }

    /// -1, 0, or 1 — the sign of this exact value.
    #[inline]
    #[must_use]
    pub fn sign(self) -> Ordering {
        match self.hi.cmp(&0) {
            Ordering::Less => Ordering::Less,
            Ordering::Greater => Ordering::Greater,
            Ordering::Equal => self.lo.cmp(&0),
        }
    }
}

/// Exact product of two `i128`s as `I256`.
#[allow(clippy::cast_possible_wrap, clippy::many_single_char_names)]
#[inline]
fn imul(a: i128, b: i128) -> I256 {
    let negative = (a < 0) != (b < 0);
    let (lo, hi) = umul128(a.unsigned_abs(), b.unsigned_abs());
    let mut r = I256 { hi: hi as i128, lo };
    if negative {
        r = r.neg();
    }
    r
}

/// `(low, high)` 128·128 -> 256 unsigned multiply.
#[allow(clippy::many_single_char_names)]
fn umul128(a: u128, b: u128) -> (u128, u128) {
    const MASK: u128 = (1 << 64) - 1;
    let a_lo = a & MASK;
    let a_hi = a >> 64;
    let b_lo = b & MASK;
    let b_hi = b >> 64;
    let lo_lo = a_lo * b_lo;
    let lo_hi = a_lo * b_hi;
    let hi_lo = a_hi * b_lo;
    let hi_hi = a_hi * b_hi;
    // s = (lo_lo >> 64) + lo_hi + hi_lo; this can exceed 2^128, so track wraps.
    let mut s = lo_lo >> 64;
    let mut overflow = 0u128;
    let (t, c) = s.overflowing_add(lo_hi);
    s = t;
    overflow += u128::from(c);
    let (t, c) = s.overflowing_add(hi_lo);
    s = t;
    overflow += u128::from(c);
    // low 128 bits: (low-64 of s) in bits 64..128, (lo_lo low-64) in 0..64.
    let lo = ((s & MASK) << 64) | (lo_lo & MASK);
    // high 128 bits: hi_hi + (s >> 64) + overflow * 2^64.
    let hi = hi_hi.wrapping_add(s >> 64).wrapping_add(overflow << 64);
    (lo, hi)
}

/// Sign (as `Ordering`) of the exact cross product
/// `(b.gas - a.gas)*(c.gross - a.gross) - (b.gross - a.gross)*(c.gas - a.gas)`,
/// computed in exact `i256`.
#[must_use]
pub fn cross_sign(dx1: i128, dy2: i128, dy1: i128, dx2: i128) -> Ordering {
    imul(dx1, dy2).sub(imul(dy1, dx2)).sign()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Validate `umul128` against hand-computed 256-bit products.
    #[test]
    fn umul128_known_vectors() {
        let cases: &[(u128, u128, u128, u128)] = &[
            // a, b, lo, hi
            (0, u128::from(u64::MAX), 0, 0),
            (1, 1, 1, 0),
            // (2^64 - 1)^2 = 2^128 - 2^65 + 1, no high.
            (
                u128::from(u64::MAX),
                u128::from(u64::MAX),
                (u128::from(u64::MAX)).wrapping_mul(u128::from(u64::MAX)),
                0,
            ),
            // 2^64 * 2^64 = 2^128 -> hi = 1
            (1 << 64, 1 << 64, 0, 1),
            // 2^127 * 2^63 = 2^190 -> hi = 2^62
            (1 << 127, 1 << 63, 0, 1 << 62),
            // 1 * (2^128 - 1) -> lo = max
            (1, u128::MAX, u128::MAX, 0),
            // (2^128 - 1)^2 = 2^256 - 2^129 + 1 -> lo=1, hi=2^128-2
            (u128::MAX, u128::MAX, 1, u128::MAX - 1),
        ];
        for &(a, b, want_lo, want_hi) in cases {
            let (lo, hi) = umul128(a, b);
            assert_eq!((lo, hi), (want_lo, want_hi), "umul128({a:#x}, {b:#x})");
        }
    }

    /// Differential check of `umul128` against `u128` products that fit.
    #[test]
    fn umul128_matches_i128_for_small_factors() {
        for &(a, b) in &[
            (123_456_789u128, 987_654_321u128),
            (1 << 60, 1 << 20),
            (2_000_000_000_000_000_000u128, 1_000_000_000),
        ] {
            let p = a * b; // fits u128
            let (lo, hi) = umul128(a, b);
            assert_eq!(hi, 0, "high must be 0 for a small product");
            assert_eq!(lo, p);
        }
    }

    /// Cross sign with values that overflow `i128` products must still be
    /// correct (the whole point of the `i256` path).
    #[test]
    fn cross_sign_extreme_does_not_overflow() {
        // dx1 * dy2 overflows i128 if done naively.
        assert_eq!(cross_sign(i128::MAX, i128::MAX, 0, 0), Ordering::Greater);
        // dy1 * dx2 overflows i128, cross negative.
        assert_eq!(cross_sign(0, 0, i128::MAX, i128::MAX), Ordering::Less);
        // cancellation: max*max - max*max == 0
        let v = i128::MAX;
        assert_eq!(cross_sign(v, v, v, v), Ordering::Equal);
        // asymmetric cancellation returning a known small positive:
        // (A)*(B) - (A-1)*(B) = B > 0
        let a = 10_000_000_000i128;
        let b = 3_000_000_000i128;
        assert_eq!(cross_sign(a, b, a - 1, b), Ordering::Greater);
    }
}
