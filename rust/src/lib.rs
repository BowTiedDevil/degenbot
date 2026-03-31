//! Rust extension for degenbot.
//!
//! This crate provides high-performance Rust implementations of common operations
//! used by the degenbot Python package.
//!
//! # Modules
//!
//! - [`abi_decoder`] - High-performance ABI decoding
//! - [`tick_math`] - Uniswap V3 tick-to-price calculations
//! - [`address_utils`] - Ethereum address utilities
//! - [`errors`] - Error types
//! - [`provider`] - Ethereum RPC provider with Alloy
//! - [`contract`] - Smart contract interface
//! - [`async_provider`] - Async Ethereum provider
//! - [`async_contract`] - Async contract interface
//!
//! See individual module documentation for usage examples.

pub mod abi_decoder;
pub mod address_utils;
pub mod async_contract;
pub mod async_provider;

pub mod contract;
pub mod contract_py;
pub mod errors;
pub mod provider;
pub mod provider_py;
pub mod runtime;
pub mod tick_math;
pub mod utils;

// Re-export commonly used items at the crate root
pub use address_utils::{to_checksum_address, to_checksum_address_bytes, to_checksum_address_str};

pub use errors::{AbiDecodeError, AddressError, ProviderError, TickMathError};
pub use tick_math::{
    get_sqrt_ratio_at_tick, get_sqrt_ratio_at_tick_internal, get_tick_at_sqrt_ratio,
    get_tick_at_sqrt_ratio_internal,
};

use pyo3::prelude::*;

#[pymodule]
fn _rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Initialize logging bridge from Rust to Python
    pyo3_log::init();

    // Tick math functions
    m.add_function(wrap_pyfunction!(tick_math::get_sqrt_ratio_at_tick, m)?)?;
    m.add_function(wrap_pyfunction!(tick_math::get_tick_at_sqrt_ratio, m)?)?;

    // Address utilities
    m.add_function(wrap_pyfunction!(address_utils::to_checksum_address, m)?)?;

    // ABI decoder functions
    m.add_function(wrap_pyfunction!(abi_decoder::decode, m)?)?;
    m.add_function(wrap_pyfunction!(abi_decoder::decode_single, m)?)?;

    // Provider module
    provider_py::add_provider_module(m)?;

    // Contract module
    contract_py::add_contract_module(m)?;

    // Async modules
    m.add_class::<async_provider::PyAsyncAlloyProvider>()?;
    m.add_class::<async_contract::PyAsyncContract>()?;

    Ok(())
}
