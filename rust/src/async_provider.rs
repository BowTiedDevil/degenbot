//! Async Ethereum RPC provider implementation using Alloy.
//!
//! Provides a Python-facing async provider that wraps `AlloyProvider`
//! methods for non-blocking Ethereum RPC operations via `PyO3`.

use crate::provider::{AlloyProvider, LogFetcher};
use crate::provider_py::PyAlloyProvider;
use crate::py_cache::create_hexbytes;
use crate::py_converters::{block_to_py_dict, json_to_py_with_hexbytes, log_to_py_dict};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyList};
use pyo3_async_runtimes::tokio::future_into_py;
use std::sync::Arc;

/// Python wrapper for async provider operations.
#[pyclass(name = "AsyncAlloyProvider")]
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
    #[staticmethod]
    #[pyo3(signature = (rpc_url, max_retries=10, max_blocks_per_request=5000))]
    fn create(
        py: Python<'_>,
        rpc_url: String,
        max_retries: u32,
        max_blocks_per_request: u64,
    ) -> PyResult<Bound<'_, PyAny>> {
        future_into_py(py, async move {
            let provider = AlloyProvider::new(&rpc_url, max_retries)
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
            provider
                .get_gas_price()
                .await
                .map_err(Into::<PyErr>::into)
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
        let pos = crate::alloy_py::extract_python_u256(position)?;
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

            Python::attach(|py| crate::alloy_py::u256_to_py(py, &balance).map(Bound::unbind))
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
        params: Bound<'_, PyList>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let params_json = python_list_to_json_async(&params)?;

        let method = method.to_string();
        let provider = Arc::clone(&self.provider);

        future_into_py(py, async move {
            let result = provider
                .make_request(&method, params_json)
                .await
                .map_err(Into::<PyErr>::into)?;

            Python::attach(|py| {
                json_to_py_with_hexbytes(py, result).map(Bound::unbind)
            })
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
}

/// Convert a Python list to a JSON array.
fn python_list_to_json_async(list: &Bound<'_, PyList>) -> PyResult<serde_json::Value> {
    let mut arr = Vec::new();
    for item in list.iter() {
        let json_val = python_to_json_async(&item)?;
        arr.push(json_val);
    }
    Ok(serde_json::Value::Array(arr))
}

/// Convert a Python object to a JSON value.
fn python_to_json_async(obj: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
    use pyo3::types::PyBool;

    // None -> null
    if obj.is_none() {
        return Ok(serde_json::Value::Null);
    }

    // bool (check before int since bool is subclass of int in Python)
    if let Ok(b) = obj.cast::<PyBool>() {
        return Ok(serde_json::Value::Bool(b.is_true()));
    }

    // int
    if let Ok(i) = obj.extract::<i64>() {
        return Ok(serde_json::Value::Number(i.into()));
    }
    // Large int (serialize as string for JSON, but Alloy handles it)
    if let Ok(s) = obj.str() {
        let s = s.to_string();
        // Check if it looks like a number
        if s.parse::<u128>().is_ok() || s.parse::<i128>().is_ok() {
            // Return as number if it fits, otherwise as string
            if let Ok(n) = s.parse::<i64>() {
                return Ok(serde_json::Value::Number(n.into()));
            }
            // For larger numbers, return as string (JSON doesn't support arbitrary precision)
            return Ok(serde_json::Value::String(s));
        }
    }

    // float
    if let Ok(f) = obj.extract::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Ok(serde_json::Value::Number(n));
        }
        return Ok(serde_json::Value::Null); // NaN/Inf -> null
    }

    // string
    if let Ok(s) = obj.extract::<String>() {
        return Ok(serde_json::Value::String(s));
    }

    // bytes -> hex string
    if let Ok(b) = obj.cast::<PyBytes>() {
        let hex = crate::hex_utils::encode_hex(b.as_bytes());
        return Ok(serde_json::Value::String(hex));
    }

    // list
    if let Ok(list) = obj.cast::<PyList>() {
        return python_list_to_json_async(&list);
    }

    // dict
    if let Ok(dict) = obj.cast::<pyo3::types::PyDict>() {
        let mut map = serde_json::Map::new();
        for (key, val) in dict.iter() {
            let key_str = key.extract::<String>()?;
            let json_val = python_to_json_async(&val)?;
            map.insert(key_str, json_val);
        }
        return Ok(serde_json::Value::Object(map));
    }

    // Fallback: convert to string
    let s = obj.str()?.to_string();
    Ok(serde_json::Value::String(s))
}
