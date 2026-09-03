# ADR-039: State-lock — enumerated sim-anchor projection (K4ETHF)

## Status

Accepted (2026-09-03, epic K4ETHF; adversarially pair-reviewed — review incorporated).

## Context

The overnight + diagnostic soaks attributed the recurring ~2-3s exclusive holds of the core
'BotState' lock to 'SimAnchorState::snapshot' (dispatch.rs:638 read guard; evidence:
logs/state-lock-holder-attribution.md, logs/state-lock-impact.md). Two algorithmic defects made a
"O(pools), no tick data" projection cost seconds per block:

1. **V3 full tick scan.** The snapshot consulted a generic arbitrary-key query interface
   ('BotState::probe_tracked_storage_slot(address, index)') for fixed indices [0,4,8]. A V3 pool at
   index 8 (V2-reserves-only) fell through to the per-tick reverse map — iterating and
   keccak-hashing the pool's ENTIRE tick map per pool per block — for a guaranteed miss
   (tick-slot words are 256-bit keccak domains; matching literal 8 has ~2^-248 probability).
2. **V4 O(V^2).** Each V4 probe call reverse-maps ALL v4_pool_ids computing a keccak each; the
   snapshot called it 2x per V4 pool. The single canonical PoolManager means no PM filtering.

Under the core read lock, a queued writer then trips parking_lot writer-pending fairness: pump log
applies, solve gate, Python with_state readers/writers, and the solve-phase resolve all stall —
measured header_to_solved p95 5.0s, log_burst p50 2.5s, ~35% of wall time held.

Pair-review sharpened the root: **an arbitrary-key lookup interface was reused as a fixed
projection.** That mismatch is the recurrence generator.

## Decision

D1 — **Enumerated projection, not a patched query.** Snapshot becomes an explicit per-family
projection: the tracked surface is a fixed, audited set (V2 -> [slot 8], V3 -> [slot 0, slot 4],
V4 -> [S_state, S_state+3]); the projection lives beside the pack helpers in 'divergence_probe.rs'
(new 'BotState::project_sim_anchor_scalars') so word packing has a single source with the env-gated
probe (the serving.rs packing-duplication point).

D2 — **The live query interface keeps its tick descent.** 'BotState::probe_tracked_storage_slot'
is UNCHANGED: the diagnostic machinery (divergence_probe.rs:709/:726 live-probe tests) stays
intact; production cost there remains zero (env-gated callers only).

D3 — **No S_state memo at registration.** The inline projection derives the V4 base once per pool
per block (O(V) keccak, ~0.5ms at current partition sizes); a registration-time cache adds a field
for no measurable win at this scale. Revisit only if V4 counts grow ~50x.

D4 — **Recurrence guard (ADR invariant).** 'snapshot = audited enumerated surface, never an
arbitrary-key query.' Any new anchor slot must be added to the projection as an enum entry with a
test — never serviced through probe.

## Alternatives considered

- **B. Fully background registration worker** (never inline registration with the pump): deferred —
  the measured holder is the snapshot, not registration itself (its own acquires were waiters).
  Revisit if post-fix registration writes show >250ms holds (T6 soak).
- **C. Chunked buffer drains**: aimed at H2 (refuted as primary holder); no action now.
- **D. Sharded / per-pool locks**: highest blast radius; unnecessary once the hold is ms-scale
  (ADR-037 engine-mutex sharding already covers the engine side).
- **E. Deprioritize registration**: traffic-shaping lever, orthogonal; re-assess after T6 delta.

## Invariants preserved

- Engine-mutex/'BotState' ordering (ADR-003) — no lock-scope change; the projection runs INSIDE the
  existing short read.
- GIL/'BotState' inversion discipline — no new Python boundary crossings.
- SimAnchorState consumers observe byte-identical words (T5 parity test).

## Named residual (follow-up task)

PyTickWordFetcher holds the with_state_mut WRITE across serial per-tick RPC (pool.rs:729; ergo task
RATR5A in epic K4ETHF). Latent, same class; not surfaced in this window's tape.

## Verification

T5 TDD (red perf gate 450ms vs 150ms bound; parity incl. negative half), committed in:
- a0f941d57 - perf(state-lock): enumerated sim-anchor projection (K4ETHF T5, ADR-039)
- 296f36163 - lint fixes from pair review

Tests (names as recorded in the tree):
- bot_core::sim_anchor::tests::snapshot_parity_with_query_semantics - anchor_words byte-identical
  to the query-interface semantics on per-family valid indices + negative half (out-of-family
  probes absent, e.g. V3 literal slot 8 / V2 slots 0/4)
- bot_core::sim_anchor::tests::snapshot_is_enumerated_not_a_scan - heavy fixture
  (120 V3 pools x 1800 ticks + 200 V4 pools) under the 150ms bound (red measured 450ms pre-fix)
- bot_core::divergence_probe 13/13 (incl. the :709/:726 live tick-descent probes - untouched by design)
- bot_core::state_lock 15/15 (drop-time forensics + telemetry taxonomy)

Pair review: adversarial post-landing pass APPROVED (projection scoped at the enumeration level,
not the query interface; no S_state memo; the fetch-under-write risk split out as task RATR5A).

T6 soak delta gates pre-registered in logs/state-lock-impact.md; met in the 17min interim soak and
re-confirmed at the full 1h soak (see the impact doc delta tables).