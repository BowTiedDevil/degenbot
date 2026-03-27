//! Error types for degenbot Rust extensions.

use pyo3::{exceptions::PyValueError, PyErr};

/// Errors that can occur during tick math calculations.
#[derive(Debug, thiserror::Error)]
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
#[derive(Debug, thiserror::Error)]
pub enum AbiDecodeError {
    /// Failed to parse an ABI type string.
    #[error("Failed to parse ABI type: {0}")]
    InvalidType(String),
    /// Decoding operation failed.
    #[error("Decoding failed: {0}")]
    DecodeError(String),
    /// Insufficient data provided for decoding.
    #[error("Insufficient data for decoding")]
    InsufficientData,
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
