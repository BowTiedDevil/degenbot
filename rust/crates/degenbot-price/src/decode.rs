//! Typed extraction of [`AbiValue`] return slots into Rust primitives.
//!
//! These helpers sit between [`degenbot_rpc::contract::Contract::call_typed`]
//! (which yields `Vec<AbiValue>` — the canonical ABI decode of the `eth_call`
//! return bytes) and the reader structs. Isolating them here keeps the decode
//! logic pure + independently parity-testable without a live RPC: a test feeds
//! recorded EVM return bytes through the shared
//! `degenbot_abi::decoder::decode_for_types` and then these extractors,
//! asserting value-exact results vs. the Python oracle's `abi_decode`.

use crate::error::PriceError;
use alloy::primitives::{I256, U256};
use degenbot_abi::abi_types::AbiValue;

/// Extract an unsigned integer (`uintN`) slot as a `U256`.
///
/// The bit width is retained in the `AbiValue` but irrelevant to the extract
/// — callers narrow further (e.g. `u128::try_from`) when the on-chain type is
/// narrower than 256 bits.
///
/// # Errors
///
/// [`PriceError::Decode`] if the slot is not a `Uint`.
pub(crate) fn as_uint(value: &AbiValue) -> Result<U256, PriceError> {
    match value {
        AbiValue::Uint(n, _) => Ok(*n),
        other => Err(PriceError::Decode(format!(
            "expected a uint slot, got {}",
            other.to_contract_string()
        ))),
    }
}

/// Extract a signed integer (`intN`) slot as an `I256`.
///
/// # Errors
///
/// [`PriceError::Decode`] if the slot is not an `Int`.
pub(crate) fn as_int(value: &AbiValue) -> Result<I256, PriceError> {
    match value {
        AbiValue::Int(n, _) => Ok(*n),
        other => Err(PriceError::Decode(format!(
            "expected an int slot, got {}",
            other.to_contract_string()
        ))),
    }
}

/// Narrow a `uint80` (or any ≤128-bit unsigned) slot to `u128`.
///
/// `latestRoundData`'s `roundId` / `answeredInRound` are `uint80`, which always
/// fits `u128` (2^80 − 1 < 2^128 − 1), so this never errors for well-formed
/// feeds. The `try_from` is kept rather than a panicking `.to::<u128>()` so a
/// future caller that points a `uintN`-with-N>128 return at this helper gets a
/// typed error instead of a panic.
///
/// # Errors
///
/// [`PriceError::Decode`] if the slot is not a `Uint` or its value exceeds
/// `u128::MAX`.
pub(crate) fn as_u128(value: &AbiValue) -> Result<u128, PriceError> {
    let n = as_uint(value)?;
    u128::try_from(n).map_err(|_| PriceError::Decode(format!("uint value {n} exceeds u128 range")))
}
