//! ABI types, decoding, encoding, and function-signature parsing.
//!
//! Pure-Rust crate (no `pyo3`, no `tokio`) depending only on `degenbot-core`
//! (for `AbiDecodeError`/`ContractError`) and `alloy`. Provides:
//!
//! - [`abi_types`] — `AbiType` / `AbiValue` / `CachedAbiTypes` (with the shared
//!   `TYPE_CACHE` interner used by both decoder and encoder).
//! - [`abi_decoder`] — pure `decode_rust` / `decode_single_rust` /
//!   `decode_for_types`.
//! - [`abi_encoder`] — pure `encode_rust` / `encode_single_rust` /
//!   `encode_for_types`.
//! - [`signature_parser`] — recursive-descent function-signature parser
//!   producing `FunctionSignature`.
//!
//! The Python-facing `decode`/`encode` wrappers live in the root `degenbot_rs`
//! crate's `abi_decoder_py` / `abi_encoder_py` modules (they need `alloy_py`
//! and `py_cache`, which are binding-layer concerns).

pub mod abi_decoder;
pub mod abi_encoder;
pub mod abi_types;
pub mod signature_parser;
