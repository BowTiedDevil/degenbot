//! `PyO3` bindings for the contract module.

use crate::contract::{encode_arguments, Contract, FunctionSignature};
use crate::provider::AlloyProvider;
use crate::runtime::get_runtime;
use alloy::hex;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyList};
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
            .detach(|| get_runtime().block_on(async { AlloyProvider::new(&url, crate::provider::DEFAULT_MAX_RETRIES).await }))
            .map_err(Into::<PyErr>::into)?;

        let contract = Contract::new(&address, Arc::new(provider))
            .map_err(Into::<PyErr>::into)?;

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
    fn call(
        &self,
        py: Python<'_>,
        function_signature: &str,
        args: &Bound<'_, PyList>,
        block_number: Option<u64>,
    ) -> PyResult<Py<PyList>> {
        let function_signature = function_signature.to_string();
        let contract = self.contract.clone();

        // Extract args from Python list
        let args: Vec<String> = args
            .iter()
            .map(|a| a.extract::<String>())
            .collect::<Result<_, _>>()?;

        // Release GIL during RPC call
        let result = py
            .detach(|| {
                get_runtime().block_on(async {
                    contract
                        .call(&function_signature, &args, block_number)
                        .await
                })
            })
            .map_err(Into::<PyErr>::into)?;

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
fn encode_function_call<'py>(
    py: Python<'py>,
    function_signature: &str,
    args: &Bound<'_, PyList>,
) -> PyResult<Bound<'py, PyBytes>> {
    // Extract args from Python list (copy before releasing GIL)
    let args: Vec<String> = args
        .iter()
        .map(|a| a.extract::<String>())
        .collect::<Result<_, _>>()?;
    let function_signature = function_signature.to_string();

    // Release GIL during pure Rust encoding work
    let calldata = py.detach(|| -> Result<Vec<u8>, crate::errors::ContractError> {
        let func = FunctionSignature::parse(&function_signature)?;
        let encoded_args = encode_arguments(&func.inputs, &args)?;

        let mut calldata = Vec::with_capacity(4 + encoded_args.len());
        calldata.extend_from_slice(&func.selector);
        calldata.extend_from_slice(&encoded_args);
        Ok(calldata)
    })?;

    Ok(PyBytes::new(py, &calldata))
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
fn decode_return_data(
    py: Python<'_>,
    data: &[u8],
    output_types: &Bound<'_, PyList>,
) -> PyResult<Vec<String>> {
    use crate::abi_types::AbiType;
    use crate::contract::decode_return_data as decode_impl;

    // Extract types from Python list (copy before releasing GIL)
    let output_types: Vec<String> = output_types
        .iter()
        .map(|t| t.extract::<String>())
        .collect::<Result<_, _>>()?;

    let types: Vec<AbiType> = output_types
        .iter()
        .map(|t| AbiType::parse(t).map_err(|e| crate::errors::ContractError::InvalidAbi { message: format!("{e}") }))
        .collect::<Result<Vec<_>, _>>()?;

    // Copy data before releasing GIL
    let data = data.to_vec();

    // Release GIL during pure Rust decoding work
    let result = py.detach(|| decode_impl(&data, &types))?;

    Ok(result)
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
