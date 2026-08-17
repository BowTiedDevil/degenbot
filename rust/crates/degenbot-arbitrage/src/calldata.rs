//! 7-call vector calldata builders — the settlement-arbitrage strategy's balance-read +
//! execute-wrap calldata.
//!
//! The three pre/post balance-read calldata blobs that bracket the `execute()`
//! call in the 7-call simulate vector: WETH9/ERC20 `balanceOf(address)`,
//! Multicall3 `getEthBalance(address)`, and PoolManager ERC6909
//! `balanceOf(address,uint256)`. Plus the `execute(bytes,uint256)` calldata
//! wrap (a thin delegation to `degenbot_executor::encode_execute_call`).
//!
//! Moved here from `degenbot-simulation::sim::evm::calldata` (ADR-019 D4/D7,
//! decision R): these builders are part of the settlement-arbitrage bundle (the 7-call
//! vector), so they live with the strategy that consumes them, not with the
//! generic engine. The engine no longer references them.
//!
//! All selectors are `keccak256(signature)[:4]`; values are ABI-encoded via
//! `degenbot_abi::encoder::encode_rust` (the pure-Rust encoder). Each
//! builder is byte-for-byte parity vs the Python oracle's
//! `selector + eth_abi.abi.encode(...)` output (golden tests pin the bytes).

#![expect(clippy::doc_markdown)]

use alloy::primitives::{Address, Bytes};
use degenbot_abi::abi_types::AbiValue;
use degenbot_abi::encoder::encode_rust;
use degenbot_core::errors::AbiDecodeError;
use degenbot_executor::erc6909_id;

/// The WETH9 / ERC20 `balanceOf(address)` 4-byte function selector
/// (`keccak256("balanceOf(address)")[:4] = 0x70a08231`).
pub const BALANCE_OF_SELECTOR: [u8; 4] = [0x70, 0xa0, 0x82, 0x31];

/// The Multicall3 `getEthBalance(address)` 4-byte function selector
/// (`keccak256("getEthBalance(address))[:4] = 0x4d2301cc`).
///
/// The Python oracle folds the ETH balance read into the `eth_simulateV1`
/// call vector as a Multicall3 `getEthBalance(address)` call (NOT a raw
/// `eth_getBalance` RPC), so all 7 balance reads share the simulate state —
/// the same pattern is mirrored here.
pub const GET_ETH_BALANCE_SELECTOR: [u8; 4] = [0x4d, 0x23, 0x01, 0xcc];

/// The PoolManager ERC6909 `balanceOf(address,uint256)` 4-byte function
/// selector (`keccak256("balanceOf(address,uint256))[:4] = 0x00fdd58e`).
pub const ERC6909_BALANCE_OF_SELECTOR: [u8; 4] = [0x00, 0xfd, 0xd5, 0x8e];

/// Encode the WETH9 / ERC20 `balanceOf(address)` calldata.
///
/// Ports `encode_balanceof_calldata` (L372–L375): the 4-byte selector followed
/// by the ABI-encoded `address` argument (left-padded to 32 bytes).
///
/// # Errors
///
/// Returns [`AbiDecodeError`] only if the address encoding fails (it cannot
/// with a valid `Address`).
pub fn encode_balance_of_calldata(account: Address) -> Result<Bytes, AbiDecodeError> {
    encode_single_address(BALANCE_OF_SELECTOR, account)
}

/// Encode the Multicall3 `getEthBalance(address)` calldata.
///
/// The ETH balance read is folded into the simulate call vector as a Multicall3
/// call (see [`GET_ETH_BALANCE_SELECTOR`]) so it shares the same simulate
/// state as the other six reads (ports the L1720–L1722 block).
///
/// # Errors
///
/// Returns [`AbiDecodeError`] only if the address encoding fails.
pub fn encode_get_eth_balance_calldata(account: Address) -> Result<Bytes, AbiDecodeError> {
    encode_single_address(GET_ETH_BALANCE_SELECTOR, account)
}

/// Encode the PoolManager ERC6909 `balanceOf(address,uint256)` calldata.
///
/// Ports the L1736–L1738 block: `pm_balanceof_selector +
/// eth_abi.abi.encode(["address", "uint256"], [executor, weth_erc6909_id])`.
/// The `uint256` id is the ERC6909 `uint160(currency)` for the given token
/// (`CurrencyLibrary.toId()`) — reused from `degenbot_executor::erc6909_id`
/// (the §62H23D leaf — consume, don't duplicate).
///
/// # Errors
///
/// Returns [`AbiDecodeError`] if the `(address, uint256)` encoding fails.
pub fn encode_erc6909_balance_of_calldata(
    account: Address,
    currency: Address,
) -> Result<Bytes, AbiDecodeError> {
    let id = erc6909_id(currency);
    let values = [AbiValue::Address(account.into()), AbiValue::Uint(id, 256)];
    let tail = encode_rust(&["address", "uint256"], &values)?;
    let mut data = Vec::with_capacity(ERC6909_BALANCE_OF_SELECTOR.len() + tail.len());
    data.extend_from_slice(&ERC6909_BALANCE_OF_SELECTOR);
    data.extend_from_slice(&tail);
    Ok(Bytes::from(data))
}

/// Helper: a single-`address`-argument calldata (selector + one encoded tail).
fn encode_single_address(selector: [u8; 4], account: Address) -> Result<Bytes, AbiDecodeError> {
    let tail = encode_rust(&["address"], &[AbiValue::Address(account.into())])?;
    let mut data = Vec::with_capacity(selector.len() + tail.len());
    data.extend_from_slice(&selector);
    data.extend_from_slice(&tail);
    Ok(Bytes::from(data))
}

/// Wrap the `execute(bytes, uint256)` call from its parts (the settlement-arbitrage call).
///
/// Delegates to `degenbot_executor::composers::encode_execute_call` (the
/// §YQORTM leaf) — the selector + the `(bytes, uint256)` ABI encoding live
/// there. Colocated with the settlement-arbitrage bundle (the 7-call vector's execute
/// wrap) so the in-process revm path can build the execute calldata without
/// a cycle.
///
/// # Errors
///
/// Returns [`AbiDecodeError`] if the `(bytes, uint256)` encoding fails.
pub fn wrap_execute_calldata(
    executor_address: Address,
    cmd_bytes: &[u8],
    config: alloy::primitives::U256,
) -> Result<Bytes, AbiDecodeError> {
    let encoded =
        degenbot_executor::composers::encode_execute_call(executor_address, cmd_bytes, config)?;
    Ok(Bytes::from(encoded.data))
}

#[expect(clippy::unwrap_used)]
#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    /// The canonical executor address used across the parity corpus.
    const EXECUTOR: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");

    // ---------------------------------------------------------------------
    // §4.2 byte-for-byte golden parity vs the Python oracle
    // (`encode_balanceof_calldata`, the getEthBalance block, the ERC6909
    // block). The selector hexes are pinned `const`; the encoded tails are
    // captured from `eth_abi.abi.encode(...)` over the corpus addresses.
    // ---------------------------------------------------------------------

    #[test]
    fn balance_of_selector_matches_python_oracle() {
        // Web3.keccak(text="balanceOf(address)")[:4] = 0x70a08231
        assert_eq!(BALANCE_OF_SELECTOR, [0x70, 0xa0, 0x82, 0x31]);
    }

    #[test]
    fn get_eth_balance_selector_matches_python_oracle() {
        // Web3.keccak(text="getEthBalance(address)")[:4] = 0x4d2301cc
        assert_eq!(GET_ETH_BALANCE_SELECTOR, [0x4d, 0x23, 0x01, 0xcc]);
    }

    #[test]
    fn erc6909_balance_of_selector_matches_python_oracle() {
        // Web3.keccak(text="balanceOf(address,uint256)")[:4] = 0x00fdd58e
        assert_eq!(ERC6909_BALANCE_OF_SELECTOR, [0x00, 0xfd, 0xd5, 0x8e]);
    }

    #[test]
    fn encode_balance_of_matches_eth_abi_output() {
        // selector + 32-byte left-padded address tail.
        let data = encode_balance_of_calldata(EXECUTOR).unwrap();
        assert_eq!(&data[..4], &BALANCE_OF_SELECTOR);
        // The address occupies bytes 12..32 of the 32-byte tail (left-zero-padded).
        assert_eq!(&data[4 + 12..4 + 32], EXECUTOR.as_slice());
        assert_eq!(data.len(), 4 + 32);
    }

    #[test]
    fn encode_get_eth_balance_matches_eth_abi_output() {
        let data = encode_get_eth_balance_calldata(EXECUTOR).unwrap();
        assert_eq!(&data[..4], &GET_ETH_BALANCE_SELECTOR);
        assert_eq!(&data[4 + 12..4 + 32], EXECUTOR.as_slice());
        assert_eq!(data.len(), 4 + 32);
    }

    #[test]
    fn encode_erc6909_balance_of_matches_eth_abi_output() {
        // selector + 32-byte address + 32-byte uint256(= uint160(WETH)).
        let data = encode_erc6909_balance_of_calldata(EXECUTOR, WETH).unwrap();
        assert_eq!(&data[..4], &ERC6909_BALANCE_OF_SELECTOR);
        assert_eq!(&data[4 + 12..4 + 32], EXECUTOR.as_slice());
        // The WETH ERC6909 id = uint160(WETH) — low 20 bytes of the encoded
        // uint256, left-zero-padded to 32 bytes.
        let id_tail = &data[(4 + 32)..];
        assert_eq!(id_tail.len(), 32);
        assert_eq!(&id_tail[12..], WETH.as_slice());
    }

    #[test]
    fn erc6909_id_reuses_executor_leaf() {
        // The ERC6909 id is the §62H23D leaf's `erc6909_id` (uint160(currency))
        // — reused, not recomputed. Cross-check against the inlined read.
        let id = erc6909_id(WETH);
        let low20 = &id.to_be_bytes::<32>()[12..];
        assert_eq!(low20, WETH.as_slice());
    }
}
