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
//! # Width discipline
//!
//! Entries grow multiplicatively across hops: per-hop magnitudes are
//! ~`γ·s ≈ 2^(20 + reserve_bits)`, so N hops approach ~2^(N·140) for
//! realistic V3 virtual reserves. This is the SAME U512 exposure the
//! existing `compute_int_mobius_coefficients` recurrence carries (its
//! `K·M` product already strains U512 at ≥ 3 hops with large reserves) —
//! not a regression, but the exposure is written down here per the task.
//! Composition panics loudly on overflow in debug builds (ruint checked
//! arithmetic), mirroring the spec-bound-pool-state philosophy of
//! `u512_to_u256_internal`.

#![allow(non_snake_case)]

use alloy::primitives::U256;
use degenbot_v2_math::IntHopState;

/// Signed 512-bit integer for coefficient composition (alloy-primitives
/// 1.6 does not root-export the `I512` alias, only `Signed`).
type I512 = alloy::primitives::Signed<512, 8>;

use crate::mobius_int_exact::isqrt_u512;

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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShiftedMobiusPieceCoefficients {
    /// Numerator linear coefficient.
    pub a: I512,
    /// Numerator constant coefficient.
    pub b: I512,
    /// Denominator linear coefficient.
    pub c: I512,
    /// Denominator constant coefficient.
    pub d: I512,
}

/// Widen a `U256` to `I512` by zero-extending the limbs.
fn i512_from_u256(v: U256) -> I512 {
    let limbs = v.into_limbs();
    I512::from_limbs([limbs[0], limbs[1], limbs[2], limbs[3], 0, 0, 0, 0])
}

/// Widen a `u64` to `I512` (test-only — production hops widen from `U256`).
#[cfg(test)]
fn i512_from_u64(v: u64) -> I512 {
    I512::from_limbs([v, 0, 0, 0, 0, 0, 0, 0])
}

/// Narrow a known-nonnegative `I512` to `U256`, saturating to `U256::MAX` on
/// overflow. Callers use this on model-optimum magnitudes; a saturating
/// optimum simply proposes the largest representable candidate (the walk's
/// window refinement treats it as a hint, not ground truth).
fn i512_to_u256_saturating(v: I512) -> U256 {
    if v <= I512::ZERO {
        return U256::ZERO;
    }
    let (_, abs) = v.into_sign_and_abs(); // Sign is Positive here
    let limbs = abs.into_limbs();
    if limbs[4..].iter().any(|&l| l != 0) {
        return U256::MAX;
    }
    U256::from_limbs([limbs[0], limbs[1], limbs[2], limbs[3]])
}

/// Multiply two 2×2 coefficient matrices (composition order: the result's
/// map applies `first` FIRST, then `second` to its output).
fn matrix_compose(
    second: &ShiftedMobiusPieceCoefficients,
    first: &ShiftedMobiusPieceCoefficients,
) -> ShiftedMobiusPieceCoefficients {
    ShiftedMobiusPieceCoefficients {
        a: second.a * first.a + second.b * first.c,
        b: second.a * first.b + second.b * first.d,
        c: second.c * first.a + second.d * first.c,
        d: second.c * first.b + second.d * first.d,
    }
}

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
/// # Panics
///
/// Panics on an empty `hops` slice, and on I512 composition overflow in
/// debug builds (see the module-level width note — the same exposure class
/// as the unshifted U512 recurrence).
#[must_use]
pub fn compute_shifted_piece_mobius_coefficients(
    hops: &[ShiftedPieceHop],
) -> ShiftedMobiusPieceCoefficients {
    assert!(!hops.is_empty(), "shifted piece needs at least one hop");

    let mut acc: Option<ShiftedMobiusPieceCoefficients> = None;
    for shifted in hops {
        let gamma = shifted.hop.gamma_numer;
        let fee_denom = shifted.hop.fee_denom;
        // γ and D fit u64 in practice (997_000 / 1_000_000 scale); widen
        // through U256 limbs to stay general.
        let g_num = i512_from_u256(gamma);
        let d_denom = i512_from_u256(fee_denom);
        let r = i512_from_u256(shifted.hop.reserve_in);
        let s = i512_from_u256(shifted.hop.reserve_out);
        let goff = i512_from_u256(shifted.gross_input_offset);
        let ooff = i512_from_u256(shifted.output_offset);

        // L = [[γs + oγ, −γs·g + o(D r − γ g)], [γ, D r − γ g]]
        let d_r_g = d_denom * r - g_num * goff; // D·r − γ·g
        let local = ShiftedMobiusPieceCoefficients {
            a: g_num * s + ooff * g_num,
            b: -(g_num * s) * goff + ooff * d_r_g,
            c: g_num,
            d: d_r_g,
        };

        acc = Some(match acc {
            None => local,
            Some(prev) => matrix_compose(&local, &prev),
        });
    }
    acc.expect("non-empty hops asserted above")
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
#[must_use]
pub fn shifted_piece_model_optimal_input(coeffs: &ShiftedMobiusPieceCoefficients) -> Option<U256> {
    if coeffs.c <= I512::ZERO {
        return None;
    }
    let det = coeffs.a * coeffs.d - coeffs.b * coeffs.c;
    if det <= I512::ZERO {
        return None;
    }
    // det > 0 by check; narrow to U512 for the exact floor isqrt.
    let (_, det_abs) = det.into_sign_and_abs();
    let sqrt_det = isqrt_u512(det_abs);
    let sqrt_det_i = I512::from_limbs(sqrt_det.into_limbs());
    // x* = (isqrt(det) − D) / C; a nonpositive numerator means the model
    // optimum is at x = 0 (not profitable past the piece's origin).
    let numerator = sqrt_det_i - coeffs.d;
    if numerator <= I512::ZERO {
        return None;
    }
    let x_star = numerator / coeffs.c;
    Some(i512_to_u256_saturating(x_star))
}

/// Whether the piece envelope's slope exceeds 1 at `x` (i.e. the path is
/// still gaining profit there): `A·D − B·C > (C·x + D)²`.
#[must_use]
pub fn shifted_piece_slope_exceeds_unity_at(
    coeffs: &ShiftedMobiusPieceCoefficients,
    x: U256,
) -> bool {
    let det = coeffs.a * coeffs.d - coeffs.b * coeffs.c;
    let xi = i512_from_u256(x);
    let denom_at = coeffs.c * xi + coeffs.d;
    det > denom_at * denom_at
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn u256(v: u64) -> U256 {
        U256::from(v)
    }

    fn v2_hop(r_in: u64, r_out: u64) -> IntHopState {
        IntHopState::new(u256(r_in), u256(r_out), 997, 1000)
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

        // Hand-derived unshifted composition (s1/r2 = hop0 out/in reserves,
        // s2/r2_2 = hop1 out/in; γ=997, D=1000):
        //   y1 = γ s2 y0 / (γ y0 + D r2_2), y0 = γ s1 x / (γ x + D r2)
        //   ⇒ a = γ²·s1·s2
        //     c = γ²·s1 + γ·D·r2_2   (denominator linear)
        //     d = D²·r2·r2_2
        let g = I512::from_limbs([997, 0, 0, 0, 0, 0, 0, 0]);
        let d_denom = I512::from_limbs([1000, 0, 0, 0, 0, 0, 0, 0]);
        let s1 = i512_from_u64(1_100_000);
        let r1 = i512_from_u64(1_000_000);
        let s2 = i512_from_u64(1_900_000);
        let r2 = i512_from_u64(2_000_000);

        assert_eq!(coeffs.a, g * g * s1 * s2);
        assert_eq!(coeffs.b, I512::ZERO);
        assert_eq!(coeffs.c, g * g * s1 + g * (d_denom * r2));
        assert_eq!(coeffs.d, d_denom * d_denom * r1 * r2);

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

        let a_expect = I512::from_limbs([997, 0, 0, 0, 0, 0, 0, 0]) * i512_from_u64(2_020_000);
        let drg = I512::from_limbs([1000, 0, 0, 0, 0, 0, 0, 0]) * i512_from_u64(1_000_000)
            - I512::from_limbs([997, 0, 0, 0, 0, 0, 0, 0]) * i512_from_u64(10_000);
        let b_expect = -(I512::from_limbs([997, 0, 0, 0, 0, 0, 0, 0]) * i512_from_u64(2_000_000))
            * i512_from_u64(10_000)
            + i512_from_u64(20_000) * drg;

        assert_eq!(coeffs.a, a_expect);
        assert_eq!(coeffs.b, b_expect);
        assert_eq!(coeffs.c, I512::from_limbs([997, 0, 0, 0, 0, 0, 0, 0]));
        assert_eq!(coeffs.d, drg);

        // Cross-check the composed map against direct evaluation at a point.
        let z = i512_from_u64(123_456);
        let direct_num = I512::from_limbs([997, 0, 0, 0, 0, 0, 0, 0])
            * i512_from_u64(2_000_000)
            * (z - i512_from_u64(10_000));
        let direct_den = I512::from_limbs([1000, 0, 0, 0, 0, 0, 0, 0]) * i512_from_u64(1_000_000)
            + I512::from_limbs([997, 0, 0, 0, 0, 0, 0, 0]) * (z - i512_from_u64(10_000));
        let direct = direct_num / direct_den + i512_from_u64(20_000);
        let composed = (coeffs.a * z + coeffs.b) / (coeffs.c * z + coeffs.d);
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
}
