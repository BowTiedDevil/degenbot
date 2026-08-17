# Active Block Window — pump-owned block promotion (QMSTSV)

**Status: decision (confirmed).** Resolves the blocked decision fork in
`MQIZ5M` (Option B) and the architectural question in `QMSTSV` (the pump owns a
pump-promoted **active block window**). Maintainer confirmed: newHead is the
promotion signal; `active_block = max(newHead, drained_block)`. This document
records the settled design so the implementation can be sliced behind the
already-landed tripwires.

## TL;DR (the decision)

**Yes — block promotion should be a pump-owned lifecycle transition, and it is
exactly `MQIZ5M` Option B.** The pump owns a single promoted **`active_block`**
that BOTH the solver and the sim derive from, replacing the three scattered
`max(_, pool_state_head)` re-derivations. Promotion is **newHead-driven**:
`newHeads(N)` promotes the active block to N. Because a push WS cannot prove
"the last event for N arrived", the new block *is* the promote signal.
`active_block = max(newHead_promoted, drained_block)` — the state clock
(`drained_block`) only catches the pump up on a header stall so state never
outruns the solve anchor. The promote gate and the publish gate are distinct:
`active_block` advances optimistically on newHead, while the final publish stays
log-closed (quiesce-gated) and re-solves on stragglers. The promoted object is
*a numeric label + a lazily-built revm view* — **never a second authoritative
state**. `BotState` remains the single source of truth; the EVM window is a
derived projection.

## Context (traced)

The pump advances two independent clocks:

1. **Header clock** — `current_block` in `run_with_stream` (`block_pump.rs`),
   advanced by `newHeads` + `logs` events.
2. **State clock** — `pool_state_head()` (`bot_core/mod.rs:630`), the max
   `update_block` across all pools. No single owner "decides" it; it is a
   derived maximum.

A drain (`SolveCoordinator::on_drain`) passes `current_block` to every engine
→ `solve_dirty(current_block)`. The solver then **re-anchors to the state
clock** because a hop's `update_block` can exceed the lagging header clock:

- `solver_dispatch.rs:109` — `solve_block = max(block_number, pool_state_head())`
- `block_pump.rs:609` — verifier `anchor = max(block, pool_state_head())`
- `dispatch.rs:593` (`degenbot-arbitrage`, the Python-driven sim) —
  `sim_block = max(candidate.solve_block)`

So "solve for a block", "verify at a block", and "simulate at a block" are
**three independent derivations of the same quantity**, welded together by a
`max()` that only holds because "an unchanged pool has byte-identical state from
its `update_block` to head" — a coincidence of the data, not an architectural
guarantee. Every time a maintainer must remember "and the sim must anchor to
head, not the pump clock", they are reconstructing an invariant the structure
should already enforce. `MQIZ5M`'s +1-wei / IIA class is the failure of this
invariant under a header stall (pools advanced 12 blocks past `current_block`).

## Promotion semantics (newHead-driven, publish log-closed)

The active block window has **two gates** that must not be conflated:

1. **Promote gate (`active_block`) — newHead-driven.** `newHeads(N)` promotes
   the active block to N. This is the solve+sim target. It advances eagerly on
   the header notification because the last-event arrival is unknowable via a
   push WS (the same principle ADR-008 D1 states: *"a push WS cannot prove no
   more logs for N"*).
2. **Publish gate (`drained_block` / cursor) — log-closed.** Only the first
   `removed:false` **log** for N+1 (the D1 tombstone) closes block N for
   state-completeness; only `Drained` feeds `last_processed_block()`. The final
   publish is quiesce-gated (ADR-008 D2). A newHead-promoted solve during
   `LogsArriving` is **fallible**: straggler events trigger a re-solve, and the
   sim discards intermediate results. This is ADR-008's existing eager
   solve-on-`LogsArriving` semantics, not new machinery.

The distinction matters: newHead is a **promote/liveness** signal, never a
**completeness** signal. That is the full reason ADR-008 demoted the header for
state-completeness — and it still holds here at the publish layer.

### The stall case — the ONE pump-owned `max`

`active_block = max(newHead_promoted, drained_block)`. newHead is the driving
signal in normal live operation. `drained_block` (the state clock) only **catches
the pump up** when a header stall lets ordered backfill advance state past the
stalled newHead-driven block (MQIZ5M's exact 12-block-ahead scenario). This is a
single central `max` at the promote point — not three scattered
`max(_, pool_state_head)` re-anchors — and it makes `pool_state_head ≤
active_block` an **invariant** (backfill applies block-by-block in order), so
future-price solves are impossible by construction.

## Why pump-owned promotion is right

The pump is the only component that knows *when a block is sealed* — it owns
the ADR-008 lifecycle (`Observed → LogsArriving → LogsApplied → Drained`), and
only `Drained` feeds `last_processed_block()`. Liveness (newHead promotion) and
state-completeness (drain) are both pump-owned concerns. Block release is
therefore a job only the pump can do, and it is currently being done implicitly
and redundantly by three `max()`s and the header-fed `current_block` variable.
Promoting once at the drain/settle point:

- makes solve, verify, and sim take the **same object** instead of three
  re-derivations that must be manually kept in agreement;
- collapses `dispatch.rs:593`'s `max(candidate.solve_block)` — every candidate
  already solved at the promoted block, so the batch max *is* the promoted
  block;
- single point to reason about what block we are solving for, testable in
  isolation.

## Resolves MQIZ5M's A/B/C fork → **B**

- **Option A (clamp backfill ≤ `current_block`)** defers state but *does not*
  eliminate the A/B problem: during a long header gap it stalls the pump, and
  the skipped mined blocks cannot be backrun anyway, so the deferral is wasted
  work. It is also a **backfill-behavior** change that couples a WS-liveness
  failure into pool-state admission.
- **Option B (promote `current_block` to the backfilled head)** is the correct
  semantics: **you cannot backrun a block you have passed**, and state that has
  advanced to head *is* the chain's current state — so solving + simming at
  head is correct, not a "future price". Realization (confirmed): newHead is the
  **driving promote signal** (`newHeads(N)` → `active_block = N`), and
  `active_block = max(newHead_promoted, drained_block)` so a header stall never
  lets the state clock outrun the solve anchor. Active-block promotion is
  eager/optimistic; publish stays log-closed (quiesce-gated).
- **Option C (per-block snapshots)** is exact but expensive and contradicts the
  I/O-free / single-source-of-truth architecture.

**Recommendation: Option B, realized as the pump-promoted active block window.**

## The residual race — promotion alone does NOT fix it

While tracing, I confirmed that the **deeper** trigger of the `MQIZ5M` IIA class
is a *mid-solve state mutation*: `pool_state_head()` is a moving target, and
backfill can advance a pool **between** the pump reading the head and the solver
finishing. Promotion removes the *labeling* inconsistency (solve/sim both at one
promoted block) but cannot freeze `BotState` mid-solve.

That residual is handled by the already-landed tripwires, which must be **kept**
as the fail-fast race net:

- `U6RNHH` — solve-stage future-price tripwire (`is_future_value` rejects a hop
  whose `update_block` exceeds the solve anchor).
- `TQ43TU` — solve-time staleness gate (defers a hop whose price clock trails
  far behind).

Under a correct promotion these tripwires become *near-impossible* (the promoted
block is `max(update_block)`, so no hop can exceed it by construction) but they
remain the guard for the mid-solve-advance window. **Do not remove them with the
promotion; the promotion makes them the exception, not the rule.**

## The guard — no second authoritative state

The earlier design discussion (the concern about "materializing a second
authoritative state") is honored: the active block window is

- a **numeric label** (`active_block: u64`), and
- a **lazily constructed revm view** of `BotState`, built on demand at sim
  time (the existing `BlockSimHandle`), **not** eagerly materialized and **not**
  dual-tracked.

`BotState` stays the single authoritative state owner (ADR-003). We do not build
a backwards-compat layer, and we do not introduce a rival state mirror.

## Concrete implementation touch-points (for the slice)

1. `block_pump.rs` — introduce the pump-owned `active_block`. **newHead** event
   promotes it (`active_block = N`); the state clock catches it up on a stall
   (`active_block = max(active_block, drained_block)`). Use the promoted block
   for `on_drain`/solve/verify in place of the raw header-fed `current_block`
   variable (which is demoted to a liveness/ordering probe only) and the
   one-shot `anchor` at `:609`.
2. `solver_dispatch.rs:109` — consume the promoted `active_block` (threaded as
   the solve anchor) instead of re-deriving `max(block_number, pool_state_head())`.
3. `solver_state_verifier.rs:297` — anchor to the promoted `active_block`.
4. `dispatch.rs:593` (`degenbot-arbitrage`) — `sim_block` becomes the
   promoted block threaded through `candidate.solve_block`; the per-batch `max`
   collapses (keep `max` as a defensive invariant assertion, not a re-anchor).
5. Thread the promoted block through the existing solve_block → DispatchCandidate
   → dispatch path (the FFI seam is unchanged; this is a value, not a type).
6. Publish path unchanged in shape: keep the quiesce gate (D2) + D1 log
   tombstone for `drained_block`; straggler events re-solve, sim discards
   intermediates (existing ADR-008 semantics).

## Validation gates

- **RED unit test (the acceptance gate):** pump promotes `active_block` on a
  newHead event; `on_drain`/solve/verify/sim all receive the **same** promoted
  block == latest newHeads; and `pool_state_head ≤ active_block` holds
  throughout.
- **Stall test:** drive a backfill-ahead-of-header desync and assert
  `active_block` is caught up to the drained/state clock by the single
  pump-owned `max` (never solving below the state).
- **Publish test:** a newHead-promoted solve during `LogsArriving` does NOT
  publish until quiesced; a straggler re-solves and the intermediate result is
  discarded.
- Keep `U6RNHH`/`TQ43TU` regression nets green (they must still fire on a
  mid-solve advance in the absence of promotion).
- Per-`MQIZ5M`: a live-node re-run proving no future-price solve during a
  backlog after the promotion lands.

## Non-goals

- Not a change to the FFI seam or a new Python mirror.
- Not the `UO3JM4` take-overdraw discriminator (that was resolved separately;
  its residual points at this same stale-state class, so it is the operational
  payoff of landing this).
- No `Option A`/`C` implementation, no per-block snapshots, no elimination of
  the tripwires.
