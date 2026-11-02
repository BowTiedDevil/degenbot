//! `BlockClockPipe` — the shared block-clock channel (ADR-027 completion;
//! architecture review 2026-08-20).
//!
//! `newHeads` ticks are chain facts, not engine business — the runtime's
//! driver (the WS pump’s stage driver) feeds ONE tick per accepted header
//! through this pipe, and any downstream driver (the pure-Rust runner, the
//! `PyO3` cockpit — which subscribes at the `Published`/block-clock edges like
//! any other sink) drains the receiver end. The pipe is neutral Rust: it
//! knows nothing about the engine, the stage machine, or Python.
//!
//! Migrated from `degenbot-bot::bot_core`: the
//! delivery-to-Python block-clock channel type is NOT runtime knowledge —
//! the shared kernel owns the type, the engine merely relays.
//!
//! The pipe's lifecycle is [`DeliveryChannel`]'s: a sender can be attached
//! once, delivered against, and closed. See [`crate::channel_lifecycle`] for
//! why the phase carries the sender rather than the sender living in an
//! `Option` — an `Option` has two `None`s ("never attached" and "closed") with
//! different consequences, and collapsing them is what let a re-attach
//! silently re-open a pipe whose receiver had already seen end-of-stream.

use crate::channel_lifecycle::{ChannelDecline, ChannelPhase, DeliveryChannel, SendOutcome};
use tokio::sync::mpsc;

/// Forwarded `newHeads` tick — the authoritative block clock.
///
/// Distinct from a result batch (which carries solve results + the solve
/// block as metadata): the consumer derives its block clock from
/// `BlockNotification`s pushed by the driver on every new header, NOT from
/// the batch's solve block. The solve block lags by the send debounce + only
/// advances when a batch is actually sent, so using it as the clock makes
/// the bot's `[block: N]` freeze behind the driver's `current_block`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockNotification {
    /// The block number (the clock field).
    pub number: u64,
    /// Block timestamp.
    pub timestamp: u64,
    /// Base fee per gas (None for pre-EIP-1559 blocks).
    pub base_fee_per_gas: Option<u64>,
    /// Gas used in this block.
    pub gas_used: u64,
    /// Gas limit of this block.
    pub gas_limit: u64,
}

/// The shared block-clock pipe: attach ([`Self::attach`]), deliver
/// ([`Self::notify`]), close ([`Self::close`]).
///
/// End-of-stream contract (owned by [`crate::channel_lifecycle`]): after
/// [`Self::close`] the receiver observes a natural stream end exactly once;
/// sends after close report [`SendOutcome::Closed`] and a standalone consumer
/// (never attached) reports [`SendOutcome::NotAttached`] — two facts that used
/// to be one `None`.
#[derive(Default)]
pub struct BlockClockPipe {
    chan: DeliveryChannel<BlockNotification>,
}

impl BlockClockPipe {
    /// Attach the sender (the wiring layer hands the receiver elsewhere — the
    /// pipe never knows about receivers).
    ///
    /// # Errors
    ///
    /// [`ChannelDecline::AlreadyAttached`] when one is already attached, and
    /// [`ChannelDecline::AttachedAfterClose`] after [`Self::close`] — a closed
    /// pipe's receiver has already observed end-of-stream, so re-opening would
    /// deliver into a stream the consumer considers finished.
    pub fn attach(
        &mut self,
        tx: mpsc::UnboundedSender<BlockNotification>,
    ) -> Result<(), ChannelDecline> {
        self.chan.attach(tx)
    }

    /// Deliver one tick, reporting whether it went out.
    ///
    /// Takes `&mut self` so a vanished receiver can latch the pipe closed.
    pub fn notify(&mut self, notification: BlockNotification) -> SendOutcome {
        self.chan.send(notification)
    }

    /// Close the pipe — pump death ends the downstream block stream.
    pub fn close(&mut self) -> ChannelPhase {
        self.chan.close()
    }

    /// Where the pipe is.
    #[must_use]
    pub const fn phase(&self) -> ChannelPhase {
        self.chan.phase()
    }
}

impl std::fmt::Debug for BlockClockPipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockClockPipe")
            .field("phase", &self.phase())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;

    fn notif(number: u64) -> BlockNotification {
        BlockNotification {
            number,
            timestamp: 0,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
        }
    }

    #[test]
    fn notify_delivers_one_notification_per_call() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut pipe = BlockClockPipe::default();
        pipe.attach(tx).expect("a fresh pipe attaches");
        assert_eq!(pipe.notify(notif(25_390_117)), SendOutcome::Delivered);
        assert_eq!(rx.try_recv().expect("tick delivered"), notif(25_390_117));
        assert!(rx.try_recv().is_err(), "exactly one notification per call");
    }

    #[test]
    fn a_never_attached_pipe_reports_not_attached() {
        let mut pipe = BlockClockPipe::default();
        assert_eq!(pipe.phase(), ChannelPhase::Detached);
        assert_eq!(pipe.notify(notif(1)), SendOutcome::NotAttached);
    }

    #[test]
    fn notify_after_close_reports_closed() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut pipe = BlockClockPipe::default();
        pipe.attach(tx).expect("a fresh pipe attaches");
        pipe.close();
        assert_eq!(pipe.notify(notif(1)), SendOutcome::Closed);
    }

    #[test]
    fn attach_after_close_is_declined() {
        // The defect this port closes: `set_channel` on a closed pipe silently
        // re-opened it, delivering into a stream whose receiver had already
        // observed end-of-stream.
        let (tx, _rx) = mpsc::unbounded_channel();
        let (tx2, _rx2) = mpsc::unbounded_channel();
        let mut pipe = BlockClockPipe::default();
        pipe.attach(tx).expect("a fresh pipe attaches");
        pipe.close();
        assert_eq!(pipe.attach(tx2), Err(ChannelDecline::AttachedAfterClose));
        assert_eq!(
            pipe.phase(),
            ChannelPhase::Closed,
            "a closed pipe stays closed"
        );
    }

    #[test]
    fn attach_twice_is_declined_rather_than_replacing_the_first_consumer() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (tx2, _rx2) = mpsc::unbounded_channel();
        let mut pipe = BlockClockPipe::default();
        pipe.attach(tx).expect("a fresh pipe attaches");
        assert_eq!(pipe.attach(tx2), Err(ChannelDecline::AlreadyAttached));
    }

    #[test]
    fn close_ends_the_stream_exactly_once() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut pipe = BlockClockPipe::default();
        pipe.attach(tx).expect("a fresh pipe attaches");
        pipe.close();
        assert!(
            rx.try_recv().is_err(),
            "the receiver observes end-of-stream exactly once"
        );
        pipe.close(); // idempotent
    }

    #[test]
    fn a_vanished_receiver_latches_the_pipe_closed() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut pipe = BlockClockPipe::default();
        pipe.attach(tx).expect("a fresh pipe attaches");
        drop(rx);
        assert_eq!(pipe.notify(notif(1)), SendOutcome::ReceiverGone);
        assert_eq!(pipe.phase(), ChannelPhase::Closed);
    }
}
