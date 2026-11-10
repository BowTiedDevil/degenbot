//! The canonical-head fact and the one sink that carries it.
//!
//! A head is a *chain* fact, not an engine epoch: it is the identity a caller
//! fences a read against (block number AND hash, plus the parent hash its
//! lineage is verified by). The pump learns it for free — the alloy header it
//! already holds carries all three — so this type exists to move that identity
//! across crate boundaries without a second header read and without either side
//! learning the other's storage.
//!
//! [`CanonicalHeadSink`] is deliberately the smallest possible surface: one
//! method, no return value, no async. A consumer that must do real work on a
//! head owns the spawning; the producer (the block pump's header arm) must never
//! await storage work on the block clock.

use alloy::primitives::B256;

/// One accepted canonical head: the identity a read is fenced against.
///
/// "Accepted" is the pump's verdict, not the chain's: a header the driver
/// dropped as duplicate/stale, or a block a gap-backfill covered without its own
/// header, is not a head this value ever names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CanonicalHead {
    /// The block number the header was mined at.
    pub number: u64,
    /// The header's own hash — half of the exact anchor a read fences against.
    pub hash: B256,
    /// The hash of the header's parent, which is what makes a lineage link
    /// verifiable rather than merely consecutive.
    pub parent_hash: B256,
}

impl CanonicalHead {
    /// A head at `number` with the given identity.
    #[must_use]
    pub const fn new(number: u64, hash: B256, parent_hash: B256) -> Self {
        Self {
            number,
            hash,
            parent_hash,
        }
    }
}

/// What a session installs to be told about every accepted canonical head.
///
/// A sink is a *consumer* of the chain fact, never a producer of it: it is
/// called synchronously from the header arm and must therefore not block on
/// provider work. Uninstalled is a hard no-op at the producer (there is no
/// placeholder implementation to forget), which is what makes an opted-out
/// session structurally request-free rather than branch-free.
pub trait CanonicalHeadSink: Send + Sync + 'static {
    /// One accepted canonical head arrived.
    fn on_canonical_head(&self, head: CanonicalHead);
}
