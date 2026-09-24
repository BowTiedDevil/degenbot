//! `PyTxSigner` — `PyO3` wrapper over [`degenbot_submission::TxSigner`].
//!
//! Python constructs this ONCE with the operator private key (hex string or
//! 32 bytes) + the chain id; the held [`PrivateKeySigner`] lives in Rust for
//! the signer's lifetime — the key **never round-trips back into Python** per
//! transaction (the prior `eth_account.Account.sign_transaction(...,
//! private_key=operator_private_key)` path crossed the key on every submit).
//!
//! [`PyTxSigner::sign_eip1559`] releases the GIL around the synchronous ECDSA
//! signing (CPU-bound secp256k1, no network — unlike the price readers'
//! async `eth_call`, signing needs no runtime + no `block_on`).

use crate::prelude::*;
use crate::submission::params::PyTxParams;
use degenbot_submission::TxSigner;
use pyo3::exceptions::PyValueError;
use pyo3::types::PyBytes;

/// An EIP-1559 transaction signer holding the operator key once.
///
/// Constructed from the operator private key (hex or 32 raw bytes) + a chain
/// id. ``sign_eip1559`` builds + signs a type-2 ``TxEnvelope`` and returns the
/// raw signed bytes ready for ``eth_sendRawTransaction``.
#[pyclass(
    name = "TxSigner",
    skip_from_py_object,
    module = "degenbot._ffi.submission"
)]
pub struct PyTxSigner {
    signer: TxSigner,
}

impl PyTxSigner {
    /// Borrow the held `TxSigner` (for the submit leaf `dispatch_and_submit`
    /// which signs each candidate inside Rust — no per-tx key crossing).
    pub(crate) fn signer(&self) -> &TxSigner {
        &self.signer
    }
}

#[pymethods]
impl PyTxSigner {
    /// Create a new signer holding the operator key.
    ///
    /// Args:
    ///     key: The operator private key, either a hex string (with or without
    ///         ``0x`` prefix) or 32 raw bytes. The key is held in Rust for the
    ///         signer's lifetime — it never crosses back into Python per-tx.
    ///     `chain_id`: The chain id used for EIP-155 replay protection on every
    ///         signed transaction.
    ///
    /// Raises:
    ///     `ValueError`: If the key is malformed (bad hex, wrong length, or not
    ///         a valid secp256k1 scalar).
    #[new]
    fn new(key: &Bound<'_, PyAny>, chain_id: u64) -> PyResult<Self> {
        let signer = if let Ok(hex_str) = key.extract::<String>() {
            TxSigner::from_key_hex(&hex_str, chain_id).map_err(Into::<PyErr>::into)?
        } else if let Ok(key_bytes) = key.extract::<Vec<u8>>() {
            TxSigner::from_key_bytes(&key_bytes, chain_id).map_err(Into::<PyErr>::into)?
        } else {
            return Err(PyValueError::new_err(
                "`key` must be a hex string or 32 raw bytes",
            ));
        };
        Ok(Self { signer })
    }

    /// The derived operator address (EIP-55 checksummed hex string).
    #[getter]
    fn address(&self) -> String {
        format!("{:#x}", self.signer.address())
    }

    /// The chain id used for EIP-155 replay protection.
    #[getter]
    const fn chain_id(&self) -> u64 {
        self.signer.chain_id()
    }

    /// Sign a type-2 EIP-1559 transaction and return the raw signed bytes.
    ///
    /// Builds the ``TxEnvelope`` from ``tx_params`` (``to``/``data``/``gas``/
    /// ``nonce``/``value``/``accessList`` + the fee fields set by
    /// :func:`finalize_fees`), signs synchronously via the held
    /// ``LocalSigner`` (RFC 6979 deterministic ECDSA — ``eth_account``-parity),
    /// and encodes as ``Typed2718`` (``0x02`` prefix).
    ///
    /// Signing is synchronous (CPU-bound secp256k1, no network); the GIL is
    /// released for the duration.
    ///
    /// Args:
    ///     `tx_params`: The `PyTxParams` to sign (fees must be finalized first).
    ///
    /// Returns:
    ///     The raw signed transaction bytes (ready for `eth_sendRawTransaction`).
    ///
    /// Raises:
    ///     `ValueError`: If ECDSA signing fails (never for a validly-constructed
    ///         signer — only on key corruption).
    fn sign_eip1559<'py>(
        &self,
        py: Python<'py>,
        tx_params: &PyTxParams,
    ) -> PyResult<Bound<'py, PyBytes>> {
        // Move the TxParams out (the Python caller built + finalized it; the
        // wrapper owns no business logic — pure arg extraction → GIL release →
        // core call → result wrap, ADR-005 §3 B).
        let params = tx_params.inner.clone();
        // Release the GIL for the CPU-bound ECDSA signing. No async runtime
        // needed (signing is sync / no I/O).
        let raw = py.detach(move || self.signer.sign_eip1559(&params))?;
        Ok(PyBytes::new(py, &raw))
    }

    /// Recover the sender address from a raw signed transaction.
    ///
    /// Decodes the bytes produced by [`sign_eip1559`](Self::sign_eip1559) and
    /// recovers the signer. Used by the submission loop's receipt-validation
    /// path + the §4.2 round-trip parity test.
    ///
    /// Args:
    ///     `raw_signed`: The raw signed transaction bytes.
    ///
    /// Returns:
    ///     The recovered sender address (EIP-55 checksummed hex string).
    ///
    /// Raises:
    ///     `ValueError`: If the bytes do not decode or the signature does not
    ///         recover.
    #[staticmethod]
    fn recover_sender(py: Python<'_>, raw_signed: &[u8]) -> PyResult<String> {
        let bytes = raw_signed.to_vec();
        let addr = py.detach(move || TxSigner::recover_sender(&bytes))?;
        Ok(format!("{addr:#x}"))
    }
}
