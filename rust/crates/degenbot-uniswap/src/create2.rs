//! CREATE2 pool-address derivation — the pure-Rust mirror of the Python
//! `generate_v2_pool_address` / `generate_v3_pool_address` (Fork A, JC6OFG).
//!
//! Two address computations:
//!
//! - [`compute_v2_address`]: salt = `keccak256(abi.encodePacked(t0_sorted,
//!   t1_sorted))` (40 packed bytes).
//! - [`compute_v3_address`]: salt = `keccak256(abi.encode(t0_sorted, t1_sorted,
//!   fee))` (96 standard-encoded bytes — addresses right-aligned in 32-byte
//!   words, `uint24` left-aligned).
//!
//! Then `address = keccak256(0xFF ++ deployer ++ salt ++ init_hash)[12:]`
//! (EIP-1014). The token pair is sorted (lexicographic on the raw 20 bytes —
//! equivalent to `Address`'s `Ord`, which compares the big-endian numeric
//! value), matching the Python `sorted([HexBytes(t0), HexBytes(t1)])`.
//!
//! Used by the Rust builder's registration-time verification
//! ([`crate::deployments::verify_v2_pool_address`] /
//! [`crate::deployments::verify_v3_pool_address`]) — the load-bearing guard
//! that recomputes the CREATE2 address against the JSON-sourced deployer +
//! init hash and rejects a mismatch.
//!
//! # Why here (ADR-005 standalone constraint)
//!
//! A standalone Rust consumer constructing a pool must be able to derive its
//! address without a Python import. The Python `generate_*_pool_address`
//! helpers live in `src/degenbot/uniswap/v{2,3}_functions.py`; this is their
//! pure-Rust twin. `degenbot-uniswap` already owns the protocol-domain value
//! objects (`DexIdentity`, deployments) — the CREATE2 primitive belongs with
//! them, not in `degenbot-core` (which stays leaf-focused).

use alloy::primitives::{keccak256, Address, B256};

/// Sort a pair of addresses lexicographically (matches Python's
/// `sorted([HexBytes(a), HexBytes(b)])` — `Address::Ord` compares the
/// big-endian numeric value, equivalent to byte-wise lexicographic order).
fn sorted_pair(a: Address, b: Address) -> (Address, Address) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// The EIP-1014 CREATE2 address derivation.
///
/// `address = keccak256(0xFF ++ deployer ++ salt ++ init_hash)[12:]`.
fn create2_address(deployer: Address, salt: B256, init_hash: B256) -> Address {
    let mut preimage = [0u8; 85]; // 1 + 20 + 32 + 32
    preimage[0] = 0xFF;
    preimage[1..21].copy_from_slice(deployer.as_slice());
    preimage[21..53].copy_from_slice(salt.as_slice());
    preimage[53..85].copy_from_slice(init_hash.as_slice());
    // The contract address is the least-significant 20 bytes of the hash.
    let hash = keccak256(preimage);
    Address::from_slice(&hash[12..])
}

/// Compute a V2-style CREATE2 pool address.
///
/// `salt = keccak256(abi.encodePacked(token0_sorted, token1_sorted))` — the
/// two addresses packed into 40 bytes (no padding), in sorted order. Mirrors
/// `generate_v2_pool_address` in `src/degenbot/uniswap/v2_functions.py`.
///
/// # Examples
///
/// ```
/// use degenbot_uniswap::create2::compute_v2_address;
/// use alloy::primitives::{address, b256};
///
/// let deployer = address!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f");
/// let init_hash = b256!("96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f");
/// // The token order is irrelevant — the address is sorted internally.
/// let t0 = address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"); // USDC
/// let t1 = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"); // WETH
/// let _addr = compute_v2_address(deployer, t0, t1, init_hash);
/// ```
#[must_use]
pub fn compute_v2_address(
    deployer: Address,
    token0: Address,
    token1: Address,
    init_hash: B256,
) -> Address {
    let (t0, t1) = sorted_pair(token0, token1);
    let mut packed = [0u8; 40];
    packed[..20].copy_from_slice(t0.as_slice());
    packed[20..].copy_from_slice(t1.as_slice());
    let salt = keccak256(packed);
    create2_address(deployer, salt, init_hash)
}

/// Compute a V3-style CREATE2 pool address.
///
/// `salt = keccak256(abi.encode(token0_sorted, token1_sorted, fee))` —
/// standard ABI encoding (each value right-aligned in a 32-byte word), so the
/// salt preimage is 96 bytes: 12 zero bytes + `token0` (20), 12 zero bytes +
/// `token1` (20), 29 zero bytes + `fee` (3 bytes big-endian). Mirrors
/// `generate_v3_pool_address` in `src/degenbot/uniswap/v3_functions.py`.
///
/// # Examples
///
/// ```
/// use degenbot_uniswap::create2::compute_v3_address;
/// use alloy::primitives::{address, b256};
///
/// let deployer = address!("1F98431c8aD98523631AE4a59f267346ea31F984");
/// let init_hash = b256!("e34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54");
/// let t0 = address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
/// let t1 = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
/// let _addr = compute_v3_address(deployer, t0, t1, 500, init_hash);
/// ```
#[must_use]
pub fn compute_v3_address(
    deployer: Address,
    token0: Address,
    token1: Address,
    fee: u32,
    init_hash: B256,
) -> Address {
    let (t0, t1) = sorted_pair(token0, token1);
    let mut encoded = [0u8; 96];
    // Word 0: address right-aligned (12 leading zeros).
    encoded[12..32].copy_from_slice(t0.as_slice());
    // Word 1: address right-aligned (12 leading zeros).
    encoded[44..64].copy_from_slice(t1.as_slice());
    // Word 2: uint24 left-aligned (big-endian, 29 leading zeros).
    let fee_be = fee.to_be_bytes(); // [u8; 4]
    encoded[93..96].copy_from_slice(&fee_be[1..]); // low 3 bytes
    let salt = keccak256(encoded);
    create2_address(deployer, salt, init_hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, b256};

    // Cross-checked against the Python `generate_v2_pool_address` /
    // `generate_v3_pool_address` (and the on-chain addresses these factories
    // deployed).

    const UNISWAP_V2_FACTORY: Address = address!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f");
    const UNISWAP_V2_INIT_HASH: B256 =
        b256!("96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f");
    const UNISWAP_V3_FACTORY: Address = address!("1F98431c8aD98523631AE4a59f267346ea31F984");
    const UNISWAP_V3_INIT_HASH: B256 =
        b256!("e34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54");
    // PancakeSwap V3's separate deployer (the load-bearing case).
    const PANCAKESWAP_V3_DEPLOYER: Address = address!("41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9");

    #[test]
    fn token_order_is_irrelevant_v2() {
        let t0 = address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
        let t1 = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
        assert_eq!(
            compute_v2_address(UNISWAP_V2_FACTORY, t0, t1, UNISWAP_V2_INIT_HASH),
            compute_v2_address(UNISWAP_V2_FACTORY, t1, t0, UNISWAP_V2_INIT_HASH),
        );
    }

    #[test]
    fn token_order_is_irrelevant_v3() {
        let t0 = address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
        let t1 = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
        assert_eq!(
            compute_v3_address(UNISWAP_V3_FACTORY, t0, t1, 500, UNISWAP_V3_INIT_HASH),
            compute_v3_address(UNISWAP_V3_FACTORY, t1, t0, 500, UNISWAP_V3_INIT_HASH),
        );
    }

    #[test]
    fn uniswap_v2_address_matches_python_reference() {
        // Cross-checked against `generate_v2_pool_address` (Python reference) +
        // the on-chain DAI/WETH Uniswap V2 mainnet pool address.
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let dai = address!("6B175474E89094C44Da98b954EedeAC495271d0F");
        let addr = compute_v2_address(UNISWAP_V2_FACTORY, weth, dai, UNISWAP_V2_INIT_HASH);
        // DAI/WETH Uniswap V2 mainnet pool.
        assert_eq!(
            addr,
            address!("A478c2975Ab1Ea89e8196811F51A7B7Ade33eB11"),
            "Uniswap V2 DAI/WETH pool address must match the on-chain CREATE2 derivation"
        );
    }

    #[test]
    fn pancakeswap_v3_separate_deployer_distinct_from_factory_deployment() {
        // The load-bearing case: with the separate deployer, the address
        // differs from computing with the factory as deployer. This is why a
        // (chain, factory)-keyed deployer lookup is required.
        let factory = address!("0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865");
        let pcs_init_hash =
            b256!("6ce8eb472fa82df5469c6ab6d485f17c3ad13c8cd7af59b3d4a8026c5ce0f7e2");
        let t0 = address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
        let t1 = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
        let with_sep = compute_v3_address(PANCAKESWAP_V3_DEPLOYER, t0, t1, 500, pcs_init_hash);
        let with_factory = compute_v3_address(factory, t0, t1, 500, pcs_init_hash);
        assert_ne!(
            with_sep, with_factory,
            "separate deployer must yield a different address than the factory"
        );
    }

    #[test]
    fn fee_changes_v3_address() {
        let t0 = address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
        let t1 = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
        assert_ne!(
            compute_v3_address(UNISWAP_V3_FACTORY, t0, t1, 500, UNISWAP_V3_INIT_HASH),
            compute_v3_address(UNISWAP_V3_FACTORY, t0, t1, 3000, UNISWAP_V3_INIT_HASH),
        );
    }
}
