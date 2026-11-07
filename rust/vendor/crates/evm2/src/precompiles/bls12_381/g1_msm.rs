//! BLS12-381 G1 msm precompile. More details in [`run`]

use crate::{
    interpreter::GasTracker,
    precompiles::{
        PrecompileHalt, PrecompileOutput, PrecompileResult,
        bls12_381::{
            G1Point,
            utils::{pad_g1_point, remove_g1_padding},
        },
        bls12_381_const::{
            DISCOUNT_TABLE_G1_MSM, G1_MSM_BASE_GAS_FEE, G1_MSM_INPUT_LENGTH, SCALAR_LENGTH,
        },
        bls12_381_utils::msm_required_gas,
    },
};

/// Implements EIP-2537 G1MSM precompile.
/// G1 multi-scalar-multiplication call expects `160*k` bytes as an input that is interpreted
/// as byte concatenation of `k` slices each of them being a byte concatenation
/// of encoding of G1 point (`128` bytes) and encoding of a scalar value (`32`
/// bytes).
/// Output is an encoding of multi-scalar-multiplication operation result - single G1
/// point (`128` bytes).
/// See also: <https://eips.ethereum.org/EIPS/eip-2537#abi-for-g1-multiexponentiation>
pub fn run(input: &[u8], gas: &mut GasTracker) -> PrecompileResult {
    let input_len = input.len();
    if input_len == 0 || !input_len.is_multiple_of(G1_MSM_INPUT_LENGTH) {
        return Err(PrecompileHalt::Bls12381G1MsmInputLength.into());
    }

    let input_chunks = input.as_chunks::<G1_MSM_INPUT_LENGTH>().0;
    let k = input_chunks.len();
    let required_gas = msm_required_gas(k, &DISCOUNT_TABLE_G1_MSM, G1_MSM_BASE_GAS_FEE);
    gas.spend(required_gas)?;

    let mut valid_pairs_iter = input_chunks.iter().map(|pair| {
        let (padded_g1, scalar_array) = pair.split_last_chunk::<SCALAR_LENGTH>().unwrap();

        // Remove padding from G1 point - this validates padding format
        let [x, y] = remove_g1_padding(padded_g1)?;

        let point: G1Point = (*x, *y);
        Ok((point, *scalar_array))
    });

    let unpadded_result = crate::precompiles::crypto().bls12_381_g1_msm(&mut valid_pairs_iter)?;

    // Pad the result for EVM compatibility
    let padded_result = pad_g1_point(&unpadded_result);

    Ok(PrecompileOutput::new(padded_result.into()))
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::precompiles::kzg_point_evaluation::RETURN_VALUE;
    use alloc::vec::Vec;
    use alloy_primitives::{U256, bytes, hex};

    #[test]
    fn bls_g1multiexp_g1_not_on_curve_but_in_subgroup() {
        let input = bytes!(
            "000000000000000000000000000000000a2833e497b38ee3ca5c62828bf4887a9f940c9e426c7890a759c20f248c23a7210d2432f4c98a514e524b5184a0ddac00000000000000000000000000000000150772d56bf9509469f9ebcd6e47570429fd31b0e262b66d512e245c38ec37255529f2271fd70066473e393a8bead0c30000000000000000000000000000000000000000000000000000000000000000"
        );
        let fail = run(&input, &mut GasTracker::new(G1_MSM_BASE_GAS_FEE));
        assert_eq!(
            fail.err().and_then(|e| e.as_halt().cloned()),
            Some(PrecompileHalt::Bls12381G1NotOnCurve)
        );
    }

    /// Scalars are reduced modulo the subgroup order `r`: `s` and `s mod r` give the same result,
    /// and invalid points are rejected whatever their scalar.
    #[test]
    fn g1_msm_scalars_reduced_mod_r() {
        let r = U256::from_be_slice(&RETURN_VALUE[32..]);
        let point = hex!(
            "0000000000000000000000000000000017f1d3a73197d7942695638c4fa9ac0fc3688c4f9774b905a14e3a3f171bac586c55e83ff97a1aeffb3af00adb22c6bb0000000000000000000000000000000008b3f481e3aaa0f1a09e30ed741d8ae4fcf5e095d5d00af600db18cb2c04b3edd03cc744a2888ae40caa232946c5e7e1"
        );
        let non_subgroup = hex!(
            "0000000000000000000000000000000000a1ecb07124951270576b9b2638d20f4aedd5932106b682ddc5f84701ed844edb57bcc0499a6c02e9abbf2e12b066380000000000000000000000000000000019c9baf3c9d0f00fbb9126e9039f177ff780fc5810f96558beb0c77e32d7b07c27c712c9a71e6f6417861e6ec0ba0778"
        );
        let infinity = [0u8; 128];
        let mut off_curve = [0u8; 128];
        off_curve[127] = 1;

        let msm = |pairs: &[(&[u8; 128], U256)]| {
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
                (&off_curve, PrecompileHalt::Bls12381G1NotOnCurve),
                (&non_subgroup, PrecompileHalt::Bls12381G1NotInSubgroup),
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
