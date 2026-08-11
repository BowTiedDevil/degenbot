//! The **`PumpDecision` producer** (epic A) — a pure, I/O-free decision machine
//! for `BlockPump::run_with_stream`, following the `BlockClock` precedent
//! (ADR-008): the per-event rules about *which effect, when, with what lock*
//! live here as decisions; the pump loop is a thin async driver that feeds
//! `(WsEvent + clock + tick)` in and executes the returned `PumpDecision`s
//! against the executor (`DispatchOwner` + provider + sink + reorg
//! coordinator). This module holds NO provider, NO timers, NO `Instant`, and
//! NO locks — all time enters as a `now_ms` argument and all I/O is returned
//! for the driver to run.
//!
//! The I/O the FSM cannot do — backfill over `eth_getLogs`, the reorg
//! coordinator, WS-completeness RPC — is expressed as `PumpDecision`
//! variants the driver executes. The FSM's decision state is `pub` so the
//! driver's I/O helpers (which are the *executor*, not the decider) can fold
//! results back; A2–A5 replace those scattered writes with explicit FSM
//! update methods as each rule is encapsulated.

use std::collections::{HashMap, HashSet};

use crate::bot_core::block_clock::{BlockClock, HeaderDecision};
use crate::bot_core::block_pump::RELEVANT_TOPICS;
use crate::bot_core::BlockMetadata;
use alloy::primitives::B256;

/// One consequence the pump's decision machine can emit. The driver maps each
/// variant onto its executor (the dispatch owner, the sink, the provider, the
/// reorg coordinator, or the process itself).
#[derive(Debug)]
pub enum PumpDecision {
    /// Eager solve of every dirty path (top-of-loop). The driver runs
    /// `dispatch.dispatch(DrainWork::Drain{..})` and `sink.set_last_solved_block`.
    Drain { block: u64, metadata: BlockMetadata },
    /// Quiesce-gated publish (ADR-008 D2) at a settle point. The driver
    /// fetches the change-set and runs `dispatch.dispatch(DrainWork::Publish)`.
    Publish { open: u64, metadata: BlockMetadata },
    /// Tombstone finalize of a fully-delivered block (VTWCIG metadata). The
    /// driver runs `dispatch.dispatch(DrainWork::Finalize{..})`.
    Finalize { block: u64, metadata: BlockMetadata },
    /// Block-clock notification for Python's head tracker (B2). The driver
    /// runs `dispatch.notify_block(block, metadata)`.
    Notify { block: u64, metadata: BlockMetadata },
    /// Mark a block solved on the engine (LEZJAS). The driver runs
    /// `sink.set_last_solved_block(block)`.
    SetLastSolved { block: u64 },
    /// Head-gap or inactivity backfill over `[from, to]` via `eth_getLogs`
    /// (from == to+1 sentinel when the driver must own the range entirely).
    /// `to` is the exclusive upper bound resolved by the driver.
    Backfill { from: u64, to: Option<u64> },
    /// A graceful stop (shutdown flag or unrecoverable state). The driver
    /// returns from the loop.
    Stop,
}

impl PumpDecision {
    /// True for variants that end the loop (the driver returns).
    #[must_use]
    pub fn stops(&self) -> bool {
        matches!(self, PumpDecision::Stop)
    }
}

/// The pure, stateful decision machine for the block pump. Owns every rule
/// about which effect happens when; all time is injected (`now_ms`), all I/O
/// is returned as [`PumpDecision`] / handled by the driver.
pub struct PumpFSM {
    /// The last block the pump's cursor has reached.
    pub current_block: u64,
    /// Metadata of `current_block` (held from its header).
    pub current_metadata: BlockMetadata,
    /// Whether we're before the first header after a resume/backfill.
    pub first_header: bool,
    /// Whether a quiesce-gated publish is armed (a forward log applied).
    pub publish_pending: bool,
    /// BQ7ZBC — the highest block an authoritative catch-up has owned.
    pub recovery_anchor: u64,
    /// Per-block metadata snapshots (deferred tombstone finalize, VTWCIG).
    pub block_metadata: HashMap<u64, BlockMetadata>,
    /// WS-delivery completeness tracker (relevant log indices per block).
    pub ws_delivered: HashMap<u64, HashSet<u64>>,
    /// [DIAG] + watchdog time anchors, injected as wall-clock ms by the driver.
    pub last_header_at_ms: u64,
    pub last_log_at_ms: u64,
    pub last_diag_ms: u64,
    /// [DIAG] counters.
    pub diag_header_count: u64,
    pub diag_log_count: u64,
    /// Logs-silence watchdog re-arm.
    pub log_silence_alarm_armed: bool,
    /// The per-block clock (the tombstone/cursor authority, ADR-008).
    pub clock: BlockClock,
}

impl PumpFSM {
    #[must_use]
    pub fn new(current_block: u64, now_ms: u64) -> Self {
        Self {
            current_block,
            current_metadata: BlockMetadata::default(),
            first_header: true,
            publish_pending: false,
            recovery_anchor: 0,
            block_metadata: HashMap::new(),
            ws_delivered: HashMap::new(),
            last_header_at_ms: now_ms,
            last_log_at_ms: now_ms,
            last_diag_ms: now_ms,
            diag_header_count: 0,
            diag_log_count: 0,
            log_silence_alarm_armed: false,
            clock: BlockClock::new(),
        }
    }

    /// Whether a `WsEvent::Log` is relevant to any tracked pool (the driver's
    /// fast-path topic pre-filter, the lock-avoidance gate).
    #[must_use]
    pub fn is_relevant_topic(log_topics_first: Option<&B256>) -> bool {
        log_topics_first.is_some_and(|t| RELEVANT_TOPICS.contains(t))
    }

    /// The top-of-loop dirty-drain decision (only called when the driver sees
    /// dirty paths). Emits the solve anchor per ADR-008 D2.
    #[must_use]
    pub fn drain_decision(&self, state_head: u64) -> PumpDecision {
        PumpDecision::Drain {
            block: self.solve_anchor(state_head),
            metadata: self.current_metadata,
        }
    }

    /// The solver-release solve anchor (ADR-008 D2): the LOG-DRIVEN settled
    /// block (`clock.latest_observed()`), falling back to `current_block` when
    /// no block logs are open, maximised against the pool-state head so a
    /// backfill-ahead stall never solves below the state it solves against.
    fn solve_anchor(&self, state_head: u64) -> u64 {
        self.clock
            .latest_observed()
            .unwrap_or(self.current_block)
            .max(state_head)
    }

    /// The per-event decision for a `WsEvent::BlockHeader`. Returns the effects
    /// the driver must execute (notify, backfill, mark-solved).
    pub fn on_header(
        &mut self,
        number: u64,
        metadata: BlockMetadata,
        now_ms: u64,
    ) -> Vec<PumpDecision> {
        let mut decisions = Vec::new();
        self.diag_header_count += 1;
        self.last_header_at_ms = now_ms;

        // Snapshot the just-finished block's metadata BEFORE overwriting
        // `current_metadata` (VTWCIG): the batch finalizing `current_block`
        // must carry ITS metadata, not the incoming header's.
        self.current_metadata = metadata;
        if matches!(self.clock.observe_header(number), HeaderDecision::Stale) {
            return decisions; // duplicate/stale header — no effect.
        }
        self.block_metadata.insert(number, self.current_metadata);

        if self.first_header {
            self.first_header = false;
            if number > self.current_block {
                if number > self.current_block + 1 {
                    decisions.push(PumpDecision::Backfill {
                        from: self.current_block + 1,
                        to: Some(number - 1),
                    });
                    self.recovery_anchor = self.recovery_anchor.max(number - 1);
                }
                self.current_block = number;
                decisions.push(PumpDecision::SetLastSolved { block: number });
                decisions.push(PumpDecision::Notify {
                    block: number,
                    metadata: self.current_metadata,
                });
            }
        } else if number > self.current_block {
            if number > self.current_block + 1 {
                decisions.push(PumpDecision::Backfill {
                    from: self.current_block + 1,
                    to: Some(number - 1),
                });
            }
            self.current_block = number;
            decisions.push(PumpDecision::Notify {
                block: number,
                metadata: self.current_metadata,
            });
        }
        decisions
    }

    /// The settle-point decision (no new event in the window): the quiesce-
    /// gated publish (ADR-008 D2) when armed, else the inactivity backfill.
    pub fn on_settle(&mut self) -> Vec<PumpDecision> {
        let mut decisions = Vec::new();
        if self.publish_pending {
            if let Some(open) = self.clock.latest_observed() {
                if self.clock.consume_quiesced(open) {
                    decisions.push(PumpDecision::Publish {
                        open,
                        metadata: self.current_metadata,
                    });
                }
            }
            self.publish_pending = false;
        } else {
            // 60s inactivity — the driver resolves the range and backfills.
            decisions.push(PumpDecision::Backfill {
                from: self.current_block + 1,
                to: None,
            });
        }
        decisions
    }

    /// The stream-exhaustion (final settle) decision: flush any pending
    /// quiesce-gated publish, then stop.
    pub fn on_stream_end(&mut self) -> Vec<PumpDecision> {
        let mut decisions = Vec::new();
        if self.publish_pending {
            if let Some(open) = self.clock.latest_observed() {
                if self.clock.consume_quiesced(open) {
                    decisions.push(PumpDecision::Publish {
                        open,
                        metadata: self.current_metadata,
                    });
                }
            }
            self.publish_pending = false;
        }
        decisions.push(PumpDecision::Stop);
        decisions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(ts: u64) -> BlockMetadata {
        BlockMetadata {
            timestamp: ts,
            base_fee_per_gas: Some(ts),
            gas_used: 1,
            gas_limit: 2,
        }
    }

    #[test]
    fn header_notify_and_solve_anchor_from_clock() {
        let mut fsm = PumpFSM::new(100, 0);
        // First header after resume > current: mark solved + notify, no drain.
        let d = fsm.on_header(101, meta(101_000), 1_000);
        assert!(matches!(d[0], PumpDecision::SetLastSolved { block: 101 }));
        assert!(matches!(d[1], PumpDecision::Notify { block: 101, .. }));
        assert_eq!(fsm.current_block, 101);

        // Sequential header: notify only (never marks solved again).
        let d = fsm.on_header(102, meta(102_000), 2_000);
        assert_eq!(d.len(), 1);
        assert!(matches!(&d[0], PumpDecision::Notify { block: 102, .. }));
    }

    #[test]
    fn header_gap_backfills_and_advances_recovery_anchor() {
        let mut fsm = PumpFSM::new(100, 0);
        // Non-first-gap: first header to close a gap.
        let d = fsm.on_header(105, meta(105_000), 1_000);
        assert!(d.iter().any(|x| matches!(
            x,
            PumpDecision::Backfill {
                from: 101,
                to: Some(104)
            }
        )));
        assert_eq!(fsm.recovery_anchor, 104);
    }

    #[test]
    fn stale_header_produces_no_effect() {
        let mut fsm = PumpFSM::new(100, 0);
        let _ = fsm.on_header(101, meta(101_000), 1_000);
        let before = fsm.current_block;
        // A duplicate/stale header (< current) must not advance or fire.
        let d = fsm.on_header(100, meta(100_000), 2_000);
        assert!(d.is_empty());
        assert_eq!(fsm.current_block, before);
    }

    #[test]
    fn settle_publishes_quiesced_block_or_backfills() {
        // Nothing pending → inactivity backfill (range owned by driver).
        let mut fsm = PumpFSM::new(200, 0);
        let d = fsm.on_settle();
        assert!(matches!(
            d[0],
            PumpDecision::Backfill {
                from: 201,
                to: None
            }
        ));
        // A forward log arms a pending publish + makes the block observable: a
        // settle emits the quiesce-gated Publish and clears the arm.
        let mut fsm = PumpFSM::new(200, 0);
        fsm.clock.observe_log(201, false);
        fsm.clock.log_received(201);
        fsm.clock.log_applied(201);
        fsm.publish_pending = true;
        let d = fsm.on_settle();
        assert!(d
            .iter()
            .any(|x| matches!(x, PumpDecision::Publish { open: 201, .. })));
        assert!(!fsm.publish_pending);
    }

    #[test]
    fn quiesce_gate_emits_exactly_one_publish_never_premature() {
        // Premature: a log is armed but still in-flight (received, not yet
        // applied) — the block is NOT quiesced, so a settle must NOT publish.
        // The arm is still cleared (the burst is still draining).
        let mut fsm = PumpFSM::new(200, 0);
        fsm.clock.observe_log(201, false);
        fsm.clock.log_received(201); // in_flight = 1, never quiesced
        fsm.publish_pending = true;
        let d = fsm.on_settle();
        assert!(
            d.iter().all(|x| !matches!(x, PumpDecision::Publish { .. })),
            "premature publish before the block quiesces: {d:?}"
        );
        assert!(
            !fsm.publish_pending,
            "arm cleared even when not yet quiesced"
        );

        // Quiesced: the applied log flips `ever_quiesced`. A settle emits
        // EXACTLY one Publish for the block, never a second.
        let mut fsm = PumpFSM::new(200, 0);
        fsm.clock.observe_log(201, false);
        fsm.clock.log_received(201);
        fsm.clock.log_applied(201); // in_flight = 0 → quiesced
        fsm.publish_pending = true;
        let d = fsm.on_settle();
        let publishes = d
            .iter()
            .filter(|x| matches!(x, PumpDecision::Publish { .. }))
            .count();
        assert_eq!(publishes, 1, "exactly one publish for the quiesced block");
        assert!(d
            .iter()
            .any(|x| matches!(x, PumpDecision::Publish { open: 201, .. })));
        // A second settle with nothing new armed must not re-publish: the
        // quiesce signal was consumed by the first settle.
        let d2 = fsm.on_settle();
        assert!(
            d2.iter()
                .all(|x| !matches!(x, PumpDecision::Publish { .. })),
            "no duplicate publish — the quiesce signal was consumed: {d2:?}"
        );
    }

    #[test]
    fn stream_end_flushes_then_stops() {
        let mut fsm = PumpFSM::new(300, 0);
        fsm.publish_pending = true;
        let d = fsm.on_stream_end();
        assert!(d.last().is_some_and(PumpDecision::stops));
    }
}

#[test]
fn drain_decision_anchor_follows_log_driven_block_not_racing_header() {
    // Header races ahead to 102 while only block 101's logs are open:
    // anchor at 101 (open), NOT 102 (current_block).
    let mut fsm = PumpFSM::new(102, 0);
    fsm.clock.observe_log(101, false);
    fsm.clock.log_received(101);
    fsm.clock.log_applied(101);
    let PumpDecision::Drain { block, .. } = fsm.drain_decision(100) else {
        panic!("drain_decision must emit a Drain");
    };
    assert_eq!(
        block, 101,
        "anchor at the open (log-driven) block, not the racing header"
    );
    // State head dominates on a backfill-ahead stall.
    let PumpDecision::Drain { block, .. } = fsm.drain_decision(500) else {
        unreachable!()
    };
    assert_eq!(block, 500);
    // No open block yet (cold start, headers only): fall back to the header.
    let fsm2 = PumpFSM::new(102, 0);
    let PumpDecision::Drain { block, .. } = fsm2.drain_decision(100) else {
        unreachable!()
    };
    assert_eq!(block, 102);
}
