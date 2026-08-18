//! `BlockClock` — the pure, I/O-free per-block state machine (ADR-008).
//!
//! The testable core of the pump's block-clock layer. Owns the per-block
//! state map and returns decisions the pump loop acts on; the pump owns all
//! async I/O (WS subscriptions, `eth_getLogs` backfill, shutdown). Pure:
//! no `async`, no `tokio`, no RPC.
//!
//! See `docs/adr/ADR-008-block-state-machine.md` for the full design.
//!
//! ## Lifecycle (per block N)
//!
//! `Observed(N)` → `LogsArriving(N)` → `LogsApplied(N)` → `Drained(N)`,
//! with `Tainted(N)` reachable from `LogsApplied`/`Drained` via a
//! `removed: true` late log (reorg path).
//!
//! **The load-bearing invariant (ADR-008 D1):** `cursor()` returns N only
//! when block N is `Drained`. A header alone can never reach `Drained` —
//! only `advance_to_drained` (called by the pump after verify) does, and
//! `advance_to_drained` requires `LogsApplied`, which requires the tombstone
//! (first `removed: false` log for N+1).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

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

/// Decision returned by [`BlockClock::observe_header`] telling the pump what
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

/// Decision returned by [`BlockClock::observe_log`] telling the pump how to
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
    /// A `removed: false` log on a tombstoned block, NOT in the reorg path —
    /// unreliable WS (out-of-order / duplicated forward events). The pump
    /// must shut down (ADR-008 D3).
    PanicLateForward(u64),
}

/// The pure per-block state machine (ADR-008).
///
/// Owns the per-block state map and the cursor. The pump loop feeds events
/// via [`observe_header`](Self::observe_header) /
/// [`observe_log`](Self::observe_log) and acts on the returned decisions; the
/// clock itself performs no I/O.
#[derive(Debug, Default)]
pub struct BlockClock {
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
    /// currently arriving. A forward log for a block > `open_block` tombstones
    /// `open_block` (D1). Headers do not touch this.
    open_block: Option<u64>,
    /// Whether the clock is currently in the reorg path (accepting a
    /// contiguous `removed: true` chunk).
    in_reorg: bool,
    /// The deepest block that has reached the TOMBSTONE (`LogsApplied` or,
    /// transiently, `Drained`) — i.e. the highest FULLY-DELIVERED block. This
    /// is the single source of truth the registration drain
    /// (`BotState::apply_pump_buffer_v3/_v4` → `drain_pump_completed`) reads
    /// for its cutoff, replacing the retired buffer shadow marker (3M5PO5).
    /// Shared by `Arc` so both the pump task (which owns `BlockClock`) and the
    /// registration path (`BotState` on the asyncio thread) read the same value
    /// without a cross-runtime lock. `0` until the first tombstone.
    highest_applied: Arc<AtomicU64>,
}

impl BlockClock {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

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
                // open block has moved past it; this is a late forward on a
                // tombstoned block → unreliable WS → panic (ADR-008 D3).
                LogDecision::PanicLateForward(block)
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

    /// Transition `block` from `LogsArriving` to `LogsApplied` (the tombstone)
    /// and advance the shared [`highest_applied`](Self::highest_applied)
    /// cutoff — the one signal the registration drain relies on.
    fn tombstone(&mut self, block: u64) {
        if let Some(state) = self.blocks.get_mut(&block) {
            *state = BlockState::LogsApplied;
        }
        // Monotonic store — a re-reported/re-awaited block never rewinds the
        // cutoff (mirrors the retired `mark_block_complete` contract).
        let mut cur = self.highest_applied.load(Ordering::Relaxed);
        while block > cur {
            match self.highest_applied.compare_exchange_weak(
                cur,
                block,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
    }

    /// Shared handle to the deepest tombstoned (`LogsApplied`/`Drained`)
    /// block. The pump hands this to `BotState` at startup so the
    /// registration drain reads the same completeness cutoff the pump's own
    /// clock tracks — the single source of truth (3M5PO5).
    #[must_use]
    pub fn highest_applied_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.highest_applied)
    }

    /// The deepest tombstoned (`LogsApplied`/`Drained`) block — the
    /// registration drain's completeness cutoff (3M5PO5). Computed over the
    /// per-block map; `0` until the first tombstone. Test-side value read
    /// until T2 (BGEDB6) moves authority for the cutoff to `BotState` and
    /// the shared atom goes away.
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
        let mut clock = BlockClock::new();
        // Observe a block header — opens the block but does NOT drain it.
        assert_eq!(clock.observe_header(100), HeaderDecision::OpenNew(100));
        assert_eq!(
            clock.cursor(),
            None,
            "a header alone must not set the cursor"
        );

        // Even observing a later header does NOT help — still no log tombstoned 100.
        assert_eq!(
            clock.observe_header(101),
            HeaderDecision::PendingSuccessor {
                sealed: 100,
                pending: 101
            },
        );
        assert_eq!(
            clock.cursor(),
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
        let mut clock = BlockClock::new();
        assert_eq!(clock.highest_applied(), 0, "no tombstone yet");

        // Observe headers — a header alone must NOT advance the cutoff.
        clock.observe_header(100);
        assert_eq!(clock.highest_applied(), 0);
        clock.observe_header(101);
        assert_eq!(clock.highest_applied(), 0);

        // The first removed:false log for 101 tombstones 100.
        assert_eq!(
            clock.observe_log(101, false),
            LogDecision::TombstonePrevious(100)
        );
        assert_eq!(
            clock.highest_applied(),
            100,
            "a successor log tombstones the predecessor into the cutoff"
        );

        // A further tombstone (a log for 102 closes 101) advances monotonically.
        assert_eq!(
            clock.observe_log(102, false),
            LogDecision::TombstonePrevious(101)
        );
        assert_eq!(clock.highest_applied(), 101);
    }

    /// The full happy path: `Observed → LogsArriving → LogsApplied → Drained`,
    /// where the tombstone is the first `removed: false` log for N+1 (D1) and
    /// `cursor()` returns N only after `advance_to_drained`.
    #[test]
    fn full_lifecycle_tombstone_via_successor_log_then_drained() {
        let mut clock = BlockClock::new();
        clock.observe_header(100); // Observed(100)
        assert_eq!(clock.state_of(100), Some(BlockState::Observed));

        // First log for 100 → LogsArriving.
        assert_eq!(clock.observe_log(100, false), LogDecision::DispatchForward);
        clock.log_received(100); // in_flight = 1
        assert_eq!(
            clock.state_of(100),
            Some(BlockState::LogsArriving {
                in_flight: 1,
                ever_quiesced: false
            })
        );
        assert!(!clock.logs_quiesced(100), "in-flight log → not quiesced");

        clock.log_applied(100); // in_flight = 0 → quiesced
        assert!(clock.logs_quiesced(100), "all applied → quiesced");

        // First forward log for 101 → tombstone 100. Cursor STILL None.
        assert_eq!(
            clock.observe_log(101, false),
            LogDecision::TombstonePrevious(100),
        );
        assert_eq!(clock.state_of(100), Some(BlockState::LogsApplied));
        assert_eq!(clock.cursor(), None, "LogsApplied is not Drained yet");

        clock.advance_to_drained(100);
        assert_eq!(
            clock.cursor(),
            Some(100),
            "Drained is the only cursor source"
        );
        assert_eq!(clock.state_of(100), Some(BlockState::Drained));
    }

    /// `advance_to_drained` on a block that is only `Observed` (no logs, no
    /// tombstone) must be a no-op — the invalid `Observed → Drained` transition
    /// has no API path (ADR-008). A header alone can never drain.
    #[test]
    fn advance_to_drained_on_non_applied_block_is_noop() {
        let mut clock = BlockClock::new();
        clock.observe_header(100); // Observed only — no log ever tombstoned it
        clock.advance_to_drained(100); // must not transition
        assert_eq!(clock.cursor(), None);
        assert_eq!(clock.state_of(100), Some(BlockState::Observed));
    }

    /// A late `removed: false` log on a `Drained` block, outside a reorg, is
    /// an unreliable-WS signal → `PanicLateForward` (ADR-008 D3). The pump
    /// shuts down; the cursor never silently regresses.
    #[test]
    fn late_forward_log_on_drained_block_panics() {
        let mut clock = BlockClock::new();
        // Drive 100 to Drained the honest way.
        clock.observe_header(100);
        clock.observe_log(100, false);
        clock.log_received(100);
        clock.log_applied(100);
        clock.observe_log(101, false); // tombstone 100
        clock.log_received(101);
        clock.log_applied(101);
        clock.observe_log(102, false); // tombstone 101
        clock.advance_to_drained(100);
        clock.advance_to_drained(101);
        assert_eq!(clock.cursor(), Some(101));

        // A forward log for 100 now arrives — late forward on a Drained block.
        assert_eq!(
            clock.observe_log(100, false),
            LogDecision::PanicLateForward(100),
        );
        assert_eq!(clock.cursor(), Some(101), "cursor holds — no regression");
    }

    /// A `removed: true` log on a tombstoned block enters the reorg path; the
    /// first `removed: false` after entry closes the window and that block
    /// becomes the new head. Earlier tracked blocks are tainted for replay
    /// (ADR-008 D3).
    #[test]
    fn reorg_path_closes_on_first_forward_after_removed_chunk() {
        let mut clock = BlockClock::new();
        // Drive 100 + 101 to Drained the honest way. 102 tombstones 101.
        clock.observe_header(100);
        clock.observe_log(100, false);
        clock.log_received(100);
        clock.log_applied(100);
        clock.observe_log(101, false); // tombstone 100
        clock.log_received(101);
        clock.log_applied(101);
        clock.observe_log(102, false); // tombstone 101
        clock.advance_to_drained(100);
        clock.advance_to_drained(101);
        assert_eq!(clock.state_of(101), Some(BlockState::Drained));
        assert_eq!(clock.cursor(), Some(101));

        // Reorg: removed:true logs arrive (any order) — enter + continue.
        assert_eq!(clock.observe_log(101, true), LogDecision::EnterReorg(101),);
        assert!(clock.in_reorg());
        assert_eq!(clock.observe_log(100, true), LogDecision::ContinueReorg);
        assert_eq!(clock.observe_log(99, true), LogDecision::ContinueReorg);

        // First removed:false → window closes, block 103 is the new head.
        assert_eq!(
            clock.observe_log(103, false),
            LogDecision::CloseReorg { new_head: 103 },
        );
        assert!(!clock.in_reorg());
        // Earlier tracked blocks tainted for replay; 103 now LogsArriving.
        assert_eq!(clock.state_of(101), Some(BlockState::Tainted));
        assert_eq!(clock.state_of(100), Some(BlockState::Tainted));
        assert_eq!(
            clock.cursor(),
            Some(101),
            "drained common ancestor survives"
        );
        assert_eq!(clock.latest_observed(), Some(103));
    }

    /// `logs_quiesced` re-enters (becomes false) on a straggler log landing
    /// after the first quiesce (ADR-008 D2). The solver-release gate must
    /// re-close until the straggler is applied.
    #[test]
    fn logs_quiesced_re_opens_on_straggler() {
        let mut clock = BlockClock::new();
        clock.observe_header(100);
        clock.observe_log(100, false);
        clock.log_received(100);
        clock.log_applied(100);
        assert!(clock.logs_quiesced(100), "first quiesce");

        // Straggler log for 100 lands (still LogsArriving — same block, same
        // latest_observed).
        clock.observe_log(100, false);
        clock.log_received(100); // in_flight=1
        assert!(!clock.logs_quiesced(100), "straggler re-opens");

        clock.log_applied(100); // in_flight=0
        assert!(clock.logs_quiesced(100), "straggler applied → re-quiesce");
    }

    /// `consume_quiesced` is the solver-release gate (ADR-008 D2): it returns
    /// `true` ONCE per quiesce cycle, then `false` until a new log re-arms it.
    /// This is what lets the pump publish exactly once per burst (not once
    /// per log), and again after a straggler settles.
    #[test]
    fn consume_quiesced_publishes_once_per_cycle_and_re_arms_on_straggler() {
        let mut clock = BlockClock::new();
        clock.observe_header(100);
        clock.observe_log(100, false);
        clock.log_received(100);
        clock.log_applied(100);

        // First consume → true (publish). Second consume (no new log) → false.
        assert!(clock.consume_quiesced(100), "first quiesce publishes");
        assert!(!clock.consume_quiesced(100), "no new log → no re-publish");

        // Straggler log re-arms the gate.
        clock.observe_log(100, false);
        clock.log_received(100);
        assert!(!clock.consume_quiesced(100), "in-flight log → not quiesced");
        clock.log_applied(100);

        assert!(
            clock.consume_quiesced(100),
            "straggler settled → publish again"
        );
        assert!(!clock.consume_quiesced(100), "second consume dry");
    }
}
