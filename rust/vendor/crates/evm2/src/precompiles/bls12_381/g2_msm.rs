//! BLS12-381 G2 msm precompile. More details in [`run`]

use super::utils::{pad_g2_point, remove_g2_padding};
use crate::{
    interpreter::GasTracker,
    precompiles::{
        PrecompileHalt, PrecompileOutput, PrecompileResult,
        bls12_381_const::{
            DISCOUNT_TABLE_G2_MSM, G2_MSM_BASE_GAS_FEE, G2_MSM_INPUT_LENGTH, SCALAR_LENGTH,
        },
        bls12_381_utils::msm_required_gas,
    },
};

/// Implements EIP-2537 G2MSM precompile.
/// G2 multi-scalar-multiplication call expects `288*k` bytes as an input that is interpreted
/// as byte concatenation of `k` slices each of them being a byte concatenation
/// of encoding of G2 point (`256` bytes) and encoding of a scalar value (`32`
/// bytes).
/// Output is an encoding of multi-scalar-multiplication operation result - single G2
/// point (`256` bytes).
/// See also: <https://eips.ethereum.org/EIPS/eip-2537#abi-for-g2-multiexponentiation>
pub fn run(input: &[u8], gas: &mut GasTracker) -> PrecompileResult {
    let input_len = input.len();
    if input_len == 0 || !input_len.is_multiple_of(G2_MSM_INPUT_LENGTH) {
        return Err(PrecompileHalt::Bls12381G2MsmInputLength.into());
    }

    let input_chunks = input.as_chunks::<G2_MSM_INPUT_LENGTH>().0;
    let k = input_chunks.len();
    let required_gas = msm_required_gas(k, &DISCOUNT_TABLE_G2_MSM, G2_MSM_BASE_GAS_FEE);
    gas.spend(required_gas)?;

    let mut valid_pairs_iter = input_chunks.iter().map(|pair| {
        let (padded_g2, scalar_array) = pair.split_last_chunk::<SCALAR_LENGTH>().unwrap();

        // Remove padding from G2 point - this validates padding format
        let [x_0, x_1, y_0, y_1] = remove_g2_padding(padded_g2)?;

        Ok(((*x_0, *x_1, *y_0, *y_1), *scalar_array))
    });

    let unpadded_result = crate::precompiles::crypto().bls12_381_g2_msm(&mut valid_pairs_iter)?;

    // Pad the result for EVM compatibility
    let padded_result = pad_g2_point(&unpadded_result);

    Ok(PrecompileOutput::new(padded_result.into()))
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::precompiles::kzg_point_evaluation::RETURN_VALUE;
    use alloc::vec::Vec;
    use alloy_primitives::{U256, hex};

    /// Scalars are reduced modulo the subgroup order `r`: `s` and `s mod r` give the same result,
    /// and invalid points are rejected whatever their scalar.
    #[test]
    fn g2_msm_scalars_reduced_mod_r() {
        let r = U256::from_be_slice(&RETURN_VALUE[32..]);
        let point = hex!(
            "00000000000000000000000000000000024aa2b2f08f0a91260805272dc51051c6e47ad4fa403b02b4510b647ae3d1770bac0326a805bbefd48056c8c121bdb80000000000000000000000000000000013e02b6052719f607dacd3a088274f65596bd0d09920b61ab5da61bbdc7f5049334cf11213945d57e5ac7d055d042b7e000000000000000000000000000000000ce5d527727d6e118cc9cdc6da2e351aadfd9baa8cbdd3a76d429a695160d12c923ac9cc3baca289e193548608b82801000000000000000000000000000000000606c4a02ea734cc32acd2b02bc28b99cb3e287e85a763af267492ab572e99ab3f370d275cec1da1aaa9075ff05f79be"
        );
        let non_subgroup = hex!(
            "0000000000000000000000000000000000ffce78f4a649e224c7cfce1239c01c4333e1189a5ce291644e04c25e3ffb75478453b018dd5bccd5ace87581cc5fff0000000000000000000000000000000000a03d0d736533784843fe492a4eedfa7a0e44fe99775726e8e2a0edfa2ef4eee106d84f5f8bcfd8a397af86032ff20b0000000000000000000000000000000018fcaf35ae50aa9e61720833502a8f8bcfe044ffeaf0d77a3ed38ccf90d7d2c59d420df502701214c47d72256c451de9000000000000000000000000000000001833aee80e1ebf865ec2464f66ce2610489af66a2a936144f092de84f33c0a6d892d6958531efdfe39c79f8c9bc460a6"
        );
        let infinity = [0u8; 256];
        let mut off_curve = [0u8; 256];
        off_curve[255] = 1;

        let msm = |pairs: &[(&[u8; 256], U256)]| {
            let input = pairs
                .iter()
                .flat_map(|(point, scalar)| {
                    [point.as_slice(), scalar.to_be_bytes::<32>().as_slice()].concat()
                })
                .collect::<Vec<_>>();
            run(&input, &mut GasTracker::new(u64::MAX))
                .map(|output| output.bytes().to_vec())
                .map_err(|err| err.as_halt().cloned())
        };

        let scalars = [
            U256::ZERO,
            U256::ONE,
            U256::from(2),
            r - U256::ONE,
            r,
            r + U256::ONE,
            r * U256::from(2),
            r * U256::from(2) + U256::ONE,
            U256::ONE << 255,
            U256::MAX,
        ];
        for scalar in scalars {
            let reduced = scalar % r;
            assert_eq!(msm(&[(&point, scalar)]), msm(&[(&point, reduced)]));
            assert_eq!(
                msm(&[(&point, scalar), (&point, U256::from(3))]),
                msm(&[(&point, reduced), (&point, U256::from(3))]),
            );
            assert_eq!(msm(&[(&infinity, scalar)]), Ok(infinity.to_vec()));
            assert_eq!(
                msm(&[(&point, scalar), (&infinity, U256::MAX), (&point, r - reduced)]),
                Ok(infinity.to_vec()),
            );
            for (bad, err) in [
                (&off_curve, PrecompileHalt::Bls12381G2NotOnCurve),
                (&non_subgroup, PrecompileHalt::Bls12381G2NotInSubgroup),
            ] {
                assert_eq!(msm(&[(bad, scalar)]), Err(Some(err.clone())));
                assert_eq!(msm(&[(&point, U256::ONE), (bad, scalar)]), Err(Some(err.clone())));
                assert_eq!(msm(&[(&point, scalar), (bad, U256::ONE)]), Err(Some(err)));
            }
        }

        assert_eq!(msm(&[(&point, U256::ONE)]), Ok(point.to_vec()));
        assert_eq!(msm(&[(&point, r), (&point, r * U256::from(2))]), Ok(infinity.to_vec()));
        assert_eq!(msm(&[(&point, r - U256::ONE), (&point, U256::ONE)]), Ok(infinity.to_vec()));
        assert_eq!(msm(&[(&point, r - U256::ONE), (&point, U256::from(2))]), Ok(point.to_vec()));

        // Many pairs with scalars just below `r` (bit 254 set) sum to `-n(n+1)/2 * P`.
        let pairs = (0..64u64).map(|i| (&point, r - U256::from(1 + i))).collect::<Vec<_>>();
        assert_eq!(msm(&pairs), msm(&[(&point, r - U256::from(64 * 65 / 2))]));
    }
}
