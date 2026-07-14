//! `degenbot-rpc` `PyO3` wrappers (provider/contract/subscription, sync + async)
//! over the pure RPC core. Mirrors `crates/degenbot-rpc/`. (ergo UG6FKN task
//! WXHGOH.)

pub mod contract;
pub mod provider;
pub mod subscription;

#[cfg(feature = "async")]
pub mod async_contract;
#[cfg(feature = "async")]
pub mod async_provider;

use pyo3::prelude::*;
use pyo3::types::PyModule;

/// Raise `degenbot.exceptions.ContractLogicError` for an `eth_call` execution
/// revert.
///
/// Replaces the Python adapter-seam string-scraping
/// (`degenbot.provider.alloy_errors.alloy_revert_error`): the Rust core now
/// classifies reverts into `ProviderError::ExecutionReverted` structurally
/// (see `degenbot-rpc::provider::IntoProviderError`), so the FFI layer raises
/// the degenbot-owned exception directly. Probe sites throughout the codebase
/// catch `RpcError` (the base of `ContractLogicError`) to mean "this method is
/// not implemented by the contract" — identical behavior across backends.
///
/// Falls back to `PyRuntimeError` (the pre-integration behavior) only if the
/// `degenbot.exceptions` module cannot be imported, which should never happen
/// at runtime.
pub(crate) fn revert_to_pyerr(py: Python<'_>, to: &str, message: &str) -> PyErr {
    let formatted = format!("eth_call to {to} reverted: {message}");
    match PyModule::import(py, "degenbot.exceptions") {
        Ok(module) => match module.getattr("ContractLogicError") {
            Ok(exc_class) => match exc_class.call1((formatted.clone(),)) {
                Ok(instance) => PyErr::from_value(instance),
                Err(_) => pyo3::exceptions::PyRuntimeError::new_err(formatted),
            },
            Err(_) => pyo3::exceptions::PyRuntimeError::new_err(formatted),
        },
        Err(_) => pyo3::exceptions::PyRuntimeError::new_err(formatted),
    }
}
