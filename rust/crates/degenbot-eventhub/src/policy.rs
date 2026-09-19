//! Overflow vocabulary and hub registration errors.

use crate::event::HubClass;

/// How a registered source treats a consumer that falls behind.
///
/// Declared once per source at registration ([`crate::Hub::register_source`]),
/// so the strictness is a property of the *registration*, not a per-call
/// choice. `name` is the metric/audit label a channel's drops are counted
/// under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowPolicy {
    /// Bounded ring; the oldest buffered event is evicted (and counted under
    /// `name`) to make room for the newest.
    DropOldestCounted {
        /// Counter label for evictions.
        name: &'static str,
    },
    /// Keep only the most recent event; older values are superseded.
    LatestOnly,
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
            Self::DropOldestCounted { name } | Self::UnboundedFlagged { name } => Some(name),
            Self::LatestOnly => None,
        }
    }

    /// Whether this is the deliberate unbounded audit posture.
    #[must_use]
    pub fn is_unbounded_flagged(&self) -> bool {
        matches!(self, Self::UnboundedFlagged { .. })
    }
}

/// Hub registration and subscription failures.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HubError {
    /// A source for this class is already registered on this hub.
    #[error("event class {0:?} is already registered on this hub")]
    AlreadyRegistered(HubClass),
    /// No source has registered this class.
    #[error("event class {0:?} is not registered on this hub")]
    NotRegistered(HubClass),
    /// The requested handle does not match the policy declared at registration.
    #[error("hub policy mismatch: expected {expected}")]
    PolicyMismatch {
        /// The handle kind the caller asked for.
        expected: &'static str,
    },
    /// No named source channel is registered under this name.
    #[error("named hub channel {0} is not registered on this hub")]
    NamedNotRegistered(&'static str),
    /// The requested payload type does not match the named channel's
    /// registration.
    #[error("named hub channel {0} carries a different payload type")]
    NamedTypeMismatch(&'static str),
}
