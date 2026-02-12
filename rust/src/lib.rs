//! Rust extension for degenbot.
//!
//! This crate provides high-performance Rust implementations of common operations
//! used by the degenbot Python package.
//!
//! # Modules
//!
//! - [`tick_math`] - Uniswap V3 tick-to-price calculations
//! - [`address_utils`] - Ethereum address utilities
//! - [`errors`] - Error types
//!
//! See individual module documentation for usage examples.

pub mod address_utils;
pub mod errors;
pub mod tick_math;

// Re-export commonly used items at the crate root
pub use address_utils::to_checksum_address;
pub use errors::TickMathError;
pub use tick_math::{get_sqrt_ratio_at_tick, get_tick_at_sqrt_ratio};

use pyo3::prelude::*;

#[pymodule]
fn degenbot_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(tick_math::get_sqrt_ratio_at_tick, m)?)?;
    m.add_function(wrap_pyfunction!(tick_math::get_tick_at_sqrt_ratio, m)?)?;
    m.add_function(wrap_pyfunction!(address_utils::to_checksum_address, m)?)?;
    Ok(())
}
