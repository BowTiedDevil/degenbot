//! The hub-owned head clock.
//!
//! The hub owns the latest head and the staleness clock; a transport (the
//! sidecar's `degenbot_rpc::head_watch`) only performs the `newHeads`
//! subscribe + reconnect and publishes each observed header. [`HeadSender`]
//! is the transport-side sink, [`HeadSubscription`] what a consumer awaits.
//!
//! Both views sit on the hub's generic `NewHead` /
//! [`crate::OverflowPolicy::LatestOnly`] channel, so the overflow semantics
//! (supersede, keep newest) and the registration/`subscribe` accounting are
//! the hub's existing ones.

use std::time::Duration;

use crate::event::HubEvent;
use crate::hub::{LatestReceiver, LatestSender};

/// The transport side of the hub head source: publish each observed header.
#[derive(Clone)]
pub struct HeadSender {
    inner: LatestSender,
}

impl HeadSender {
    pub(crate) fn new(inner: LatestSender) -> Self {
        Self { inner }
    }

    /// Publish the newest head, superseding any previous one.
    pub fn publish(&self, event: HubEvent) {
        self.inner.push(event);
    }

    /// Time since the last published header, or `None` if none was ever seen.
    #[must_use]
    pub fn last_header_age(&self) -> Option<Duration> {
        self.inner.last_seen_age()
    }

    /// Whether no header has arrived for at least `threshold`.
    #[must_use]
    pub fn stale(&self, threshold: Duration) -> bool {
        self.inner.stale(threshold)
    }
}

/// The consumer side of the hub head source.
#[derive(Clone)]
pub struct HeadSubscription {
    inner: LatestReceiver,
}

impl HeadSubscription {
    pub(crate) fn new(inner: LatestReceiver) -> Self {
        Self { inner }
    }

    /// The current head block number, if any header has been published.
    #[must_use]
    pub fn head(&self) -> Option<u64> {
        head_number(self.inner.latest().as_ref())
    }

    /// The current head number, marking the value seen for the next
    /// [`Self::changed`].
    #[must_use]
    pub fn borrow_and_update(&mut self) -> Option<u64> {
        head_number(self.inner.latest_and_update().as_ref())
    }

    /// Wait until a newer header is published.
    ///
    /// # Errors
    ///
    /// Fails once the transport's sender is gone and no newer head will
    /// arrive.
    pub async fn changed(&mut self) -> Result<(), tokio::sync::watch::error::RecvError> {
        self.inner.changed().await
    }

    /// Time since the last published header, or `None` if none was ever seen.
    #[must_use]
    pub fn last_header_age(&self) -> Option<Duration> {
        self.inner.last_seen_age()
    }

    /// Whether no header has arrived for at least `threshold`.
    #[must_use]
    pub fn stale(&self, threshold: Duration) -> bool {
        self.inner.stale(threshold)
    }
}

fn head_number(event: Option<&HubEvent>) -> Option<u64> {
    match event {
        Some(HubEvent::NewHead { number, .. }) => Some(*number),
        Some(HubEvent::PoolEvent { .. } | HubEvent::PendingTx(_)) | None => None,
    }
}
