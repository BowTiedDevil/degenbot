//! `PyO3` seam over the `degenbot-submission` core crate.
//!
//! Wraps [`degenbot_submission::TxSigner`] as [`PyTxSigner`] and
//! [`degenbot_submission::TxParams`] as [`PyTxParams`] so the Python
//! submission path can sign EIP-1559 transactions + finalize fees through the
//! Rust core — the operator key crosses into Rust ONCE at construction and
//! never round-trips back per-tx (ADR-005 §3 PyO3-layer discipline).
//!
//! The signing is **synchronous ECDSA** (CPU-bound secp256k1, no network), so
//! [`PyTxSigner::sign_eip1559`] releases the GIL around the core
//! [`TxSigner::sign_eip1559`] call via [`Python::detach`]. There is no async
//! runtime + no `block_on` here — signing is pure compute, unlike the price
//! readers' `eth_call` (which IS async I/O).

pub mod params;
pub mod signer;

pub use params::PyTxParams;
pub use signer::PyTxSigner;

use pyo3::prelude::*;

/// Register the submission pyclasses + `finalize_fees` pyfunction on the module.
///
/// # Errors
///
/// Returns `PyErr` if a class/function fails to register on the module.
pub fn add_submission_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyTxSigner>()?;
    m.add_class::<PyTxParams>()?;
    m.add_function(wrap_pyfunction!(
        crate::submission::params::finalize_fees_py,
        m
    )?)?;
    Ok(())
}
