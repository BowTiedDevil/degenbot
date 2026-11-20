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

use parking_lot::Mutex;
use tokio::sync::watch;

/// One observed block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    /// The block number every consumer keys on.
    pub number: u64,
    /// The header fields the observation carried, when it carried a full
    /// header. A number-only poll (`eth_blockNumber`) carries `None`.
    pub metadata: Option<HeadMetadata>,
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
        self.rx.borrow().clone()
    }

    /// The current head, marking it seen for the next [`Self::changed`].
    pub fn borrow_and_update(&mut self) -> Option<Head> {
        self.rx.borrow_and_update().clone()
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
