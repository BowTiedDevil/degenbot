//! Async Ethereum RPC provider implementation using Alloy.
//!
//! Provides async variants of the provider methods for non-blocking
//! Ethereum RPC operations.

use crate::errors::ProviderResult;
use crate::provider::{AlloyProvider, LogFilter};
use crate::provider_py::{json_to_py, PyAlloyProvider};
use alloy::rpc::types::Log;
use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;
use std::sync::Arc;

/// A log entry with its associated metadata.
/// Tuple of: (address, topics, data, block_number, block_hash, transaction_hash, log_index)
type LogEntry = (
    String,
    Vec<String>,
    String,
    Option<u64>,
    Option<String>,
    Option<String>,
    Option<u64>,
);

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
    /// * `max_connections` - Maximum number of concurrent connections (default: 10)
    /// * `timeout` - Request timeout in seconds (default: 30.0)
    /// * `max_retries` - Maximum retry attempts (default: 10)
    #[staticmethod]
    #[pyo3(signature = (rpc_url, max_connections=10, timeout=30.0, max_retries=10))]
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    fn create(
        py: Python<'_>,
        rpc_url: String,
        max_connections: u32,
        timeout: f64,
        max_retries: u32,
    ) -> PyResult<Bound<'_, PyAny>> {
        future_into_py(py, async move {
            let provider =
                AlloyProvider::new(&rpc_url, max_connections, timeout as u64, max_retries)
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

            // Convert to a simple Vec of tuples that can be converted to Python
            let result: Vec<LogEntry> = logs
                .into_iter()
                .map(|log| {
                    (
                        log.address().to_string(),
                        log.topics()
                            .iter()
                            .map(std::string::ToString::to_string)
                            .collect(),
                        format!("0x{}", hex::encode(log.data().data.clone())),
                        log.block_number,
                        log.block_hash.map(|h| h.to_string()),
                        log.transaction_hash.map(|h| h.to_string()),
                        log.log_index,
                    )
                })
                .collect();

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
                    let py_obj = json_to_py(py, json_val)?;
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
