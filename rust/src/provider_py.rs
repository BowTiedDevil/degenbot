//! `PyO3` bindings for the provider module.

use crate::provider::{AlloyProvider, LogFetcher, LogFilter};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use std::sync::Arc;

/// Python wrapper for `LogFilter`.
#[pyclass(name = "LogFilter", from_py_object)]
#[derive(Clone)]
pub struct PyLogFilter {
    pub inner: LogFilter,
}

#[pymethods]
impl PyLogFilter {
    /// Create a new `LogFilter`.
    #[new]
    #[pyo3(signature = (from_block, to_block, addresses=None, topics=None))]
    fn new(
        from_block: u64,
        to_block: u64,
        addresses: Option<Vec<String>>,
        topics: Option<Vec<Vec<String>>>,
    ) -> PyResult<Self> {
        if from_block > to_block {
            return Err(PyValueError::new_err(
                "from_block must be <= to_block",
            ));
        }

        Ok(Self {
            inner: LogFilter::new(from_block, to_block, addresses, topics)
                .map_err(|e| PyValueError::new_err(format!("Failed to create LogFilter: {e}")))?,
        })
    }

    #[getter]
    #[allow(clippy::wrong_self_convention)]
    const fn from_block(&self) -> Option<u64> {
        self.inner.from_block
    }

    #[getter]
    const fn to_block(&self) -> Option<u64> {
        self.inner.to_block
    }

    #[getter]
    fn addresses(&self) -> &[String] {
        &self.inner.addresses
    }

    #[getter]
    fn topics(&self) -> &[Vec<String>] {
        &self.inner.topics
    }
}

/// Python wrapper for `AlloyProvider`.
#[pyclass(name = "AlloyProvider")]
pub struct PyAlloyProvider {
    provider: Arc<AlloyProvider>,
    max_blocks_per_request: u64,
}

#[pymethods]
impl PyAlloyProvider {
    /// Create a new provider.
    #[new]
    #[pyo3(signature = (rpc_url, max_connections=10, timeout=30.0, max_retries=10, max_blocks_per_request=5000))]
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    fn new(
        rpc_url: &str,
        max_connections: u32,
        timeout: f64,
        max_retries: u32,
        max_blocks_per_request: u64,
    ) -> PyResult<Self> {
        let provider = AlloyProvider::new(
            rpc_url,
            max_connections,
            timeout as u64,  // Cast is intentional - timeout is always positive
            max_retries,
        )
        .map_err(|e| PyValueError::new_err(format!("Failed to create provider: {e}")))?;

        Ok(Self {
            provider: Arc::new(provider),
            max_blocks_per_request,
        })
    }

    /// Fetch logs synchronously (blocking).
    #[pyo3(signature = (from_block, to_block, addresses=None, topics=None))]
    fn get_logs(
        &self,
        py: Python<'_>,
        from_block: u64,
        to_block: u64,
        addresses: Option<Vec<String>>,
        topics: Option<Vec<Vec<String>>>,
    ) -> PyResult<Py<PyList>> {
        // Use existing tokio runtime or create one
        let handle = tokio::runtime::Handle::try_current()
            .unwrap_or_else(|_| {
                tokio::runtime::Runtime::new()
                    .expect("Failed to create tokio runtime")
                    .handle()
                    .clone()
            });

        let fetcher = LogFetcher::new(
            // Clone the Arc to share the provider (cheap - just increments ref count)
            Arc::clone(&self.provider),
            self.max_blocks_per_request,
        );

        let logs = handle.block_on(async {
            fetcher
                .fetch_logs_chunked(from_block, to_block, addresses, topics)
                .await
        }).map_err(|e| PyValueError::new_err(format!("Failed to fetch logs: {e}")))?;

        // Convert logs to Python list of dicts
        let py_logs = PyList::empty(py);
        for log in logs {
            let dict = PyDict::new(py);
            
            dict.set_item("address", log.address().to_string())?;

            let topics_list = PyList::empty(py);
            for topic in log.topics() {
                topics_list.append(topic.to_string())?;
            }
            dict.set_item("topics", topics_list)?;

            dict.set_item("data", PyBytes::new(py, &log.data().data))?;
            dict.set_item("blockNumber", log.block_number)?;
            dict.set_item("blockHash", log.block_hash.map(|h| h.to_string()))?;
            dict.set_item("transactionHash", log.transaction_hash.map(|h| h.to_string()))?;
            dict.set_item("logIndex", log.log_index)?;

            py_logs.append(dict)?;
        }

        Ok(py_logs.into())
    }

    /// Get current block number.
    fn get_block_number(&self) -> PyResult<u64> {
        let handle = tokio::runtime::Handle::try_current()
            .unwrap_or_else(|_| {
                tokio::runtime::Runtime::new()
                    .expect("Failed to create tokio runtime")
                    .handle()
                    .clone()
            });

        let block_number = handle.block_on(async {
            self.provider.get_block_number().await
        }).map_err(|e| PyValueError::new_err(format!("Failed to get block number: {e}")))?;

        Ok(block_number)
    }

    /// Get chain ID.
    fn get_chain_id(&self) -> PyResult<u64> {
        let handle = tokio::runtime::Handle::try_current()
            .unwrap_or_else(|_| {
                tokio::runtime::Runtime::new()
                    .expect("Failed to create tokio runtime")
                    .handle()
                    .clone()
            });

        let chain_id = handle.block_on(async {
            self.provider.get_chain_id().await
        }).map_err(|e| PyValueError::new_err(format!("Failed to get chain ID: {e}")))?;

        Ok(chain_id)
    }

    /// Close the provider.
    #[allow(clippy::unused_self)]
    const fn close(&self) {
        // No-op for now
    }

    #[getter]
    fn rpc_url(&self) -> String {
        self.provider.rpc_url().to_string()
    }
}

/// Add provider module to Python module.
pub fn add_provider_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyLogFilter>()?;
    m.add_class::<PyAlloyProvider>()?;
    Ok(())
}
