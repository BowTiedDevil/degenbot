//! KZG point evaluation precompile using BLST BLS12-381 implementation.

#![allow(dead_code)]

use crate::precompiles::{OnceLock, PrecompileHalt, bls12_381_const::TRUSTED_SETUP_TAU_G2_BYTES};
use ::blst::{
    BLS12_381_G1, BLS12_381_NEG_G2, blst_final_exp, blst_fp, blst_fp_add, blst_fp_cneg,
    blst_fp_mul, blst_fp6, blst_fp12, blst_fp12_conjugate, blst_fp12_is_one,
    blst_fp12_mul_by_xy00z0, blst_fp12_one, blst_fp12_sqr, blst_p1, blst_p1_add_or_double,
    blst_p1_add_or_double_affine, blst_p1_affine, blst_p1_affine_in_g1, blst_p1_affine_is_inf,
    blst_p1_affine_on_curve, blst_p1_cneg, blst_p1_double, blst_p1_from_affine, blst_p1_mult,
    blst_p1_to_affine, blst_p1s_mult_wbits, blst_p1s_mult_wbits_precompute, blst_p2_affine,
    blst_precompute_lines, blst_scalar, blst_scalar_fr_check, blst_scalar_from_bendian,
};
use core::ptr;

/// Precomputed Miller loop lines of a G2 point, see `blst_precompute_lines`.
type Lines = [blst_fp6; 68];

/// Number of chunks `[y]G₁` is split into, see [`g1_table`].
const G1_CHUNKS: usize = 8;

/// Number of bits in each chunk of `y`.
const G1_CHUNK_BITS: usize = 256 / G1_CHUNKS;

/// Window size of the fixed-base `[y]G₁` multiplication.
const G1_WBITS: usize = 6;

/// Number of points in [`g1_table`].
const G1_TABLE_LEN: usize = G1_CHUNKS << (G1_WBITS - 1);

/// Verify KZG proof using BLST BLS12-381 implementation.
///
/// <https://github.com/ethereum/EIPs/blob/4d2a00692bb131366ede1a16eced2b0e25b1bf99/EIPS/eip-4844.md?plain=1#L203>
/// <https://github.com/ethereum/consensus-specs/blob/master/specs/deneb/polynomial-commitments.md#verify_kzg_proof_impl>
#[inline]
pub(crate) fn verify_kzg_proof(
    commitment: &[u8; 48],
    z: &[u8; 32],
    y: &[u8; 32],
    proof: &[u8; 48],
) -> bool {
    // Parse the commitment (G1 point)
    let Ok(commitment_point) = parse_g1_compressed(commitment) else {
        return false;
    };

    // Parse the proof (G1 point)
    let Ok(proof_point) = parse_g1_compressed(proof) else {
        return false;
    };

    // Parse z and y as field elements (Fr, scalar field)
    let Ok(z_scalar) = read_scalar_canonical(z) else {
        return false;
    };
    let Ok(y_scalar) = read_scalar_canonical(y) else {
        return false;
    };

    // The spec checks e(P - [y]G₁, -G₂) * e(proof, [τ]G₂ - [z]G₂) == 1. By bilinearity,
    // e(proof, -[z]G₂) == e([z]proof, -G₂), so this is equivalent to
    // e(P - [y]G₁ + [z]proof, -G₂) * e(proof, [τ]G₂) == 1, where both G₂ points are fixed.
    let lhs = p1_lhs(&commitment_point, &proof_point, &z_scalar, &y_scalar);
    let [neg_g2_lines, tau_g2_lines] = g2_lines();
    multi_pairing_is_one([(&lhs, neg_g2_lines), (&proof_point, tau_g2_lines)])
}

/// Computes `commitment - [y]G₁ + [z]proof`.
fn p1_lhs(
    commitment: &blst_p1_affine,
    proof: &blst_p1_affine,
    z: &blst_scalar,
    y: &blst_scalar,
) -> blst_p1_affine {
    let mut acc = blst_p1::default();
    let mut proof_jacobian = blst_p1::default();
    let mut z_proof = blst_p1::default();
    let mut out = blst_p1_affine::default();
    // `[y]G₁` is the sum of `[y_i][2^(32i)]G₁` over the 32-bit little-endian chunks `y_i`.
    let y_chunks = [y.b.as_ptr(), ptr::null()];
    // SAFETY: all arguments are valid blst types, `y_chunks` is a null-terminated array pointing
    // to `G1_CHUNKS` contiguous chunks of `G1_CHUNK_BITS` bits, and a null scratch buffer is
    // allocated by blst.
    unsafe {
        blst_p1s_mult_wbits(
            &mut acc,
            g1_table().as_ptr(),
            G1_WBITS,
            G1_CHUNKS,
            y_chunks.as_ptr(),
            G1_CHUNK_BITS,
            ptr::null_mut(),
        );
        blst_p1_cneg(&mut acc, true);
        let acc_ptr = &raw mut acc;
        blst_p1_add_or_double_affine(acc_ptr, acc_ptr, commitment);
        blst_p1_from_affine(&mut proof_jacobian, proof);
        blst_p1_mult(&mut z_proof, &proof_jacobian, z.b.as_ptr(), 256);
        blst_p1_add_or_double(acc_ptr, acc_ptr, &z_proof);
        blst_p1_to_affine(&mut out, &acc);
    }
    out
}

/// Returns the `blst_p1s_mult_wbits` table of `[2^(G1_CHUNK_BITS * i)]G₁` for `i` in
/// `0..G1_CHUNKS`.
fn g1_table() -> &'static [blst_p1_affine; G1_TABLE_LEN] {
    static TABLE: OnceLock<[blst_p1_affine; G1_TABLE_LEN]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut points = [blst_p1_affine::default(); G1_CHUNKS];
        let mut p = blst_p1::default();
        let p_ptr = &raw mut p;
        let mut table = [blst_p1_affine::default(); G1_TABLE_LEN];
        // SAFETY: all arguments are valid blst types and `table` has the size given by
        // `blst_p1s_mult_wbits_precompute_sizeof`.
        unsafe {
            blst_p1_from_affine(p_ptr, &BLS12_381_G1);
            for point in &mut points {
                blst_p1_to_affine(point, p_ptr);
                for _ in 0..G1_CHUNK_BITS {
                    blst_p1_double(p_ptr, p_ptr);
                }
            }
            let points = points.each_ref().map(ptr::from_ref);
            blst_p1s_mult_wbits_precompute(
                table.as_mut_ptr(),
                G1_WBITS,
                points.as_ptr(),
                G1_CHUNKS,
            );
        }
        table
    })
}

/// Checks that the product of the pairings of each G1 point with the G2 point of its precomputed
/// lines is the identity.
///
/// G1 points at infinity are skipped: their pairing is the identity, but the line evaluations
/// do not special-case infinity. E.g. the proof of a constant polynomial is the point at infinity.
///
/// The Miller loop follows `blst_miller_loop_lines`, but shares the squarings between the pairs.
fn multi_pairing_is_one(pairs: [(&blst_p1_affine, &Lines); 2]) -> bool {
    // Move the common expression from the line evaluation: (-2 * P.x, 2 * P.y).
    let terms = pairs.map(|(p, lines)| {
        // SAFETY: all arguments are valid blst types.
        unsafe {
            if blst_p1_affine_is_inf(p) {
                return None;
            }
            let (mut x, mut y) = (blst_fp::default(), blst_fp::default());
            blst_fp_add(&mut x, &p.x, &p.x);
            let x_ptr = &raw mut x;
            blst_fp_cneg(x_ptr, x_ptr, true);
            blst_fp_add(&mut y, &p.y, &p.y);
            Some((x, y, lines))
        }
    });

    // SAFETY: `blst_fp12_one` returns a valid pointer to a static.
    let mut f = unsafe { *blst_fp12_one() };
    let f_ptr = &raw mut f;
    for i in 0..68 {
        // Lines 1, 4, 8, 18 and 51 are additions, the rest are doublings that follow a squaring,
        // except for the first doubling that starts from one.
        if !matches!(i, 0 | 1 | 4 | 8 | 18 | 51) {
            // SAFETY: `f` is a valid blst type; blst allows aliasing the output and input.
            unsafe { blst_fp12_sqr(f_ptr, f_ptr) };
        }
        for (x, y, lines) in terms.iter().flatten() {
            let l = &lines[i];
            let mut line = *l;
            // SAFETY: all arguments are valid blst types.
            unsafe {
                blst_fp_mul(&mut line.fp2[1].fp[0], &l.fp2[1].fp[0], x);
                blst_fp_mul(&mut line.fp2[1].fp[1], &l.fp2[1].fp[1], x);
                blst_fp_mul(&mut line.fp2[2].fp[0], &l.fp2[2].fp[0], y);
                blst_fp_mul(&mut line.fp2[2].fp[1], &l.fp2[2].fp[1], y);
                blst_fp12_mul_by_xy00z0(f_ptr, f_ptr, &line);
            }
        }
    }

    let mut out = blst_fp12::default();
    // SAFETY: all arguments are valid blst types.
    unsafe {
        // Account for the BLS parameter being negative.
        blst_fp12_conjugate(f_ptr);
        blst_final_exp(&mut out, &f);
        blst_fp12_is_one(&out)
    }
}

/// Returns the precomputed Miller loop lines of `-G₂` and of the trusted setup point `[τ]G₂`.
fn g2_lines() -> &'static [Lines; 2] {
    static LINES: OnceLock<[Lines; 2]> = OnceLock::new();
    LINES.get_or_init(|| {
        let mut tau_g2 = blst_p2_affine::default();
        // SAFETY: all arguments are valid blst types.
        unsafe {
            let result = blst::blst_p2_uncompress(&mut tau_g2, TRUSTED_SETUP_TAU_G2_BYTES.as_ptr());
            if result != blst::BLST_ERROR::BLST_SUCCESS {
                panic!("Failed to deserialize trusted setup G2 point");
            }
        }
        let mut lines = [[blst_fp6::default(); 68]; 2];
        // SAFETY: all arguments are valid blst types and `lines` has 68 entries each.
        unsafe {
            blst_precompute_lines(lines[0].as_mut_ptr(), &BLS12_381_NEG_G2);
            blst_precompute_lines(lines[1].as_mut_ptr(), &tau_g2);
        }
        lines
    })
}

/// Parse a G1 point from compressed format (48 bytes)
fn parse_g1_compressed(bytes: &[u8; 48]) -> Result<blst_p1_affine, PrecompileHalt> {
    let mut point = blst_p1_affine::default();
    unsafe {
        let result = blst::blst_p1_uncompress(&mut point, bytes.as_ptr());
        if result != blst::BLST_ERROR::BLST_SUCCESS {
            return Err(PrecompileHalt::KzgInvalidG1Point);
        }

        // Verify the point is on curve
        if !blst_p1_affine_on_curve(&point) {
            return Err(PrecompileHalt::KzgG1PointNotOnCurve);
        }

        // Verify the point is in the correct subgroup
        if !blst_p1_affine_in_g1(&point) {
            return Err(PrecompileHalt::KzgG1PointNotInSubgroup);
        }
    }
    Ok(point)
}

/// Read a scalar field element from bytes and verify it's canonical
fn read_scalar_canonical(bytes: &[u8; 32]) -> Result<blst_scalar, PrecompileHalt> {
    let mut scalar = blst_scalar::default();

    // Read scalar from big endian bytes
    unsafe {
        blst_scalar_from_bendian(&mut scalar, bytes.as_ptr());
    }

    if unsafe { !blst_scalar_fr_check(&scalar) } {
        return Err(PrecompileHalt::NonCanonicalFp);
    }

    Ok(scalar)
}
