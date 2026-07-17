//! `PyO3` bindings for address utilities.

use crate::address_utils::{to_checksum_address_bytes, to_checksum_address_str};
use crate::prelude::*;
use degenbot_core::address_utils::parse_address;
use degenbot_uniswap::create2::{compute_aerodrome_v2_address, compute_aerodrome_v3_address};
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
/// `aerodrome.functions.generate_aerodrome_v2_pool_address` (S5SJXF/U43OVR).
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
/// `aerodrome.functions.generate_aerodrome_v3_pool_address` (S5SJXF/U43OVR).
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
