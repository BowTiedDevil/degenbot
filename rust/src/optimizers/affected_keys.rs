//! Tiny container for the set of pool keys affected by a single engine event.

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
        Self { keys: [0; 2], len: 0 }
    }

    /// Exactly one affected key (V3 engines).
    #[must_use]
    pub const fn single(key: u64) -> Self {
        Self { keys: [key, 0], len: 1 }
    }

    /// Two affected keys (V2/V4 dual-orientation engines).
    #[must_use]
    pub const fn pair(k1: u64, k2: u64) -> Self {
        Self { keys: [k1, k2], len: 2 }
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
