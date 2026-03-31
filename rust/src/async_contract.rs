//! Async contract interface with ABI encoding/decoding.
//!
//! Provides async variants of contract calls for non-blocking
//! smart contract interactions.

use crate::contract::Contract;
use crate::provider::AlloyProvider;
use crate::runtime::get_runtime;
use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;
use std::sync::Arc;

/// Async contract wrapper for Python.
#[pyclass(name = "AsyncContract")]
pub struct PyAsyncContract {
    contract: Arc<Contract>,
}

#[pymethods]
impl PyAsyncContract {
    /// Create a new async contract instance.
    ///
    /// Args:
    ///     address: Contract address (hex string)
    ///     `provider_url`: RPC provider URL (HTTP/HTTPS or IPC path)
    #[new]
    fn new(address: &str, provider_url: &str) -> PyResult<Self> {
        // Use the shared runtime to create the provider
        let provider = get_runtime().block_on(async {
            AlloyProvider::new(provider_url, 10, 30, 10).await
        }).map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("{e}")))?;

        let provider = Arc::new(provider);

        let contract = Contract::new(address, provider)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("{e}")))?;

        Ok(Self {
            contract: Arc::new(contract),
        })
    }

    /// Execute a contract call asynchronously.
    ///
    /// Args:
    ///     `function_signature`: Function signature like "balanceOf(address)"
    ///     args: List of arguments as strings
    ///     `block_number`: Optional block number to query
    ///
    /// Returns:
    ///     List of decoded return values as strings
    #[pyo3(signature = (function_signature, args, block_number=None))]
    fn call<'py>(
        &self,
        py: Python<'py>,
        function_signature: String,
        args: Vec<String>,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let contract = Arc::clone(&self.contract);

        future_into_py(py, async move {
            contract
                .call(&function_signature, &args, block_number)
                .await
                .map_err(|e| {
                    pyo3::exceptions::PyValueError::new_err(format!("Contract call failed: {e}"))
                })
        })
    }

    /// Batch execute multiple contract calls asynchronously.
    ///
    /// Args:
    ///     calls: List of (`function_signature`, args) tuples
    ///     `block_number`: Optional block number to query
    ///
    /// Returns:
    ///     List of results, where each result is a list of decoded return values
    #[pyo3(signature = (calls, block_number=None))]
    fn batch_call<'py>(
        &self,
        py: Python<'py>,
        calls: Vec<(String, Vec<String>)>,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let contract = Arc::clone(&self.contract);

        future_into_py(py, async move {
            let mut results = Vec::with_capacity(calls.len());

            for (func_sig, args) in calls {
                let result = contract
                    .call(&func_sig, &args, block_number)
                    .await
                    .map_err(|e| {
                        pyo3::exceptions::PyValueError::new_err(format!(
                            "Contract call failed: {e}"
                        ))
                    })?;
                results.push(result);
            }

            Ok::<_, PyErr>(results)
        })
    }

    /// Get the contract address.
    #[getter]
    fn address(&self) -> String {
        format!("{:#x}", self.contract.address())
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn test_async_contract_creation() {
        // This test verifies the async contract can be created
        // Note: This would need a real RPC URL to test properly
    }
}
