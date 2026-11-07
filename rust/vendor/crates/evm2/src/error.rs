//! Owned errors crossing database and execution boundaries.

use alloc::{string::String, sync::Arc};
use core::{error::Error, fmt};

/// Type-erased host error.
#[derive(Clone, Debug)]
pub struct AnyError(Arc<dyn Error + Send + Sync>);

impl fmt::Display for AnyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl Error for AnyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.0.source()
    }
}

impl PartialEq for AnyError {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for AnyError {}

impl AnyError {
    /// Creates a new [`AnyError`] from any error type.
    pub fn new(err: impl Error + Send + Sync + 'static) -> Self {
        Self(Arc::new(err))
    }

    /// Returns the original error when it has type `E`.
    pub fn downcast_ref<E: Error + 'static>(&self) -> Option<&E> {
        self.0.downcast_ref()
    }
}

struct StringError(String);

impl Error for StringError {}

impl fmt::Display for StringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

// Purposefully skip printing "StringError(..)"
impl fmt::Debug for StringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl From<String> for AnyError {
    fn from(value: String) -> Self {
        Self::new(StringError(value))
    }
}

impl From<&str> for AnyError {
    fn from(value: &str) -> Self {
        Self::new(StringError(value.into()))
    }
}

/// An owned database failure, retaining whether it invalidates input or indicates an internal
/// failure.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{error}")]
pub struct DatabaseError {
    #[source]
    error: AnyError,
    fatal: bool,
}

impl DatabaseError {
    /// Wraps a database error and its classification before erasing the concrete type.
    pub fn new(error: impl Error + Send + Sync + 'static, fatal: bool) -> Self {
        if let Some(error) = (&error as &dyn Error).downcast_ref::<Self>() {
            return error.clone();
        }
        Self { error: AnyError::new(error), fatal }
    }

    /// Whether the failure is internal, rather than invalid execution input.
    pub const fn is_fatal(&self) -> bool {
        self.fatal
    }

    /// Returns the concrete error when its type matches.
    pub fn downcast_ref<E: Error + 'static>(&self) -> Option<&E> {
        self.error.downcast_ref()
    }
}

/// A state load can be skipped before reading a cold account or slot.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LoadError {
    /// The requested cold load was skipped without reading the database.
    #[error("cold load skipped")]
    ColdLoadSkipped,
    /// The backing database failed.
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

/// An error that aborts execution instead of becoming an EVM revert or halt.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ExecutionError {
    /// An owned, classified database error.
    #[error(transparent)]
    Database(#[from] DatabaseError),
    /// An unrecoverable precompile or execution failure.
    #[error("{0}")]
    Fatal(#[source] AnyError),
}

/// Failure of an interpreter host operation.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    /// A normal EVM stop, such as out of gas.
    #[error("{0:?}")]
    Halt(crate::interpreter::InstrStop),
    /// An error that must escape the EVM call stack.
    #[error(transparent)]
    Execution(#[from] ExecutionError),
}

impl From<DatabaseError> for HostError {
    fn from(error: DatabaseError) -> Self {
        Self::Execution(error.into())
    }
}

impl From<LoadError> for HostError {
    fn from(error: LoadError) -> Self {
        match error {
            LoadError::ColdLoadSkipped => Self::Halt(crate::interpreter::InstrStop::OutOfGas),
            LoadError::Database(error) => error.into(),
        }
    }
}

impl From<crate::interpreter::InstrStop> for HostError {
    fn from(stop: crate::interpreter::InstrStop) -> Self {
        Self::Halt(stop)
    }
}
