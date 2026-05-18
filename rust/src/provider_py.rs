//! `PyO3` bindings for the provider module.
//!
//! Error mapping convention:
//! - `ProviderError` → Python exception via `From<ProviderError> for PyErr` (preserves
//!   specific types like `TimeoutError`, `ConnectionError`, `RuntimeError`)
//! - Input validation errors (invalid address, invalid position) → `PyValueError`
//! - Serialization/conversion errors during Python object creation → `PyValueError`

use crate::provider::{AlloyProvider, LogFetcher, LogFilter};
use crate::py_cache::create_hexbytes;
use crate::py_converters::{block_to_py_dict, json_to_py_with_hexbytes, log_to_py_dict};
use crate::runtime::get_runtime;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyList};
use std::sync::Arc;

/// Python wrapper for `LogFilter`.
#[pyclass(name = "LogFilter", skip_from_py_object)]
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
        // Delegate all validation to LogFilter::new, convert error to PyErr
        Ok(Self {
            inner: LogFilter::new(from_block, to_block, addresses, topics)
                .map_err(Into::<PyErr>::into)?,
        })
    }

    #[getter]
    #[pyo3(name = "from_block")]
    const fn get_from_block(&self) -> Option<u64> {
        self.inner.from_block()
    }

    #[getter]
    const fn to_block(&self) -> Option<u64> {
        self.inner.to_block()
    }

    #[getter]
    fn addresses(&self) -> Vec<String> {
        self.inner.address_strings()
    }

    #[getter]
    fn topics(&self) -> Vec<Vec<String>> {
        self.inner.topic_strings()
    }

    fn __repr__(&self) -> String {
        format!(
            "LogFilter(from_block={:?}, to_block={:?}, addresses={:?})",
            self.inner.from_block(),
            self.inner.to_block(),
            self.inner.address_strings(),
        )
    }
}

/// Python wrapper for `AlloyProvider`.
#[pyclass(name = "AlloyProvider")]
pub struct PyAlloyProvider {
    pub provider: Arc<AlloyProvider>,
    pub max_blocks_per_request: u64,
}

#[pymethods]
impl PyAlloyProvider {
    /// Create a new provider.
    ///
    /// Automatically detects connection type from URL:
    /// - HTTP/HTTPS URLs use HTTP transport with connection pooling
    /// - WS/WSS URLs use WebSocket transport
    /// - File paths (Unix: /path, Windows: \\.\pipe\...) use IPC transport
    ///
    /// # Retry Behavior
    ///
    /// The provider implements exponential backoff with jitter for retryable errors:
    /// - Initial delay: 100ms
    /// - Max delay: 30 seconds
    /// - Backoff multiplier: 2x
    /// - Jitter: up to 100ms (randomized)
    /// - Max attempts: `max_retries + 1` (1 initial + retries)
    ///
    /// Retryable errors: rate limits (HTTP 429), timeouts, connection failures.
    ///
    /// # Rate Limiting (HTTP only, opt-in)
    ///
    /// By default, HTTP connections run without transport-level rate limiting.
    /// Pass `requests_per_second` and `burst` to enable throttling, which avoids
    /// overwhelming the RPC endpoint. `requests_per_second` controls the sustained
    /// rate, while `burst` allows temporary spikes. Both must be provided together
    /// to activate throttling. These parameters are ignored for WS/IPC transports.
    #[new]
    #[pyo3(signature = (rpc_url, max_retries=10, max_blocks_per_request=5000, requests_per_second=None, burst=None))]
    fn new(
        py: Python<'_>,
        rpc_url: &str,
        max_retries: u32,
        max_blocks_per_request: u64,
        requests_per_second: Option<u32>,
        burst: Option<u32>,
    ) -> PyResult<Self> {
        /// Default burst size for rate limiting (guaranteed non-zero)
        const DEFAULT_BURST: std::num::NonZeroU32 = std::num::NonZeroU32::new(1).unwrap();

        let rate_limit = match (requests_per_second, burst) {
            (Some(rps), Some(b)) => {
                let burst_nz = std::num::NonZeroU32::new(b).unwrap_or(DEFAULT_BURST);
                Some((rps, burst_nz))
            }
            (Some(_), None) | (None, Some(_)) => {
                return Err(PyValueError::new_err(
                    "requests_per_second and burst must be provided together",
                ));
            }
            (None, None) => None,
        };

        // Copy string before detaching from GIL
        let rpc_url = rpc_url.to_string();

        // Release GIL during provider creation to allow parallel instantiation
        let provider = py
            .detach(|| {
                get_runtime().block_on(async {
                    AlloyProvider::build_provider(&rpc_url, max_retries, rate_limit).await
                })
            })
            .map_err(Into::<PyErr>::into)?;

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

        // Release GIL during RPC calls to allow parallel execution
        let logs = py
            .detach(|| {
                get_runtime().block_on(async {
                    fetcher
                        .fetch_logs_chunked(from_block, to_block, addresses, topics)
                        .await
                })
            })
            .map_err(Into::<PyErr>::into)?;

        // Convert logs to Python list of dicts with HexBytes for appropriate fields
        let py_logs = PyList::empty(py);
        for log in logs {
            let dict = log_to_py_dict(py, &log)?;
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
        // Parse the address (input validation → ValueError)
        let to_address = crate::address_utils::parse_address(to)
            .map_err(|e| PyValueError::new_err(format!("Invalid address: {e}")))?;

        // Get the data bytes (copy before releasing GIL)
        let data_bytes = alloy::primitives::Bytes::from(data.as_bytes().to_vec());
        let provider = Arc::clone(&self.provider);

        // Release GIL during RPC call
        let result = py
            .detach(|| {
                get_runtime().block_on(async {
                    provider
                        .eth_call(&to_address, data_bytes, block_number)
                        .await
                })
            })
            .map_err(Into::<PyErr>::into)?;

        // Create HexBytes from result
        let result_hb = create_hexbytes(py, &result)?;

        Ok(result_hb.into())
    }

    /// Get contract code at an address.
    #[pyo3(signature = (address, block_number=None))]
    fn get_code(
        &self,
        py: Python<'_>,
        address: &str,
        block_number: Option<u64>,
    ) -> PyResult<Py<PyAny>> {
        // Parse the address (input validation → ValueError)
        let addr = crate::address_utils::parse_address(address)
            .map_err(|e| PyValueError::new_err(format!("Invalid address: {e}")))?;

        let provider = Arc::clone(&self.provider);

        // Release GIL during RPC call
        let result = py
            .detach(|| {
                get_runtime().block_on(async { provider.get_code(&addr, block_number).await })
            })
            .map_err(Into::<PyErr>::into)?;

        // Create HexBytes from result
        let result_hb = create_hexbytes(py, &result)?;

        Ok(result_hb.into())
    }

    /// Get current block number.
    fn get_block_number(&self, py: Python<'_>) -> PyResult<u64> {
        let provider = Arc::clone(&self.provider);

        // Release GIL during RPC call
        let block_number = py
            .detach(|| get_runtime().block_on(async { provider.get_block_number().await }))
            .map_err(Into::<PyErr>::into)?;

        Ok(block_number)
    }

    /// Get chain ID.
    fn get_chain_id(&self, py: Python<'_>) -> PyResult<u64> {
        let provider = Arc::clone(&self.provider);

        // Release GIL during RPC call
        let chain_id = py
            .detach(|| get_runtime().block_on(async { provider.get_chain_id().await }))
            .map_err(Into::<PyErr>::into)?;

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
        let provider = Arc::clone(&self.provider);

        // Release GIL during RPC call
        let block = py
            .detach(|| get_runtime().block_on(async { provider.get_block(block_number).await }))
            .map_err(Into::<PyErr>::into)?;

        match block {
            Some(block) => {
                let py_dict = block_to_py_dict(py, &block)?;
                Ok(Some(py_dict.into_any()))
            }
            None => Ok(None),
        }
    }

    #[getter]
    fn rpc_url(&self) -> String {
        self.provider.rpc_url().to_string()
    }

    fn __repr__(&self) -> String {
        format!("AlloyProvider(rpc_url={:?})", self.provider.rpc_url())
    }

    /// Get current gas price.
    fn get_gas_price(&self, py: Python<'_>) -> PyResult<u128> {
        let provider = Arc::clone(&self.provider);

        // Release GIL during RPC call
        let gas_price = py
            .detach(|| get_runtime().block_on(async { provider.get_gas_price().await }))
            .map_err(Into::<PyErr>::into)?;

        Ok(gas_price)
    }

    /// Estimate gas for a transaction.
    #[pyo3(signature = (to, data, from=None, value=None, block_number=None))]
    fn estimate_gas(
        &self,
        py: Python<'_>,
        to: &str,
        data: &Bound<'_, PyBytes>,
        from: Option<&str>,
        value: Option<u128>,
        block_number: Option<u64>,
    ) -> PyResult<u64> {
        // Parse addresses (input validation → ValueError)
        let to_address = crate::address_utils::parse_address(to)
            .map_err(|e| PyValueError::new_err(format!("Invalid 'to' address: {e}")))?;

        let from_address = from
            .map(crate::address_utils::parse_address)
            .transpose()
            .map_err(|e| PyValueError::new_err(format!("Invalid 'from' address: {e}")))?;

        // Get the data bytes (copy before releasing GIL)
        let data_bytes = alloy::primitives::Bytes::from(data.as_bytes().to_vec());
        let provider = Arc::clone(&self.provider);

        // Release GIL during RPC call
        let gas = py
            .detach(|| {
                get_runtime().block_on(async {
                    provider
                        .estimate_gas(
                            &to_address,
                            data_bytes,
                            from_address.as_ref(),
                            value,
                            block_number,
                        )
                        .await
                })
            })
            .map_err(Into::<PyErr>::into)?;

        Ok(gas)
    }

    /// Get a transaction by hash.
    fn get_transaction<'py>(
        &self,
        py: Python<'py>,
        tx_hash: &str,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let tx_hash = tx_hash.to_string();
        let provider = Arc::clone(&self.provider);

        // Release GIL during RPC call
        let tx = py
            .detach(|| get_runtime().block_on(async { provider.get_transaction(&tx_hash).await }))
            .map_err(Into::<PyErr>::into)?;

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
        let provider = Arc::clone(&self.provider);

        // Release GIL during RPC call
        let receipt = py
            .detach(|| {
                get_runtime().block_on(async { provider.get_transaction_receipt(&tx_hash).await })
            })
            .map_err(Into::<PyErr>::into)?;

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

    /// Get storage at a given address and position.
    #[pyo3(signature = (address, position, block_number=None))]
    fn get_storage_at(
        &self,
        py: Python<'_>,
        address: &str,
        position: &Bound<'_, PyAny>,
        block_number: Option<u64>,
    ) -> PyResult<Py<PyAny>> {
        // Parse the address (input validation → ValueError)
        let addr = crate::address_utils::parse_address(address)
            .map_err(|e| PyValueError::new_err(format!("Invalid address: {e}")))?;

        // Extract position as U256 (supports large integers like mapping slots)
        let pos = crate::alloy_py::extract_python_u256(position)?;

        let provider = Arc::clone(&self.provider);

        // Release GIL during RPC call
        let result = py
            .detach(|| {
                get_runtime()
                    .block_on(async { provider.get_storage_at(&addr, pos, block_number).await })
            })
            .map_err(Into::<PyErr>::into)?;

        // Create HexBytes from result (32-byte storage slot)
        let result_hb = create_hexbytes(py, result.as_slice())?;

        Ok(result_hb.into())
    }

    /// Get the balance of an address in wei.
    #[pyo3(signature = (address, block_number=None))]
    fn get_balance(
        &self,
        py: Python<'_>,
        address: &str,
        block_number: Option<u64>,
    ) -> PyResult<Py<PyAny>> {
        // Parse the address (input validation → ValueError)
        let addr = crate::address_utils::parse_address(address)
            .map_err(|e| PyValueError::new_err(format!("Invalid address: {e}")))?;

        let provider = Arc::clone(&self.provider);

        // Release GIL during RPC call
        let balance = py
            .detach(|| {
                get_runtime().block_on(async { provider.get_balance(&addr, block_number).await })
            })
            .map_err(Into::<PyErr>::into)?;

        // Convert U256 to Python int
        let py_int = crate::alloy_py::u256_to_py(py, &balance)?;

        Ok(py_int.into())
    }

    /// Get the transaction count (nonce) for an address.
    #[pyo3(signature = (address, block_number=None))]
    fn get_transaction_count(
        &self,
        py: Python<'_>,
        address: &str,
        block_number: Option<u64>,
    ) -> PyResult<u64> {
        // Parse the address (input validation → ValueError)
        let addr = crate::address_utils::parse_address(address)
            .map_err(|e| PyValueError::new_err(format!("Invalid address: {e}")))?;

        let provider = Arc::clone(&self.provider);

        // Release GIL during RPC call
        let count = py
            .detach(|| {
                get_runtime()
                    .block_on(async { provider.get_transaction_count(&addr, block_number).await })
            })
            .map_err(Into::<PyErr>::into)?;

        Ok(count)
    }

    /// Make a raw JSON-RPC request.
    ///
    /// This allows calling arbitrary RPC methods that don't have typed wrappers.
    ///
    /// # Arguments
    /// * `method` - The RPC method name (e.g., "`debug_traceTransaction`")
    /// * `params` - The parameters as a list (will be serialized to JSON array)
    ///
    /// # Returns
    /// The raw result as a Python object (deserialized from JSON)
    fn make_request<'py>(
        &self,
        py: Python<'py>,
        method: &str,
        params: &Bound<'_, PyList>,
    ) -> PyResult<Bound<'py, PyAny>> {
        // Convert Python list to JSON array using shared converter
        let params_json = crate::json_converters::python_list_to_json(params)?;

        let method = method.to_string();
        let provider = Arc::clone(&self.provider);

        // Release GIL during RPC call
        let result = py
            .detach(|| {
                get_runtime().block_on(async { provider.make_request(&method, params_json).await })
            })
            .map_err(Into::<PyErr>::into)?;

        // Convert JSON result back to Python
        let py_obj = json_to_py_with_hexbytes(py, result).map_err(|e| {
            PyValueError::new_err(format!("Failed to convert result: {e}"))
        })?;

        Ok(py_obj)
    }

    // -----------------------------------------------------------------------
    // Subscription methods (require WS or IPC transport)
    // -----------------------------------------------------------------------

    /// Close the underlying connection pool and release resources.
    #[allow(clippy::unused_self, clippy::missing_const_for_fn)]
    fn close(&self) {
        // AlloyProvider doesn't have an explicit close method,
        // but the connection pool is released when the Arc is dropped.
        // This is a no-op for now to satisfy the Python context manager protocol.
    }

    /// Subscribe to new block headers.
    ///
    /// Returns an `AlloySubscription` that yields block header dicts
    /// via the async iterator protocol.
    ///
    /// Requires a WebSocket or IPC transport. Raises `RuntimeError` for HTTP providers.
    fn subscribe_blocks(&self, py: Python<'_>) -> PyResult<Py<crate::subscription_py::PyAlloySubscription>> {
        let handle = self.provider.subscribe_blocks();
        let sub = crate::subscription_py::PyAlloySubscription::from_handle(handle);
        Py::new(py, sub)
    }

    /// Subscribe to full block bodies.
    ///
    /// Returns an `AlloySubscription` that yields full block dicts.
    fn subscribe_full_blocks(&self, py: Python<'_>) -> PyResult<Py<crate::subscription_py::PyAlloySubscription>> {
        let handle = self.provider.subscribe_full_blocks();
        let sub = crate::subscription_py::PyAlloySubscription::from_handle(handle);
        Py::new(py, sub)
    }

    /// Subscribe to pending transaction hashes.
    ///
    /// Returns an `AlloySubscription` that yields transaction hash hex strings.
    fn subscribe_pending_transactions(&self, py: Python<'_>) -> PyResult<Py<crate::subscription_py::PyAlloySubscription>> {
        let handle = self.provider.subscribe_pending_transactions();
        let sub = crate::subscription_py::PyAlloySubscription::from_handle(handle);
        Py::new(py, sub)
    }

    /// Subscribe to full pending transaction bodies.
    ///
    /// Returns an `AlloySubscription` that yields transaction dicts.
    fn subscribe_full_pending_transactions(&self, py: Python<'_>) -> PyResult<Py<crate::subscription_py::PyAlloySubscription>> {
        let handle = self.provider.subscribe_full_pending_transactions();
        let sub = crate::subscription_py::PyAlloySubscription::from_handle(handle);
        Py::new(py, sub)
    }

    /// Subscribe to logs matching the given filter.
    ///
    /// # Arguments
    /// * `addresses` - Optional list of contract addresses to filter
    /// * `topics` - Optional list of topic filters (nested: [[topic0], [topic1], ...])
    ///
    /// Returns an `AlloySubscription` that yields log receipt dicts.
    #[pyo3(signature = (addresses=None, topics=None))]
    fn subscribe_logs(
        &self,
        py: Python<'_>,
        addresses: Option<Vec<String>>,
        topics: Option<Vec<Vec<String>>>,
    ) -> PyResult<Py<crate::subscription_py::PyAlloySubscription>> {
        use alloy::rpc::types::{Filter, Topic};

        let mut filter = Filter::new();

        if let Some(addresses) = addresses {
            let addr_set: Vec<alloy::primitives::Address> = addresses
                .iter()
                .map(|addr| {
                    crate::address_utils::parse_address(addr)
                        .map_err(|e| PyValueError::new_err(format!("Invalid address: {e}")))
                })
                .collect::<PyResult<Vec<_>>>()?;
            filter = filter.address(addr_set);
        }

        if let Some(topic_groups) = topics {
            let mut parsed_topics: [Topic; 4] = Default::default();
            for (i, group) in topic_groups.iter().enumerate() {
                if i >= 4 {
                    return Err(PyValueError::new_err(
                        "Topic index out of range (0-3)"
                    ));
                }
                let hashes: Vec<alloy::primitives::B256> = group
                    .iter()
                    .map(|topic| {
                        topic.parse::<alloy::primitives::B256>().map_err(|e| {
                            PyValueError::new_err(format!("Invalid topic hash: {e}"))
                        })
                    })
                    .collect::<PyResult<Vec<_>>>()?;
                parsed_topics[i] = Topic::from(hashes);
            }
            filter.topics = parsed_topics;
        }

        let handle = self.provider.subscribe_logs(filter);
        let sub = crate::subscription_py::PyAlloySubscription::from_handle(handle);
        Py::new(py, sub)
    }
}

/// Add provider module to Python module.
pub fn add_provider_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyLogFilter>()?;
    m.add_class::<PyAlloyProvider>()?;
    Ok(())
}
