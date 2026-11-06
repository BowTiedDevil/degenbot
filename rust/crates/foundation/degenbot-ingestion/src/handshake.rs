//! `HandshakeMachine` — the pure subscribe-handshake transition owner.
//!
//! The handshake runs against a merged `newHeads` + `logs` stream until it can
//! name the delivery-hole-free boundary block `W`. Every input arrives as a
//! closed [`HandshakeEvent`] and every transition answers with a closed
//! [`HandshakeEffect`]; the async adapter around the machine (`WsIngestor`)
//! owns only stream polling, the `eth_blockNumber` fallback, the injected
//! clock, and re-injection of [`HandshakeCompletion::pending`] — it never
//! re-authors boundary legality.

use std::mem;

use futures_util::{stream, StreamExt};

use crate::events::{IngestEvent, PoolEvent};
use crate::ingestor::{SubscribeBoundary, LOG_CATCHUP_SETTLE_SECS};

/// The log-catchup settle window in injected-clock milliseconds.
pub const LOG_CATCHUP_SETTLE_MS: u64 = LOG_CATCHUP_SETTLE_SECS * 1_000;

/// The handshake's nonterminal phase and the header facts that phase owns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandshakeState {
    /// No header observed yet.
    AwaitingFirstHeader,
    /// `prev_header` is the confirmation anchor: only `prev_header + 1`
    /// confirms it.
    AwaitingConfirmation {
        /// The confirmation anchor.
        prev_header: u64,
        /// Timestamp of the last anchor-moving or confirming header.
        prev_timestamp: u64,
    },
    /// The head is header-confirmed; the log-settle window is open until
    /// `settle_deadline`.
    Settling {
        /// Highest header-confirmed block.
        confirmed_head: u64,
        /// Injected-clock ms at which the fallback rules take over.
        settle_deadline: u64,
        /// The confirmation anchor (a confirmation leaves it in place).
        prev_header: u64,
        /// Timestamp of the last anchor-moving or confirming header.
        prev_timestamp: u64,
    },
}

/// The closed input vocabulary of the handshake.
#[derive(Clone, Debug)]
pub enum HandshakeEvent {
    /// A `newHeads` header from the merged stream.
    Header {
        /// Block number.
        number: u64,
        /// Block timestamp (seconds).
        timestamp: u64,
    },
    /// A logs-subscription delivery. Consumed into pending; only its block
    /// number is read. Boxed: the raw log dwarfs every other event.
    Pool(Box<PoolEvent>),
    /// The adapter's poll budget elapsed with no stream item.
    StreamTimeout,
    /// The merged stream ended.
    StreamEnded,
    /// The caller's shutdown flag, observed at the adapter loop head.
    Shutdown,
    /// Explicit deadline evaluation at the `now_ms` passed to
    /// [`HandshakeMachine::handle`].
    Tick,
    /// `eth_blockNumber` succeeded on the degraded path.
    LatestBlock(u64),
    /// `eth_blockNumber` failed on the degraded path.
    LatestBlockFailed,
}

impl From<IngestEvent> for HandshakeEvent {
    fn from(event: IngestEvent) -> Self {
        match event {
            IngestEvent::BlockHeader {
                number, timestamp, ..
            } => Self::Header { number, timestamp },
            IngestEvent::Pool(pe) => Self::Pool(Box::new(pe)),
        }
    }
}

/// The closed output vocabulary: what the adapter must do next.
#[derive(Debug)]
pub enum HandshakeEffect {
    /// Keep driving the stream.
    Continue,
    /// Ask the provider for `eth_blockNumber` and feed its outcome back as
    /// [`HandshakeEvent::LatestBlock`] / [`HandshakeEvent::LatestBlockFailed`].
    FetchLatestBlock,
    /// Terminal: the handshake picked its boundary.
    Complete(HandshakeCompletion),
}

/// The terminal handshake outcome: the boundary parts plus every pool log the
/// handshake consumed, in arrival order.
#[derive(Debug)]
pub struct HandshakeCompletion {
    /// Boundary block `W` — the first complete block or the fallback.
    pub first_block: u64,
    /// Timestamp from the last anchor-moving or confirming header.
    pub first_timestamp: u64,
    /// Pool logs to re-inject at the head of the returned stream.
    pub pending: Vec<IngestEvent>,
}

impl HandshakeCompletion {
    /// Re-inject `pending` ahead of the live tail: the delivery-hole-free
    /// handoff into [`SubscribeBoundary`].
    #[must_use]
    pub fn into_boundary(self, tail: stream::BoxStream<'static, IngestEvent>) -> SubscribeBoundary {
        let stream = if self.pending.is_empty() {
            tail
        } else {
            stream::iter(self.pending).chain(tail).boxed()
        };
        SubscribeBoundary {
            first_block: self.first_block,
            first_timestamp: self.first_timestamp,
            stream,
        }
    }
}

/// The pure subscribe-handshake state machine: one transition owner for the
/// phase, the header facts, the log-liveness fact, the settle deadline, and
/// the pending logs.
#[derive(Debug)]
pub struct HandshakeMachine {
    /// `None` once complete — completion is the emitted [`HandshakeEffect`],
    /// never a reusable live state.
    state: Option<HandshakeState>,
    /// The first delivered pool log's block: the preferred boundary.
    first_log_block: Option<u64>,
    /// Every pool log consumed during the handshake, in arrival order.
    pending: Vec<IngestEvent>,
}

impl Default for HandshakeMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl HandshakeMachine {
    /// A fresh handshake at [`HandshakeState::AwaitingFirstHeader`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Some(HandshakeState::AwaitingFirstHeader),
            first_log_block: None,
            pending: Vec::new(),
        }
    }

    /// Apply one event at injected-clock `now_ms` and answer with the next
    /// effect. Events after [`HandshakeEffect::Complete`] are ignored.
    pub fn handle(&mut self, now_ms: u64, event: HandshakeEvent) -> HandshakeEffect {
        if self.state.is_none() {
            return HandshakeEffect::Continue;
        }
        match event {
            HandshakeEvent::Header { number, timestamp } => {
                self.apply_header(now_ms, number, timestamp);
            }
            HandshakeEvent::Pool(pe) => self.apply_pool(pe),
            HandshakeEvent::StreamTimeout => return HandshakeEffect::FetchLatestBlock,
            HandshakeEvent::LatestBlock(block) => return self.complete(block, 0),
            HandshakeEvent::Tick | HandshakeEvent::LatestBlockFailed => {}
            HandshakeEvent::StreamEnded => {
                let (block, timestamp) = match self.state.as_ref() {
                    Some(
                        HandshakeState::AwaitingConfirmation {
                            prev_header,
                            prev_timestamp,
                        }
                        | HandshakeState::Settling {
                            prev_header,
                            prev_timestamp,
                            ..
                        },
                    ) => (*prev_header, *prev_timestamp),
                    _ => (0, 0),
                };
                return self.complete(block, timestamp);
            }
            HandshakeEvent::Shutdown => return self.complete(0, 0),
        }
        self.try_complete(now_ms)
    }

    fn apply_header(&mut self, now_ms: u64, number: u64, timestamp: u64) {
        let Some(state) = self.state.take() else {
            return;
        };
        self.state = Some(match state {
            HandshakeState::AwaitingFirstHeader => HandshakeState::AwaitingConfirmation {
                prev_header: number,
                prev_timestamp: timestamp,
            },
            HandshakeState::AwaitingConfirmation {
                prev_header,
                prev_timestamp,
            } => {
                if prev_header.checked_add(1) == Some(number) {
                    HandshakeState::Settling {
                        confirmed_head: prev_header,
                        settle_deadline: now_ms.saturating_add(LOG_CATCHUP_SETTLE_MS),
                        prev_header,
                        prev_timestamp: timestamp,
                    }
                } else if number > prev_header {
                    HandshakeState::AwaitingConfirmation {
                        prev_header: number,
                        prev_timestamp: timestamp,
                    }
                } else {
                    HandshakeState::AwaitingConfirmation {
                        prev_header,
                        prev_timestamp,
                    }
                }
            }
            HandshakeState::Settling {
                confirmed_head,
                settle_deadline,
                prev_header,
                prev_timestamp,
            } => {
                if prev_header.checked_add(1) == Some(number) {
                    HandshakeState::Settling {
                        confirmed_head: prev_header,
                        settle_deadline,
                        prev_header,
                        prev_timestamp: timestamp,
                    }
                } else if number > prev_header {
                    HandshakeState::Settling {
                        confirmed_head,
                        settle_deadline,
                        prev_header: number,
                        prev_timestamp: timestamp,
                    }
                } else {
                    HandshakeState::Settling {
                        confirmed_head,
                        settle_deadline,
                        prev_header,
                        prev_timestamp,
                    }
                }
            }
        });
    }

    fn apply_pool(&mut self, pe: Box<PoolEvent>) {
        if self.first_log_block.is_none() {
            if let Some(block) = pe.payload.block_number {
                self.first_log_block = Some(block);
            }
        }
        self.pending.push(IngestEvent::Pool(*pe));
    }

    fn try_complete(&mut self, now_ms: u64) -> HandshakeEffect {
        let Some(HandshakeState::Settling {
            confirmed_head,
            settle_deadline,
            prev_timestamp,
            ..
        }) = self.state.as_ref()
        else {
            return HandshakeEffect::Continue;
        };
        let (confirmed_head, settle_deadline, prev_timestamp) =
            (*confirmed_head, *settle_deadline, *prev_timestamp);
        let deadline_passed = now_ms >= settle_deadline;
        let boundary_ok = match self.first_log_block {
            Some(l) => l <= confirmed_head || deadline_passed,
            None => deadline_passed,
        };
        if !boundary_ok {
            return HandshakeEffect::Continue;
        }
        let boundary = self.first_log_block.unwrap_or(confirmed_head);
        self.complete(boundary, prev_timestamp)
    }

    fn complete(&mut self, first_block: u64, first_timestamp: u64) -> HandshakeEffect {
        self.state = None;
        HandshakeEffect::Complete(HandshakeCompletion {
            first_block,
            first_timestamp,
            pending: mem::take(&mut self.pending),
        })
    }

    /// The current phase (`None` once the handshake has completed).
    #[must_use]
    pub fn state(&self) -> Option<&HandshakeState> {
        self.state.as_ref()
    }

    /// The preferred boundary block, if a pool log has been delivered.
    #[must_use]
    pub fn first_log_block(&self) -> Option<u64> {
        self.first_log_block
    }

    /// The pool logs consumed so far, in arrival order.
    #[must_use]
    pub fn pending(&self) -> &[IngestEvent] {
        &self.pending
    }

    /// The armed settle deadline in injected-clock ms ([`None`] before the
    /// first header confirmation).
    #[must_use]
    pub fn settle_deadline(&self) -> Option<u64> {
        match self.state {
            Some(HandshakeState::Settling {
                settle_deadline, ..
            }) => Some(settle_deadline),
            _ => None,
        }
    }
}

#[cfg(test)]
mod machine_tests {
    //! The pure transition seam: scripted events and an injected clock, no
    //! wall clock and no stream.

    use super::*;

    use alloy::primitives::{Address, Bytes};
    use alloy::rpc::types::Log as RpcLog;

    fn header(number: u64) -> HandshakeEvent {
        HandshakeEvent::Header {
            number,
            timestamp: 0,
        }
    }

    fn header_ts(number: u64, timestamp: u64) -> HandshakeEvent {
        HandshakeEvent::Header { number, timestamp }
    }

    /// A pool delivery whose only handshake-visible fact is `block`.
    fn pool(block: Option<u64>, log_index: u64) -> HandshakeEvent {
        let inner = alloy::primitives::Log::new_unchecked(Address::ZERO, Vec::new(), Bytes::new());
        HandshakeEvent::Pool(Box::new(PoolEvent::from_log(RpcLog {
            inner,
            block_hash: None,
            block_number: block,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: Some(log_index),
            removed: false,
        })))
    }

    fn effect_label(effect: &HandshakeEffect) -> &'static str {
        match effect {
            HandshakeEffect::Continue => "continue",
            HandshakeEffect::FetchLatestBlock => "fetch",
            HandshakeEffect::Complete(_) => "complete",
        }
    }

    fn completion_parts(effect: &HandshakeEffect) -> Option<(u64, u64)> {
        match effect {
            HandshakeEffect::Complete(c) => Some((c.first_block, c.first_timestamp)),
            _ => None,
        }
    }

    #[test]
    fn first_header_alone_never_completes() {
        let mut m = HandshakeMachine::new();
        assert_eq!(effect_label(&m.handle(0, header(5))), "continue");
        assert_eq!(
            effect_label(&m.handle(1_000_000, HandshakeEvent::Tick)),
            "continue"
        );
        assert_eq!(
            m.state(),
            Some(&HandshakeState::AwaitingConfirmation {
                prev_header: 5,
                prev_timestamp: 0,
            })
        );
    }

    #[test]
    fn consecutive_headers_confirm_and_complete_on_log_liveness() {
        let mut m = HandshakeMachine::new();
        assert_eq!(effect_label(&m.handle(0, header(5))), "continue");
        assert_eq!(effect_label(&m.handle(0, pool(Some(5), 0))), "continue");
        let effect = m.handle(0, header_ts(6, 60));
        assert_eq!(effect_label(&effect), "complete");
        assert_eq!(completion_parts(&effect), Some((5, 60)));
    }

    #[test]
    fn forward_jump_reanchors_without_confirming() {
        let mut m = HandshakeMachine::new();
        assert_eq!(effect_label(&m.handle(0, header(5))), "continue");
        assert_eq!(effect_label(&m.handle(0, header(8))), "continue");
        assert_eq!(
            m.state(),
            Some(&HandshakeState::AwaitingConfirmation {
                prev_header: 8,
                prev_timestamp: 0,
            })
        );
        // only the new anchor's successor confirms — the pre-jump anchor stays unconfirmed
        assert_eq!(effect_label(&m.handle(0, header(9))), "continue");
        assert_eq!(
            m.state(),
            Some(&HandshakeState::Settling {
                confirmed_head: 8,
                settle_deadline: LOG_CATCHUP_SETTLE_MS,
                prev_header: 8,
                prev_timestamp: 0,
            })
        );
    }

    #[test]
    fn gap_reanchors_after_confirmation_without_advancing_the_head() {
        let mut m = HandshakeMachine::new();
        assert_eq!(effect_label(&m.handle(0, header_ts(5, 50))), "continue");
        assert_eq!(effect_label(&m.handle(0, header_ts(6, 60))), "continue");
        assert_eq!(effect_label(&m.handle(0, header_ts(9, 90))), "continue");
        assert_eq!(
            m.state(),
            Some(&HandshakeState::Settling {
                confirmed_head: 5,
                settle_deadline: LOG_CATCHUP_SETTLE_MS,
                prev_header: 9,
                prev_timestamp: 90,
            })
        );
    }

    #[test]
    fn duplicate_and_stale_headers_do_not_confirm() {
        // stale below the anchor
        let mut m = HandshakeMachine::new();
        m.handle(0, header_ts(5, 50));
        assert_eq!(effect_label(&m.handle(0, header_ts(4, 40))), "continue");
        assert_eq!(
            m.state(),
            Some(&HandshakeState::AwaitingConfirmation {
                prev_header: 5,
                prev_timestamp: 50,
            })
        );

        // duplicate of the anchor
        let mut m = HandshakeMachine::new();
        m.handle(0, header_ts(5, 50));
        assert_eq!(effect_label(&m.handle(0, header_ts(5, 50))), "continue");
        assert_eq!(
            m.state(),
            Some(&HandshakeState::AwaitingConfirmation {
                prev_header: 5,
                prev_timestamp: 50,
            })
        );

        // a duplicate confirming header cannot advance the head or re-arm the deadline
        let mut m = HandshakeMachine::new();
        m.handle(0, header_ts(5, 50));
        m.handle(100, header_ts(6, 60));
        assert_eq!(effect_label(&m.handle(200, header_ts(6, 60))), "continue");
        assert_eq!(
            m.state(),
            Some(&HandshakeState::Settling {
                confirmed_head: 5,
                settle_deadline: 100 + LOG_CATCHUP_SETTLE_MS,
                prev_header: 5,
                prev_timestamp: 60,
            })
        );
        // stale below the anchor while settling
        assert_eq!(effect_label(&m.handle(300, header_ts(3, 30))), "continue");
        assert_eq!(
            m.state(),
            Some(&HandshakeState::Settling {
                confirmed_head: 5,
                settle_deadline: 100 + LOG_CATCHUP_SETTLE_MS,
                prev_header: 5,
                prev_timestamp: 60,
            })
        );
    }

    #[test]
    fn first_log_before_confirmation_selects_its_block_once_confirmed() {
        let mut m = HandshakeMachine::new();
        assert_eq!(effect_label(&m.handle(0, pool(Some(5), 0))), "continue");
        assert_eq!(effect_label(&m.handle(0, header(5))), "continue");
        let effect = m.handle(0, header_ts(6, 60));
        assert_eq!(effect_label(&effect), "complete");
        assert_eq!(completion_parts(&effect), Some((5, 60)));
    }

    #[test]
    fn first_log_after_confirmation_selects_its_block_once_confirmed() {
        let mut m = HandshakeMachine::new();
        m.handle(0, header_ts(5, 50));
        m.handle(0, header_ts(6, 60));
        // the log lands above the confirmed head: not yet a legal boundary
        assert_eq!(effect_label(&m.handle(0, pool(Some(7), 0))), "continue");
        // the log's own block becomes confirmable only once a successor header arrives
        assert_eq!(effect_label(&m.handle(0, header_ts(7, 70))), "continue");
        let effect = m.handle(0, header_ts(8, 80));
        assert_eq!(effect_label(&effect), "complete");
        assert_eq!(completion_parts(&effect), Some((7, 80)));
    }

    #[test]
    fn first_log_below_the_confirmed_head_still_supplies_the_boundary() {
        let mut m = HandshakeMachine::new();
        m.handle(0, pool(Some(3), 0));
        m.handle(0, header(5));
        let effect = m.handle(0, header_ts(6, 60));
        assert_eq!(effect_label(&effect), "complete");
        assert_eq!(completion_parts(&effect), Some((3, 60)));
    }

    #[test]
    fn first_log_above_the_confirmed_head_selects_its_block_only_after_the_settle_deadline() {
        let mut m = HandshakeMachine::new();
        m.handle(0, header_ts(5, 50));
        assert_eq!(effect_label(&m.handle(0, pool(Some(9), 0))), "continue");
        assert_eq!(effect_label(&m.handle(0, header_ts(6, 60))), "continue");
        assert_eq!(
            effect_label(&m.handle(LOG_CATCHUP_SETTLE_MS - 1, HandshakeEvent::Tick)),
            "continue"
        );
        let effect = m.handle(LOG_CATCHUP_SETTLE_MS, HandshakeEvent::Tick);
        assert_eq!(effect_label(&effect), "complete");
        assert_eq!(completion_parts(&effect), Some((9, 60)));
    }

    #[test]
    fn blockless_first_log_does_not_supply_the_boundary() {
        let mut m = HandshakeMachine::new();
        assert_eq!(effect_label(&m.handle(0, pool(None, 3))), "continue");
        assert_eq!(m.first_log_block(), None);
        assert_eq!(m.pending().len(), 1);
        // no boundary from the log side: confirmation alone must wait for liveness/fallback
        m.handle(0, header(5));
        assert_eq!(effect_label(&m.handle(0, header(6))), "continue");
        assert_eq!(completion_parts(&m.handle(0, HandshakeEvent::Tick)), None);
    }

    #[test]
    fn deadline_expiry_selects_the_header_fallback_exactly_once() {
        let mut m = HandshakeMachine::new();
        m.handle(0, header_ts(5, 50));
        m.handle(0, header_ts(6, 60));
        assert_eq!(
            effect_label(&m.handle(LOG_CATCHUP_SETTLE_MS - 1, HandshakeEvent::Tick)),
            "continue"
        );
        let effect = m.handle(LOG_CATCHUP_SETTLE_MS, HandshakeEvent::Tick);
        assert_eq!(effect_label(&effect), "complete");
        assert_eq!(completion_parts(&effect), Some((5, 60)));
        // the fallback is selected once: a spent machine answers Continue forever
        assert_eq!(m.state(), None);
        assert_eq!(effect_label(&m.handle(20_000, header(7))), "continue");
        assert_eq!(
            effect_label(&m.handle(20_000, HandshakeEvent::Tick)),
            "continue"
        );
        assert_eq!(m.state(), None);
    }

    #[test]
    fn first_log_arriving_after_the_settle_deadline_selects_its_block() {
        // an event that lands after the deadline is processed before any
        // deadline evaluation: its block is the boundary, not the fallback
        let mut m = HandshakeMachine::new();
        m.handle(0, header_ts(5, 50));
        m.handle(0, header_ts(6, 60));
        let effect = m.handle(LOG_CATCHUP_SETTLE_MS + 1, pool(Some(9), 0));
        assert_eq!(effect_label(&effect), "complete");
        assert_eq!(completion_parts(&effect), Some((9, 60)));
    }

    #[test]
    fn stream_timeout_emits_the_rpc_fallback_effect() {
        for setup in [
            vec![],
            vec![(0, header(5))],
            vec![(0, header(5)), (0, header(6))],
        ] {
            let mut m = HandshakeMachine::new();
            for (now, event) in setup {
                m.handle(now, event);
            }
            let before = m.state().cloned();
            assert_eq!(
                effect_label(&m.handle(0, HandshakeEvent::StreamTimeout)),
                "fetch"
            );
            assert_eq!(
                m.state(),
                before.as_ref(),
                "timeout must not alter the phase"
            );
        }
    }

    #[test]
    fn latest_block_success_completes_degraded_without_inventing_a_confirmed_head() {
        let mut m = HandshakeMachine::new();
        m.handle(0, pool(Some(5), 0));
        m.handle(0, header_ts(5, 50));
        assert_eq!(
            effect_label(&m.handle(0, HandshakeEvent::StreamTimeout)),
            "fetch"
        );
        let effect = m.handle(0, HandshakeEvent::LatestBlock(999));
        assert_eq!(effect_label(&effect), "complete");
        // degraded: the RPC block, a zero timestamp, and the pending logs preserved
        assert_eq!(completion_parts(&effect), Some((999, 0)));
        let HandshakeEffect::Complete(c) = &effect else {
            return;
        };
        assert_eq!(c.pending.len(), 1);
    }

    #[test]
    fn latest_block_failure_keeps_waiting_then_follows_the_existing_rules() {
        let mut m = HandshakeMachine::new();
        m.handle(0, header_ts(5, 50));
        assert_eq!(
            effect_label(&m.handle(0, HandshakeEvent::StreamTimeout)),
            "fetch"
        );
        assert_eq!(
            effect_label(&m.handle(0, HandshakeEvent::LatestBlockFailed)),
            "continue"
        );
        assert_eq!(
            m.state(),
            Some(&HandshakeState::AwaitingConfirmation {
                prev_header: 5,
                prev_timestamp: 50,
            })
        );
        m.handle(0, pool(Some(5), 0));
        let effect = m.handle(0, header_ts(6, 60));
        assert_eq!(effect_label(&effect), "complete");
        assert_eq!(completion_parts(&effect), Some((5, 60)));
    }

    #[test]
    fn latest_block_failure_after_the_deadline_falls_through_to_the_fallback() {
        let mut m = HandshakeMachine::new();
        m.handle(0, header_ts(5, 50));
        m.handle(0, header_ts(6, 60));
        assert_eq!(
            effect_label(&m.handle(LOG_CATCHUP_SETTLE_MS, HandshakeEvent::StreamTimeout)),
            "fetch"
        );
        let effect = m.handle(LOG_CATCHUP_SETTLE_MS, HandshakeEvent::LatestBlockFailed);
        assert_eq!(effect_label(&effect), "complete");
        assert_eq!(completion_parts(&effect), Some((5, 60)));
    }

    fn pending_indices(events: &[IngestEvent]) -> Vec<Option<u64>> {
        events
            .iter()
            .map(|ev| match ev {
                IngestEvent::Pool(pe) => pe.log_index,
                IngestEvent::BlockHeader { .. } => None,
            })
            .collect()
    }

    #[test]
    fn pending_logs_are_kept_once_in_arrival_order() {
        let mut m = HandshakeMachine::new();
        m.handle(0, pool(Some(1), 0));
        m.handle(0, pool(Some(2), 1));
        m.handle(0, header(5));
        m.handle(0, pool(Some(3), 2));
        let effect = m.handle(0, header_ts(6, 60));
        assert_eq!(effect_label(&effect), "complete");
        let HandshakeEffect::Complete(c) = &effect else {
            return;
        };
        assert_eq!(c.first_block, 1);
        assert_eq!(pending_indices(&c.pending), vec![Some(0), Some(1), Some(2)]);
    }

    #[test]
    fn shutdown_completes_at_zero_and_preserves_pending() {
        let mut m = HandshakeMachine::new();
        m.handle(0, pool(Some(5), 0));
        m.handle(0, header(5));
        let effect = m.handle(0, HandshakeEvent::Shutdown);
        assert_eq!(effect_label(&effect), "complete");
        assert_eq!(completion_parts(&effect), Some((0, 0)));
        let HandshakeEffect::Complete(c) = &effect else {
            return;
        };
        assert_eq!(pending_indices(&c.pending), vec![Some(0)]);
    }

    #[test]
    fn shutdown_while_settling_still_completes_at_zero() {
        let mut m = HandshakeMachine::new();
        m.handle(0, header_ts(5, 50));
        m.handle(0, header_ts(6, 60));
        let effect = m.handle(0, HandshakeEvent::Shutdown);
        assert_eq!(effect_label(&effect), "complete");
        assert_eq!(completion_parts(&effect), Some((0, 0)));
    }

    #[test]
    fn stream_end_falls_back_to_the_anchor_and_preserves_pending() {
        let mut m = HandshakeMachine::new();
        m.handle(0, pool(Some(7), 0));
        m.handle(0, header_ts(5, 50));
        let effect = m.handle(0, HandshakeEvent::StreamEnded);
        assert_eq!(effect_label(&effect), "complete");
        assert_eq!(completion_parts(&effect), Some((5, 50)));
        let HandshakeEffect::Complete(c) = &effect else {
            return;
        };
        assert_eq!(pending_indices(&c.pending), vec![Some(0)]);
    }

    #[test]
    fn stream_end_without_a_header_completes_at_zero() {
        let mut m = HandshakeMachine::new();
        m.handle(0, pool(Some(9), 0));
        let effect = m.handle(0, HandshakeEvent::StreamEnded);
        assert_eq!(effect_label(&effect), "complete");
        assert_eq!(completion_parts(&effect), Some((0, 0)));
        let HandshakeEffect::Complete(c) = &effect else {
            return;
        };
        assert_eq!(pending_indices(&c.pending), vec![Some(0)]);
    }

    /// Drive a scripted setup and probe exactly one event, asserting the
    /// effect, the completion parts, and the resulting phase.
    fn check(
        name: &str,
        setup: Vec<(u64, HandshakeEvent)>,
        now_ms: u64,
        probe: HandshakeEvent,
        want_label: &'static str,
        want_parts: Option<(u64, u64)>,
        want_state: Option<&HandshakeState>,
    ) {
        let mut m = HandshakeMachine::new();
        for (now, event) in setup {
            m.handle(now, event);
        }
        let effect = m.handle(now_ms, probe);
        assert_eq!(effect_label(&effect), want_label, "case {name}");
        assert_eq!(completion_parts(&effect), want_parts, "case {name}");
        assert_eq!(m.state(), want_state, "case {name}");
    }

    fn anchoring(prev_header: u64, prev_timestamp: u64) -> HandshakeState {
        HandshakeState::AwaitingConfirmation {
            prev_header,
            prev_timestamp,
        }
    }

    fn settling(
        confirmed_head: u64,
        settle_deadline: u64,
        prev_header: u64,
        prev_timestamp: u64,
    ) -> HandshakeState {
        HandshakeState::Settling {
            confirmed_head,
            settle_deadline,
            prev_header,
            prev_timestamp,
        }
    }

    const FRESH: HandshakeState = HandshakeState::AwaitingFirstHeader;

    #[test]
    fn matrix_awaiting_first_header() {
        let setup = Vec::new();
        check(
            "A/header",
            setup.clone(),
            0,
            header_ts(7, 70),
            "continue",
            None,
            Some(&anchoring(7, 70)),
        );
        check(
            "A/pool-with-block",
            setup.clone(),
            0,
            pool(Some(4), 0),
            "continue",
            None,
            Some(&FRESH),
        );
        check(
            "A/pool-blockless",
            setup.clone(),
            0,
            pool(None, 0),
            "continue",
            None,
            Some(&FRESH),
        );
        check(
            "A/tick",
            setup.clone(),
            0,
            HandshakeEvent::Tick,
            "continue",
            None,
            Some(&FRESH),
        );
        check(
            "A/stream-timeout",
            setup.clone(),
            0,
            HandshakeEvent::StreamTimeout,
            "fetch",
            None,
            Some(&FRESH),
        );
        check(
            "A/latest-block",
            setup.clone(),
            0,
            HandshakeEvent::LatestBlock(11),
            "complete",
            Some((11, 0)),
            None,
        );
        check(
            "A/latest-block-failed",
            setup.clone(),
            0,
            HandshakeEvent::LatestBlockFailed,
            "continue",
            None,
            Some(&FRESH),
        );
        check(
            "A/stream-ended",
            setup.clone(),
            0,
            HandshakeEvent::StreamEnded,
            "complete",
            Some((0, 0)),
            None,
        );
        check(
            "A/shutdown",
            setup,
            0,
            HandshakeEvent::Shutdown,
            "complete",
            Some((0, 0)),
            None,
        );
    }

    #[test]
    fn matrix_awaiting_confirmation() {
        let setup = vec![(0, header_ts(5, 50))];
        let anchored = anchoring(5, 50);
        check(
            "B/header-confirm",
            setup.clone(),
            0,
            header_ts(6, 60),
            "continue",
            None,
            Some(&settling(5, LOG_CATCHUP_SETTLE_MS, 5, 60)),
        );
        check(
            "B/header-jump",
            setup.clone(),
            0,
            header_ts(8, 80),
            "continue",
            None,
            Some(&anchoring(8, 80)),
        );
        check(
            "B/header-duplicate",
            setup.clone(),
            0,
            header_ts(5, 50),
            "continue",
            None,
            Some(&anchored),
        );
        check(
            "B/header-stale",
            setup.clone(),
            0,
            header_ts(4, 40),
            "continue",
            None,
            Some(&anchored),
        );
        check(
            "B/pool",
            setup.clone(),
            0,
            pool(Some(2), 0),
            "continue",
            None,
            Some(&anchored),
        );
    }

    #[test]
    fn matrix_awaiting_confirmation_effects() {
        let setup = vec![(0, header_ts(5, 50))];
        let anchored = anchoring(5, 50);
        check(
            "B/tick",
            setup.clone(),
            0,
            HandshakeEvent::Tick,
            "continue",
            None,
            Some(&anchored),
        );
        check(
            "B/stream-timeout",
            setup.clone(),
            0,
            HandshakeEvent::StreamTimeout,
            "fetch",
            None,
            Some(&anchored),
        );
        check(
            "B/latest-block",
            setup.clone(),
            0,
            HandshakeEvent::LatestBlock(11),
            "complete",
            Some((11, 0)),
            None,
        );
        check(
            "B/latest-block-failed",
            setup.clone(),
            0,
            HandshakeEvent::LatestBlockFailed,
            "continue",
            None,
            Some(&anchored),
        );
        check(
            "B/stream-ended",
            setup.clone(),
            0,
            HandshakeEvent::StreamEnded,
            "complete",
            Some((5, 50)),
            None,
        );
        check(
            "B/shutdown",
            setup,
            0,
            HandshakeEvent::Shutdown,
            "complete",
            Some((0, 0)),
            None,
        );
    }

    #[test]
    fn matrix_settling_headers_and_logs() {
        let setup = vec![(0, header_ts(5, 50)), (0, header_ts(6, 60))];
        let open = settling(5, LOG_CATCHUP_SETTLE_MS, 5, 60);
        check(
            "C/header-reconfirm",
            setup.clone(),
            100,
            header_ts(6, 60),
            "continue",
            None,
            Some(&open),
        );
        check(
            "C/header-jump",
            setup.clone(),
            100,
            header_ts(9, 90),
            "continue",
            None,
            Some(&settling(5, LOG_CATCHUP_SETTLE_MS, 9, 90)),
        );
        check(
            "C/header-duplicate",
            setup.clone(),
            100,
            header_ts(5, 50),
            "continue",
            None,
            Some(&open),
        );
        check(
            "C/header-stale",
            setup.clone(),
            100,
            header_ts(3, 30),
            "continue",
            None,
            Some(&open),
        );
        check(
            "C/pool-above-head",
            setup.clone(),
            100,
            pool(Some(7), 0),
            "continue",
            None,
            Some(&open),
        );
        check(
            "C/pool-at-head",
            setup.clone(),
            100,
            pool(Some(5), 0),
            "complete",
            Some((5, 60)),
            None,
        );
        check(
            "C/pool-blockless",
            setup.clone(),
            100,
            pool(None, 0),
            "continue",
            None,
            Some(&open),
        );
        check(
            "C/stream-ended",
            setup.clone(),
            100,
            HandshakeEvent::StreamEnded,
            "complete",
            Some((5, 60)),
            None,
        );
        check(
            "C/shutdown",
            setup,
            100,
            HandshakeEvent::Shutdown,
            "complete",
            Some((0, 0)),
            None,
        );
    }

    #[test]
    fn matrix_settling_effects() {
        let setup = vec![(0, header_ts(5, 50)), (0, header_ts(6, 60))];
        let open = settling(5, LOG_CATCHUP_SETTLE_MS, 5, 60);
        check(
            "C/tick",
            setup.clone(),
            100,
            HandshakeEvent::Tick,
            "continue",
            None,
            Some(&open),
        );
        check(
            "C/stream-timeout",
            setup.clone(),
            100,
            HandshakeEvent::StreamTimeout,
            "fetch",
            None,
            Some(&open),
        );
        check(
            "C/latest-block",
            setup.clone(),
            100,
            HandshakeEvent::LatestBlock(11),
            "complete",
            Some((11, 0)),
            None,
        );
        check(
            "C/latest-block-failed",
            setup,
            100,
            HandshakeEvent::LatestBlockFailed,
            "continue",
            None,
            Some(&open),
        );
    }

    #[test]
    fn matrix_settling_past_the_deadline() {
        let setup = vec![(0, header_ts(5, 50)), (0, header_ts(6, 60))];
        let late = LOG_CATCHUP_SETTLE_MS;
        check(
            "C-late/tick",
            setup.clone(),
            late,
            HandshakeEvent::Tick,
            "complete",
            Some((5, 60)),
            None,
        );
        check(
            "C-late/latest-block-failed",
            setup.clone(),
            late,
            HandshakeEvent::LatestBlockFailed,
            "complete",
            Some((5, 60)),
            None,
        );
        check(
            "C-late/pool-above-head",
            setup.clone(),
            late,
            pool(Some(9), 0),
            "complete",
            Some((9, 60)),
            None,
        );
        check(
            "C-late/header",
            setup,
            late,
            header_ts(6, 60),
            "complete",
            Some((5, 60)),
            None,
        );
    }

    #[test]
    fn matrix_spent_machine() {
        let mut setup = vec![(0, header_ts(5, 50)), (0, header_ts(6, 60))];
        setup.push((LOG_CATCHUP_SETTLE_MS, HandshakeEvent::Tick));
        check(
            "D/header",
            setup.clone(),
            20_000,
            header(7),
            "continue",
            None,
            None,
        );
        check(
            "D/pool",
            setup.clone(),
            20_000,
            pool(Some(7), 0),
            "continue",
            None,
            None,
        );
        check(
            "D/tick",
            setup.clone(),
            20_000,
            HandshakeEvent::Tick,
            "continue",
            None,
            None,
        );
        check(
            "D/stream-timeout",
            setup.clone(),
            20_000,
            HandshakeEvent::StreamTimeout,
            "continue",
            None,
            None,
        );
        check(
            "D/latest-block",
            setup.clone(),
            20_000,
            HandshakeEvent::LatestBlock(11),
            "continue",
            None,
            None,
        );
        check(
            "D/latest-block-failed",
            setup.clone(),
            20_000,
            HandshakeEvent::LatestBlockFailed,
            "continue",
            None,
            None,
        );
        check(
            "D/stream-ended",
            setup.clone(),
            20_000,
            HandshakeEvent::StreamEnded,
            "continue",
            None,
            None,
        );
        check(
            "D/shutdown",
            setup,
            20_000,
            HandshakeEvent::Shutdown,
            "continue",
            None,
            None,
        );
    }

    #[test]
    fn confirmation_without_log_liveness_waits() {
        let mut m = HandshakeMachine::new();
        assert_eq!(effect_label(&m.handle(0, header(5))), "continue");
        assert_eq!(effect_label(&m.handle(0, header(6))), "continue");
        assert_eq!(
            m.state(),
            Some(&HandshakeState::Settling {
                confirmed_head: 5,
                settle_deadline: LOG_CATCHUP_SETTLE_MS,
                prev_header: 5,
                prev_timestamp: 0,
            })
        );
    }
}
