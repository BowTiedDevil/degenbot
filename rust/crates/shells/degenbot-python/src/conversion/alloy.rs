//! `PyO3` conversions for Alloy primitive types.
//!
//! Direct Python conversions for Alloy's `U256` and `I256`, plus
//! `abi_value_from_python` for converting arbitrary Python objects into
//! `AbiValue` enums for ABI encoding.
//!
//! # GIL Safety
//!
//! `Python::attach()` calls in this module run on Tokio worker threads after
//! async RPC calls complete. These may block briefly while the Python event
//! loop or another Python thread holds the GIL. This cannot deadlock — see
//! `rpc::async_provider.rs` module-level comment for the full reasoning.
//!
//! # Architecture
//!
//! The newtype wrapper pattern works around Rust's orphan rules,
//! which prevent implementing foreign traits (`IntoPyObject` from `pyo3`)
//! for foreign types (`U256`/`I256` from `alloy`).

#[cfg(feature = "abi")]
use crate::abi_types::type_::BitWidth;
#[cfg(feature = "abi")]
use crate::abi_types::value::AbiValue;
use crate::conversion::cache::{bytes_to_int, bytes_to_int_signed};
use alloy::primitives::{I256, U256};
use pyo3::types::{PyAny, PyBool, PyBytes, PyDict, PyList, PyString, PyTuple};
use pyo3::{exceptions::PyValueError, prelude::*, PyTypeInfo};
use std::str::FromStr;

/// Wrapper for Alloy `U256` that implements `IntoPyObject`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PyU256(pub U256);

impl<'py> IntoPyObject<'py> for PyU256 {
    type Target = PyAny;
    type Output = Bound<'py, PyAny>;
    type Error = pyo3::PyErr;

    fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
        bytes_to_int(py, &self.0.to_be_bytes::<32>())
    }
}

/// Wrapper for Alloy `I256` that implements `IntoPyObject`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PyI256(pub I256);

impl<'py> IntoPyObject<'py> for PyI256 {
    type Target = PyAny;
    type Output = Bound<'py, PyAny>;
    type Error = pyo3::PyErr;

    fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
        bytes_to_int_signed(py, &self.0.to_be_bytes::<32>())
    }
}

/// Convert a `U256` to a Python `int`.
///
/// # Errors
///
/// Returns an error if the Python conversion fails.
pub fn u256_to_py<'py>(py: Python<'py>, val: &U256) -> PyResult<Bound<'py, PyAny>> {
    PyU256(*val).into_pyobject(py)
}

/// Convert an `I256` to a Python `int`.
///
/// # Errors
///
/// Returns an error if the Python conversion fails.
pub fn i256_to_py<'py>(py: Python<'py>, val: &I256) -> PyResult<Bound<'py, PyAny>> {
    PyI256(*val).into_pyobject(py)
}

/// Extract a `U256` from a Python object.
///
/// # Errors
///
/// Returns `PyValueError` if the object cannot be converted to `U256`,
/// or if the integer is negative, or if the value exceeds 256 bits.
pub(crate) fn extract_python_u256(obj: &Bound<'_, PyAny>) -> PyResult<U256> {
    if let Ok(int_val) = obj.extract::<i128>() {
        if int_val < 0 {
            return Err(PyValueError::new_err("Value cannot be negative"));
        }
        return Ok(U256::from(int_val.cast_unsigned()));
    }

    let int_type = obj.py().import("builtins")?.getattr("int")?;
    if obj.is_instance(&int_type)? {
        let kwargs = PyDict::new(obj.py());
        kwargs.set_item("signed", false)?;
        let bytes = obj.call_method("to_bytes", (32, "big"), Some(&kwargs))?;
        let bytes: &[u8] = bytes.extract()?;
        return U256::try_from_be_slice(bytes)
            .ok_or_else(|| PyValueError::new_err("Value is too large (exceeds 256 bits)"));
    }

    if let Ok(bytes) = obj.extract::<&[u8]>() {
        return U256::try_from_be_slice(bytes)
            .ok_or_else(|| PyValueError::new_err("Failed to parse value from bytes"));
    }

    Err(PyValueError::new_err("Value must be an integer or bytes"))
}

/// Extract an `I256` from a Python object (signed two's complement).
///
/// Accepts small integers directly (via `i128`) and larger ones through
/// Python's `int.to_bytes(32, "big", signed=True)` — the same encoding the ABI
/// argument conversion takes, so `I256::MIN` and every negative amount
/// round-trip exactly. The mirror of [`extract_python_u256`] for the signed
/// fields (a captured swap's `amount0`/`amount1` are deltas, negative for the
/// paid-in side).
///
/// # Errors
///
/// Returns `PyValueError` if the object is not an integer or does not fit in
/// 256 signed bits.
pub(crate) fn extract_python_i256(obj: &Bound<'_, PyAny>) -> PyResult<I256> {
    if let Ok(int_val) = obj.extract::<i128>() {
        return I256::try_from(int_val)
            .map_err(|_| PyValueError::new_err("Integer value out of range for int256"));
    }
    let int_type = pyo3::types::PyInt::type_object(obj.py());
    if obj.is_instance(&int_type)? {
        let kwargs = PyDict::new(obj.py());
        kwargs.set_item("signed", true)?;
        let bytes = obj.call_method("to_bytes", (32, "big"), Some(&kwargs))?;
        let bytes: &[u8] = bytes.extract()?;
        let raw = U256::from_be_bytes(
            <[u8; 32]>::try_from(bytes)
                .map_err(|_| PyValueError::new_err("Integer value out of range for int256"))?,
        );
        return Ok(I256::from_raw(raw));
    }
    Err(PyValueError::new_err("Value must be an integer"))
}

#[cfg(feature = "abi")]
/// Create an `AbiValue` from a Python object.
///
/// # Errors
///
/// Returns `PyValueError` if the Python object cannot be converted.
pub fn abi_value_from_python(py: Python<'_>, obj: &Bound<'_, PyAny>) -> PyResult<AbiValue> {
    fn convert_item(item: &Bound<'_, PyAny>, py: Python<'_>) -> PyResult<AbiValue> {
        abi_value_from_python(py, item)
    }

    // bool is an int subclass in Python, so it must be tested first
    if let Ok(b) = obj.cast::<PyBool>() {
        return Ok(AbiValue::Bool(b.is_true()));
    }

    // Python integers can be arbitrarily large, but only 256-bit values are supported
    if let Ok(int_val) = obj.extract::<i128>() {
        if int_val >= 0 {
            return Ok(AbiValue::Uint(
                U256::from(int_val.cast_unsigned()),
                BitWidth::BITS_256,
            ));
        }
        return Ok(AbiValue::Int(
            I256::try_from(int_val)
                .map_err(|_| PyValueError::new_err("Integer conversion failed"))?,
            BitWidth::BITS_256,
        ));
    }

    let int_type = pyo3::types::PyInt::type_object(obj.py());
    if obj.is_instance(&int_type)? {
        let is_negative: bool = obj.call_method1("__lt__", (0,))?.extract()?;

        if is_negative {
            // signed=True gives two's complement encoding, so I256::MIN round-trips
            let kwargs = PyDict::new(py);
            kwargs.set_item("signed", true)?;
            let bytes = obj.call_method("to_bytes", (32, "big"), Some(&kwargs))?;
            let bytes: &[u8] = bytes.extract()?;
            let u256 = U256::from_be_bytes(
                <[u8; 32]>::try_from(bytes)
                    .map_err(|_| PyValueError::new_err("Integer value out of range for int256"))?,
            );
            return Ok(AbiValue::Int(I256::from_raw(u256), BitWidth::BITS_256));
        }
        let kwargs = PyDict::new(py);
        kwargs.set_item("signed", false)?;
        let bytes = obj.call_method("to_bytes", (32, "big"), Some(&kwargs))?;
        let bytes: &[u8] = bytes.extract()?;
        let u256 = U256::from_be_bytes(
            <[u8; 32]>::try_from(bytes)
                .map_err(|_| PyValueError::new_err("Integer value out of range for uint256"))?,
        );
        return Ok(AbiValue::Uint(u256, BitWidth::BITS_256));
    }

    if let Ok(s) = obj.cast::<PyString>() {
        let s = s.to_string();
        // Hex is validated explicitly so a random 42-char string starting
        // with "0x" is not misread as an address.
        if s.starts_with("0x") && s.len() == 42 && s[2..].chars().all(|c| c.is_ascii_hexdigit()) {
            if let Ok(addr) = alloy::primitives::Address::from_str(&s) {
                return Ok(AbiValue::Address(addr.into()));
            }
        }
        return Ok(AbiValue::String(s));
    }

    if let Ok(b) = obj.cast::<PyBytes>() {
        return Ok(AbiValue::Bytes(b.as_bytes().to_vec()));
    }

    if let Ok(list) = obj.cast::<PyList>() {
        let values: Result<Vec<_>, _> = list.iter().map(|item| convert_item(&item, py)).collect();
        return Ok(AbiValue::Array(values?));
    }

    // The target type decides whether a tuple maps to DynSolValue::Tuple
    // or an array element.
    if let Ok(tup) = obj.cast::<PyTuple>() {
        let values: Result<Vec<_>, _> = tup.iter().map(|item| convert_item(&item, py)).collect();
        return Ok(AbiValue::Array(values?));
    }

    Err(PyValueError::new_err(format!(
        "Cannot convert Python object to ABI value: {}",
        obj.repr()?
    )))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn test_u256_to_python() {
        pyo3::Python::attach(|py| {
            let zero = U256::ZERO;
            let py_zero = u256_to_py(py, &zero).unwrap();
            let val: u64 = py_zero.extract().unwrap();
            assert_eq!(val, 0);

            let small = U256::from(42u64);
            let py_small = u256_to_py(py, &small).unwrap();
            let val: u64 = py_small.extract().unwrap();
            assert_eq!(val, 42);

            let max_u64 = U256::from(u64::MAX);
            let py_max = u256_to_py(py, &max_u64).unwrap();
            let val: u64 = py_max.extract().unwrap();
            assert_eq!(val, u64::MAX);

            let max_u256 = U256::MAX;
            let py_max = u256_to_py(py, &max_u256).unwrap();
            let bytes = py_max.call_method1("to_bytes", (32, "big")).unwrap();
            let bytes: &[u8] = bytes.extract().unwrap();
            let expected_bytes: [u8; 32] = max_u256.to_be_bytes();
            assert_eq!(bytes, expected_bytes);
        });
    }

    #[test]
    fn test_extract_python_u256() {
        pyo3::Python::attach(|py| {
            let small = pyo3::types::PyInt::new(py, 42);
            let val = extract_python_u256(&small).unwrap();
            assert_eq!(val, U256::from(42u64));

            let zero = pyo3::types::PyInt::new(py, 0);
            let val = extract_python_u256(&zero).unwrap();
            assert_eq!(val, U256::ZERO);

            let neg = pyo3::types::PyInt::new(py, -1);
            assert!(extract_python_u256(&neg).is_err());

            let large = pyo3::types::PyInt::new(py, i128::from(u64::MAX));
            let val = extract_python_u256(&large).unwrap();
            assert_eq!(val, U256::from(u64::MAX));

            let bytes = pyo3::types::PyBytes::new(py, &[0u8; 32]);
            let val = extract_python_u256(&bytes).unwrap();
            assert_eq!(val, U256::ZERO);

            let mut data = [0u8; 32];
            data[31] = 0xff;
            let bytes = pyo3::types::PyBytes::new(py, &data);
            let val = extract_python_u256(&bytes).unwrap();
            assert_eq!(val, U256::from(255u64));
        });
    }

    #[test]
    fn test_i256_to_python() {
        pyo3::Python::attach(|py| {
            let zero = I256::ZERO;
            let py_zero = i256_to_py(py, &zero).unwrap();
            let val: i64 = py_zero.extract().unwrap();
            assert_eq!(val, 0);

            let pos = I256::try_from(12345i64).unwrap();
            let py_pos = i256_to_py(py, &pos).unwrap();
            let val: i64 = py_pos.extract().unwrap();
            assert_eq!(val, 12345);

            let neg = I256::try_from(-12345i64).unwrap();
            let py_neg = i256_to_py(py, &neg).unwrap();
            let val: i64 = py_neg.extract().unwrap();
            assert_eq!(val, -12345);

            let min = I256::MINUS_ONE;
            let py_min = i256_to_py(py, &min).unwrap();
            let val: i64 = py_min.extract().unwrap();
            assert_eq!(val, -1);
        });
    }
}
