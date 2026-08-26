# ADR-038: The CL event-routing FSM — one table decides every event's fate

**Status:** Accepted
**Date:** 2026-08-25
**Task:** FUWYUR / epic U5DDC4 ("CL event-routing FSM")

## Context

A live V3 Mint for pool `0x88e6A0c2…5640` (USDC/WETH 0.05%) landed at block
25834714 while crawl was mid-flight and never reached engine state. The
tombstone-confirmed delivery was fine (no `[WS-INVARIANT]`), decode/apply was
fine (unit-tested), the dual-buffer machinery was fine (quarantine suites were
green) — yet the event vanished. Root cause: **an event's fate was decided in
three places, each holding a different partial copy of the policy**:

1. `LogDispatcher::dispatch`'s APPLY-MISS funnel: *unregistered ⇒ drop*
   (inferred as a perf shortcut — "skip the exclusive write lock for work that
   mutates nothing"). False for liquidity events: buffering IS the work.
2. `process_backfill_logs`: Mint/Burn → backfill buffer; Swap → dropped when
   unregistered (silent `None`).
3. Inline quarantine/unregistered arms inside `apply_v3/v4_*` on `BotState`.

Rust enforces exhaustiveness within one `match`; it cannot enforce agreement
across three scattered layers. The funnel's inference was exactly the gap.
Consequence chain: unregistered-window Mint → silently lost → late
registration pinned a DB row whose freshness stamp (`update_block = 25834714`)
outran its own content (the concurrent liquidity updater raced the Mint) →
verify passed against an anchor sharing the blind spot → Live with permanent
desync until ADR-021 tripped.

## Decision

1. **One exhaustive routing table.** `bot_core/cl_route.rs::route_action`
   maps `(Phase::{Backfill,Live}, PoolPresence::{Unregistered,
   Quarantined,Live}, EventKind::{ScalarRefresh,TickMutation}) → RouteAction
   {ApplyDirect, Buffer(BufferKind), Drop(NoOpReason)}` as a single match.
   Adding any axis variant is a compile error until every cell is filled —
   the compiler now polices what three layers used to negotiate informally.
   "Unregistered" is a first-class presence value, not an `Option` callers
   interpret themselves.

2. **Total outcomes.** Routing returns `ApplyOutcome {Applied(pool_id),
   Buffered(BufferKind), NoOp(NoOpReason)}`. Dropping requires naming a
   reason; today only `ScalarReseedAtRegistration` exists, and only for
   scalar-refresh payloads (their payload IS re-seeded wholesale from the DB
   row at registration — a documented trust assumption, not an inference).
   Tick mutations are never droppable anywhere in the table (asserted by
   test): tick data cannot be retro-supplied.

3. **BotState owns execution; callers are transport.**
   `BotState::route_v3_event / route_v4_event` resolve presence, consult the
   table, and execute. The dispatcher decodes, asks, executes the verdict,
   and reports telemetry — it holds no policy of its own. Its former fast
   path survives only as a **verdict-only pre-check**: it may skip the write
   lock solely when the table verdict is provably `Drop` (unregistered +
   scalar refresh), where skipping is the semantics. Perf patches may skip
   neutral work; they may never re-decide policy.

4. **Clock provenance.** Engine-witnessed horizons (`v3/v4_event_horizons`,
   advanced only by routed events, never by imported stamps) plus the
   tombstone cutoff bound what pin stamps can corroborate.
   `pin_provenance_verdict` classifies every pin; `SeedTrustOnly` with a
   non-zero witnessed horizon — the exact re-seed-after-activity lie shape —
   warns loudly instead of passing silently.

5. **Composition is tested, not assumed.** A seed-deterministic fuzz harness
   (`bamkki_routing_fuzz_oracle_holds_across_lifecycle_roles`) drives the real
   pump over randomized feeds × lifecycle roles with a replay oracle, so the
   whole bug family fails CI rather than production.

## Rejected alternatives

- **Full typestate pools** (encoding lifecycle in types per entry): the
  registry is a `HashMap`; typestate would force monomorphization sprawl and
  dynamic dispatch at every accessor without adding safety beyond the table.
- **Deleting the dispatcher fast path outright**: unnecessary — the
  verdict-only form preserves the lock-skip where it is semantically free.
- **Fixing only the funnel** (the minimal patch): leaves the other two
  policy copies to diverge again on the next family addition (Curve/Aave).

## Consequences

- New CL families extend ONE table; the compiler demands all rows.
- Buffer staging is observable as a distinct outcome (`Buffered`) rather than
  conflated into `None`; telemetry counters keep their meaning.
- The DB updater's stamp-honesty race remains open separately (FUWYUR
  follow-up): provenance classification makes such lies *visible*, but the
  updater should eventually stamp truthfully.
