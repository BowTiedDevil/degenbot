//! Async Ethereum RPC provider implementation using Alloy.
//!
//! Provides a Python-facing async provider that wraps `AlloyProvider`
//! methods for non-blocking Ethereum RPC operations via `PyO3`.
//!
//! # GIL Safety
//!
//! All `Python::attach()` calls in this module run on Tokio worker threads
//! after the Rust RPC call completes. These may block briefly while the
//! Python event loop or another Python thread holds the `GIL`. This cannot
//! deadlock because:
//! - Sync provider methods release the `GIL` via `py.detach()` before `block_on()`
//! - The Python event loop periodically releases the `GIL` during I/O polling
//! - `CPython`'s thread scheduler yields the `GIL` every check interval
//!
//! If attach fails because the interpreter is shutting down, `try_attach`
//! returns None and we propagate a clear error.

use crate::conversion::cache::create_hexbytes;
use crate::conversion::rpc_types::{block_to_py_dict, json_to_py_with_hexbytes, log_to_py_dict};
use crate::prelude::*;
use crate::provider::{AlloyProvider, LogFetcher};
use crate::rpc::provider::PyAlloyProvider;
use pyo3::exceptions::PyValueError;
use pyo3::types::PyList;
use pyo3_async_runtimes::tokio::future_into_py;
use std::sync::Arc;

/// Python wrapper for async provider operations.
#[pyclass(name = "AsyncAlloyProvider", skip_from_py_object)]
pub struct PyAsyncAlloyProvider {
    provider: Arc<AlloyProvider>,
    max_blocks_per_request: u64,
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
            max_blocks_per_request: sync_provider.max_blocks_per_request,
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
    /// * `requests_per_second` - Rate limit for HTTP connections (opt-in, must pair with `burst`)
    /// * `burst` - Burst size for rate limiting (opt-in, must pair with `requests_per_second`)
    #[staticmethod]
    #[pyo3(signature = (rpc_url, max_retries=10, max_blocks_per_request=5000, requests_per_second=None, burst=None))]
    fn create(
        py: Python<'_>,
        rpc_url: String,
        max_retries: u32,
        max_blocks_per_request: u64,
        requests_per_second: Option<u32>,
        burst: Option<u32>,
    ) -> PyResult<Bound<'_, PyAny>> {
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

        future_into_py(py, async move {
            let provider = AlloyProvider::build_provider(&rpc_url, max_retries, rate_limit)
                .await
                .map_err(Into::<PyErr>::into)?;

            Ok(Self {
                provider: Arc::new(provider),
                max_blocks_per_request,
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
                .map_err(Into::<PyErr>::into)
        })
    }

    /// Get chain ID asynchronously.
    fn get_chain_id<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let provider = Arc::clone(&self.provider);
        future_into_py(py, async move {
            provider.get_chain_id().await.map_err(Into::<PyErr>::into)
        })
    }

    /// Get logs asynchronously with chunked fetching.
    #[pyo3(signature = (from_block, to_block, addresses=None, topics=None))]
    fn get_logs<'py>(
        &self,
        py: Python<'py>,
        from_block: u64,
        to_block: u64,
        addresses: Option<Vec<String>>,
        topics: Option<Vec<Vec<String>>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let fetcher = LogFetcher::new(Arc::clone(&self.provider), self.max_blocks_per_request);

        future_into_py(py, async move {
            let logs = fetcher
                .fetch_logs_chunked(from_block, to_block, addresses, topics)
                .await
                .map_err(Into::<PyErr>::into)?;

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
            .map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!("Failed to convert logs: {e}"))
            })?;

            Ok::<_, PyErr>(result)
        })
    }

    /// Get a block by number asynchronously.
    fn get_block<'py>(&self, py: Python<'py>, block_number: u64) -> PyResult<Bound<'py, PyAny>> {
        let provider = Arc::clone(&self.provider);

        future_into_py(py, async move {
            let block = provider
                .get_block(block_number)
                .await
                .map_err(Into::<PyErr>::into)?;

            let result = Python::attach(|py| match block {
                Some(block_val) => {
                    let py_dict = block_to_py_dict(py, &block_val)?;
                    Ok::<_, PyErr>(Some(py_dict.into_any().unbind()))
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

    /// Execute an `eth_call` to a contract asynchronously.
    #[pyo3(signature = (to, data, block_number=None))]
    fn call<'py>(
        &self,
        py: Python<'py>,
        to: &str,
        data: Vec<u8>,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let to_address = crate::address_utils::parse_address(to)
            .map_err(|e| PyValueError::new_err(format!("Invalid address: {e}")))?;
        let data_bytes = alloy::primitives::Bytes::from(data);
        let provider = Arc::clone(&self.provider);

        future_into_py(py, async move {
            let result = provider
                .eth_call(&to_address, data_bytes, block_number)
                .await
                .map_err(Into::<PyErr>::into)?;

            Python::attach(|py| create_hexbytes(py, &result).map(Bound::unbind))
        })
    }

    /// Get contract code at an address asynchronously.
    #[pyo3(signature = (address, block_number=None))]
    fn get_code<'py>(
        &self,
        py: Python<'py>,
        address: &str,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let addr = crate::address_utils::parse_address(address)
            .map_err(|e| PyValueError::new_err(format!("Invalid address: {e}")))?;
        let provider = Arc::clone(&self.provider);

        future_into_py(py, async move {
            let result = provider
                .get_code(&addr, block_number)
                .await
                .map_err(Into::<PyErr>::into)?;

            Python::attach(|py| create_hexbytes(py, &result).map(Bound::unbind))
        })
    }

    /// Get current gas price asynchronously.
    fn get_gas_price<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let provider = Arc::clone(&self.provider);

        future_into_py(py, async move {
            provider.get_gas_price().await.map_err(Into::<PyErr>::into)
        })
    }

    /// Estimate gas for a transaction asynchronously.
    #[pyo3(signature = (to, data, from=None, value=None, block_number=None))]
    fn estimate_gas<'py>(
        &self,
        py: Python<'py>,
        to: &str,
        data: Vec<u8>,
        from: Option<String>,
        value: Option<u128>,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let to_address = crate::address_utils::parse_address(to)
            .map_err(|e| PyValueError::new_err(format!("Invalid 'to' address: {e}")))?;
        let from_address = from
            .map(|s| crate::address_utils::parse_address(&s))
            .transpose()
            .map_err(|e| PyValueError::new_err(format!("Invalid 'from' address: {e}")))?;
        let data_bytes = alloy::primitives::Bytes::from(data);
        let provider = Arc::clone(&self.provider);

        future_into_py(py, async move {
            let gas = provider
                .estimate_gas(
                    &to_address,
                    data_bytes,
                    from_address.as_ref(),
                    value,
                    block_number,
                )
                .await
                .map_err(Into::<PyErr>::into)?;

            Ok(gas)
        })
    }

    /// Get a transaction by hash asynchronously.
    fn get_transaction<'py>(
        &self,
        py: Python<'py>,
        tx_hash: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let provider = Arc::clone(&self.provider);

        future_into_py(py, async move {
            let tx = provider
                .get_transaction(&tx_hash)
                .await
                .map_err(Into::<PyErr>::into)?;

            tx.map_or_else(
                || Ok(pyo3::Python::attach(|py| py.None().into_bound(py).unbind())),
                |tx_json| {
                    Python::attach(|py| json_to_py_with_hexbytes(py, tx_json).map(Bound::unbind))
                },
            )
        })
    }

    /// Get a transaction receipt by hash asynchronously.
    fn get_transaction_receipt<'py>(
        &self,
        py: Python<'py>,
        tx_hash: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let provider = Arc::clone(&self.provider);

        future_into_py(py, async move {
            let receipt = provider
                .get_transaction_receipt(&tx_hash)
                .await
                .map_err(Into::<PyErr>::into)?;

            receipt.map_or_else(
                || Ok(pyo3::Python::attach(|py| py.None().into_bound(py).unbind())),
                |receipt_json| {
                    Python::attach(|py| {
                        json_to_py_with_hexbytes(py, receipt_json).map(Bound::unbind)
                    })
                },
            )
        })
    }

    /// Get storage at a given address and position asynchronously.
    #[pyo3(signature = (address, position, block_number=None))]
    fn get_storage_at<'py>(
        &self,
        py: Python<'py>,
        address: &str,
        position: &Bound<'_, PyAny>,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let addr = crate::address_utils::parse_address(address)
            .map_err(|e| PyValueError::new_err(format!("Invalid address: {e}")))?;
        let pos = crate::conversion::alloy::extract_python_u256(position)?;
        let provider = Arc::clone(&self.provider);

        future_into_py(py, async move {
            let result = provider
                .get_storage_at(&addr, pos, block_number)
                .await
                .map_err(Into::<PyErr>::into)?;

            Python::attach(|py| create_hexbytes(py, result.as_slice()).map(Bound::unbind))
        })
    }

    /// Get the balance of an address in wei asynchronously.
    #[pyo3(signature = (address, block_number=None))]
    fn get_balance<'py>(
        &self,
        py: Python<'py>,
        address: &str,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let addr = crate::address_utils::parse_address(address)
            .map_err(|e| PyValueError::new_err(format!("Invalid address: {e}")))?;
        let provider = Arc::clone(&self.provider);

        future_into_py(py, async move {
            let balance = provider
                .get_balance(&addr, block_number)
                .await
                .map_err(Into::<PyErr>::into)?;

            Python::attach(|py| {
                crate::conversion::alloy::u256_to_py(py, &balance).map(Bound::unbind)
            })
        })
    }

    /// Get the transaction count (nonce) for an address asynchronously.
    #[pyo3(signature = (address, block_number=None))]
    fn get_transaction_count<'py>(
        &self,
        py: Python<'py>,
        address: &str,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let addr = crate::address_utils::parse_address(address)
            .map_err(|e| PyValueError::new_err(format!("Invalid address: {e}")))?;
        let provider = Arc::clone(&self.provider);

        future_into_py(py, async move {
            let count = provider
                .get_transaction_count(&addr, block_number)
                .await
                .map_err(Into::<PyErr>::into)?;

            Ok(count)
        })
    }

    /// Make a raw JSON-RPC request asynchronously.
    ///
    /// This allows calling arbitrary RPC methods that don't have typed wrappers.
    #[pyo3(signature = (method, params))]
    fn make_request<'py>(
        &self,
        py: Python<'py>,
        method: &str,
        params: &Bound<'_, PyList>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let params_json = crate::conversion::json::python_list_to_json(params)?;

        let method = method.to_string();
        let provider = Arc::clone(&self.provider);

        future_into_py(py, async move {
            let result = provider
                .make_request(&method, params_json)
                .await
                .map_err(Into::<PyErr>::into)?;

            Python::attach(|py| json_to_py_with_hexbytes(py, result).map(Bound::unbind))
        })
    }

    /// Close the provider.
    const fn close(&self) {
        // No-op for now - provider connection is managed internally
        // This method exists for API compatibility
        let _ = self;
    }

    /// Get the RPC URL.
    #[getter]
    fn rpc_url(&self) -> String {
        self.provider.rpc_url().to_string()
    }

    fn __repr__(&self) -> String {
        format!("AsyncAlloyProvider(rpc_url={:?})", self.provider.rpc_url())
    }
}
