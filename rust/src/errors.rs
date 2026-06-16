//! Error types for degenbot Rust extensions.

use pyo3::{exceptions::PyValueError, PyErr};

/// Errors that can occur during tick math calculations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TickMathError {
    /// Invalid tick value outside the valid range [-887272, 887272].
    #[error("Invalid tick value: {0}. Must be in range [-887272, 887272]")]
    InvalidTick(i32),
    /// Sqrt ratio value outside the valid [`MIN_SQRT_RATIO`, `MAX_SQRT_RATIO`) range.
    #[error("Sqrt ratio {actual} out of bounds [{min}, {max})")]
    SqrtRatioOutOfBounds {
        /// The value that was out of bounds.
        actual: alloy::primitives::aliases::U160,
        /// Minimum valid sqrt ratio.
        min: alloy::primitives::aliases::U160,
        /// Maximum valid sqrt ratio (exclusive).
        max: alloy::primitives::aliases::U160,
    },
}

/// Errors from the concentrated-liquidity math library.
#[derive(Debug, thiserror::Error, PartialEq)]
#[non_exhaustive]
pub enum ClMathError {
    /// Division by zero.
    #[error("DIVISION BY ZERO")]
    DivisionByZero,
    /// Result would overflow uint256.
    #[error("product > MAX_UINT256")]
    Uint256Overflow,
    /// Zero input where a non-zero value is required.
    #[error("Zero input")]
    ZeroInput,
    /// Invalid price (zero where non-zero is required).
    #[error("InvalidPrice")]
    InvalidPrice,
    /// Invalid price or liquidity (either is zero where non-zero is required).
    #[error("InvalidPriceOrLiquidity")]
    InvalidPriceOrLiquidity,
    /// Price overflow (exceeds uint160 or subtraction underflow).
    #[error("PriceOverflow")]
    PriceOverflow,
    /// Result overflowed uint160.
    #[error("Result overflowed MAX_UINT160")]
    ResultOverflowedUint160,
    /// Insufficient liquidity for the requested output.
    #[error("NotEnoughLiquidity")]
    InsufficientLiquidity,
    /// Invalid liquidity (negative where non-negative is required).
    #[error("InvalidLiquidity")]
    InvalidLiquidity,
    /// Safe cast overflow (value doesn't fit in target type).
    #[error("SafeCastOverflow")]
    SafeCastOverflow,
}

impl From<ClMathError> for PyErr {
    fn from(err: ClMathError) -> Self {
        Self::new::<PyValueError, _>(err.to_string())
    }
}

impl From<TickMathError> for ClMathError {
    fn from(err: TickMathError) -> Self {
        match err {
            TickMathError::InvalidTick(_) | TickMathError::SqrtRatioOutOfBounds { .. } => Self::InvalidPrice,
        }
    }
}

impl From<TickMathError> for PyErr {
    fn from(err: TickMathError) -> Self {
        // Forward the Display impl so the Python error message matches
        // the Rust error message exactly.
        Self::new::<PyValueError, _>(err.to_string())
    }
}

/// Errors that can occur during ABI decoding.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AbiDecodeError {
    /// Empty types list provided.
    #[error("Types list cannot be empty")]
    EmptyTypesList,
    /// Empty data provided for decoding.
    #[error("Data cannot be empty")]
    EmptyData,
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
        // Preserve error context by forwarding the Display impl
        Self::new::<PyValueError, _>(err.to_string())
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
        // Preserve original error message with full context
        Self::new::<PyValueError, _>(err.to_string())
    }
}

/// Errors that can occur during provider operations.
#[derive(Debug, thiserror::Error, Clone)]
#[non_exhaustive]
pub enum ProviderError {
    /// Request timeout.
    #[error("Request timeout: {message}")]
    Timeout { message: String },

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

    /// Encoding error.
    #[error("Encoding error: {message}")]
    EncodingError { message: String },

    /// Decoding error.
    #[error("Decoding error: {message}")]
    DecodingError { message: String },

    /// Serialization error.
    #[error("Serialization error: {message}")]
    SerializationError { message: String },

    /// Subscription not supported (e.g., HTTP transport).
    #[error("Subscription not supported: {message}")]
    SubscriptionNotSupported { message: String },

    /// Other error.
    #[error("{message}")]
    Other { message: String },
}

impl ProviderError {
    /// Returns `true` if this error is retryable (rate-limited, timeout, connection failure).
    ///
    /// Used by the retry-with-backoff loop to decide whether to sleep and retry
    /// or surface the error immediately.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. } | Self::Timeout { .. } | Self::ConnectionFailed { .. }
        )
    }
}

impl From<ProviderError> for PyErr {
    fn from(err: ProviderError) -> Self {
        let msg = format!("Provider error: {err}");
        match err {
            ProviderError::Timeout { .. } => Self::new::<pyo3::exceptions::PyTimeoutError, _>(msg),
            ProviderError::ConnectionFailed { .. } => {
                Self::new::<pyo3::exceptions::PyConnectionError, _>(msg)
            }
            ProviderError::SubscriptionNotSupported { .. } => {
                Self::new::<pyo3::exceptions::PyRuntimeError, _>(msg)
            }
            ProviderError::RateLimited { .. }
            | ProviderError::RpcError { .. }
            | ProviderError::SerializationError { .. }
            | ProviderError::InvalidResponse { .. }
            | ProviderError::AnvilError { .. }
            | ProviderError::Other { .. }
            | ProviderError::InvalidBlockRange { .. }
            | ProviderError::InvalidAddress { .. }
            | ProviderError::InvalidTopic { .. }
            | ProviderError::InvalidParams { .. }
            | ProviderError::InvalidAbi { .. }
            | ProviderError::EncodingError { .. }
            | ProviderError::DecodingError { .. } => Self::new::<pyo3::exceptions::PyRuntimeError, _>(msg),
        }
    }
}

/// Result type alias for provider operations.
pub type ProviderResult<T> = Result<T, ProviderError>;

/// Errors that can occur during contract ABI encoding/decoding.
#[derive(Debug, thiserror::Error, Clone)]
#[non_exhaustive]
pub enum ContractError {
    /// Invalid ABI type or value.
    #[error("Invalid ABI: {message}")]
    InvalidAbi {
        /// Description of the ABI validation failure.
        message: String,
    },
    /// Invalid address format.
    #[error("Invalid address: {address} - {reason}")]
    InvalidAddress {
        /// The address string that failed validation.
        address: String,
        /// Why the address is invalid.
        reason: String,
    },
    /// Encoding failure.
    #[error("Encoding error: {message}")]
    EncodingError {
        /// Description of the encoding failure.
        message: String,
    },
    /// Decoding failure.
    #[error("Decoding error: {message}")]
    DecodingError {
        /// Description of the decoding failure.
        message: String,
    },
}

impl From<ContractError> for ProviderError {
    fn from(err: ContractError) -> Self {
        match err {
            ContractError::InvalidAbi { message } => Self::InvalidAbi { message },
            ContractError::InvalidAddress { address, reason } => {
                Self::InvalidAddress { address, reason }
            }
            ContractError::EncodingError { message } => Self::EncodingError { message },
            ContractError::DecodingError { message } => Self::DecodingError { message },
        }
    }
}

impl From<ContractError> for PyErr {
    fn from(err: ContractError) -> Self {
        Self::new::<PyValueError, _>(format!("{err}"))
    }
}

impl From<crate::errors::AbiDecodeError> for ContractError {
    fn from(err: crate::errors::AbiDecodeError) -> Self {
        match err {
            crate::errors::AbiDecodeError::UnsupportedType(msg) => {
                Self::InvalidAbi { message: msg }
            }
            crate::errors::AbiDecodeError::InvalidLength(msg)
            | crate::errors::AbiDecodeError::InvalidOffset(msg) => {
                Self::DecodingError { message: msg }
            }
            crate::errors::AbiDecodeError::InsufficientData { .. } => Self::DecodingError {
                message: err.to_string(),
            },
            other => Self::DecodingError {
                message: other.to_string(),
            },
        }
    }
}

/// Result type alias for contract ABI encoding/decoding operations.
pub type ContractResult<T> = Result<T, ContractError>;
