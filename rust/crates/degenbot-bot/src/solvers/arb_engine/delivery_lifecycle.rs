//! `DeliveryLifecycle` — the engine-side delivery channels' open/send/close
//! and the end-of-stream contract (architecture review 2026-08-20; incident
//! 2026-08-20 #2 class).
//!
//! The contract this module owns: **receivers observe a natural stream end
//! exactly once, on pump death or engine drop.** Before this module the
//! contract was a five-hop relay with default-no-op links; now it has one
//! home and one test suite.

use tokio::sync::mpsc;

use super::{BlockMetadata, BlockNotification, ResultBatch};

/// The engine-side delivery channels' open/send/close — ONE home for the
/// end-of-stream contract (architecture review 2026-08-20; incident
/// 2026-08-20 #2 class):
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
    /// Sender for the result-batch channel. `None` = standalone/no-pyo3
    /// consumer (sends are quiet no-ops).
    result_tx: Option<mpsc::UnboundedSender<ResultBatch>>,
    /// Sender for the block-notification channel (epic 6W35AI). Independent
    /// of `result_tx`.
    block_tx: Option<mpsc::UnboundedSender<BlockNotification>>,
}

impl DeliveryLifecycle {
    /// Attach the result-batch channel sender (engine construction/wiring).
    pub(crate) fn set_result_channel(&mut self, tx: mpsc::UnboundedSender<ResultBatch>) {
        self.result_tx = Some(tx);
    }

    /// Attach the block-notification channel sender. Independent of
    /// `result_tx`.
    pub(crate) fn set_block_channel(&mut self, tx: mpsc::UnboundedSender<BlockNotification>) {
        self.block_tx = Some(tx);
    }

    /// Close both channels — THE end-of-stream contract (see the type doc).
    /// Called on pump death (`Engine::on_pump_ended`) and engine drop.
    /// Idempotent; a no-op when no channel was attached (standalone
    /// consumers have nothing to end).
    pub(crate) fn close(&mut self) {
        self.block_tx = None;
        self.result_tx = None;
    }

    /// Send a result batch to Python. Returns whether a channel was open —
    /// after [`Self::close`] this is a quiet `false`, never a panic.
    pub(crate) fn send_batch(&self, batch: ResultBatch) -> bool {
        match &self.result_tx {
            // Unbounded channel: send fails only when the receiver is gone.
            Some(tx) => tx.send(batch).is_ok(),
            None => false,
        }
    }

    /// Forward a `newHeads` block tick onto the block-notification channel
    /// (epic 6W35AI). A quiet no-op when no block channel is attached or
    /// after [`Self::close`].
    pub(crate) fn notify_block(&self, block: u64, metadata: &BlockMetadata) {
        if let Some(ref tx) = self.block_tx {
            let _ = tx.send(BlockNotification::from_metadata(block, metadata));
        }
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn close_ends_both_streams_exactly_once() {
        let (rtx, mut rrx) = mpsc::unbounded_channel::<ResultBatch>();
        let (btx, mut brx) = mpsc::unbounded_channel::<BlockNotification>();
        let mut lc = DeliveryLifecycle::default();
        lc.set_result_channel(rtx);
        lc.set_block_channel(btx);

        lc.close();
        assert!(
            rrx.try_recv().is_err(),
            "result receiver must observe end-of-stream"
        );
        assert!(
            brx.try_recv().is_err(),
            "block receiver must observe end-of-stream"
        );
    }

    #[test]
    fn close_without_channels_is_a_no_op() {
        let mut lc = DeliveryLifecycle::default();
        lc.close(); // must not panic
    }

    #[test]
    fn send_after_close_is_a_quiet_no_op() {
        let (tx, _rx) = mpsc::unbounded_channel::<ResultBatch>();
        let mut lc = DeliveryLifecycle::default();
        lc.set_result_channel(tx);
        lc.close();

        let batch = ResultBatch {
            solve_block: 1,
            timestamp: 0,
            base_fee_per_gas: Some(0),
            gas_used: 0,
            gas_limit: 0,
            fresh: Vec::new(),
            updated: Vec::new(),
            expired: Vec::new(),
            removed: Vec::new(),
        };
        assert!(
            !lc.send_batch(batch),
            "send after close must report not-sent"
        );
    }

    #[test]
    fn notify_block_without_channel_or_after_close_is_a_no_op() {
        let mut lc = DeliveryLifecycle::default();
        let metadata = BlockMetadata::default();
        lc.notify_block(1, &metadata); // no channel attached
        let (tx, mut rx) = mpsc::unbounded_channel::<BlockNotification>();
        lc.set_block_channel(tx);
        lc.notify_block(2, &metadata);
        assert!(rx.try_recv().is_ok(), "open channel delivers the tick");
        lc.close();
        lc.notify_block(3, &metadata); // closed — quiet no-op
        assert!(rx.try_recv().is_err(), "closed channel delivers nothing");
    }

    #[test]
    fn send_batch_delivers_when_open() {
        let (tx, mut rx) = mpsc::unbounded_channel::<ResultBatch>();
        let mut lc = DeliveryLifecycle::default();
        lc.set_result_channel(tx);

        let batch = ResultBatch {
            solve_block: 42,
            timestamp: 0,
            base_fee_per_gas: Some(0),
            gas_used: 0,
            gas_limit: 0,
            fresh: Vec::new(),
            updated: Vec::new(),
            expired: Vec::new(),
            removed: Vec::new(),
        };
        assert!(lc.send_batch(batch));
        let got = rx.try_recv().expect("batch delivered");
        assert_eq!(got.solve_block, 42);
    }
}
