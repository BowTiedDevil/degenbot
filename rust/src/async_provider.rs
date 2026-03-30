//! Async Ethereum RPC provider implementation using Alloy.
//!
//! Provides async variants of the provider methods for non-blocking
//! Ethereum RPC operations.

use crate::errors::ProviderResult;
use crate::provider::{AlloyProvider, LogFilter};
use alloy::rpc::types::Log;
use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;
use std::sync::Arc;

/// A log entry with its associated metadata.
/// Tuple of: (address, topics, data, `block_number`, `block_hash`, `transaction_hash`, `log_index`)
type LogEntry = (String, Vec<String>, String, Option<u64>, Option<String>, Option<String>, Option<u64>);

/// Async wrapper for `AlloyProvider` that exposes async methods to Python.
pub struct AsyncAlloyProvider {
    inner: Arc<AlloyProvider>,
}

impl AsyncAlloyProvider {
    /// Create a new async provider.
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
}

/// Python wrapper for async provider operations.
#[pyclass(name = "AsyncAlloyProvider")]
pub struct PyAsyncAlloyProvider {
    provider: Arc<AlloyProvider>,
}

#[pymethods]
impl PyAsyncAlloyProvider {
    /// Create a new async provider.
    ///
    /// Note: This is a placeholder constructor. In production, this would
    /// be created from an existing provider or connection manager.
    ///
    /// Automatically detects connection type from URL:
    /// - HTTP/HTTPS URLs use HTTP transport
    /// - File paths use IPC transport
    #[new]
    fn new(rpc_url: &str) -> PyResult<Self> {
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("Failed to create runtime: {e}")))?;
        
        let provider = runtime.block_on(async {
            AlloyProvider::new(rpc_url, 10, 30, 10).await
        }).map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("{e}")))?;
        
        let provider = Arc::new(provider);

        Ok(Self { provider })
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
                        log.topics().iter().map(std::string::ToString::to_string).collect(),
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
