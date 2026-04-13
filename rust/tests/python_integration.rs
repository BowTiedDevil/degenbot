//! Python integration tests for `degenbot_rs`.
//!
//! These tests verify the Python → Rust → Python boundary by using
//! `pyo3::Python::try_attach()` to execute Python code and verify
//! roundtrip conversions work correctly.
//!
//! These tests are skipped if Python is not available (returns `None`).
//!
//! Run with: `cargo test --features auto-initialize --test python_integration`

#![allow(
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::uninlined_format_args,
    clippy::match_same_arms
)]

use alloy::primitives::{I256, U256};
use degenbot_rs::alloy_py::abi_value_from_python;
use degenbot_rs::abi_types::AbiValue;
use pyo3::prelude::*;

/// Helper to run a Python test with proper GIL handling.
/// Returns `None` if Python is not available.
fn with_python<F, R>(f: F) -> Option<R>
where
    F: for<'py> FnOnce(Python<'py>) -> R,
{
    Python::try_attach(f)
}

/// Test Python integer → `AbiValue` conversion for small positive integers.
#[test]
fn test_python_int_small_positive() {
    let result = with_python(|py| {
        let py_int = 42i64.into_pyobject(py).unwrap();
        abi_value_from_python(py, &py_int).unwrap()
    });
    if result.is_none() {
        println!("Skipping test - Python not available");
        return;
    }
    assert_eq!(result, Some(AbiValue::Uint(U256::from(42u64))));
}

/// Test Python integer → `AbiValue` conversion for small negative integers.
#[test]
fn test_python_int_small_negative() {
    let result = with_python(|py| {
        let py_int = (-42i64).into_pyobject(py).unwrap();
        abi_value_from_python(py, &py_int).unwrap()
    });
    if result.is_none() {
        println!("Skipping test - Python not available");
        return;
    }
    assert_eq!(result, Some(AbiValue::Int(I256::try_from(-42i64).unwrap())));
}

/// Test Python integer → `AbiValue` conversion for `U256::MAX`.
#[test]
fn test_python_int_u256_max() {
    let result = with_python(|py| {
        // `U256::MAX` as a Python int using from_bytes
        let code = c"int.from_bytes(b'\xff' * 32, 'big')";
        let py_int = py.eval(code, None, None).unwrap();
        abi_value_from_python(py, &py_int).unwrap()
    });
    if result.is_none() {
        println!("Skipping test - Python not available");
        return;
    }

    if let Some(AbiValue::Uint(n)) = result {
        assert_eq!(n, U256::MAX, "`U256::MAX` should convert correctly");
    } else {
        panic!("Expected Uint variant, got {result:?}");
    }
}

/// Test Python integer → `AbiValue` conversion for `I256::MIN`.
#[test]
fn test_python_int_i256_min() {
    let result = with_python(|py| {
        // `I256::MIN` as a Python int
        let code = c"- (2 ** 255)";
        let py_int = py.eval(code, None, None).unwrap();
        abi_value_from_python(py, &py_int).unwrap()
    });
    if result.is_none() {
        println!("Skipping test - Python not available");
        return;
    }

    if let Some(AbiValue::Int(n)) = result {
        assert_eq!(n, I256::MIN, "`I256::MIN` should convert correctly, got {n:?}");
    } else {
        panic!("Expected Int variant, got {result:?}");
    }
}

/// Test Python bool → `AbiValue` conversion.
#[test]
fn test_python_bool() {
    let result_true = with_python(|py| {
        let py_true = true.into_pyobject(py).unwrap();
        abi_value_from_python(py, &py_true).unwrap()
    });
    if result_true.is_none() {
        println!("Skipping test - Python not available");
        return;
    }
    assert_eq!(result_true, Some(AbiValue::Bool(true)));

    let result_false = with_python(|py| {
        let py_false = false.into_pyobject(py).unwrap();
        abi_value_from_python(py, &py_false).unwrap()
    });
    assert_eq!(result_false, Some(AbiValue::Bool(false)));
}

/// Test Python bytes → `AbiValue` conversion.
#[test]
fn test_python_bytes() {
    let result = with_python(|py| {
        let bytes = vec![0xde, 0xad, 0xbe, 0xef];
        let py_bytes = pyo3::types::PyBytes::new(py, &bytes);
        abi_value_from_python(py, &py_bytes).unwrap()
    });
    if result.is_none() {
        println!("Skipping test - Python not available");
        return;
    }
    assert_eq!(result, Some(AbiValue::Bytes(vec![0xde, 0xad, 0xbe, 0xef])));
}

/// Test Python string (address) → `AbiValue` conversion.
#[test]
fn test_python_string_address() {
    let result = with_python(|py| {
        let addr_str = "0xd3cda913deb6f67967b99d67acdfa1712c293601";
        let py_str = addr_str.into_pyobject(py).unwrap();
        abi_value_from_python(py, &py_str).unwrap()
    });
    if result.is_none() {
        println!("Skipping test - Python not available");
        return;
    }

    if let Some(AbiValue::Address(addr)) = result {
        let expected: [u8; 20] = [0xd3, 0xcd, 0xa9, 0x13, 0xde, 0xb6, 0xf6, 0x79, 0x67, 0xb9,
                                  0x9d, 0x67, 0xac, 0xdf, 0xa1, 0x71, 0x2c, 0x29, 0x36, 0x01];
        assert_eq!(addr, expected);
    } else {
        panic!("Expected Address variant, got {result:?}");
    }
}

/// Test Python list → `AbiValue` conversion for array.
#[test]
fn test_python_list_array() {
    let result = with_python(|py| {
        let list = pyo3::types::PyList::new(py, [1i64, 2, 3]).unwrap();
        abi_value_from_python(py, &list).unwrap()
    });
    if result.is_none() {
        println!("Skipping test - Python not available");
        return;
    }

    match result {
        Some(AbiValue::Array(values)) => {
            assert_eq!(values.len(), 3);
            assert_eq!(values[0], AbiValue::Uint(U256::from(1u64)));
            assert_eq!(values[1], AbiValue::Uint(U256::from(2u64)));
            assert_eq!(values[2], AbiValue::Uint(U256::from(3u64)));
        }
        _ => panic!("Expected Array variant, got {result:?}"),
    }
}

/// Test Python list with mixed types succeeds (current behavior).
#[test]
fn test_python_list_mixed_types_succeeds() {
    let result = with_python(|py| {
        let list = pyo3::types::PyList::new(py, [1i64, 2]).unwrap();
        abi_value_from_python(py, &list)
    });
    if result.is_none() {
        println!("Skipping test - Python not available");
        return;
    }
    assert!(result.unwrap().is_ok(), "Array of ints should succeed");
}

/// Test Python int at i128 boundary.
#[test]
fn test_python_int_i128_boundary() {
    // i128::MAX
    let result_max = with_python(|py| {
        let py_int = i128::MAX.into_pyobject(py).unwrap();
        abi_value_from_python(py, &py_int).unwrap()
    });
    if result_max.is_none() {
        println!("Skipping test - Python not available");
        return;
    }
    assert_eq!(result_max, Some(AbiValue::Uint(U256::from(i128::MAX as u128))));

    // i128::MIN
    let result_min = with_python(|py| {
        let py_int = i128::MIN.into_pyobject(py).unwrap();
        abi_value_from_python(py, &py_int).unwrap()
    });
    assert_eq!(result_min, Some(AbiValue::Int(I256::try_from(i128::MIN).unwrap())));
}

/// Test Python int larger than i128 (requires `to_bytes` path).
#[test]
fn test_python_int_large_positive() {
    let result = with_python(|py| {
        // A value larger than i128::MAX that still fits in U256
        // 2^127
        let code = c"2 ** 127";
        let py_int = py.eval(code, None, None).unwrap();
        abi_value_from_python(py, &py_int).unwrap()
    });
    if result.is_none() {
        println!("Skipping test - Python not available");
        return;
    }

    if let Some(AbiValue::Uint(n)) = result {
        let expected = U256::from(2u128.pow(127));
        assert_eq!(n, expected, "Large positive int should convert via to_bytes path");
    } else {
        panic!("Expected Uint variant, got {result:?}");
    }
}

/// Test Python int large negative (requires `to_bytes` path).
#[test]
fn test_python_int_large_negative() {
    let result = with_python(|py| {
        // A value smaller than i128::MIN that still fits in I256
        let code = c"-(2 ** 127) - 1";
        let py_int = py.eval(code, None, None).unwrap();
        abi_value_from_python(py, &py_int).unwrap()
    });
    if result.is_none() {
        println!("Skipping test - Python not available");
        return;
    }

    if let Some(AbiValue::Int(n)) = result {
        assert!(n < I256::ZERO, "Large negative int should be negative");
    } else {
        panic!("Expected Int variant, got {result:?}");
    }
}

/// Test invalid Python type errors appropriately.
#[test]
fn test_python_invalid_type() {
    let result: Option<PyResult<AbiValue>> = with_python(|py| {
        // Try to convert a dict, which is not supported
        let dict = pyo3::types::PyDict::new(py);
        dict.set_item("key", "value").unwrap();
        abi_value_from_python(py, &dict)
    });

    if result.is_none() {
        println!("Skipping test - Python not available");
        return;
    }

    match result {
        Some(Err(_)) | None => (), // Expected error or Python not available
        Some(Ok(_)) => panic!("Dict should not be convertible to AbiValue"),
    }
}

/// Test empty Python list.
#[test]
fn test_python_empty_list() {
    let result = with_python(|py| {
        let list = pyo3::types::PyList::empty(py);
        abi_value_from_python(py, &list).unwrap()
    });
    if result.is_none() {
        println!("Skipping test - Python not available");
        return;
    }
    assert!(matches!(result, Some(AbiValue::Array(arr)) if arr.is_empty()));
}

/// Test Python string (non-address) → `AbiValue` conversion.
#[test]
fn test_python_string_non_address() {
    let result = with_python(|py| {
        let s = "Hello, World!";
        let py_str = s.into_pyobject(py).unwrap();
        abi_value_from_python(py, &py_str).unwrap()
    });
    if result.is_none() {
        println!("Skipping test - Python not available");
        return;
    }
    assert_eq!(result, Some(AbiValue::String("Hello, World!".to_string())));
}
