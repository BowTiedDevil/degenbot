# Design note: per-block state machine for the pump's block clock

**Status: implemented (core) — see ADR-008.** Design note drafted through interactive review;
the accepted form is recorded in `docs/adr/ADR-008-block-state-machine.md`, which carries the
implementation-status section (what's landed vs deferred). Implementation is a separate body
of work.

## The problem this exists to solve

The pump has one cursor — `last_processed_block` (= `last_drained_block`, in
`solve_coordinator.rs:157`) — advanced by **two independent WS event sources** (`newHeads` +
`logs`, combined via `stream_select` at `bot_core/block_pump.rs:201`). The correctness invariant the
verify depends on is:

> **engine-state@cursor ≡ on-chain@cursor** on the liquidity dimension.

Today the cursor is allowed to advance to N+1 (on the `newHeads` edge) while block N's
liquidity log is still in-flight on the *other* WS stream. The V2-V2-V3 run crashed here
(block 25397049: a Mint at 25397047 was un-applied when 25397049's header advanced the cursor;
engine `update_block=25396861 < verify_block=25397049`). The verify-side "fix" in commit
`033a6b2f` (drop the redundant startup batch verify) only hides the race from the verify; it
does nothing for the solver/dispatch loop, which still runs against possibly-incomplete state.
This is a pump-correctness bug, not a verify bug.

The current code enforces the invariant (badly) with scattered booleans — `has_logs_this_block`,
`first_header`, `debounce_active`, `last_solved_block`, `current_block` — and `has_logs_this_block`
is *reset* on the next header advance, so a late log silently reopens a "closed" block with no
reconciliation. There is no single authoritative "what is the state of block N," so the invalid
transition is easy to fall into and hard to test for.

## What a state machine buys (and what it doesn't)

The win is not the diagram. The win is that a per-block state machine **forces you to answer an
epistemological question the current design dodges: how do you *know* block N's log stream is
complete?** A push WS subscription cannot tell you "no more logs for N" — absence of evidence is
not evidence of absence. So whatever the terminal "this block is done" state is, it can only be
reached via an *honest tombstone*: the first `removed: false` log for block N+1, because only a
successor block opening can prove N can't receive more logs.

A state machine earns its keep by making you *name the recovery edge as an explicit transition*
(backfill via `eth_getLogs`), because **absent that edge, it trades a silent race for a silent
stall** (cursor frozen forever on a genuinely-dropped log). Today there's neither the barrier nor
the recovery; the SM is what pushes you to admit you need both.

## Per-block state machine

One state machine **per block number N** the pump is tracking. A block is in exactly one state at
a time. The lifecycle is minimal — only states with *observable, non-timer triggers* are in the
machine.

```
                 ┌──────────────┐
                 │   Observed   │   ← newHeads(N) arrives OR first log with block_number==N
                 │              │
                 └──────┬───────┘
                        │ first log for N received
                        ▼
                 ┌──────────────┐
                 │ LogsArriving  │   ← ≥1 log received for N; more may come
                 │              │      solver MAY run here (fallible, accept re-solve
                 └──────┬───────┘      on straggler, downstream simulation discards superseded)
                        │ first removed:false log for N+1 (tombstone)
                        ▼
                 ┌──────────────┐
                 │  LogsApplied │   ← N is provably closed by successor opening
                 │              │
                 └──────┬───────┘
                        │ verify sealed
                        ▼
                 ┌──────────────┐
                 │   Drained    │   ← last_processed_block() is permitted to return N
                 │              │      ONLY in this state
                 └──────────────┘
```

### The settled transition table

| From | To | Trigger | Notes |
|---|---|---|---|
| (none) | `Observed(N)` | `newHeads(N)` OR first log with `block_number==N` | Either stream can open N. |
| `Observed(N)` | `LogsArriving(N)` | first log for N dispatched via `dispatch_log` | No log for N → never leaves `Observed`. |
| `LogsArriving(N)` | `LogsApplied(N)` | **first `removed: false` log for N+1** (the tombstone) | Only a real successor log closes N — not `newHeads`. |
| `LogsApplied(N)` | `Drained(N)` | verify completion for N | `last_processed_block()` may now return N. **Only `Drained` feeds the cursor.** |

### The two invalid transitions the current code makes (physically impossible under the SM)

1. **`Observed(N)/LogsArriving(N) → Drained(N)` on `newHeads(N+1)`** — the current empty-block
   finalize fast path (`bot_core/block_pump.rs:593-595`). The SM has no such arm. To reach `Drained(N)`
   you must go through `LogsApplied(N)`, which requires a real `removed: false` log for N+1.
   **A `newHeads` advance alone is not "the block is done."**
2. **Late `block_number==N` log reopens a closed block silently** — today `has_logs_this_block`
   is reset and `update_block` jumps backward. Under the SM, a late log on a `LogsApplied(N)`/
   `Drained(N)` block is handled by the asymmetric late-event path below, not silently applied.

## `LogsQuiesced` — a side-channel predicate, not a state-machine node

`LogsQuiesced` was initially drafted as a state between `LogsArriving` and `LogsApplied`. On
review it has no honest positive trigger as a state: a push WS has no "no more logs" event, and
"queue momentarily empty between two events" tells us nothing useful. So it does **not** earn a
slot in the block lifecycle above.

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

This is real (observable, not a timer), it's the right gate (prevents solving-against-queued-
state), and it doesn't pretend to prove completeness. The existing debounce timer — solver-to-
consumer release throttle — is a separate concern that consumes `LogsQuiesced` as an input but
does not drive state-machine transitions.

## Asymmetric late-event handling

A log with `block_number == N` arriving *after* N is `LogsApplied` (or `Drained`) means two very
different things depending on the `removed` flag. This is a **strong correctness stance: their
door, not ours** — a late log on a tombstoned block is either a chain reorg (handle via journal
restore) or a broken subscription (halt), with no tolerate-and-re-solve middle path.

### `removed: true` on a tombstoned block → reorg path

- A `removed: true` log on a `LogsApplied`/`Drained` block enters the **reorg path**.
- **Reorg-window event handling:** nodes may deliver reorg events in any order (reverse log
  index, reverse block number, unordered) — we make no ordering assumption. We accept the
  contiguous chunk of `removed: true` events.
- **Reorg-window close:** gated on the **first `removed: false` event received after entering
  the reorg path.** That event's block is the new head; forward tracking resumes monotonically
  from there. No separate resumption-point selection.
- **Pool state restore:** restore via the existing `ReorgCoordinator::dispatch_reorg_log` /
  `BotState::restore_before_block` machinery to the common ancestor (the deepest `Drained` block
  that survives the reorg). Rewind the per-block machines of unwound blocks to `Observed(N-X)`
  and re-track forward. If the reorg is too deep (`NoStatePriorToBlock`), graceful shutdown — the
  cursor never regresses past a `Drained` block.

### `removed: false` on a tombstoned block → panic/shutdown

A forward log on a tombstoned block (not in the reorg path) means WS delivery is unreliable
(out-of-order or duplicated forward events), not a reorg. This is unrecoverable for correctness —
we can't trust any tombstone the WS gave us. Panic/shutdown.

## Trigger clarifications (settled)

- **Tombstone trigger for `LogsApplied(N)`**: the first log (not `newHeads`) with
  `block_number == N+1`, `removed: false`.
- **`newHeads` role**: liveness probe for the logs subscription only; never a completeness
  signal on its own. A `newHeads(N+1)` header arriving with no logs for N+1 following within a
  window may indicate the logs subscription died — fire Edge B backfill (below).
- **Reorg-window close**: first `removed: false` event after entering the reorg path (block
  becomes new head).
- **Solver-runnable window**: `LogsArriving` (and onward). Solver results released to consumers
  only when `LogsQuiesced` predicate is true.

## Dead logs subscription → Edge B backfill

The logs subscription may die while `newHeads` stays alive. This is **self-healing in one sense**:
no forward state transitions occur (no logs arrive → no pool state mutates → nothing corrupts,
just goes stale). The recovery is Edge B backfill.

- `newHeads(N+1)` arrives but no log for N+1 follows within the timeout window → logs sub
  suspected dead → fire Edge B backfill.
- **Backfill range starts at N+1**, *not* N. We only tombstone a block on positive receipt of an
  event on the next block, so if N is `LogsApplied`, the logs sub was alive at least through
  N+1's first event. The dead-sub gap can only start *after* the last successfully tombstoned
  block. **N stays sealed.**

### Backfill taint + replay semantics

- Fetch `eth_getLogs([N+1, head])` authoritatively.
- **Taint range = `[N+1, head]`** — these are the blocks we never proved complete. Mark every
  block in the range that was `LogsApplied`/`Drained` as `Tainted`, restore pool state to its
  end-of-N state (the common ancestor), and replay forward.
- **Single-branch replay — no fast path.** The backfilled logs flow through the *same* state
  machine as live logs: N+1 goes `Observed → LogsArriving → LogsApplied` as logs are fed in. The
  state-machine overhead is small and one branch means one set of invariants to test. During a
  fast multi-block replay the `LogsQuiesced` predicate churns (in-flight counter hits 0 between
  events then re-enters on the next), and the solver-release gate opens only at the genuine tail
  when no more events are queued — exactly the same mechanics as a live block. The `LogsApplied`
  tombstone arrives via the first `removed: false` log for the block after the backfill range.

## Bootstrap (backfill) edge

`backfill_range` (`bot_core/block_pump.rs:763`) is the same mechanism as the recovery Edge B above — it
fetches `eth_getLogs([from, to])` and dispatches each log in order through the state machine.
Under the SM, the backfill path is not a distinguished `Backfilled → Drained` edge; it is the
same `Observed → LogsArriving → LogsApplied → Drained` lifecycle, fed by RPC-fetched logs rather
than WS-pushed logs. The distinction "WS contribution is racy, RPC contribution is authoritative"
is *not* structural — both flow through the same machine, because the machine's invariants
(tombstone via successor, verify-sealed `Drained`) are what establish correctness, not the
source of the log.

## What this prevents (the invalid transitions, made impossible)

| Invalid transition | Current code path | Why the SM forbids it |
|---|---|---|
| `newHeads(N+1)` advances cursor past N while N's logs are in-flight | empty-block finalize (`:593-595`) + `finalize_if_dirty` on header | No `Observed/LogsArriving → Drained` arm. Cursor advance requires `Drained(N+1)`, which requires `LogsApplied(N+1)`, which requires a real `removed: false` log for N+1 — a header alone is neither. |
| Late `block_number==N` log reopens a closed block | `has_logs_this_block` reset on next header; `update_block` silently jumps backward | Handled by the asymmetric late-event path: `removed: true` → reorg path; `removed: false` → panic/shutdown. Cursor never silently regresses. |
| Verify sees a "drained" cursor whose block isn't actually applied | `last_processed_block()` returns `last_drained_block` set by `on_drain`, gated only by `has_dirty_paths()` | `last_processed_block()` is specified to return N only when the per-block machine for N is `Drained`. |
| Solver releases results against queued-but-unapplied logs | Debounce timer fires on schedule regardless of queue state | Solver-release gate requires `LogsQuiesced` (in-flight counter == 0). |

## What this does NOT supersede

The per-pool pin gates (step-1 / step-2 snapshots, V3 + V4 twins) **complement** the SM, they do
not replace it:

- The pins are the **verify's** defense (frozen-block comparison, immune to whatever the cursor
  is doing).
- The SM is the **pump's** defense (the cursor itself can't advance past un-applied logs; the
  solver can't publish against un-applied state).

They defend different things and should both exist: the pins catch a bug the SM *should* have
prevented but didn't (defense in depth); the SM prevents the solver from running against
incomplete state in the first place (the thing the pins can't help with, because they only run at
verify time, not solve time).

## Open questions for the design review

1. **V4 parity.** V4 uses the same pump + state machine; the SM is family-agnostic (it's about
   block completeness, not pool math). V4 verification already pins step-1/step-2 via the
   `post_drain_snapshot` (twin of V3's), so the cursor-correctness fix transfers without
   family-specific work. Recording it here so nobody reads this as V3-only.
2. **Reorg + cursor interaction under multi-engine fan-out.** `SolveCoordinator` fans out
   `on_drain`/`on_send` to every engine under the `drain_lock`. The per-block SM must live above
   the coordinator (one machine per block, shared across engines), not per-engine — otherwise a
   slow engine could hold the cursor behind a fast one. Design the SM as a sink wrapper, not an
   engine field.
3. **Cost.** Edge B is one `eth_getLogs([N+1, head])` per dead-sub timeout. On a healthy node
   this is ~0 fires; on a laggy/dropping subscription it's the difference between a correct bot
   and a silently-trading-against-stale-state bot. Worth it, but needs a budget so a misbehaving
   node can't DOS the bot with perpetual reconciliation.