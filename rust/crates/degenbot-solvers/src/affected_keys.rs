//! Tiny container for the set of pool keys affected by a single engine event,
//! and the path-index key type the affected-path derivation is keyed on
//!.

use crate::mixed::HopType;

/// One affected (hop family, pool id) key — the `pool_to_paths` reverse
/// index key (the "path-index role" of this module). The affected-path
/// derivation is DELTA-DRIVEN since epic MROOY7 task LXDY4C: log application
/// records `AffectedKey`s into the block's
/// `degenbot_bot::bot_core::EpochDelta` as a byproduct of
/// `dispatch_log`, and the drain's derivation consumes the delta's taken
/// keys directly (no dirty-set intake, no subscriber-side classification).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AffectedKey {
    hop: HopType,
    pool_id: u64,
}

impl AffectedKey {
    /// The key of `pool_id` in hop family `hop`.
    #[must_use]
    pub const fn new(hop: HopType, pool_id: u64) -> Self {
        Self { hop, pool_id }
    }

    /// The hop family of this key.
    #[must_use]
    pub const fn hop(&self) -> HopType {
        self.hop
    }

    /// The pool id of this key.
    #[must_use]
    pub const fn pool_id(&self) -> u64 {
        self.pool_id
    }

    /// The `pool_to_paths` reverse-index key this key resolves to.
    #[must_use]
    pub const fn path_index_key(&self) -> (HopType, u64) {
        (self.hop, self.pool_id)
    }
}

/// Up to two pool keys affected by one engine event.
///
/// V2 and V4 engines maintain dual-orientation state (forward + reverse),
/// so a single event can dirty two keys. V3 engines use a single key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AffectedKeys {
    keys: [u64; 2],
    len: u8,
}

impl AffectedKeys {
    /// No affected keys (e.g., event for an unregistered pool).
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            keys: [0; 2],
            len: 0,
        }
    }

    /// Exactly one affected key (V3 engines).
    #[must_use]
    pub const fn single(key: u64) -> Self {
        Self {
            keys: [key, 0],
            len: 1,
        }
    }

    /// Two affected keys (V2/V4 dual-orientation engines).
    #[must_use]
    pub const fn pair(k1: u64, k2: u64) -> Self {
        Self {
            keys: [k1, k2],
            len: 2,
        }
    }

    /// Iterate over affected keys.
    pub fn iter(&self) -> impl Iterator<Item = u64> + '_ {
        self.keys[..usize::from(self.len)].iter().copied()
    }

    /// Whether no keys are affected.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}
