//! Unified ABI type and value representation.
//!
//! This module provides:
//! - `AbiType` enum for representing ABI type signatures
//! - `AbiValue` enum for representing encoded/decoded ABI values
//! - `CachedAbiTypes` struct for pre-parsed batch encoding/decoding
//!
//! Used by `abi_decoder.rs`, `abi_encoder.rs`, and `contract.rs` to ensure
//! consistent type handling across the codebase.
//!
//! # Module Organization
//!
//! - [`type_`] - `AbiType` enum, `AbiTypeError`, type parsing
//! - [`value`] - `AbiValue` enum, integer parsing, string-to-value conversion
//! - [`cached`] - `CachedAbiTypes` struct, `value_to_alloy_for_type`

pub mod cached;
pub mod type_;
pub mod value;

// Re-export all public items at the module root for backward compatibility.
// External code can use `crate::abi_types::AbiType` or `crate::abi_types::type_::AbiType`.
pub use type_::{AbiType, AbiTypeError, parse_type_list};
pub use value::{AbiValue, ParseIntError};
#[allow(deprecated)]
pub use value::{ParseU256Error, ParseI256Error};
pub use cached::{CachedAbiTypes, value_to_alloy_for_type, get_cached_types};
