//! ABI types, decoding, encoding, and function-signature parsing.
//!
//! Pure-Rust crate (no `pyo3`, no `tokio`): the Python-facing `decode`/`encode`
//! wrappers live in the root `degenbot_rs` crate's `decoder_py` / `encoder_py`
//! modules, because they need `conversion::alloy` and `conversion::cache` —
//! binding-layer concerns this crate must not depend on.

pub mod abi_types;
pub mod decoder;
pub mod encoder;
pub mod signature_parser;
