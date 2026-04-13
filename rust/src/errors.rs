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
        // Preserve the specific error type and message
        match err {
            TickMathError::InvalidTick(tick) => {
                Self::new::<PyValueError, _>(format!(
                    "Invalid tick value: {tick}. Must be in range [-887272, 887272]"
                ))
            }
            TickMathError::SqrtRatioOutOfBounds => {
                Self::new::<PyValueError, _>("Sqrt ratio out of bounds")
            }
        }
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

    /// Serialization error.
    #[error("Serialization error: {message}")]
    SerializationError { message: String },

    /// Other error.
    #[error("{message}")]
    Other { message: String },
}

impl From<ProviderError> for PyErr {
    fn from(err: ProviderError) -> Self {
        let msg = format!("Provider error: {err}");
        match err {
            ProviderError::Timeout { .. } => Self::new::<pyo3::exceptions::PyTimeoutError, _>(msg),
            ProviderError::ConnectionFailed { .. } => {
                Self::new::<pyo3::exceptions::PyConnectionError, _>(msg)
            }
            ProviderError::RateLimited { .. }
            | ProviderError::RpcError { .. }
            | ProviderError::SerializationError { .. }
            | ProviderError::InvalidResponse { .. }
            | ProviderError::AnvilError { .. }
            | ProviderError::Other { .. } => Self::new::<pyo3::exceptions::PyRuntimeError, _>(msg),
            _ => Self::new::<PyValueError, _>(msg),
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
        Self::Other {
            message: err.to_string(),
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
            crate::errors::AbiDecodeError::UnsupportedType(msg) => Self::InvalidAbi { message: msg },
            crate::errors::AbiDecodeError::InvalidLength(msg)
            | crate::errors::AbiDecodeError::InvalidOffset(msg) => Self::DecodingError { message: msg },
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
