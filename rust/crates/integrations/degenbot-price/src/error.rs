//! Error type for the degenbot-price readers.
//!
//! The readers reach on-chain state through [`degenbot_rpc::contract::Contract`],
//! which surfaces RPC + ABI failures as
//! [`degenbot_core::errors::ProviderError`]. This module adds a price-specific
//! error that also carries the address-validation, typed-decode, and
//! feed-data failures the readers can introduce (enriching messages without
//! losing the original).

use alloy::primitives::{Address, I256};
use degenbot_core::errors::{AddressError, ProviderError};

#[cfg(feature = "pyo3")]
use pyo3::exceptions::{PyRuntimeError, PyValueError};
#[cfg(feature = "pyo3")]
use pyo3::PyErr;

/// Why a feed's on-chain data is not representable as a whole-unit price.
///
/// Every case is a property of the data the feed answered, not of the caller's
/// request: the answer is outside what the feed's own `decimals` can price.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FeedDataReason {
    /// The `int256` answer is negative. A USD feed answering below zero is
    /// broken, and there is no non-negative price to report.
    #[error("negative answer")]
    NegativeAnswer,

    /// `10**decimals` overflows `u128`. No deployed aggregator reports more
    /// than 18 decimals, so this is a malformed `decimals()` rather than a
    /// feed shape the reader can scale.
    #[error("10**decimals overflows u128")]
    DecimalsOverflow,

    /// `10**decimals` does not fit the `int256` denominator the correction
    /// divides by.
    #[error("10**decimals does not fit int256")]
    DenominatorConversion,

    /// `10**decimals` is zero, which no power of ten is; the correction would
    /// have no denominator.
    #[error("10**decimals is zero")]
    ZeroDenominator,
}

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

    /// The feed answered, but its data cannot be represented as a price.
    ///
    /// Carries the aggregator and the exact on-chain values that produced the
    /// refusal, so a caller can name the misbehaving feed without re-reading
    /// the chain. Malformed data is never converted into a zero price: `0` is a
    /// price, and one a valuation would act on.
    #[error("price-feed data error for {feed}: {reason} (answer {answer}, decimals {decimals})")]
    FeedData {
        /// The aggregator address the answer came from.
        feed: Address,
        /// The raw `int256` answer as returned on chain.
        answer: I256,
        /// The feed's `decimals()` as returned on chain.
        decimals: u8,
        /// Which representability check the data failed.
        reason: FeedDataReason,
    },
}

/// Convenience alias used throughout the crate.
pub type PriceResult<T> = Result<T, PriceError>;

#[cfg(feature = "pyo3")]
impl From<PriceError> for PyErr {
    fn from(err: PriceError) -> Self {
        match err {
            // Everything wrong with a *value* lands as `ValueError`: the
            // caller's malformed address, a return shape that does not decode,
            // and feed data that cannot be priced. A driver catching
            // `ValueError` around a price read therefore handles both halves of
            // "the feed answered something I cannot use", while a transport
            // failure stays a `RuntimeError`.
            PriceError::Address(_) | PriceError::Decode(_) | PriceError::FeedData { .. } => {
                PyValueError::new_err(err.to_string())
            }
            PriceError::Provider(_) => PyRuntimeError::new_err(err.to_string()),
        }
    }
}
