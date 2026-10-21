//! The one state-update payload an orchestration verb takes.
//!
//! A wholesale pool-state sync is family-shaped: the two concentrated-liquidity
//! designs carry the same five fields but as different types, so the payload is
//! an enum instead of two verbs. Naming a pool stays the caller's job
//! (`PoolLookup` in `degenbot-substrate`); this type only says *what* to write.
//!
//! A key whose family disagrees with the payload is a caller bug, not a silent
//! no-op — the refusal carries both families so the error names the mistake.

use crate::registry::RegisteredPoolFamily;
use crate::v3_state::V3StateSync;
use crate::v4_state::V4StateSync;

/// A wholesale pool-state overwrite.
#[derive(Clone, Debug)]
pub enum PoolStateUpdate {
    /// A V3 (address-keyed) state overwrite.
    V3(V3StateSync),
    /// A V4 (`PoolManager` + pool id) state overwrite.
    V4(V4StateSync),
}

impl PoolStateUpdate {
    /// The family whose state this update replaces.
    #[must_use]
    pub const fn family(&self) -> RegisteredPoolFamily {
        match self {
            Self::V3(_) => RegisteredPoolFamily::V3,
            Self::V4(_) => RegisteredPoolFamily::V4,
        }
    }
}

/// Why a family-tagged payload was refused.
///
/// One type serves every door that takes a payload beside a pool key: a
/// mismatch is a caller bug either way, and a second refusal type would be a
/// mirror.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolRefusal {
    /// The payload names a different family than the pool key does.
    FamilyMismatch {
        /// The family the key names.
        key: RegisteredPoolFamily,
        /// The family the payload names.
        value: RegisteredPoolFamily,
    },
    /// The key names no registered pool.
    Unregistered,
}

impl core::fmt::Display for PoolRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::FamilyMismatch { key, value } => write!(
                f,
                "payload for a {} pool applied to a {} pool key",
                value.tag(),
                key.tag()
            ),
            Self::Unregistered => f.write_str("payload for an unregistered pool"),
        }
    }
}

impl std::error::Error for PoolRefusal {}
