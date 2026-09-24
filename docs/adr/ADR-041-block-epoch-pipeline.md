# ADR-041: The block-epoch pipeline — one stage machine over a cheap-read data plane

**Status: implemented** (2026-09-07, ergo epic `MROOY7` / task `PLRGIN`).
Settled in the originating architecture conversation, recorded here so every
later task can execute without it; the user checkpoint on this ADR was
sign-off of the stage table and seam retirement list (canonical form in
[the design doc](../architecture/block-epoch-pipeline.md)). Implemented across
the epic's tasks and validated by the final-integration gate: capture-replay
regression sweep (zero divergences; see PLRGIN result) and the live Jaeger
soak A/B against the pre-epic operator baselines
(../architecture/stateview-feasibility.md §3). The unified `StageMachine`
lives at `rust/crates/engine/degenbot-bot/src/bot_core/stage_machine.rs` (ergo
`7NFYQW`); the retired `DrainSink`/`Engine`/`SolveCoordinator`/
`DispatchOwner`/`DirtySets`/`EngineSubscriber` seams are gone (SZJUKL).

## Context

The per-block loop of the arb engine is currently choreographed by **six
correlated state machines** plus a parallel accounting system built around
them:

- `EnginePhase`, `BlockClock` (ADR-008), `PumpFSM` (ADR-028), the registration
  verify-lifecycle (ADR-022), the path lifecycle, and the delivery lifecycle —
  living in `rust/crates/engine/degenbot-bot/src/bot_core/{block_pump.rs,pump_fsm.rs,block_clock.rs}`
  and `rust/crates/engine/degenbot-bot/src/arb_engine/{mod.rs,lifecycle.rs,path_lifecycle.rs,delivery_lifecycle.rs}`.
  Each is pure (the ADR-028 producer/driver family), but their interaction is
  opaque: which machine may advance in a given block, and in what order, has
  no single owner.
- A parallel accounting layer — the `DrainSink`/`Engine` dual seam,
  `SolveCoordinator` with its `drain_lock`, the `DispatchOwner` + `DrainWork`
  FIFO, `DrainerHealth`, the `DirtySets` + `EngineSubscriber`
  classification, and a **soup of anchors** (solve/sim/verifier/backfill
  anchors, `last_solved_block`, `last_drained_block`, `BlockMetadata`) that
  each carry an answer to "what block is this work about?".
- The engine `Mutex` sits on the solve path (sharded by ADR-037 for
  *contention*, but still an ordering constraint:
  `drain_lock → engine Mutex → BotState RwLock`).

The state-accuracy tripwire (ADR-021) exists largely because nothing
structurally prevents a desynced snapshot from reaching the solver. A design
in which the data plane's writers are stage-confined makes the in-process
half of that concern *unrepresentable* rather than merely detected.

An ergo spike (`KWKEVV`) already settled the data-plane mechanism question
(see the stateview feasibility doc recorded by that spike; canonical copy on branch `pi-fabric/stateview-spike`).
This ADR records the pipeline architecture that spike unblocked.

## Decision

### 1. One pipeline stage machine per block epoch

A single stage machine owns every per-block edge condition. The canonical
stage sequence is:

**Streaming → Quiesced → Resolved → Solved → Simulated → Gated → Published →
Finalized**, plus **`Rewind{to_epoch}`** on reorg.

`Rewind` is a transition from any stage to a fresh epoch at an earlier block;
the epoch's `seq` bumps and stale contexts fail fast. Full stage
responsibilities, data-plane posture, and absorbed sub-state are specified in
[the design doc](../architecture/block-epoch-pipeline.md).

The six existing machines **fold in as sub-state** of the stage machine;
their pinned tests are the behavioral contract and are ported verbatim.
Runtime decisions are dispatched through a single **`StageHandlers`** seam —
the pure per-stage function set the runtime drives — completing the ADR-028
pure-producer/thin-driver pattern at pipeline scope: `BlockPump` becomes an
event+tick feeder and decision executor only. Its reorg-episode and resume
tracking (`block_pump.rs`) folds into `Rewind` handling, and its WS transport
+ watchdog machinery later moves to `degenbot-ingestion` (epic task `5WTYYQ`).

### 2. The data plane survives as cheap-read (spike `KWKEVV` outcome)

**Cheap-read (mechanism (a)) for every family**: V2-family scalars, V3
tickmaps (Uniswap/Pancake/Sushi), and V4 tickmaps. No materialization, no
COW. Concretely:

- `StateLock<RwLock<BotState>>` stays the data plane. **Writers are confined
to the Streaming stage.** Quiesced, Resolved, Solved, and Simulated hold read
guards; with the engine `Mutex` off the solve path, those reads are
uncontended by construction.
- Measured basis (full method and repro in the feasibility doc):
  90th-percentile CL tickmap density is 2–4 ticks (registry max 1,536); a
  full tickmap clone is 40–50 ns at p90 density and 16–19 µs p99 at the
  registry max; journal-replay of a full 32-block rewind is ≤ 23 µs p99 even
  at 8 priors/block; `Arc` COW view construction is 30 ns — all ~10⁵ under
  the 2 ms view-construction leg. The cheap-read exception leg did not
  trigger: per-path solve p99 is 5 ms and the solve gate alone 1 ms (≪ the
  100 ms threshold), and live `state_lock_wait` p99 is ≤ 0.1 ms across 19.6 M
  acquisitions — measured on the *still-contended*, pre-stage-machine
  architecture. Since cheap-read only loses if **both** rule legs pass,
  cheap-read wins outright for all families.
- Sizing for `Rewind`: the `restore_before_block` bounds (≤ 23 µs p99 V3 at
  depth 32; 20–30 ns V2) mean no view machinery is needed to hit the reorg
  budgets. StateLock hold/wait histograms are retained as the budget
  verifier. One out-of-corpus caveat is recorded: whole-map clone goes
  superlinear above ~131k entries (≥ 16 MB working sets, allocator/TLB) —
  irrelevant at today's registry max of 1,536 ticks/pool.

### 3. Seam retirement

The parallel accounting system is **deleted**, not wrapped (hard cutover,
Q6). Exact list, with disposition:

1. **`DrainSink`/`Engine` dual seam → one `StageHandlers` seam.** The pump's
   executor fan-out (ADR-028 addendum (c)) becomes one trait; delivery and
   submission subscribe at the Published edge as sinks, not as a dispatch
   seam in front of the engine.
2. **`DirtySets` + `EngineSubscriber` classification → `EpochDelta`.** Log
   application records touched pools as a byproduct (the delta rides the
   `BlockContext`); affected-path derivation reads the delta directly behind
   a capture-replay parity gate.
3. **`SolveCoordinator`, `drain_lock`, `DispatchOwner`, `DrainerHealth`
   dissolved.** They accounted for work the stage machine now owns; the
   no-progress watchdog obligation moves onto the machine's watchdogs.
4. **Engine `Mutex` off the solve path.** ADR-037's sharding made it
   low-contention; stage confinement removes it as an ordering constraint
   entirely. Registration/FFI locking via `StateLock` remains (a slow
   operator path, not the solve path).
5. **Anchor soup → a single `Epoch` carried on `BlockContext`.** Solve/sim/
   verifier/backfill anchors, `last_solved_block`, `last_drained_block`, and
   `BlockMetadata` are replaced by exactly one block coordinate; "what block
   is this work about" has one answer.

### 4. ADR-021 repositioning

ADR-021's *posture* (detect, classify, stop loudly, never heal) is retained
and referenced, but its mechanism repositions: **in-process desync detection
(solver-state vs chain scalar comparison) retires** — desync becomes
unrepresentable once writers are stage-confined — while
**RPC-disagreement verification at the Published edge is retained** (the
chain/RPC truth can still diverge from us; that failure stays loud and means
the *provider* diverged, not our bookkeeping).

### 5. The Q1–Q6 decision record

| Q | Decision |
|---|---|
| Q1 | **A+D hybrid confirmed** — one stage machine (A) driving a stage-confined cheap-read data plane (D) |
| Q2 | **Incremental in-place throughout.** A temporary internal A/B gate is permitted only inside the stage-machine swap task, and must be removed by that task's completion |
| Q3 | StateView mechanism was gated on the spike outcome; spike `KWKEVV` closed the gate: cheap-read for all families |
| Q4 | **Arb engine ships first.** Multi-engine machinery is a non-goal; the design is guarded by a `NoopStubEngine` exercising every `StageHandlers` hook (see non-goals) |
| Q5 | **12-factor config parity** (file + env, one typed `BotConfig`); effort for coherence is sanctioned |
| Q6 | **Immediate cutover** on each task's completion — 0.6 is alpha, breaking changes permitted. ADR-010/011 Alembic gating and the 0.7 kill list are untouched by this epic |

### 6. Runtime lifecycle is an orthogonal axis

Pipeline stages answer "what has happened to this epoch's block". The
process-level runtime lifecycle — **Boot → Subscribed → SnapshotLoaded →
Resumed** — is a separate axis and never interleaves with the stage
machine's per-epoch transitions. A resume starts a fresh stage machine at
the snapshot-seed epoch; the sweep-back of epoch bookkeeping is what
`SnapshotLoaded` exists to bound.

`EnginePhase` is remapped in full onto this axis: its role —
registration/snapshot/backfill ordering — is runtime-level, not per-epoch, so
it contributes no `Solved`/`Simulated` sub-state to the stage machine. The
existing enum (`Created → Subscribed → SnapshotLoaded → (Backfilled) →
Resumed`) already carries exactly this shape.

## Consequences

- **One decision surface at pipeline scope.** Exactly one component emits
  drain, publish, finalize, notify, backfill, recover, and verify decisions;
  the six machines are its sub-state. The ADR-008/ADR-028 pinned tests are
  the contract and are ported verbatim.
- **Composition desync goes away structurally, so its detector is deleted.**
  The ADR-021 in-process tripwire surface retires; upstream verification at
  the Published edge remains.
- **Cheap-read means near-zero new machinery.** No view representations, no
  COW memory, no replay cost on the steady path; the `ReorgJournal` already
  in the pools crate bounds `Rewind`.
- **`NoopStubEngine` is the executable spec of hook completeness.** It
  asserts stage order, epoch monotonicity, and rewind handling so a future
  second engine cannot discover a missing hook at integration time, and
  keeps the `StageHandlers` trait honest from day one.
- **Tracing carries `epoch` on every span/metric**, so Jaeger/Grafana tell
  the migration's regression story directly.
- **Migration order mirrors the epic task graph** (epoch types →
  `EpochDelta` → StateView plane → `StageHandlers` + stub → unified machine →
  telemetry → seam retirement → ingestion crate → final integration + status
  flip).

## Non-goals

- **No multi-engine machinery.** One arb engine exists; no registry, no
  routing, no second-engine configuration surface. The `NoopStubEngine` is a
  test-declared conformance harness — a test-landmine guard, never
  runtime-selectable.
- **No new StateView representation.** The spike's cheap-read outcome retires
  the COW/journal-materialization mechanisms; their costs remain recorded as
  sizing input for `Rewind` and out-of-corpus growth only.
- **No auto-repair.** ADR-021's posture is unchanged: divergence and the
  desync class stop loudly; nothing self-heals state from chain reads.
- **No backwards-compatibility layer.** Per the repository architecture and
  Q6, retired seams are deleted with a hard cutover; no legacy exports are
  maintained in parallel.
- **No drift into the 0.7 ADR-010/011 kill list.** Alembic retention, the
  SQLAlchemy models, and `ensure_schema`'s Alembic branch are out of scope
  for this epic entirely.
