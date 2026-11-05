//! `DeliveryLifecycle` — the engine-side delivery channel's open/send/close
//! and the end-of-stream contract (architecture review 2026-08-20; incident
//! 2026-08-20 #2 class).
//!
//! The contract this module owns: **receivers observe a natural stream end
//! exactly once, on pump death or engine drop.** Before this module the
//! contract was a five-hop relay with default-no-op links; now it has one
//! home and one test suite.
//!
//! # Why the transport is the session's ORDERED source (ADR-093 D5)
//!
//! A result batch is one arm of the session's ordered event stream
//! ([`SessionEvent::Batch`](super::session_events::SessionEvent)); the header
//! tick is the other. Both are written into ONE source
//! ([`SessionEventSource`]) by the engine's two production sites, so
//! `Block(N)` precedes every `Batch(N)` by the producer's call order rather
//! than by a consumer's poll order. This module keeps the *end-of-stream*
//! contract for that one stream: it is the sole closer, and closing it once is
//! what makes the natural end a single observation.
//!
//! The channel's lifecycle is [`SessionEventSource`]'s — see
//! [`degenbot_core::channel_lifecycle`] for why the phase carries the sender
//! instead of the sender living in an `Option`. [`SendOutcome`] replaces the
//! former `bool`, which collapsed "never attached", "closed" and "the receiver
//! is gone" into a single `false`.

use std::sync::Arc;

use super::session_events::{SessionEvent, SessionEventSource};
use super::ResultBatch;
#[cfg(test)]
use degenbot_core::channel_lifecycle::ChannelDecline;
use degenbot_core::channel_lifecycle::{ChannelPhase, SendOutcome};
#[cfg(test)]
use tokio::sync::mpsc;

/// The engine-side delivery channel's open/send/close — ONE home for the
/// end-of-stream contract (architecture review 2026-08-20; incident 2026-08-20
/// #2 class):
///
/// > **Receivers observe a natural stream end exactly once, on pump death or
/// > engine drop.**
///
/// Before this module the contract lived as a five-hop relay across two
/// languages with default-no-op links; a missed link was invisible at the
/// source and fatal at the Python consumer (the engine owned the senders and
/// outlived the pump task, so the streams neither delivered nor ended).
/// Now the contract has one module and one test suite; [`DeliveryPolicy`]
/// (super) owns the *policy* half — what is worth sending — and holds one
/// instance of this type for the *transport* half.
///
/// The Python-facing semantics of [`Self::close`]: each receiver's
/// `__anext__` raises `StopAsyncIteration` exactly once (the runner cockpit's
/// `consume_result_batches` converts a natural end into a loud
/// `RuntimeError`; explicit stop/cancellation unaffected).
///
/// [`DeliveryPolicy`]: super::delivery_policy::DeliveryPolicy
#[derive(Default)]
pub(crate) struct DeliveryLifecycle {
    /// The session's one ordered source. Shared with the engine's header site
    /// (through [`super::engine_stages::EngineStages`]), so the two production
    /// arms cannot write to different channels. Detached = standalone/no-pyo3
    /// consumer (sends report [`SendOutcome::NotAttached`]).
    source: Arc<SessionEventSource>,
}

// The block-notification channel is NOT engine-side any more (ADR-027
// completion, 2026-08-20 review): the block-clock pipe is coordinator-owned
// (bot_core::block_clock_pipe::BlockClockPipe). The session's own `Block` arm
// is written at the header producer site, not here.

impl DeliveryLifecycle {
    /// The session's one ordered source, as a shared handle.
    ///
    /// The engine's delivery lifecycle OWNS the source (it is created with the
    /// engine and closed by this module), and the stage surface's header edge
    /// writes the SAME one — so both production arms land on one ordered stream.
    /// A second stage surface over the same engine shares it rather than
    /// replacing it, which is what keeps one engine one stream.
    pub(crate) fn source_handle(&self) -> Arc<SessionEventSource> {
        Arc::clone(&self.source)
    }

    /// Attach the session-event sender (the crate's test construction seam:
    /// production attaches through `EngineStages::attach_session_events`, whose
    /// source is the one adopted here).
    ///
    /// # Errors
    ///
    /// [`ChannelDecline::AlreadyAttached`] when one is already attached, and
    /// [`ChannelDecline::AttachedAfterClose`] after [`Self::close`].
    #[cfg(test)]
    pub(crate) fn attach(
        &mut self,
        tx: mpsc::UnboundedSender<SessionEvent>,
    ) -> Result<(), ChannelDecline> {
        self.source.attach(tx)
    }

    /// Close the stream — THE end-of-stream contract (see the type doc).
    /// Called on pump death (`EngineStages::on_pump_ended`) and engine drop.
    /// Idempotent; a no-op when no sender was attached (standalone consumers
    /// have nothing to end).
    pub(crate) fn close(&mut self) -> ChannelPhase {
        self.source.close()
    }

    /// Send a result batch as the stream's `Batch` arm, reporting which way it
    /// did (or did not) go out. After [`Self::close`] this is
    /// [`SendOutcome::Closed`], never a panic; a vanished receiver reports
    /// [`SendOutcome::ReceiverGone`] and latches the stream closed.
    pub(crate) fn send_batch(&self, batch: ResultBatch) -> SendOutcome {
        self.source.send(SessionEvent::Batch(batch))
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;
    use hashbrown::HashMap;

    fn batch() -> ResultBatch {
        ResultBatch {
            solve_block: 1,
            timestamp: 0,
            base_fee_per_gas: Some(0),
            gas_used: 0,
            gas_limit: 0,
            fresh: Vec::new(),
            updated: Vec::new(),
            expired: Vec::new(),
            removed: Vec::new(),
            payloads: HashMap::new(),
        }
    }

    fn chan() -> (
        mpsc::UnboundedSender<SessionEvent>,
        mpsc::UnboundedReceiver<SessionEvent>,
    ) {
        mpsc::unbounded_channel()
    }

    #[test]
    fn close_ends_the_stream_exactly_once() {
        let (tx, mut rx) = chan();
        let mut lc = DeliveryLifecycle::default();
        lc.attach(tx).expect("a fresh lifecycle attaches");
        lc.close();
        assert!(
            rx.try_recv().is_err(),
            "the stream's receiver must observe end-of-stream"
        );
        assert_eq!(lc.close(), ChannelPhase::Closed, "closed is absorbing");
    }

    #[test]
    fn close_without_a_sender_is_a_no_op() {
        let mut lc = DeliveryLifecycle::default();
        assert_eq!(lc.close(), ChannelPhase::Closed);
    }

    #[test]
    fn send_after_close_reports_closed_rather_than_a_bare_false() {
        let (tx, _rx) = chan();
        let mut lc = DeliveryLifecycle::default();
        lc.attach(tx).expect("a fresh lifecycle attaches");
        lc.close();
        assert_eq!(lc.send_batch(batch()), SendOutcome::Closed);
    }

    #[test]
    fn send_batch_delivers_the_batch_arm_when_open() {
        let (tx, mut rx) = chan();
        let mut lc = DeliveryLifecycle::default();
        lc.attach(tx).expect("a fresh lifecycle attaches");
        assert_eq!(lc.send_batch(batch()), SendOutcome::Delivered);
        let delivered = rx.try_recv().expect("one event delivered");
        assert!(
            matches!(&delivered, SessionEvent::Batch(batch) if batch.solve_block == 1),
            "the delivery edge writes the Batch arm: {delivered:?}"
        );
    }

    #[test]
    fn send_without_a_sender_reports_not_attached() {
        let lc = DeliveryLifecycle::default();
        assert_eq!(lc.send_batch(batch()), SendOutcome::NotAttached);
    }

    #[test]
    fn a_vanished_receiver_reports_receiver_gone_and_latches_closed() {
        let (tx, rx) = chan();
        let mut lc = DeliveryLifecycle::default();
        lc.attach(tx).expect("a fresh lifecycle attaches");
        drop(rx);
        assert_eq!(lc.send_batch(batch()), SendOutcome::ReceiverGone);
        assert_eq!(
            lc.send_batch(batch()),
            SendOutcome::Closed,
            "a vanished receiver latches the stream closed"
        );
    }

    #[test]
    fn attach_after_close_is_declined() {
        let (tx, _rx) = chan();
        let (tx2, _rx2) = chan();
        let mut lc = DeliveryLifecycle::default();
        lc.attach(tx).expect("a fresh lifecycle attaches");
        lc.close();
        assert_eq!(lc.attach(tx2), Err(ChannelDecline::AttachedAfterClose));
    }

    #[test]
    fn the_source_handle_is_the_one_this_lifecycle_closes() {
        // The engine's header site writes the SAME source; closing here must
        // be the one closure the consumer observes, whichever site closed it.
        let source = DeliveryLifecycle::default().source_handle();
        let (tx, mut rx) = chan();
        source.attach(tx).expect("a fresh source attaches");
        let mut lc = DeliveryLifecycle {
            source: Arc::clone(&source),
        };
        lc.close();
        assert!(rx.try_recv().is_err(), "the shared source closed");
        assert_eq!(source.phase(), ChannelPhase::Closed);
    }
}
