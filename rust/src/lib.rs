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
//!
//! See individual module documentation for usage examples.

pub mod abi_decoder;
pub mod address_utils;
pub mod connection;
pub mod connection_py;
pub mod contract;
pub mod contract_py;
pub mod errors;
pub mod provider;
pub mod provider_py;
pub mod tick_math;

// Re-export commonly used items at the crate root
pub use address_utils::{to_checksum_address, to_checksum_address_bytes, to_checksum_address_str};
pub use connection::{ChainConfig, ConnectionManager, EndpointMetrics, HealthStatus};
pub use errors::{AbiDecodeError, AddressError, ProviderError, TickMathError};
pub use tick_math::{
    get_sqrt_ratio_at_tick, get_sqrt_ratio_at_tick_internal, get_tick_at_sqrt_ratio,
    get_tick_at_sqrt_ratio_internal,
};

use pyo3::prelude::*;

#[pymodule]
fn _rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
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

    // Connection module
    connection_py::add_connection_module(m)?;

    // Contract module
    contract_py::add_contract_module(m)?;

    Ok(())
}
