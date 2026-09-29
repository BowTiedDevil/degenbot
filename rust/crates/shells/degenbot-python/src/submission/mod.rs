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

use pyo3::prelude::*;

pub mod dispatcher;
pub mod params;
pub mod signer;
pub mod sim_pipeline;
pub mod submit;

pub use dispatcher::{PyDispatcher, PyDivergentPool};
pub use params::PyTxParams;
pub use signer::PyTxSigner;
pub use sim_pipeline::PySimSubmitPipeline;
pub use submit::PySubmitCandidate;

/// The `degenbot._ffi.submission` Python submodule (declarative
/// `#[pymodule]`), carrying the submission pyclasses + the
/// `finalize_fees_py` / `dispatch_and_submit_py` / `fetch_fee_history_py`
/// pyfunctions. The parent module registers the submodule itself and its
/// `sys.modules` entry.
#[pymodule(submodule)]
#[pyo3(module = "degenbot._ffi")]
pub mod submission {
    #[pymodule_export]
    use super::{
        PyDispatcher, PyDivergentPool, PySimSubmitPipeline, PySubmitCandidate, PyTxParams,
        PyTxSigner,
    };

    #[pymodule_export]
    use super::params::finalize_fees_py;

    #[pymodule_export]
    use super::submit::{dispatch_and_submit_py, fetch_fee_history_py};
}
