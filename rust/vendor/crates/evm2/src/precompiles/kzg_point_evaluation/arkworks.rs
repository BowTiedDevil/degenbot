//! KZG point evaluation precompile using Arkworks BLS12-381 implementation.

#![allow(dead_code)]

use crate::precompiles::{OnceLock, PrecompileHalt, bls12_381_const::TRUSTED_SETUP_TAU_G2_BYTES};
use ark_bls12_381::{Bls12_381, Config, Fr, G1Affine, G2Affine};
use ark_ec::{AffineRepr, CurveGroup, bls12::G2Prepared, pairing::Pairing};
use ark_ff::{BigInteger, One, PrimeField};
use ark_serialize::CanonicalDeserialize;

/// Verify KZG proof using BLS12-381 implementation.
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
    // We expect 32-byte big-endian scalars that must be canonical
    let Ok(z_fr) = read_scalar_canonical(z) else {
        return false;
    };
    let Ok(y_fr) = read_scalar_canonical(y) else {
        return false;
    };

    // The spec checks e(P - [y]G₁, -G₂) * e(proof, [τ]G₂ - [z]G₂) == 1. By bilinearity,
    // e(proof, -[z]G₂) == e([z]proof, -G₂), so this is equivalent to
    // e(P - [y]G₁ + [z]proof, -G₂) * e(proof, [τ]G₂) == 1, where both G₂ points are fixed.
    let lhs = (commitment_point.into_group()
        - G1Affine::generator().mul_bigint(y_fr.into_bigint())
        + proof_point.mul_bigint(z_fr.into_bigint()))
    .into_affine();
    Bls12_381::multi_pairing([lhs, proof_point], g2_prepared().clone()).0.is_one()
}

/// Returns the prepared `-G₂` and trusted setup point `[τ]G₂`.
fn g2_prepared() -> &'static [G2Prepared<Config>; 2] {
    static G2: OnceLock<[G2Prepared<Config>; 2]> = OnceLock::new();
    G2.get_or_init(|| {
        // Parse g2_monomial_1 from trusted_setup_4096.json using unchecked deserialization since
        // we trust this point.
        let tau_g2 = G2Affine::deserialize_compressed_unchecked(&TRUSTED_SETUP_TAU_G2_BYTES[..])
            .expect("Failed to parse trusted setup G2 point");
        [(-G2Affine::generator()).into(), tau_g2.into()]
    })
}

/// Parse a G1 point from compressed format (48 bytes)
fn parse_g1_compressed(bytes: &[u8; 48]) -> Result<G1Affine, PrecompileHalt> {
    G1Affine::deserialize_compressed(&bytes[..]).map_err(|_| PrecompileHalt::KzgInvalidG1Point)
}

/// Read a scalar field element from bytes and verify it's canonical
fn read_scalar_canonical(bytes: &[u8; 32]) -> Result<Fr, PrecompileHalt> {
    let fr = Fr::from_be_bytes_mod_order(bytes);

    // Check if the field element is canonical by serializing back and comparing
    let bytes_roundtrip = fr.into_bigint().to_bytes_be();

    if bytes_roundtrip != bytes {
        return Err(PrecompileHalt::NonCanonicalFp);
    }

    Ok(fr)
}
