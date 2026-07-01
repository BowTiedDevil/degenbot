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
use degenbot_uniswap::deployments;

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