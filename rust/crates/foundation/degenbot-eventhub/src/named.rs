//! Named, typed source channels held by the hub.
//!
//! The hub's vocabulary channels are keyed by [`HubClass`] and carry
//! [`HubEvent`]. A *named* source channel carries a domain payload the hub
//! vocabulary deliberately does not name — the engine's `ResultBatch` and
//! `BlockNotification` are engine-owned downstream types, so forcing them
//! through `HubEvent` would invent vocabulary. A named channel is instead
//! keyed by a `&'static str` and its strictness is fixed at
//! [`OverflowPolicy::UnboundedFlagged`]: lossless by construction, flagged
//! because unbounded intake is a deliberate, audited choice.
//!
//! The channel is a `tokio::sync::mpsc` unbounded pair — the same primitive
//! the engine used before the hub owned it. The hub holds the consumer end
//! and hands it out once ([`crate::Hub::take_named_receiver`]); the source
//! holds the producer end. Dropping every producer closes the receiver
//! exactly as a bare `mpsc::unbounded_channel` did, so the end-of-stream
//! contract is unchanged.
//!
//! An unbounded queue keeps no depth signal of its own, so growth would be
//! invisible at runtime. [`NamedCounters`] carries a send/receive tally that
//! [`crate::Hub::named_pending`] reads as an approximate in-flight depth; the
//! tally rides the [`NamedSender`] / [`NamedReceiver`] wrappers and never
//! alters ordering or close semantics.

use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::mpsc;

/// Shared send/receive tallies for one named channel.
///
/// The pair is a runtime depth signal, not an exact queue mirror: each tally
/// records a completed [`NamedSender::send`] / [`NamedReceiver`] operation, so
/// a value can be in flight between the operation and its tally.
/// [`Self::pending`] saturates at zero rather than underflowing.
#[derive(Default)]
pub(crate) struct NamedCounters {
    sent: AtomicU64,
    received: AtomicU64,
}

impl NamedCounters {
    pub(crate) const fn new() -> Self {
        Self {
            sent: AtomicU64::new(0),
            received: AtomicU64::new(0),
        }
    }

    fn record_send(&self) {
        self.sent.fetch_add(1, Ordering::Relaxed);
    }

    fn record_receive(&self) {
        self.received.fetch_add(1, Ordering::Relaxed);
    }

    /// Approximate in-flight depth: sent minus received, saturating.
    fn pending(&self) -> u64 {
        self.sent
            .load(Ordering::Relaxed)
            .saturating_sub(self.received.load(Ordering::Relaxed))
    }
}

/// The type-erased consumer slot the hub holds for one named channel.
pub(crate) struct NamedChannel {
    pub(crate) receiver: Mutex<Option<Box<dyn Any + Send>>>,
    counters: Arc<NamedCounters>,
}

impl NamedChannel {
    /// Wrap a freshly-minted consumer end for hub storage.
    pub(crate) fn new<T: Send + 'static>(
        rx: mpsc::UnboundedReceiver<T>,
        counters: Arc<NamedCounters>,
    ) -> Self {
        let tracked = NamedReceiver::new(rx, Arc::clone(&counters));
        Self {
            receiver: Mutex::new(Some(Box::new(tracked))),
            counters,
        }
    }

    /// Approximate in-flight depth for this channel.
    pub(crate) fn pending(&self) -> u64 {
        self.counters.pending()
    }
}

/// The source half of a named hub channel.
///
/// Handed back by [`crate::Hub::add_named_unbounded_source`]. The producer
/// passes [`Self::into_inner`] to the code that sends, transferring the sole
/// sender so the channel closes when that holder drops it.
pub struct NamedSender<T> {
    name: &'static str,
    tx: mpsc::UnboundedSender<T>,
    counters: Arc<NamedCounters>,
}

impl<T> NamedSender<T> {
    pub(crate) const fn new(
        name: &'static str,
        tx: mpsc::UnboundedSender<T>,
        counters: Arc<NamedCounters>,
    ) -> Self {
        Self { name, tx, counters }
    }

    /// The registration name (also the audit label).
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Send `value`, tallying the send on success.
    ///
    /// # Errors
    ///
    /// [`mpsc::error::SendError`] when the consumer end has been dropped,
    /// mirroring [`mpsc::UnboundedSender::send`].
    pub fn send(&self, value: T) -> Result<(), mpsc::error::SendError<T>> {
        let sent = self.tx.send(value);
        if sent.is_ok() {
            self.counters.record_send();
        }
        sent
    }

    /// Approximate in-flight depth on this channel; see
    /// [`crate::Hub::named_pending`] for the in-flight tolerance note.
    #[must_use]
    pub fn pending(&self) -> u64 {
        self.counters.pending()
    }

    /// Take the raw producer end. The hub retains no sender, so this is the
    /// sole sender: dropping it closes the consumer end.
    ///
    /// Raw sends bypass the depth tally; [`Self::send`] is the tallied path.
    #[must_use]
    pub fn into_inner(self) -> mpsc::UnboundedSender<T> {
        self.tx
    }
}

/// The consumer half of a named hub channel, tallying receives.
///
/// Obtained from [`crate::Hub::take_named_counting_receiver`]; the
/// compatibility handoff [`crate::Hub::take_named_receiver`] still yields the
/// raw [`mpsc::UnboundedReceiver`]. `recv` / `try_recv` delegate to that raw
/// receiver, so ordering and close-on-sender-drop are unchanged.
pub struct NamedReceiver<T> {
    rx: mpsc::UnboundedReceiver<T>,
    counters: Arc<NamedCounters>,
}

impl<T> NamedReceiver<T> {
    pub(crate) fn new(rx: mpsc::UnboundedReceiver<T>, counters: Arc<NamedCounters>) -> Self {
        Self { rx, counters }
    }

    /// Await the next value, tallying it when one arrives.
    pub async fn recv(&mut self) -> Option<T> {
        let value = self.rx.recv().await;
        if value.is_some() {
            self.counters.record_receive();
        }
        value
    }

    /// Try to take the next value without waiting, tallying a successful take.
    ///
    /// # Errors
    ///
    /// The raw receiver's [`mpsc::error::TryRecvError`] (`Empty` or
    /// `Disconnected`).
    pub fn try_recv(&mut self) -> Result<T, mpsc::error::TryRecvError> {
        let value = self.rx.try_recv();
        if value.is_ok() {
            self.counters.record_receive();
        }
        value
    }

    /// Approximate in-flight depth on this channel; see
    /// [`crate::Hub::named_pending`] for the in-flight tolerance note.
    #[must_use]
    pub fn pending(&self) -> u64 {
        self.counters.pending()
    }

    /// Take the raw consumer end, giving up the tally (raw receives are not
    /// counted).
    #[must_use]
    pub fn into_inner(self) -> mpsc::UnboundedReceiver<T> {
        self.rx
    }
}
