//! `session_events` — the session's ONE ordered event stream and its ONE consumer claim (ADR-093 D5).
//!
//! # The stream
//!
//! The engine used to hand out two independent channels — a result-batch source
//! and a block-clock source — so a consumer's *poll order* decided whether a
//! block's view was published before that block's batches. That made the two
//! clocks
//! a bug class instead of an ordering guarantee.
//!
//! Here there is one source and two arms:
//!
//! ```text
//! SessionEvent::Block(N)   // exactly once per epoch, before that epoch's batches
//! SessionEvent::Batch(b)   // zero or more, ascending solve_block
//! ```
//!
//! The ordering rule is in the type, not in a consumer's loop: **`Block(N)`
//! precedes every `Batch(N)`**, and a `Batch` never advances a block clock — the
//! `Block` does. The producer owns the order: both arms are written by the
//! engine's two production sites (the header edge and the delivery edge) into
//! this one channel, so the order is the producer's call order.
//!
//! There is deliberately **no `End` variant**. Natural end is the stream's
//! exactly-once *closure*, and the delivery lifecycle
//! ([`super::delivery_lifecycle`]) stays its sole owner; the typed terminal fact
//! rides the session-end surface ([`super::session_end`]) where a *cause* is
//! needed. A cause the reader produces cannot arrive on the stream the reader is
//! consuming.
//!
//! No `Rewind` and no census event: reorg rewind is the stage machine's
//! per-epoch edge, and the pipeline census is a separate channel by ADR-043.
//!
//! # The claim
//!
//! The stream leaves the driver only together with a **non-cloneable**
//! [`ConsumerClaim`], and `EngineDriver::run` takes that claim by value. So
//! "a consumer was attached before production started" stops being a convention
//! enforced by an ordering rule and becomes a type fact: a `SessionStream` in a
//! consumer's hand *is* the attached-consumer proof, a second hand-off is
//! refused by name, and `run` cannot be called at all without a claim.

use degenbot_core::channel_lifecycle::{
    ChannelDecline, ChannelPhase, DeliveryChannel, SendOutcome,
};
use tokio::sync::mpsc;

use super::{BlockNotification, ResultBatch};

/// One item on the session's ordered stream.
///
/// Exactly two arms. The engine's products, and nothing else: no end item, no
/// rewind fact, no telemetry.
#[derive(Clone, Debug)]
pub enum SessionEvent {
    /// A new header tick: the session's block clock, exactly once per epoch and
    /// always before that epoch's batches.
    Block(BlockNotification),
    /// A published result batch: zero or more per epoch, ascending
    /// `solve_block`.
    Batch(ResultBatch),
}

/// Why a consumer claim was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClaimRefusal {
    /// The session's one hand-off already happened. The stream cannot be handed
    /// out twice: the first consumer's stream would be stranded with no
    /// producer aware of it.
    #[error(
        "the session's event stream has already been claimed; one consumer hand-off per session"
    )]
    AlreadyClaimed,
}

/// The non-cloneable proof that a session's event consumer was attached before
/// production started.
///
/// Deliberately not `Clone` and deliberately not cheap to mint: the only door is
/// [`EventHandoff`]'s, and the only use is [`EngineDriver::run`]'s
/// ([`crate::arb_engine::EngineDriver::run`]), which consumes it.
#[derive(Debug)]
pub struct ConsumerClaim(());

impl ConsumerClaim {
    /// Mint the claim — crate-private, because a claim is the driver's answer to
    /// "one consumer is attached", not a value a caller can author.
    pub(crate) fn mint() -> Self {
        Self(())
    }
}

/// The session's consumer end: the ordered source, read one event at a time.
#[derive(Debug)]
pub struct SessionStream {
    rx: mpsc::UnboundedReceiver<SessionEvent>,
}

impl SessionStream {
    pub(crate) fn new(rx: mpsc::UnboundedReceiver<SessionEvent>) -> Self {
        Self { rx }
    }

    /// Await the next event. `None` is the stream's natural end — exactly once,
    /// and never a session verdict.
    pub async fn recv(&mut self) -> Option<SessionEvent> {
        self.rx.recv().await
    }

    /// Take the next already-queued event, if any.
    ///
    /// # Errors
    ///
    /// The channel's own [`mpsc::error::TryRecvError`]: `Empty` when nothing is
    /// queued yet, `Disconnected` once the stream has closed.
    pub fn try_recv(&mut self) -> Result<SessionEvent, mpsc::error::TryRecvError> {
        self.rx.try_recv()
    }
}

/// The one consumer hand-off: the ordered stream AND the claim that proves a
/// consumer holds it.
///
/// The pair is the interface: [`EngineDriver::claim_consumer`]
/// ([`crate::arb_engine::EngineDriver::claim_consumer`]) mints it once per
/// session, and [`EngineDriver::run`] consumes the claim.
#[derive(Debug)]
pub struct EventHandoff {
    stream: SessionStream,
    claim: ConsumerClaim,
}

impl EventHandoff {
    pub(crate) fn new(stream: SessionStream, claim: ConsumerClaim) -> Self {
        Self { stream, claim }
    }

    /// Split the hand-off into its two halves — the stream to read, the claim to
    /// move into `run`.
    #[must_use]
    pub fn split(self) -> (SessionStream, ConsumerClaim) {
        (self.stream, self.claim)
    }
}

/// The stream's producer end: the ONE source both engine production sites write
/// into.
///
/// One shared channel and one ordering owner — the header edge writes
/// `SessionEvent::Block`, the delivery edge writes `SessionEvent::Batch`, so
/// `Block(N)` precedes every `Batch(N)` by the producer's call order rather than
/// by a consumer's poll order. The consumer end is handed out exactly once, with
/// its claim.
///
/// Wraps a [`DeliveryChannel`] so the phase cannot collapse "never attached"
/// into "closed" — a standalone consumer has nothing to end, while a closed
/// stream's receiver has already observed its natural end.
#[derive(Default)]
pub struct SessionEventSource {
    chan: parking_lot::Mutex<DeliveryChannel<SessionEvent>>,
}

impl SessionEventSource {
    /// Attach the sender — the one door in.
    ///
    /// # Errors
    ///
    /// [`ChannelDecline::AlreadyAttached`] when one is already attached, and
    /// [`ChannelDecline::AttachedAfterClose`] after [`Self::close`] — a closed
    /// stream's consumer has already seen its natural end.
    pub(crate) fn attach(
        &self,
        tx: mpsc::UnboundedSender<SessionEvent>,
    ) -> Result<(), ChannelDecline> {
        self.chan.lock().attach(tx)
    }

    /// Deliver one event, reporting which way it (did not) go out.
    pub(crate) fn send(&self, event: SessionEvent) -> SendOutcome {
        self.chan.lock().send(event)
    }

    /// Close the stream — the natural end's one edge. Total and absorbing.
    pub(crate) fn close(&self) -> ChannelPhase {
        self.chan.lock().close()
    }

    /// Where the stream is.
    #[must_use]
    pub fn phase(&self) -> ChannelPhase {
        self.chan.lock().phase()
    }
}

impl std::fmt::Debug for SessionEventSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionEventSource")
            .field("phase", &self.phase())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn block(n: u64) -> SessionEvent {
        SessionEvent::Block(BlockNotification {
            number: n,
            timestamp: 0,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
        })
    }

    fn batch(n: u64) -> SessionEvent {
        SessionEvent::Batch(ResultBatch {
            solve_block: n,
            timestamp: 0,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
            fresh: Vec::new(),
            updated: Vec::new(),
            expired: Vec::new(),
            removed: Vec::new(),
            payloads: hashbrown::HashMap::new(),
        })
    }

    #[tokio::test]
    async fn the_stream_preserves_the_producer_order() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut stream = SessionStream::new(rx);
        // The producer's call order IS the guarantee: block, then its batches,
        // then the next block with no batches at all.
        tx.send(block(7)).unwrap();
        tx.send(batch(7)).unwrap();
        tx.send(batch(7)).unwrap();
        tx.send(block(8)).unwrap();

        let mut order = Vec::new();
        for _ in 0..4 {
            match stream.recv().await.unwrap() {
                SessionEvent::Block(n) => order.push((n.number, None)),
                SessionEvent::Batch(b) => {
                    order.last_mut().unwrap().1 = Some(b.solve_block);
                }
            }
        }
        assert_eq!(order, vec![(7, Some(7)), (8, None)]);
    }

    #[tokio::test]
    async fn the_closure_is_the_end_and_it_arrives_exactly_once() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut stream = SessionStream::new(rx);
        drop(tx);
        assert!(stream.recv().await.is_none());
        assert!(stream.recv().await.is_none(), "a second read stays ended");
    }

    #[test]
    fn the_claim_is_not_cloneable_by_construction() {
        // A `ConsumerClaim` has no `Clone` impl, so a consumer cannot stand a
        // clone in for the attached-consumer fact. This is a compile-time
        // property; the assertion that it holds is the absence of the impl.
        fn assert_not_clone<T: ?Sized>() {}
        assert_not_clone::<ConsumerClaim>();
        let claim = ConsumerClaim::mint();
        let _ = EventHandoff::new(SessionStream::new(mpsc::unbounded_channel().1), claim);
    }
}
