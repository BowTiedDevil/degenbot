//! Rust extension for degenbot.
//!
//! This crate provides high-performance Rust implementations of common operations
//! used by the degenbot Python package.
//!
//! # Modules
//!
//! - [`abi_types`] - Unified ABI type/value representation (`AbiType`, `AbiValue`, `CachedAbiTypes`)
//! - [`abi_decoder`] - High-performance ABI decoding
//! - [`abi_encoder`] - High-performance ABI encoding
//! - [`alloy_py`] - Zero-intermediate-allocation U256/I256 → Python int conversion
//! - [`py_cache`] - Cached Python function/class references (`int.from_bytes`, `HexBytes`)
//! - [`tick_math`] - Uniswap V3 tick-to-price calculations
//! - [`address_utils`] - Ethereum address utilities (EIP-55 checksumming)
//! - [`errors`] - Centralized error types with `thiserror`
//! - [`provider`] - Ethereum RPC provider with Alloy (HTTP, WS, IPC)
//! - [`provider_py`] - `PyO3` bindings for sync provider
//! - [`async_provider`] - Async Ethereum provider wrapper
//! - [`contract`] - Smart contract interface with ABI encoding/decoding
//! - [`contract_bindings`] - Generated parent-contract Alloy bindings
//! - [`contract_py`] - `PyO3` bindings for contract
//! - [`execution`] - Executor calldata builders for the locked on-chain entrypoints
//! - [`execution_py`] - `PyO3` bindings for executor calldata builders
//! - [`execution_engine`] - Deterministic Rust/Alloy execution-job composition
//! - [`execution_engine_py`] - `PyO3` bindings for execution-job composition
//! - [`signed_order_admission`] - Deterministic signed-order hashing, fill-capacity, and min-output admission
//! - [`signed_order_admission_py`] - `PyO3` bindings for signed-order admission
//! - [`fixed_abi`] - `sol!`-checked bindings for stable hot-path contract interfaces
//! - [`async_contract`] - Async contract wrapper with batch calls
//! - [`signature_parser`] - Robust function signature parsing
//! - [`runtime`] - Shared Tokio runtime singleton
//! - [`types`] - Canonical Rust wire/ABI mirrors used by the solver engine
//! - [`hex_utils`] - Pure-Rust hex encoding/decoding (no `PyO3` dependency)
//! - [`py_converters`] - Python object converters for RPC types (block/tx/log dicts, JSON-to-Python with `HexBytes`)
//!
//! See individual module documentation for usage examples.

#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

#[cfg(all(test, feature = "extension-module"))]
compile_error!(
    "Rust integration/unit tests must not be built with `extension-module`. \
Use `cargo test --features auto-initialize` (or `just test-rust`) for tests."
);

pub mod abi_decoder;
pub mod abi_encoder;
pub mod abi_types;
pub mod address_utils;
pub mod address_utils_py;
pub mod alloy_py;
pub mod async_contract;
pub mod async_provider;

pub mod contract;
pub mod contract_bindings;
pub mod contract_py;
pub mod decision;
pub mod errors;
pub mod execution;
pub mod execution_engine;
pub mod execution_engine_py;
pub mod execution_py;
pub mod executor;
pub mod fixed_abi;
pub mod hex_utils;
pub mod matching;
pub mod monitor;
pub mod optimizers;
pub mod provider;
pub mod provider_py;
pub mod py_cache;
pub mod py_converters;
pub mod runtime;
pub mod signature_parser;
pub mod signed_order_admission;
pub mod signed_order_admission_py;
pub mod simulation;
pub mod simulation_py;
pub mod tick_math;
pub mod tick_math_py;
pub mod types;
pub mod utils;

// Re-export commonly used items at the crate root
pub use address_utils::{parse_address, to_checksum_address_bytes, to_checksum_address_str};
pub use address_utils_py::to_checksum_address;
pub use hex_utils::{decode_hex, encode_hex};

pub use errors::{AbiDecodeError, AddressError, ProviderError, TickMathError};
pub use monitor::{
    Eip7702Delegation, GasEnvelope, Lane, Message, Plan, PlanKind, Reserves, Settlement,
    SettlementStatus, Timestamps,
};
pub use tick_math::{get_sqrt_ratio_at_tick_internal, get_tick_at_sqrt_ratio_internal};
pub use tick_math_py::{get_sqrt_ratio_at_tick, get_tick_at_sqrt_ratio};

use pyo3::prelude::*;

#[cfg(test)]
pub(crate) fn with_python_for_tests<R>(f: impl for<'py> FnOnce(Python<'py>) -> R) -> R {
    static PYTHON_INIT: std::sync::Once = std::sync::Once::new();
    PYTHON_INIT.call_once(Python::initialize);
    Python::attach(f)
}

#[pymodule]
fn degenbot_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Initialize logging bridge from Rust to Python. Pytest and editable
    // reloads can import this extension more than once in-process; a prior
    // logger should not turn an otherwise valid import into a panic.
    let _ = pyo3_log::try_init();

    // Tick math functions
    m.add_function(wrap_pyfunction!(tick_math_py::get_sqrt_ratio_at_tick, m)?)?;
    m.add_function(wrap_pyfunction!(tick_math_py::get_tick_at_sqrt_ratio, m)?)?;

    // Address utilities
    m.add_function(wrap_pyfunction!(address_utils_py::to_checksum_address, m)?)?;

    // ABI decoder functions
    m.add_function(wrap_pyfunction!(abi_decoder::decode, m)?)?;
    m.add_function(wrap_pyfunction!(abi_decoder::decode_single, m)?)?;

    // ABI encoder functions
    m.add_function(wrap_pyfunction!(abi_encoder::encode, m)?)?;
    m.add_function(wrap_pyfunction!(abi_encoder::encode_single, m)?)?;

    // Provider module
    provider_py::add_provider_module(m)?;

    // Contract module
    contract_py::add_contract_module(m)?;

    // Executor calldata module
    execution_py::add_execution_module(m)?;

    // Execution-engine composition module
    execution_engine_py::add_execution_engine_module(m)?;

    // Signed-order admission module
    signed_order_admission_py::add_signed_order_admission_module(m)?;

    // Simulation module
    simulation_py::add_simulation_module(m)?;

    // Möbius optimizer module
    optimizers::mobius_py::add_mobius_module(m)?;

    // Async modules
    m.add_class::<async_provider::PyAsyncAlloyProvider>()?;
    m.add_class::<async_contract::PyAsyncContract>()?;

    Ok(())
}
