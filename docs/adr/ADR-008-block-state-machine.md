# ADR-008: Per-block state machine for the pump's block clock

**Status: implemented (core).** Recorded during the block-state-machine review, June 2026.
Resolves the verify-race class of bugs (V2-V2-V3 crash at block 25397049: a Mint at 25397047
un-applied when 25397049's `newHeads` advanced the cursor while N's liquidity log was still
in-flight). Full design note and rationale live in `docs/architecture/block-state-machine.md`;
this ADR records the settled shape only.

## Implementation status

Implemented in `rust/crates/degenbot-bot/src/bot_core/` (commits `5673f8ce`, `440b848`,
`c4d21b1e`, `cdac7363`, `0baed1e`):

- **D1 (tombstone via successor log)** — `BlockClock` + pump wiring. A `newHeads` header
  alone never finalizes or drains; only the first `removed: false` log for N+1 tombstones N.
  The empty-block finalize fast path (`block_pump.rs:593-595`) is deleted. Pinned by
  `header_alone_does_not_drain_block` + `full_lifecycle_tombstone_via_successor_log_then_drained`.
- **D2 (LogsQuiesced solver-release gate)** — the wall-clock `DEBOUNCE_MS` send timer is
  **replaced** by the `consume_quiesced` predicate. `on_send` fires only when the open block is
  quiesced (all dispatched logs applied) AND at a settle point (a `DEBOUNCE_MS` window with no
  new event, coalescing a same-block burst into one publish at the tail, OR stream exhaustion).
  Publication is gated on state, not schedule. A straggler log re-arms the gate (the one-shot
  `consume_quiesced` resets; the new log's receive→apply re-sets it). Pinned by
  `burst_of_logs_publishes_once_at_tail_via_quiesce_gate` (pump) +
  `consume_quiesced_publishes_once_per_cycle_and_re_arms_on_straggler` (clock).
- **D3 (asymmetric late-event handling)** — clock returns `EnterReorg`/`ContinueReorg`/
  `CloseReorg`/`PanicLateForward`; the pump routes `removed: true` to `ReorgCoordinator`
  (per-event restore), closes the reorg window on the first `removed: false` (its block is the
  new head), and shuts down on a late `removed: false`. Pinned by
  `reorg_contiguous_chunk_closes_on_first_forward_and_continues` +
  `late_forward_log_on_tombstoned_block_shuts_down_pump`.
- **D4 (backfill single-branch)** — `backfill_range` feeds each fetched log through
  `clock.observe_log` (same SM as live logs; no `Backfilled → Drained` edge). Dead-logs-sub
  Edge B RPC-budget guard + the dedicated dead-sub detection timeout are **deferred** (the
  existing 60s inactivity timeout covers the degraded path).

**Deferred to follow-up:** the `[DIAG]` newHeads-stall instrumentation in `run_with_stream`
(operational logging, retained until a follow-up confirms the SM's liveness paths cover the stall
scenario); the dead-logs-sub Edge B timeout tuning + RPC budget.

## Context

The pump has one cursor — `last_processed_block` (= `last_drained_block`, in
`solve_coordinator.rs:157`) — advanced by **two independent WS event sources** (`newHeads` +
`logs`, combined via `stream_select` at `bot_core/block_pump.rs:201`). The correctness invariant the verify
depends on is:

> **engine-state@cursor ≡ on-chain@cursor** on the liquidity dimension.

Today the cursor is allowed to advance to N+1 (on the `newHeads` edge) while block N's liquidity
log is still in-flight on the *other* WS stream. The verify-side "fix" in commit `033a6b2f` (drop
the redundant startup batch verify) only hides the race from the verify; it does nothing for the
solver/dispatch loop, which still runs against possibly-incomplete state. This is a
pump-correctness bug, not a verify bug.

The current code enforces the invariant (badly) with scattered booleans — `has_logs_this_block`,
`first_header`, `debounce_active`, `last_solved_block`, `current_block` — and
`has_logs_this_block` is *reset* on the next header advance, so a late log silently reopens a
"closed" block with no reconciliation. There is no single authoritative "what is the state of
block N," so the invalid transition is easy to fall into and hard to test for.

## Decision

Adopt a **per-block state machine** for the pump's block clock. One state machine **per block
number N** the pump is tracking; a block is in exactly one state at a time. Only states with
*observable, non-timer triggers* are in the machine.

```
                 ┌──────────────┐
                 │   Observed   │   ← newHeads(N) OR first log with block_number==N
                 └──────┬───────┘
                        │ first log for N received
                        ▼
                 ┌──────────────┐
                 │ LogsArriving  │   ← ≥1 log received for N; more may come.
                 │              │      Solver MAY run here (fallible, accept re-solve
                 └──────┬───────┘      on straggler, downstream simulation discards)
                        │ first removed:false log for N+1 (the tombstone)
                        ▼
                 ┌──────────────┐
                 │  LogsApplied │   ← N is provably closed by successor opening
                 └──────┬───────┘
                        │ verify completion
                        ▼
                 ┌──────────────┐
                 │   Drained    │   ← last_processed_block() may return N ONLY here
                 └──────────────┘
```

### Transition table (settled)

| From | To | Trigger |
|---|---|---|
| (none) | `Observed(N)` | `newHeads(N)` OR first log with `block_number==N` |
| `Observed(N)` | `LogsArriving(N)` | first log for N dispatched via `dispatch_log` |
| `LogsArriving(N)` | `LogsApplied(N)` | **first `removed: false` log for N+1** (the tombstone) |
| `LogsApplied(N)` | `Drained(N)` | verify completion for N |

**`last_processed_block()` is permitted to return N only when the per-block machine for N is
`Drained`.** Only `Drained` feeds the cursor.

### D1. The tombstone is a real successor log, never a header

`LogsApplied(N)` is reached via the **first log (not `newHeads`) with `block_number == N+1`,
`removed: false`.** A push WS cannot prove "no more logs for N"; only a successor block's first
real event can. `newHeads` is demoted to a **liveness probe for the logs subscription only**,
never a completeness signal — a `newHeads(N+1)` header arriving with no logs for N+1 following
within a window indicates the logs subscription may have died (→ D4).

This makes the current code's two invalid transitions physically impossible (no match arm):

1. `Observed/LogsArriving → Drained` on `newHeads(N+1)` — the empty-block finalize fast path
   (`bot_core/block_pump.rs:593-595`).
2. Late `block_number==N` log silently reopening a closed block (`has_logs_this_block` reset,
   `update_block` jumping backward).

### D2. `LogsQuiesced` is a side-channel predicate, not a state

`LogsQuiesced` was initially drafted as a state between `LogsArriving` and `LogsApplied`. It has
no honest positive trigger as a state: a push WS has no "no more logs" event, and "queue
momentarily empty between two events" tells us nothing useful. So it does **not** earn a slot in
the block lifecycle.

It *does* earn a slot as a **solver-release gate**. The debounce timer in the current code is a
dumb wall-clock throttle that fires whether or not more logs are queued behind it. Replacing it
with a real signal fixes that:

- Per-block **in-flight counter**: number of logs for N received but not yet fully applied to
  pool state.
- **`LogsQuiesced(N)` predicate** (not a state): true when the in-flight counter for N hits 0
  since entering `LogsArriving(N)`. Re-enters (becomes false) on the next straggler log.
  Semantics: "every log event the WS has given us for N has been fully applied to pool state."
- **Solver-release gate**: solver results may be released to consumers only when the current
  block's `LogsQuiesced` predicate is true. The solver may be *invoked* at any point during
  `LogsArriving`; only *publication* is gated.

The solver may run at `LogsArriving` (best-effort): a fallible intermediate solve is OK, because
these are always simulated later and failing results from an intermediate solve that was
superseded later in the block can just be discarded.

### D3. Asymmetric late-event handling (their door, not ours)

A log with `block_number == N` arriving *after* N is `LogsApplied` (or `Drained`) means two very
different things depending on the `removed` flag. A late log on a tombstoned block is either a
chain reorg (handle via journal restore) or a broken subscription (halt) — **no
tolerate-and-re-solve middle path**.

- **`removed: true` on a tombstoned block → reorg path.**
  - Nodes may deliver reorg events in any order (reverse log index, reverse block number,
    unordered); we make no ordering assumption. We accept the contiguous chunk of `removed: true`
    events.
  - **Reorg-window close**: gated on the **first `removed: false` event received after entering
    the reorg path.** That event's block is the new head; forward tracking resumes monotonically
    from there. No separate resumption-point selection.
  - **Pool state restore**: restore via the existing `ReorgCoordinator::dispatch_reorg_log` /
    `BotState::restore_before_block` machinery to the common ancestor (the deepest `Drained`
    block that survives the reorg). Rewind the per-block machines of unwound blocks to
    `Observed(N-X)` and re-track forward. If the reorg is too deep (`NoStatePriorToBlock`),
    graceful shutdown — the cursor never regresses past a `Drained` block.

- **`removed: false` on a tombstoned block → panic/shutdown.** A forward log on a tombstoned
  block means WS delivery is unreliable (out-of-order or duplicated forward events), not a reorg.
  This is unrecoverable for correctness — we can't trust any tombstone the WS gave us.

### D4. Dead logs subscription → Edge B backfill (taint from N+1, single-branch replay)

The logs subscription may die while `newHeads` stays alive. This is **self-healing in one sense**:
no forward state transitions occur (no logs arrive → no pool state mutates → nothing corrupts,
just goes stale). The recovery is Edge B backfill.

- `newHeads(N+1)` arrives but no log for N+1 follows within the timeout window → logs sub
  suspected dead → fire Edge B backfill.
- **Backfill range starts at N+1, not N.** We only tombstone a block on positive receipt of an
  event on the next block, so if N is `LogsApplied`, the logs sub was alive at least through
  N+1's first event. The dead-sub gap can only start *after* the last successfully tombstoned
  block. **N stays sealed.**
- **Taint range = `[N+1, head]`** — these are the blocks we never proved complete. Mark every
  block in the range that was `LogsApplied`/`Drained` as `Tainted`, restore pool state to its
  end-of-N state (the common ancestor), and replay forward.
- **Single-branch replay — no fast path.** The backfilled logs flow through the *same* state
  machine as live logs: N+1 goes `Observed → LogsArriving → LogsApplied` as logs are fed in. The
  state-machine overhead is small and one branch means one set of invariants to test. The
  `LogsApplied` tombstone arrives via the first `removed: false` log for the block after the
  backfill range. (The existing `backfill_range` at `bot_core/block_pump.rs:763` is the same mechanism —
  there is no distinguished `Backfilled → Drained` edge.)

## Alternatives considered

- **Separate cursors** — `block_clock` legitimately leads (the dispatcher's fee/timing can); only
  the *verify* keys off a `liquidity_cursor` that advances only on reconciled log-application.
  Smaller blast radius, doesn't stall the solve clock, but treats the symptom (verify at a
  non-moving point) rather than the disease. **Rejected:** the solve path would still run against
  possibly-incomplete state — exactly the lag that burned the verify thread. The SM is only the
  right call if the solver itself is provably running against complete liquidity state, which is
  what this bot requires.

- **`LogsQuiesced` as a state with a debounce-timer trigger** — promotes `LogsArriving →
  LogsQuiesced` on "WS silent for ≥ debounce window." **Rejected:** ties state transitions to a
  wall-clock timer, which makes no sense during a fast multi-block reorg/replay (would cascade
  `O(debounce × range)` of wall time before catching up). The debounce timer is a
  solver-to-consumer release throttle, a separate concern from the state machine — see D2.

- **Distinguished `Backfilled(N) → Drained(N)` edge** that bypasses `LogsApplied` only when the
  log set came from a self-contained `eth_getLogs` fetch. **Rejected:** the distinction "WS
  contribution is racy, RPC contribution is authoritative" would make correctness depend on the
  *source* of the log rather than the machine's invariants. Both flow through the same lifecycle,
  because the invariants (tombstone via successor, verify-sealed `Drained`) are what establish
  correctness, not the log source.

## What this does NOT supersede

The per-pool pin gates (step-1 / step-2 snapshots, V3 + V4 twins) **complement** the SM, they do
not replace it:

- The pins are the **verify's** defense (frozen-block comparison, immune to whatever the cursor is
  doing).
- The SM is the **pump's** defense (the cursor itself can't advance past un-applied logs; the
  solver can't publish against un-applied state).

They defend different things and should both exist: the pins catch a bug the SM *should* have
prevented but didn't (defense in depth); the SM prevents the solver from running against
incomplete state in the first place (the thing the pins can't help with, because they only run at
verify time, not solve time). V4 verification already pins step-1/step-2 via the
`post_drain_snapshot` (twin of V3's), so the cursor-correctness fix transfers without
family-specific work — the SM is family-agnostic (it's about block completeness, not pool math).

## Deferred / open

- **Multi-engine fan-out interaction.** `SolveCoordinator` fans out `on_drain`/`on_send` to
  every engine under the `drain_lock`. The per-block SM must live above the coordinator (one
  machine per block, shared across engines), not per-engine — otherwise a slow engine could hold
  the cursor behind a fast one. Design the SM as a sink wrapper, not an engine field.
- **Edge B RPC budget.** Edge B is one `eth_getLogs([N+1, head])` per dead-sub timeout. On a
  healthy node this is ~0 fires; on a laggy/dropping subscription it's the difference between a
  correct bot and a silently-trading-against-stale-state bot. Worth it, but needs a budget so a
  misbehaving node can't DOS the bot with perpetual reconciliation. Specific budget value
  deferred to implementation.
- **Timeout value for dead-logs-sub detection.** The window after which `newHeads(N+1)` with no
  logs for N+1 implies the logs sub died — deferred to implementation (tuned ~2× observed WS log
  lag, as in the design note).

## References

- `docs/architecture/block-state-machine.md` — full design note and rationale.
- `docs/adr/ADR-006-bot-as-per-chain-orchestrator.md` — `Bot` as state owner; the SM is the
  per-block clock layer above `SolveCoordinator`.
- `rust/crates/degenbot-bot/src/bot_core/block_pump.rs` (line refs in the design note) —
  current scattered-boolean invariants the SM replaces.