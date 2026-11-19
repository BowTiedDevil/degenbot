//! Unified ABI type and value representation.
//!
//! This module provides:
//! - `AbiType` enum for representing ABI type signatures
//! - `AbiValue` enum for representing encoded/decoded ABI values
//! - `CachedAbiTypes` struct for pre-parsed batch encoding/decoding
//!
//! Used by `decoder.rs`, `encoder.rs`, and `contract.rs` to ensure
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
