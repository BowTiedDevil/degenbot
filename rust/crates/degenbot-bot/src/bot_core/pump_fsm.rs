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
    /// A watchdog tick concluded headers have been stale for >= the staleness
    /// window (JIABO3). The driver runs `handle_timeout_eager` (an
    /// authoritative `eth_getLogs` catch-up).
    Recover,
    /// A watchdog tick concluded the logs subscription is silent (headers
    /// fresh but no log arrived in the window). The driver emits one diagnostic
    /// warning per silence episode, then re-arms on the next log.
    LogSilence,
    /// WS-delivery completeness cross-check (DFQYM5 / WS-DROP): a block has
    /// been tombstoned (confirmed fully delivered); the FSM hands the tracked
    /// delivered relevant log-index set to the driver, which fetches
    /// `eth_getLogs` and aborts on any on-chain log the websocket missed. The
    /// FSM owns *when* to verify (only a complete block); the abort is the
    /// executor's consequence of the authoritative mismatch.
    VerifyCompleteness {
        block: u64,
        delivered_log_indices: HashSet<u64>,
    },
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

    /// BQ7ZBC — record that an authoritative catch-up (a header-gap backfill
    /// or a `handle_timeout_eager` recovery) has OWNED the range up to
    /// `through`. Per the single-writer rule (DFQYM5), the live WS no longer
    /// owns any block ≤ `recovery_anchor`, so later recovered forwards there
    /// are benign duplicates (dropped) rather than re-asserted faults.
    pub fn record_backfill(&mut self, through: u64) {
        self.recovery_anchor = self.recovery_anchor.max(through);
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

    /// BQ7ZBC / DFQYM5 single-writer recovery discard. A stalled WS that
    /// recovers flushes buffered forward logs for blocks ≤ `recovery_anchor` —
    /// duplicates of state the authoritative catch-up already applied. These
    /// are DROPPED (never reaching `observe_log`/`PanicLateForward`). Reorg
    /// logs (`removed: true`) are NEVER dropped — they must reach the reorg
    /// classifier to unwind the backfilled range. A forward ABOVE
    /// `recovery_anchor` that is still stale remains a hard ADR-008 D3 fault
    /// (only the pump's own single-writer range is benign).
    #[must_use]
    pub fn should_drop_recovered_forward(&self, log_block: u64, removed: bool) -> bool {
        !removed && self.recovery_anchor > 0 && log_block <= self.recovery_anchor
    }

    /// Feed a log-activity event (a `WsEvent::Log` that passed the topic
    /// pre-filter). Refreshes the logs-silence watchdog clock and re-arms the
    /// alarm so one diagnostic warning fires per silence episode.
    pub fn record_log(&mut self, now_ms: u64) {
        self.last_log_at_ms = now_ms;
        self.log_silence_alarm_armed = false;
    }

    /// Feed a header-activity event. Refreshes the header-staleness watchdog
    /// clock (the still-inline header arm updates it here; `on_header` sets it
    /// too once the arm routes through the FSM).
    pub fn record_header(&mut self, now_ms: u64) {
        self.last_header_at_ms = now_ms;
    }

    /// The watchdog tick (JIABO3 / logs-silence): the driver's interval fires
    /// and feeds a synthetic `now_ms`; the windows enter as data
    /// (`header_staleness_ms`, `log_silence_ms`). Decides, from elapsed-time
    /// only: `Recover` when headers have been stale >= the staleness window
    /// (an authoritative `eth_getLogs` catch-up), else `LogSilence` (once per
    /// silenced episode) when headers are fresh but no log has arrived in
    /// `log_silence_ms`. The FSM owns no timer.
    pub fn on_tick(
        &mut self,
        now_ms: u64,
        header_staleness_ms: u64,
        log_silence_ms: u64,
    ) -> Vec<PumpDecision> {
        let mut decisions = Vec::new();
        if now_ms.saturating_sub(self.last_header_at_ms) >= header_staleness_ms {
            decisions.push(PumpDecision::Recover);
        } else if now_ms.saturating_sub(self.last_log_at_ms) >= log_silence_ms
            && !self.log_silence_alarm_armed
        {
            self.log_silence_alarm_armed = true;
            decisions.push(PumpDecision::LogSilence);
        }
        decisions
    }

    /// The WS-completeness verdict decision (DFQYM5 / WS-DROP) at a block's
    /// tombstone. The FSM owns the rule — only a just-confirmed-complete block
    /// (`prev`, tombstoned by the first log of N+1) is verified, and only when
    /// it was actually tracked (a block with no delivered relevant logs has
    /// nothing to cross-check; the authoritative side is also empty). Hands the
    /// tracked delivered log-index set to the driver, which runs the `eth_getLogs`
    /// cross-check and aborts on a live-websocket log drop.
    #[must_use]
    pub fn completeness_decision(&mut self, prev: u64) -> PumpDecision {
        PumpDecision::VerifyCompleteness {
            block: prev,
            delivered_log_indices: self.ws_delivered.remove(&prev).unwrap_or_default(),
        }
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
    fn completeness_decision_verifies_tracked_block_and_clears_it() {
        let mut fsm = PumpFSM::new(200, 0);
        // Track delivered relevant log indices for block 201.
        fsm.ws_delivered.entry(201).or_default().extend([7, 8, 9]);

        // Tombstone → the FSM passes the delivered set to the driver and
        // clears the tracking map (one-shot: a re-verify yields empty).
        let PumpDecision::VerifyCompleteness {
            block,
            delivered_log_indices,
        } = fsm.completeness_decision(201)
        else {
            panic!("completeness_decision must emit VerifyCompleteness");
        };
        assert_eq!(block, 201);
        assert_eq!(delivered_log_indices, HashSet::from([7, 8, 9]));
        assert!(!fsm.ws_delivered.contains_key(&201), "tracked set consumed");

        // A block with no tracked relevant logs yields an empty set (the
        // authoritative side is empty too, so the cross-check is a no-op).
        let PumpDecision::VerifyCompleteness {
            delivered_log_indices,
            ..
        } = fsm.completeness_decision(202)
        else {
            unreachable!()
        };
        assert!(delivered_log_indices.is_empty());
    }

    #[test]
    fn reorg_routing_owned_by_clock_classification() {
        // Reorg routing is FSM state: the clock (part of the FSM) classifies
        // removed logs into an unwind window (enter → continue → close). The
        // driver only executes the classified routing against the coordinator.
        let mut fsm = PumpFSM::new(200, 0);
        use crate::bot_core::block_clock::LogDecision;
        // Enter: a removed:true log opens the reorg window at its block.
        assert!(matches!(
            fsm.clock.observe_log(201, true),
            LogDecision::EnterReorg(_)
        ));
        // Continue: a further removed log in the same window.
        assert!(matches!(
            fsm.clock.observe_log(201, true),
            LogDecision::ContinueReorg
        ));
        // Close: the window ends and forward tracking resumes from a new head.
        assert!(matches!(
            fsm.clock.observe_log(202, false),
            LogDecision::CloseReorg { .. }
        ));
    }

    #[test]
    fn watchdog_tick_fires_on_stale_not_fresh_and_arms_code_episode() {
        let mut fsm = PumpFSM::new(200, 1_000);
        // Fresh (just recorded a header): a tick must not fire.
        fsm.record_header(1_000);
        fsm.record_log(1_000);
        assert!(fsm.on_tick(1_050, 500, 300).is_empty());

        // Headers stale: Recover fires (header clock quiet long enough).
        let d = fsm.on_tick(1_600, 500, 300);
        assert!(d.iter().any(|x| matches!(x, PumpDecision::Recover)));

        // Fresh headers (new header), but logs silent: one LogSilence per
        // episode — a second tick without a log doesn't re-emit.
        fsm.record_header(1_600); // headers fresh again at 1600
        let d = fsm.on_tick(2_000, 500, 300);
        assert!(d.iter().any(|x| matches!(x, PumpDecision::LogSilence)));
        assert!(d.iter().all(|x| !matches!(x, PumpDecision::Recover)));
        // Still silent but the alarm is armed and headers stay fresh → no
        // re-emit (one LogSilence per episode).
        fsm.record_header(2_300);
        let d = fsm.on_tick(2_500, 500, 300);
        assert!(d.is_empty());
        // A log arrives → re-arms; another silence window (headers kept fresh)
        // fires a fresh LogSilence.
        fsm.record_log(2_500);
        fsm.record_header(2_600);
        let d = fsm.on_tick(2_900, 500, 300);
        assert!(d.iter().any(|x| matches!(x, PumpDecision::LogSilence)));
    }

    #[test]
    fn single_writer_recovery_anchor_drops_owned_range_only() {
        // Record an authoritative catch-up owning [.., 205]. Per the
        // single-writer rule (DFQYM5) the WS no longer owns those blocks.
        let mut fsm = PumpFSM::new(200, 0);
        fsm.record_backfill(205);
        assert_eq!(fsm.recovery_anchor, 205);

        // A recovered forward INSIDE the owned range is a benign duplicate:…
        // dropped, not re-asserted (no BQ7ZBC / PanicLateForward fault).
        assert!(fsm.should_drop_recovered_forward(205, false));
        assert!(fsm.should_drop_recovered_forward(201, false));
        // A reorg log (removed:true) is NEVER dropped — it must unwind the
        // backfilled range through the reorg classifier.
        assert!(!fsm.should_drop_recovered_forward(205, true));
        assert!(!fsm.should_drop_recovered_forward(150, true));
        // A forward ABOVE the anchor is not owned — still a hard D3 fault
        // (surfaces to the driver, never silently dropped).
        assert!(!fsm.should_drop_recovered_forward(206, false));

        // record_backfill only extends the anchor (monotone), never shrinks.
        fsm.record_backfill(200);
        assert_eq!(fsm.recovery_anchor, 205);
        fsm.record_backfill(210);
        assert_eq!(fsm.recovery_anchor, 210);
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
