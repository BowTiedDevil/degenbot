//! Async Ethereum RPC provider implementation using Alloy.
//!
//! Provides async variants of the provider methods for non-blocking
//! Ethereum RPC operations.

use crate::errors::ProviderResult;
use crate::provider::{AlloyProvider, LogFilter};
use crate::provider_py::PyAlloyProvider;
use crate::utils::{json_to_py_with_hexbytes, log_to_py_dict};
use alloy::rpc::types::Log;
use pyo3::prelude::*;
use pyo3::types::PyList;
use pyo3_async_runtimes::tokio::future_into_py;
use std::sync::Arc;

/// Async wrapper for `AlloyProvider` that exposes async methods to Python.
pub struct AsyncAlloyProvider {
    inner: Arc<AlloyProvider>,
}

impl AsyncAlloyProvider {
    /// Create a new async provider from an existing provider.
    #[must_use]
    pub const fn new(provider: Arc<AlloyProvider>) -> Self {
        Self { inner: provider }
    }

    /// Get current block number asynchronously.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_block_number(&self) -> ProviderResult<u64> {
        self.inner.get_block_number().await
    }

    /// Get chain ID asynchronously.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_chain_id(&self) -> ProviderResult<u64> {
        self.inner.get_chain_id().await
    }

    /// Fetch logs asynchronously.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError` if the RPC call fails or filter is invalid.
    pub async fn get_logs(&self, filter: &LogFilter) -> ProviderResult<Vec<Log>> {
        self.inner.get_logs(filter).await
    }

    /// Get a block by number asynchronously.
    ///
    /// Returns the full block data including header and transactions.
    /// All field names use `snake_case` for Python consistency.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_block(&self, block_number: u64) -> ProviderResult<Option<serde_json::Value>> {
        self.inner.get_block(block_number).await
    }
}

/// Python wrapper for async provider operations.
#[pyclass(name = "AsyncAlloyProvider")]
pub struct PyAsyncAlloyProvider {
    provider: Arc<AlloyProvider>,
}

#[pymethods]
impl PyAsyncAlloyProvider {
    /// Create a new async provider from an existing sync provider.
    ///
    /// This constructor takes a `PyAlloyProvider` and creates an async wrapper
    /// around it. The async provider uses Python's asyncio event loop instead
    /// of creating its own runtime.
    #[new]
    fn new(sync_provider: &PyAlloyProvider) -> Self {
        Self {
            provider: sync_provider.provider.clone(),
        }
    }

    /// Create a new async provider from RPC URL.
    ///
    /// This is an async factory method that creates both the underlying provider
    /// and the async wrapper in one call.
    ///
    /// # Arguments
    /// * `rpc_url` - The RPC endpoint URL
    /// * `max_retries` - Maximum retry attempts (default: 10)
    #[staticmethod]
    #[pyo3(signature = (rpc_url, max_retries=10))]
    fn create(
        py: Python<'_>,
        rpc_url: String,
        max_retries: u32,
    ) -> PyResult<Bound<'_, PyAny>> {
        future_into_py(py, async move {
            let provider =
                AlloyProvider::new(&rpc_url, max_retries)
                    .await
                    .map_err(|e| {
                        pyo3::exceptions::PyValueError::new_err(format!(
                            "Failed to create provider: {e}"
                        ))
                    })?;

            Ok(Self {
                provider: Arc::new(provider),
            })
        })
    }

    /// Get current block number asynchronously.
    fn get_block_number<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let provider = Arc::clone(&self.provider);
        future_into_py(py, async move {
            provider
                .get_block_number()
                .await
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("{e}")))
        })
    }

    /// Get chain ID asynchronously.
    fn get_chain_id<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let provider = Arc::clone(&self.provider);
        future_into_py(py, async move {
            provider
                .get_chain_id()
                .await
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("{e}")))
        })
    }

    /// Get logs asynchronously with filter.
    #[pyo3(signature = (from_block, to_block, addresses=None, topics=None))]
    fn get_logs<'py>(
        &self,
        py: Python<'py>,
        from_block: u64,
        to_block: u64,
        addresses: Option<Vec<String>>,
        topics: Option<Vec<Vec<String>>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let filter = LogFilter::new(from_block, to_block, addresses, topics)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("{e}")))?;
        let provider = Arc::clone(&self.provider);

        future_into_py(py, async move {
            let logs = provider
                .get_logs(&filter)
                .await
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("{e}")))?;

            // Convert logs to Python list of dicts with HexBytes for appropriate fields
            // Use Python::attach to get GIL for Python object creation
            let result = Python::attach(|py| {
                let py_logs = PyList::empty(py);

                for log in logs {
                    let dict = log_to_py_dict(py, &log)?;
                    py_logs.append(dict)?;
                }

                Ok::<_, PyErr>(py_logs.unbind())
            })
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("Failed to convert logs: {e}")))?;

            Ok::<_, PyErr>(result)
        })
    }

    /// Get a block by number asynchronously.
    fn get_block<'py>(&self, py: Python<'py>, block_number: u64) -> PyResult<Bound<'py, PyAny>> {
        let provider = Arc::clone(&self.provider);

        future_into_py(py, async move {
            // Step 1: Async work (GIL released automatically by future_into_py)
            let block = provider
                .get_block(block_number)
                .await
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("{e}")))?;

            // Step 2: Convert to Python objects (need GIL)
            // Use Python::attach to temporarily acquire GIL
            let result = Python::attach(|py| match block {
                Some(json_val) => {
                    // Use json_to_py_with_hexbytes to convert with automatic HexBytes detection
                    let py_obj = json_to_py_with_hexbytes(py, json_val)?;
                    Ok::<_, PyErr>(Some(py_obj.unbind()))
                }
                None => Ok(None),
            })
            .map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "Failed to convert block data: {e}"
                ))
            })?;

            Ok::<_, PyErr>(result)
        })
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn test_async_provider_creation() {
        // This test verifies the async provider can be created
        // Note: This would need a real RPC URL to test properly
        // For now we just verify the types compile correctly
    }
}
