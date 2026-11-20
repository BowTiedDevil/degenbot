//! Differential tests of the BN254 backends against the arkworks backend.
//!
//! Every backend compiled into the crate runs the same seeded inputs as arkworks: edge-case and
//! random G1 additions and multiplications, pairing checks, and G2 points inside and outside the
//! subgroup, including points with torsion components of the twist's cofactor. Results must
//! match exactly, including the halt reason.

#![cfg_attr(not(any(feature = "bn", feature = "bn254-mcl")), expect(dead_code))]

use super::{
    Bn254Ops, FQ_LEN, G1_LEN, G2_LEN, SCALAR_LEN, arkworks::ArkworksOps, g1_point_add_with,
    g1_point_mul_with, pairing_check_with,
};
use crate::precompiles::PrecompileHalt;
use alloc::vec::Vec;
use alloy_primitives::{U256, hex};
use ark_bn254::{Fq, Fq2, Fr, G1Affine, G2Affine};
use ark_ec::{AffineRepr, CurveConfig, CurveGroup};
use ark_ff::{BigInteger, PrimeField, Zero};
use rand::{RngExt, SeedableRng, rngs::StdRng};

const FQ_MODULUS: U256 =
    U256::from_be_bytes(hex!("30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47"));
const FR_MODULUS: U256 =
    U256::from_be_bytes(hex!("30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001"));

const ADD_MUL_CASES: usize = 150;
const PAIRING_CASES: usize = 30;
const G2_BASE_POINTS: usize = 20;

type Output<T> = Result<T, PrecompileHalt>;

#[test]
fn arkworks_sanity() {
    sanity::<ArkworksOps>();
}

#[test]
#[cfg(feature = "bn254-mcl")]
fn mcl_matches_arkworks() {
    sanity::<super::mcl::MclOps>();
    matches_arkworks::<super::mcl::MclOps>();
}

#[test]
#[cfg(feature = "bn")]
fn substrate_matches_arkworks() {
    sanity::<super::substrate::SubstrateOps>();
    matches_arkworks::<super::substrate::SubstrateOps>();
}

/// Checks fixed identities, so a backend that agrees with arkworks is also correct.
fn sanity<O: Bn254Ops>() {
    let g1 = enc_g1(G1Affine::generator());
    let neg_g1 = enc_g1(-G1Affine::generator());
    let g2 = enc_g2(G2Affine::generator());
    let two = U256::from(2).to_be_bytes::<SCALAR_LEN>();

    let doubled = g1_point_add_with::<O>(&g1, &g1).unwrap();
    assert_eq!(g1_point_mul_with::<O>(&g1, &two).unwrap(), doubled);
    assert_eq!(g1_point_add_with::<O>(&g1, &neg_g1).unwrap(), [0; G1_LEN]);

    assert_eq!(pairing::<O>(&[(g1, g2), (neg_g1, g2)]), Ok(true));
    assert_eq!(pairing::<O>(&[(g1, g2), (g1, g2)]), Ok(false));
    assert_eq!(pairing::<O>(&[]), Ok(true));
}

fn matches_arkworks<O: Bn254Ops>() {
    let mut rng = StdRng::seed_from_u64(0xb254);
    g1_ops_match::<O>(&mut rng);
    pairings_match::<O>(&mut rng);
    g2_subgroup_matches::<O>(&mut rng);
}

fn g1_ops_match<O: Bn254Ops>(rng: &mut StdRng) {
    for _ in 0..ADD_MUL_CASES {
        let a = random_g1_input(rng);
        let b = match rng.random_range(0..5) {
            0 => a,
            1 => negate_g1_input(&a),
            _ => random_g1_input(rng),
        };
        assert_eq!(
            g1_point_add_with::<O>(&a, &b),
            g1_point_add_with::<ArkworksOps>(&a, &b),
            "add({}, {})",
            hex::encode(a),
            hex::encode(b)
        );

        let scalar = random_scalar(rng);
        assert_eq!(
            g1_point_mul_with::<O>(&a, &scalar),
            g1_point_mul_with::<ArkworksOps>(&a, &scalar),
            "mul({}, {})",
            hex::encode(a),
            hex::encode(scalar)
        );
    }
}

fn pairings_match<O: Bn254Ops>(rng: &mut StdRng) {
    for _ in 0..PAIRING_CASES {
        // A balanced product of up to 20 pairs, which spans mcl's 16-pair chunks.
        let n = rng.random_range(1..=20);
        let mut pairs = Vec::new();
        let mut sum = Fr::zero();
        for _ in 0..n {
            let a = random_fr(rng);
            let b = random_fr(rng);
            sum += a * b;
            pairs.push((
                enc_g1((G1Affine::generator() * a).into_affine()),
                enc_g2((G2Affine::generator() * b).into_affine()),
            ));
        }
        pairs.push((
            enc_g1((G1Affine::generator() * -sum).into_affine()),
            enc_g2(G2Affine::generator()),
        ));
        assert_eq!(pairing::<O>(&pairs), Ok(true));
        assert_eq!(pairing::<O>(&pairs), pairing::<ArkworksOps>(&pairs));

        // Any change to a pair unbalances the product or invalidates a point.
        let i = rng.random_range(0..pairs.len());
        match rng.random_range(0..4) {
            0 => pairs[i].0 = random_g1_input(rng),
            1 => pairs[i].0 = [0; G1_LEN],
            2 => pairs[i].1 = [0; G2_LEN],
            _ => pairs[i].1 = random_g2_input(rng),
        }
        assert_eq!(pairing::<O>(&pairs), pairing::<ArkworksOps>(&pairs));
    }
}

/// G2 membership: a point is valid only if it is on the twist and has order `r`.
fn g2_subgroup_matches<O: Bn254Ops>(rng: &mut StdRng) {
    let cofactor = <ark_bn254::g2::Config as CurveConfig>::COFACTOR;
    for _ in 0..G2_BASE_POINTS {
        let q = random_twist_point(rng);
        // `q * cofactor` has order `r`; `q * r` has order dividing the cofactor.
        let in_subgroup = q.mul_bigint(cofactor).into_affine();
        let torsion = q.mul_bigint(Fr::MODULUS).into_affine();
        let candidates = [
            q,
            -q,
            in_subgroup,
            torsion,
            (in_subgroup + torsion).into_affine(),
            (in_subgroup + q).into_affine(),
            (torsion + torsion).into_affine(),
            (in_subgroup + in_subgroup).into_affine(),
            G2Affine::zero(),
        ];
        for candidate in candidates {
            let expected = candidate.mul_bigint(Fr::MODULUS).is_zero();
            let encoded = enc_g2(candidate);
            let pairs = [([0; G1_LEN], encoded)];
            let result = pairing::<O>(&pairs);
            assert_eq!(result.is_ok(), expected, "G2 {}", hex::encode(encoded));
            assert_eq!(result, pairing::<ArkworksOps>(&pairs));
        }
    }

    // Random encodings, mostly off the curve or non-canonical.
    for _ in 0..G2_BASE_POINTS * 5 {
        let encoded = random_g2_input(rng);
        let pairs = [([0; G1_LEN], encoded)];
        assert_eq!(
            pairing::<O>(&pairs),
            pairing::<ArkworksOps>(&pairs),
            "G2 {}",
            hex::encode(encoded)
        );
    }
}

fn pairing<O: Bn254Ops>(pairs: &[([u8; G1_LEN], [u8; G2_LEN])]) -> Output<bool> {
    let pairs = pairs.iter().map(|(g1, g2)| (&g1[..], &g2[..])).collect::<Vec<_>>();
    pairing_check_with::<O>(&pairs)
}

fn enc_fq(x: Fq) -> [u8; FQ_LEN] {
    let mut out = [0; FQ_LEN];
    out.copy_from_slice(&x.into_bigint().to_bytes_be());
    out
}

fn enc_g1(p: G1Affine) -> [u8; G1_LEN] {
    let mut out = [0; G1_LEN];
    if let Some((x, y)) = p.xy() {
        out[..FQ_LEN].copy_from_slice(&enc_fq(x));
        out[FQ_LEN..].copy_from_slice(&enc_fq(y));
    }
    out
}

/// Encodes as `x.c1 | x.c0 | y.c1 | y.c0`.
fn enc_g2(p: G2Affine) -> [u8; G2_LEN] {
    let mut out = [0; G2_LEN];
    if let Some((x, y)) = p.xy() {
        for (i, c) in [x.c1, x.c0, y.c1, y.c0].into_iter().enumerate() {
            out[i * FQ_LEN..(i + 1) * FQ_LEN].copy_from_slice(&enc_fq(c));
        }
    }
    out
}

fn random_fq(rng: &mut StdRng) -> Fq {
    Fq::from_be_bytes_mod_order(&rng.random::<[u8; 32]>())
}

fn random_fr(rng: &mut StdRng) -> Fr {
    Fr::from_be_bytes_mod_order(&rng.random::<[u8; 32]>())
}

/// A random point of the twist, in the subgroup only with probability `1 / cofactor`.
fn random_twist_point(rng: &mut StdRng) -> G2Affine {
    loop {
        let x = Fq2::new(random_fq(rng), random_fq(rng));
        if let Some(p) = G2Affine::get_point_from_x_unchecked(x, rng.random()) {
            return p;
        }
    }
}

/// An encoded G1 point: valid, the point at infinity, off the curve, or non-canonical.
fn random_g1_input(rng: &mut StdRng) -> [u8; G1_LEN] {
    let valid = loop {
        if let Some(p) = G1Affine::get_point_from_x_unchecked(random_fq(rng), rng.random()) {
            break enc_g1(p);
        }
    };
    let mut out = valid;
    match rng.random_range(0..8) {
        0 => out = [0; G1_LEN],
        1 => out = rng.random(),
        2 => out[..FQ_LEN].copy_from_slice(&add_fq_modulus(&valid[..FQ_LEN])),
        3 => out[FQ_LEN..].copy_from_slice(&add_fq_modulus(&valid[FQ_LEN..])),
        4 => out[G1_LEN - 1] ^= 1,
        5 => out[..FQ_LEN].copy_from_slice(&[0xff; FQ_LEN]),
        _ => {}
    }
    out
}

fn negate_g1_input(input: &[u8; G1_LEN]) -> [u8; G1_LEN] {
    let mut out = *input;
    let y = U256::from_be_slice(&input[FQ_LEN..]);
    if !y.is_zero() && y < FQ_MODULUS {
        out[FQ_LEN..].copy_from_slice(&(FQ_MODULUS - y).to_be_bytes::<FQ_LEN>());
    }
    out
}

/// An encoded G2 point: random bytes, a twist point, or a twist point with a mutated coordinate.
fn random_g2_input(rng: &mut StdRng) -> [u8; G2_LEN] {
    let mut out = enc_g2(random_twist_point(rng));
    match rng.random_range(0..5) {
        0 => out = rng.random(),
        1 => out[rng.random_range(0..G2_LEN)] ^= 1 << rng.random_range(0..8),
        2 => {
            let i = rng.random_range(0..4) * FQ_LEN;
            let shifted = add_fq_modulus(&out[i..i + FQ_LEN]);
            out[i..i + FQ_LEN].copy_from_slice(&shifted);
        }
        3 => out[..FQ_LEN].copy_from_slice(&[0xff; FQ_LEN]),
        _ => {}
    }
    out
}

fn add_fq_modulus(x: &[u8]) -> [u8; FQ_LEN] {
    U256::from_be_slice(x).wrapping_add(FQ_MODULUS).to_be_bytes()
}

fn random_scalar(rng: &mut StdRng) -> [u8; SCALAR_LEN] {
    let k = U256::from(rng.random_range(1..4u8));
    let scalar = match rng.random_range(0..7) {
        0 => U256::ZERO,
        1 => U256::ONE,
        2 => FR_MODULUS - U256::ONE,
        3 => FR_MODULUS,
        4 => FR_MODULUS + k,
        5 => U256::MAX,
        _ => return rng.random(),
    };
    scalar.to_be_bytes()
}
