//! `BlockClockPipe` — the coordinator-owned block-clock channel (ADR-027
//! completion; architecture review 2026-08-20).
//!
//! The pump's hand-offs are owned by ONE dispatch owner (ADR-027) but the
//! block-clock pipe used to live on the *engine* (`DeliveryPolicy.block_tx`),
//! contradicting that design: `newHeads` ticks are chain facts, not engine
//! business — the engine merely relayed them, taking its `Mutex` per header.
//! Now the [`SolveCoordinator`](super::solve_coordinator::SolveCoordinator)
//! owns the pipe directly: one non-blocking send per accepted header, engines
//! out of the block path entirely, and pump death closes it with the engines'
//! result lifecycles.

use tokio::sync::mpsc;

use super::BlockMetadata;

/// Forwarded newHeads tick — the authoritative block clock for Python.
///
/// Distinct from a result batch (which carries solve results + the solve
/// block as metadata): the consumer derives its block clock from
/// `BlockNotification`s pushed by the pump on every `WsEvent::BlockHeader`,
/// NOT from the batch's solve block. The solve block lags by the send
/// debounce + only advances when a batch is actually sent, so using it as the
/// clock makes the bot's `[block: N]` freeze behind the pump's `current_block`
/// (epic 6W35AI).
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

impl BlockNotification {
    /// Build a notification from a block number + its `BlockMetadata`.
    #[must_use]
    pub fn from_metadata(number: u64, metadata: &BlockMetadata) -> Self {
        Self {
            number,
            timestamp: metadata.timestamp,
            base_fee_per_gas: metadata.base_fee_per_gas,
            gas_used: metadata.gas_used,
            gas_limit: metadata.gas_limit,
        }
    }
}

/// The coordinator-owned block-clock pipe: open ([`Self::set_channel`]),
/// deliver ([`Self::notify`]), close ([`Self::close`]). End-of-stream
/// contract as for the delivery lifecycle: after [`Self::close`] the receiver
/// observes a natural stream end exactly once; sends are quiet no-ops when no
/// channel is attached (standalone consumers) or after close.
#[derive(Default)]
pub struct BlockClockPipe {
    tx: Option<mpsc::UnboundedSender<BlockNotification>>,
}

impl BlockClockPipe {
    /// Attach the sender (the wiring layer hands the Python-facing receiver
    /// elsewhere — the pipe never knows about receivers).
    pub fn set_channel(&mut self, tx: mpsc::UnboundedSender<BlockNotification>) {
        self.tx = Some(tx);
    }

    /// Deliver one tick. Quiet no-op without a channel or after close.
    pub fn notify(&self, block: u64, metadata: &BlockMetadata) {
        if let Some(ref tx) = self.tx {
            let _ = tx.send(BlockNotification::from_metadata(block, metadata));
        }
    }

    /// Close the pipe — pump death ends the Python block stream.
    pub fn close(&mut self) {
        self.tx = None;
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn notify_delivers_one_notification_with_metadata() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut pipe = BlockClockPipe::default();
        pipe.set_channel(tx);
        let metadata = BlockMetadata {
            timestamp: 1_700_000_000,
            base_fee_per_gas: Some(7_000_000_000),
            gas_used: 15_000_000,
            gas_limit: 30_000_000,
        };
        pipe.notify(25_390_117, &metadata);
        let notif = rx.try_recv().expect("tick delivered");
        assert_eq!(notif.number, 25_390_117);
        assert_eq!(notif.timestamp, metadata.timestamp);
        assert_eq!(notif.base_fee_per_gas, metadata.base_fee_per_gas);
        assert!(rx.try_recv().is_err(), "exactly one notification per call");
    }

    #[test]
    fn notify_without_channel_and_after_close_are_quiet_no_ops() {
        let mut pipe = BlockClockPipe::default();
        let metadata = BlockMetadata::default();
        pipe.notify(1, &metadata); // no channel
        let (tx, mut rx) = mpsc::unbounded_channel();
        pipe.set_channel(tx);
        pipe.close();
        pipe.notify(2, &metadata); // closed
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
