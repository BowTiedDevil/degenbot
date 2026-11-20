//! The typed pending-transaction ring: one publisher, one drainer.
//!
//! A [`PendingTxPublisher`] is the source's end — the feed owns it, clones it,
//! and publishes every parsed frame. A [`PendingTxView`] is the consumer's end
//! and is deliberately NOT `Clone`: one ring has exactly one drainer, so a
//! second consumer cannot silently steal (or race) another's corpus.
//!
//! The ring is a bounded drop-oldest buffer whose capacity is a
//! [`NonZeroUsize`], so a zero-capacity ring is unrepresentable rather than a
//! clamped surprise. Evictions are counted on the publisher, so feed
//! telemetry needs no consumer view to exist.

use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::event::PendingTx;

/// One source kind's bounded pending-transaction ring.
pub(crate) struct PendingTxRing {
    capacity: NonZeroUsize,
    frames: Mutex<VecDeque<PendingTx>>,
    dropped: AtomicU64,
}

impl PendingTxRing {
    pub(crate) fn new(capacity: NonZeroUsize) -> Self {
        Self {
            capacity,
            frames: Mutex::new(VecDeque::with_capacity(capacity.get())),
            dropped: AtomicU64::new(0),
        }
    }

    fn push(&self, frame: PendingTx) {
        let mut frames = self.frames.lock();
        if frames.len() >= self.capacity.get() {
            frames.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        frames.push_back(frame);
    }

    fn drain(&self) -> Vec<PendingTx> {
        std::mem::take(&mut *self.frames.lock()).into()
    }
}

/// The publishing end of one pending ring.
///
/// Cloneable because a pump and its feed loop publish to the same ring; the
/// ring lives exactly as long as some end holds it.
#[derive(Clone)]
pub struct PendingTxPublisher {
    ring: Arc<PendingTxRing>,
}

impl PendingTxPublisher {
    pub(crate) fn new(ring: Arc<PendingTxRing>) -> Self {
        Self { ring }
    }

    /// Append a frame, evicting (and counting) the oldest at capacity.
    pub fn push(&self, frame: PendingTx) {
        self.ring.push(frame);
    }

    /// Evictions counted on this ring.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.ring.dropped.load(Ordering::Relaxed)
    }
}

/// The draining end of one pending ring.
///
/// Deliberately not `Clone`: one ring has one drainer, which is a compile-time
/// fact of this interface rather than a runtime re-proof.
pub struct PendingTxView {
    ring: Arc<PendingTxRing>,
}

impl PendingTxView {
    pub(crate) fn new(ring: Arc<PendingTxRing>) -> Self {
        Self { ring }
    }

    /// Take every buffered frame, oldest first, atomically.
    #[must_use]
    pub fn drain(&self) -> Vec<PendingTx> {
        self.ring.drain()
    }

    /// Evictions counted on this ring.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.ring.dropped.load(Ordering::Relaxed)
    }

    /// The ring's configured capacity.
    #[must_use]
    pub fn capacity(&self) -> NonZeroUsize {
        self.ring.capacity
    }
}

/// A ring no hub tracks: a feed that is its own host owns both ends.
///
/// The one eager path (the Python single-feed adapter) is exactly this: it
/// spawns one feed and drains the one view the ring hands back. A process host
/// that owns a shared [`crate::Hub`] opens its ring through
/// [`crate::Hub::open_pending_tx_ring`] instead, so a second consumer of one
/// kind is refused at the hub rather than silently sharing a corpus.
#[must_use]
pub fn detached_pending_ring(capacity: NonZeroUsize) -> (PendingTxPublisher, PendingTxView) {
    let ring = Arc::new(PendingTxRing::new(capacity));
    (
        PendingTxPublisher::new(Arc::clone(&ring)),
        PendingTxView::new(ring),
    )
}
