//! Python integration tests for `degenbot_rs`.
//!
//! These tests verify the Python → Rust → Python boundary by using
//! `pyo3::Python::try_attach()` to execute Python code and verify
//! roundtrip conversions work correctly.
//!
//! These tests are skipped if Python is not available (returns `None`).
//!
//! Run with: `cargo test --features auto-initialize --test python_integration`

#![expect(clippy::panic)]
#![cfg(feature = "auto-initialize")]
#![expect(clippy::unwrap_used, clippy::doc_markdown)]

use alloy::primitives::{I256, U256};
use degenbot_rs::abi_types::AbiValue;
use degenbot_rs::conversion::alloy::abi_value_from_python;
use pyo3::prelude::*;

/// Helper to run a Python test with proper GIL handling.
/// Panics if Python is not available (should not happen with `auto-initialize` feature).
fn with_python<F, R>(f: F) -> R
where
    F: for<'py> FnOnce(Python<'py>) -> R,
{
    Python::attach(f)
}

/// Test Python integer → `AbiValue` conversion for small positive integers.
#[test]
fn test_python_int_small_positive() {
    let result = with_python(|py| {
        let py_int = 42i64.into_pyobject(py).unwrap();
        abi_value_from_python(py, &py_int).unwrap()
    });
    assert_eq!(result, AbiValue::Uint(U256::from(42u64), 256));
}

/// Test Python integer → `AbiValue` conversion for small negative integers.
#[test]
fn test_python_int_small_negative() {
    let result = with_python(|py| {
        let py_int = (-42i64).into_pyobject(py).unwrap();
        abi_value_from_python(py, &py_int).unwrap()
    });
    assert_eq!(result, AbiValue::Int(I256::try_from(-42i64).unwrap(), 256));
}

/// Test Python integer → `AbiValue` conversion for `U256::MAX`.
#[test]
fn test_python_int_u256_max() {
    let result = with_python(|py| {
        let code = c"int.from_bytes(bytes([255]) * 32, 'big')";
        let py_int = py.eval(code, None, None).unwrap();
        abi_value_from_python(py, &py_int).unwrap()
    });

    if let AbiValue::Uint(n, bits) = result {
        assert_eq!(n, U256::MAX, "`U256::MAX` should convert correctly");
        assert_eq!(bits, 256);
    } else {
        panic!("Expected Uint variant, got {result:?}");
    }
}

/// Test Python integer → `AbiValue` conversion for `I256::MIN`.
#[test]
fn test_python_int_i256_min() {
    let result = with_python(|py| {
        let code = c"- (2 ** 255)";
        let py_int = py.eval(code, None, None).unwrap();
        abi_value_from_python(py, &py_int).unwrap()
    });

    if let AbiValue::Int(n, bits) = result {
        assert_eq!(
            n,
            I256::MIN,
            "`I256::MIN` should convert correctly, got {n:?}"
        );
        assert_eq!(bits, 256);
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
    assert_eq!(result_true, AbiValue::Bool(true));

    let result_false = with_python(|py| {
        let py_false = false.into_pyobject(py).unwrap();
        abi_value_from_python(py, &py_false).unwrap()
    });
    assert_eq!(result_false, AbiValue::Bool(false));
}

/// Test Python bytes → `AbiValue` conversion.
#[test]
fn test_python_bytes() {
    let result = with_python(|py| {
        let bytes = vec![0xde, 0xad, 0xbe, 0xef];
        let py_bytes = pyo3::types::PyBytes::new(py, &bytes);
        abi_value_from_python(py, &py_bytes).unwrap()
    });
    assert_eq!(result, AbiValue::Bytes(vec![0xde, 0xad, 0xbe, 0xef]));
}

/// Test Python string (address) → `AbiValue` conversion.
#[test]
fn test_python_string_address() {
    let result = with_python(|py| {
        let addr_str = "0xd3cda913deb6f67967b99d67acdfa1712c293601";
        let py_str = addr_str.into_pyobject(py).unwrap();
        abi_value_from_python(py, &py_str).unwrap()
    });

    if let AbiValue::Address(addr) = result {
        let expected: [u8; 20] = [
            0xd3, 0xcd, 0xa9, 0x13, 0xde, 0xb6, 0xf6, 0x79, 0x67, 0xb9, 0x9d, 0x67, 0xac, 0xdf,
            0xa1, 0x71, 0x2c, 0x29, 0x36, 0x01,
        ];
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

    match result {
        AbiValue::Array(values) => {
            assert_eq!(values.len(), 3);
            assert_eq!(values[0], AbiValue::Uint(U256::from(1u64), 256));
            assert_eq!(values[1], AbiValue::Uint(U256::from(2u64), 256));
            assert_eq!(values[2], AbiValue::Uint(U256::from(3u64), 256));
        }
        _ => panic!("Expected Array variant, got {result:?}"),
    }
}

/// Test Python int at i128 boundary.
#[test]
fn test_python_int_i128_boundary() {
    let result_max = with_python(|py| {
        let py_int = i128::MAX.into_pyobject(py).unwrap();
        abi_value_from_python(py, &py_int).unwrap()
    });
    assert_eq!(
        result_max,
        AbiValue::Uint(U256::from(i128::MAX as u128), 256)
    );

    let result_min = with_python(|py| {
        let py_int = i128::MIN.into_pyobject(py).unwrap();
        abi_value_from_python(py, &py_int).unwrap()
    });
    assert_eq!(
        result_min,
        AbiValue::Int(I256::try_from(i128::MIN).unwrap(), 256)
    );
}

/// Test Python int larger than i128 (requires `to_bytes` path).
#[test]
fn test_python_int_large_positive() {
    let result = with_python(|py| {
        let code = c"2 ** 127";
        let py_int = py.eval(code, None, None).unwrap();
        abi_value_from_python(py, &py_int).unwrap()
    });

    if let AbiValue::Uint(n, bits) = result {
        let expected = U256::from(2u128.pow(127));
        assert_eq!(
            n, expected,
            "Large positive int should convert via to_bytes path"
        );
        assert_eq!(bits, 256);
    } else {
        panic!("Expected Uint variant, got {result:?}");
    }
}

/// Test Python int large negative (requires `to_bytes` path).
#[test]
fn test_python_int_large_negative() {
    let result = with_python(|py| {
        let code = c"-(2 ** 127) - 1";
        let py_int = py.eval(code, None, None).unwrap();
        abi_value_from_python(py, &py_int).unwrap()
    });

    if let AbiValue::Int(n, bits) = result {
        assert!(n < I256::ZERO, "Large negative int should be negative");
        assert_eq!(bits, 256);
    } else {
        panic!("Expected Int variant, got {result:?}");
    }
}

/// Test invalid Python type errors appropriately.
#[test]
fn test_python_invalid_type() {
    let result: PyResult<AbiValue> = with_python(|py| {
        let dict = pyo3::types::PyDict::new(py);
        dict.set_item("key", "value").unwrap();
        abi_value_from_python(py, &dict)
    });

    assert!(
        result.is_err(),
        "Dict should not be convertible to AbiValue"
    );
}

/// Test empty Python list.
#[test]
fn test_python_empty_list() {
    let result = with_python(|py| {
        let list = pyo3::types::PyList::empty(py);
        abi_value_from_python(py, &list).unwrap()
    });
    assert!(matches!(result, AbiValue::Array(arr) if arr.is_empty()));
}

/// Test Python string (non-address) → `AbiValue` conversion.
#[test]
fn test_python_string_non_address() {
    let result = with_python(|py| {
        let s = "Hello, World!";
        let py_str = s.into_pyobject(py).unwrap();
        abi_value_from_python(py, &py_str).unwrap()
    });
    assert_eq!(result, AbiValue::String("Hello, World!".to_string()));
}

/// Test the `PyBalancerRateProvider` adapter: a Python object exposing
/// `get_rates(block_identifier)` is wrapped as a stored
/// `Arc<dyn BalancerRateProvider>` and the `is_static()` / `get_rates()`
/// calls delegate to Python (ADR-005 slice 12c I/O trait object).
#[test]
fn test_py_balancer_rate_provider_delegates() {
    use degenbot_rs::bot::pool::make_balancer_rate_provider;

    let provider = with_python(|py| {
        // A Python class that records its `get_rates` call args and returns
        // a fixed tuple of ints.
        py
            .run(pyo3::ffi::c_str!(
                "class _Rates:\n    def __init__(self, rates):\n        self.rates = tuple(rates)\n        self.calls = []\n    def get_rates(self, block_identifier=None):\n        self.calls.append(block_identifier)\n        return self.rates\n_Rates"
            ), None, None)
            .unwrap();
        let () = ();
        let instance = py
            .eval(
                pyo3::ffi::c_str!("_Rates([10**18, 2 * 10**18])"),
                None,
                None,
            )
            .unwrap();
        make_balancer_rate_provider(instance.into())
    });
    // Dynamic provider → not static.
    assert!(!provider.is_static());
    // Delegate to Python: returns the construction-time tuple as U256s.
    let rates = provider.get_rates(Some(42)).unwrap();
    assert_eq!(rates.len(), 2);
    assert_eq!(
        rates[0],
        alloy::primitives::U256::from(10u64).pow(alloy::primitives::U256::from(18u64))
    );
    assert_eq!(
        rates[1],
        alloy::primitives::U256::from(2u64)
            * alloy::primitives::U256::from(10u64).pow(alloy::primitives::U256::from(18u64))
    );
}

/// Test the `PyCurveDataProvider` adapter: a Python object exposing the
/// CurveDataProvider read interface is wrapped as a stored
/// `Arc<dyn CurveDataProvider>` and the reads delegate to Python
/// (ADR-005 JFGCHJ I/O trait object).
#[test]
fn test_py_curve_data_provider_delegates() {
    use degenbot_rs::bot::pool::make_curve_data_provider;

    let provider = with_python(|py| {
        let src = pyo3::ffi::c_str!(
            "class _Cdp:\n    def __init__(self):\n        self.calls = []\n    def block_number(self):\n        return 18000000\n    def block_timestamp(self, block_number):\n        self.calls.append(('block_timestamp', block_number))\n        return 1700000000 + block_number\n    def token_balance(self, token_address, holder_address, block_number):\n        self.calls.append(('token_balance', token_address, holder_address, block_number))\n        return 12345\n    def token_total_supply(self, token_address, block_number):\n        return 98765\n    def lending_rates(self, block_number):\n        return (10**18, 2 * 10**18)\n    def d(self, block_number):\n        return 42000\n    def gamma(self, block_number):\n        return 7 * 10**9\n    def price_scale(self, block_number):\n        return (10**18, 10**18)\n    def admin_balances(self, block_number):\n        return (0, 0)\n    def redemption_price(self, block_number):\n        return 10**18\n    def base_cache_updated(self, block_number):\n        return 17999000\n    def base_virtual_price(self, block_number):\n        return 1010 * 10**15\n    def virtual_price(self, block_number):\n        return 1020 * 10**15\n_Cdp"
        );
        py.run(src, None, None).unwrap();
        let instance = py.eval(pyo3::ffi::c_str!("_Cdp()"), None, None).unwrap();
        make_curve_data_provider(instance.into())
    });
    // Delegate to Python: every read crosses the FFI boundary.
    assert_eq!(provider.block_number().unwrap(), 18_000_000);
    assert_eq!(
        provider.block_timestamp(1_700_000_000).unwrap(),
        3_400_000_000
    );
    assert_eq!(
        provider
            .token_balance(
                alloy::primitives::Address::ZERO,
                alloy::primitives::Address::ZERO,
                18_000_000
            )
            .unwrap(),
        alloy::primitives::U256::from(12_345_u64)
    );
    assert_eq!(
        provider
            .token_total_supply(alloy::primitives::Address::ZERO, 18_000_000)
            .unwrap(),
        alloy::primitives::U256::from(98_765_u64)
    );
    assert_eq!(
        provider.lending_rates(18_000_000).unwrap(),
        vec![
            alloy::primitives::U256::from(10u64).pow(alloy::primitives::U256::from(18u64)),
            alloy::primitives::U256::from(2u64)
                * alloy::primitives::U256::from(10u64).pow(alloy::primitives::U256::from(18u64))
        ]
    );
    assert_eq!(
        provider.d(18_000_000).unwrap(),
        alloy::primitives::U256::from(42_000_u64)
    );
    assert_eq!(
        provider.gamma(18_000_000).unwrap(),
        alloy::primitives::U256::from(7_000_000_000_u64)
    );
    assert_eq!(provider.price_scale(18_000_000).unwrap().len(), 2);
    assert_eq!(provider.admin_balances(18_000_000).unwrap().len(), 2);
    assert_eq!(
        provider.redemption_price(18_000_000).unwrap(),
        alloy::primitives::U256::from(10u64).pow(alloy::primitives::U256::from(18u64))
    );
    assert_eq!(provider.base_cache_updated(18_000_000).unwrap(), 17_999_000);
    assert_eq!(
        provider.base_virtual_price(18_000_000).unwrap(),
        alloy::primitives::U256::from(1_010_000_000_000_000_000_u128)
    );
    assert_eq!(
        provider.virtual_price(18_000_000).unwrap(),
        alloy::primitives::U256::from(1_020_000_000_000_000_000_u128)
    );
}
