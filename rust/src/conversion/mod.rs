//! Shared PyO3-dependent converters used across the binding-crate wrappers.
//!
//! Mirrors `polars-python/src/conversion/`: PyO3-dependent glue (argument
//! extraction, result wrapping, Python-object construction) used by many
//! `*_py` wrapper modules, but exposing no `#[pyfunction]` of its own.
//!
//! - [`alloy`] — U256/I256 ↔ Python `int` newtypes + extractors (`PyU256`,
//!   `PyI256`, `extract_python_u256`, `u256_to_py`, …)
//! - [`cache`] — cached Python function/class refs via `PyOnceLock`
//!   (`int.from_bytes`, `HexBytes`, `bytes_to_int`, `create_hexbytes`, …)
//! - [`json`] — JSON-to-Python converters feeding RPC result wrapping
//! - [`rpc_types`] — block / transaction / log RPC types → Python dicts
//!
//! (ergo UG6FKN task XRF6HV.)

pub mod alloy;
pub mod cache;
pub mod json;
pub mod rpc_types;
