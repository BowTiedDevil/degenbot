//! The hub-owned head clock.
//!
//! The hub owns the head clock: the permanent claim, the shared receiver
//! prototype, and the staleness instant. A transport claims the hub's one
//! [`HeadSender`] and publishes every observed head — the sidecar's
//! `degenbot_rpc::head_watch` for a watched chain, the engine block pump's
//! accepted-header edge for a hosted engine. A consumer holds a
//! [`HeadSubscription`] and awaits [`HeadSubscription::changed`].
//!
//! Publisher and subscriber state are separate: the shared state holds a
//! `watch` receiver prototype and one staleness [`Instant`], never a sender, so
//! a subscription cannot keep the clock alive. A claimed clock's subscriptions
//! close when every publisher drops; a never-claimed clock closes them when the
//! hub drops its slot sender.

use std::sync::Arc;
use std::time::{Duration, Instant};

use degenbot_core::canonical_head::ChainAnchor;
use parking_lot::Mutex;
use tokio::sync::watch;

/// One observed block: the exact chain identity every consumer keys on, beside
/// the header facts the observation happened to carry.
///
/// The number is NOT a field: it is the anchor's height, so a head can never say
/// one height and identity another. A publisher that cannot name the block
/// exactly publishes nothing (ADR-105 D1) — a number-only observation is
/// resolved before it reaches this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Head {
    /// The exact block identity: chain, height, own hash, parent hash.
    anchor: ChainAnchor,
    /// The header fields the observation carried, when it carried a full
    /// header. A resolved number-only poll carries `None`.
    metadata: Option<HeadMetadata>,
}

impl Head {
    /// One observed block, stated as its exact anchor plus the header facts the
    /// observation carried (if any).
    #[must_use]
    pub const fn new(anchor: ChainAnchor, metadata: Option<HeadMetadata>) -> Self {
        Self { anchor, metadata }
    }

    /// The anchor this head was published under.
    #[must_use]
    pub const fn anchor(&self) -> &ChainAnchor {
        &self.anchor
    }

    /// The block number every consumer keys on: the anchor's height.
    #[must_use]
    pub const fn number(&self) -> u64 {
        self.anchor.number()
    }

    /// The header facts the observation carried, when it carried a full header.
    #[must_use]
    pub fn metadata(&self) -> Option<&HeadMetadata> {
        self.metadata.as_ref()
    }
}

/// The header fields a full `newHeads` observation carries beside the number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadMetadata {
    /// Block timestamp (seconds).
    pub timestamp: u64,
    /// Base fee per gas (`None` pre-EIP-1559).
    pub base_fee_per_gas: Option<u64>,
    /// Gas used by the block.
    pub gas_used: u64,
    /// Gas limit of the block.
    pub gas_limit: u64,
}

/// The receiver prototype and staleness instant every subscription shares.
pub(crate) struct HeadShared {
    prototype: watch::Receiver<Option<Head>>,
    last_seen: Mutex<Instant>,
}

impl HeadShared {
    pub(crate) fn new(sender: &watch::Sender<Option<Head>>) -> Self {
        Self {
            prototype: sender.subscribe(),
            last_seen: Mutex::new(Instant::now()),
        }
    }

    /// Seed the staleness clock: a claim is a fresh start for the clock.
    pub(crate) fn reset_clock(&self) {
        *self.last_seen.lock() = Instant::now();
    }

    /// Refresh the staleness clock after a publish.
    pub(crate) fn touch(&self) {
        *self.last_seen.lock() = Instant::now();
    }

    fn elapsed(&self) -> Duration {
        self.last_seen.lock().elapsed()
    }

    /// A subscription at the clock's CURRENT value: the prototype is cloned and
    /// immediately marked seen, so a late subscription does not replay the
    /// current head as a new edge.
    pub(crate) fn subscribe(self: &Arc<Self>) -> HeadSubscription {
        let mut rx = self.prototype.clone();
        rx.borrow_and_update();
        HeadSubscription {
            rx,
            shared: Arc::clone(self),
        }
    }
}

/// The transport side of the hub head clock: publish each observed head.
#[derive(Clone)]
pub struct HeadSender {
    tx: watch::Sender<Option<Head>>,
    shared: Arc<HeadShared>,
}

impl HeadSender {
    pub(crate) fn new(tx: watch::Sender<Option<Head>>, shared: Arc<HeadShared>) -> Self {
        Self { tx, shared }
    }

    /// Publish the newest head, superseding any previous one, and refresh the
    /// clock every consumer's [`HeadSubscription::stale`] reads.
    pub fn publish(&self, head: Head) {
        self.tx.send_replace(Some(head));
        self.shared.touch();
    }
}

/// The consumer side of the hub head clock.
#[derive(Clone)]
pub struct HeadSubscription {
    rx: watch::Receiver<Option<Head>>,
    shared: Arc<HeadShared>,
}

impl HeadSubscription {
    /// The current head, if any has been published.
    #[must_use]
    pub fn head(&self) -> Option<Head> {
        *self.rx.borrow()
    }

    /// The current head, marking it seen for the next [`Self::changed`].
    pub fn borrow_and_update(&mut self) -> Option<Head> {
        *self.rx.borrow_and_update()
    }

    /// Wait until a newer head is published.
    ///
    /// # Errors
    ///
    /// Fails once every publisher is gone and no newer head will arrive: a
    /// claimed clock's close when its publishers drop, or a never-claimed
    /// clock's close when the hub drops.
    pub async fn changed(&mut self) -> Result<(), watch::error::RecvError> {
        self.rx.changed().await
    }

    /// Whether no head has arrived for at least `threshold`.
    #[must_use]
    pub fn stale(&self, threshold: Duration) -> bool {
        self.shared.elapsed() > threshold
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "test assertions build fixture anchors")]
mod tests {
    use super::*;
    use degenbot_core::canonical_head::ChainAnchor;

    fn anchor(number: u64) -> ChainAnchor {
        ChainAnchor::new(
            1,
            number,
            alloy::primitives::B256::with_last_byte(0xaa),
            alloy::primitives::B256::with_last_byte(0xbb),
        )
        .unwrap()
    }

    #[test]
    fn a_head_derives_its_number_from_its_exact_anchor() {
        let head = Head::new(anchor(101), None);
        assert_eq!(head.number(), 101, "the number is the anchor's height");
        assert_eq!(
            head.anchor().hash(),
            alloy::primitives::B256::with_last_byte(0xaa),
            "the published head names the exact block, not just its height"
        );
        assert_eq!(
            head.anchor().parent_hash(),
            alloy::primitives::B256::with_last_byte(0xbb)
        );
    }

    #[test]
    fn header_metadata_stays_separate_from_identity() {
        let metadata = HeadMetadata {
            timestamp: 7,
            base_fee_per_gas: Some(9),
            gas_used: 11,
            gas_limit: 13,
        };
        let head = Head::new(anchor(102), Some(metadata));
        assert_eq!(
            head.metadata(),
            Some(&metadata),
            "a full header carries its facts"
        );
        assert_eq!(
            Head::new(anchor(102), None).metadata(),
            None,
            "a polled head carries none"
        );
    }
}
