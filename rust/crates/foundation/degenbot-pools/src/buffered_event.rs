//! The family-tagged buffered-event payload.
//!
//! The two concentrated-liquidity designs decode liquidity events into
//! different shapes, so an event carries its family with it: one door takes the
//! payload, and the key says which pool. A key whose family disagrees with the
//! payload is a caller bug, refused rather than silently dropped.

use crate::registry::RegisteredPoolFamily;
use crate::v3_state::BufferedV3PoolEvent;
use crate::v4_state::BufferedV4PoolEvent;

/// A buffered concentrated-liquidity event, tagged with its family.
#[derive(Clone, Debug)]
pub enum BufferedPoolEvent {
    /// An address-keyed (V3) event.
    V3(BufferedV3PoolEvent),
    /// A V4 (`PoolManager` + pool id) event.
    V4(BufferedV4PoolEvent),
}

impl BufferedPoolEvent {
    /// The family this event belongs to.
    #[must_use]
    pub const fn family(&self) -> RegisteredPoolFamily {
        match self {
            Self::V3(_) => RegisteredPoolFamily::V3,
            Self::V4(_) => RegisteredPoolFamily::V4,
        }
    }
}
