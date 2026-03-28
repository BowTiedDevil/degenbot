//! `PyO3` bindings for the contract module.

use crate::contract::{Contract, FunctionSignature};
use crate::provider::AlloyProvider;
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
    ///     provider: AlloyProvider instance (not implemented - uses Arc for now)
    #[new]
    fn new(address: &str) -> PyResult<Self> {
        // Create a placeholder provider - in production this would be passed in
        // For now, we'll need to implement provider passing properly
        let provider = Arc::new(
            AlloyProvider::new("http://localhost:8545", 10, 30, 10).map_err(|e| {
                PyValueError::new_err(format!("Failed to create provider: {e}"))
            })?,
        );

        let contract =
            Contract::new(address, provider).map_err(|e| PyValueError::new_err(format!("{e}")))?;

        Ok(Self { contract })
    }

    /// Execute a contract call.
    ///
    /// Args:
    ///     function_signature: Function signature like "balanceOf(address)"
    ///     args: List of arguments as strings
    ///     block_number: Optional block number to query
    ///
    /// Returns:
    ///     List of decoded return values as strings
    fn call(
        &self,
        py: Python<'_>,
        function_signature: &str,
        args: Vec<String>,
        block_number: Option<u64>,
    ) -> PyResult<Py<PyList>> {
        let handle = tokio::runtime::Handle::try_current()
            .unwrap_or_else(|_| {
                tokio::runtime::Runtime::new()
                    .expect("Failed to create tokio runtime")
                    .handle()
                    .clone()
            });

        let result = handle.block_on(async {
            self.contract.call(function_signature, &args, block_number).await
        }).map_err(|e| PyValueError::new_err(format!("Contract call failed: {e}")))?;

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
///     function_signature: Function signature like "transfer(address,uint256)"
///     args: List of arguments as strings
///
/// Returns:
///     Encoded calldata as bytes
#[pyfunction]
fn encode_function_call(function_signature: &str, args: Vec<String>) -> PyResult<Vec<u8>> {
    use crate::contract::{encode_arguments, FunctionSignature};

    let func =
        FunctionSignature::parse(function_signature).map_err(|e| PyValueError::new_err(format!("{e}")))?;

    let encoded_args =
        encode_arguments(&func.inputs, &args).map_err(|e| PyValueError::new_err(format!("{e}")))?;

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
///     output_types: List of output type strings like ["uint256", "address"]
///
/// Returns:
///     List of decoded values as strings
#[pyfunction]
fn decode_return_data(data: &[u8], output_types: Vec<String>) -> PyResult<Vec<String>> {
    use crate::contract::{decode_return_data as decode_impl, AbiType};

    let types: Vec<AbiType> = output_types
        .iter()
        .map(|t| AbiType::parse(t).map_err(|e| PyValueError::new_err(format!("{e}"))))
        .collect::<PyResult<Vec<_>>>()?;

    decode_impl(data, &types).map_err(|e| PyValueError::new_err(format!("{e}")))
}

/// Parse a function signature and return its selector.
///
/// Args:
///     function_signature: Function signature like "transfer(address,uint256)"
///
/// Returns:
///     4-byte function selector as hex string
#[pyfunction]
fn get_function_selector(function_signature: &str) -> PyResult<String> {
    let func =
        FunctionSignature::parse(function_signature).map_err(|e| PyValueError::new_err(format!("{e}")))?;
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
