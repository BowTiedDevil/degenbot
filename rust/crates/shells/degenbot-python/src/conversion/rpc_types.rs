//! Python object converters for Ethereum RPC types.
//!
//! Converts Alloy RPC response types (blocks, transactions, logs) and JSON-RPC
//! responses into Python dicts with field-aware `HexBytes`, checksummed addresses,
//! and integer conversion.
//!
//! # GIL Requirement
//!
//! All public functions in this module require the GIL because they create Python
//! objects. This is inherently necessary — Python objects cannot be constructed
//! without holding the GIL. The actual RPC I/O that precedes these calls already
//! releases the GIL (via `py.detach()` in the provider layer).
//!
//! For large blocks with many transactions, these converters will hold the GIL
//! for the duration of Python object construction. This is a known and accepted
//! cost. An alternative would be lazy field-by-field access via `__getattr__`,
//! but the eager-conversion approach is simpler and matches the web3.py convention.

use alloy::consensus::{
    Header as ConsensusHeader, TxEip1559, TxEip2930, TxEip4844, TxEip4844Variant, TxEip7702,
    TxEnvelope, TxLegacy,
};
use alloy::eips::eip4895::Withdrawal;
use alloy::network::primitives::BlockTransactions;
use alloy::primitives::{Address, TxKind, B256, U256};
use alloy::rpc::types::eth::Transaction;
use alloy::rpc::types::Log;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::address_utils::address_to_checksum_string;
use crate::conversion::cache::to_py_bytes;
use crate::hex_utils::decode_hex;
#[cfg(feature = "rpc")]
use crate::provider::EthBlock;
use alloy::rpc::types::eth::Header as RpcHeader;

/// How one JSON-RPC response field decodes into Python.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// `0x`-hex byte string → `HexBytes`.
    HexBytes,
    /// 20-byte `0x` address → EIP-55 checksummed `str`.
    Address,
    /// JSON-RPC `QUANTITY` (`0x`-hex, leading zeros dropped) → `int`.
    Numeric,
}

/// One RPC method's response shape: its field names and the decode each needs.
///
/// The decode is declared per method, never as one name table shared by every
/// call: a field name means what the method's own response says it means, and
/// a field the method does not declare crosses as the JSON's natural Python
/// type (never a guessed conversion, and never an error — an unmodeled method
/// has no shape to check a name against).
#[derive(Clone, Copy, Debug)]
pub struct FieldSchema(&'static [(&'static str, FieldKind)]);

/// The transaction object's fields (`eth_getTransactionByHash` and its
/// block-indexed siblings, and a pending-transaction subscription).
const TRANSACTION_FIELDS: &[(&str, FieldKind)] = &[
    ("hash", FieldKind::HexBytes),
    ("block_hash", FieldKind::HexBytes),
    ("input", FieldKind::HexBytes),
    ("nonce", FieldKind::HexBytes),
    ("r", FieldKind::HexBytes),
    ("s", FieldKind::HexBytes),
    ("blob_versioned_hashes", FieldKind::HexBytes),
    ("from", FieldKind::Address),
    ("to", FieldKind::Address),
    ("block_number", FieldKind::Numeric),
    ("transaction_index", FieldKind::Numeric),
    ("chain_id", FieldKind::Numeric),
    ("gas", FieldKind::Numeric),
    ("gas_price", FieldKind::Numeric),
    ("max_fee_per_gas", FieldKind::Numeric),
    ("max_priority_fee_per_gas", FieldKind::Numeric),
    ("max_fee_per_blob_gas", FieldKind::Numeric),
    ("value", FieldKind::Numeric),
];

/// The transaction receipt's fields, including the log rows it carries.
const RECEIPT_FIELDS: &[(&str, FieldKind)] = &[
    ("transaction_hash", FieldKind::HexBytes),
    ("block_hash", FieldKind::HexBytes),
    ("contract_address", FieldKind::HexBytes),
    ("logs_bloom", FieldKind::HexBytes),
    ("topics", FieldKind::HexBytes),
    ("data", FieldKind::HexBytes),
    ("from", FieldKind::Address),
    ("to", FieldKind::Address),
    ("address", FieldKind::Address),
    ("block_number", FieldKind::Numeric),
    ("transaction_index", FieldKind::Numeric),
    ("log_index", FieldKind::Numeric),
    ("status", FieldKind::Numeric),
    ("cumulative_gas_used", FieldKind::Numeric),
    ("effective_gas_price", FieldKind::Numeric),
    ("gas_used", FieldKind::Numeric),
    ("transaction_type", FieldKind::Numeric),
    ("blob_gas_used", FieldKind::Numeric),
];

/// A block's fields, including the nested transactions and log rows it
/// carries (`eth_getBlockBy*`).
const BLOCK_FIELDS: &[(&str, FieldKind)] = &[
    ("hash", FieldKind::HexBytes),
    ("parent_hash", FieldKind::HexBytes),
    ("transactions_root", FieldKind::HexBytes),
    ("receipts_root", FieldKind::HexBytes),
    ("state_root", FieldKind::HexBytes),
    ("logs_bloom", FieldKind::HexBytes),
    ("mix_hash", FieldKind::HexBytes),
    ("sha3_uncles", FieldKind::HexBytes),
    ("extra_data", FieldKind::HexBytes),
    ("withdrawals_root", FieldKind::HexBytes),
    ("block_hash", FieldKind::HexBytes),
    ("input", FieldKind::HexBytes),
    ("r", FieldKind::HexBytes),
    ("s", FieldKind::HexBytes),
    ("blob_versioned_hashes", FieldKind::HexBytes),
    ("transaction_hash", FieldKind::HexBytes),
    ("contract_address", FieldKind::HexBytes),
    ("topics", FieldKind::HexBytes),
    ("data", FieldKind::HexBytes),
    ("miner", FieldKind::Address),
    ("address", FieldKind::Address),
    ("from", FieldKind::Address),
    ("to", FieldKind::Address),
    ("number", FieldKind::Numeric),
    ("timestamp", FieldKind::Numeric),
    ("gas_used", FieldKind::Numeric),
    ("gas_limit", FieldKind::Numeric),
    ("base_fee_per_gas", FieldKind::Numeric),
    ("blob_gas_used", FieldKind::Numeric),
    ("excess_blob_gas", FieldKind::Numeric),
    ("difficulty", FieldKind::Numeric),
    ("total_difficulty", FieldKind::Numeric),
    ("size", FieldKind::Numeric),
    ("chain_id", FieldKind::Numeric),
    ("gas_price", FieldKind::Numeric),
    ("max_fee_per_gas", FieldKind::Numeric),
    ("max_priority_fee_per_gas", FieldKind::Numeric),
    ("max_fee_per_blob_gas", FieldKind::Numeric),
    ("value", FieldKind::Numeric),
    ("gas", FieldKind::Numeric),
    ("cumulative_gas_used", FieldKind::Numeric),
    ("effective_gas_price", FieldKind::Numeric),
    ("status", FieldKind::Numeric),
    ("transaction_type", FieldKind::Numeric),
    ("block_number", FieldKind::Numeric),
    ("log_index", FieldKind::Numeric),
    ("transaction_index", FieldKind::Numeric),
];

/// A bundle/simulation call result's fields (`eth_callMany`,
/// `eth_simulateV1`), including the log rows each call carries.
const SIMULATION_FIELDS: &[(&str, FieldKind)] = &[
    ("status", FieldKind::Numeric),
    ("gasUsed", FieldKind::Numeric),
    ("returnData", FieldKind::HexBytes),
    ("topics", FieldKind::HexBytes),
    ("data", FieldKind::HexBytes),
    ("address", FieldKind::Address),
];

impl FieldSchema {
    /// A method this seam does not model: no field is declared, so every value
    /// crosses as the JSON's natural Python type.
    pub const RAW: Self = Self(&[]);

    /// A transaction object.
    pub const TRANSACTION: Self = Self(TRANSACTION_FIELDS);

    /// A transaction receipt (log rows included).
    pub const RECEIPT: Self = Self(RECEIPT_FIELDS);

    /// A block (nested transactions and log rows included).
    pub const BLOCK: Self = Self(BLOCK_FIELDS);

    /// A bundle/simulation call result.
    pub const SIMULATION: Self = Self(SIMULATION_FIELDS);

    /// The declared shape of `method`'s response.
    ///
    /// The typed getters pass their own constant; the raw `make_request`
    /// surface resolves the method name here so a caller of a modelled method
    /// gets the same decode the typed getter would, and a caller of anything
    /// else gets [`Self::RAW`].
    #[must_use]
    pub fn for_method(method: &str) -> Self {
        match method {
            "eth_getTransactionByHash"
            | "eth_getTransactionByBlockHashAndIndex"
            | "eth_getTransactionByBlockNumberAndIndex" => Self::TRANSACTION,
            "eth_getTransactionReceipt" | "eth_getBlockReceipts" => Self::RECEIPT,
            "eth_getBlockByHash" | "eth_getBlockByNumber" => Self::BLOCK,
            "eth_callMany" | "eth_simulateV1" => Self::SIMULATION,
            _ => Self::RAW,
        }
    }

    /// The declared decode for `field`, or `None` when this shape says nothing
    /// about it.
    fn kind(&self, field: &str) -> Option<FieldKind> {
        self.0
            .iter()
            .find(|(name, _)| *name == field)
            .map(|(_, kind)| *kind)
    }
}

/// Check if a string value should be converted to `HexBytes`.
/// Returns true if the string starts with "0x" and contains valid hex characters.
fn is_hex_string(s: &str) -> bool {
    if s.len() < 3 {
        return false;
    }
    s.starts_with("0x") && s[2..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Convert a hex string to a `HexBytes` object.
fn hex_to_hexbytes<'py>(py: Python<'py>, hex_str: &str) -> PyResult<Bound<'py, PyAny>> {
    let bytes = decode_hex(hex_str)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("Invalid hex string: {e}")))?;
    to_py_bytes(py, &bytes)
}

/// Convert a hex string to a checksummed address string.
fn hex_to_checksum_address(hex_str: &str) -> PyResult<String> {
    let bytes = decode_hex(hex_str).map_err(|msg| {
        pyo3::exceptions::PyValueError::new_err(format!("Invalid hex string: {msg}"))
    })?;
    if bytes.len() != 20 {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "Address must be exactly 20 bytes, got {} bytes",
            bytes.len()
        )));
    }
    let address = Address::from_slice(&bytes);
    Ok(address_to_checksum_string(&address))
}

/// Convert a single Alloy `Log` to a Python dict.
///
/// This is the shared implementation used by both sync and async provider log conversion.
/// Accesses raw bytes directly from Alloy types (no hex decode round-trip).
#[expect(clippy::missing_errors_doc)]
pub fn log_to_py_dict<'py>(py: Python<'py>, log: &Log) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);

    // address: convert to checksummed string
    let address = log.address();
    dict.set_item("address", address_to_checksum_string(&address))?;

    // topics: list of B256 hashes as HexBytes
    let topics_list = PyList::empty(py);
    for topic in log.topics() {
        let topic_hb = to_py_bytes(py, topic.as_ref())?;
        topics_list.append(topic_hb)?;
    }
    dict.set_item("topics", topics_list)?;

    // data: dynamic bytes as HexBytes
    let data = &log.data().data;
    let data_hb = to_py_bytes(py, data)?;
    dict.set_item("data", data_hb)?;

    // blockNumber as int
    dict.set_item("blockNumber", log.block_number)?;

    // blockHash as HexBytes (optional)
    if let Some(block_hash) = log.block_hash {
        let block_hash_hb = to_py_bytes(py, block_hash.as_ref())?;
        dict.set_item("blockHash", block_hash_hb)?;
    } else {
        dict.set_item("blockHash", py.None())?;
    }

    // transactionHash as HexBytes (optional)
    if let Some(tx_hash) = log.transaction_hash {
        let tx_hash_hb = to_py_bytes(py, tx_hash.as_ref())?;
        dict.set_item("transactionHash", tx_hash_hb)?;
    } else {
        dict.set_item("transactionHash", py.None())?;
    }

    // transactionIndex as int (optional)
    dict.set_item("transactionIndex", log.transaction_index)?;

    // logIndex as int
    dict.set_item("logIndex", log.log_index)?;

    // removed (Geth compatibility: whether this log was removed via reorg)
    dict.set_item("removed", log.removed)?;

    Ok(dict)
}

/// Convert a `serde_json::Value` to a Python object, decoding the fields
/// `schema` declares.
///
/// Single-pass conversion that builds Python objects directly from JSON. The
/// field kinds come from the caller's [`FieldSchema`] — the response shape of
/// the method this value came from — so one method's field name is never
/// decoded by another method's rule.
///
/// - `null` → `None`
/// - `bool` → `bool`
/// - `number` → `int` or `float`
/// - `string` → `str`, or `HexBytes`/checksummed address/`int` where the
///   schema declares that field kind and the value is a hex string
/// - `array` → `list` (elements inherit the array's field name, so a declared
///   list of hashes decodes element-wise)
/// - `object` → `dict` (keys are looked up in the same schema)
///
/// # Errors
///
/// Returns `PyValueError` if hex string conversion fails for a declared field.
pub fn json_to_py_with_hexbytes(
    py: Python<'_>,
    value: serde_json::Value,
    schema: FieldSchema,
) -> PyResult<Bound<'_, PyAny>> {
    json_to_py_inner(py, value, schema, None)
}

/// Single-pass recursive JSON-to-Python conversion with field-aware hex handling.
fn json_to_py_inner<'py>(
    py: Python<'py>,
    value: serde_json::Value,
    schema: FieldSchema,
    field_name: Option<&str>,
) -> PyResult<Bound<'py, PyAny>> {
    match value {
        serde_json::Value::Null => Ok(py.None().into_bound(py)),
        serde_json::Value::Bool(b) => {
            let py_bool = b.into_pyobject(py)?;
            Ok(Bound::clone(&py_bool).into_any())
        }
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i.into_pyobject(py)?.into_any())
            } else if let Some(u) = n.as_u64() {
                Ok(u.into_pyobject(py)?.into_any())
            } else if let Some(f) = n.as_f64() {
                Ok(f.into_pyobject(py)?.into_any())
            } else {
                Ok(py.None().into_bound(py))
            }
        }
        serde_json::Value::String(s) => {
            let kind = field_name.and_then(|name| schema.kind(name));
            // Address fields decode to a checksummed string.
            if kind == Some(FieldKind::Address) && is_hex_string(&s) {
                let checksummed = hex_to_checksum_address(&s)?;
                return Ok(checksummed.into_pyobject(py)?.into_any());
            }
            // Hash/data fields decode to `HexBytes`.
            if kind == Some(FieldKind::HexBytes) && is_hex_string(&s) {
                return hex_to_hexbytes(py, &s).map(Bound::into_any);
            }
            // Quantity fields decode to `int`.
            if kind == Some(FieldKind::Numeric) && is_hex_string(&s) {
                // Use the odd-length-tolerant decode_hex (JSON-RPC QUANTITY
                // values routinely drop leading zeros, producing single-digit
                // hex like "0x1"/"0x0" for e.g. eth_simulateV1 call `status`).
                let bytes = decode_hex(&s).map_err(|e| {
                    pyo3::exceptions::PyValueError::new_err(format!("Invalid hex number: {e}"))
                })?;
                // For values up to 32 bytes, use U256 conversion
                // For larger values, use Python's int.from_bytes
                if bytes.len() <= 32 {
                    let mut arr = [0u8; 32];
                    arr[32 - bytes.len()..].copy_from_slice(&bytes);
                    let u256 = U256::from_be_bytes(arr);
                    return crate::conversion::alloy::u256_to_py(py, &u256);
                }
                // Fall back to Python's int.from_bytes for very large values
                return crate::conversion::cache::bytes_to_int(py, &bytes).map(Bound::into_any);
            }
            Ok(s.into_pyobject(py)?.into_any())
        }
        serde_json::Value::Array(arr) => {
            let py_list = PyList::empty(py);
            for item in arr {
                py_list.append(json_to_py_inner(py, item, schema, field_name)?)?;
            }
            Ok(py_list.into_any())
        }
        serde_json::Value::Object(map) => {
            let py_dict = PyDict::new(py);
            for (key, val) in map {
                py_dict.set_item(&key, json_to_py_inner(py, val, schema, Some(&key))?)?;
            }
            Ok(py_dict.into_any())
        }
    }
}

/// Convert U256 to Python int.
///
/// # Errors
fn set_opt_u64(dict: &Bound<'_, PyDict>, key: &str, val: Option<u64>) -> PyResult<()> {
    val.map_or_else(
        || dict.set_item(key, dict.py().None()),
        |v| dict.set_item(key, v),
    )
}

fn set_opt_u128(dict: &Bound<'_, PyDict>, key: &str, val: Option<u128>) -> PyResult<()> {
    val.map_or_else(
        || dict.set_item(key, dict.py().None()),
        |v| dict.set_item(key, v),
    )
}

fn set_opt_u256(dict: &Bound<'_, PyDict>, key: &str, val: Option<&U256>) -> PyResult<()> {
    val.map_or_else(
        || dict.set_item(key, dict.py().None()),
        |v| {
            let int_val = crate::conversion::alloy::u256_to_py(dict.py(), v)?;
            dict.set_item(key, int_val)
        },
    )
}

fn set_opt_b256(dict: &Bound<'_, PyDict>, key: &str, val: Option<B256>) -> PyResult<()> {
    match val {
        Some(v) => {
            let hb = to_py_bytes(dict.py(), v.as_ref())?;
            dict.set_item(key, hb)
        }
        None => dict.set_item(key, dict.py().None()),
    }
}

fn tx_kind_to_py<'py>(py: Python<'py>, kind: &TxKind) -> PyResult<Bound<'py, PyAny>> {
    match kind {
        TxKind::Call(addr) => Ok(address_to_checksum_string(addr)
            .into_pyobject(py)?
            .into_any()),
        TxKind::Create => Ok(py.None().into_bound(py)),
    }
}

fn access_list_to_py<'py>(
    py: Python<'py>,
    access_list: &alloy::eips::eip2930::AccessList,
) -> PyResult<Bound<'py, PyList>> {
    let py_list = PyList::empty(py);
    for item in access_list.iter() {
        let item_dict = PyDict::new(py);
        item_dict.set_item("address", address_to_checksum_string(&item.address))?;
        let storage_keys = PyList::empty(py);
        for key in &item.storage_keys {
            storage_keys.append(to_py_bytes(py, key.as_ref())?)?;
        }
        item_dict.set_item("storageKeys", storage_keys)?;
        py_list.append(item_dict)?;
    }
    Ok(py_list)
}

fn blob_hashes_to_py<'py>(
    py: Python<'py>,
    hashes: &[alloy::primitives::B256],
) -> PyResult<Bound<'py, PyList>> {
    let py_list = PyList::empty(py);
    for hash in hashes {
        py_list.append(to_py_bytes(py, hash.as_ref())?)?;
    }
    Ok(py_list)
}

fn set_signature_fields(
    dict: &Bound<'_, PyDict>,
    sig: &alloy::primitives::Signature,
    tx_type: u8,
) -> PyResult<()> {
    let py = dict.py();

    let r_bytes: [u8; 32] = sig.r().to_be_bytes();
    let s_bytes: [u8; 32] = sig.s().to_be_bytes();
    dict.set_item("r", to_py_bytes(py, &r_bytes)?)?;
    dict.set_item("s", to_py_bytes(py, &s_bytes)?)?;

    let v: u64 = u64::from(sig.v());
    dict.set_item("v", v)?;

    if tx_type != 0 {
        let y_parity: bool = sig.v();
        dict.set_item("y_parity", y_parity)?;
    }

    Ok(())
}

fn set_legacy_tx_fields(dict: &Bound<'_, PyDict>, tx: &TxLegacy) -> PyResult<()> {
    let py = dict.py();

    if let Some(chain_id) = tx.chain_id {
        dict.set_item("chain_id", chain_id)?;
    } else {
        dict.set_item("chain_id", py.None())?;
    }
    dict.set_item("nonce", tx.nonce)?;
    dict.set_item("gas_price", tx.gas_price)?;
    dict.set_item("gas", tx.gas_limit)?;
    dict.set_item("to", tx_kind_to_py(py, &tx.to)?)?;
    dict.set_item(
        "value",
        crate::conversion::alloy::u256_to_py(py, &tx.value)?,
    )?;
    dict.set_item("input", to_py_bytes(py, &tx.input)?)?;

    Ok(())
}

fn set_eip2930_tx_fields(dict: &Bound<'_, PyDict>, tx: &TxEip2930) -> PyResult<()> {
    let py = dict.py();

    dict.set_item("chain_id", tx.chain_id)?;
    dict.set_item("nonce", tx.nonce)?;
    dict.set_item("gas_price", tx.gas_price)?;
    dict.set_item("gas", tx.gas_limit)?;
    dict.set_item("to", tx_kind_to_py(py, &tx.to)?)?;
    dict.set_item(
        "value",
        crate::conversion::alloy::u256_to_py(py, &tx.value)?,
    )?;
    dict.set_item("input", to_py_bytes(py, &tx.input)?)?;
    dict.set_item("access_list", access_list_to_py(py, &tx.access_list)?)?;

    Ok(())
}

fn set_eip1559_tx_fields(dict: &Bound<'_, PyDict>, tx: &TxEip1559) -> PyResult<()> {
    let py = dict.py();

    dict.set_item("chain_id", tx.chain_id)?;
    dict.set_item("nonce", tx.nonce)?;
    dict.set_item("max_fee_per_gas", tx.max_fee_per_gas)?;
    dict.set_item("max_priority_fee_per_gas", tx.max_priority_fee_per_gas)?;
    dict.set_item("gas", tx.gas_limit)?;
    dict.set_item("to", tx_kind_to_py(py, &tx.to)?)?;
    dict.set_item(
        "value",
        crate::conversion::alloy::u256_to_py(py, &tx.value)?,
    )?;
    dict.set_item("input", to_py_bytes(py, &tx.input)?)?;
    dict.set_item("access_list", access_list_to_py(py, &tx.access_list)?)?;

    Ok(())
}

fn set_eip4844_tx_fields(dict: &Bound<'_, PyDict>, tx: &TxEip4844) -> PyResult<()> {
    let py = dict.py();

    dict.set_item("chain_id", tx.chain_id)?;
    dict.set_item("nonce", tx.nonce)?;
    dict.set_item("max_fee_per_gas", tx.max_fee_per_gas)?;
    dict.set_item("max_priority_fee_per_gas", tx.max_priority_fee_per_gas)?;
    dict.set_item("gas", tx.gas_limit)?;
    dict.set_item("to", address_to_checksum_string(&tx.to))?;
    dict.set_item(
        "value",
        crate::conversion::alloy::u256_to_py(py, &tx.value)?,
    )?;
    dict.set_item("input", to_py_bytes(py, &tx.input)?)?;
    dict.set_item("access_list", access_list_to_py(py, &tx.access_list)?)?;
    dict.set_item("max_fee_per_blob_gas", tx.max_fee_per_blob_gas)?;
    dict.set_item(
        "blob_versioned_hashes",
        blob_hashes_to_py(py, &tx.blob_versioned_hashes)?,
    )?;

    Ok(())
}

fn set_eip7702_tx_fields(dict: &Bound<'_, PyDict>, tx: &TxEip7702) -> PyResult<()> {
    let py = dict.py();

    dict.set_item("chain_id", tx.chain_id)?;
    dict.set_item("nonce", tx.nonce)?;
    dict.set_item("max_fee_per_gas", tx.max_fee_per_gas)?;
    dict.set_item("max_priority_fee_per_gas", tx.max_priority_fee_per_gas)?;
    dict.set_item("gas", tx.gas_limit)?;
    dict.set_item("to", address_to_checksum_string(&tx.to))?;
    dict.set_item(
        "value",
        crate::conversion::alloy::u256_to_py(py, &tx.value)?,
    )?;
    dict.set_item("input", to_py_bytes(py, &tx.input)?)?;
    dict.set_item("access_list", access_list_to_py(py, &tx.access_list)?)?;

    let auth_list = PyList::empty(py);
    for auth in &tx.authorization_list {
        let auth_dict = PyDict::new(py);
        auth_dict.set_item(
            "chain_id",
            crate::conversion::alloy::u256_to_py(py, auth.chain_id())?,
        )?;
        auth_dict.set_item("address", address_to_checksum_string(auth.address()))?;
        auth_dict.set_item("nonce", auth.nonce())?;
        let r_bytes: [u8; 32] = auth.r().to_be_bytes();
        let s_bytes: [u8; 32] = auth.s().to_be_bytes();
        auth_dict.set_item("r", to_py_bytes(py, &r_bytes)?)?;
        auth_dict.set_item("s", to_py_bytes(py, &s_bytes)?)?;
        auth_dict.set_item("v", auth.y_parity())?;
        auth_list.append(auth_dict)?;
    }
    dict.set_item("authorization_list", auth_list)?;

    Ok(())
}

fn transaction_to_py_dict<'py>(
    py: Python<'py>,
    tx: &Transaction<TxEnvelope>,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);

    let recovered = tx.as_recovered();
    let from_addr = recovered.signer();
    let envelope = recovered.inner();

    let tx_type: u8 = envelope.tx_type().into();

    dict.set_item("hash", to_py_bytes(py, envelope.hash().as_ref())?)?;
    dict.set_item("type", tx_type)?;
    dict.set_item("from", address_to_checksum_string(&from_addr))?;

    set_opt_u64(&dict, "block_number", tx.block_number)?;
    set_opt_b256(&dict, "block_hash", tx.block_hash)?;
    set_opt_u64(&dict, "transaction_index", tx.transaction_index)?;
    set_opt_u128(&dict, "effective_gas_price", tx.effective_gas_price)?;
    set_opt_u64(&dict, "block_timestamp", tx.block_timestamp)?;

    match envelope {
        TxEnvelope::Legacy(signed_tx) => {
            set_legacy_tx_fields(&dict, signed_tx.tx())?;
            set_signature_fields(&dict, signed_tx.signature(), 0)?;
        }
        TxEnvelope::Eip2930(signed_tx) => {
            set_eip2930_tx_fields(&dict, signed_tx.tx())?;
            set_signature_fields(&dict, signed_tx.signature(), 1)?;
        }
        TxEnvelope::Eip1559(signed_tx) => {
            set_eip1559_tx_fields(&dict, signed_tx.tx())?;
            set_signature_fields(&dict, signed_tx.signature(), 2)?;
        }
        TxEnvelope::Eip4844(signed_tx) => {
            match signed_tx.tx() {
                TxEip4844Variant::TxEip4844(eip4844_tx) => {
                    set_eip4844_tx_fields(&dict, eip4844_tx)?;
                }
                TxEip4844Variant::TxEip4844WithSidecar(tx_with_sidecar) => {
                    set_eip4844_tx_fields(&dict, tx_with_sidecar.tx())?;
                }
            }
            set_signature_fields(&dict, signed_tx.signature(), 3)?;
        }
        TxEnvelope::Eip7702(signed_tx) => {
            set_eip7702_tx_fields(&dict, signed_tx.tx())?;
            set_signature_fields(&dict, signed_tx.signature(), 4)?;
        } // NOTE: When alloy adds new TxEnvelope variants, add match arms here.
          // The compiler will error on exhaustive match to alert you.
    }

    Ok(dict)
}

fn consensus_header_to_py_dict<'py>(
    py: Python<'py>,
    header: &ConsensusHeader,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);

    dict.set_item("parent_hash", to_py_bytes(py, header.parent_hash.as_ref())?)?;
    dict.set_item("sha3_uncles", to_py_bytes(py, header.ommers_hash.as_ref())?)?;
    dict.set_item("miner", address_to_checksum_string(&header.beneficiary))?;
    dict.set_item("state_root", to_py_bytes(py, header.state_root.as_ref())?)?;
    dict.set_item(
        "transactions_root",
        to_py_bytes(py, header.transactions_root.as_ref())?,
    )?;
    dict.set_item(
        "receipts_root",
        to_py_bytes(py, header.receipts_root.as_ref())?,
    )?;
    dict.set_item("logs_bloom", to_py_bytes(py, header.logs_bloom.as_ref())?)?;
    dict.set_item(
        "difficulty",
        crate::conversion::alloy::u256_to_py(py, &header.difficulty)?,
    )?;
    dict.set_item("number", header.number)?;
    dict.set_item("gas_limit", header.gas_limit)?;
    dict.set_item("gas_used", header.gas_used)?;
    dict.set_item("timestamp", header.timestamp)?;
    dict.set_item("extra_data", to_py_bytes(py, &header.extra_data)?)?;
    dict.set_item("mix_hash", to_py_bytes(py, header.mix_hash.as_ref())?)?;
    dict.set_item("nonce", to_py_bytes(py, header.nonce.as_ref())?)?;

    set_opt_u64(&dict, "base_fee_per_gas", header.base_fee_per_gas)?;
    set_opt_b256(&dict, "withdrawals_root", header.withdrawals_root)?;
    set_opt_u64(&dict, "blob_gas_used", header.blob_gas_used)?;
    set_opt_u64(&dict, "excess_blob_gas", header.excess_blob_gas)?;
    set_opt_b256(
        &dict,
        "parent_beacon_block_root",
        header.parent_beacon_block_root,
    )?;
    set_opt_b256(&dict, "requests_hash", header.requests_hash)?;
    set_opt_b256(
        &dict,
        "block_access_list_hash",
        header.block_access_list_hash,
    )?;
    set_opt_u64(&dict, "slot_number", header.slot_number)?;

    Ok(dict)
}

fn withdrawal_to_py_dict<'py>(
    py: Python<'py>,
    withdrawal: &Withdrawal,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("index", withdrawal.index)?;
    dict.set_item("validator_index", withdrawal.validator_index)?;
    dict.set_item("address", address_to_checksum_string(&withdrawal.address))?;
    dict.set_item("amount", withdrawal.amount)?;
    Ok(dict)
}

/// Convert an RPC block header to a Python dict.
///
/// Used by the subscription pump for `subscribe_blocks()` events.
#[expect(clippy::missing_errors_doc)]
pub fn header_to_py_dict<'py>(
    py: Python<'py>,
    header: &RpcHeader<ConsensusHeader>,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);

    dict.set_item("hash", to_py_bytes(py, header.hash.as_ref())?)?;

    let inner_dict = consensus_header_to_py_dict(py, &header.inner)?;
    for (key, val) in inner_dict.iter() {
        dict.set_item(&key, val)?;
    }

    set_opt_u256(&dict, "total_difficulty", header.total_difficulty.as_ref())?;
    set_opt_u256(&dict, "size", header.size.as_ref())?;

    Ok(dict)
}

#[cfg(feature = "rpc")]
#[expect(clippy::missing_errors_doc)]
pub fn block_to_py_dict<'py>(py: Python<'py>, block: &EthBlock) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);

    dict.set_item("hash", to_py_bytes(py, block.header.hash.as_ref())?)?;

    let inner_dict = consensus_header_to_py_dict(py, &block.header.inner)?;
    for (key, val) in inner_dict.iter() {
        dict.set_item(&key, val)?;
    }

    set_opt_u256(
        &dict,
        "total_difficulty",
        block.header.total_difficulty.as_ref(),
    )?;
    set_opt_u256(&dict, "size", block.header.size.as_ref())?;

    let uncles_list = PyList::empty(py);
    for uncle in &block.uncles {
        uncles_list.append(to_py_bytes(py, uncle.as_ref())?)?;
    }
    dict.set_item("uncles", uncles_list)?;

    match &block.transactions {
        BlockTransactions::Full(txs) => {
            let tx_list = PyList::empty(py);
            for tx in txs {
                tx_list.append(transaction_to_py_dict(py, tx)?)?;
            }
            dict.set_item("transactions", tx_list)?;
        }
        BlockTransactions::Hashes(hashes) => {
            let hash_list = PyList::empty(py);
            for hash in hashes {
                hash_list.append(to_py_bytes(py, hash.as_ref())?)?;
            }
            dict.set_item("transactions", hash_list)?;
        }
        BlockTransactions::Uncle => {
            dict.set_item("transactions", py.None())?;
        }
    }

    if let Some(withdrawals) = &block.withdrawals {
        let w_list = PyList::empty(py);
        for w in withdrawals {
            w_list.append(withdrawal_to_py_dict(py, w)?)?;
        }
        dict.set_item("withdrawals", w_list)?;
    } else {
        dict.set_item("withdrawals", py.None())?;
    }

    Ok(dict)
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use pyo3::types::PyAnyMethods;

    /// Regression: JSON-RPC `QUANTITY` values may drop leading zeros,
    /// producing odd-length hex strings like `"0x1"` / `"0x0"` for the
    /// `status` field in an `eth_simulateV1` call result. The field-aware
    /// converter must turn these into Python ints rather than raising
    /// `ValueError: Invalid hex number: odd number of digits`.
    #[test]
    fn test_numeric_field_odd_length_hex_quantity() {
        Python::attach(|py| {
            // eth_simulateV1 result shape: {calls: [{status: "0x1", ...}]}
            let value = serde_json::json!({
                "calls": [
                    {"status": "0x1", "gasUsed": "0x5208", "returnData": "0x"}
                ]
            });
            let py_obj = json_to_py_with_hexbytes(py, value, FieldSchema::SIMULATION).unwrap();
            let dict = py_obj.cast::<PyDict>().unwrap();
            let calls = dict.get_item("calls").unwrap().unwrap();
            let first = calls.get_item(0).unwrap();
            let first = first.cast::<PyDict>().unwrap();
            let status = first.get_item("status").unwrap().unwrap();
            assert_eq!(status.extract::<i64>().unwrap(), 1);

            // status "0x0" -> 0
            let value = serde_json::json!({"status": "0x0"});
            let py_obj = json_to_py_with_hexbytes(py, value, FieldSchema::SIMULATION).unwrap();
            let d = py_obj.cast::<PyDict>().unwrap();
            assert_eq!(
                d.get_item("status")
                    .unwrap()
                    .unwrap()
                    .extract::<i64>()
                    .unwrap(),
                0
            );
        });
    }
}
