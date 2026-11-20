//! KZG point evaluation precompile added in [`EIP-4844`](https://eips.ethereum.org/EIPS/eip-4844)
//! For more details check [`run`] function.

use crate::{
    interpreter::GasTracker,
    precompiles::{PrecompileHalt, PrecompileOutput, PrecompileResult},
};
pub(crate) mod arkworks;

#[cfg(feature = "blst")]
pub(crate) mod blst;

use alloy_primitives::hex_literal::hex;

/// cost of the KZG point evaluation precompile.
pub(crate) const GAS_COST: u64 = 50_000;

/// Versioned hash version for KZG.
pub(crate) const VERSIONED_HASH_VERSION_KZG: u8 = 0x01;

/// `U256(FIELD_ELEMENTS_PER_BLOB).to_be_bytes() ++ BLS_MODULUS.to_bytes32()`
pub(crate) const RETURN_VALUE: &[u8; 64] = &hex!(
    "0000000000000000000000000000000000000000000000000000000000001000"
    "73eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000001"
);

/// Run kzg point evaluation precompile.
///
/// The Env has the KZGSettings that is needed for evaluation.
///
/// The input is encoded as follows:
/// | versioned_hash |  z  |  y  | commitment | proof |
/// |     32         | 32  | 32  |     48     |   48  |
/// with z and y being padded 32 byte big endian values
pub fn run(input: &[u8], gas: &mut GasTracker) -> PrecompileResult {
    gas.spend(GAS_COST)?;

    // Verify input length.
    if input.len() != 192 {
        return Err(PrecompileHalt::BlobInvalidInputLength.into());
    }

    // Verify commitment matches versioned_hash
    let versioned_hash = &input[..32];
    let commitment = &input[96..144];
    if kzg_to_versioned_hash_with_crypto(crate::precompiles::crypto(), commitment) != versioned_hash
    {
        return Err(PrecompileHalt::BlobMismatchedVersion.into());
    }

    // Verify KZG proof with z and y in big endian format
    let commitment = commitment.as_array().unwrap();
    let z = input[32..].first_chunk().unwrap();
    let y = input[64..].first_chunk().unwrap();
    let proof = input.last_chunk().unwrap();
    crate::precompiles::crypto().verify_kzg_proof(z, y, commitment, proof)?;

    // Return FIELD_ELEMENTS_PER_BLOB and BLS_MODULUS as padded 32 byte big endian values
    Ok(PrecompileOutput::new(RETURN_VALUE.into()))
}

/// `VERSIONED_HASH_VERSION_KZG ++ sha256(commitment)[1..]`
#[inline]
#[allow(dead_code)]
pub(crate) fn kzg_to_versioned_hash(commitment: &[u8]) -> [u8; 32] {
    kzg_to_versioned_hash_with_crypto(&crate::precompiles::DefaultCrypto::new(), commitment)
}

/// `VERSIONED_HASH_VERSION_KZG ++ sha256(commitment)[1..]`
#[inline]
pub(crate) fn kzg_to_versioned_hash_with_crypto(
    crypto: &dyn crate::precompiles::Crypto,
    commitment: &[u8],
) -> [u8; 32] {
    let mut hash = crypto.sha256(commitment);
    hash[0] = VERSIONED_HASH_VERSION_KZG;
    hash
}

/// Verify KZG proof.
#[inline]
pub(crate) fn verify_kzg_proof(
    commitment: &[u8; 48],
    z: &[u8; 32],
    y: &[u8; 32],
    proof: &[u8; 48],
) -> bool {
    core::cfg_select! {
        feature = "c-kzg" => {
            use c_kzg::{Bytes32, Bytes48};

            let as_bytes48 = |bytes: &[u8; 48]| -> &Bytes48 { unsafe { &*bytes.as_ptr().cast() } };
            let as_bytes32 = |bytes: &[u8; 32]| -> &Bytes32 { unsafe { &*bytes.as_ptr().cast() } };

            let kzg_settings = c_kzg::ethereum_kzg_settings(8);
            kzg_settings
                .verify_kzg_proof(
                    as_bytes48(commitment),
                    as_bytes32(z),
                    as_bytes32(y),
                    as_bytes48(proof),
                )
                .unwrap_or(false)
        }
        feature = "blst" => blst::verify_kzg_proof(commitment, z, y, proof),
        _ => arkworks::verify_kzg_proof(commitment, z, y, proof),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{vec, vec::Vec};
    use alloy_primitives::{B256, U256};
    use rand::{RngExt, SeedableRng, rngs::StdRng};

    #[test]
    fn basic_test() {
        // Test data from: https://github.com/ethereum/c-kzg-4844/blob/main/tests/verify_kzg_proof/kzg-mainnet/verify_kzg_proof_case_correct_proof_4_4/data.yaml

        let commitment = hex!("8f59a8d2a1a625a17f3fea0fe5eb8c896db3764f3185481bc22f91b4aaffcca25f26936857bc3a7c2539ea8ec3a952b7").to_vec();
        let mut versioned_hash = crate::precompiles::Crypto::sha256(
            &crate::precompiles::DefaultCrypto::new(),
            &commitment,
        )
        .to_vec();
        versioned_hash[0] = VERSIONED_HASH_VERSION_KZG;
        let z = hex!("73eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000000").to_vec();
        let y = hex!("1522a4a7f34e1ea350ae07c29c96c7e79655aa926122e95fe69fcbd932ca49e9").to_vec();
        let proof = hex!("a62ad71d14c5719385c0686f1871430475bf3a00f0aa3f7b8dd99a9abc2160744faf0070725e00b60ad9a026a15b1a8c").to_vec();

        let input = [versioned_hash, z, y, commitment, proof].concat();

        let expected_output = hex!(
            "000000000000000000000000000000000000000000000000000000000000100073eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000001"
        );
        let mut gas = GasTracker::new(50_000);
        let output = run(&input, &mut gas).unwrap();
        assert_eq!(gas.spent(), GAS_COST);
        assert_eq!(output.bytes()[..], expected_output);
    }

    #[test]
    fn test_invalid_input() {
        let commitment = hex!(
            "c00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
        );
        let z = hex!("0000000000000000000000000000000000000000000000000000000000000000");
        let y = hex!("73eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000001");
        let proof = hex!(
            "c00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
        );

        let t = verify_kzg_proof(&commitment, &z, &y, &proof);
        assert!(!t);
    }

    #[test]
    fn test_valid_input() {
        let commitment = hex!(
            "c00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
        );
        let z = hex!("73eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000000");
        let y = hex!("0000000000000000000000000000000000000000000000000000000000000000");
        let proof = hex!(
            "c00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
        );

        let t = verify_kzg_proof(&commitment, &z, &y, &proof);
        assert!(t);
    }

    /// The proof of a constant polynomial p(x) = 5 is the point at infinity,
    /// with commitment `[5]G1`. Exercises the infinity-pair handling in the
    /// pairing check with a non-zero y.
    #[test]
    #[cfg(feature = "blst")]
    fn test_constant_polynomial_valid_proof() {
        // [5]G1 compressed
        let commitment = hex!(
            "b0e7791fb972fe014159aa33a98622da3cdc98ff707965e536d8636b5fcc5ac7a91a8c46e59a00dca575af0f18fb13dc"
        );
        let z = hex!("0000000000000000000000000000000000000000000000000000000000000003");
        let y = hex!("0000000000000000000000000000000000000000000000000000000000000005");
        let proof = hex!(
            "c00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
        );

        let t = blst::verify_kzg_proof(&commitment, &z, &y, &proof);
        assert!(t);
    }

    /// An infinity proof point paired with a non-infinity `commitment - [y]G1`
    /// is invalid and must be rejected. Exercises the mixed case where only
    /// one pair of the pairing check contains a point at infinity.
    #[test]
    #[cfg(feature = "blst")]
    fn test_infinity_proof_invalid() {
        // [5]G1 compressed
        let commitment = hex!(
            "b0e7791fb972fe014159aa33a98622da3cdc98ff707965e536d8636b5fcc5ac7a91a8c46e59a00dca575af0f18fb13dc"
        );
        let z = hex!("0000000000000000000000000000000000000000000000000000000000000001");
        let y = hex!("0000000000000000000000000000000000000000000000000000000000000000");
        let proof = hex!(
            "c00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
        );

        let t = blst::verify_kzg_proof(&commitment, &z, &y, &proof);
        assert!(!t);
    }

    type Verify = fn(&[u8; 48], &[u8; 32], &[u8; 32], &[u8; 48]) -> bool;
    type Input = ([u8; 48], [u8; 32], [u8; 32], [u8; 48]);
    type Vector = (&'static str, [u8; 48], [u8; 32], [u8; 32], [u8; 48], bool);

    /// Every compiled backend. With `c-kzg`, the dispatching function is the c-kzg backend.
    const BACKENDS: &[(&str, Verify)] = &[
        ("arkworks", arkworks::verify_kzg_proof),
        #[cfg(feature = "blst")]
        ("blst", blst::verify_kzg_proof),
        #[cfg(feature = "c-kzg")]
        ("c-kzg", verify_kzg_proof),
    ];

    fn vectors() -> [Vector; 7] {
        let infinity = hex!(
            "c00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
        );
        let generator = hex!(
            "97f1d3a73197d7942695638c4fa9ac0fc3688c4f9774b905a14e3a3f171bac586c55e83ff97a1aeffb3af00adb22c6bb"
        );
        // [5]G1 compressed
        let five_g1 = hex!(
            "b0e7791fb972fe014159aa33a98622da3cdc98ff707965e536d8636b5fcc5ac7a91a8c46e59a00dca575af0f18fb13dc"
        );
        let zero = [0u8; 32];
        // BLS_MODULUS - 1.
        let r_minus_one = hex!("73eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000000");
        let valid_commitment = hex!(
            "8f59a8d2a1a625a17f3fea0fe5eb8c896db3764f3185481bc22f91b4aaffcca25f26936857bc3a7c2539ea8ec3a952b7"
        );
        let valid_y = hex!("1522a4a7f34e1ea350ae07c29c96c7e79655aa926122e95fe69fcbd932ca49e9");
        let valid_proof = hex!(
            "a62ad71d14c5719385c0686f1871430475bf3a00f0aa3f7b8dd99a9abc2160744faf0070725e00b60ad9a026a15b1a8c"
        );
        let wrong_y = hex!("1522a4a7f34e1ea350ae07c29c96c7e79655aa926122e95fe69fcbd932ca49ea");
        let scalar = |n: u8| B256::with_last_byte(n).0;

        [
            ("c-kzg valid", valid_commitment, r_minus_one, valid_y, valid_proof, true),
            ("c-kzg wrong y", valid_commitment, r_minus_one, wrong_y, valid_proof, false),
            ("constant polynomial", five_g1, scalar(3), scalar(5), infinity, true),
            ("constant polynomial wrong y", five_g1, scalar(1), zero, infinity, false),
            ("zero polynomial", infinity, r_minus_one, zero, infinity, true),
            ("infinity commitment, generator proof", infinity, zero, zero, generator, false),
            (
                "y with maximal chunks",
                hex!(
                    "af9fa6ab68e8bf95421568b59f6fada4aac1f754efdaf173f5aafc8747feeb6ad373ab7967fd5807806fc301df995597"
                ),
                hex!("4dd733910cc8eedc40e2dcdaf04ba70e8d3af425c7cd10cdbe08bae5e9183864"),
                hex!("73eda752ffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                hex!(
                    "aeef75c83def587786d1f97cd044edd51396403cb00bd107d88086e184216e5a6b2c397140fd1615f33a987a48f57a6b"
                ),
                true,
            ),
        ]
    }

    /// Runs each vector through every compiled backend. Reaches
    /// [`arkworks::verify_kzg_proof`], which the dispatching tests never do in workspace builds.
    #[test]
    fn test_backends_agree() {
        for (backend, verify) in BACKENDS {
            for (case, commitment, z, y, proof, expected) in &vectors() {
                assert_eq!(
                    verify(commitment, z, y, proof),
                    *expected,
                    "{backend} backend, case: {case}"
                );
            }
        }
    }

    /// Mutated vectors stress parsing and the pairing check. Every backend must give the same
    /// verdict, and the same as `c-kzg` when it is compiled in.
    #[test]
    fn test_backends_agree_on_mutations() {
        let modulus = U256::from_be_bytes(hex!(
            "73eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000001"
        ));
        let non_canonical =
            |x: &[u8; 32]| U256::from_be_bytes(*x).wrapping_add(modulus).to_be_bytes();
        let infinity = [&[0xc0][..], &[0; 47]].concat().try_into().unwrap();

        let mut rng = StdRng::seed_from_u64(4844);
        let valid = vectors()
            .into_iter()
            .filter(|vector| vector.5)
            .map(|(_, commitment, z, y, proof, _)| (commitment, z, y, proof))
            .chain(c_kzg_vectors());

        for (commitment, z, y, proof) in valid {
            let mut inputs = vec![
                (commitment, z, y, proof),
                (
                    commitment,
                    z,
                    [y[..31].to_vec(), vec![y[31] ^ 1]].concat().try_into().unwrap(),
                    proof,
                ),
                (commitment, non_canonical(&z), y, proof),
                (commitment, z, non_canonical(&y), proof),
                (commitment, [0xff; 32], y, proof),
                (infinity, z, y, proof),
                (commitment, z, y, infinity),
                (proof, z, y, commitment),
                (commitment, z, y, commitment),
                (
                    [&[commitment[0] & 0x7f][..], &commitment[1..]].concat().try_into().unwrap(),
                    z,
                    y,
                    proof,
                ),
                (
                    commitment,
                    z,
                    y,
                    [&[proof[0] & 0x7f][..], &proof[1..]].concat().try_into().unwrap(),
                ),
            ];
            // The point at infinity with leftover bits set.
            for flag in [0xc0, 0xe0, 0xd0] {
                let mut malformed = [0; 48];
                malformed[0] = flag;
                malformed[47] = 1;
                inputs.push((malformed, z, y, proof));
                inputs.push((commitment, z, y, malformed));
            }
            // Single bit flips, and random compressed points, which are off the curve or outside
            // the subgroup.
            for _ in 0..16 {
                let in_commitment = rng.random();
                let mut flipped = if in_commitment { commitment } else { proof };
                flipped[rng.random_range(0..48)] ^= 1 << rng.random_range(0..8);
                inputs.push(if in_commitment {
                    (flipped, z, y, proof)
                } else {
                    (commitment, z, y, flipped)
                });
            }
            for _ in 0..8 {
                let mut random = rng.random::<[u8; 48]>();
                random[0] |= 0x80;
                random[0] &= 0xbf;
                inputs.push((random, z, y, proof));
                inputs.push((commitment, z, y, random));
            }

            for (i, (commitment, z, y, proof)) in inputs.iter().enumerate() {
                let verdicts = BACKENDS
                    .iter()
                    .map(|(backend, verify)| (backend, verify(commitment, z, y, proof)))
                    .collect::<Vec<_>>();
                assert!(
                    verdicts.iter().all(|(_, verdict)| *verdict == verdicts[0].1),
                    "input {i} ({}, {}, {}, {}): {verdicts:?}",
                    hex::encode(commitment),
                    hex::encode(z),
                    hex::encode(y),
                    hex::encode(proof),
                );
                assert!(i != 0 || verdicts[0].1, "valid vector rejected");
            }
        }
    }

    /// Valid proofs of random blobs from `c-kzg`, if compiled in.
    fn c_kzg_vectors() -> Vec<Input> {
        core::cfg_select! {
            feature = "c-kzg" => {
                let mut rng = StdRng::seed_from_u64(7594);
                let settings = c_kzg::ethereum_kzg_settings(8);
                let mut vectors = Vec::new();
                for _ in 0..2 {
                    let mut blob = [0u8; c_kzg::BYTES_PER_BLOB];
                    rng.fill(&mut blob[..]);
                    // Clear the top byte of each field element, so it is below the modulus.
                    blob.as_chunks_mut::<32>().0.iter_mut().for_each(|element| element[0] = 0);
                    let blob = c_kzg::Blob::new(blob);
                    let commitment = settings.blob_to_kzg_commitment(&blob).unwrap().to_bytes();
                    for _ in 0..2 {
                        let mut z = rng.random::<[u8; 32]>();
                        z[0] = 0;
                        let (proof, y) =
                            settings.compute_kzg_proof(&blob, &c_kzg::Bytes32::new(z)).unwrap();
                        vectors.push((*commitment, z, *y, *proof.to_bytes()));
                    }
                }
                vectors
            }
            _ => Vec::new(),
        }
    }
}
