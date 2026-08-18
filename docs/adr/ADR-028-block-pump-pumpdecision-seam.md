# ADR-028: The block-pump `PumpDecision` seam — pure producer + thin executor

**Status: accepted.** Codifies epic A (ergo tasks A1–A5, `FUE5SP`): the block pump's
per-event policy is owned by a pure, I/O-free `PumpFSM` producing a `PumpDecision`
enum, executed by a thin async driver. This is the next step of the ADR-008 (BlockClock)
deepening — the same "deep module: pure decision producer + thin I/O driver" family as
ADR-027's dispatch owner. The FSM *decides*; the driver *executes*. No provider, no
timer, no `Instant`, no lock lives in the FSM.

## Context

`BlockPump::run_with_stream` (rust/crates/degenbot-bot/src/bot_core/block_pump.rs) was a
~935-line async loop that was the single choreographer of five interacting state machines —
the registration lifecycle, the `BlockClock`, the solve/drain fan-out, the quiesce-gated
publish, the reorg coordinator — plus all provider I/O. ADR-008 had already lifted block
completeness (the tombstone, the cursor, the quiesce classification) into the pure
`BlockClock`; the surrounding *policies* — when a settle publishes, when a recovery anchor
makes recovered forwards benign, when the watchdogs fire, when a tombstone verifies WS
delivery — were still inline async `if`s entangled with `tokio` timers and the sink.

That was hard to reason about and untestable without a live provider: the rules closed over
`tokio::time::Instant` and the `DrainSink`, so no test could feed a synthetic log sequence
and assert "exactly one publish, never a premature one".

## Decision

Introduce a pure decision producer, `PumpFSM` (`rust/crates/degenbot-bot/src/bot_core/
pump_fsm.rs`), that owns **every "which effect, when" rule** of the pump's per-block loop,
and turn `run_with_stream` into a **thin async driver** that feeds events in and executes
the returned `PumpDecision`s. The FSM holds NO provider, NO timer, NO `Instant`, NO lock —
all time enters as `now_ms` data, all I/O returns as decisions for the driver to run.

The FSM owns the rules for the pump's policy families:

- **Quiesce-before-publish + solver-release gate (ADR-008 D2)** — `on_settle`: emits a
  `Publish` only at a settle point when the open block is quiesced (all dispatched logs
  applied); a `Backfill` otherwise. Test-proven: exactly one publish per quiesce cycle,
  never a premature one while logs are in flight.
- **Recovery anchor + single-writer discard (BQ7ZBC / DFQYM5)** — `record_backfill`
  (monotone re-anchor after an authoritative catch-up) and `should_drop_recovered_forward`
  (a recovered forward ≤ the anchor is a benign duplicate, dropped; a reorg log is never
  dropped; a stale forward above the anchor still faults).
- **Watchdogs as tick inputs (JIABO3 / logs-silence)** — `on_tick(now_ms,
  header_staleness_ms, log_silence_ms)` decides `Recover` / `LogSilence` from elapsed data;
  `record_header` / `record_log` feed the watchdog clocks. The driver's
  `tokio::time::interval` only drives the data feed.
- **WS-delivery completeness verdict (DFQYM5 / WS-DROP)** — `completeness_decision`
  hands the tracked delivered log-index set to the driver as `VerifyCompleteness`; the FSM
  owns *when* (only a just-tombstoned, tracked block), the abort is the executor's
  consequence of the authoritative `eth_getLogs` mismatch.
- **Solve anchor (ADR-008 D2)** — `drain_decision` emits `Drain` at the log-driven
  settled block (never the racing header).
- **State ownership** — the cursor (`current_block`), per-block metadata snapshots
  (VTWCIG), the quiesce arm, `recovery_anchor`, the ws-delivered tracker, and the
  `BlockClock` all live on the FSM. The driver references them through `fsm.*` and its
  I/O helpers fold results back through explicit FSM methods (never scattered writes).

`run_with_stream` remains the sole caller that realizes `PumpDecision` against the
executor it already has — the `DispatchOwner` of ADR-027 (via `DrainWork`), the provider,
the `ReorgCoordinator`, the sink, and the process.

## What the executor (driver) owns

The I/O the FSM cannot do, returned as decisions the driver executes:

- **Locks + ordering** — the `drain_lock → engine-Mutex → BotState RwLock` discipline
  (ADR-006 D2) stays entirely in the coordinator/engine/sink layer, never in the FSM.
- **RPC** — `eth_getLogs` for backfill and for the WS-completeness cross-check; the
  abort/panic on a live-websocket log drop (`std::process::abort`) is the executor's
  loud failure of the authoritative mismatch.
- **Spawning / async** — the background drainer task (ADR-027), `handle_timeout_eager`
  (which now takes `&mut PumpFSM`), the verifier watch.
- **The `DispatchOwner`** — routes `DrainWork::{Drain, Finalize, Publish}` + the
  direct `notify_block` clock pipe.
- **The `ReorgCoordinator`** — executes the reorg unwind the FSM's clock classified
  (`EnterReorg` / `ContinueReorg` / `CloseReorg`).

## Consequences

- **Testability without horology.** Every policy is a pure FSM method exercised by feeding
  synthetic `(event, now_ms)` sequences with a fake clock — no provider, no timers. The pure
  tests added with the epic cover each rule (quiesce exactly-once/premature, single-writer
  discard, watchdog fire/not-fire/once-per-episode, completeness verdict, drain anchor,
  header notify/gap/backfill, settle publish/backfill, stream-end flush+stop).
- **One decision surface.** The pump's per-event effect is a `PumpDecision`; `run_with_stream`
  became a thin driver over that surface instead of a 935-line inline policy. Behavior is
  relocated + decision-surfaced, not changed.
- **Watchdogs are data, not timers.** The FSM owns no `tokio` timer; the interval is a data
  feed. Time enters as `now_ms` so the FSM is deterministic and free of `Instant`.
- **Recovery/single-writer/consistency rules are encapsulated.** `record_backfill`, the
  recover-discard, the completeness verdict, and the settle gate are single FSM ownership
  points instead of inline comments scattered across the loop.

## Not decided here / superseded — A6 "DrainSink one per-block entry" (Candidate C)

Epic A's task A6 (ergo `34QPUZ`) was specced (from the earlier architecture-review
candidates) as collapsing the 9-method `DrainSink` trait into one per-block entry
`drain(block, metadata)` that "internally owns cursor advancement, lock order, and the
quiesce gate". **That exact shape is superseded by this epic and ADR-027, and is
deliberately not built.**

- The **quiesce gate** the A6 spec wants inside the drain entry is precisely what A2 moved
  *out of* the executor and *into* the FSM's `on_settle` decision. Re-owning it in the drain
  entry would un-build the pure-producer design this epic establishes.
- The **cursor advancement** ("last drained block") already lives inside the
  `SolveCoordinator`'s drain-locked entry methods (under `drain_lock`), and the per-event
  cursor lives in the FSM's `BlockClock` (ADR-008).
- The pump's *drain* hand-offs already go through a single per-block surface: the FSM
  decision → `DispatchOwner` → `DrainWork` (ADR-027). The wide `DrainSink`
  (`SolveCoordinator`) + `Engine` surfaces are the executor's fan-out detail behind that
  seam, not a policy the pump names directly.

**Disposition: the FSM `PumpDecision` surface (A1–A5) + the ADR-027 dispatch owner jointly
realize A6's goal** ("the FSM's dispatcher is the single caller that would otherwise
re-expose the nine methods"); the literal `DrainSink::drain` collapse is recorded here as
superseded rather than executed, so it is not re-litigated without a forcing function (a
structural need for the trait object itself to be the one-entry policy surface).

## Correction addendum (epic O3HW7E, 2026-08)

The architecture review of this seam (2026-08-17) found two ownership frictions the
original ADR did not anticipate. Both are corrected in place; the decisions above stand.

### (a) State ownership: no shared interior crosses the PumpFSM capsule

The delivery cutoff (glossary: **Last complete block**; code name
`pump_complete_cutoff` on `BotState`) is a **monotone value owned by `BotState`**,
advanced by the pump driver when it *executes* a tombstone verdict
(`LogDecision::TombstonePrevious`). It is **not** FSM-owned and **not** shared: the
3M5PO5 `Arc<AtomicU64>` handle that once bridged the shared atom into the driver is
retired — no shared interior state crosses the `PumpFSM` capsule in either direction.
`BotState` is the single owner; the driver mirrors the tombstone verdict into it; a
resume (fresh pump, fresh `PumpFSM` + `BlockClock`) never resets it (test-pinned:
`resume_never_resets_pump_complete_cutoff`). This closes the last seam-friction: the
FSM capsule remains pure, and the one stateful fact the executor needs across pump
lifetimes lives in the one stateful owner (state), not threaded through the seam.

### (b) Resume boundary drop: the single-writer rule has one owner

At a resume where the snapshot→WS gap was backfilled (seed S < first observed W), the
backfill covers `[S+1, W]` inclusive. That boundary was previously enforced by an
**inline driver check** in `run_with_stream`'s log loop (the original DFQYM5
`snapshot_seed` drop) in addition to the FSM's BQ7ZBC
`should_drop_recovered_forward` — the same rule, two owners. Corrected: the driver now
seeds the FSM's `recovery_anchor` with W via `record_backfill(first_observed_block)`
at resume when backfill covered `[S+1, W]`, and the inline check is **deleted** — the
single-writer drop rule ("backfill owns `[S+1, W]`, the live WS owns `[W+1, ∞)`") is
owned by exactly one FSM method, unified across the resume boundary and mid-run
`handle_timeout_eager` recovery. The reorg arm of the rule is unchanged and now
actually holds at the resume boundary: `should_drop_recovered_forward(removed: true)`
is always false, so a re-delivered `removed: true` log at ≤ W reaches the reorg
classifier and can unwind the backfilled range (a behavior delta of this epic — the
old inline check dropped reorg logs silently at the boundary; test-pinned:
`resume_boundary_reorg_reaches_classifier_not_inline_drop`). The drop is traced as
`DroppedRecovery` on the `ws_event_decision` target.

### (c) A6 reaffirmation: the cursor stamps are executor fan-out, by design

Future reviews must not re-suggest collapsing the pump's direct cursor stamps —
`set_last_solved_block`, `set_solve_anchor`, `record_logs_this_block`,
`last_processed_block`, the change-set take — into the FSM or into a narrower
`DrainSink`. They are the **executor's fan-out detail behind the `PumpDecision` seam**
(ADR-027's dispatch owner + ADR-025/LEZJAS engine ownership), executed because the FSM
emitted a decision; they are not pump policy. The apply-site pairing doc-comment
(test-pinned by `log_applied_pairing_forward_records_reorg_does_not`) states the one
coordination the driver does own: one applied forward log feeds exactly two consumers
—the FSM quiesce arm and the engine's `has_logs_this_block` bookkeeping. The A6
supersession above closes the re-suggestion loop for good: absent a structural forcing
function (the trait object itself becoming the one-entry policy surface), the wide
`DrainSink` + `Engine` surfaces stay as executor fan-out.
