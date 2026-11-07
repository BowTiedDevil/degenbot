//! BN254 precompile implementation using herumi/mcl via [`mcl_rust`].
//!
//! mcl is fastest on x86-64 CPUs with AVX, BMI2, and ADX, where it JIT-compiles its field
//! arithmetic with `mulx`, `adcx`, and `adox`; this cuts pairing time by about 40%. Without
//! these features, including under emulators that do not report ADX such as valgrind, mcl runs
//! generic code that is slower but still faster than the arkworks backend.

use super::{Bn254Ops, FQ_LEN, FQ2_LEN, G1_LEN, SCALAR_LEN};
use crate::precompiles::PrecompileHalt;
use alloy_primitives::hex;
use mcl_rust::{CurveType, Fp, Fp2, Fr, G1, G2, GT};
use std::sync::{Once, OnceLock};

// Not exposed by `mcl_rust`.
unsafe extern "C" {
    fn mclBn_getCurveType() -> i32;
    fn mclBn_millerLoopVec(z: *mut GT, x: *const G1, y: *const G2, n: usize);
    fn mclBn_verifyOrderG2(do_verify: i32);
    fn mclBnFp_serialize(buf: *mut u8, max_buf_size: usize, x: *const Fp) -> usize;
}

/// The BN254 base field modulus, big-endian.
const FQ_MODULUS: [u8; FQ_LEN] =
    hex!("30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47");

pub(crate) struct MclOps;

impl Bn254Ops for MclOps {
    type G1 = G1;
    type G2 = G2;
    type Scalar = Fr;

    #[inline]
    fn init() {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            // CurveType::SNARK (MCL_BN_SNARK1) is the Ethereum-compatible BN254 (alt_bn128).
            // CurveType::BN254 is a different, older BN254 parameterization.
            assert!(mcl_rust::init(CurveType::SNARK), "mcl BN254 initialization failed");
            // mcl checks the G2 order with a generic multiplication by r; `read_g2` uses the
            // faster endomorphism check instead.
            unsafe { mclBn_verifyOrderG2(0) };
        });

        let curve = unsafe { mclBn_getCurveType() };
        assert_eq!(curve, CurveType::SNARK as i32, "mcl curve changed after initialization");
    }

    #[inline]
    fn read_g1(input: &[u8]) -> Result<Self::G1, PrecompileHalt> {
        let [px, py, ..] = input.as_chunks::<FQ_LEN>().0 else {
            panic!("input must be at least G1_LEN bytes");
        };
        let px = read_fp(px)?;
        let py = read_fp(py)?;

        if px.is_zero() && py.is_zero() {
            return Ok(G1::zero());
        }

        let mut p = G1::zero();
        p.x = px;
        p.y = py;
        p.z = Fp::from_int(1);

        if !p.is_valid() {
            return Err(PrecompileHalt::Bn254AffineGFailedToCreate);
        }

        Ok(p)
    }

    #[inline]
    fn encode_g1(point: Self::G1) -> [u8; G1_LEN] {
        if point.is_zero() {
            return [0u8; G1_LEN];
        }

        // Normalize to affine coordinates (z = 1).
        let mut affine = G1::zero();
        G1::normalize(&mut affine, &point);

        let mut output = [0u8; G1_LEN];
        output[..FQ_LEN].copy_from_slice(&encode_fp(&affine.x));
        output[FQ_LEN..].copy_from_slice(&encode_fp(&affine.y));
        output
    }

    #[inline]
    fn read_g2(input: &[u8]) -> Result<Self::G2, PrecompileHalt> {
        let [x, y, ..] = input.as_chunks::<FQ2_LEN>().0 else {
            panic!("input must be at least G2_LEN bytes");
        };
        let x = read_fp2(x)?;
        let y = read_fp2(y)?;

        if x.is_zero() && y.is_zero() {
            return Ok(G2::zero());
        }

        let mut p = G2::zero();
        p.x = x;
        p.y = y;
        p.z.d[0] = Fp::from_int(1);

        if !p.is_valid() || !g2_in_subgroup(&p) {
            return Err(PrecompileHalt::Bn254AffineGFailedToCreate);
        }

        Ok(p)
    }

    #[inline]
    fn read_scalar(input: &[u8]) -> Self::Scalar {
        let mut le_bytes = *input.as_array::<SCALAR_LEN>().expect("input must be SCALAR_LEN bytes");
        le_bytes.reverse();

        let mut fr = Fr::zero();
        fr.set_little_endian_mod(&le_bytes);
        fr
    }

    #[inline]
    fn g1_is_zero(p: &Self::G1) -> bool {
        p.is_zero()
    }

    #[inline]
    fn g2_is_zero(p: &Self::G2) -> bool {
        p.is_zero()
    }

    #[inline]
    fn g1_add(p1: Self::G1, p2: Self::G1) -> Self::G1 {
        &p1 + &p2
    }

    #[inline]
    fn g1_mul(p: Self::G1, s: Self::Scalar) -> Self::G1 {
        let mut result = G1::zero();
        G1::mul(&mut result, &p, &s);
        result
    }

    #[inline]
    fn pairing_check(g1: &[Self::G1], g2: &[Self::G2]) -> bool {
        debug_assert_eq!(g1.len(), g2.len());
        // One batched Miller loop shares its squarings across all pairs, which is cheaper than
        // running a loop per pair and multiplying the results.
        let mut acc = GT::zero();
        unsafe { mclBn_millerLoopVec(&mut acc, g1.as_ptr(), g2.as_ptr(), g1.len()) };

        let mut result = GT::zero();
        mcl_rust::final_exp(&mut result, &acc);

        result.is_one()
    }
}

#[inline]
fn read_fp(input: &[u8; FQ_LEN]) -> Result<Fp, PrecompileHalt> {
    if *input >= FQ_MODULUS {
        return Err(PrecompileHalt::Bn254FieldPointNotAMember);
    }

    let mut le_bytes = *input;
    le_bytes.reverse();

    let mut fp = Fp::zero();
    if !fp.set_little_endian(&le_bytes) {
        return Err(PrecompileHalt::Bn254FieldPointNotAMember);
    }
    Ok(fp)
}

#[inline]
fn encode_fp(fp: &Fp) -> [u8; FQ_LEN] {
    // `Fp::serialize` returns a heap-allocated `Vec`.
    let mut result = [0u8; FQ_LEN];
    let len = unsafe { mclBnFp_serialize(result.as_mut_ptr(), FQ_LEN, fp) };
    assert_eq!(len, FQ_LEN, "mcl Fp serialization failed");
    result.reverse();
    result
}

#[inline]
fn read_fp2(input: &[u8]) -> Result<Fp2, PrecompileHalt> {
    let [imag, real, ..] = input.as_chunks::<FQ_LEN>().0 else {
        panic!("input must be at least FQ2_LEN bytes");
    };
    let imag = read_fp(imag)?;
    let real = read_fp(real)?;
    let mut fp2 = Fp2::zero();
    fp2.d[0] = real;
    fp2.d[1] = imag;
    Ok(fp2)
}

/// The BN254 curve parameter `x`.
const BN_X: u64 = 4965661367192848881;

/// Checks that an on-curve G2 point is in the r-torsion subgroup by testing
/// `[x + 1]P + psi([x]P) + psi^2([x]P) == psi^3([2x]P)`, from sections 3 and 5.1 of
/// <https://eprint.iacr.org/2022/348.pdf>. gnark-crypto uses the same check.
///
/// mcl's own check multiplies by the 254-bit group order, while this needs a 63-bit
/// multiplication and three endomorphisms.
#[inline]
fn g2_in_subgroup(p: &G2) -> bool {
    static PSI_COEFFS: OnceLock<(Fp2, Fp2)> = OnceLock::new();
    let (psi_x, psi_y) = PSI_COEFFS.get_or_init(|| {
        let fp2 = |c0, c1| {
            let mut r = Fp2::zero();
            assert!(r.d[0].set_str(c0, 10) && r.d[1].set_str(c1, 10));
            r
        };
        (
            // (u + 9)^((p - 1) / 3)
            fp2(
                "21575463638280843010398324269430826099269044274347216827212613867836435027261",
                "10307601595873709700152284273816112264069230130616436755625194854815875713954",
            ),
            // (u + 9)^((p - 1) / 2)
            fp2(
                "2821565182194536844548159561693502659359617185244120367078079554186484126554",
                "3505843767911556378687030309984248845540243509899259641013678093033130930403",
            ),
        )
    });

    // psi is the untwist-Frobenius-twist endomorphism. For affine P it maps
    // (x, y) to (conj(x) * psi_x, conj(y) * psi_y), where conj negates the imaginary part.
    // conj is a field automorphism, so conjugating z as well maps Jacobian coordinates.
    let conj = |a: &Fp2| {
        let mut r = a.clone();
        Fp::neg(&mut r.d[1], &a.d[1]);
        r
    };
    let psi = |a: &G2| {
        let mut r = G2::zero();
        r.x = &conj(&a.x) * psi_x;
        r.y = &conj(&a.y) * psi_y;
        r.z = conj(&a.z);
        r
    };

    // Compute [x]P with plain double-and-add. `G2::mul` splits the scalar with the GLV
    // endomorphism, which only gives the right product for points that are already in the
    // subgroup, so it cannot be used to test membership.
    let mut xp = p.clone();
    for i in (0..BN_X.ilog2()).rev() {
        let mut doubled = G2::zero();
        G2::dbl(&mut doubled, &xp);
        xp = doubled;
        if (BN_X >> i) & 1 == 1 {
            xp += p;
        }
    }

    let psi_xp = psi(&xp);
    let psi2_xp = psi(&psi_xp);
    let mut rhs = G2::zero();
    G2::dbl(&mut rhs, &psi(&psi2_xp));
    let mut lhs = psi2_xp;
    lhs += &psi_xp;
    lhs += &xp;
    lhs += p;
    lhs == rhs
}
