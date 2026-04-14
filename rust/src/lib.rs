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
//! - [`contract_py`] - `PyO3` bindings for contract
//! - [`async_contract`] - Async contract wrapper with batch calls
//! - [`signature_parser`] - Robust function signature parsing
//! - [`runtime`] - Shared Tokio runtime singleton
//! - [`hex_utils`] - Pure-Rust hex encoding/decoding (no `PyO3` dependency)
//! - [`py_converters`] - Python object converters for RPC types (block/tx/log dicts, JSON-to-Python with `HexBytes`)
//!
//! See individual module documentation for usage examples.

pub mod abi_decoder;
pub mod abi_encoder;
pub mod abi_types;
pub mod alloy_py;
pub mod address_utils;
pub mod async_contract;
pub mod async_provider;

pub mod contract;
pub mod contract_py;
pub mod errors;
pub mod hex_utils;
pub mod provider;
pub mod provider_py;
pub mod py_cache;
pub mod py_converters;
pub mod runtime;
pub mod signature_parser;
pub mod tick_math;
pub mod tick_math_py;

// Re-export commonly used items at the crate root
pub use address_utils::{parse_address, to_checksum_address, to_checksum_address_bytes, to_checksum_address_str};
pub use hex_utils::{decode_hex, encode_hex};

pub use errors::{AbiDecodeError, AddressError, ProviderError, TickMathError};
pub use tick_math::{
    get_sqrt_ratio_at_tick_internal, get_tick_at_sqrt_ratio_internal,
};
pub use tick_math_py::{get_sqrt_ratio_at_tick, get_tick_at_sqrt_ratio};

use pyo3::prelude::*;

#[pymodule]
fn degenbot_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Initialize logging bridge from Rust to Python
    pyo3_log::init();

    // Tick math functions
    m.add_function(wrap_pyfunction!(tick_math_py::get_sqrt_ratio_at_tick, m)?)?;
    m.add_function(wrap_pyfunction!(tick_math_py::get_tick_at_sqrt_ratio, m)?)?;

    // Address utilities
    m.add_function(wrap_pyfunction!(address_utils::to_checksum_address, m)?)?;

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

    // Async modules
    m.add_class::<async_provider::PyAsyncAlloyProvider>()?;
    m.add_class::<async_contract::PyAsyncContract>()?;

    Ok(())
}
