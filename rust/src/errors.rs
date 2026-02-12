//! Error types for tick math calculations.

use pyo3::{PyErr, exceptions::PyValueError};

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
