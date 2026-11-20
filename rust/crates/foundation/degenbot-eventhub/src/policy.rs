//! Overflow vocabulary and hub registration errors.

use crate::event::PendingTxSource;

/// How a registered source treats a consumer that falls behind.
///
/// A pending ring's overflow behavior is fixed by the ring itself (bounded,
/// drop-oldest, capacity a [`std::num::NonZeroUsize`]); the policy vocabulary
/// names the one channel kind whose posture is a deliberate audit flag rather
/// than a bound: a named engine channel, which is unbounded by construction.
/// `name` is the label the choice is recorded under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowPolicy {
    /// Unbounded buffer, intentionally flagged under `name` for audit.
    UnboundedFlagged {
        /// Audit label naming the deliberate unbounded choice.
        name: &'static str,
    },
}

impl OverflowPolicy {
    /// The declared label, if the policy carries one.
    #[must_use]
    pub fn name(&self) -> Option<&'static str> {
        match self {
            Self::UnboundedFlagged { name } => Some(name),
        }
    }

    /// Whether this is the deliberate unbounded audit posture.
    #[must_use]
    pub fn is_unbounded_flagged(&self) -> bool {
        matches!(self, Self::UnboundedFlagged { .. })
    }
}

/// Hub registration failures.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HubError {
    /// A ring for this source kind is already open on this hub.
    #[error("pending-tx ring for {0:?} is already open on this hub")]
    AlreadyRegistered(PendingTxSource),
    /// No named source channel is registered under this name.
    #[error("named hub channel {0} is not registered on this hub")]
    NamedNotRegistered(&'static str),
    /// The requested payload type does not match the named channel's
    /// registration.
    #[error("named hub channel {0} carries a different payload type")]
    NamedTypeMismatch(&'static str),
}
