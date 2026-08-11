# ADR-027: The block-pump dispatch seam — one owner, three application-specific pipes

**Status: accepted.** Codifies the block pump's delivery architecture (ergo epic `5CGU4V`,
tasks B1–B4): the pump's hand-offs to the sink, the solver-state verifier, and Python's
block clock are owned by a single **dispatch owner** but delivered over **three
application-specific pipes**, each with the delivery semantics its task needs. A future
architecture review must not re-suggest a third bespoke channel or a single-bus unification.

## Context

`BlockPump::run_with_stream` (rust/crates/degenbot-bot/src/bot_core/block_pump.rs) drives the
block pump's drain loop. Its hand-offs were ad-hoc and coupled:

- The **drain pipe** sent `Drain`/`Finalize`/`Publish`/`Notify` through one `mpsc` to a
  background drainer task — but **mixed delivery semantics**, FIFO-queuing `newHeads`
  notifications behind heavy Möbius solves, and shipped under an **opt-in flag**
  (`DEGENBOT_DECOUPLE_DRAIN`) with an inline fallback that had to stay behavior-identical.
- The **verifier** (ADR-021 solver-state accuracy gate) used a latest-wins `watch`.
- The **block clock** (Python's head tracker) rode the drain FIFO via `DrainWork::Notify`,
  so a slow solve delayed Python's `newHeads` tick by the solve duration.
- Liveness was a **30s time-based poll watchdog** (`DRAINER_STALL_SECS`) — a tuned knob
  that silently buffered a stalled drainer for up to 30s before failing.

This was hard to reason about (the long pole: the WS poller parking behind GIL-bound Python
/ Möbius solve), and the both-modes (inline vs decoupled) divergence had to be re-proven
correct twice.

## Decision

One **dispatch owner** module (`rust/crates/degenbot-bot/src/bot_core/event_dispatch.rs`,
`DispatchOwner`) owns all three pipes and coordinates ordering/liveness in one place. It is
a coordinated home — **NOT a single bus**; each pipe keeps the delivery semantics its task
needs ("one owner, multiple application-specific pipes").

1. **Drain pipe** — an ordered FIFO (`mpsc`) carrying `Drain`/`Finalize`/`Publish` to a
   background drainer task → `DrainSink`. Solves/dispatch/finalize run in enqueue order;
   FIFO + the engine/sink locks give the deferred work the semantics the pre-B4GX7C inline
   path had. The **background drainer is the sole mode** — the inline path and the
   `DEGENBOT_DECOUPLE_DRAIN` flag are retired (`bot_env_flag_default_off` deleted).

2. **Block-clock pipe** — a **direct** `DispatchOwner::notify_block` → `sink.notify_block`,
   deliberately NOT a `DrainWork` item, so a `newHeads` tick never rides the drain FIFO and
   is delivered ASAP, 1:1 (no coalescing). `SolveCoordinator::notify_block` no longer takes
   the `drain_lock` (the `engines` vec is frozen after start — ADR-006 late-registration
   panics — so the read-only fan-out needs no lock), so the clock does not contend with the
   drain fan-out. Callers hold no ordering guarantee on solver results.

3. **Verifier pipe** — a latest-wins `watch` to the solver-state verifier task. Only the
   most recent published block is ever verified (ADR-021); non-blocking so a slow verify can
   never stall the pump.

4. **Stall backstop** — the drain-pipe liveness check (B3), soak-hardened: the
   pump aborts when the queue holds a backlog (`depth >= BACKLOG_FLOOR = 2`) AND
   the drainer has completed no work for `STALL_WINDOW` (~30s). The wall-clock
   window (NOT event-counting) is what correctly distinguishes a *frozen*
   drainer from one mid-way through a single exceptionally long solve — a live
   mainnet dry-run proved that pure strike-counting (whether on pick-up depth
   or on completion count) **false-positives** under heavy multi-path solve
   load, aborting a busy-but-alive drainer. A drainer that progresses but falls
   behind is observed via `DispatchOwner::pending()` (a lag metric), never
   aborted; a dead (closed-channel) drainer still aborts immediately on a send
   into a closed channel. The old 30s *poll*-watchdog (`DRAINER_STALL_SECS`) is
   folded into this dispatch-time `STALL_WINDOW` backstop (checked on the
   pump's dispatch, not a background poll task).

## Consequences

- The WS poller never parks behind GIL-bound Python or a Möbius solve (the drain work is
  deferred), and Python's block clock is never queued behind solver work.
- The both-modes divergence is gone: one delivery path in the hot loop; the flag machinery
  is deleted.
- Liveness is robust: a frozen drainer fails loud
  ("fail loud, never half-alive") within the stall window; a slow-but-alive drainer is
  observed via the `pending()` lag metric instead of being killed.
- `DispatchOwner` exposes a small interface (dispatch/notify/pending), giving the seam a
  testable surface; the pure `NoProgress` accounting is unit-tested, and the frozen-drainer
  abort is validated by a subprocess test (`no_progress_frozen_drainer_aborts_proc`).

## Not decided here

- A literal latest-wins `watch`/coalescing block-clock: the direct path (every header, 1:1)
  is strictly more complete for a head tracker and was chosen instead.
- A per-engine-lock bypass for `notify_block`: `engine.notify_block` still briefly takes the
  engine `Mutex` (a solve holds it for its full duration — documented intentional lock
  discipline). The `drain_lock` layer of contention is removed (B2); the residual short
  engine-lock acquisition is existing behavior, out of scope.

## Tests

- `event_dispatch` unit + subprocess tests cover the stall predicate, the
  not-on-the-FIFO block clock, the verifier hand-off, and the frozen-drainer abort.
- A 300s live mainnet dry-run soak (B6) processed ~90 blocks under heavy solve load with
  zero false-positive aborts and a fresh (non-solve-gated) block clock.
- All 450 degenbot-bot lib tests pass (B1–B4 + the soak-driven stall fix), clippy-clean on the
  new/changed files, and
  the standalone-consumer gate is green.
