//! `PyO3` bindings for address utilities.

use crate::address_utils::{to_checksum_address_bytes, to_checksum_address_str};
use crate::prelude::*;
use degenbot_core::address_utils::parse_address;
use degenbot_uniswap::create2::{
    compute_aerodrome_v2_address, compute_aerodrome_v3_address, compute_v2_address,
    compute_v3_address,
};
use pyo3::exceptions::{PyTypeError, PyValueError};

/// Generates an EIP-55 checksummed address from the input.
///
/// Accepts either a hex string or a 20-byte sequence and returns
/// a checksummed Ethereum address.
///
/// # Arguments
///
/// * `address` - A Python `str` (hex) or `bytes` (20 bytes) representing an address
///
/// # Returns
///
/// A checksummed address string with uppercase/lowercase letters
///
/// # Errors
///
/// Returns `PyValueError` if:
/// - The string is not a valid hex address
/// - The bytes are not exactly 20 bytes long
///
/// Returns `PyTypeError` if the input is not a string or bytes
///
/// # Example
///
/// ```
/// use degenbot_rs::address_utils::to_checksum_address_str;
///
/// let result = to_checksum_address_str("0x66f9664f97f2b50f62d13ea064982f936de76657");
/// match result {
///     Ok(checksummed) => println!("Checksummed: {}", checksummed),
///     Err(e) => eprintln!("Error: {}", e),
/// }
/// ```
#[pyfunction(signature = (address))]
pub fn to_checksum_address(address: &Bound<'_, PyAny>) -> PyResult<String> {
    // SAFETY: to_checksum_address takes ~50ns — less than the ~200ns
    // GIL release/reacquire overhead. Holding the GIL is faster.
    // Verified by `cargo bench --bench address_utils`.
    if let Ok(s) = address.extract::<&str>() {
        return to_checksum_address_str(s)
            .map_err(|e| PyErr::new::<PyValueError, _>(e.to_string()));
    }

    if let Ok(bytes) = address.extract::<&[u8]>() {
        return to_checksum_address_bytes(bytes)
            .map_err(|e| PyErr::new::<PyValueError, _>(e.to_string()));
    }

    Err(PyErr::new::<PyTypeError, _>(
        "Address must be string or bytes",
    ))
}

/// Compute an Aerodrome V2-style EIP-1167 clone pool address (CREATE2).
///
/// Thin binding over the pure-Rust `degenbot_uniswap::create2::
/// compute_aerodrome_v2_address` — byte-exact parity with the Python
/// `aerodrome.functions.generate_aerodrome_v2_pool_address`.
///
/// The salt is `keccak256(abi.encodePacked(token0_sorted, token1_sorted,
/// stable))` and the pool is the EIP-1167 minimal-proxy clone of
/// `implementation_address` deployed by `deployer_address`.
///
/// # Errors
///
/// Returns `PyValueError` if any address argument is not a valid hex
/// Ethereum address.
#[pyfunction(signature = (deployer_address, token0, token1, stable, implementation_address))]
pub fn compute_aerodrome_v2_pool_address(
    deployer_address: &str,
    token0: &str,
    token1: &str,
    stable: bool,
    implementation_address: &str,
) -> PyResult<String> {
    let deployer = parse_address(deployer_address)
        .map_err(|e| PyValueError::new_err(format!("invalid deployer_address: {e}")))?;
    let t0 =
        parse_address(token0).map_err(|e| PyValueError::new_err(format!("invalid token0: {e}")))?;
    let t1 =
        parse_address(token1).map_err(|e| PyValueError::new_err(format!("invalid token1: {e}")))?;
    let implementation = parse_address(implementation_address)
        .map_err(|e| PyValueError::new_err(format!("invalid implementation_address: {e}")))?;
    let addr = compute_aerodrome_v2_address(deployer, t0, t1, stable, implementation);
    Ok(degenbot_core::address_utils::address_to_checksum_string(
        &addr,
    ))
}

/// Compute an Aerodrome V3 (Slipstream)-style EIP-1167 clone pool address.
///
/// Thin binding over the pure-Rust `degenbot_uniswap::create2::
/// compute_aerodrome_v3_address` — byte-exact parity with the Python
/// `aerodrome.functions.generate_aerodrome_v3_pool_address`.
///
/// The salt is `keccak256(abi.encode(token0_sorted, token1_sorted,
/// tick_spacing))` and the pool is the EIP-1167 minimal-proxy clone of
/// `implementation_address` deployed by `deployer_address`.
///
/// # Errors
///
/// Returns `PyValueError` if any address argument is not a valid hex
/// Ethereum address.
#[pyfunction(signature = (deployer_address, token0, token1, tick_spacing, implementation_address))]
pub fn compute_aerodrome_v3_pool_address(
    deployer_address: &str,
    token0: &str,
    token1: &str,
    tick_spacing: i32,
    implementation_address: &str,
) -> PyResult<String> {
    let deployer = parse_address(deployer_address)
        .map_err(|e| PyValueError::new_err(format!("invalid deployer_address: {e}")))?;
    let t0 =
        parse_address(token0).map_err(|e| PyValueError::new_err(format!("invalid token0: {e}")))?;
    let t1 =
        parse_address(token1).map_err(|e| PyValueError::new_err(format!("invalid token1: {e}")))?;
    let implementation = parse_address(implementation_address)
        .map_err(|e| PyValueError::new_err(format!("invalid implementation_address: {e}")))?;
    let addr = compute_aerodrome_v3_address(deployer, t0, t1, tick_spacing, implementation);
    Ok(degenbot_core::address_utils::address_to_checksum_string(
        &addr,
    ))
}

// --- Uniswap V2/V3 pool-address derivations ---------------------------------
//
// Thin bindings over the pure-Rust `degenbot_uniswap::create2` family —
// the Python counterparts in `src/degenbot/uniswap/v{2,3}_functions.py` and
// `src/degenbot/contract/addresses.py` delegate here.

fn parse_b256_hex(hex_str: &str, field: &str) -> PyResult<alloy::primitives::B256> {
    let bytes = crate::hex_utils::decode_hex(hex_str)
        .map_err(|e| PyValueError::new_err(format!("invalid {field}: {e}")))?;
    if bytes.len() != 32 {
        return Err(PyValueError::new_err(format!(
            "invalid {field}: expected 32 bytes, got {}",
            bytes.len()
        )));
    }
    Ok(alloy::primitives::B256::from_slice(&bytes))
}

/// The generic EIP-1014 CREATE2 address derivation.
///
/// Thin binding over `degenbot_uniswap::create2::create2_address` — the one
/// implementation the whole workspace reads (the Python
/// `contract.addresses.create2_address` chain delegates to it).
///
/// # Errors
///
/// `PyValueError` on a bad hex address or a non-32-byte salt/init hash.
#[pyfunction(signature = (deployer_address, salt, init_code_hash))]
pub fn create2_address(
    deployer_address: &str,
    salt: &str,
    init_code_hash: &str,
) -> PyResult<String> {
    let deployer = parse_address(deployer_address)
        .map_err(|e| PyValueError::new_err(format!("invalid deployer_address: {e}")))?;
    let salt = parse_b256_hex(salt, "salt")?;
    let init_code_hash = parse_b256_hex(init_code_hash, "init_code_hash")?;
    let addr = degenbot_uniswap::create2::create2_address(deployer, salt, init_code_hash);
    Ok(degenbot_core::address_utils::address_to_checksum_string(
        &addr,
    ))
}

/// Compute a Uniswap V2-style CREATE2 pool address — byte-exact counterpart
/// of the Python `degenbot.uniswap.v2_functions.generate_v2_pool_address`.
///
/// `salt = keccak256(abi.encodePacked(token0_sorted, token1_sorted))`.
///
/// # Errors
///
/// `PyValueError` on a bad hex argument.
#[pyfunction(signature = (deployer_address, token0, token1, init_hash))]
pub fn generate_v2_pool_address(
    deployer_address: &str,
    token0: &str,
    token1: &str,
    init_hash: &str,
) -> PyResult<String> {
    let deployer = parse_address(deployer_address)
        .map_err(|e| PyValueError::new_err(format!("invalid deployer_address: {e}")))?;
    let t0 =
        parse_address(token0).map_err(|e| PyValueError::new_err(format!("invalid token0: {e}")))?;
    let t1 =
        parse_address(token1).map_err(|e| PyValueError::new_err(format!("invalid token1: {e}")))?;
    let init_hash = parse_b256_hex(init_hash, "init_hash")?;
    let addr = compute_v2_address(deployer, t0, t1, init_hash);
    Ok(degenbot_core::address_utils::address_to_checksum_string(
        &addr,
    ))
}

/// Compute a Uniswap V3-style CREATE2 pool address — byte-exact counterpart
/// of the Python `degenbot.uniswap.v3_functions.generate_v3_pool_address`.
///
/// `salt = keccak256(abi.encode(token0_sorted, token1_sorted, fee))` — the
/// token addresses may be passed in any order (sorted internally).
///
/// # Errors
///
/// `PyValueError` on a bad hex argument.
#[pyfunction(signature = (deployer_address, token0, token1, fee, init_hash))]
pub fn generate_v3_pool_address(
    deployer_address: &str,
    token0: &str,
    token1: &str,
    fee: u32,
    init_hash: &str,
) -> PyResult<String> {
    let deployer = parse_address(deployer_address)
        .map_err(|e| PyValueError::new_err(format!("invalid deployer_address: {e}")))?;
    let t0 =
        parse_address(token0).map_err(|e| PyValueError::new_err(format!("invalid token0: {e}")))?;
    let t1 =
        parse_address(token1).map_err(|e| PyValueError::new_err(format!("invalid token1: {e}")))?;
    let init_hash = parse_b256_hex(init_hash, "init_hash")?;
    let addr = compute_v3_address(deployer, t0, t1, fee, init_hash);
    Ok(degenbot_core::address_utils::address_to_checksum_string(
        &addr,
    ))
}
