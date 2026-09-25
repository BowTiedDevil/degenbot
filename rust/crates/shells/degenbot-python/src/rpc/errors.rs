//! The chain-identity refusal on the FFI boundary (ADR-062 D8).
//!
//! Binding an endpoint to a chain reads `eth_chainId` once in the core and
//! refuses a disagreement there, so the console, a pure-Rust consumer, and
//! Python share one invariant. The refusal crosses into Python as its own
//! exception type carrying `expected` and `actual`: a driver can act on the
//! disagreement (a misconfigured endpoint is an operator fix, not a retry),
//! which a message string alone cannot be matched against safely.

use pyo3::create_exception;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use degenbot_core::errors::ProviderError;

create_exception!(
    degenbot._ffi,
    ChainMismatchError,
    PyValueError,
    "The endpoint serves a different chain than the one it was bound to."
);

/// The refusal message, spelled by the core error's own `Display` so the
/// Python text and the Rust text cannot drift apart.
fn mismatch_message(expected: u64, actual: u64, endpoint: &str) -> String {
    ProviderError::ChainMismatch {
        expected,
        actual,
        endpoint: endpoint.to_string(),
    }
    .to_string()
}

/// A provider error as the exception a Python caller sees.
///
/// Only the chain-identity refusal is reshaped; every other variant keeps the
/// classification `degenbot-core` gives it (a timeout is a `TimeoutError`, a
/// connection failure a `ConnectionError`), so this adds one case without
/// re-deciding the rest.
pub(crate) fn provider_error_to_pyerr(error: ProviderError) -> PyErr {
    match error {
        ProviderError::ChainMismatch {
            expected,
            actual,
            endpoint,
        } => chain_mismatch_to_pyerr(expected, actual, &endpoint),
        other => other.into(),
    }
}

/// The chain-identity refusal as a `ChainMismatchError` carrying both chain
/// ids and the endpoint that disagreed, so a driver reads the disagreement as
/// values instead of parsing a message.
fn chain_mismatch_to_pyerr(expected: u64, actual: u64, endpoint: &str) -> PyErr {
    let built = Python::attach(|py| -> PyResult<PyErr> {
        let class = py.get_type::<ChainMismatchError>();
        let instance = class.call1((mismatch_message(expected, actual, endpoint),))?;
        for (name, value) in [("expected", expected), ("actual", actual)] {
            instance.setattr(name, value)?;
        }
        instance.setattr("endpoint", endpoint)?;
        Ok(PyErr::from_value(instance))
    });
    // A refusal to attach the attributes is itself the error to raise: an
    // instance without them would be the untyped refusal this replaces.
    built.unwrap_or_else(|error| error)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test assertions fail loudly")]

    use degenbot_core::errors::ProviderError;
    use pyo3::prelude::*;

    use super::{provider_error_to_pyerr, ChainMismatchError};

    /// The refusal must be classifiable by TYPE and carry the two chain ids as
    /// values: a driver decides whether to abort or fix the operator's file
    /// from `expected` / `actual`, not from a substring of the message.
    #[test]
    fn chain_mismatch_carries_the_expected_and_actual_chain_ids() {
        let error = provider_error_to_pyerr(ProviderError::ChainMismatch {
            expected: 1,
            actual: 8453,
            endpoint: "wss://node.example/ws".to_string(),
        });
        pyo3::Python::attach(|py| {
            assert!(error.is_instance_of::<ChainMismatchError>(py));
            // A ValueError subclass, so a caller that already handles a
            // misconfigured provider keeps catching this one.
            assert!(error.is_instance_of::<pyo3::exceptions::PyValueError>(py));
            let instance = error.value(py).as_any();
            assert_eq!(
                instance
                    .getattr("expected")
                    .unwrap()
                    .extract::<u64>()
                    .unwrap(),
                1
            );
            assert_eq!(
                instance
                    .getattr("actual")
                    .unwrap()
                    .extract::<u64>()
                    .unwrap(),
                8453
            );
            assert_eq!(
                instance
                    .getattr("endpoint")
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "wss://node.example/ws"
            );
            let message = instance.str().unwrap().extract::<String>().unwrap();
            assert!(
                message.contains('1') && message.contains("8453"),
                "the message must spell the disagreement, got: {message}"
            );
        });
    }

    /// The translation adds ONE case: every other provider error keeps the
    /// class the core maps it to.
    #[test]
    fn other_provider_errors_keep_their_core_classification() {
        let error = provider_error_to_pyerr(ProviderError::ConnectionFailed {
            message: "connection refused".to_string(),
        });
        pyo3::Python::attach(|py| {
            assert!(error.is_instance_of::<pyo3::exceptions::PyConnectionError>(py));
            assert!(!error.is_instance_of::<ChainMismatchError>(py));
        });
    }
}
