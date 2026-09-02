# detached solve cycle decision (epic SRQEK5, tasks WV62TX + 4QKZE3 + SF3QLP)

## Context

Epic BXZBWY shipped streamed solve orchestration (dedicated tokio solve runtime +
per-path delivery) but kept ONE engine-Mutex hold spanning the whole solve cycle
— for a heavy 1700-path cycle that hold is multi-second, and `register_path`/
`deregister_path` (GIL-released, concurrent with the pump) serialize behind it.
The offline detach-strategy A/B probe (commit 881c098f5, heavy-CL corpus,
156 paths x 5 passes) measured the merge seam directly:

| disposition | engine-Mutex hold | cycle return | makespan |
|---|---|---|---|
| in-cycle (baseline) | 2370-3812 us (one hold) | makespan-equal | equal |
| **detach-always** | **9-18 us (per-result)** | **60-170 us (enqueue-end)** | **equal** |
| hybrid 2x-median budget | 141-155 us | budget-expiry | degenerates to detach |

Makespan-equal at every thread count + an order-of-magnitude hold win =
detach-always (no fast/slow tier; the budget arm was recorded degenerate).

## Design (user-approved 2026-09-02)

The detached cycle is an **enqueue-and-return** contract: the enqueue half keeps
the resolve/gate/deferred-path bookkeeping, the SOLVES run on a plain
`std::thread` per LPT bin (a scoped rayon install JOINS its tasks and can starve
against a held `parking_lot` guard — the deadlock diagnosis in the task record),
and each result flows through an unbounded mpsc to the merge sidecar, a plain
`std::thread` that applies each straggler under the engine Mutex.

## The five theses (all match the shipped code)

1. **Stale policy (Q1a): apply-if-unchanged, drop-on-touched.** Each item rides
   its enqueue-time per-hop `pool_update_block` stamp; `merge_detached_item`
   re-reads the LIVE pool clocks at merge: ANY advance (a swap, or a
   price-neutral liquidity event — V3 Mint/Burn, V4 `ModifyLiquidity` both bump
   `update_block` AND re-classify the pools dirty) drops the straggler
   pre-insert. A de-registered path drops too. Code:
   `solver_dispatch.rs::merge_detached_item`; tests:
   `detached_straggler_with_stale_update_stamp_is_dropped`,
   `detached_straggler_after_deregister_is_dropped`.
2. **In-flight: cap 8, self-consistent gauge.** Bin threads bump the gauge at
   SEND time, the sidecar decrements per terminal disposition — a bin that dies
   before sending never leaks a count. Code: `detached_outstanding`
   (`Arc<AtomicU64>`), `DETACHED_INFLIGHT_CAP = 8`.
3. **Backpressure: degrade, never block.** At cycle start, in-flight >= 8
   degrades THAT cycle to the pre-epic in-cycle path (the fallback thesis) —
   the engine mutex is released only at enqueue-end, so waiting on the sidecar
   while holding it would deadlock by construction; in-cycle fallback instead.
4. **Observability:** hotpath labels `arb_solve.detached_enqueue` /
   `arb_solve.detached_merge`; metrics `degenbot.detached.in_flight` (gauge),
   `.stale_dropped`, `.applied` (counters; stubbed no-ops in the no-otel
   build). In-cycle counters/labels unchanged for the fallback mode. The
   ADR-021 publish tripwire is UNCHANGED and stays the correctness backstop:
   an APPLIED straggler re-extends `last_solved_path_ids` at merge so the next
   publish's verifier diff covers late merges (pinned by
   `adr021_detached_stragglers_stay_scoped_to_the_publish_verifier`); a
   stale-DROPPED straggler reaches the publish path never.
5. **Fallback:** `DEGENBOT_DETACHED_SOLVES` unset/0 = byte-identical pre-epic
   in-cycle code path (the detached arm is skipped entirely);
   `detached_off_merges_synchronously_inside_the_call` pins the sync merge.

## Policy matrix

| axis | values | shipped default | effect |
|---|---|---|---|
| DEGENBOT_DETACHED_SOLVES | 1 / unset | enqueue-and-return sidecar cycle / in-cycle single-hold cycle | epic SRQEK5; default flips with the soaked readout |
| DEGENBOT_STREAMING_DELIVERY | unset / 0 | streaming (per-path micro-batches) / debounce sweep | T3 default flip; A/B opt-out keeps the debounce sweep; the sweep still owns expired/removed + end-of-cycle metadata either way |
| DEGENBOT_SOLVE_EXECUTOR | tokio / rayon | tokio / per stance docs | INDEPENDENT axis: governs the IN-CYCLE arms only; detached solves are per-bin std::threads |
| in-flight gauge | 0..8 | >= cap at cycle start degrades that cycle to in-cycle | backpressure thesis |
| Q1a oracle | stamp vs live clocks | mismatch or deregistered => drop | stale policy thesis |

## What a detached cycle guarantees

- `rebuild_and_solve_affected` RETURNS at enqueue-end (results_block stamped at
  the solve block); drift between enqueue and merge is bounded by the sidecar
  drain, which re-acquires the engine Mutex per item (the
  register/deregister serialization invariant holds — per-item acquisitions
  take the same Mutex).
- A straggler result lands in `results` iff its intake is UNCHANGED live (
  oracle above) — otherwise it is dropped and the world stays consistent (the
  touch that invalidated it re-marks the pools dirty, so the next cycle
  re-solves fresh).
- The Python consumer contract drifts NOTHING: the result channel still only
  ever carries clamp-passed above-threshold batches (streaming micro-batches
  per solve completion + the end-of-cycle debounce sweep).

## What we measured (10.2-min mainnet soak, dry-run, shipped stack, 2026-09-02)

- Blocks 25892125-25892175 (51 blocks); 98 detached cycles; 3686 of 3712
  dispatch phases = **99.3% single-candidate batches** (per-path micro-batches
  with streaming DEFAULT-UNSET — the flag flip itself validated live); the
  multi-candidate remnants are the end-of-cycle debounce sweeps + startup
  accumulation (6..109 candidates).
- 86 Q1a stale drops (pools genuinely moved between resolve and sidecar merge);
  0 error/panic/abort lines; ADR-021 publish tripwire silent for the whole run.
- Evidence: logs/t3_soak_batch_shapes.csv, logs/t3_soak_summary.json,
  logs/t3_soak_hotpath.json (hotpath timed-exit report), logs/bot_run.log.

## Allocator-lane combined soak (2026-09-02 21:15Z, the SF3QLP closer)

The COMBINED config — `DEGENBOT_STREAMING_DELIVERY=1` EXPLICIT +
`DEGENBOT_DETACHED_SOLVES=1` — re-soaked from the allocator terminal on a
fresh log segment (zero contamination): 10.4 min live mainnet, blocks
25892238-25892288 (51 blocks), **3966 of 4000 dispatch phases = 99.15%
single-candidate micro-batches**, 96 detached cycles (enqueue-end + sidecar
merge every cycle), 0 Q1a drops this window, 0 errors, publish tripwire
silent. Evidence: logs/t3_lane_batch_shapes.csv, logs/t3_lane_summary.json,
logs/t3_lane_hotpath.json. The decision table shape is unchanged on live
data from both lanes; the deferred cross-cycle straggler rejection analysis
has no trigger.

## Watchdog + publish-verifier audit

- B3 frozen-drainer backstop: detached drain items COMPLETE at enqueue-end
  (return is enqueue-end, not apply-end), so no stall accrues across cycles —
  pinned by `detached_stragglers_do_not_trip_the_frozen_drainer_backstop`
  through `SolveCoordinator`-in-`DispatchOwner`, the production cadence; the
  synthetic-freeze abort pair (`no_progress_frozen_drainer_aborts_proc` et al.)
  stays green in the same suite.
- `EngineHandle::solve_dirty`'s hold-time invariant now documents the detached
  exception: the hold collapses to enqueue-end (~us) while the stance is ON;
  register/deregister serialization is preserved by the per-item sidecar
  acquisitions (engine_handle.rs).
