//! `PyO3` bindings for the provider module.

use crate::provider::{AlloyProvider, LogFetcher, LogFilter};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
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
    pub provider: Arc<AlloyProvider>,
    max_blocks_per_request: u64,
    runtime: tokio::runtime::Runtime,
}

#[pymethods]
impl PyAlloyProvider {
    /// Create a new provider with embedded tokio runtime.
    ///
    /// Automatically detects connection type from URL:
    /// - HTTP/HTTPS URLs use HTTP transport with connection pooling
    /// - File paths (Unix: /path, Windows: \\.\pipe\...) use IPC transport
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
        // Create a dedicated tokio runtime for this provider
        // max_connections is used for both:
        // 1. Runtime worker threads (concurrent execution)
        // 2. HTTP connection pool size (in AlloyProvider)
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(max_connections as usize)
            .enable_all()
            .build()
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to create tokio runtime: {e}")))?;

        // Create the provider inside the runtime context
        let provider = runtime.block_on(async {
            AlloyProvider::new(
                rpc_url,
                max_connections,
                timeout as u64,
                max_retries,
            ).await
        }).map_err(|e| PyValueError::new_err(format!("Failed to create provider: {e}")))?;

        Ok(Self {
            provider: Arc::new(provider),
            max_blocks_per_request,
            runtime,
        })
    }

    /// Fetch logs synchronously (blocking).
    #[pyo3(signature = (*, from_block, to_block, addresses=None, topics=None))]
    fn get_logs(
        &self,
        py: Python<'_>,
        from_block: u64,
        to_block: u64,
        addresses: Option<Vec<String>>,
        topics: Option<Vec<Vec<String>>>,
    ) -> PyResult<Py<PyList>> {
        let fetcher = LogFetcher::new(
            // Clone the Arc to share the provider (cheap - just increments ref count)
            Arc::clone(&self.provider),
            self.max_blocks_per_request,
        );

        // Use our embedded runtime to execute async code
        let logs = self.runtime.block_on(async {
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

    /// Execute an `eth_call` to a contract.
    #[pyo3(signature = (to, data, block_number=None))]
    fn call(
        &self,
        py: Python<'_>,
        to: &str,
        data: &Bound<'_, PyBytes>,
        block_number: Option<u64>,
    ) -> PyResult<Py<PyBytes>> {
        use alloy_primitives::Address;
        use std::str::FromStr;

        // Parse the address
        let to_address = Address::from_str(to)
            .map_err(|e| PyValueError::new_err(format!("Invalid address: {e}")))?;

        // Get the data bytes
        let data_bytes: &[u8] = data.as_bytes();

        // Execute eth_call using the runtime
        let result = self.runtime.block_on(async {
            self.provider
                .eth_call(&to_address, alloy_primitives::Bytes::from(data_bytes.to_vec()), block_number)
                .await
        }).map_err(|e| PyValueError::new_err(format!("eth_call failed: {e}")))?;

        // Return the result as PyBytes
        Ok(PyBytes::new(py, &result).into())
    }

    /// Get current block number.
    fn get_block_number(&self) -> PyResult<u64> {
        // Use our embedded runtime to execute async code
        let block_number = self.runtime.block_on(async {
            self.provider.get_block_number().await
        }).map_err(|e| PyValueError::new_err(format!("Failed to get block number: {e}")))?;

        Ok(block_number)
    }

    /// Get chain ID.
    fn get_chain_id(&self) -> PyResult<u64> {
        // Use our embedded runtime to execute async code
        let chain_id = self.runtime.block_on(async {
            self.provider.get_chain_id().await
        }).map_err(|e| PyValueError::new_err(format!("Failed to get chain ID: {e}")))?;

        Ok(chain_id)
    }

    /// Get a block by number.
    ///
    /// Returns a dictionary with block information including:
    /// - number: Block number
    /// - hash: Block hash
    /// - timestamp: Block timestamp
    /// - parentHash: Parent block hash
    /// - miner: Miner address
    /// - difficulty: Block difficulty
    /// - totalDifficulty: Total difficulty
    /// - gasLimit: Gas limit
    /// - gasUsed: Gas used
    /// - transactions: Number of transactions
    fn get_block<'py>(&self, py: Python<'py>, block_number: u64) -> PyResult<Option<Bound<'py, PyDict>>> {
        // Use our embedded runtime to execute async code
        let block = self.runtime.block_on(async {
            self.provider.get_block(block_number).await
        }).map_err(|e| PyValueError::new_err(format!("Failed to get block: {e}")))?;
        
        match block {
            Some(block_json) => {
                let dict = PyDict::new(py);
                if let Some(number) = block_json.get("number") {
                    dict.set_item("number", number.as_u64().unwrap_or(0))?;
                }
                if let Some(hash) = block_json.get("hash") {
                    dict.set_item("hash", hash.as_str().unwrap_or(""))?;
                }
                if let Some(timestamp) = block_json.get("timestamp") {
                    dict.set_item("timestamp", timestamp.as_u64().unwrap_or(0))?;
                }
                if let Some(parent_hash) = block_json.get("parentHash") {
                    dict.set_item("parentHash", parent_hash.as_str().unwrap_or(""))?;
                }
                if let Some(miner) = block_json.get("miner") {
                    dict.set_item("miner", miner.as_str().unwrap_or(""))?;
                }
                if let Some(difficulty) = block_json.get("difficulty") {
                    dict.set_item("difficulty", difficulty.as_str().unwrap_or("0"))?;
                }
                if let Some(total_difficulty) = block_json.get("totalDifficulty") {
                    dict.set_item("totalDifficulty", total_difficulty.as_str().unwrap_or(""))?;
                }
                if let Some(gas_limit) = block_json.get("gasLimit") {
                    dict.set_item("gasLimit", gas_limit.as_str().unwrap_or("0"))?;
                }
                if let Some(gas_used) = block_json.get("gasUsed") {
                    dict.set_item("gasUsed", gas_used.as_str().unwrap_or("0"))?;
                }
                if let Some(transactions) = block_json.get("transactions") {
                    dict.set_item("transactions", transactions.as_u64().unwrap_or(0))?;
                }
                Ok(Some(dict))
            }
            None => Ok(None),
        }
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
