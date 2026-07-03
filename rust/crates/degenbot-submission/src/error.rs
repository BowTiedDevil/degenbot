//! `SubmissionError` + `SubmissionResult` — error surface for the
//! `degenbot-submission` core crate.

use thiserror::Error;

/// Errors raised by the submission signing + fee-finalization path.
#[derive(Debug, Error)]
pub enum SubmissionError {
    /// Private-key decode or ECDSA signing failure
    /// (`alloy-signer-local::PrivateKeySigner` rejects zero scalars / invalid
    /// key bytes; `sign_transaction_sync` surfaces the alloy signer error).
    #[error(transparent)]
    Sign(#[from] alloy::signers::Error),

    /// Signature-recovery failure: the decoded `TxEnvelope`'s signature did
    /// not recover to a valid address (the §4.2 round-trip gate + the
    /// submission-loop receipt validation path).
    #[error(transparent)]
    Recovery(#[from] alloy::consensus::crypto::RecoveryError),

    /// `TxEnvelope` 2718 decode failure (the raw bytes were not a valid
    /// typed transaction envelope).
    #[error(transparent)]
    Decode(#[from] alloy::eips::eip2718::Eip2718Error),

    /// Fee arithmetic overflow: `max_fee_per_gas = int(1.5 * base_fee_next) +
    /// priority_fee` exceeded `u128`. Only reachable for absurd
    /// (>~10^38 wei) `base_fee_next` values — never on mainnet — but the
    /// integer path is fallible so it does not silently wrap.
    #[error("fee computation overflowed u128 (base_fee_next={base_fee_next}, priority_fee={priority_fee})")]
    FeeOverflow {
        base_fee_next: u128,
        priority_fee: u128,
    },

    /// A receipt-probe RPC failure surfaced by the monitor loop (a non-"not-
    /// found" RPC error from `Provider::get_transaction_receipt`). The Python
    /// oracle wrapped ONLY `except TransactionNotFound`; other RPC errors
    /// propagated — this variant is the typed home for that propagation.
    #[error("monitor receipt-probe RPC failure: {0}")]
    MonitorProbe(String),
}

/// Convenience alias used throughout the crate.
pub type SubmissionResult<T> = Result<T, SubmissionError>;

#[cfg(feature = "pyo3")]
impl From<SubmissionError> for pyo3::PyErr {
    fn from(err: SubmissionError) -> Self {
        use pyo3::exceptions::{PyRuntimeError, PyValueError};
        match err {
            SubmissionError::Sign(_)
            | SubmissionError::Recovery(_)
            | SubmissionError::Decode(_) => PyValueError::new_err(err.to_string()),
            SubmissionError::FeeOverflow { .. }
            | SubmissionError::MonitorProbe(_) => PyRuntimeError::new_err(err.to_string()),
        }
    }
}
