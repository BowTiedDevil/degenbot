//! Error type for the degenbot-price readers.
//!
//! The readers reach on-chain state through [`degenbot_rpc::contract::Contract`],
//! which surfaces RPC + ABI failures as
//! [`degenbot_core::errors::ProviderError`]. This module adds a price-specific
//! error that also carries the address-validation + typed-decode failures the
//! readers can introduce (enriching messages without losing the original).

use degenbot_core::errors::{AddressError, ProviderError};

/// Errors raised by the Chainlink / Aave price readers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PriceError {
    /// An `eth_call` / RPC failure (revert, transport, retry-exhausted, …).
    #[error(transparent)]
    Provider(#[from] ProviderError),

    /// The reader was constructed with a malformed contract address.
    #[error("invalid contract address: {0}")]
    Address(#[from] AddressError),

    /// The decoded return did not match the expected ABI shape
    /// (wrong value count, wrong `AbiValue` variant, or an integer that does
    /// not narrow to the target Rust type).
    #[error("price-feed decode error: {0}")]
    Decode(String),
}

/// Convenience alias used throughout the crate.
pub type PriceResult<T> = Result<T, PriceError>;
