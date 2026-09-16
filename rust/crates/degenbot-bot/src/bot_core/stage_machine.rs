//! **The unified block stage machine** (ADR-041).
//!
//! ONE pure, I/O-free machine owns every per-block edge condition for one
//! block epoch. The six cooperating machines fold in here as sub-state; their
//! pinned tests are the behavioral contract and live in this file verbatim:
//!
//! - `BlockClock` (ADR-008) — the per-block state map, the tombstone/cursor
//!   authority, and the reorg window. Folded: the `blocks`/`cursor`/
//!   `latest_header`/`open_block`/`in_reorg` fields below ARE that machine
//!   (the standalone `BlockClock` type is gone — hard cutover, Q6).
//! - `PumpFSM` (ADR-028) — the per-event decision producer. Folded: the
//!   remaining decision state below (publish arm, recovery anchor, rewind
//!   generation, watchdog anchors, WS-completeness tracker, metadata map).
//! - The quiesce / debounce / early-slice gates, the header-staleness +
//!   logs-silence watchdogs, the backfill trigger, the tombstone /
//!   WS-completeness verify, and the `Rewind` handling (epoch `seq` bump +
//!   staleness of pre-rewind contexts) are all machine decisions below.
//! - `DrainerHealth`'s no-progress obligation (retired by SZJUKL) is
//!   represented here: the machine exposes
//!   [`watchdog_phase`](Self::watchdog_phase) — `Healthy` / `HeaderStale` /
//!   `LogsSilent` — the phase space the dissolved accounting's watchdogs
//!   map onto, and every stage row stays reachable via the stage cycle
//!   below.
//!
//! ## The stage cycle (ADR-041 stage table)
//!
//! Streaming -> Quiesced -> Resolved -> Solved -> Simulated -> Gated ->
//! Published -> Finalized, plus `Rewind{to_epoch}` from any stage. The
//! machine tracks which stage an epoch has reached in
//! [`stage`](Self::stage), advancing it as decisions fire:
//!
//! | stage-table row | machine event that advances it |
//! |---|---|
//! | Streaming | every log `on_log`/`on_log_applied` accepts (the only writer window) |
//! | Quiesced (`StreamingComplete`) | `consume_quiesced` / the settle classification |
//! | Published | a `StageDecision::Publish` emitted by `on_settle`/`on_stream_end` |
//! | Finalized | the tombstone finalize window |
//! | `Rewind` | the `rewind_seq` bump in `on_log`'s reorg classification |
//!
//! Watchdog `Recover`/`LogSilence`, backfill, and the verify decisions are
//! machine outputs below; the runtime stays a thin driver (ADR-028): feed
//! events + clock ticks, execute the returned decisions.
//!
//! This module holds NO provider, NO timers, NO `Instant`, and NO locks —
//! all time enters as a `now_ms` argument and all I/O is returned for the
//! driver to run. Invariants I1-I7 (design doc `block-epoch-pipeline.md`)
//! bind every transition: the epoch is the only coordinate (I1), `seq`
//! bumps exactly once per rewind (I2), stale contexts fail fast (I3),
//! writers are Streaming-confined (I4), one publish per quiesce cycle (I5),
//! one rewind in flight (I6), cutoff monotone (I7).

/// The state of a single tracked block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockState {
    /// Block header seen, no logs yet.
    Observed,
    /// At least one log received for this block; more may come.
    /// `in_flight` = logs received but not yet fully applied to pool state
    /// (decremented by the pump as logs dispatch); the `LogsQuiesced`
    /// predicate is true when it first reaches 0.
    LogsArriving { in_flight: u32, ever_quiesced: bool },
    /// Tombstone — N proven closed by the first `removed: false` log for N+1.
    LogsApplied,
    /// Verify sealed. The only state that satisfies `cursor()`.
    Drained,
    /// A `removed: true` log arrived after the block was tombstoned → reorg.
    Tainted,
}

/// Decision returned by [`StageMachine::observe_header`] telling the pump what
/// to do with a `newHeads` event. A header alone NEVER advances the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderDecision {
    /// First observation of block N — open `Observed(N)`. The pump records
    /// metadata for this block from the header.
    OpenNew(u64),
    /// Duplicate/stale header (≤ the latest observed block) — ignore.
    Stale,
    /// A header for N+1 arrived while N is still pre-tombstone. This is a
    /// **liveness-probe signal**, NOT a cursor advance (ADR-008 D1). The pump
    /// may start a dead-logs-sub timer; the cursor holds until a real log
    /// tombstones N.
    PendingSuccessor { sealed: u64, pending: u64 },
}

/// Decision returned by [`StageMachine::observe_log`] telling the pump how to
/// route a WS log event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogDecision {
    /// A normal forward log for the current/arriving block — dispatch it.
    DispatchForward,
    /// The first `removed: false` log for block N+1 — tombstone block N.
    /// The pump should finalize N (it is now `LogsApplied`).
    TombstonePrevious(u64),
    /// A `removed: true` log on a tombstoned block — enter the reorg path.
    /// The pump restores the target pool via `ReorgCoordinator`.
    EnterReorg(u64),
    /// A `removed: true` log while already in the reorg path — keep
    /// restoring; window closes on the first `removed: false`.
    ContinueReorg,
    /// The first `removed: false` event after entering the reorg path —
    /// the window closed. Its block is the new head; the pump resumes
    /// forward tracking monotonically from there.
    CloseReorg { new_head: u64 },
    /// A `removed: false` forward log for a block that is ALREADY
    /// tombstoned (delivery jitter carried it past its quiesce/tombstone
    /// edge) while NOT in the reorg path — the benign LATE-ADMIT class
    /// (HJ5HWF no-landmine ruling). The pump drops it UN-applied, counts it
    /// (`degenbot.late_log.admitted` + the deduped `late_log` failure-policy
    /// bucket), and KEEPS RUNNING: post-tombstone lateness is counted
    /// delivery noise, never a structural fault. The FSM mutates nothing
    /// here (I4: writers stay Streaming-confined; I7: the cutoff never
    /// regresses) — the tombstone already proved the block fully delivered,
    /// and an out-of-order survivor cannot rewind or re-open it.
    LateForward(u64),
}

use std::collections::{HashMap, HashSet};

use crate::bot_core::RELEVANT_TOPICS;
use crate::bot_core::{BlockContext, BlockMetadata, Epoch};
use alloy::primitives::B256;

use super::stage_handlers::Stage;

/// One consequence the pump's decision machine can emit. The driver maps each
/// variant onto its executor (the dispatch owner, the sink, the provider, the
/// reorg coordinator, or the process itself).
#[derive(Debug)]
pub enum StageDecision {
    /// Eager solve of every dirty path (top-of-loop). The driver runs
    /// the engine's Resolved→Solved hooks and the `set_last_solved_block` bookkeeping.
    Drain { block: u64, metadata: BlockMetadata },
    /// Quiesce-gated publish (ADR-008 D2) at a settle point. The driver
    /// fetches the change-set and drives the engine's Published-edge `on_publish`.
    Publish { open: u64, metadata: BlockMetadata },
    /// Tombstone finalize of a fully-delivered block (VTWCIG metadata). The
    /// driver drives the engine's `on_finalize` stage hook.
    Finalize { block: u64, metadata: BlockMetadata },
    /// Block-clock notification for Python's head tracker (B2). The driver
    /// feeds the engine's block-clock pipe (`notify_block`).
    Notify { block: u64, metadata: BlockMetadata },
    /// Mark a block solved on the engine. The driver runs
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
    /// window. The driver runs `handle_timeout_eager` (an
    /// authoritative `eth_getLogs` catch-up).
    Recover,
    /// A watchdog tick concluded the logs subscription is silent (headers
    /// fresh but no log arrived in the window). The driver emits one diagnostic
    /// warning per silence episode, then re-arms on the next log.
    LogSilence,
}

impl StageDecision {
    /// True for variants that end the loop (the driver returns).
    #[must_use]
    pub fn stops(&self) -> bool {
        matches!(self, StageDecision::Stop)
    }
}

/// The verdict of the WS-delivery completeness rule at a tombstone
/// (DFQYM5 / WS-DROP). The FSM owns the whole accountability policy: whether
/// the live websocket is even answerable for the tombstoned block. The
/// driver only executes the `Verify` arm (fetch `eth_getLogs`, abort on any
/// on-chain relevant log the websocket missed) and ignores `BackfillOwned`.
#[derive(Debug)]
pub enum CompletenessDecision {
    /// The single-writer rule owns this block: it lies at or below the
    /// recovery anchor (`record_backfill`), so an authoritative catch-up —
    /// not the live websocket — delivered its logs. WS delivery is not
    /// accountable; any tracked set is stale residue and is consumed to
    /// bound the tracking map. Verifying here would false-abort on every
    /// post-catch-up tombstone (an empty delivered set is EXPECTED, not a
    /// drop).
    BackfillOwned,
    /// A live-WS-owned complete block: the driver cross-checks the tracked
    /// delivered set against `eth_getLogs` and aborts on a real drop.
    Verify {
        block: u64,
        delivered_log_indices: HashSet<u64>,
    },
}

/// The pure, stateful decision machine for the block pump. Owns every rule
/// about which effect happens when; all time is injected (`now_ms`), all I/O
/// is returned as [`StageDecision`] / handled by the driver.
///
/// The four bools are the folded machines' irreducible flag sub-state
/// (first-header, publish arm, silence alarm, reorg window); encoding
/// them into a bitmask would only obscure the stage-table reading.
#[expect(clippy::struct_excessive_bools)]
pub struct StageMachine {
    /// The last block the pump's cursor has reached.
    current_block: u64,
    /// Metadata of `current_block` (held from its header).
    current_metadata: BlockMetadata,
    /// Whether we're before the first header after a resume/backfill.
    first_header: bool,
    /// Whether a quiesce-gated publish is armed (a forward log applied).
    publish_pending: bool,
    /// the highest block an authoritative catch-up has owned, with
    /// the rewind generation the ownership was established in (T6IYKY: the
    /// recovery anchor is an `Epoch`, not a bare block).
    recovery_anchor: Epoch,
    /// The rewind generation — bumped once per reorg episode (the stage
    /// machine's future `Rewind` event, T6IYKY). Epochs minted before the
    /// bump are stale and fail fast via `Epoch::ensure_current`.
    rewind_seq: u64,
    /// Per-block metadata snapshots (deferred tombstone finalize, VTWCIG).
    block_metadata: HashMap<u64, BlockMetadata>,
    /// WS-delivery completeness tracker (relevant log indices per block).
    ws_delivered: HashMap<u64, HashSet<u64>>,
    /// [DIAG] + watchdog time anchors, injected as wall-clock ms by the driver.
    last_header_at_ms: u64,
    last_log_at_ms: u64,
    /// Logs-silence watchdog re-arm.
    log_silence_alarm_armed: bool,
    // --- folded BlockClock sub-state (ADR-008; the six-machine fold) ---
    /// Per-block state. Append-only: a block's entry is never removed (so
    /// reorgs can rewind to `Tainted`/`Observed` without losing history).
    blocks: std::collections::HashMap<u64, BlockState>,
    /// The deepest `Drained` block — the ONLY value `cursor()` may return.
    cursor: Option<u64>,
    /// The latest HEADER observed — for stale-header detection only. A header
    /// does NOT drive the tombstone (ADR-008 D1), so this is NOT the open
    /// block; it only suppresses duplicate/stale headers.
    latest_header: Option<u64>,
    /// The LOG-driven "open block" — the block whose forward logs are
    /// currently arriving. A forward log for a block > `open_block`
    /// tombstones `open_block` (D1). Headers do not touch this.
    open_block: Option<u64>,
    /// Whether the machine is in the reorg path (a contiguous removed chunk).
    in_reorg: bool,
    /// The open epoch's stage-table row (module header's stage table):
    /// `None` = Streaming; advances as the row's decisions fire; resets on a
    /// rewind (the fresh epoch restarts the cycle).
    stage_in_cycle: Option<Stage>,
    /// the adaptive trailing-quiesce estimator (pure; timings
    /// arrive as data on `observe_settle_gap`). Parameters arrive from the
    /// driver via `set_quiesce_params`; defaults mirror the historical
    /// fixed-debounce posture.
    quiesce: QuiesceEstimator,
}

impl StageMachine {
    #[must_use]
    pub fn new(current_block: u64, now_ms: u64) -> Self {
        Self {
            current_block,
            current_metadata: BlockMetadata::default(),
            first_header: true,
            publish_pending: false,
            recovery_anchor: Epoch::at(0),
            rewind_seq: 0,
            block_metadata: HashMap::new(),
            ws_delivered: HashMap::new(),
            last_header_at_ms: now_ms,
            last_log_at_ms: now_ms,
            log_silence_alarm_armed: false,
            blocks: HashMap::new(),
            cursor: None,
            latest_header: None,
            open_block: None,
            in_reorg: false,
            stage_in_cycle: None,
            quiesce: QuiesceEstimator::new(QuiesceParams::default()),
        }
    }

    /// The last block the pump's cursor has reached (read accessor).
    #[must_use]
    pub fn current_block(&self) -> u64 {
        self.current_block
    }

    /// Metadata of the current block (read accessor).
    #[must_use]
    pub fn current_metadata(&self) -> BlockMetadata {
        self.current_metadata
    }

    /// Whether a quiesce-gated publish is armed (read accessor).
    #[must_use]
    pub fn publish_pending(&self) -> bool {
        self.publish_pending
    }

    /// The FSM's CURRENT epoch: the cursor block in the current rewind
    /// generation. Contexts minted earlier — before the last
    /// reorg episode bumped the generation — fail `ensure_current`
    /// against this epoch instead of silently applying.
    #[must_use]
    pub fn current_epoch(&self) -> Epoch {
        Epoch::with_generation(self.current_block, self.rewind_seq)
    }

    /// Mint a `BlockContext` for work about `block` carrying `metadata`:
    /// THE single-answer coordinate (the FSM's rewind generation stamped
    /// onto the work's block). The driver calls this at every decision
    /// point that hands work downstream, so every stage work item and
    /// every verifier anchor carries the same epoch type.
    #[must_use]
    pub fn context_for(&self, block: u64, metadata: BlockMetadata) -> BlockContext {
        BlockContext::new(Epoch::with_generation(block, self.rewind_seq), metadata)
    }

    /// A reorg episode opened (the FSM's `Rewind`): the rewind generation
    /// bumps, so every context minted before this point is stale and must
    /// fail `ensure_current` rather than silently applying to the rewound
    /// chain view. No separate driver call — the bump lives
    /// inside `on_log`'s reorg classification.
    fn rewind(&mut self, reorg_block: u64) {
        self.rewind_seq += 1;
        // tighten the retained per-block
        // keys to the epoch-voided window — the replaced chain segment (the
        // fork block and everything above it) keeps no pre-rewind
        // bookkeeping. Fresh WS logs + headers re-populate both maps in the
        // new generation; nothing outside the segment is touched (I2: the
        // seq bump invalidates every pre-rewind context anyway).
        self.ws_delivered.retain(|&block, _| block < reorg_block);
        self.block_metadata.retain(|&block, _| block < reorg_block);
    }

    /// The snapshotted metadata for `block` (deferred tombstone finalize, VTWCIG).
    #[must_use]
    pub fn block_metadata_for(&self, block: u64) -> Option<BlockMetadata> {
        self.block_metadata.get(&block).copied()
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
    pub fn drain_decision(&self, state_head: u64) -> StageDecision {
        StageDecision::Drain {
            block: self.solve_anchor(state_head),
            metadata: self.current_metadata,
        }
    }

    /// The solver-release solve anchor (ADR-008 D2): the LOG-DRIVEN settled
    /// block (`clock.latest_observed()`), falling back to `current_block` when
    /// no block logs are open, resolved through the shared solve-anchor rule
    /// (`super::solve_anchor` — the head floor + future-hop rule; the rule's
    /// failure history lives in that module).
    fn solve_anchor(&self, state_head: u64) -> u64 {
        super::solve_anchor::SolveAnchor::for_head(
            self.latest_observed().unwrap_or(self.current_block),
            state_head,
        )
        .block()
    }

    /// record that an authoritative catch-up (a header-gap backfill
    /// or a `handle_timeout_eager` recovery) has OWNED the range up to the
    /// epoch of that catch-up's work. Per the single-writer rule,
    /// the live WS no longer owns any block at/below the anchor's block, so
    /// later recovered forwards there are benign duplicates (dropped) rather
    /// than re-asserted faults. The anchor is stamped in the CURRENT rewind
    /// generation and only ever extends (monotone in the block coordinate —
    /// the anchor is an `Epoch` now).
    pub fn record_backfill(&mut self, through: impl Into<Epoch>) {
        let through = through.into();
        if through.block() > self.recovery_anchor.block() {
            self.recovery_anchor = Epoch::with_generation(through.block(), self.rewind_seq);
        }
    }

    /// The recovery anchor's block coordinate (the single-writer boundary
    /// `should_drop_recovered_forward` / `completeness_decision` apply).
    #[must_use]
    pub fn recovery_anchor_block(&self) -> u64 {
        self.recovery_anchor.block()
    }

    /// Record that a relevant live `WsEvent::Log` was delivered for `block`
    /// (the WS-completeness tracker cross-checked at the tombstone).
    pub fn record_ws_delivered(&mut self, block: u64, log_index: u64) {
        self.ws_delivered
            .entry(block)
            .or_default()
            .insert(log_index);
    }

    /// The per-event decision + state transition for a relevant WS log whose
    /// `removed` flag has NOT passed the single-writer / boundary drops. Owns
    /// the ADC-008 clock transition, the reorg-entry/close cursor moves, the
    /// tombstone advance, and the publish disarm on reorg — returning the
    /// `LogDecision` so the driver knows which I/O effects to run (apply the
    /// log, reach for the reorg coordinator, finalize a tombstoned block,
    /// or shut down on a late forward). Reorg/panic paths deliberately do NOT
    /// arm the publish.
    pub fn on_log(&mut self, block: u64, removed: bool) -> LogDecision {
        let decision = self.observe_log(block, removed);
        match decision {
            LogDecision::EnterReorg(reorg_block) => {
                // A reorg invalidates any publish armed from pre-reorg state —
                // AND rewinds the machine: the rewind generation bumps so every
                // context minted pre-reorg is stale. The stage row
                // resets — the fresh epoch restarts the cycle at Streaming.
                self.publish_pending = false;
                self.rewind(reorg_block);
                self.stage_in_cycle = None;
            }
            LogDecision::ContinueReorg => {
                // A reorg invalidates any publish armed from pre-reorg state.
                self.publish_pending = false;
            }
            LogDecision::CloseReorg { new_head } => {
                self.publish_pending = false;
                self.current_block = new_head;
                self.stage_in_cycle = None;
            }
            LogDecision::TombstonePrevious(prev) => {
                self.publish_pending = false;
                self.advance_to_drained(prev);
                self.stage_in_cycle = Some(Stage::Finalize); // tombstone finalize window (Finalized row)
                if block > self.current_block {
                    self.current_block = block;
                }
            }
            LogDecision::DispatchForward => {
                if block > self.current_block {
                    self.current_block = block;
                }
            }
            LogDecision::LateForward(_) => {
                // Benign late admission: no FSM transition — the
                // block is tombstoned and stays so; publication state,
                // publish arm, and cutoff are all left untouched (I4/I5/I7).
                // The pump owns the drop + count.
            }
        }
        decision
    }

    /// Mark a forward/tombstone log as applied to engine state (call AFTER the
    /// driver ran `dispatch_log`). Arms the quiesce-gated publish and records
    /// the clock's received/applied edges (ADR-008).
    pub fn on_log_applied(&mut self, block: u64) {
        self.log_received(block);
        self.log_applied(block);
        self.publish_pending = true;
        // The applied log quiesces the open block's window: the Quiesced
        // (streaming-complete) row is now the epoch's stage (bookkeeping —
        // the publish gate remains `consume_quiesced`'s to consume).
        self.stage_in_cycle = Some(Stage::StreamingComplete);
    }

    /// Mark an authoritative backfill-range complete: advance the cursor and
    /// extend the single-writer recovery anchor.
    pub fn on_backfill_range_done(&mut self, through: u64) {
        self.current_block = self.current_block.max(through);
        self.record_backfill(through);
    }

    /// The per-event decision for a `WsEvent::BlockHeader`. Returns the effects
    /// the driver must execute (notify, backfill, mark-solved).
    pub fn on_header(
        &mut self,
        number: u64,
        metadata: BlockMetadata,
        now_ms: u64,
    ) -> Vec<StageDecision> {
        let mut decisions = Vec::new();
        self.last_header_at_ms = now_ms;

        // Snapshot the just-finished block's metadata BEFORE overwriting
        // `current_metadata` (VTWCIG): the batch finalizing `current_block`
        // must carry ITS metadata, not the incoming header's.
        self.current_metadata = metadata;
        if matches!(self.observe_header(number), HeaderDecision::Stale) {
            return decisions; // duplicate/stale header — no effect.
        }
        self.block_metadata.insert(number, self.current_metadata);

        if self.first_header {
            self.first_header = false;
            if number > self.current_block {
                if number > self.current_block + 1 {
                    decisions.push(StageDecision::Backfill {
                        from: self.current_block + 1,
                        to: Some(number - 1),
                    });
                    self.record_backfill(number - 1);
                }
                self.current_block = number;
                decisions.push(StageDecision::SetLastSolved { block: number });
                decisions.push(StageDecision::Notify {
                    block: number,
                    metadata: self.current_metadata,
                });
            }
        } else if number > self.current_block {
            if number > self.current_block + 1 {
                decisions.push(StageDecision::Backfill {
                    from: self.current_block + 1,
                    to: Some(number - 1),
                });
                // this authoritative header-gap catch-up OWNS
                // `[old+1, number-1]`; a recovering WS flushing those blocks
                // must be discarded (single-writer), not re-asserted.
                self.record_backfill(number - 1);
            }
            self.current_block = number;
            decisions.push(StageDecision::Notify {
                block: number,
                metadata: self.current_metadata,
            });
        }
        decisions
    }

    /// BQ7ZBC / DFQYM5 single-writer recovery discard. A stalled WS that
    /// recovers flushes buffered forward logs for blocks ≤ `recovery_anchor` —
    /// duplicates of state the authoritative catch-up already applied. These
    /// are DROPPED (never reaching `observe_log`'s `LateForward` class).
    /// Reorg logs (`removed: true`) are NEVER dropped — they must reach the
    /// reorg classifier to unwind the backfilled range. A forward ABOVE
    /// `recovery_anchor` that is still stale takes the same benign
    /// `LateForward` drop as any other post-tombstone survivor (HJ5HWF:
    /// lateness is never a fatal signal — only the pump's own single-writer
    /// range is a silent duplicate).
    #[must_use]
    pub fn should_drop_recovered_forward(&self, log_block: u64, removed: bool) -> bool {
        let anchor_block = self.recovery_anchor.block();
        !removed && anchor_block > 0 && log_block <= anchor_block
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
    ) -> Vec<StageDecision> {
        let mut decisions = Vec::new();
        if now_ms.saturating_sub(self.last_header_at_ms) >= header_staleness_ms {
            decisions.push(StageDecision::Recover);
        } else if now_ms.saturating_sub(self.last_log_at_ms) >= log_silence_ms
            && !self.log_silence_alarm_armed
        {
            self.log_silence_alarm_armed = true;
            decisions.push(StageDecision::LogSilence);
        }
        decisions
    }

    /// The WS-completeness verdict (DFQYM5 / WS-DROP) at a block's tombstone.
    /// The FSM owns the full accountability policy, deriving BOTH arms from
    /// the same single-writer rule that governs recovered-forward dedup:
    ///
    /// - Blocks at/below `recovery_anchor` were owned by an authoritative
    ///   `eth_getLogs` catch-up; the live websocket is NOT their delivery
    ///   authority, so the cross-check is vacuous → [`CompletenessDecision::BackfillOwned`].
    /// - Any other just-confirmed-complete block is verified against the
    ///   tracked delivered set → [`CompletenessDecision::Verify`].
    #[must_use]
    pub fn completeness_decision(&mut self, prev: u64) -> CompletenessDecision {
        // Consume any tracked set in both arms: the map must stay bounded
        // regardless of the verdict.
        let delivered = self.ws_delivered.remove(&prev).unwrap_or_default();
        let anchor_block = self.recovery_anchor.block();
        if anchor_block > 0 && prev <= anchor_block {
            CompletenessDecision::BackfillOwned
        } else {
            CompletenessDecision::Verify {
                block: prev,
                delivered_log_indices: delivered,
            }
        }
    }

    /// The settle-point decision (no new event in the window): the quiesce-
    /// gated publish (ADR-008 D2) when armed, else the inactivity backfill.
    pub fn on_settle(&mut self) -> Vec<StageDecision> {
        let mut decisions = Vec::new();
        if self.publish_pending {
            if let Some(open) = self.latest_observed() {
                if self.consume_quiesced(open) {
                    decisions.push(StageDecision::Publish {
                        open,
                        metadata: self.current_metadata,
                    });
                    // The Published row: the quiesce cycle's one publish (I5).
                    self.stage_in_cycle = Some(Stage::Publish);
                }
            }
            self.publish_pending = false;
        } else {
            // 60s inactivity — the driver resolves the range and backfills.
            decisions.push(StageDecision::Backfill {
                from: self.current_block + 1,
                to: None,
            });
        }
        decisions
    }

    /// The stream-exhaustion (final settle) decision: flush any pending
    /// quiesce-gated publish, then stop.
    pub fn on_stream_end(&mut self) -> Vec<StageDecision> {
        let mut decisions = Vec::new();
        if self.publish_pending {
            if let Some(open) = self.latest_observed() {
                if self.consume_quiesced(open) {
                    decisions.push(StageDecision::Publish {
                        open,
                        metadata: self.current_metadata,
                    });
                }
            }
            self.publish_pending = false;
        }
        decisions.push(StageDecision::Stop);
        decisions
    }
}

#[cfg(test)]
mod block_clock_contract {
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
        let mut fsm = StageMachine::new(100, 0);
        // First header after resume > current: mark solved + notify, no drain.
        let d = fsm.on_header(101, meta(101_000), 1_000);
        assert!(matches!(d[0], StageDecision::SetLastSolved { block: 101 }));
        assert!(matches!(d[1], StageDecision::Notify { block: 101, .. }));
        assert_eq!(fsm.current_block, 101);

        // Sequential header: notify only (never marks solved again).
        let d = fsm.on_header(102, meta(102_000), 2_000);
        assert_eq!(d.len(), 1);
        assert!(matches!(&d[0], StageDecision::Notify { block: 102, .. }));
    }

    #[test]
    fn header_gap_backfills_and_advances_recovery_anchor() {
        let mut fsm = StageMachine::new(100, 0);
        // Non-first-gap: first header to close a gap.
        let d = fsm.on_header(105, meta(105_000), 1_000);
        assert!(d.iter().any(|x| matches!(
            x,
            StageDecision::Backfill {
                from: 101,
                to: Some(104)
            }
        )));
        assert_eq!(fsm.recovery_anchor, 104);
    }

    #[test]
    fn stale_header_produces_no_effect() {
        let mut fsm = StageMachine::new(100, 0);
        let _ = fsm.on_header(101, meta(101_000), 1_000);
        let before = fsm.current_block;
        // A duplicate/stale header (< current) must not advance or fire.
        let d = fsm.on_header(100, meta(100_000), 2_000);
        assert!(d.is_empty());
        assert_eq!(fsm.current_block, before);
    }

    #[test]
    fn non_first_gap_header_extends_recovery_anchor() {
        // The non-first-header gap branch must also own `[old+1, number-1]` for
        // the single-writer recovery anchor — the old inline driver
        // copy did; `on_header` is now the single authority and must too.
        let mut fsm = StageMachine::new(100, 0);
        let _ = fsm.on_header(101, meta(101_000), 1_000); // contiguous first header
        assert_eq!(fsm.recovery_anchor, 0);
        let d = fsm.on_header(105, meta(105_000), 2_000); // non-first gap
        assert!(d.iter().any(|x| matches!(
            x,
            StageDecision::Backfill {
                from: 102,
                to: Some(104)
            }
        )));
        assert_eq!(fsm.recovery_anchor, 104);
        assert_eq!(fsm.current_block(), 105);
    }

    #[test]
    fn on_log_forward_advances_cursor_and_applied_arms_publish() {
        let mut fsm = StageMachine::new(100, 0);
        // A forward log on the next block advances the cursor (forward edge).
        let d = fsm.on_log(101, false);
        assert!(matches!(d, LogDecision::DispatchForward));
        assert_eq!(fsm.current_block(), 101);
        assert!(!fsm.publish_pending());
        // The driver applies the log, then arms the quiesce-gated publish.
        fsm.on_log_applied(101);
        assert!(fsm.publish_pending());
    }

    #[test]
    fn on_log_tombstone_advances_cursor_and_publish_rearms() {
        let mut fsm = StageMachine::new(100, 0);
        // Open block 100 with an applied log.
        fsm.on_log(100, false);
        fsm.on_log_applied(100);
        // First log of 101 tombstones 100, advances the cursor to 101.
        let d = fsm.on_log(101, false);
        assert!(matches!(d, LogDecision::TombstonePrevious(100)));
        assert_eq!(fsm.current_block(), 101);
        fsm.on_log_applied(101);
        assert!(fsm.publish_pending());
    }

    #[test]
    fn on_log_reorg_disarms_publish() {
        let mut fsm = StageMachine::new(100, 0);
        // Arm a publish from a forward apply.
        fsm.on_log(101, false);
        fsm.on_log_applied(101);
        assert!(fsm.publish_pending());
        // A removed log reorgs the open block and disarms the stale publish.
        let d = fsm.on_log(101, true);
        assert!(matches!(d, LogDecision::EnterReorg(_)));
        assert!(!fsm.publish_pending());
    }

    #[test]
    fn completeness_decision_verifies_tracked_block_and_clears_it() {
        let mut fsm = StageMachine::new(200, 0);
        // Track delivered relevant log indices for block 201.
        fsm.ws_delivered.entry(201).or_default().extend([7, 8, 9]);

        // Tombstone → the FSM passes the delivered set to the driver and
        // clears the tracking map (one-shot: a re-verify yields empty).
        let CompletenessDecision::Verify {
            block,
            delivered_log_indices,
        } = fsm.completeness_decision(201)
        else {
            unreachable!("completeness_decision must emit Verify");
        };
        assert_eq!(block, 201);
        assert_eq!(delivered_log_indices, HashSet::from([7, 8, 9]));
        assert!(!fsm.ws_delivered.contains_key(&201), "tracked set consumed");

        // A block with no tracked relevant logs yields an empty set (the
        // authoritative side is empty too, so the cross-check is a no-op).
        let CompletenessDecision::Verify {
            delivered_log_indices,
            ..
        } = fsm.completeness_decision(202)
        else {
            unreachable!()
        };
        assert!(delivered_log_indices.is_empty());
    }

    #[test]
    fn completeness_backfill_owned_block_is_not_ws_accountable() {
        // Single-writer rule: a block <= recovery_anchor was
        // owned by an authoritative eth_getLogs catch-up, so the live WS is
        // NOT its delivery authority — the completeness cross-check is
        // vacuous there (an empty delivered set is expected, not a drop).
        // Regression guard for the false abort observed live: the inactivity
        // watchdog backfilled [25821576..25821578], then the first WS log of
        // 25821579 tombstoned 25821578 and the check compared 2 on-chain
        // logs against an empty delivered set -> spurious process abort.
        let mut fsm = StageMachine::new(200, 0);
        fsm.record_backfill(205);

        // An owned block must yield BackfillOwned even if a racing WS log
        // left a stale tracked set behind (cleared to bound the map).
        fsm.ws_delivered.entry(204).or_default().insert(3);
        assert!(matches!(
            fsm.completeness_decision(204),
            CompletenessDecision::BackfillOwned
        ));
        assert!(
            !fsm.ws_delivered.contains_key(&204),
            "stale tracked set consumed on the skip path too"
        );

        // A live-WS-owned block above the anchor still verifies.
        fsm.ws_delivered.entry(206).or_default().extend([7, 8]);
        match fsm.completeness_decision(206) {
            CompletenessDecision::Verify {
                block,
                delivered_log_indices,
            } => {
                assert_eq!(block, 206);
                assert_eq!(delivered_log_indices, HashSet::from([7, 8]));
            }
            CompletenessDecision::BackfillOwned => {
                unreachable!("block above the anchor must verify")
            }
        }
    }

    #[test]
    fn reorg_routing_owned_by_clock_classification() {
        // Reorg routing is FSM state: the clock (part of the FSM) classifies
        // removed logs into an unwind window (enter → continue → close). The
        // driver only executes the classified routing against the coordinator.
        let mut fsm = StageMachine::new(200, 0);
        // Enter: a removed:true log opens the reorg window at its block.
        assert!(matches!(
            fsm.observe_log(201, true),
            LogDecision::EnterReorg(_)
        ));
        // Continue: a further removed log in the same window.
        assert!(matches!(
            fsm.observe_log(201, true),
            LogDecision::ContinueReorg
        ));
        // Close: the window ends and forward tracking resumes from a new head.
        assert!(matches!(
            fsm.observe_log(202, false),
            LogDecision::CloseReorg { .. }
        ));
    }

    #[test]
    fn watchdog_tick_fires_on_stale_not_fresh_and_arms_code_episode() {
        let mut fsm = StageMachine::new(200, 1_000);
        // Fresh (just recorded a header): a tick must not fire.
        fsm.record_header(1_000);
        fsm.record_log(1_000);
        assert!(fsm.on_tick(1_050, 500, 300).is_empty());

        // Headers stale: Recover fires (header clock quiet long enough).
        let d = fsm.on_tick(1_600, 500, 300);
        assert!(d.iter().any(|x| matches!(x, StageDecision::Recover)));

        // Fresh headers (new header), but logs silent: one LogSilence per
        // episode — a second tick without a log doesn't re-emit.
        fsm.record_header(1_600); // headers fresh again at 1600
        let d = fsm.on_tick(2_000, 500, 300);
        assert!(d.iter().any(|x| matches!(x, StageDecision::LogSilence)));
        assert!(d.iter().all(|x| !matches!(x, StageDecision::Recover)));
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
        assert!(d.iter().any(|x| matches!(x, StageDecision::LogSilence)));
    }

    #[test]
    fn single_writer_recovery_anchor_drops_owned_range_only() {
        // Record an authoritative catch-up owning [.., 205]. Per the
        // single-writer rule the WS no longer owns those blocks.
        let mut fsm = StageMachine::new(200, 0);
        fsm.record_backfill(205);
        assert_eq!(fsm.recovery_anchor, 205);

        // A recovered forward INSIDE the owned range is a benign duplicate:…
        // dropped, not re-asserted (no BQ7ZBC recover-forward re-assert; the
        // recover flush never reaches the `LateForward` drop either).
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
        let mut fsm = StageMachine::new(200, 0);
        let d = fsm.on_settle();
        assert!(matches!(
            d[0],
            StageDecision::Backfill {
                from: 201,
                to: None
            }
        ));
        // A forward log arms a pending publish + makes the block observable: a
        // settle emits the quiesce-gated Publish and clears the arm.
        let mut fsm = StageMachine::new(200, 0);
        fsm.observe_log(201, false);
        fsm.log_received(201);
        fsm.log_applied(201);
        fsm.publish_pending = true;
        let d = fsm.on_settle();
        assert!(d
            .iter()
            .any(|x| matches!(x, StageDecision::Publish { open: 201, .. })));
        assert!(!fsm.publish_pending);
    }

    #[test]
    fn quiesce_gate_emits_exactly_one_publish_never_premature() {
        // Premature: a log is armed but still in-flight (received, not yet
        // applied) — the block is NOT quiesced, so a settle must NOT publish.
        // The arm is still cleared (the burst is still draining).
        let mut fsm = StageMachine::new(200, 0);
        fsm.observe_log(201, false);
        fsm.log_received(201); // in_flight = 1, never quiesced
        fsm.publish_pending = true;
        let d = fsm.on_settle();
        assert!(
            d.iter()
                .all(|x| !matches!(x, StageDecision::Publish { .. })),
            "premature publish before the block quiesces: {d:?}"
        );
        assert!(
            !fsm.publish_pending,
            "arm cleared even when not yet quiesced"
        );

        // Quiesced: the applied log flips `ever_quiesced`. A settle emits
        // EXACTLY one Publish for the block, never a second.
        let mut fsm = StageMachine::new(200, 0);
        fsm.observe_log(201, false);
        fsm.log_received(201);
        fsm.log_applied(201); // in_flight = 0 → quiesced
        fsm.publish_pending = true;
        let d = fsm.on_settle();
        let publishes = d
            .iter()
            .filter(|x| matches!(x, StageDecision::Publish { .. }))
            .count();
        assert_eq!(publishes, 1, "exactly one publish for the quiesced block");
        assert!(d
            .iter()
            .any(|x| matches!(x, StageDecision::Publish { open: 201, .. })));
        // A second settle with nothing new armed must not re-publish: the
        // quiesce signal was consumed by the first settle.
        let d2 = fsm.on_settle();
        assert!(
            d2.iter()
                .all(|x| !matches!(x, StageDecision::Publish { .. })),
            "no duplicate publish — the quiesce signal was consumed: {d2:?}"
        );
    }

    #[test]
    fn stream_end_flushes_then_stops() {
        let mut fsm = StageMachine::new(300, 0);
        fsm.publish_pending = true;
        let d = fsm.on_stream_end();
        assert!(d.last().is_some_and(StageDecision::stops));
    }
}

#[test]
fn drain_decision_anchor_follows_log_driven_block_not_racing_header() {
    // Header races ahead to 102 while only block 101's logs are open:
    // anchor at 101 (open), NOT 102 (current_block).
    let mut fsm = StageMachine::new(102, 0);
    fsm.observe_log(101, false);
    fsm.log_received(101);
    fsm.log_applied(101);
    let StageDecision::Drain { block, .. } = fsm.drain_decision(100) else {
        unreachable!("drain_decision must emit a Drain");
    };
    assert_eq!(
        block, 101,
        "anchor at the open (log-driven) block, not the racing header"
    );
    // State head dominates on a backfill-ahead stall.
    let StageDecision::Drain { block, .. } = fsm.drain_decision(500) else {
        unreachable!()
    };
    assert_eq!(block, 500);
    // No open block yet (cold start, headers only): fall back to the header.
    let fsm2 = StageMachine::new(102, 0);
    let StageDecision::Drain { block, .. } = fsm2.drain_decision(100) else {
        unreachable!()
    };
    assert_eq!(block, 102);
}
impl StageMachine {
    /// The deepest `Drained` block, or `None` if no block has reached
    /// `Drained` yet. A header alone can never advance this (ADR-008 D1).
    #[must_use]
    pub fn cursor(&self) -> Option<u64> {
        self.cursor
    }

    /// Observe a `newHeads` event. A header alone NEVER advances the cursor or
    /// opens a block for log-arrival — it only records the block's existence +
    /// metadata and feeds the stale-header + liveness-probe decisions. The
    /// tombstone is driven by LOGS (D1).
    pub fn observe_header(&mut self, block: u64) -> HeaderDecision {
        match self.latest_header {
            None => {
                self.blocks.entry(block).or_insert(BlockState::Observed);
                self.latest_header = Some(block);
                HeaderDecision::OpenNew(block)
            }
            Some(latest) if block <= latest => HeaderDecision::Stale,
            Some(_) => {
                // A header for a new block. The still-open block (logs in
                // flight, not yet tombstoned) is `open_block`, or — if no log
                // has arrived yet — the latest header block. Signal the
                // liveness probe; do NOT advance the cursor / open_block.
                let sealed = self.open_block.or(self.latest_header).unwrap_or(0);
                self.blocks.entry(block).or_insert(BlockState::Observed);
                self.latest_header = Some(block);
                HeaderDecision::PendingSuccessor {
                    sealed,
                    pending: block,
                }
            }
        }
    }

    /// Observe a log event for `block` with the given `removed` flag.
    pub fn observe_log(&mut self, block: u64, removed: bool) -> LogDecision {
        if self.in_reorg {
            return self.handle_reorg_log(block, removed);
        }
        if removed {
            return self.handle_removed_outside_reorg(block);
        }
        self.handle_forward_log(block)
    }

    fn handle_forward_log(&mut self, block: u64) -> LogDecision {
        // The "open block" is the one whose forward logs are currently in
        // flight. Only LOGS open it — headers do not (ADR-008 D1).
        match self.open_block {
            Some(open) if block < open => {
                // A forward log for a block older than the open block → the
                // open block has moved past it: a late forward on a
                // tombstoned block. BENIGN under the HJ5HWF no-landmine
                // ruling — the pump drops + counts it (the `LateForward`
                // late-admit class); it must never mutate state or rewind
                // the cursor here.
                LogDecision::LateForward(block)
            }
            Some(open) if block == open => {
                // Another log for the open block — keep arriving.
                self.enter_logs_arriving(block);
                LogDecision::DispatchForward
            }
            _ => {
                // A NEW block (block > open, or open is None). The FIRST log
                // for it tombstones its predecessor — the latest non-
                // tombstoned tracked block strictly below it (the open block,
                // or — if no log has arrived yet — the latest header-observed
                // block, i.e. an empty block: ADR-008 covers this case).
                let pred = self.predecessor_block(block);
                if let Some(p) = pred {
                    self.tombstone(p);
                }
                self.enter_logs_arriving(block);
                self.open_block = Some(block);
                match pred {
                    Some(p) => LogDecision::TombstonePrevious(p),
                    None => LogDecision::DispatchForward,
                }
            }
        }
    }

    /// The latest non-tombstoned tracked block strictly below `block` — i.e. the
    /// block a forward log for `block` tombstones. Either the open log block
    /// (O(1)), or — if no log has arrived yet — the latest header-observed
    /// non-tombstoned block below `block` (a scan, run only on the very first
    /// log before `open_block` is set). `None` if `block` is the first block.
    fn predecessor_block(&self, block: u64) -> Option<u64> {
        if let Some(open) = self.open_block {
            return (open < block).then_some(open);
        }
        // No log has arrived yet: scan for the latest non-tombstoned tracked
        // block strictly below `block` (the empty-block predecessor case).
        self.blocks
            .iter()
            .filter(|(b, s)| {
                **b < block && matches!(s, BlockState::Observed | BlockState::LogsArriving { .. })
            })
            .map(|(b, _)| *b)
            .max()
    }

    fn handle_removed_outside_reorg(&mut self, block: u64) -> LogDecision {
        // A removed:true log while not in the reorg path. Whether the block is
        // already tombstoned (LogsApplied/Drained) or still in flight, the WS
        // is unwinding a chain segment — enter the reorg path. The window
        // closes on the first removed:false after entry (ADR-008 D3).
        self.in_reorg = true;
        LogDecision::EnterReorg(block)
    }

    fn handle_reorg_log(&mut self, block: u64, removed: bool) -> LogDecision {
        if removed {
            LogDecision::ContinueReorg
        } else {
            // First removed:false after entering reorg → window closed.
            // This block is the new head; reopen it as the log-driven open
            // block (forward tracking resumes monotonically from it).
            self.in_reorg = false;
            self.taint_range_before(block);
            self.enter_logs_arriving(block);
            self.open_block = Some(block);
            self.latest_header = Some(block);
            LogDecision::CloseReorg { new_head: block }
        }
    }

    /// Mark a block's log as received (enter `LogsArriving` if needed + bump
    /// the in-flight counter). Called by the pump when it begins dispatching a
    /// log for `block`.
    pub fn log_received(&mut self, block: u64) {
        self.enter_logs_arriving(block);
        if let Some(BlockState::LogsArriving { in_flight, .. }) = self.blocks.get_mut(&block) {
            *in_flight += 1;
        }
    }

    /// Mark a block's log as fully applied (decrement in-flight counter).
    /// Called by the pump after `dispatch_log` completes for `block`.
    /// When the in-flight counter first reaches 0, `logs_quiesced(block)`
    /// becomes true (the solver-release gate — ADR-008 D2).
    pub fn log_applied(&mut self, block: u64) {
        if let Some(BlockState::LogsArriving {
            in_flight,
            ever_quiesced,
        }) = self.blocks.get_mut(&block)
        {
            if *in_flight > 0 {
                *in_flight -= 1;
            }
            if *in_flight == 0 {
                *ever_quiesced = true;
            }
        }
    }

    /// The `LogsQuiesced` predicate (ADR-008 D2): true when every log the WS
    /// has given us for `block` has been fully applied to pool state. This
    /// requires BOTH that at least one log-received→applied cycle completed
    /// (`ever_quiesced`) AND that no log is currently being dispatched
    /// (`in_flight == 0`). A block that merely entered `LogsArriving` (a
    /// tombstone-triggering log observed but not yet dispatched) is NOT
    /// quiesced. Used as the solver-release gate; NOT a state transition.
    #[must_use]
    pub fn logs_quiesced(&self, block: u64) -> bool {
        matches!(
            self.blocks.get(&block),
            Some(
                BlockState::LogsArriving {
                    in_flight: 0,
                    ever_quiesced: true
                } | BlockState::LogsApplied
                    | BlockState::Drained
            )
        )
    }

    /// Consume the quiesce signal for `block` (the solver-release gate —
    /// ADR-008 D2). Returns `true` if the block was quiesced AND the signal
    /// hadn't been consumed since the last `log_received` (so the pump
    /// publishes exactly once per quiesce cycle). Resets `ever_quiesced`;
    /// a straggler log (`log_received` → `in_flight > 0` → `log_applied` →
    /// `in_flight == 0`) re-arms it for the next publish.
    ///
    /// On `LogsApplied`/`Drained` blocks (already tombstoned) this returns
    /// `false`: the tombstone-driven finalize is the terminal publish, not
    /// the quiesce gate.
    pub fn consume_quiesced(&mut self, block: u64) -> bool {
        if let Some(BlockState::LogsArriving {
            in_flight: 0,
            ever_quiesced,
        }) = self.blocks.get_mut(&block)
        {
            let was_quiesced = *ever_quiesced;
            *ever_quiesced = false;
            was_quiesced
        } else {
            false
        }
    }

    /// Transition `block` from `LogsArriving` to `LogsApplied` (the tombstone).
    /// The pump driver mirrors this verdict into `BotState`'s delivery cutoff
    /// — the clock holds no shared cutoff interior.
    fn tombstone(&mut self, block: u64) {
        if let Some(state) = self.blocks.get_mut(&block) {
            *state = BlockState::LogsApplied;
        }
    }

    /// The deepest tombstoned (`LogsApplied`/`Drained`) block — the test-side
    /// read of the delivery cutoff (3M5PO5, last complete block). Computed
    /// over the per-block map; `0` until the first tombstone. Production
    /// authority for the cutoff lives on `BotState` since BGEDB6; this
    /// accessor exists so the clock's own tests pin the tombstone-only
    /// advance semantics.
    #[cfg(test)]
    #[must_use]
    fn highest_applied(&self) -> u64 {
        self.blocks
            .iter()
            .filter(|(_, s)| matches!(s, BlockState::LogsApplied | BlockState::Drained))
            .map(|(b, _)| *b)
            .max()
            .unwrap_or(0)
    }

    /// Mark every tracked block strictly before `head` as `Tainted` (reorg
    /// rewind) so the replay can re-track them forward through the SM.
    fn taint_range_before(&mut self, head: u64) {
        for (&b, state) in &mut self.blocks {
            if b < head {
                *state = BlockState::Tainted;
            }
        }
    }

    /// Transition `block` to `LogsArriving` (from `Observed` or absent).
    /// Counter stays at 0 — `log_received`/`log_applied` own the in-flight
    /// accounting. Idempotent on blocks already in `LogsArriving`.
    fn enter_logs_arriving(&mut self, block: u64) {
        let entry = self.blocks.entry(block).or_insert(BlockState::Observed);
        if matches!(entry, BlockState::Observed) {
            *entry = BlockState::LogsArriving {
                in_flight: 0,
                ever_quiesced: false,
            };
        }
    }

    /// Transition `block` from `LogsApplied` to `Drained` (verify sealed).
    /// This is the ONLY mutator that lets [`cursor`](Self::cursor) return N.
    /// Does nothing if the block is not `LogsApplied`.
    pub fn advance_to_drained(&mut self, block: u64) {
        if let Some(state) = self.blocks.get_mut(&block) {
            if matches!(state, BlockState::LogsApplied) {
                *state = BlockState::Drained;
                self.cursor = match self.cursor {
                    Some(c) if c >= block => self.cursor,
                    _ => Some(block),
                };
            }
        }
    }

    /// Whether the clock is currently in the reorg path.
    #[must_use]
    pub fn in_reorg(&self) -> bool {
        self.in_reorg
    }

    /// The LOG-driven open block — the block whose forward logs are currently
    /// arriving (the next tombstone target). Headers do not change this.
    #[must_use]
    pub fn latest_observed(&self) -> Option<u64> {
        self.open_block
    }

    /// Read a block's state (for tests + diagnostics).
    #[cfg(test)]
    #[must_use]
    pub fn state_of(&self, block: u64) -> Option<BlockState> {
        self.blocks.get(&block).copied()
    }
}

// ======================================================================
// the adaptive trailing-quiesce estimator.
// ======================================================================

/// The settle-window mode declared by the typed config schema
/// (degenbot-config `pump.quiesce_mode`). Re-exported here because the FSM
/// owns the estimator the mode drives.
pub use degenbot_config::QuiesceMode;

/// Parameters of the adaptive trailing-quiesce estimator — one snapshot of
/// the `pump.quiesce_*` schema keys, copied in by the driver at pump
/// startup (the FSM owns no config access; the driver decides, see ADR-008).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuiesceParams {
    /// `fixed` = constant `fixed_ms` (today's debounce); `adaptive` = the
    /// EWMA map below.
    pub mode: QuiesceMode,
    /// How many late-admit events in a sliding hour hold the window at the
    /// ceiling.
    pub late_budget: u64,
    /// Fixed-mode window; also the adaptive fallback when the estimator
    /// cannot produce a value.
    pub fixed_ms: u64,
    /// Adaptive floor ms (clamped ≥ 1).
    pub floor_ms: u64,
    /// Adaptive ceiling ms (never exceeded, even under straggler regimes).
    pub ceil_ms: u64,
    /// Safety multiplier over the silence-gap EWMA.
    pub margin: f64,
    /// EWMA smoothing constant over per-block max silence gaps.
    pub alpha: f64,
}

impl Default for QuiesceParams {
    fn default() -> Self {
        Self {
            mode: QuiesceMode::Fixed,
            late_budget: 120,
            fixed_ms: 50,
            floor_ms: 2,
            ceil_ms: 20,
            margin: 3.0,
            alpha: 0.1,
        }
    }
}

impl QuiesceParams {
    /// The parameters a test (or a fallback driver) that mirrors the
    /// historical 50 ms debounce posture needs.
    #[must_use]
    pub fn fixed(fixed_ms: u64) -> Self {
        Self {
            fixed_ms: fixed_ms.max(1),
            ..Self::default()
        }
    }

    /// Snapshot the `pump.quiesce_*` schema keys. `fixed_ms` is
    /// the pump's live debounce — the fixed-mode window AND the adaptive
    /// fallback contract (the debounce's "never 0" parse rule carries over).
    /// The FSM stays I/O-free: it never reads the ambient config stance
    /// itself; the driver copies this snapshot in via `set_quiesce_params`.
    #[must_use]
    pub fn from_schema(config: &degenbot_config::BotConfig, fixed_ms: u64) -> Self {
        let pump = &config.pump;
        Self {
            mode: pump.quiesce_mode,
            fixed_ms: fixed_ms.max(1),
            floor_ms: pump.quiesce_floor_ms,
            ceil_ms: pump.quiesce_ceil_ms,
            margin: pump.quiesce_margin_ms,
            alpha: pump.quiesce_ewma_alpha,
            late_budget: pump.quiesce_late_budget,
        }
    }
}

/// Sliding-hour retention of the late-admit ledger (the HJ5HWF backstop
/// contract window).
pub const LATE_LEDGER_WINDOW_MS: u64 = 60 * 60 * 1000;

/// Bound on the ledger's physical length: the budget backstop needs at
/// most `late_budget + 1` recent entries; anything beyond that can never
/// change the hold decision, so the ring is capped for memory discipline.
const LATE_LEDGER_CAP: usize = 4_096;

/// f64 ms → u64 ms, saturating and sign-safe (the `as` cast below saturates
/// in modern Rust; the guards keep NaN/negatives out entirely).
#[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
#[must_use]
fn f64_ms_to_u64(v: f64) -> u64 {
    if !v.is_finite() || v <= 0.0 {
        0
    } else {
        v as u64
    }
}

/// The pure EWMA trailing-quiesce estimator (design §6.1). NO clock, NO I/O:
/// all timings arrive as data — the driver feeds each block's observed max
/// intra-block silence gap at the settle point and the late-admit events
/// with their driver-clock stamps, exactly like the watchdog's `now_ms`s.
///
/// Window rule: `W_next = clamp(EWMA × margin, floor, ceil)`, with the
/// backstop that exceeding `late_budget` late admits in a sliding hour
/// holds `W` at `ceil` until the ledger drains. The EWMA's first
/// observation seeds it fully (no halved cold-start update).
#[derive(Debug, Clone)]
struct QuiesceEstimator {
    params: QuiesceParams,
    /// EWMA of the per-block max intra-block silence gap, ms.
    ewma_ms: f64,
    /// Whether the EWMA has its first (fully-seeding) observation.
    seeded: bool,
    /// Driver-clock stamps (ms) of observed late admissions, oldest first,
    /// kept within `LATE_LEDGER_WINDOW_MS`.
    late_admits: std::collections::VecDeque<u64>,
    /// Whether the backstop currently holds the window at the ceiling.
    held_at_ceil: bool,
}

impl QuiesceEstimator {
    fn new(params: QuiesceParams) -> Self {
        Self {
            params,
            ewma_ms: 0.0,
            seeded: false,
            late_admits: std::collections::VecDeque::new(),
            held_at_ceil: false,
        }
    }

    /// Sanitized bounds: the window is never zero or crossed, the multiplier
    /// is never negative/non-finite, the smoothing constant stays in (0, 1].
    fn bounds(&self) -> (u64, u64, f64, f64) {
        // floor >= 1; ceil >= floor (a 0/garbage ceil falls back, never 0).
        let floor = self.params.floor_ms.max(1);
        let ceil = self.params.ceil_ms.max(floor);
        let margin = if self.params.margin.is_finite() {
            self.params.margin.max(0.0)
        } else {
            0.0
        };
        let alpha = if self.params.alpha.is_finite() {
            self.params.alpha.clamp(f64::EPSILON, 1.0)
        } else {
            1.0
        };
        (floor, ceil, margin, alpha)
    }

    /// The currently-armed settle window in ms.
    fn window_ms(&self) -> u64 {
        let (floor, ceil, margin, _alpha) = self.bounds();
        match self.params.mode {
            // Fixed mode ignores the estimator entirely (today's behavior).
            QuiesceMode::Fixed => self.params.fixed_ms.max(1),
            QuiesceMode::Adaptive => {
                if self.held_at_ceil || !self.seeded {
                    // Pre-seed and backstop both arm at the ceiling: an
                    // unknown regime defaults to conservative coverage and
                    // descends as observations accumulate.
                    return ceil;
                }
                f64_ms_to_u64(self.ewma_ms * margin).clamp(floor, ceil)
            }
        }
    }

    /// Feed one per-block max intra-block silence-gap observation (ms) at
    /// its settle point, and prune the late ledger against the same
    /// driver-clock `now_ms` (the hold re-evaluates at least once per
    /// settled block).
    fn observe(&mut self, max_gap_ms: u64, now_ms: u64) {
        self.prune_late_ledger(now_ms);
        if self.mode_is_fixed() {
            return;
        }
        let g = f64::from(u32::try_from(max_gap_ms).unwrap_or(u32::MAX));
        if self.seeded {
            let alpha = self.bounds().3;
            self.ewma_ms = alpha * g + (1.0 - alpha) * self.ewma_ms;
        } else {
            // Seed fully: the first settled block anchors the estimator.
            self.ewma_ms = g;
            self.seeded = true;
        }
        self.refresh_hold();
    }

    /// Record one late-admit event (benign counted delivery noise, HJ5HWF)
    /// at driver-clock stamp `now_ms`.
    fn record_late_admit(&mut self, now_ms: u64) {
        self.prune_late_ledger(now_ms);
        if self.late_admits.len() < LATE_LEDGER_CAP {
            self.late_admits.push_back(now_ms);
        }
        self.refresh_hold();
    }

    /// Whether the window is currently held at the ceiling by the backstop
    /// (telemetry).
    const fn held_at_ceil(&self) -> bool {
        self.held_at_ceil
    }

    const fn mode_is_fixed(&self) -> bool {
        matches!(self.params.mode, QuiesceMode::Fixed)
    }

    fn prune_late_ledger(&mut self, now_ms: u64) {
        while let Some(front) = self.late_admits.front() {
            if now_ms.saturating_sub(*front) > LATE_LEDGER_WINDOW_MS {
                self.late_admits.pop_front();
            } else {
                break;
            }
        }
    }

    /// Backstop: more late admits in the trailing hour than the budget →
    /// hold at the ceiling; otherwise the estimator may resume descending.
    fn refresh_hold(&mut self) {
        if !self.mode_is_fixed() {
            self.held_at_ceil =
                u64::try_from(self.late_admits.len()).unwrap_or(u64::MAX) > self.params.late_budget;
        }
    }
}

impl StageMachine {
    /// Swap in quiesce-estimator parameters (driver, at pump startup). The
    /// estimator state resets with the new parameters — operator tuning is
    /// a stance, not a mid-stream instrument nudge.
    pub fn set_quiesce_params(&mut self, params: QuiesceParams) {
        self.quiesce = QuiesceEstimator::new(params);
    }

    /// The currently-armed settle (quiesce) window in ms — the value the
    /// driver arms its settle timers with. Fixed mode returns the fixed
    /// window (the `pump_debounce_ms` contract); adaptive mode the clamped
    /// EWMA projection (design §6.1, BM35LK).
    #[must_use]
    pub fn settle_window_ms(&self) -> u64 {
        self.quiesce.window_ms()
    }

    /// Feed one settle-point observation: the settled block's max
    /// intra-block silence gap (ms) between consecutive relevant logs, and
    /// the driver clock (`now_ms`, ms) for the late-admit ledger prune.
    /// Pure — the FSM owns no timer; timings arrive as data.
    pub fn observe_settle_gap(&mut self, max_gap_ms: u64, now_ms: u64) {
        self.quiesce.observe(max_gap_ms, now_ms);
    }

    /// Record one benign late-admit event (the HJ5HWF counted class) at
    /// driver-clock `now_ms` for the sliding-hour budget backstop: over
    /// budget → the window is held at the ceiling (design §6.1).
    pub fn record_late_admit(&mut self, now_ms: u64) {
        self.quiesce.record_late_admit(now_ms);
    }

    /// Whether the late-budget backstop currently holds the window at the
    /// ceiling (telemetry for the `quiesce_window_ms` gauge).
    #[must_use]
    pub const fn quiesce_held_at_ceil(&self) -> bool {
        self.quiesce.held_at_ceil()
    }

    /// The estimator's current EWMA of per-block max silence gaps (ms; test
    /// + telemetry read accessor).
    #[must_use]
    pub fn quiesce_ewma_ms(&self) -> f64 {
        self.quiesce.ewma_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Tracer bullet (ADR-008 D1):** a header alone MUST NOT advance the
    /// cursor. `cursor()` is `None` until a block reaches `Drained`, and the
    /// only path to `Drained` is `advance_to_drained` after `LogsApplied`,
    /// which requires the tombstone (a real successor log). This is the
    /// invalid transition the current pump makes (`block_pump.rs:593-595`)
    /// — the whole reason ADR-008 exists.
    #[test]
    fn header_alone_never_advances_cursor() {
        let mut machine = StageMachine::new(0, 0);
        // Observe a block header — opens the block but does NOT drain it.
        assert_eq!(machine.observe_header(100), HeaderDecision::OpenNew(100));
        assert_eq!(
            machine.cursor(),
            None,
            "a header alone must not set the cursor"
        );

        // Even observing a later header does NOT help — still no log tombstoned 100.
        assert_eq!(
            machine.observe_header(101),
            HeaderDecision::PendingSuccessor {
                sealed: 100,
                pending: 101
            },
        );
        assert_eq!(
            machine.cursor(),
            None,
            "headers alone can never reach Drained"
        );
    }

    /// The `highest_applied` cutoff (3M5PO5) — the registration drain's
    /// single source of truth — advances ONLY on the tombstone (the first
    /// `removed: false` log of N+1), exactly as the retired buffer
    /// `last_complete_block` marker did.
    #[test]
    fn highest_applied_cutoff_advances_on_tombstone_only() {
        let mut machine = StageMachine::new(0, 0);
        assert_eq!(machine.highest_applied(), 0, "no tombstone yet");

        // Observe headers — a header alone must NOT advance the cutoff.
        machine.observe_header(100);
        assert_eq!(machine.highest_applied(), 0);
        machine.observe_header(101);
        assert_eq!(machine.highest_applied(), 0);

        // The first removed:false log for 101 tombstones 100.
        assert_eq!(
            machine.observe_log(101, false),
            LogDecision::TombstonePrevious(100)
        );
        assert_eq!(
            machine.highest_applied(),
            100,
            "a successor log tombstones the predecessor into the cutoff"
        );

        // A further tombstone (a log for 102 closes 101) advances monotonically.
        assert_eq!(
            machine.observe_log(102, false),
            LogDecision::TombstonePrevious(101)
        );
        assert_eq!(machine.highest_applied(), 101);
    }

    /// The full happy path: `Observed → LogsArriving → LogsApplied → Drained`,
    /// where the tombstone is the first `removed: false` log for N+1 (D1) and
    /// `cursor()` returns N only after `advance_to_drained`.
    #[test]
    fn full_lifecycle_tombstone_via_successor_log_then_drained() {
        let mut machine = StageMachine::new(0, 0);
        machine.observe_header(100); // Observed(100)
        assert_eq!(machine.state_of(100), Some(BlockState::Observed));

        // First log for 100 → LogsArriving.
        assert_eq!(
            machine.observe_log(100, false),
            LogDecision::DispatchForward
        );
        machine.log_received(100); // in_flight = 1
        assert_eq!(
            machine.state_of(100),
            Some(BlockState::LogsArriving {
                in_flight: 1,
                ever_quiesced: false
            })
        );
        assert!(!machine.logs_quiesced(100), "in-flight log → not quiesced");

        machine.log_applied(100); // in_flight = 0 → quiesced
        assert!(machine.logs_quiesced(100), "all applied → quiesced");

        // First forward log for 101 → tombstone 100. Cursor STILL None.
        assert_eq!(
            machine.observe_log(101, false),
            LogDecision::TombstonePrevious(100),
        );
        assert_eq!(machine.state_of(100), Some(BlockState::LogsApplied));
        assert_eq!(machine.cursor(), None, "LogsApplied is not Drained yet");

        machine.advance_to_drained(100);
        assert_eq!(
            machine.cursor(),
            Some(100),
            "Drained is the only cursor source"
        );
        assert_eq!(machine.state_of(100), Some(BlockState::Drained));
    }

    /// `advance_to_drained` on a block that is only `Observed` (no logs, no
    /// tombstone) must be a no-op — the invalid `Observed → Drained` transition
    /// has no API path (ADR-008). A header alone can never drain.
    #[test]
    fn advance_to_drained_on_non_applied_block_is_noop() {
        let mut machine = StageMachine::new(0, 0);
        machine.observe_header(100); // Observed only — no log ever tombstoned it
        machine.advance_to_drained(100); // must not transition
        assert_eq!(machine.cursor(), None);
        assert_eq!(machine.state_of(100), Some(BlockState::Observed));
    }

    /// A late `removed: false` log on a `Drained` block, outside a reorg,
    /// takes the BENIGN late-admit class (HJ5HWF no-landmine ruling):
    /// `LateForward` — never a fatal signal. The pinned invariants: the
    /// cursor never silently regresses (I7), the block's tombstoned state is
    /// untouched (I4: no re-entry into a writable state), and the publish
    /// arm is neither armed nor consumed by the late arrival (I5: exactly
    /// one publish per quiesce cycle, the late log is not a new quiesce).
    #[test]
    fn late_forward_log_on_drained_block_is_benign_late_admit() {
        let mut machine = StageMachine::new(0, 0);
        // Drive 100 to Drained the honest way.
        machine.observe_header(100);
        machine.observe_log(100, false);
        machine.log_received(100);
        machine.log_applied(100);
        machine.observe_log(101, false); // tombstone 100
        machine.log_received(101);
        machine.log_applied(101);
        machine.observe_log(102, false); // tombstone 101
        machine.advance_to_drained(100);
        machine.advance_to_drained(101);
        assert_eq!(machine.cursor(), Some(101));

        // Arm a pending publish so we can pin that the late log leaves it
        // untouched (I5).
        machine.publish_pending = true;

        // A forward log for 100 now arrives — late forward on a Drained
        // block: benign late admission, NOT a fatal signal.
        assert_eq!(
            machine.observe_log(100, false),
            LogDecision::LateForward(100),
        );
        assert_eq!(machine.cursor(), Some(101), "cursor holds — no regression");
        assert_eq!(
            machine.state_of(100),
            Some(BlockState::Drained),
            "the tombstoned block stays tombstoned — late log cannot reopen it (I4)"
        );
        assert!(
            machine.publish_pending,
            "the late log must not arm or disarm the pending publish (I5)"
        );
    }

    /// A `removed: true` log on a tombstoned block enters the reorg path; the
    /// first `removed: false` after entry closes the window and that block
    /// becomes the new head. Earlier tracked blocks are tainted for replay
    /// (ADR-008 D3).
    #[test]
    fn reorg_path_closes_on_first_forward_after_removed_chunk() {
        let mut machine = StageMachine::new(0, 0);
        // Drive 100 + 101 to Drained the honest way. 102 tombstones 101.
        machine.observe_header(100);
        machine.observe_log(100, false);
        machine.log_received(100);
        machine.log_applied(100);
        machine.observe_log(101, false); // tombstone 100
        machine.log_received(101);
        machine.log_applied(101);
        machine.observe_log(102, false); // tombstone 101
        machine.advance_to_drained(100);
        machine.advance_to_drained(101);
        assert_eq!(machine.state_of(101), Some(BlockState::Drained));
        assert_eq!(machine.cursor(), Some(101));

        // Reorg: removed:true logs arrive (any order) — enter + continue.
        assert_eq!(machine.observe_log(101, true), LogDecision::EnterReorg(101),);
        assert!(machine.in_reorg());
        assert_eq!(machine.observe_log(100, true), LogDecision::ContinueReorg);
        assert_eq!(machine.observe_log(99, true), LogDecision::ContinueReorg);

        // First removed:false → window closes, block 103 is the new head.
        assert_eq!(
            machine.observe_log(103, false),
            LogDecision::CloseReorg { new_head: 103 },
        );
        assert!(!machine.in_reorg());
        // Earlier tracked blocks tainted for replay; 103 now LogsArriving.
        assert_eq!(machine.state_of(101), Some(BlockState::Tainted));
        assert_eq!(machine.state_of(100), Some(BlockState::Tainted));
        assert_eq!(
            machine.cursor(),
            Some(101),
            "drained common ancestor survives"
        );
        assert_eq!(machine.latest_observed(), Some(103));
    }

    /// `logs_quiesced` re-enters (becomes false) on a straggler log landing
    /// after the first quiesce (ADR-008 D2). The solver-release gate must
    /// re-close until the straggler is applied.
    #[test]
    fn logs_quiesced_re_opens_on_straggler() {
        let mut machine = StageMachine::new(0, 0);
        machine.observe_header(100);
        machine.observe_log(100, false);
        machine.log_received(100);
        machine.log_applied(100);
        assert!(machine.logs_quiesced(100), "first quiesce");

        // Straggler log for 100 lands (still LogsArriving — same block, same
        // latest_observed).
        machine.observe_log(100, false);
        machine.log_received(100); // in_flight=1
        assert!(!machine.logs_quiesced(100), "straggler re-opens");

        machine.log_applied(100); // in_flight=0
        assert!(machine.logs_quiesced(100), "straggler applied → re-quiesce");
    }

    /// `consume_quiesced` is the solver-release gate (ADR-008 D2): it returns
    /// `true` ONCE per quiesce cycle, then `false` until a new log re-arms it.
    /// This is what lets the pump publish exactly once per burst (not once
    /// per log), and again after a straggler settles.
    #[test]
    fn consume_quiesced_publishes_once_per_cycle_and_re_arms_on_straggler() {
        let mut machine = StageMachine::new(0, 0);
        machine.observe_header(100);
        machine.observe_log(100, false);
        machine.log_received(100);
        machine.log_applied(100);

        // First consume → true (publish). Second consume (no new log) → false.
        assert!(machine.consume_quiesced(100), "first quiesce publishes");
        assert!(!machine.consume_quiesced(100), "no new log → no re-publish");

        // Straggler log re-arms the gate.
        machine.observe_log(100, false);
        machine.log_received(100);
        assert!(
            !machine.consume_quiesced(100),
            "in-flight log → not quiesced"
        );
        machine.log_applied(100);

        assert!(
            machine.consume_quiesced(100),
            "straggler settled → publish again"
        );
        assert!(!machine.consume_quiesced(100), "second consume dry");
    }
}

#[cfg(test)]
mod quiesce_estimator_contract {
    use super::*;
    use degenbot_config::QuiesceMode;

    fn params(mode: QuiesceMode, overrides: impl FnOnce(&mut QuiesceParams)) -> QuiesceParams {
        let mut p = QuiesceParams {
            mode,
            fixed_ms: 50,
            floor_ms: 2,
            ceil_ms: 20,
            margin: 3.0,
            alpha: 0.1,
            late_budget: 120,
        };
        overrides(&mut p);
        p
    }

    fn adaptive(overrides: impl FnOnce(&mut QuiesceParams)) -> QuiesceParams {
        params(QuiesceMode::Adaptive, overrides)
    }

    /// Fixed mode is today's behavior: a constant window, blind to (and
    /// unperturbed by) the estimator's observations.
    #[test]
    fn fixed_mode_window_is_constant_and_ignores_observations() {
        let mut fsm = StageMachine::new(0, 0);
        fsm.set_quiesce_params(params(QuiesceMode::Fixed, |p| p.fixed_ms = 50));
        fsm.observe_settle_gap(40, 1_000);
        fsm.observe_settle_gap(1, 2_000);
        fsm.record_late_admit(2_000);
        fsm.record_late_admit(3_000);
        fsm.record_late_admit(4_000);
        assert_eq!(
            fsm.settle_window_ms(),
            50,
            "fixed mode ignores the estimator"
        );
        assert!(
            !fsm.quiesce_held_at_ceil(),
            "backstop never latches in fixed mode"
        );
    }

    /// The `pump_debounce_ms` parse contract carried over: a fixed window
    /// never collapses to 0.
    #[test]
    fn fixed_mode_window_never_collapses_to_zero() {
        let mut fsm = StageMachine::new(0, 0);
        fsm.set_quiesce_params(params(QuiesceMode::Fixed, |p| p.fixed_ms = 0));
        assert_eq!(fsm.settle_window_ms(), 1);
    }

    /// VD62GX design §6.1: the FIRST observation seeds the EWMA fully (no
    /// halved first update), so the estimator arms at a meaningful window
    /// from the very first block instead of drifting up from the floor.
    #[test]
    fn first_observation_seeds_the_ewma_within_bounds() {
        let mut fsm = StageMachine::new(0, 0);
        fsm.set_quiesce_params(adaptive(|p| p.alpha = 0.1));
        // Seed gap 6ms → W = min(6 × 3.0, ceil 20) = 18.
        fsm.observe_settle_gap(6, 100);
        assert_eq!(fsm.settle_window_ms(), 18);
    }

    /// `W_next = clamp(EWMA × SAFETY_MULT, W_FLOOR, W_CEIL)` — the EWMA
    /// (α = 0.5 for exact-arithmetic assertions) descends toward the
    /// coalescing regime and eventually parks at the floor.
    #[test]
    fn window_tracks_toward_the_gap_ewma_and_parks_at_the_floor() {
        let mut fsm = StageMachine::new(0, 0);
        fsm.set_quiesce_params(adaptive(|p| {
            p.alpha = 0.5;
            p.margin = 3.0;
        }));
        fsm.observe_settle_gap(4, 100);
        assert_eq!(fsm.settle_window_ms(), 12, "seeded EWMA 4 → 4×3");
        fsm.observe_settle_gap(0, 200);
        assert_eq!(fsm.settle_window_ms(), 6, "EWMA 2 → 2×3");
        fsm.observe_settle_gap(0, 300);
        assert_eq!(fsm.settle_window_ms(), 3, "EWMA 1 → 1×3");
        fsm.observe_settle_gap(0, 400);
        assert_eq!(fsm.settle_window_ms(), 2, "EWMA 0.5 → clamped at floor 2");
    }

    /// A 20ms+ WS-straggler regime may raise W toward the ceil but never
    /// through it (the D1 tombstone stays the correctness authority).
    #[test]
    fn ceiling_is_respected_under_straggler_bursts() {
        let mut fsm = StageMachine::new(0, 0);
        fsm.set_quiesce_params(adaptive(|p| p.alpha = 1.0));
        for i in 0..10u64 {
            fsm.observe_settle_gap(1_000, i * 100);
        }
        assert_eq!(
            fsm.settle_window_ms(),
            20,
            "W never grows beyond the ceiling"
        );
    }

    /// The pure-coalescing regime (sub-ms WS frame jitter) bottoms out at
    /// the floor, never 0.
    #[test]
    fn floor_is_respected_under_coalescing_bursts() {
        let mut fsm = StageMachine::new(0, 0);
        fsm.set_quiesce_params(adaptive(|p| p.alpha = 1.0));
        fsm.observe_settle_gap(0, 100);
        assert_eq!(
            fsm.settle_window_ms(),
            2,
            "W never collapses below the floor"
        );
    }

    /// Garbage config (zero floor, crossed ceiling, zero alpha) degenerates
    /// to a valid, nonzero, monotone window — never NaN or 0.
    #[test]
    fn unsanitized_config_degenerates_safely() {
        let mut fsm = StageMachine::new(0, 0);
        fsm.set_quiesce_params(adaptive(|p| {
            p.floor_ms = 0;
            p.ceil_ms = 0;
            p.alpha = 0.0;
            p.margin = f64::NAN;
        }));
        fsm.observe_settle_gap(9, 100);
        let w = fsm.settle_window_ms();
        assert!(w >= 1, "window must stay >= 1ms, got {w}");
        assert!(fsm.quiesce_ewma_ms().is_finite(), "EWMA must stay finite");
    }

    /// HJ5HWF contract §6.1: exceeding the late-admit budget in a sliding
    /// hour HOLDS the window at the ceiling; the hold releases once the
    /// ledger ages out. At-budget (not over) must NOT hold.
    #[test]
    fn late_budget_backstop_holds_at_the_ceiling_then_relaxes() {
        let mut fsm = StageMachine::new(0, 0);
        fsm.set_quiesce_params(adaptive(|p| {
            p.alpha = 1.0;
            p.late_budget = 2;
        }));
        fsm.observe_settle_gap(0, 1_000);
        assert_eq!(fsm.settle_window_ms(), 2);
        assert!(!fsm.quiesce_held_at_ceil());
        fsm.record_late_admit(1_100);
        fsm.record_late_admit(1_200);
        assert_eq!(fsm.settle_window_ms(), 2, "at budget is not over budget");
        fsm.record_late_admit(1_300);
        assert_eq!(fsm.settle_window_ms(), 20, "over budget → hold at ceiling");
        assert!(fsm.quiesce_held_at_ceil());
        // An hour after the NEWEST ledger event the entries drain and the
        // estimator relaxes again (the sliding hour measures from each
        // event's own stamp).
        fsm.observe_settle_gap(0, 1_300 + 60 * 60 * 1000 + 1);
        assert_eq!(
            fsm.settle_window_ms(),
            2,
            "aged-out ledger releases the hold"
        );
    }

    /// The estimator is purely additive: observations + the backstop never
    /// perturb the settle-point decision semantics (I5 publish-once).
    #[test]
    fn estimator_is_additive_to_the_settle_decision() {
        let mut fsm = StageMachine::new(0, 0);
        fsm.set_quiesce_params(adaptive(|p| {
            p.alpha = 1.0;
            p.late_budget = 0;
        }));
        fsm.observe_header(100);
        assert_eq!(fsm.on_log(100, false), LogDecision::DispatchForward);
        fsm.on_log_applied(100);
        fsm.record_late_admit(5);
        fsm.observe_settle_gap(3, 10);
        let decisions = fsm.on_settle();
        assert_eq!(decisions.len(), 1, "one publish, once per quiesce cycle");
        assert!(
            matches!(decisions[0], StageDecision::Publish { open: 100, .. }),
            "estimator state must not perturb the publish decision"
        );
    }
}

// ======================================================================
// 7NFYQW fold additions: stage-cycle view + watchdog phase space
// ======================================================================

/// The watchdog phase space (the dissolved `DrainerHealth`'s no-progress
/// obligation must remain representable here — see the module header).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchdogPhase {
    /// Headers fresh, logs flowing (or no window exceeded yet).
    Healthy,
    /// Headers have been stale >= the staleness window — the machine's
    /// `Recover` decision covers this phase (authoritative catch-up).
    HeaderStale,
    /// Headers fresh but no log arrived in the silence window — the
    /// `LogSilence` decision covers this phase (one warning per episode).
    LogsSilent,
}

impl WatchdogPhase {
    /// Is the phase a no-progress signal (an obligation for a driver-side
    /// watchdog to act on — recover, warn, or strike)?
    #[must_use]
    pub const fn is_no_progress(self) -> bool {
        !matches!(self, Self::Healthy)
    }
}

impl StageMachine {
    /// The machine's view of the open epoch's stage-table row:
    /// `None` = Streaming (logs open, unquiesced), then the Quiesced /
    /// Published / Finalize rows as their decisions fire (see the module
    /// header's stage table). A rewind resets the row — the fresh epoch
    /// starts the cycle again at Streaming (I2/I6).
    #[must_use]
    pub fn stage(&self) -> Option<Stage> {
        self.stage_in_cycle
    }

    /// The current rewind generation (the `Epoch.seq` of everything the
    /// machine mints; I2: bumps exactly once per `Rewind`). Drain-side
    /// consumers (the machine driver, at each inline work site) mirror this to WARN on
    /// reorg-flying stale work instead of silently consuming its epoch (I3).
    #[must_use]
    pub fn rewind_seq(&self) -> u64 {
        self.rewind_seq
    }

    /// The watchdog phase (see [`WatchdogPhase`]): the no-progress phase
    /// space the dissolved `DrainerHealth` (SZJUKL) maps its
    /// strike detector onto. Pure: same inputs as [`on_tick`](Self::on_tick);
    /// advances no state.
    #[must_use]
    pub fn watchdog_phase(
        &self,
        now_ms: u64,
        header_staleness_ms: u64,
        log_silence_ms: u64,
    ) -> WatchdogPhase {
        if now_ms.saturating_sub(self.last_header_at_ms) >= header_staleness_ms {
            WatchdogPhase::HeaderStale
        } else if now_ms.saturating_sub(self.last_log_at_ms) >= log_silence_ms {
            WatchdogPhase::LogsSilent
        } else {
            WatchdogPhase::Healthy
        }
    }
}
