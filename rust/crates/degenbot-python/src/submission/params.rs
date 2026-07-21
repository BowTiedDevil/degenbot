//! `PyTxParams` — `PyO3` wrapper over [`degenbot_submission::TxParams`].
//!
//! Python constructs this with the EIP-1559 transaction fields the caller
//! assembles (`to`/`data`=`execute()` calldata/`gas`/`nonce`/`value=0`/
//! `accessList`), then [`finalize_fees_py`] sets the fee fields, then
//! [`crate::submission::PyTxSigner::sign_eip1559`] signs it.

use crate::prelude::*;
use degenbot_submission::{finalize_fees, TxParams};
use pyo3::exceptions::PyValueError;
use pyo3::types::{PyBytes, PyList};

/// The EIP-1559 transaction field set, minus `chain_id` (owned by
/// [`PyTxSigner`](crate::submission::PyTxSigner)).
///
/// Construction:
/// ```python
/// params = PyTxParams(
///     to="0x0000...0000",
///     data=execute_calldata,           # bytes
///     gas_limit=250_000,
///     nonce=7,
///     value=0,                          # optional, default 0
///     access_list=[],                   # optional, web3 shape
/// )
/// finalize_fees(params, base_fee_next, priority_fee)
/// raw = signer.sign_eip1559(params)
/// ```
#[pyclass(
    name = "PyTxParams",
    skip_from_py_object,
    module = "degenbot._ffi.submission"
)]
pub struct PyTxParams {
    pub(crate) inner: TxParams,
}

#[pymethods]
impl PyTxParams {
    /// Create a new EIP-1559 transaction field set.
    ///
    /// Args:
    ///     to: Recipient contract address (hex string)
    ///     data: The ``execute()`` calldata (bytes)
    ///     `gas_limit`: Gas limit (units)
    ///     `nonce`: Sender account nonce
    ///     `value`: Ether value to transfer in wei (default 0)
    ///     `access_list`: EIP-2930 access list in the web3 `create_access_list`
    ///         shape — a list of ``{"address": "0x...", "storageKeys":
    ///         ["0x..."]}`` dicts. Default empty.
    #[new]
    #[pyo3(signature = (to, data, gas_limit, nonce, value=0, access_list=None))]
    fn new(
        to: &str,
        data: &Bound<'_, PyBytes>,
        gas_limit: u64,
        nonce: u64,
        value: u128,
        access_list: Option<Bound<'_, PyList>>,
    ) -> PyResult<Self> {
        let to_addr = crate::address_utils::parse_address(to)
            .map_err(|e| PyValueError::new_err(format!("Invalid `to` address: {e}")))?;
        let access_list = access_list
            .map(|lst| parse_access_list(&lst))
            .transpose()?
            .unwrap_or_default();

        let inner = TxParams {
            to: to_addr,
            data: data.as_bytes().to_vec().into(),
            gas_limit,
            nonce,
            value: alloy::primitives::U256::from(value),
            access_list,
            max_fee_per_gas: 0,
            max_priority_fee_per_gas: 0,
        };
        Ok(Self { inner })
    }

    /// The ``maxFeePerGas`` (wei) set by [`finalize_fees_py`].
    #[getter]
    const fn max_fee_per_gas(&self) -> u128 {
        self.inner.max_fee_per_gas
    }

    /// The ``maxPriorityFeePerGas`` (wei) set by [`finalize_fees_py`].
    #[getter]
    const fn max_priority_fee_per_gas(&self) -> u128 {
        self.inner.max_priority_fee_per_gas
    }
}

/// `#[pyfunction]` wrapper over [`degenbot_submission::finalize_fees`].
///
/// Sets ``params.max_priority_fee_per_gas = priority_fee`` and
/// ``params.max_fee_per_gas = int(1.5 * base_fee_next) + priority_fee``
/// (the Python oracle's inline fee computation, including the float→int
/// truncate — see the core crate's `fee` module).
///
/// Args:
///     `params`: The `PyTxParams` to finalize fees onto (mutated in place)
///     `base_fee_next`: The next-block base fee in wei
///     `priority_fee`: The priority fee in wei (consumed from the Simulation
///         leaf's `_compute_priority_fee` — submission does not compute it)
///
/// # Errors
///
/// Raises:
///     `RuntimeError`: If the fee arithmetic overflows `u128` (impossible for
///         realistic mainnet fees).
#[pyfunction(name = "finalize_fees")]
#[pyo3(signature = (params, base_fee_next, priority_fee))]
pub fn finalize_fees_py(
    params: &mut PyTxParams,
    base_fee_next: u128,
    priority_fee: u128,
) -> PyResult<()> {
    finalize_fees(&mut params.inner, base_fee_next, priority_fee).map_err(Into::<PyErr>::into)
}

/// Parse a web3-shape access list (list of ``{"address", "storageKeys"}``
/// dicts) into alloy's [`alloy::eips::eip2930::AccessList`].
///
/// This is pure arg extraction (ADR-005 §3 B): no business logic, just
/// translating the Python dict shape the `web3.eth.create_access_list` RPC
/// returns into the typed Rust `AccessList`.
pub(crate) fn parse_access_list(
    list: &Bound<'_, PyList>,
) -> PyResult<alloy::eips::eip2930::AccessList> {
    use alloy::eips::eip2930::{AccessList, AccessListItem};
    let mut items = Vec::with_capacity(list.len());
    for entry in list.iter() {
        let entry = entry.cast::<pyo3::types::PyDict>()?;
        let address_str = entry
            .get_item("address")?
            .ok_or_else(|| PyValueError::new_err("access list entry missing 'address'"))?
            .to_string();
        let address = crate::address_utils::parse_address(&address_str)
            .map_err(|e| PyValueError::new_err(format!("Invalid access list address: {e}")))?;
        let storage_keys = match entry.get_item("storageKeys")? {
            None => Vec::new(),
            Some(sk) => {
                let sk_list = sk.cast::<PyList>()?;
                sk_list
                    .iter()
                    .map(|k| {
                        let s = k.to_string();
                        let stripped = s.trim_start_matches("0x");
                        let bytes = alloy::hex::decode(stripped).map_err(|e| {
                            PyValueError::new_err(format!("Invalid storage key '{s}': {e}"))
                        })?;
                        if bytes.len() != 32 {
                            return Err(PyValueError::new_err(format!(
                                "Invalid storage key '{s}': expected 32 bytes, got {}",
                                bytes.len()
                            )));
                        }
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(&bytes);
                        Ok(alloy::primitives::B256::from(arr))
                    })
                    .collect::<PyResult<Vec<_>>>()?
            }
        };
        items.push(AccessListItem {
            address,
            storage_keys,
        });
    }
    Ok(AccessList(items))
}
