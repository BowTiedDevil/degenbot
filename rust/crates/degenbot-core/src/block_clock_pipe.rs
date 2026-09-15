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
//! Migrated from `degenbot-bot::bot_core` (epic MROOY7, 5WTYYQ): the
//! delivery-to-Python block-clock channel type is NOT runtime knowledge —
//! the shared kernel owns the type, the engine merely relays.

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

/// The shared block-clock pipe: open ([`Self::set_channel`]), deliver
/// ([`Self::notify`]), close ([`Self::close`]). End-of-stream contract
/// as for the delivery lifecycle: after [`Self::close`] the receiver
/// observes a natural stream end exactly once; sends are quiet no-ops when
/// no channel is attached (standalone consumers) or after close.
#[derive(Default)]
pub struct BlockClockPipe {
    tx: Option<mpsc::UnboundedSender<BlockNotification>>,
}

impl BlockClockPipe {
    /// Attach the sender (the wiring layer hands the receiver elsewhere —
    /// the pipe never knows about receivers).
    pub fn set_channel(&mut self, tx: mpsc::UnboundedSender<BlockNotification>) {
        self.tx = Some(tx);
    }

    /// Deliver one tick. Quiet no-op without a channel or after close.
    pub fn notify(&self, notification: BlockNotification) {
        if let Some(ref tx) = self.tx {
            let _ = tx.send(notification);
        }
    }

    /// Close the pipe — pump death ends the downstream block stream.
    pub fn close(&mut self) {
        self.tx = None;
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn notify_delivers_one_notification_per_call() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut pipe = BlockClockPipe::default();
        pipe.set_channel(tx);
        let notif = BlockNotification {
            number: 25_390_117,
            timestamp: 1_700_000_000,
            base_fee_per_gas: Some(7_000_000_000),
            gas_used: 15_000_000,
            gas_limit: 30_000_000,
        };
        pipe.notify(notif);
        let got = rx.try_recv().expect("tick delivered");
        assert_eq!(got, notif);
        assert!(rx.try_recv().is_err(), "exactly one notification per call");
    }

    #[test]
    fn notify_without_channel_and_after_close_are_quiet_no_ops() {
        let mut pipe = BlockClockPipe::default();
        let notif = BlockNotification {
            number: 1,
            timestamp: 0,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
        };
        pipe.notify(notif); // no channel
        let (tx, mut rx) = mpsc::unbounded_channel();
        pipe.set_channel(tx);
        pipe.close();
        pipe.notify(notif); // closed
        assert!(rx.try_recv().is_err(), "closed pipe delivers nothing");
    }

    #[test]
    fn close_ends_the_stream_exactly_once() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut pipe = BlockClockPipe::default();
        pipe.set_channel(tx);
        pipe.close();
        assert!(rx.try_recv().is_err(), "receiver observes end-of-stream");
    }
}
