//! BN254 precompiles added in [`EIP-1962`](https://eips.ethereum.org/EIPS/eip-1962)

use crate::{
    interpreter::GasTracker,
    precompiles::{PrecompileHalt, PrecompileOutput, PrecompileResult},
    utils::{bool_to_bytes32, right_pad},
};
use alloc::vec::Vec;

#[cfg_attr(any(feature = "bn", feature = "bn254-mcl"), expect(dead_code))]
pub(crate) mod arkworks;

core::cfg_select! {
    feature = "bn254-mcl" => {
        pub(crate) mod mcl;
        type Ops = mcl::MclOps;
    }
    feature = "bn" => {
        pub(crate) mod substrate;
        type Ops = substrate::SubstrateOps;
    }
    _ => {
        type Ops = arkworks::ArkworksOps;
    }
}

pub(crate) trait Bn254Ops {
    type G1;
    type G2;
    type Scalar;

    #[inline]
    fn init() {}

    fn read_g1(input: &[u8]) -> Result<Self::G1, PrecompileHalt>;
    fn encode_g1(point: Self::G1) -> [u8; G1_LEN];
    fn read_g2(input: &[u8]) -> Result<Self::G2, PrecompileHalt>;
    fn read_scalar(input: &[u8]) -> Self::Scalar;
    fn g1_is_zero(p: &Self::G1) -> bool;
    fn g2_is_zero(p: &Self::G2) -> bool;
    fn g1_add(p1: Self::G1, p2: Self::G1) -> Self::G1;
    fn g1_mul(p: Self::G1, s: Self::Scalar) -> Self::G1;
    fn pairing_check(g1: &[Self::G1], g2: &[Self::G2]) -> bool;
}

/// Performs point addition on two G1 points using the selected backend.
#[inline]
pub(crate) fn g1_point_add(p1_bytes: &[u8], p2_bytes: &[u8]) -> Result<[u8; 64], PrecompileHalt> {
    Ops::init();
    let p1 = Ops::read_g1(p1_bytes)?;
    let p2 = Ops::read_g1(p2_bytes)?;
    Ok(Ops::encode_g1(Ops::g1_add(p1, p2)))
}

/// Performs a G1 scalar multiplication using the selected backend.
#[inline]
pub(crate) fn g1_point_mul(
    point_bytes: &[u8],
    fr_bytes: &[u8],
) -> Result<[u8; 64], PrecompileHalt> {
    Ops::init();
    let p = Ops::read_g1(point_bytes)?;
    let fr = Ops::read_scalar(fr_bytes);
    Ok(Ops::encode_g1(Ops::g1_mul(p, fr)))
}

/// Performs a pairing check on a list of G1 and G2 point pairs using the selected backend.
#[inline]
pub(crate) fn pairing_check(pairs: &[(&[u8], &[u8])]) -> Result<bool, PrecompileHalt> {
    Ops::init();
    let mut g1_points = Vec::with_capacity(pairs.len());
    let mut g2_points = Vec::with_capacity(pairs.len());

    for (g1_bytes, g2_bytes) in pairs {
        let g1 = Ops::read_g1(g1_bytes)?;
        let g2 = Ops::read_g2(g2_bytes)?;

        // Skip pairs where either point is at infinity
        if !Ops::g1_is_zero(&g1) && !Ops::g2_is_zero(&g2) {
            g1_points.push(g1);
            g2_points.push(g2);
        }
    }

    if g1_points.is_empty() {
        return Ok(true);
    }

    Ok(Ops::pairing_check(&g1_points, &g2_points))
}

/// BN254 point addition precompile entrypoints.
pub mod add {
    use super::*;

    pub(crate) const ISTANBUL_ADD_GAS_COST: u64 = 150;
    pub(crate) const BYZANTIUM_ADD_GAS_COST: u64 = 500;

    /// Runs the Istanbul BN254 point addition precompile.
    pub fn run_istanbul(input: &[u8], gas: &mut GasTracker) -> PrecompileResult {
        run(input, ISTANBUL_ADD_GAS_COST, gas)
    }

    /// Runs the Byzantium BN254 point addition precompile.
    pub fn run_byzantium(input: &[u8], gas: &mut GasTracker) -> PrecompileResult {
        run(input, BYZANTIUM_ADD_GAS_COST, gas)
    }

    pub(crate) fn run(input: &[u8], gas_cost: u64, gas: &mut GasTracker) -> PrecompileResult {
        super::run_add(input, gas_cost, gas)
    }
}

/// BN254 scalar multiplication precompile entrypoints.
pub mod mul {
    use super::*;

    pub(crate) const ISTANBUL_MUL_GAS_COST: u64 = 6_000;
    pub(crate) const BYZANTIUM_MUL_GAS_COST: u64 = 40_000;

    /// Runs the Istanbul BN254 scalar multiplication precompile.
    pub fn run_istanbul(input: &[u8], gas: &mut GasTracker) -> PrecompileResult {
        run(input, ISTANBUL_MUL_GAS_COST, gas)
    }

    /// Runs the Byzantium BN254 scalar multiplication precompile.
    pub fn run_byzantium(input: &[u8], gas: &mut GasTracker) -> PrecompileResult {
        run(input, BYZANTIUM_MUL_GAS_COST, gas)
    }

    pub(crate) fn run(input: &[u8], gas_cost: u64, gas: &mut GasTracker) -> PrecompileResult {
        super::run_mul(input, gas_cost, gas)
    }
}

/// BN254 pairing precompile entrypoints.
pub mod pair {
    use super::*;

    pub(crate) const ISTANBUL_PAIR_PER_POINT: u64 = 34_000;
    pub(crate) const ISTANBUL_PAIR_BASE: u64 = 45_000;
    pub(crate) const BYZANTIUM_PAIR_PER_POINT: u64 = 80_000;
    pub(crate) const BYZANTIUM_PAIR_BASE: u64 = 100_000;

    /// Runs the Istanbul BN254 pairing precompile.
    pub fn run_istanbul(input: &[u8], gas: &mut GasTracker) -> PrecompileResult {
        run(input, ISTANBUL_PAIR_PER_POINT, ISTANBUL_PAIR_BASE, gas)
    }

    /// Runs the Byzantium BN254 pairing precompile.
    pub fn run_byzantium(input: &[u8], gas: &mut GasTracker) -> PrecompileResult {
        run(input, BYZANTIUM_PAIR_PER_POINT, BYZANTIUM_PAIR_BASE, gas)
    }

    pub(crate) fn run(
        input: &[u8],
        pair_per_point_cost: u64,
        pair_base_cost: u64,
        gas: &mut GasTracker,
    ) -> PrecompileResult {
        super::run_pair(input, pair_per_point_cost, pair_base_cost, gas)
    }
}

/// FQ_LEN specifies the number of bytes needed to represent an
/// Fq element. This is an element in the base field of BN254.
///
/// Note: The base field is used to define G1 and G2 elements.
const FQ_LEN: usize = 32;

/// SCALAR_LEN specifies the number of bytes needed to represent an Fr element.
/// This is an element in the scalar field of BN254.
const SCALAR_LEN: usize = 32;

/// FQ2_LEN specifies the number of bytes needed to represent an
/// Fq^2 element.
///
/// Note: This is the quadratic extension of Fq, and by definition
/// means we need 2 Fq elements.
const FQ2_LEN: usize = 2 * FQ_LEN;

/// G1_LEN specifies the number of bytes needed to represent a G1 element.
///
/// Note: A G1 element contains 2 Fq elements.
const G1_LEN: usize = 2 * FQ_LEN;
/// G2_LEN specifies the number of bytes needed to represent a G2 element.
///
/// Note: A G2 element contains 2 Fq^2 elements.
const G2_LEN: usize = 2 * FQ2_LEN;

/// Input length for the add operation.
/// `ADD` takes two uncompressed G1 points (64 bytes each).
pub(crate) const ADD_INPUT_LEN: usize = 2 * G1_LEN;

/// Input length for the multiplication operation.
/// `MUL` takes an uncompressed G1 point (64 bytes) and scalar (32 bytes).
pub(crate) const MUL_INPUT_LEN: usize = G1_LEN + SCALAR_LEN;

/// Pair element length.
/// `PAIR` elements are composed of an uncompressed G1 point (64 bytes) and an uncompressed G2 point
/// (128 bytes).
pub(crate) const PAIR_ELEMENT_LEN: usize = G1_LEN + G2_LEN;

/// Run the Bn254 add precompile
pub fn run_add(input: &[u8], gas_cost: u64, gas: &mut GasTracker) -> PrecompileResult {
    gas.spend(gas_cost)?;

    let input = right_pad::<ADD_INPUT_LEN>(input);

    let p1_bytes = &input[..G1_LEN];
    let p2_bytes = &input[G1_LEN..];
    let output = crate::precompiles::crypto().bn254_g1_add(p1_bytes, p2_bytes)?;

    Ok(PrecompileOutput::new(output.into()))
}

/// Run the Bn254 mul precompile
pub fn run_mul(input: &[u8], gas_cost: u64, gas: &mut GasTracker) -> PrecompileResult {
    gas.spend(gas_cost)?;

    let input = right_pad::<MUL_INPUT_LEN>(input);

    let point_bytes = &input[..G1_LEN];
    let scalar_bytes = &input[G1_LEN..G1_LEN + SCALAR_LEN];
    let output = crate::precompiles::crypto().bn254_g1_mul(point_bytes, scalar_bytes)?;

    Ok(PrecompileOutput::new(output.into()))
}

/// Run the Bn254 pair precompile
pub fn run_pair(
    input: &[u8],
    pair_per_point_cost: u64,
    pair_base_cost: u64,
    gas: &mut GasTracker,
) -> PrecompileResult {
    let gas_used = (input.len() / PAIR_ELEMENT_LEN) as u64 * pair_per_point_cost + pair_base_cost;
    gas.spend(gas_used)?;

    if !input.len().is_multiple_of(PAIR_ELEMENT_LEN) {
        return Err(PrecompileHalt::Bn254PairLength.into());
    }

    let mut points = Vec::with_capacity(input.len() / PAIR_ELEMENT_LEN);

    for pair in input.as_chunks::<PAIR_ELEMENT_LEN>().0 {
        let (encoded_g1_element, encoded_g2_element) = pair.split_at(G1_LEN);
        points.push((encoded_g1_element, encoded_g2_element));
    }

    let pairing_result = crate::precompiles::crypto().bn254_pairing_check(&points)?;
    Ok(PrecompileOutput::new(bool_to_bytes32(pairing_result)))
}

#[cfg(test)]
mod tests {
    use crate::precompiles::{
        PrecompileError, PrecompileHalt,
        bn254::{
            add::BYZANTIUM_ADD_GAS_COST,
            mul::BYZANTIUM_MUL_GAS_COST,
            pair::{BYZANTIUM_PAIR_BASE, BYZANTIUM_PAIR_PER_POINT},
        },
    };
    use alloy_primitives::{B256, hex};
    use core::assert_matches;

    use super::*;

    #[test]
    fn test_bn254_add() {
        let input = hex!(
            "\
             18b18acfb4c2c30276db5411368e7185b311dd124691610c5d3b74034e093dc9\
             063c909c4720840cb5134cb9f59fa749755796819658d32efc0d288198f37266\
             07c2b7f58a84bd6145f00c9c2bc0bb1a187f20ff2c92963a88019e7c6a014eed\
             06614e20c147e940f2d70da3f74c9a17df361706a4485c742bd6788478fa17d7"
        );
        let expected = hex!(
            "\
            2243525c5efd4b9c3d3c45ac0ca3fe4dd85e830a4ce6b65fa1eeaee202839703\
            301d1d33be6da8e509df21cc35964723180eed7532537db9ae5e7d48f195c915"
        );

        let outcome = add::run(&input, BYZANTIUM_ADD_GAS_COST, &mut GasTracker::new(500)).unwrap();
        assert_eq!(outcome.bytes(), expected);

        // Zero sum test
        let input = hex!(
            "\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000000"
        );
        let expected = hex!(
            "\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000000"
        );

        let outcome = add::run(&input, BYZANTIUM_ADD_GAS_COST, &mut GasTracker::new(500)).unwrap();
        assert_eq!(outcome.bytes(), expected);

        // Out of gas test
        let input = hex!(
            "\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000000"
        );

        let res = add::run(&input, BYZANTIUM_ADD_GAS_COST, &mut GasTracker::new(499));

        assert_matches!(res, Err(PrecompileError::Halt(PrecompileHalt::OutOfGas)));

        // No input test
        let input = [0u8; 0];
        let expected = hex!(
            "\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000000"
        );

        let outcome = add::run(&input, BYZANTIUM_ADD_GAS_COST, &mut GasTracker::new(500)).unwrap();
        assert_eq!(outcome.bytes(), expected);

        // Point not on curve fail
        let input = hex!(
            "\
            1111111111111111111111111111111111111111111111111111111111111111\
            1111111111111111111111111111111111111111111111111111111111111111\
            1111111111111111111111111111111111111111111111111111111111111111\
            1111111111111111111111111111111111111111111111111111111111111111"
        );

        let res = add::run(&input, BYZANTIUM_ADD_GAS_COST, &mut GasTracker::new(500));
        assert_matches!(
            res,
            Err(PrecompileError::Halt(PrecompileHalt::Bn254AffineGFailedToCreate)),
        );

        // Short input is right-padded. This makes the first field element non-canonical.
        let res = add::run(&[0x40], BYZANTIUM_ADD_GAS_COST, &mut GasTracker::new(500));
        assert_matches!(res, Err(PrecompileError::Halt(PrecompileHalt::Bn254FieldPointNotAMember)),);

        // A coordinate equal to the field modulus is non-canonical.
        let input = hex!("30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47");
        let res = add::run(&input, BYZANTIUM_ADD_GAS_COST, &mut GasTracker::new(500));
        assert_matches!(res, Err(PrecompileError::Halt(PrecompileHalt::Bn254FieldPointNotAMember)),);

        // The largest canonical coordinate, p - 1, is accepted: (p - 1, sqrt(2)) is on the curve.
        let mut input = [0u8; 128];
        input[..32].copy_from_slice(&hex!(
            "30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd46"
        ));
        input[32..64].copy_from_slice(&hex!(
            "279d7bc4e184e3a57f5fa684690c6df6b484a7f1daa1de608d266a2a4be6593f"
        ));
        let outcome = add::run(&input, BYZANTIUM_ADD_GAS_COST, &mut GasTracker::new(500)).unwrap();
        assert_eq!(outcome.bytes(), &input[..64]);

        let input = hex!(
            "\
            0000000000000000000000000000000000000000000000000000000000000001\
            30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd45\
            0000000000000000000000000000000000000000000000000000000000000001\
            30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd45"
        );
        let expected = hex!(
            "\
            030644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd3\
            1a76dae6d3272396d0cbe61fced2bc532edac647851e3ac53ce1cc9c7e645a83"
        );

        let outcome = add::run(&input, BYZANTIUM_ADD_GAS_COST, &mut GasTracker::new(500)).unwrap();
        assert_eq!(outcome.bytes(), expected);
    }

    #[test]
    fn test_bn254_mul() {
        let input = hex!(
            "\
            2bd3e6d0f3b142924f5ca7b49ce5b9d54c4703d7ae5648e61d02268b1a0a9fb7\
            21611ce0a6af85915e2f1d70300909ce2e49dfad4a4619c8390cae66cefdb204\
            00000000000000000000000000000000000000000000000011138ce750fa15c2"
        );
        let expected = hex!(
            "\
            070a8d6a982153cae4be29d434e8faef8a47b274a053f5a4ee2a6c9c13c31e5c\
            031b8ce914eba3a9ffb989f9cdd5b0f01943074bf4f0f315690ec3cec6981afc"
        );

        let outcome =
            mul::run(&input, BYZANTIUM_MUL_GAS_COST, &mut GasTracker::new(40_000)).unwrap();
        assert_eq!(outcome.bytes(), expected);

        // Out of gas test
        let input = hex!(
            "\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000000\
            0200000000000000000000000000000000000000000000000000000000000000"
        );

        let res = mul::run(&input, BYZANTIUM_MUL_GAS_COST, &mut GasTracker::new(39_999));
        assert_matches!(res, Err(PrecompileError::Halt(PrecompileHalt::OutOfGas)));

        // Zero multiplication test
        let input = hex!(
            "\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000000\
            0200000000000000000000000000000000000000000000000000000000000000"
        );
        let expected = hex!(
            "\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000000"
        );

        let outcome =
            mul::run(&input, BYZANTIUM_MUL_GAS_COST, &mut GasTracker::new(40_000)).unwrap();
        assert_eq!(outcome.bytes(), expected);

        // No input test
        let input = [0u8; 0];
        let expected = hex!(
            "\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000000"
        );

        let outcome =
            mul::run(&input, BYZANTIUM_MUL_GAS_COST, &mut GasTracker::new(40_000)).unwrap();
        assert_eq!(outcome.bytes(), expected);

        // Point not on curve fail
        let input = hex!(
            "\
            1111111111111111111111111111111111111111111111111111111111111111\
            1111111111111111111111111111111111111111111111111111111111111111\
            0f00000000000000000000000000000000000000000000000000000000000000"
        );

        let res = mul::run(&input, BYZANTIUM_MUL_GAS_COST, &mut GasTracker::new(40_000));
        assert_matches!(
            res,
            Err(PrecompileError::Halt(PrecompileHalt::Bn254AffineGFailedToCreate)),
        );

        // Short input is right-padded. This makes the point x-coordinate non-canonical.
        let res = mul::run(&[0x40], BYZANTIUM_MUL_GAS_COST, &mut GasTracker::new(40_000));
        assert_matches!(res, Err(PrecompileError::Halt(PrecompileHalt::Bn254FieldPointNotAMember)),);

        let input = hex!(
            "\
            0000000000000000000000000000000000000000000000000000000000000001\
            30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd45\
            0000000000000000000000000000000000000000000000000000000000000002"
        );
        let expected = hex!(
            "\
            030644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd3\
            1a76dae6d3272396d0cbe61fced2bc532edac647851e3ac53ce1cc9c7e645a83"
        );

        let outcome =
            mul::run(&input, BYZANTIUM_MUL_GAS_COST, &mut GasTracker::new(40_000)).unwrap();
        assert_eq!(outcome.bytes(), expected);
    }

    #[test]
    fn test_bn254_pair() {
        let input = hex!(
            "\
            1c76476f4def4bb94541d57ebba1193381ffa7aa76ada664dd31c16024c43f59\
            3034dd2920f673e204fee2811c678745fc819b55d3e9d294e45c9b03a76aef41\
            209dd15ebff5d46c4bd888e51a93cf99a7329636c63514396b4a452003a35bf7\
            04bf11ca01483bfa8b34b43561848d28905960114c8ac04049af4b6315a41678\
            2bb8324af6cfc93537a2ad1a445cfd0ca2a71acd7ac41fadbf933c2a51be344d\
            120a2a4cf30c1bf9845f20c6fe39e07ea2cce61f0c9bb048165fe5e4de877550\
            111e129f1cf1097710d41c4ac70fcdfa5ba2023c6ff1cbeac322de49d1b6df7c\
            2032c61a830e3c17286de9462bf242fca2883585b93870a73853face6a6bf411\
            198e9393920d483a7260bfb731fb5d25f1aa493335a9e71297e485b7aef312c2\
            1800deef121f1e76426a00665e5c4479674322d4f75edadd46debd5cd992f6ed\
            090689d0585ff075ec9e99ad690c3395bc4b313370b38ef355acdadcd122975b\
            12c85ea5db8c6deb4aab71808dcb408fe3d1e7690c43d37b4ce6cc0166fa7daa"
        );
        let expected = hex!("0000000000000000000000000000000000000000000000000000000000000001");

        let outcome = pair::run(
            &input,
            BYZANTIUM_PAIR_PER_POINT,
            BYZANTIUM_PAIR_BASE,
            &mut GasTracker::new(260_000),
        )
        .unwrap();
        assert_eq!(outcome.bytes(), expected);

        // Out of gas test
        let input = hex!(
            "\
            1c76476f4def4bb94541d57ebba1193381ffa7aa76ada664dd31c16024c43f59\
            3034dd2920f673e204fee2811c678745fc819b55d3e9d294e45c9b03a76aef41\
            209dd15ebff5d46c4bd888e51a93cf99a7329636c63514396b4a452003a35bf7\
            04bf11ca01483bfa8b34b43561848d28905960114c8ac04049af4b6315a41678\
            2bb8324af6cfc93537a2ad1a445cfd0ca2a71acd7ac41fadbf933c2a51be344d\
            120a2a4cf30c1bf9845f20c6fe39e07ea2cce61f0c9bb048165fe5e4de877550\
            111e129f1cf1097710d41c4ac70fcdfa5ba2023c6ff1cbeac322de49d1b6df7c\
            2032c61a830e3c17286de9462bf242fca2883585b93870a73853face6a6bf411\
            198e9393920d483a7260bfb731fb5d25f1aa493335a9e71297e485b7aef312c2\
            1800deef121f1e76426a00665e5c4479674322d4f75edadd46debd5cd992f6ed\
            090689d0585ff075ec9e99ad690c3395bc4b313370b38ef355acdadcd122975b\
            12c85ea5db8c6deb4aab71808dcb408fe3d1e7690c43d37b4ce6cc0166fa7daa"
        );

        let res = pair::run(
            &input,
            BYZANTIUM_PAIR_PER_POINT,
            BYZANTIUM_PAIR_BASE,
            &mut GasTracker::new(259_999),
        );
        assert_matches!(res, Err(PrecompileError::Halt(PrecompileHalt::OutOfGas)));

        // No input test
        let input = [0u8; 0];
        let expected = hex!("0000000000000000000000000000000000000000000000000000000000000001");

        let outcome = pair::run(
            &input,
            BYZANTIUM_PAIR_PER_POINT,
            BYZANTIUM_PAIR_BASE,
            &mut GasTracker::new(260_000),
        )
        .unwrap();
        assert_eq!(outcome.bytes(), expected);

        // Point not on curve fail
        let input = hex!(
            "\
            1111111111111111111111111111111111111111111111111111111111111111\
            1111111111111111111111111111111111111111111111111111111111111111\
            1111111111111111111111111111111111111111111111111111111111111111\
            1111111111111111111111111111111111111111111111111111111111111111\
            1111111111111111111111111111111111111111111111111111111111111111\
            1111111111111111111111111111111111111111111111111111111111111111"
        );

        let res = pair::run(
            &input,
            BYZANTIUM_PAIR_PER_POINT,
            BYZANTIUM_PAIR_BASE,
            &mut GasTracker::new(260_000),
        );
        assert_matches!(
            res,
            Err(PrecompileError::Halt(PrecompileHalt::Bn254AffineGFailedToCreate)),
        );

        // G2 point on the curve but not in the subgroup fail
        let input = hex!(
            "\
            0000000000000000000000000000000000000000000000000000000000000001\
            0000000000000000000000000000000000000000000000000000000000000002\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000001\
            0d1271953ed9ea0836846e70a1934187998c7f790cb4d7511b7f8da82de048a4\
            2869111d5381f072f8e2728fdb825a51aadd70e52c9830e9ab4b871c0531f1bb"
        );

        let res = pair::run(
            &input,
            BYZANTIUM_PAIR_PER_POINT,
            BYZANTIUM_PAIR_BASE,
            &mut GasTracker::new(260_000),
        );
        assert_matches!(
            res,
            Err(PrecompileError::Halt(PrecompileHalt::Bn254AffineGFailedToCreate)),
        );

        // G2 points on the twist whose order involves the cofactor 2p - r: a subgroup point
        // (accepted), prime-order torsion points, and the subgroup point plus that torsion.
        const SUBGROUP_G2: &str = "09cc2377cd92f79de395306be74f05cfaeaed3991fa542fce5883ba8044c14e72c60c657a11d6593a04248c80112e79c348a4d18af6c5087ecc3c5e28149c3ed05ae74f920dc46a6170e7d3c52e6327fc85c17bdf5cada2b9e41542f0311fd1024667477acf930cad522366478365c806828422b23d0bb5ff3da77e364406fd2";
        const TORSION_G2: [&str; 8] = [
            "213ff81b7dc63d686334374756bcc72559a94293a74ae4e6d81ed31d271ef261070f17cc5537848d6ab2d204a9a904d495005b666bb1c99b05058a066373c6952d1e1ce0cfe183e0b657ad0714c20e98772dae710c210d3c720b0c4c6513ab352c32b900d54734063b8fe7d88c403468fcf1d6a2d3883e036650d7e4b48781da",
            "02910f1cd7433ab639c66ed45bb92d38ea934b571bd12db6cabf0d24cc756aac29dc6a249374c383c9b6297416f88ab0bfb6ff933014a266e2f2940ebd8d27bd0578f8545688a055a19ee79acd1808d61e78252bdd98281e4c2b4db0807e2945148c5480fedb5531e57ae91c8846e26b5244f60728163581f62a6410dee6cde1",
            "2a7a5ff037b463795571e05f69a996c0289147a534a32788dd606fc67fd3375f06f849b3a65040105698841a26321ccd36e6593d409f0e64973bd3a70971b1b618f64f0544e874ff6e0d267a1a4042515ce0a2786d62bdb1e964096ec83c836213c4f2816afe7fbc308bbf8e52c8a72e6abcfea133b361083021d5287deb1d92",
            "2e30cf9eb02b7d1751634c042963db701da8e33d6856ad47deb658f9af6701752fdc91fbeca171de450f569ff5c63184a889be190928aeeb017c8bd7a83024c723c462861d16f4eada580e8678c26bab1cc3229639a51624c9e774d586ac774908bb4342b5764877165895d2698b504fcb7a359c209b5abfb6135a35ed2333f1",
            "047062e2a3bca5ee01cd14dc29049f4c97f0fac6e4124e3e26f12a0a4a272c701b7e74bfd226cbaa83a37e69e600698dfa9adcc5c359fcfa0943ad5ca68f83a5007c24e9e200f9fca90f4d0c4d86c32b4df7f06e4765fb308ed40ea8a133c07d18d8037fb8ccf781153cb8ca7287fb3be9015a6ed2ea0feba2c59ad40a35b378",
            "2f018f69e3deec50d3f81dc447122b80fe4c23f68565b3cf78adfd031a03d43920c5cdc4e3480fd143933c32e0cd0effb18a0a67ed5eee3c0012ce82605904c01c9562b082c4f53bb8aef1f88230f2d12d344e792eb1ee8661f919d8c8aa1c2523bdf52a82b6078198a6a167c9d67b06c3658ce59b3dc0b01e13e0125ac7a4c8",
            "0114798e8db529833ba6664c067049e1afcd7e5e09ff1a80def786e1dbc2935111d88447dac20bb481482e5065d0a5c49f249c60feb0bd07821dbe99488ac28d15e26933e2a0f843a3ad26c982ed4b122115277b24b14c47e85629c71572c11f22fb3776eda9a92561fac2966b87e85cd736e2cae95ea010ace84ce57cd5ff65",
            "210d66db18237e0ed086b30bac4680d53aef0c7b498d13394e9e2634ff416e001b5a0355ed264f23681e9331e8fb8a30db3680b21f8004c8cee74d8af678830505e35b4630eeaafa6571a02ff3355debdeea12a2acc0528c4039fd1acdf0024d14d28ee448cc68a1dcd77e367cbebce4cfb18bec0849c489497efe4ce95ec69f",
        ];
        let pair_infinity_with = |g2: &str| {
            let mut input = [0u8; PAIR_ELEMENT_LEN];
            hex::decode_to_slice(g2, &mut input[G1_LEN..]).unwrap();
            pair::run(
                &input,
                BYZANTIUM_PAIR_PER_POINT,
                BYZANTIUM_PAIR_BASE,
                &mut GasTracker::new(260_000),
            )
        };
        let one: [u8; 32] = B256::with_last_byte(1).into();
        assert_eq!(pair_infinity_with(SUBGROUP_G2).unwrap().bytes(), &one);
        for g2 in TORSION_G2 {
            assert_matches!(
                pair_infinity_with(g2),
                Err(PrecompileError::Halt(PrecompileHalt::Bn254AffineGFailedToCreate)),
            );
        }

        let mut input = [0u8; PAIR_ELEMENT_LEN];
        input[0] = 0x40;
        let res = pair::run(
            &input,
            BYZANTIUM_PAIR_PER_POINT,
            BYZANTIUM_PAIR_BASE,
            &mut GasTracker::new(180_000),
        );
        assert_matches!(res, Err(PrecompileError::Halt(PrecompileHalt::Bn254FieldPointNotAMember)));

        // Invalid input length
        let input = hex!(
            "\
            1111111111111111111111111111111111111111111111111111111111111111\
            1111111111111111111111111111111111111111111111111111111111111111\
            111111111111111111111111111111\
        "
        );

        let res = pair::run(
            &input,
            BYZANTIUM_PAIR_PER_POINT,
            BYZANTIUM_PAIR_BASE,
            &mut GasTracker::new(260_000),
        );
        assert_matches!(res, Err(PrecompileError::Halt(PrecompileHalt::Bn254PairLength)));

        // Test with point at infinity - should return true (identity element)
        // G1 point at infinity (0,0) followed by a valid G2 point
        let input = hex!(
            "\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000000\
            209dd15ebff5d46c4bd888e51a93cf99a7329636c63514396b4a452003a35bf7\
            04bf11ca01483bfa8b34b43561848d28905960114c8ac04049af4b6315a41678\
            2bb8324af6cfc93537a2ad1a445cfd0ca2a71acd7ac41fadbf933c2a51be344d\
            120a2a4cf30c1bf9845f20c6fe39e07ea2cce61f0c9bb048165fe5e4de877550"
        );
        let expected = hex!("0000000000000000000000000000000000000000000000000000000000000001");

        let outcome = pair::run(
            &input,
            BYZANTIUM_PAIR_PER_POINT,
            BYZANTIUM_PAIR_BASE,
            &mut GasTracker::new(260_000),
        )
        .unwrap();
        assert_eq!(outcome.bytes(), expected);

        // Test with G2 point at infinity - should also return true
        // Valid G1 point followed by G2 point at infinity (0,0,0,0)
        let input = hex!(
            "\
            1c76476f4def4bb94541d57ebba1193381ffa7aa76ada664dd31c16024c43f59\
            3034dd2920f673e204fee2811c678745fc819b55d3e9d294e45c9b03a76aef41\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000000\
            0000000000000000000000000000000000000000000000000000000000000000"
        );

        let outcome = pair::run(
            &input,
            BYZANTIUM_PAIR_PER_POINT,
            BYZANTIUM_PAIR_BASE,
            &mut GasTracker::new(260_000),
        )
        .unwrap();
        assert_eq!(outcome.bytes(), expected);
    }
}
