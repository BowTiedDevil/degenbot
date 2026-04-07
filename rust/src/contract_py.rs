//! `PyO3` bindings for the contract module.

use crate::contract::{encode_arguments, Contract, FunctionSignature};
use crate::provider::AlloyProvider;
use crate::runtime::get_runtime;
use alloy::hex;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyList;
use std::sync::Arc;

/// Python wrapper for a Contract.
#[pyclass(name = "Contract")]
pub struct PyContract {
    contract: Contract,
}

#[pymethods]
impl PyContract {
    /// Create a new contract instance.
    ///
    /// Args:
    ///     address: Contract address (hex string)
    ///     `provider_url`: RPC provider URL (optional, defaults to localhost)
    #[new]
    #[pyo3(signature = (address, provider_url=None))]
    fn new(py: Python<'_>, address: &str, provider_url: Option<String>) -> PyResult<Self> {
        // Default provider URL if not provided
        let url = provider_url.unwrap_or_else(|| "http://localhost:8545".to_string());
        let address = address.to_string();

        // Release GIL during provider creation
        let provider = py
            .detach(|| get_runtime().block_on(async { AlloyProvider::new(&url, 10).await }))
            .map_err(|e| PyValueError::new_err(format!("Failed to create provider: {e}")))?;

        let contract = Contract::new(&address, Arc::new(provider))
            .map_err(|e| PyValueError::new_err(format!("{e}")))?;

        Ok(Self { contract })
    }

    /// Execute a contract call.
    ///
    /// Args:
    ///     `function_signature`: Function signature like "balanceOf(address)"
    ///     args: List of arguments as strings
    ///     `block_number`: Optional block number to query
    ///
    /// Returns:
    ///     List of decoded return values as strings
    #[allow(clippy::needless_pass_by_value)]
    fn call(
        &self,
        py: Python<'_>,
        function_signature: &str,
        args: Vec<String>,
        block_number: Option<u64>,
    ) -> PyResult<Py<PyList>> {
        let function_signature = function_signature.to_string();
        let contract = self.contract.clone();

        // Release GIL during RPC call
        let result = py
            .detach(|| {
                get_runtime().block_on(async {
                    contract.call(&function_signature, &args, block_number).await
                })
            })
            .map_err(|e| PyValueError::new_err(format!("Contract call failed: {e}")))?;

        // Convert results to Python list
        let py_list = PyList::empty(py);
        for value in result {
            py_list.append(value)?;
        }

        Ok(py_list.into())
    }

    /// Get the contract address.
    #[getter]
    fn address(&self) -> String {
        format!("{:#x}", self.contract.address())
    }
}

/// Encode function arguments.
///
/// Args:
///     `function_signature`: Function signature like "transfer(address,uint256)"
///     args: List of arguments as strings
///
/// Returns:
///     Encoded calldata as bytes
#[pyfunction]
#[allow(clippy::needless_pass_by_value)]
fn encode_function_call(function_signature: &str, args: Vec<String>) -> PyResult<Vec<u8>> {
    let func = FunctionSignature::parse(function_signature)?;

    let encoded_args = encode_arguments(&func.inputs, &args)?;

    // Build calldata: selector + encoded_args
    let mut calldata = Vec::with_capacity(4 + encoded_args.len());
    calldata.extend_from_slice(&func.selector);
    calldata.extend_from_slice(&encoded_args);

    Ok(calldata)
}

/// Decode return data.
///
/// Args:
///     data: Return data as bytes
///     `output_types`: List of output type strings like `["uint256", "address"]`
///
/// Returns:
///     List of decoded values as strings
#[pyfunction]
#[allow(clippy::needless_pass_by_value)]
fn decode_return_data(data: &[u8], output_types: Vec<String>) -> PyResult<Vec<String>> {
    use crate::abi_types::AbiType;
    use crate::contract::decode_return_data as decode_impl;

    let types: Vec<AbiType> = output_types
        .iter()
        .map(|t| AbiType::parse(t).map_err(|e| PyValueError::new_err(format!("{e}"))))
        .collect::<PyResult<Vec<_>>>()?;

    decode_impl(data, &types).map_err(Into::into)
}

/// Parse a function signature and return its selector.
///
/// Args:
///     `function_signature`: Function signature like "transfer(address,uint256)"
///
/// Returns:
///     4-byte function selector as hex string
#[pyfunction]
fn get_function_selector(function_signature: &str) -> PyResult<String> {
    let func = FunctionSignature::parse(function_signature)?;
    Ok(format!("0x{}", hex::encode(func.selector)))
}

/// Add contract module to Python module.
pub fn add_contract_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyContract>()?;
    m.add_function(wrap_pyfunction!(encode_function_call, m)?)?;
    m.add_function(wrap_pyfunction!(decode_return_data, m)?)?;
    m.add_function(wrap_pyfunction!(get_function_selector, m)?)?;
    Ok(())
}
