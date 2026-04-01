//! `PyO3` bindings for the provider module.

use crate::fast_hexbytes::create_fast_hexbytes;
use crate::provider::{AlloyProvider, LogFetcher, LogFilter};
use crate::runtime::get_runtime;
use crate::utils::json_to_py_with_hexbytes;
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
            return Err(PyValueError::new_err("from_block must be <= to_block"));
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
}

#[pymethods]
impl PyAlloyProvider {
    /// Create a new provider.
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
        // Use the shared runtime to create the provider
        let provider = get_runtime()
            .block_on(async {
                AlloyProvider::new(rpc_url, max_connections, timeout as u64, max_retries).await
            })
            .map_err(|e| PyValueError::new_err(format!("Failed to create provider: {e}")))?;

        Ok(Self {
            provider: Arc::new(provider),
            max_blocks_per_request,
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

        // Use the shared runtime to execute async code
        let logs = get_runtime()
            .block_on(async {
                fetcher
                    .fetch_logs_chunked(from_block, to_block, addresses, topics)
                    .await
            })
            .map_err(|e| PyValueError::new_err(format!("Failed to fetch logs: {e}")))?;

        // Convert logs to Python list of dicts with HexBytes for appropriate fields
        // Use optimized path: Alloy stores raw bytes, access directly without hex decode
        let py_logs = PyList::empty(py);
        for log in logs {
            let dict = PyDict::new(py);

            // address: access inner bytes array directly (Address is wrapper around [u8; 20])
            let address = log.address();
            let address_fhb = create_fast_hexbytes(py, address.as_ref())?;
            dict.set_item("address", address_fhb)?;

            // topics: list of B256 hashes (B256 is wrapper around [u8; 32])
            let topics_list = PyList::empty(py);
            for topic in log.topics() {
                let topic_fhb = create_fast_hexbytes(py, topic.as_ref())?;
                topics_list.append(topic_fhb)?;
            }
            dict.set_item("topics", topics_list)?;

            // data: dynamic bytes (alloy_primitives::Bytes wraps Vec<u8>)
            let data = &log.data().data;
            let data_fhb = create_fast_hexbytes(py, data)?;
            dict.set_item("data", data_fhb)?;

            // blockNumber as int
            dict.set_item("blockNumber", log.block_number)?;

            // blockHash as FastHexBytes (optional)
            if let Some(block_hash) = log.block_hash {
                let block_hash_fhb = create_fast_hexbytes(py, block_hash.as_ref())?;
                dict.set_item("blockHash", block_hash_fhb)?;
            } else {
                dict.set_item("blockHash", py.None())?;
            }

            // transactionHash as FastHexBytes (optional)
            if let Some(tx_hash) = log.transaction_hash {
                let tx_hash_fhb = create_fast_hexbytes(py, tx_hash.as_ref())?;
                dict.set_item("transactionHash", tx_hash_fhb)?;
            } else {
                dict.set_item("transactionHash", py.None())?;
            }

            // logIndex as int
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
    ) -> PyResult<Py<PyAny>> {
        use alloy_primitives::Address;
        use std::str::FromStr;

        // Parse the address
        let to_address = Address::from_str(to)
            .map_err(|e| PyValueError::new_err(format!("Invalid address: {e}")))?;

        // Get the data bytes
        let data_bytes: &[u8] = data.as_bytes();

        // Execute eth_call using the shared runtime
        let result = get_runtime()
            .block_on(async {
                self.provider
                    .eth_call(
                        &to_address,
                        alloy_primitives::Bytes::from(data_bytes.to_vec()),
                        block_number,
                    )
                    .await
            })
            .map_err(|e| PyValueError::new_err(format!("eth_call failed: {e}")))?;

        // Create FastHexBytes from result
        let result_fhb = create_fast_hexbytes(py, &result)?;

        Ok(result_fhb.into())
    }

    /// Get contract code at an address.
    #[pyo3(signature = (address, block_number=None))]
    fn get_code(
        &self,
        py: Python<'_>,
        address: &str,
        block_number: Option<u64>,
    ) -> PyResult<Py<PyAny>> {
        use alloy_primitives::Address;
        use std::str::FromStr;

        // Parse the address
        let addr = Address::from_str(address)
            .map_err(|e| PyValueError::new_err(format!("Invalid address: {e}")))?;

        // Execute eth_getCode using the shared runtime
        let result = get_runtime()
            .block_on(async { self.provider.get_code(&addr, block_number).await })
            .map_err(|e| PyValueError::new_err(format!("Failed to get code: {e}")))?;

        // Create FastHexBytes from result
        let result_fhb = create_fast_hexbytes(py, &result)?;

        Ok(result_fhb.into())
    }

    /// Get current block number.
    fn get_block_number(&self) -> PyResult<u64> {
        // Use the shared runtime to execute async code
        let block_number = get_runtime()
            .block_on(async { self.provider.get_block_number().await })
            .map_err(|e| PyValueError::new_err(format!("Failed to get block number: {e}")))?;

        Ok(block_number)
    }

    /// Get chain ID.
    fn get_chain_id(&self) -> PyResult<u64> {
        // Use the shared runtime to execute async code
        let chain_id = get_runtime()
            .block_on(async { self.provider.get_chain_id().await })
            .map_err(|e| PyValueError::new_err(format!("Failed to get chain ID: {e}")))?;

        Ok(chain_id)
    }

    /// Get a block by number.
    ///
    /// Returns the full block data including header and transactions.
    /// All field names use `snake_case` for Python consistency.
    fn get_block<'py>(
        &self,
        py: Python<'py>,
        block_number: u64,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        // Use the shared runtime to execute async code
        let block = get_runtime()
            .block_on(async { self.provider.get_block(block_number).await })
            .map_err(|e| PyValueError::new_err(format!("Failed to get block: {e}")))?;

        match block {
            Some(block_json) => {
                // Use json_to_py_with_hexbytes to convert with automatic HexBytes detection
                let py_value = json_to_py_with_hexbytes(py, block_json)?;
                Ok(Some(py_value))
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

    /// Get current gas price.
    fn get_gas_price(&self) -> PyResult<String> {
        let gas_price = get_runtime()
            .block_on(async { self.provider.get_gas_price().await })
            .map_err(|e| PyValueError::new_err(format!("Failed to get gas price: {e}")))?;

        Ok(gas_price.to_string())
    }

    /// Estimate gas for a transaction.
    #[pyo3(signature = (to, data, from=None, value=None, block_number=None))]
    fn estimate_gas(
        &self,
        to: &str,
        data: &Bound<'_, PyBytes>,
        from: Option<&str>,
        value: Option<u128>,
        block_number: Option<u64>,
    ) -> PyResult<u64> {
        use alloy_primitives::Address;
        use std::str::FromStr;

        // Parse addresses
        let to_address = Address::from_str(to)
            .map_err(|e| PyValueError::new_err(format!("Invalid 'to' address: {e}")))?;

        let from_address = from
            .map(Address::from_str)
            .transpose()
            .map_err(|e| PyValueError::new_err(format!("Invalid 'from' address: {e}")))?;

        // Get the data bytes
        let data_bytes: &[u8] = data.as_bytes();

        // Execute estimation using the shared runtime
        let gas = get_runtime()
            .block_on(async {
                self.provider
                    .estimate_gas(
                        &to_address,
                        alloy_primitives::Bytes::from(data_bytes.to_vec()),
                        from_address.as_ref(),
                        value,
                        block_number,
                    )
                    .await
            })
            .map_err(|e| PyValueError::new_err(format!("Failed to estimate gas: {e}")))?;

        Ok(gas)
    }

    /// Get a transaction by hash.
    fn get_transaction<'py>(
        &self,
        py: Python<'py>,
        tx_hash: &str,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let tx_hash = tx_hash.to_string();
        let tx = get_runtime()
            .block_on(async { self.provider.get_transaction(&tx_hash).await })
            .map_err(|e| PyValueError::new_err(format!("Failed to get transaction: {e}")))?;

        match tx {
            Some(tx_json) => {
                // Use json_to_py_with_hexbytes to convert with automatic HexBytes detection
                let py_obj = json_to_py_with_hexbytes(py, tx_json).map_err(|e| {
                    PyValueError::new_err(format!("Failed to convert transaction: {e}"))
                })?;
                Ok(Some(py_obj))
            }
            None => Ok(None),
        }
    }

    /// Get a transaction receipt by hash.
    fn get_transaction_receipt<'py>(
        &self,
        py: Python<'py>,
        tx_hash: &str,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let tx_hash = tx_hash.to_string();
        let receipt = get_runtime()
            .block_on(async { self.provider.get_transaction_receipt(&tx_hash).await })
            .map_err(|e| {
                PyValueError::new_err(format!("Failed to get transaction receipt: {e}"))
            })?;

        match receipt {
            Some(receipt_json) => {
                // Use json_to_py_with_hexbytes to convert with automatic HexBytes detection
                let py_obj = json_to_py_with_hexbytes(py, receipt_json).map_err(|e| {
                    PyValueError::new_err(format!("Failed to convert receipt: {e}"))
                })?;
                Ok(Some(py_obj))
            }
            None => Ok(None),
        }
    }
}

/// Add provider module to Python module.
pub fn add_provider_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyLogFilter>()?;
    m.add_class::<PyAlloyProvider>()?;
    Ok(())
}
