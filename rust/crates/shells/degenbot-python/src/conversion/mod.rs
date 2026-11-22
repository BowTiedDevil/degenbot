//! Shared PyO3-dependent converters used across the binding-crate wrappers.
//!
//! Mirrors `polars-python/src/conversion/`: PyO3-dependent glue (argument
//! extraction, result wrapping, Python-object construction) used by many
//! `*_py` wrapper modules, but exposing no `#[pyfunction]` of its own.
//!
//! - [`alloy`] — U256/I256 ↔ Python `int` newtypes + extractors (`PyU256`,
//!   `PyI256`, `extract_python_u256`, `u256_to_py`, …)
//! - [`cache`] — cached Python function refs via `PyOnceLock`
//!   (`int.from_bytes`, `bytes_to_int`, `to_py_bytes`, …)
//! - [`json`] — Python-to-JSON converters building JSON-RPC requests
//! - [`rpc_types`] — block / transaction / log RPC types → Python dicts

pub mod alloy;
pub mod cache;
pub mod json;
pub mod rpc_types;
