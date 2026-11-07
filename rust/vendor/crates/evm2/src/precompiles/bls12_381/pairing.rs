//! BLS12-381 pairing precompile. More details in [`run`]

use super::{
    PairingPair,
    utils::{remove_g1_padding, remove_g2_padding},
};
use crate::{
    interpreter::GasTracker,
    precompiles::{
        PrecompileHalt, PrecompileOutput, PrecompileResult,
        bls12_381_const::{
            PADDED_G1_LENGTH, PAIRING_INPUT_LENGTH, PAIRING_MULTIPLIER_BASE, PAIRING_OFFSET_BASE,
        },
    },
    utils::bool_to_bytes32,
};
use alloc::vec::Vec;

/// Pairing call expects 384*k (k being a positive integer) bytes as an inputs
/// that is interpreted as byte concatenation of k slices. Each slice has the
/// following structure:
///    * 128 bytes of G1 point encoding
///    * 256 bytes of G2 point encoding
///
/// Each point is expected to be in the subgroup of order q.
/// Output is 32 bytes where first 31 bytes are equal to 0x00 and the last byte
/// is 0x01 if pairing result is equal to the multiplicative identity in a pairing
/// target field and 0x00 otherwise.
///
/// See also: <https://eips.ethereum.org/EIPS/eip-2537#abi-for-pairing>
pub fn run(input: &[u8], gas: &mut GasTracker) -> PrecompileResult {
    let input_len = input.len();
    if input_len == 0 || !input_len.is_multiple_of(PAIRING_INPUT_LENGTH) {
        return Err(PrecompileHalt::Bls12381PairingInputLength.into());
    }

    let k = input_len / PAIRING_INPUT_LENGTH;
    let required_gas: u64 = PAIRING_MULTIPLIER_BASE * k as u64 + PAIRING_OFFSET_BASE;
    gas.spend(required_gas)?;

    // Collect pairs of points for the pairing check
    let mut pairs: Vec<PairingPair> = Vec::with_capacity(k);
    for pair in input.as_chunks::<PAIRING_INPUT_LENGTH>().0 {
        let (encoded_g1_element, encoded_g2_element) = pair.split_at(PADDED_G1_LENGTH);

        let [a_x, a_y] = remove_g1_padding(encoded_g1_element)?;
        let [b_x_0, b_x_1, b_y_0, b_y_1] = remove_g2_padding(encoded_g2_element)?;

        pairs.push(((*a_x, *a_y), (*b_x_0, *b_x_1, *b_y_0, *b_y_1)));
    }

    let result = crate::precompiles::crypto().bls12_381_pairing_check(&pairs)?;
    Ok(PrecompileOutput::new(bool_to_bytes32(result)))
}
