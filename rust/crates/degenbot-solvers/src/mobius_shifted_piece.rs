//! Exact affine-shifted Möbius closed form for a single path piece (ergo EHSWSX).
//!
//! Within one ending-range tuple piece, an N-hop V2/CL path's output is
//! EXACTLY Möbius in the path input `x`: per-hop constant-product maps
//! `m_i(u) = γ_i·s_i·u / (γ_i·u + D_i·r_i)` composed with the tick-crossing
//! translations — each hop's crossing consumes an input-independent
//! `gross_input_offset g_i` before its ending range and contributes an
//! output offset `o_i` (`IntTickRangeCrossing`). Möbius maps are closed
//! under translation and composition (SL(2) closure), so
//!
//! ```text
//! O(x) = (A·x + B) / (C·x + D)
//! ```
//!
//! with signed integer coefficients composed by 2×2 matrix multiplication.
//! This module computes `(A, B, C, D)` directly, replacing the transitional
//! unshifted-coefficients + additive-gross anchor of the active-set walk
//! (`mobius_v3_int::walk_piece_anchor`), which mispriced downstream
//! crossings (they are paid from an upstream hop's OUTPUT, not the path
//! input).
//!
//! The piece argmax of `P(x) = O(x) − x` is closed form:
//!
//! ```text
//! P′(x) = (A·D − B·C) / (C·x + D)² − 1 = 0
//! x* = (isqrt(A·D − B·C) − D) / C
//! ```
//!
//! With zero offsets (`g_i = o_i = 0` everywhere) this reduces to the
//! unshifted recurrence of `compute_int_mobius_coefficients` and
//! `x* = (isqrt(K·M) − M) / N`.
//!
//! # Width discipline — hybrid fast/BigInt
//!
//! Composition entries grow ~multiplicatively across hops: an N-hop piece's
//! entries reach ~(N · per_hop_width) bits, and the composed determinant
//! `A·D − B·C` doubles that again (product of two entries). There is no
//! fixed-width integer that captures arbitrary-depth intermediates.
//!
//! This module's [`compute_shifted_piece_mobius_coefficients`] therefore uses
//! a **hybrid** strategy:
//!
//! 1. **Fast path (I1024).** Coefficients are composed in 1024-bit signed
//!    fixed-width arithmetic with `overflowing_*` checks. This comfortably
//!    covers ≤4-hop paths with realistic reserves (typical V3 virtual
//!    reserves are u128 liquidity × Q96 price ≈ 192 bits; major-pool V2
//!    reserves ≈ 80–93 bits, so a 4-hop composed entry is ~850–1100 bits —
//!    well under 1024). The composed determinant is widened to I2048 (it
//!    is a product of two I1024 magnitudes) and `isqrt`-ed in U2048; that
//!    widening cannot overflow given the I1024 coefficient bound.
//! 2. **BigInt fallback.** The moment ANY `overflowing_*` op fires (a deep
//!    path, or a pathological full-U256-V2-reserve 4-hop), the whole piece
//!    is recomputed in `num-bigint::BigInt` — arbitrary precision, no
//!    overflow possible. The result is narrowed to `U256` with saturation
//!    at the end.
//!
//! Both paths produce the SAME mathematical coefficients; the only
//! difference is the storage type. Consumers drive the result through
//! [`ShiftedMobiusPieceCoefficients::model_optimal_input`] /
//! [`shifted_piece_model_optimal_input`] and never branch on which path
//! ran.

// The Möbius 2×2 matrix entries are canonically `a, b, c, d`; the module is
// already `non_snake_case`-allowed for that reason, so the matching
// clippy lint is allowed here too.
#![expect(clippy::many_single_char_names)]
// `ShiftedMobiusPieceCoefficients::Fast` holds four fixed-width I1024 stack
// values on purpose — the fast path's zero-allocation storage IS the point.
// Boxing would add a heap indirection to the per-candidate solver hot path.
#![expect(clippy::large_enum_variant)]

use alloy::primitives::{Sign as AlloySign, U256};
use degenbot_math::v2::IntHopState;
use num_bigint::BigInt;

use crate::mobius_int_exact::isqrt_u2048;

/// 2048-bit unsigned integer — width for the composed determinant magnitude.
type U2048 = alloy::primitives::Uint<2048, 32>;

/// 1024-bit signed integer — fast-path coefficient width.
///
/// Covers ≤4-hop paths with realistic reserves. See the module-level width
/// note. `Signed<1024, 16>` because alloy 1.6's `Signed` aliases stop at
/// `I512`; the raw constructor accepts any `(BITS, LIMBS)` where
/// `LIMBS == nlimbs(BITS)`.
type I1024 = alloy::primitives::Signed<1024, 16>;

/// 2048-bit signed integer — width for the composed determinant.
///
/// `A·D − B·C` is a product of two I1024 magnitudes, so its bit width is at
/// most `2 × 1024 − 2 = 2046`, which fits [`I2048`]'s positive range
/// (`[0, 2^2047 − 1]`). Widening I1024 coefficients to I2048 and computing
/// the determinant there is therefore overflow-free BY CONSTRUCTION
/// whenever the fast-path composition did not already overflow.
type I2048 = alloy::primitives::Signed<2048, 32>;

/// One hop of a piecewise path piece: the hop's constant-product state
/// (a V2 pool, or a CL pool's ending range as effective reserves) plus the
/// input-independent crossing translations.
#[derive(Clone, Debug)]
pub struct ShiftedPieceHop {
    /// Ending-range (or whole-pool for V2) swap state.
    pub hop: IntHopState,
    /// Gross input consumed crossing into the ending range, in THIS hop's
    /// input units. Zero for V2 hops and for ending-range index 0.
    pub gross_input_offset: U256,
    /// Output produced by the crossed ranges, added to the ending-range
    /// output. Zero for V2 hops and for index 0.
    pub output_offset: U256,
}

/// Signed 2×2-matrix coefficients of the exact piece map
/// `O(x) = (A·x + B) / (C·x + D)`. Signed because the translation entries
/// `−γ·s·g` / `D·r − γ·g` can be negative.
///
/// This is the fast-path representation — see [`ShiftedMobiusPieceCoefficients`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FastShiftedCoeffs {
    /// Numerator linear coefficient.
    pub a: I1024,
    /// Numerator constant coefficient.
    pub b: I1024,
    /// Denominator linear coefficient.
    pub c: I1024,
    /// Denominator constant coefficient.
    pub d: I1024,
}

/// Arbitrary-precision fallback coefficients (deep paths that overflow the
/// [`FastShiftedCoeffs`] I1024 width).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BigShiftedCoeffs {
    /// Numerator linear coefficient.
    pub a: BigInt,
    /// Numerator constant coefficient.
    pub b: BigInt,
    /// Denominator linear coefficient.
    pub c: BigInt,
    /// Denominator constant coefficient.
    pub d: BigInt,
}

/// Composed Möbius coefficients for a path piece.
///
/// Holds EITHER the fast-path [`FastShiftedCoeffs`] (I1024 fixed-width,
/// chosen by [`compute_shifted_piece_mobius_coefficients`] when no
/// intermediate overflowed) OR the arbitrary-precision
/// [`BigShiftedCoeffs`] fallback (selected when any composition step
/// overflowed I1024). Consumers do not branch on the variant — drive the
/// result through [`Self::model_optimal_input`] (or the free function
/// [`shifted_piece_model_optimal_input`]) and
/// [`shifted_piece_slope_exceeds_unity_at`].
pub enum ShiftedMobiusPieceCoefficients {
    /// I1024 fast path: ≤4-hop paths with realistic reserves.
    Fast(FastShiftedCoeffs),
    /// Arbitrary-precision fallback: deep paths that overflow I1024.
    Big(BigShiftedCoeffs),
}

impl ShiftedMobiusPieceCoefficients {
    /// Model-optimal piece input `x* = (isqrt(det) − D) / C`.
    ///
    /// Thin dispatch over the fast/BigInt representation; see
    /// [`shifted_piece_model_optimal_input`] for the contract.
    #[must_use]
    pub fn model_optimal_input(&self) -> Option<U256> {
        match self {
            Self::Fast(c) => fast_model_optimal_input(c),
            Self::Big(c) => big_model_optimal_input(c),
        }
    }
}

// ---------------------------------------------------------------------------
// Fast-path helpers (I1024 coefficients, I2048 determinant)
// ---------------------------------------------------------------------------

/// Widen a `U256` to [`I1024`] by zero-extending the limbs. Each `U256` value
/// is non-negative and fits comfortably in I1024's positive range (`< 2^256
/// < 2^1023`).
fn i1024_from_u256(v: U256) -> I1024 {
    let limbs = v.into_limbs();
    let mut wide = [0u64; 16];
    wide[..4].copy_from_slice(&limbs);
    I1024::from_limbs(wide)
}

/// Widen a `u64` to [`I1024`] (used by cross-check tests).
#[cfg(test)]
fn i1024_from_u64(v: u64) -> I1024 {
    let mut wide = [0u64; 16];
    wide[0] = v;
    I1024::from_limbs(wide)
}

/// Widen an [`I1024`] to [`I2048`] preserving sign. Cannot overflow: the
/// magnitude of an I1024 value (`≤ 2^1023`) is well inside I2048's positive
/// range (`≤ 2^2047 − 1`).
fn widen_i1024_to_i2048(v: I1024) -> I2048 {
    let (sign, abs) = v.into_sign_and_abs();
    let limbs = abs.into_limbs();
    let mut wide = [0u64; 32];
    wide[..16].copy_from_slice(&limbs);
    let abs_wide = U2048::from_limbs(wide);
    let (result, overflow) = I2048::overflowing_from_sign_and_abs(sign, abs_wide);
    debug_assert!(!overflow, "widen i1024 -> i2048 cannot overflow");
    result
}

/// Narrow a known-nonnegative [`I1024`] to `U256`, saturating to `U256::MAX`
/// on overflow. Callers use this on model-optimum magnitudes; a saturating
/// optimum simply proposes the largest representable candidate (the walk's
/// window refinement treats it as a hint, not ground truth).
fn i2048_to_u256_saturating(v: I2048) -> U256 {
    if v <= I2048::ZERO {
        return U256::ZERO;
    }
    let (_, abs) = v.into_sign_and_abs();
    let limbs = abs.into_limbs();
    if limbs[4..].iter().any(|&l| l != 0) {
        return U256::MAX;
    }
    U256::from_limbs([limbs[0], limbs[1], limbs[2], limbs[3]])
}

/// `p1·q1 + p2·q2` with overflow detection. Returns `None` if either
/// product or the sum overflows [`I1024`]. Used by [`checked_matrix_compose`]
/// so the fast path falls back to BigInt rather than silently wrapping.
fn checked_dot(p1: I1024, q1: I1024, p2: I1024, q2: I1024) -> Option<I1024> {
    let (t1, of1) = p1.overflowing_mul(q1);
    if of1 {
        return None;
    }
    let (t2, of2) = p2.overflowing_mul(q2);
    if of2 {
        return None;
    }
    let (sum, of3) = t1.overflowing_add(t2);
    if of3 {
        return None;
    }
    Some(sum)
}

/// Build the local matrix for one hop in I1024 with checked arithmetic.
///
/// Returns `None` if any local product overflows I1024 (only possible with
/// pathological reserve sizes that exceed the fast-path budget). The local
/// matrix is
///
/// ```text
/// L = [[γs + oγ,  −γs·g + o·(D r − γ g)], [γ, D r − γ g]]
/// ```
fn build_local_matrix_fast(hop: &ShiftedPieceHop) -> Option<FastShiftedCoeffs> {
    let g_num = i1024_from_u256(hop.hop.gamma_numer);
    let d_denom = i1024_from_u256(hop.hop.fee_denom);
    let r = i1024_from_u256(hop.hop.reserve_in);
    let s = i1024_from_u256(hop.hop.reserve_out);
    let goff = i1024_from_u256(hop.gross_input_offset);
    let ooff = i1024_from_u256(hop.output_offset);

    // d_r_g = D·r − γ·g
    let (t1, of1) = d_denom.overflowing_mul(r);
    if of1 {
        return None;
    }
    let (t2, of2) = g_num.overflowing_mul(goff);
    if of2 {
        return None;
    }
    let (d_r_g, of3) = t1.overflowing_sub(t2);
    if of3 {
        return None;
    }

    // a = γ·s + o·γ
    let a = checked_dot(g_num, s, ooff, g_num)?;

    // b = −(γ·s)·g + o·(D r − γ g)
    let (gs, of_gs) = g_num.overflowing_mul(s);
    if of_gs {
        return None;
    }
    let (gsg, of_gsg) = gs.overflowing_mul(goff);
    if of_gsg {
        return None;
    }
    let (neg_gsg, of_neg) = gsg.overflowing_neg();
    if of_neg {
        return None;
    }
    let (odrg, of_odrg) = ooff.overflowing_mul(d_r_g);
    if of_odrg {
        return None;
    }
    let (b, of_b) = neg_gsg.overflowing_add(odrg);
    if of_b {
        return None;
    }

    Some(FastShiftedCoeffs {
        a,
        b,
        c: g_num,
        d: d_r_g,
    })
}

/// Multiply two 2×2 coefficient matrices with overflow detection
/// (composition order: the result's map applies `first` FIRST, then
/// `second` to its output). Returns `None` on any overflow — the caller
/// then defers to the BigInt path.
fn checked_matrix_compose(
    second: &FastShiftedCoeffs,
    first: &FastShiftedCoeffs,
) -> Option<FastShiftedCoeffs> {
    // result row·col entries:
    //   a = second.a·first.a + second.b·first.c
    //   b = second.a·first.b + second.b·first.d
    //   c = second.c·first.a + second.d·first.c
    //   d = second.c·first.b + second.d·first.d
    let a = checked_dot(second.a, first.a, second.b, first.c)?;
    let b = checked_dot(second.a, first.b, second.b, first.d)?;
    let c = checked_dot(second.c, first.a, second.d, first.c)?;
    let d = checked_dot(second.c, first.b, second.d, first.d)?;
    Some(FastShiftedCoeffs { a, b, c, d })
}

/// Fast-path model-optimal input: `x* = (isqrt(det) − D) / C`, with the
/// determinant widened to I2048 (product of two I1024 magnitudes, so it
/// cannot overflow given the fast-path coefficient bound).
///
/// Returns `None` when the piece's envelope is not profitable anywhere on
/// the nonnegative half-line (`det ≤ D²`) or the composition is degenerate
/// (`C ≤ 0`).
fn fast_model_optimal_input(c: &FastShiftedCoeffs) -> Option<U256> {
    if c.c <= I1024::ZERO {
        return None;
    }
    // Widen the four coefficients to I2048 and compute the determinant there.
    // Since each coefficient fits I1024 (magnitude < 2^1023), the products
    // fit I2048 (< 2^2046) WITHOUT overflow — this is the invariant the
    // fast path is sized for.
    let a_w = widen_i1024_to_i2048(c.a);
    let b_w = widen_i1024_to_i2048(c.b);
    let c_w = widen_i1024_to_i2048(c.c);
    let d_w = widen_i1024_to_i2048(c.d);

    let (ad, of_ad) = a_w.overflowing_mul(d_w);
    debug_assert!(!of_ad, "i1024 coefficient product cannot overflow i2048");
    let (bc, of_bc) = b_w.overflowing_mul(c_w);
    debug_assert!(!of_bc, "i1024 coefficient product cannot overflow i2048");
    let (det, of_det) = ad.overflowing_sub(bc);
    debug_assert!(!of_det, "i2048 subtraction of two products cannot overflow");

    if det <= I2048::ZERO {
        return None;
    }
    let (_, det_abs) = det.into_sign_and_abs(); // U2048
    let sqrt = isqrt_u2048(det_abs); // U2048, value < 2^1024
    let (sqrt_i2048, of_sqrt) = I2048::overflowing_from_sign_and_abs(AlloySign::Positive, sqrt);
    debug_assert!(!of_sqrt, "isqrt(det) < 2^1024 fits i2048");

    // x* = (isqrt(det) − D) / C; a nonpositive numerator means the model
    // optimum is at x = 0 (not profitable past the piece's origin).
    let numerator = sqrt_i2048 - d_w;
    if numerator <= I2048::ZERO {
        return None;
    }
    let x_star = numerator / c_w;
    Some(i2048_to_u256_saturating(x_star))
}

/// Fast-path slope check: `det > (C·x + D)²`. Computed in I2048 (BigInt when
/// the fast path was not used — see [`shifted_piece_slope_exceeds_unity_at`]).
fn fast_slope_exceeds_unity_at(c: &FastShiftedCoeffs, x: U256) -> bool {
    // Promote the fast coefficients to BigInt so the squared denominator
    // (which can exceed even I2048 for large x) is exact — this function is
    // test-only and perf is not hotpath-critical.
    let a = bigint_from_u1024_signed(c.a);
    let b = bigint_from_u1024_signed(c.b);
    let cc = bigint_from_u1024_signed(c.c);
    let d = bigint_from_u1024_signed(c.d);
    let det = &a * &d - &b * &cc;
    let xi = bigint_from_u256(x);
    let denom_at = &cc * &xi + &d;
    det > &denom_at * &denom_at
}

// ---------------------------------------------------------------------------
// BigInt fallback helpers
// ---------------------------------------------------------------------------

/// Convert a non-negative `U256` to a `BigInt`.
fn bigint_from_u256(v: U256) -> BigInt {
    let bytes = v.to_be_bytes::<32>();
    BigInt::from_bytes_be(num_bigint::Sign::Plus, &bytes)
}

/// Convert an [`I1024`] (signed) to a `BigInt`, preserving the sign.
fn bigint_from_u1024_signed(v: I1024) -> BigInt {
    let (sign, abs) = v.into_sign_and_abs();
    let bytes = abs.to_be_bytes::<128>();
    let big_sign = match sign {
        AlloySign::Negative => num_bigint::Sign::Minus,
        AlloySign::Positive => num_bigint::Sign::Plus,
    };
    BigInt::from_bytes_be(big_sign, &bytes)
}

/// Convert a known-nonnegative `BigInt` to `U256`, saturating to
/// `U256::MAX` on overflow.
fn bigint_to_u256_saturating(v: &BigInt) -> U256 {
    if v.sign() == num_bigint::Sign::Minus || v.sign() == num_bigint::Sign::NoSign {
        return U256::ZERO;
    }
    let (sign, bytes) = v.to_bytes_be();
    debug_assert_eq!(sign, num_bigint::Sign::Plus);
    if bytes.len() > 32 {
        return U256::MAX;
    }
    let mut arr = [0u8; 32];
    arr[32 - bytes.len()..].copy_from_slice(&bytes);
    U256::from_be_bytes(arr)
}

/// Build the local matrix for one hop in BigInt (no overflow possible).
fn build_local_matrix_big(hop: &ShiftedPieceHop) -> BigShiftedCoeffs {
    let g_num = bigint_from_u256(hop.hop.gamma_numer);
    let d_denom = bigint_from_u256(hop.hop.fee_denom);
    let r = bigint_from_u256(hop.hop.reserve_in);
    let s = bigint_from_u256(hop.hop.reserve_out);
    let goff = bigint_from_u256(hop.gross_input_offset);
    let ooff = bigint_from_u256(hop.output_offset);

    let d_r_g = &(&d_denom * &r) - &(&g_num * &goff);
    let a = &(&g_num * &s) + &(&ooff * &g_num);
    let b = &(-(&(&g_num * &s) * &goff)) + &(&ooff * &d_r_g);

    BigShiftedCoeffs {
        a,
        b,
        c: g_num,
        d: d_r_g,
    }
}

/// Matrix compose in BigInt (order: `first` applied FIRST, then `second`).
fn matrix_compose_big(second: &BigShiftedCoeffs, first: &BigShiftedCoeffs) -> BigShiftedCoeffs {
    let a = &(&second.a * &first.a) + &(&second.b * &first.c);
    let b = &(&second.a * &first.b) + &(&second.b * &first.d);
    let c = &(&second.c * &first.a) + &(&second.d * &first.c);
    let d = &(&second.c * &first.b) + &(&second.d * &first.d);
    BigShiftedCoeffs { a, b, c, d }
}

/// BigInt model-optimal input: `x* = (isqrt(det) − D) / C`.
fn big_model_optimal_input(c: &BigShiftedCoeffs) -> Option<U256> {
    // C ≤ 0 ⇒ degenerate (no interior optimum). `sign() != Plus` covers both
    // NoSign (zero) and Minus.
    if c.c.sign() != num_bigint::Sign::Plus {
        return None;
    }
    let det = &(&c.a * &c.d) - &(&c.b * &c.c);
    if det.sign() != num_bigint::Sign::Plus {
        return None;
    }
    let sqrt_det = det.sqrt();
    let numerator = &sqrt_det - &c.d;
    if numerator.sign() != num_bigint::Sign::Plus {
        return None;
    }
    let x_star = &numerator / &c.c;
    Some(bigint_to_u256_saturating(&x_star))
}

// ---------------------------------------------------------------------------
// Public dispatch entry points
// ---------------------------------------------------------------------------

/// Compose the exact shifted Möbius coefficients of a path piece.
///
/// Hop `i`'s local map, in terms of the variable fed to the hop (`z`),
/// including its crossing translations, is
///
/// ```text
/// L_i(z) = m_i(z − g_i) + o_i
///        = ((γs + oγ)·z + (−γs·g + o·(D r − γ g)))
///          / (γ·z + (D r − γ g))
/// ```
///
/// with `(r, s) = (reserve_in, reserve_out)`, `γ = gamma_numer`,
/// `D = fee_denom`, `g = gross_input_offset`, `o = output_offset`. The piece
/// map is `L_n ∘ … ∘ L_1` (matrix product). `det(L_i) = γ·s·D·r ≥ 0` for
/// every hop (the translations cancel out of the determinant), so the
/// composed determinant `A·D − B·C` is positive whenever the piece extends a
/// profitable envelope.
///
/// Selects the [`ShiftedMobiusPieceCoefficients::Fast`] (I1024) path when the
/// composition fits, falling back to
/// [`ShiftedMobiusPieceCoefficients::Big`] (arbitrary precision) on any
/// intermediate overflow — so the result is always mathematically exact
/// regardless of path depth or reserve magnitude.
///
/// # Panics
///
/// Panics on an empty `hops` slice.
#[must_use]
pub fn compute_shifted_piece_mobius_coefficients(
    hops: &[ShiftedPieceHop],
) -> ShiftedMobiusPieceCoefficients {
    assert!(!hops.is_empty(), "shifted piece needs at least one hop");

    // Fast path: compose in I1024 with checked arithmetic. If ANY step
    // overflows, defer to the BigInt path for the whole piece.
    if let Some(fast) = try_compute_fast(hops) {
        return ShiftedMobiusPieceCoefficients::Fast(fast);
    }
    ShiftedMobiusPieceCoefficients::Big(compute_big(hops))
}

/// Attempt the I1024 fast-path composition. Returns `None` as soon as any
/// local-matrix construction or composition step overflows I1024.
fn try_compute_fast(hops: &[ShiftedPieceHop]) -> Option<FastShiftedCoeffs> {
    let mut acc: Option<FastShiftedCoeffs> = None;
    for shifted in hops {
        let local = build_local_matrix_fast(shifted)?;
        acc = Some(match acc {
            None => local,
            Some(prev) => checked_matrix_compose(&local, &prev)?,
        });
    }
    acc
}

/// BigInt fallback composition (used when the fast path overflowed).
fn compute_big(hops: &[ShiftedPieceHop]) -> BigShiftedCoeffs {
    let mut acc: Option<BigShiftedCoeffs> = None;
    for shifted in hops {
        let local = build_local_matrix_big(shifted);
        acc = Some(match acc {
            None => local,
            Some(prev) => matrix_compose_big(&local, &prev),
        });
    }
    #[expect(clippy::expect_used)] // non-empty hops asserted by caller
    let acc = acc.expect("non-empty hops asserted by caller");
    acc
}

/// Closed-form model-optimum input of the piece:
/// `x* = (isqrt(A·D − B·C) − D) / C`.
///
/// Returns `None` when the piece's envelope is not profitable anywhere on
/// the nonnegative half-line (`A·D − B·C ≤ D²`, i.e. slope ≤ 1 at `x = 0`)
/// or the composition is degenerate (`C ≤ 0`). The result is exact at the
/// Möbius-model layer — same two-layer discipline as
/// `mobius_int_exact::compute_mobius_model_optimal_input`: EVM floor
/// staircase effects are the caller's ±2 sweep.
///
/// Dispatches over the fast / BigInt representation carried by
/// [`ShiftedMobiusPieceCoefficients`].
#[must_use]
pub fn shifted_piece_model_optimal_input(coeffs: &ShiftedMobiusPieceCoefficients) -> Option<U256> {
    coeffs.model_optimal_input()
}

/// Whether the piece envelope's slope exceeds 1 at `x` (i.e. the path is
/// still gaining profit there): `A·D − B·C > (C·x + D)²`.
///
/// Computed in `BigInt` for both representations (the squared denominator
/// can exceed I2048 for large `x`, and this check is test-only so the
/// arbitrary-precision cost is acceptable).
#[must_use]
pub fn shifted_piece_slope_exceeds_unity_at(
    coeffs: &ShiftedMobiusPieceCoefficients,
    x: U256,
) -> bool {
    match coeffs {
        ShiftedMobiusPieceCoefficients::Fast(c) => fast_slope_exceeds_unity_at(c, x),
        ShiftedMobiusPieceCoefficients::Big(c) => {
            let det = &(&c.a * &c.d) - &(&c.b * &c.c);
            let xi = bigint_from_u256(x);
            let denom_at = &(&c.c * &xi) + &c.d;
            det > &denom_at * &denom_at
        }
    }
}

#[expect(clippy::panic, clippy::unwrap_used)]
#[cfg(test)]
mod tests {
    use super::*;

    fn u256(v: u64) -> U256 {
        U256::from(v)
    }

    fn v2_hop(r_in: u64, r_out: u64) -> IntHopState {
        IntHopState::new(u256(r_in), u256(r_out), 997, 1000)
    }

    /// Unwrap the fast variant (u64-reserve tests always take the fast path).
    fn expect_fast(coeffs: &ShiftedMobiusPieceCoefficients) -> &FastShiftedCoeffs {
        match coeffs {
            ShiftedMobiusPieceCoefficients::Fast(c) => c,
            ShiftedMobiusPieceCoefficients::Big(_) => {
                panic!("expected Fast variant for u64-reserve test, got Big fallback")
            }
        }
    }

    /// Without offsets the shifted composition MUST reduce to the unshifted
    /// recurrence: A = K (= Πγ·s), B = 0, C = N, D = M.
    #[test]
    fn zero_offsets_reduce_to_unshifted_recurrence() {
        let hops = [
            ShiftedPieceHop {
                hop: v2_hop(1_000_000, 1_100_000),
                gross_input_offset: U256::ZERO,
                output_offset: U256::ZERO,
            },
            ShiftedPieceHop {
                hop: v2_hop(2_000_000, 1_900_000),
                gross_input_offset: U256::ZERO,
                output_offset: U256::ZERO,
            },
        ];
        let coeffs = compute_shifted_piece_mobius_coefficients(&hops);
        let c = expect_fast(&coeffs);

        // Hand-derived unshifted composition (s1/r2 = hop0 out/in reserves,
        // s2/r2_2 = hop1 out/in; γ=997, D=1000):
        //   y1 = γ s2 y0 / (γ y0 + D r2_2), y0 = γ s1 x / (γ x + D r2)
        //   ⇒ a = γ²·s1·s2
        //     c = γ²·s1 + γ·D·r2_2   (denominator linear)
        //     d = D²·r2·r2_2
        let g = i1024_from_u64(997);
        let d_denom = i1024_from_u64(1000);
        let s1 = i1024_from_u64(1_100_000);
        let r1 = i1024_from_u64(1_000_000);
        let s2 = i1024_from_u64(1_900_000);
        let r2 = i1024_from_u64(2_000_000);

        assert_eq!(c.a, g * g * s1 * s2);
        assert_eq!(c.b, I1024::ZERO);
        assert_eq!(c.c, g * g * s1 + g * (d_denom * r2));
        assert_eq!(c.d, d_denom * d_denom * r1 * r2);

        // x* must equal the unshifted model optimum exactly.
        let unshifted = [hops[0].hop.clone(), hops[1].hop.clone()];
        let flat = crate::mobius_int::compute_int_mobius_coefficients(&unshifted).unwrap();
        let expect = crate::mobius_int_exact::compute_mobius_model_optimal_input(&flat);
        assert_eq!(shifted_piece_model_optimal_input(&coeffs), Some(expect));
    }

    /// Hand-derived single-hop case with offsets:
    /// hop (r,s) = (1_000_000, 2_000_000), γ/ρ = 997/1000,
    /// g = 10_000, o = 20_000.
    ///
    /// O(z) = γ s (z − g) / (D r + γ (z − g)) + o
    ///      = (997·(2_000_000 + 20_000)·z + (−997·2_000_000·10_000
    ///                                              + 20_000·(1000·1_000_000 − 997·10_000)))
    ///        / (997·z + (1000·1_000_000 − 997·10_000))
    #[test]
    fn single_hop_offsets_match_hand_derivation() {
        let hops = [ShiftedPieceHop {
            hop: v2_hop(1_000_000, 2_000_000),
            gross_input_offset: u256(10_000),
            output_offset: u256(20_000),
        }];
        let coeffs = compute_shifted_piece_mobius_coefficients(&hops);
        let c = expect_fast(&coeffs);

        let a_expect = i1024_from_u64(997) * i1024_from_u64(2_020_000);
        let drg = i1024_from_u64(1000) * i1024_from_u64(1_000_000)
            - i1024_from_u64(997) * i1024_from_u64(10_000);
        let b_expect = -(i1024_from_u64(997) * i1024_from_u64(2_000_000)) * i1024_from_u64(10_000)
            + i1024_from_u64(20_000) * drg;

        assert_eq!(c.a, a_expect);
        assert_eq!(c.b, b_expect);
        assert_eq!(c.c, i1024_from_u64(997));
        assert_eq!(c.d, drg);

        // Cross-check the composed map against direct evaluation at a point.
        let z = i1024_from_u64(123_456);
        let direct_num =
            i1024_from_u64(997) * i1024_from_u64(2_000_000) * (z - i1024_from_u64(10_000));
        let direct_den = i1024_from_u64(1000) * i1024_from_u64(1_000_000)
            + i1024_from_u64(997) * (z - i1024_from_u64(10_000));
        let direct = direct_num / direct_den + i1024_from_u64(20_000);
        let composed = (c.a * z + c.b) / (c.c * z + c.d);
        assert_eq!(composed, direct);
    }

    /// Profitability slope check: with a profitable envelope, slope must
    /// exceed 1 at small x and fall below 1 at large x.
    #[test]
    fn slope_check_matches_envelope_geometry() {
        let hops = [
            ShiftedPieceHop {
                hop: v2_hop(1_000_000, 5_000_000),
                gross_input_offset: U256::ZERO,
                output_offset: U256::ZERO,
            },
            ShiftedPieceHop {
                hop: v2_hop(1_500_000, 3_000_000),
                gross_input_offset: U256::ZERO,
                output_offset: U256::ZERO,
            },
        ];
        let coeffs = compute_shifted_piece_mobius_coefficients(&hops);
        let x_star = shifted_piece_model_optimal_input(&coeffs).unwrap();
        assert!(!x_star.is_zero());
        assert!(shifted_piece_slope_exceeds_unity_at(&coeffs, U256::ZERO));
        assert!(shifted_piece_slope_exceeds_unity_at(
            &coeffs,
            x_star / u256(4)
        ));
        assert!(!shifted_piece_slope_exceeds_unity_at(
            &coeffs,
            x_star * u256(4)
        ));
    }

    /// Degenerate compositions do not panic and return None.
    #[test]
    fn degenerate_compositions_return_none() {
        // Zero gamma → C = 0 → degenerate.
        let zero_gamma = IntHopState::new(u256(1_000_000), u256(1_000_000), 1, 1_000_000);
        let hops = [ShiftedPieceHop {
            hop: zero_gamma,
            gross_input_offset: U256::ZERO,
            output_offset: U256::ZERO,
        }];
        let coeffs = compute_shifted_piece_mobius_coefficients(&hops);
        // γ=1/1e6 envelope is not profitable (det <= D^2).
        assert!(shifted_piece_model_optimal_input(&coeffs).is_none());
    }

    /// Fast / BigInt agreement: both paths compute the SAME coefficients and
    /// model optimum (the dispatch is purely a width choice; the math is
    /// identical). Uses a profitable u64-reserve envelope so the fast path is
    /// selected and the optimum is `Some`.
    #[test]
    fn fast_and_big_paths_agree_on_model_optimum() {
        let hops = [
            ShiftedPieceHop {
                hop: v2_hop(1_000_000, 5_000_000),
                gross_input_offset: U256::ZERO,
                output_offset: U256::ZERO,
            },
            ShiftedPieceHop {
                hop: v2_hop(1_500_000, 3_000_000),
                gross_input_offset: U256::ZERO,
                output_offset: U256::ZERO,
            },
        ];
        let fast_or_big = compute_shifted_piece_mobius_coefficients(&hops);
        // u64 reserves always take the fast path — confirm the routing, then
        // independently recompute in BigInt and compare the optimum.
        assert!(
            matches!(fast_or_big, ShiftedMobiusPieceCoefficients::Fast(_)),
            "u64-reserve piece must take the fast path"
        );
        let big = compute_big(&hops);
        let via_dispatch = shifted_piece_model_optimal_input(&fast_or_big);
        let via_big = big_model_optimal_input(&big);
        assert_eq!(via_dispatch, via_big);
        assert!(via_dispatch.is_some(), "envelope must be profitable");
    }

    /// Pathological full-U256-reserve depth forces the BigInt fallback and
    /// still returns a sane (saturating-or-finite) optimum without panic.
    #[test]
    fn full_u256_reserves_do_not_panic() {
        let near_max = U256::MAX - U256::from(1u64);
        // 6 hops of near-max reserves: composition overflows I1024 → Big.
        let hops: Vec<ShiftedPieceHop> = (0..6)
            .map(|i| ShiftedPieceHop {
                hop: IntHopState::new(
                    near_max - U256::from(i),
                    near_max - U256::from(i + 1),
                    997,
                    1000,
                ),
                gross_input_offset: U256::ZERO,
                output_offset: U256::ZERO,
            })
            .collect();
        let coeffs = compute_shifted_piece_mobius_coefficients(&hops);
        // Must have taken the Big path AND returned without panic.
        assert!(matches!(coeffs, ShiftedMobiusPieceCoefficients::Big(_)));
        let _ = shifted_piece_model_optimal_input(&coeffs); // no panic
    }
}
