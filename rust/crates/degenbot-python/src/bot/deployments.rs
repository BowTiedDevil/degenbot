//! Python seam for the Rust-core deployment-identity lookup (Fork A, 7FA5EZ).
//!
//! Two thin `#[pyfunction]` views over
//! [`degenbot_uniswap::deployments`]: the embedded canonical `deployments.json`
//! is the single source the Python loader *and* the Rust builder both read.
//! These surface the CREATE2-critical fields so a Python builder (and the
//! cross-source lock test) can resolve identity from Rust without touching
//! the Python loader.
//!
//! ```python
//! from degenbot.degenbot_rs import init_hash_for, deployer_for
//! # Uniswap V2 mainnet — deployer=None → effective = factory.
//! assert init_hash_for(1, "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
//! assert deployer_for(1, "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f") == \
//!     "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
//! # PancakeSwap V3 mainnet — separate deployer (the load-bearing case).
//! assert deployer_for(1, "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865") == \
//!     "0x41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9"
//! ```
//!
//! Lives in this `bot::deployments.rs` file (not in the standalone
//! `degenbot-uniswap` crate) so the Rust core stays `pyo3`-free per the
//! ADR-005 standalone constraint. The view is built at call time from the
//! parsed `&'static` record (no second copy of the data).

use crate::prelude::*;

use address_utils::{address_to_checksum_string, parse_address};
use degenbot_uniswap::deployments::{self, AddressMismatch};

/// Resolve the CREATE2 init code hash for a ``(chain_id, factory)`` pair from
/// the embedded canonical `deployments.json`.
///
/// Returns the hash as a lowercase `0x`-prefixed hex string, or `None` when
/// the ``(chain, factory)`` is not a shipped deployment OR the row carries
/// no CREATE2 address generation (Aerodrome, Balancer). Address lookup is
/// case-insensitive.
///
/// # Errors
/// Returns `ValueError` if `factory` is not a valid hex address.
#[pyfunction]
fn init_hash_for(chain_id: u64, factory: &str) -> PyResult<Option<String>> {
    let addr = parse_address(factory)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
    Ok(deployments::lookup(chain_id, addr).and_then(|rec| {
        rec.init_hash.map(|h| format!("{h:#x}"))
    }))
}

/// Resolve the *effective* CREATE2 deployer for a ``(chain_id, factory)``
/// pair from the embedded canonical `deployments.json`.
///
/// The effective deployer is the row's `deployer` when set, else the
/// `factory` itself (the `null → factory` convention). Returns the
/// EIP-55 checksummed address, or `None` for an unregistered
/// ``(chain, factory)``. This is the load-bearing helper for the
/// separate-deployer case (PancakeSwap V3 uses a deployer distinct from
/// its factory).
///
/// # Errors
/// Returns `ValueError` if `factory` is not a valid hex address.
#[pyfunction]
fn deployer_for(chain_id: u64, factory: &str) -> PyResult<Option<String>> {
    let addr = parse_address(factory)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
    Ok(deployments::lookup(chain_id, addr)
        .map(|rec| address_to_checksum_string(&rec.effective_deployer())))
}

/// Register the `init_hash_for` / `deployer_for` free functions on the
/// top-level `degenbot_rs` module.
pub(crate) fn add_deployments(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(init_hash_for, m)?)?;
    m.add_function(wrap_pyfunction!(deployer_for, m)?)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Registration-time verification wrappers (Fork A, JC6OFG)
// ---------------------------------------------------------------------------
//
// Thin `PyResult` wrappers over the pure `degenbot_uniswap::deployments::
// verify_{v2,v3}_pool_address` checks. Called from the `register_v2_pool` /
// `register_v3_pool` Python seams: before registering, recomputes the CREATE2
// address from the JSON-sourced deployer + init hash and rejects a mismatch
// with a clear `ValueError`. Verification is skipped (returns `Ok(())`) when
// the `(chain, factory)` is not in the shipped JSON (manual/ad-hoc
// registration) or the row has no CREATE2 — so only JSON-registered pools are
// enforced.

fn map_mismatch(m: AddressMismatch) -> pyo3::PyErr {
    pyo3::exceptions::PyValueError::new_err(m.to_string()).into()
}

/// Verify a V2 pool registration's declared address against the JSON-sourced
/// CREATE2 deployer + init hash. `Ok(())` if it matches or is not applicable;
/// `Err(PyValueError)` on a verified mismatch.
pub(crate) fn verify_v2(
    chain_id: u64,
    factory: alloy::primitives::Address,
    expected: alloy::primitives::Address,
    token0: alloy::primitives::Address,
    token1: alloy::primitives::Address,
) -> PyResult<()> {
    deployments::verify_v2_pool_address(chain_id, factory, expected, token0, token1)
        .map_err(map_mismatch)
}

/// Verify a V3 pool registration's declared address against the JSON-sourced
/// CREATE2 deployer + init hash (the V3 salt includes the fee). `Ok(())` if it
/// matches or is not applicable; `Err(PyValueError)` on a verified mismatch.
pub(crate) fn verify_v3(
    chain_id: u64,
    factory: alloy::primitives::Address,
    expected: alloy::primitives::Address,
    token0: alloy::primitives::Address,
    token1: alloy::primitives::Address,
    fee: u32,
) -> PyResult<()> {
    deployments::verify_v3_pool_address(chain_id, factory, expected, token0, token1, fee)
        .map_err(map_mismatch)
}