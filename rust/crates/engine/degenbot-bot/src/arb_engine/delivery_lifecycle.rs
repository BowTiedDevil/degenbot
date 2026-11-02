//! `DeliveryLifecycle` — the engine-side delivery channel's open/send/close
//! and the end-of-stream contract (architecture review 2026-08-20; incident
//! 2026-08-20 #2 class).
//!
//! The contract this module owns: **receivers observe a natural stream end
//! exactly once, on pump death or engine drop.** Before this module the
//! contract was a five-hop relay with default-no-op links; now it has one
//! home and one test suite.
//!
//! The channel's lifecycle is [`DeliveryChannel`]'s — see
//! [`degenbot_core::channel_lifecycle`] for why the phase carries the sender instead of
//! the sender living in an `Option`. [`SendOutcome`] replaces the former `bool`,
//! which collapsed "never attached", "closed" and "the receiver is gone" into a
//! single `false`.
use super::ResultBatch;
use degenbot_core::channel_lifecycle::{
    ChannelDecline, ChannelPhase, DeliveryChannel, SendOutcome,
};
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
#[derive(Default)]
pub(crate) struct DeliveryLifecycle {
    /// The result-batch channel. Detached = standalone/no-pyo3 consumer (sends
    /// report [`SendOutcome::NotAttached`]).
    chan: DeliveryChannel<ResultBatch>,
}

// The block-notification channel is NOT engine-side any more (ADR-027
// completion, 2026-08-20 review): the block-clock pipe is coordinator-owned
// (bot_core::block_clock_pipe::BlockClockPipe) — a header tick is a chain
// fact, not engine business.
impl DeliveryLifecycle {
    /// Attach the result-batch channel sender (engine construction/wiring).
    ///
    /// # Errors
    ///
    /// [`ChannelDecline::AlreadyAttached`] when one is already attached, and
    /// [`ChannelDecline::AttachedAfterClose`] after [`Self::close`].
    pub(crate) fn attach(
        &mut self,
        tx: mpsc::UnboundedSender<ResultBatch>,
    ) -> Result<(), ChannelDecline> {
        self.chan.attach(tx)
    }

    /// Close the channel — THE end-of-stream contract (see the type doc).
    /// Called on pump death (`Engine::on_pump_ended`) and engine drop.
    /// Idempotent; a no-op when no channel was attached (standalone consumers
    /// have nothing to end).
    pub(crate) fn close(&mut self) -> ChannelPhase {
        self.chan.close()
    }

    /// Send a result batch to Python, reporting which way it did (or did not)
    /// go out. After [`Self::close`] this is [`SendOutcome::Closed`], never a
    /// panic; a vanished receiver reports [`SendOutcome::ReceiverGone`] and
    /// latches the channel closed.
    pub(crate) fn send_batch(&mut self, batch: ResultBatch) -> SendOutcome {
        self.chan.send(batch)
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

    #[test]
    fn close_ends_the_result_stream_exactly_once() {
        let (rtx, mut rrx) = mpsc::unbounded_channel::<ResultBatch>();
        let mut lc = DeliveryLifecycle::default();
        lc.attach(rtx).expect("a fresh lifecycle attaches");
        lc.close();
        assert!(
            rrx.try_recv().is_err(),
            "result receiver must observe end-of-stream"
        );
    }

    #[test]
    fn close_without_channels_is_a_no_op() {
        let mut lc = DeliveryLifecycle::default();
        assert_eq!(lc.close(), ChannelPhase::Closed);
    }

    #[test]
    fn send_after_close_reports_closed_rather_than_a_bare_false() {
        let (tx, _rx) = mpsc::unbounded_channel::<ResultBatch>();
        let mut lc = DeliveryLifecycle::default();
        lc.attach(tx).expect("a fresh lifecycle attaches");
        lc.close();
        assert_eq!(lc.send_batch(batch()), SendOutcome::Closed);
    }

    #[test]
    fn send_batch_delivers_when_open() {
        let (tx, mut rx) = mpsc::unbounded_channel::<ResultBatch>();
        let mut lc = DeliveryLifecycle::default();
        lc.attach(tx).expect("a fresh lifecycle attaches");
        assert_eq!(lc.send_batch(batch()), SendOutcome::Delivered);
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn send_without_a_channel_reports_not_attached() {
        let mut lc = DeliveryLifecycle::default();
        assert_eq!(lc.send_batch(batch()), SendOutcome::NotAttached);
    }

    #[test]
    fn a_vanished_receiver_reports_receiver_gone_and_latches_closed() {
        let (tx, rx) = mpsc::unbounded_channel::<ResultBatch>();
        let mut lc = DeliveryLifecycle::default();
        lc.attach(tx).expect("a fresh lifecycle attaches");
        drop(rx);
        assert_eq!(lc.send_batch(batch()), SendOutcome::ReceiverGone);
        assert_eq!(
            lc.send_batch(batch()),
            SendOutcome::Closed,
            "a vanished receiver latches the channel closed"
        );
    }

    #[test]
    fn attach_after_close_is_declined() {
        let (tx, _rx) = mpsc::unbounded_channel::<ResultBatch>();
        let (tx2, _rx2) = mpsc::unbounded_channel::<ResultBatch>();
        let mut lc = DeliveryLifecycle::default();
        lc.attach(tx).expect("a fresh lifecycle attaches");
        lc.close();
        assert_eq!(lc.attach(tx2), Err(ChannelDecline::AttachedAfterClose));
    }
}
