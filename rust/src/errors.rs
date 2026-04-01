//! Error types for degenbot Rust extensions.

use pyo3::{exceptions::PyValueError, PyErr};

/// Errors that can occur during tick math calculations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TickMathError {
    /// Invalid tick value outside the valid range [-887272, 887272].
    #[error("Invalid tick value: {0}")]
    InvalidTick(i32),
    /// Sqrt ratio value outside the valid [`MIN_SQRT_RATIO`, `MAX_SQRT_RATIO`) range.
    #[error("Sqrt ratio out of bounds")]
    SqrtRatioOutOfBounds,
}

impl From<TickMathError> for PyErr {
    fn from(err: TickMathError) -> Self {
        Self::new::<PyValueError, _>(format!("Tick calculation error: {err}"))
    }
}

/// Errors that can occur during ABI decoding.
#[derive(Debug, thiserror::Error, Clone)]
#[non_exhaustive]
pub enum AbiDecodeError {
    /// Invalid array size in type string.
    #[error("Invalid array size in type: {0}")]
    InvalidArraySize(String),
    /// Insufficient data provided for decoding.
    #[error("Insufficient data: need {needed} bytes at offset {offset}, have {have} bytes")]
    InsufficientData {
        /// Number of bytes needed.
        needed: usize,
        /// Number of bytes available.
        have: usize,
        /// Offset where data was needed.
        offset: usize,
    },
    /// Invalid offset value in encoded data.
    #[error("Invalid offset: {0}")]
    InvalidOffset(String),
    /// Invalid length value in encoded data.
    #[error("Invalid length: {0}")]
    InvalidLength(String),
    /// Unsupported or invalid ABI type.
    #[error("Unsupported type: {0}")]
    UnsupportedType(String),
    /// Fixed-point types are not yet implemented.
    #[error("Fixed-point types are not yet implemented")]
    FixedPointNotImplemented,
    /// Non-strict decoding mode is not yet implemented.
    #[error("Non-strict decoding mode is not yet implemented")]
    NonStrictNotImplemented,
}

impl From<AbiDecodeError> for PyErr {
    fn from(err: AbiDecodeError) -> Self {
        Self::new::<PyValueError, _>(format!("ABI decode error: {err}"))
    }
}

/// Errors that can occur during address operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AddressError {
    /// Invalid address string format.
    #[error("Invalid address string: {0}")]
    InvalidAddress(String),
    /// Address bytes must be exactly 20 bytes.
    #[error("Address must be exactly 20 bytes, got {0} bytes")]
    InvalidByteLength(usize),
}

impl From<AddressError> for PyErr {
    fn from(err: AddressError) -> Self {
        Self::new::<PyValueError, _>(format!("Address error: {err}"))
    }
}

/// Errors that can occur during provider operations.
#[derive(Debug, thiserror::Error, Clone)]
#[non_exhaustive]
pub enum ProviderError {
    /// Request timeout.
    #[error("Request timeout after {timeout}s: {message}")]
    Timeout { timeout: u64, message: String },

    /// Rate limit exceeded.
    #[error("Rate limited: {message}")]
    RateLimited { message: String },

    /// Connection failed.
    #[error("Connection failed: {message}")]
    ConnectionFailed { message: String },

    /// Invalid response from RPC.
    #[error("Invalid response: {message}")]
    InvalidResponse { message: String },

    /// RPC error with code and message.
    #[error("RPC error: {code} - {message}")]
    RpcError { code: i64, message: String },

    /// Invalid block range.
    #[error("Invalid block range: {from} to {to}")]
    InvalidBlockRange { from: u64, to: u64 },

    /// Invalid Ethereum address format.
    #[error("Invalid address format: {address} - {reason}")]
    InvalidAddress { address: String, reason: String },

    /// Invalid topic format.
    #[error("Invalid topic format: {topic} - {reason}")]
    InvalidTopic { topic: String, reason: String },

    /// Invalid parameters.
    #[error("Invalid parameters: {message}")]
    InvalidParams { message: String },

    /// Anvil-specific error.
    #[error("Anvil error: {message}")]
    AnvilError { message: String },

    /// Invalid ABI.
    #[error("Invalid ABI: {message}")]
    InvalidAbi { message: String },

    /// Decoding error.
    #[error("Decoding error: {message}")]
    DecodingError { message: String },

    /// Other error.
    #[error("{message}")]
    Other { message: String },
}

impl From<ProviderError> for PyErr {
    fn from(err: ProviderError) -> Self {
        PyValueError::new_err(err.to_string())
    }
}

/// Result type alias for provider operations.
pub type ProviderResult<T> = Result<T, ProviderError>;
