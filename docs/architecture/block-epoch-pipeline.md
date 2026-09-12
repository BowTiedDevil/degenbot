# The block-epoch pipeline

Ergo epic `MROOY7` · decision record: [ADR-041](../adr/ADR-041-block-epoch-pipeline.md) · data-plane spike outcome: the stateview feasibility doc (spike `KWKEVV`; canonical copy on branch `pi-fabric/stateview-spike`)

> **Implemented (2026-09-07, ergo epic `MROOY7` / `PLRGIN`).** The tables below are the
> signed-off design the epic executed against; [As-built](#as-built-post-mrooy7) at the end of
> this file maps the landed architecture onto the real types and files.

This is the working design reference for the block-epoch pipeline: how one block
flows from logs to settled results through a single stage machine, over a single
seam between the pump and the engine. Every implementation task of the epic
builds against the stage table and seam retirement list below, which the
**user checkpoint has signed off**; code lands against them as the epic's
tasks execute.

There are two orthogonal axes:

1. **Runtime lifecycle** — process-level phases: Boot → Subscribed → SnapshotLoaded
   → Resumed. It never interleaves with per-epoch work; see its table below.
2. **The block-epoch stage machine** — the per-epoch stages of one block, below.

The existing `EnginePhase` enum (`Created`/`Subscribed`/`SnapshotLoaded`/
`Backfilled`/`Resumed`) is remapped in full onto axis 1: its role —
registration/snapshot/backfill ordering — is runtime-level, not per-epoch, so
it contributes no sub-state to the `Solved`/`Simulated` stage rows.

## Epoch invariants

- **I1.** `Epoch { block, seq }` is the *only* block coordinate. `BlockContext`
  carries it; every stage transition, decision, span, and metric records it.
  The anchor soup (solve/sim/verifier/backfill anchors, `last_solved_block`,
  `last_drained_block`, `BlockMetadata`) is deleted.
- **I2.** `seq` is monotone non-decreasing; it bumps exactly once per `Rewind`.
  `block` may regress on reorg; `seq` never does. `(block, seq)` pairs compare
  by `seq` first, then `block`.
- **I3.** A `BlockContext` whose `(block, seq)` does not match the active epoch
  fails fast — mirroring the repository's fail-loud posture (ADR-021): stale
  contexts are never silently applied.
- **I4.** Writers to `StateLock<RwLock<BotState>>` exist **only** in the
  Streaming stage of the active epoch. Quiesced..Simulated take read guards
  (cheap-read, per the spike). Gated..Finalized commit only delivery/results
  surfaces, never pool state.
- **I5.** Exactly one `Publish` per quiesce cycle; Published implies the
  block's completeness verdict (tombstone-by-successor or settle) was obtained.
- **I6.** `Rewind{to_epoch}` may originate from **any** stage; at most one
  rewind is in flight, and a second fails fast.
- **I7.** The delivery cutoff (glossary: *Last complete block*, on `BotState`)
  is monotone and never reset by a resume (ADR-028 addendum (a)); it is
  mirrored from the Finalized stage's tombstone verdict.

## Stage table

| Stage | What happens | Data-plane posture (`StateLock<RwLock<BotState>>`) | Emits / decides | Sub-state absorbed (from the six machines) | Retires |
|---|---|---|---|---|---|
| **Streaming** | Live WS log events + gap `eth_getLogs` backfill applied in log order | **The only stage permitted to write** | Dirty pools into the epoch's `EpochDelta`; recovery single-writer discard; `Backfill` request | `BlockClock` log arrival; `PumpFSM` recovery rules (ADR-028) | `DispatchOwner`'s dispatch-log path |
| **Quiesced** | All dispatched logs for the open block applied; completeness classified (tombstone vs settle; early-slice/debounce gates) | Read guard | `Notify` (header tick), `Verify` (WS completeness), `Backfill`, watchdog `Recover`/`LogSilence` — data, not timers | `BlockClock` tick detection + quiesce classification (ADR-008); `PumpFSM` watchdogs + backfill trigger (ADR-028) | inline quiesce/`block-window` reasoning |
| **Resolved** | The block's `EpochDelta` → affected-path derivation | Read guard (path index) | The epoch's affected-path set | `PathLifecycle` registration bookkeeping | `DirtySets` + `EngineSubscriber` classification |
| **Solved** | Solver runs the affected paths | **Cheap-read:** read guard through the solve — `solve_path_duration` p99 **5 ms**, `solve_gate_duration` p99 **1 ms** (budget 100 ms) | Solved candidates | Solver dispatch | `drain_lock` + engine `Mutex` on the solve path (ADR-037 shard retired here) |
| **Simulated** | In-process revm simulation — the sole executor (ADR-019) | Read guard | Derivation/profit facts per candidate (tri-state per ADR-030) | simulation bookkeeping only (`EnginePhase` carries none of this — it is runtime-lifecycle, see below) | the sim anchor of `sim_anchor.rs` (expressed as `Epoch`) |
| **Gated** | Risk/size/profit rechecks on solved+simulated candidates | None (pure evaluation) | Deliver/reject verdicts per bucket (ADR-040) | `DeliveryLifecycle` gating policy | `DrainerHealth` no-progress checks (→ machine watchdogs) |
| **Published** | Winning bundle to execution; sinks subscribe here (delivery channel, submission, Python) | None (results written to delivery/FFI surface, not pool state) | `Publish` — exactly one per quiesce cycle; RPC-disagreement `Verify` (ADR-021 upstream check retained) | `PumpFSM` settle publish gate | `DrainWork::Publish` + `DispatchOwner` + `DrainSink` routing |
| **Finalized** | Epoch closed: delivery cutoff stamped, results final | Cutoff monotone on `BotState`; never reset by resume | Epoch close | `DeliveryLifecycle` terminal stamps; registration verify-lifecycle terminal (ADR-022) | cursor stamps in `solve_coordinator.rs` (`last_solved_block`) |
| **Rewind{to_epoch}** | Reorg unwind from **any** stage to a fresh epoch at an earlier block | `ReorgJournal` restore-before-block ≤ **23 µs p99** (V3, depth 32), 20–30 ns (V2); no view machinery needed | `Epoch.seq` bump; stale contexts fail fast; epoch views above the fork invalidated | The pump's reorg-episode and resume tracking (`block_pump.rs`) | the `ReorgCoordinator` becomes the rewind executor |

The pump's (`block_pump.rs`) reorg-episode and resume tracking folds into
`Rewind` handling; its WS transport + watchdog machinery later moves to
`degenbot-ingestion` (epic task `5WTYYQ`).

### Runtime lifecycle (orthogonal axis)

| Phase | Meaning | Stage-machine relation |
|---|---|---|
| **Boot** | Process init: config, telemetry, DB | No stage machine exists |
| **Subscribed** | WS subscriptions + topic/address filters up (degenbot-ingestion) | Streams feeding; no active epoch |
| **SnapshotLoaded** | Snapshot seed epoch `E(S)` established from the store | Fresh stage machine; gap `[S+1, W]` backfilled and owned by backfill |
| **Resumed** | Live log flow enters Streaming | The per-epoch stage cycle runs; delivery-cutoff monotony preserved across resumes |

`EnginePhase` is the in-code representation of this axis, remapped here from
any per-epoch reading; its registration/snapshot/backfill ordering role is
runtime-level, never per-epoch stage sub-state.

## Epoch invariants at the stage boundaries

See invariants I1–I7 above; the spike-derived numbers binding the stage table
are: writers confined to Streaming (log-burst window p99 ≤ 100 ms);
`state_lock_hold`/`state_lock_wait` p99 ≤ **0.1 ms** across 19.6 M
acquisitions on the still-contended pre-stage-machine architecture; rewind
bounds above.

## Seam retirement list

The parallel accounting system is deleted, not wrapped (Q6: hard cutover):

1. **`DrainSink`/`Engine` dual seam → one `StageHandlers` seam.** One trait; a
   future second engine implements it or there is no second engine.
   `NoopStubEngine` is the executable spec keeping the trait honest (see
   non-goals).
2. **`DirtySets` + `EngineSubscriber` classification → `EpochDelta`.** Log
   application records touched pools as a byproduct of dispatch; affected-path
   derivation reads the delta. `EngineSubscriber` shrinks to liveness +
   notification only.
3. **`SolveCoordinator` / `drain_lock` / `DispatchOwner` / `DrainerHealth`
   dissolved.** The stage machine owns the edge conditions; delivery and
   submission are sinks at the Published edge, not seams in front of the
   engine.
4. **Engine `Mutex` off the solve path.** Stage separation makes the
   cheap-read on Quiesced..Simulated uncontended by construction.
   Registration/FFI locking via `StateLock` remains (slow operator path, not
   the solve path).
5. **Anchor soup → a single `Epoch` on `BlockContext`.** solve/sim/verifier/
   backfill anchors, `last_solved_block`, `last_drained_block`, and
   `BlockMetadata` collapse into one `Epoch` on `BlockContext`.

## Non-goals

- **Multi-engine machinery.** No registry, no routing, no second-engine
  configuration surface. Landmine guard: **`NoopStubEngine`** implements
  `StageHandlers` alongside the real arb engine and is exercised in a scripted
  conformance harness (synthetic block stream driving the full lifecycle + a
  reorg + a backfill episode, asserting hook completeness, stage order, and
  epoch monotonicity — Q4). It is test-declared: a conformance harness, never
  runtime-selectable.
- **Materialization / COW StateView machinery.** Cheap-read won unambiguously
  (spike `KWKEVV`); the alternative mechanisms' costs are retained only as
  `Rewind`-bounding data.
- **Tripwire re-expansion.** In-process desync detection retires (desync
  unrepresentable post stage-confinement); the upstream, Published-edge
  RPC-disagreement check stays and stays loud (ADR-021 posture).
- **Backwards compatibility** of any retired surface (hard cutover, Q6).
  Pinned behavioral tests are ported; stale APIs do not get a parallel life.
- **Schema changes.** ADR-010/011 Alembic ownership and the 0.7 kill list (see
  the repository `AGENTS.md`) are untouched by this epic.

## Migration order (mirror of the epic task graph)

| # | Ergo task | Lands | Stage coverage |
|---|---|---|---|
| 1 | `7LKJFY` ✅ + `KAHU5W` | Typed `BotConfig` + 12-factor parity; migrate env reads | (config axis; orthogonal) |
| 2 | `KWKEVV` ✅ | StateView spike — cheap-read for all families; Q3 gate closed | data plane |
| 3 | `T6IYKY` | `Epoch` + `BlockContext`; delete the anchor soup (seam #5) | context for all stages |
| 4 | `LXDY4C` | `EpochDelta` dirty tracking; delete `DirtySets` + subscriber classification (seam #2) under a capture-replay parity gate | Streaming → Resolved |
| 5 | `2UVG3E` | StateView data plane: write confinement + engine lock off the solve path (seam #4); reposition the ADR-021 tripwire | Streaming ↔ Quiesced..Simulated; Published (`Verify`) — **landed (cheap-read branch): `DEGENBOT_DETACHED_SOLVES` default ON (engine Mutex off the solve path); the in-process chain-vs-solver tripwire module deleted; `EpochDelta` placeholder unified onto the real ledger; lock inventory + p99 replay in the feasibility doc §5.1** |
| 6 | `YM2FZR` | The `StageHandlers` trait + `NoopStubEngine` conformance harness (target shape of seam #1) | all (trait shape) |
| 7 | `7NFYQW` | Unified stage machine: fold the six machines, `BlockPump` → thin driver, pinned tests ported verbatim; Q2's only sanctioned internal A/B gate lives here and must be deleted by task end | all |
| 8 | `BF43PM` | Per-stage OTel spans + metrics carrying epoch attributes | observable across all |
| 9 | `SZJUKL` | Seam retirement: delete `DrainSink`/`Engine`/`SolveCoordinator`/`drain_lock`/`DispatchOwner`/`DrainerHealth` (seams #1, #3) | Published-edge + drive wiring |
| 10 | `5WTYYQ` | Extract `degenbot-ingestion` (pyo3-free); Python becomes a Published-edge sink | Subscribed + Published |
| 11 | `PLRGIN` | Regression: capture-replay sweep + live Jaeger soak A/B; flip ADR-041 to implemented; update `CONTEXT.md` vocabulary | proves all |
The order above is dependency-ordered per `ergo`; #8 (`BF43PM`) and #9
(`SZJUKL`) may interleave once #7 lands.

## As-built (post-MROOY7)

Every type and file below is verified to exist in the merged tree (epic `MROOY7`
landed through `PLRGIN`; see [ADR-041](../adr/ADR-041-block-epoch-pipeline.md),
status **implemented**). Task ids referenced: typed config `KAHU5W`, data-plane
spike `KWKEVV`, epoch contexts `T6IYKY`, ledger `LXDY4C`, lock/confinement
`2UVG3E`, trait `YM2FZR`, machine fold `7NFYQW`, stage telemetry `BF43PM`, seam
retirement `SZJUKL`, transport extraction `5WTYYQ`, final integration `PLRGIN`.

### Crate map

**`degenbot-ingestion` — the pyo3-free WS transport** (boundary contract: *ingestion
emits, the runtime decides* — the crate knows nothing about `BotState` or the stage
machine). Source: `rust/crates/degenbot-ingestion/src/`

- `ingestor.rs` — `WsIngestor`: one WS connection; `newHeads` + **unfiltered** `logs`
  merged into one `IngestEvent` stream (`stream_select`, fair interleave).
  `subscribe_with_handshake` → `SubscribeBoundary` — the MJXP5Z one-stream handshake
  (no drop+resubscribe; handshake-consumed logs are re-injected), with the DFQYM5
  log-liveness boundary (`LOG_CATCHUP_SETTLE_SECS` = 15 settle window before the
  header-confirmed fallback). Gap backfill = `fetch_logs` over `eth_getLogs` in
  `DEFAULT_BACKFILL_CHUNK_SIZE` (2000-block) chunks; `BACKFILL_TIMEOUT_SECS` (60) is
  both the idle/degraded window and the handshake deadline. `exact_relevant_indices`
  is the client-side exact-topic pre-filter the WS-completeness cross-check uses
  (the server-side OR-list over-matches on some nodes).
- `events.rs` — `IngestEvent` (`BlockHeader` | `Pool`); `PoolEvent` carries
  `{ epoch, log_index, payload }` so consumers order/drop without touching the payload.
- `topics.rs` — `RELEVANT_TOPICS`, `is_relevant_log`: the single Rust-side hot
  pre-filter (also the backfill filter's OR-list source and the dispatcher's
  defensive re-check).
- `filter.rs` — `build_backfill_filter` / `backfill_filter` (single-block shape).
- `watchdog.rs` — `Watchdog`: the transport liveness *windows*
  (`HEADER_STALENESS_SECS` = 30, `LOG_SILENCE_SECS` = 60, `LOG_WAIT_MAX_AGE_SECS` = 5
  — the SONJQA span-force-close bound) + per-episode `silence_alarm_count`. The
  *decisions* on those windows belong to the stage machine (`watchdog_phase`).
- `examples/headless_boot.rs` — the standalone no-Python boot smoke
  (`just test-standalone` runs it).

**`degenbot-bot` — the runtime** (the stage machine, its driver, and the engine):

- `bot_core/stage_machine.rs` — `StageMachine` (7NFYQW): the ONE pure, I/O-free
  machine over one block epoch — no provider, no timers, no `Instant`, no locks.
  The six retired machines (incl. the standalone `BlockClock`, `PumpFSM`) are folded
  in as sub-state (hard cutover). Emits `HeaderDecision`, `LogDecision`,
  `StageDecision`, `CompletenessDecision`; exposes `watchdog_phase`
  (`Healthy`/`HeaderStale`/`LogsSilent`) — the phase space the dissolved no-progress
  accounting mapped onto. Invariants I1–I7 (above) are pinned by the ported tests.
- `bot_core/epoch.rs` — `Epoch` `{ block, seq }` + `BlockContext` (T6IYKY): the
  only block coordinate. The derived `Ord` is `(seq, block)` — **a rewind sorts
  ABOVE any earlier-generation epoch** (a monotone cursor never regresses across a
  rewind even though the block moves down); `ensure_current` fail-fasts stale
  contexts with `StaleEpoch`.
- `bot_core/epoch_delta.rs` — `EpochDelta` (LXDY4C): the per-epoch touched-pool
  ledger keyed by `AffectedKey` (`degenbot-solvers`), recorded as a byproduct of
  `Bot::dispatch_log`; `take_keys` is the drain's atomic consumption; a rewind
  RELABELS the ledger (`set_epoch`) and retains keys (solve-cursor state, not
  block-window state).
- `bot_core/stage_handlers.rs` — `StageHandlers` (YM2FZR): the one engine seam,
  encoding the stage table as a required-hook trait (no default bodies: adding a
  hook without updating every implementer is a compile error) plus the exhaustive
  `Stage` order table (`legal_successors`, sized `ALL_STAGES`).
- `bot_core/block_pump.rs` — `BlockPump`: the thin async driver. Work executes
  INLINE at the machine's decision points (no FIFO); it feeds `Bot::dispatch_log`
  per log, drives the stage hooks, and runs the driver-side `reorg_flying_stale`
  I3 check at each work site.
- `bot_core/log_dispatcher.rs` — `LogDispatcher`: decoder registry + `dispatch`
  (decode → apply under a write guard → release → notify `PoolStateSubscriber`s;
  records into the epoch's `EpochDelta` as a byproduct). Strict
  `DEGENBOT_WS_COMPLETENESS` mode fails loudly on malformed-event drops. Span:
  `degenbot.log.dispatch`.
- `bot_core/reorg_coordinator.rs` — `ReorgCoordinator`: per-event journal rollback
  (idempotent + order-insensitive restore-before-block); `NoStatePriorToBlock` → the
  pump shuts down gracefully (never a silent stale state). Rewind is a Bot concern,
  never a stage-hook seam.
- `bot_core/solve_anchor.rs` — `SolveAnchor`: the request block floored by the
  pool-state head, itself an `Epoch` (the head-floor desync rule from MQIZ5M/IIA/
  0x99ac8c).
- `bot_core/state_lock.rs` — `StateLock`: the diagnostic `parking_lot::RwLock`
  wrapper (Z4Z6VO) guarding `BotState`; hold-tracking forensics off by default
  (`DEGENBOT_STATE_LOCK_DIAG`), the blocked-wait warn threshold (500 ms) always on.
- `bot_core/stage_telemetry.rs` + `bot_core/pump_telemetry.rs` — BF43PM: one
  `degenbot.epoch.run` ROOT per block epoch (carrying `epoch.block` + `epoch.seq`) and
  one `degenbot.stage.<stage>` span per transition (`stage.from`→`stage.to`,
  `queue.age_us`); open rows (`streaming`, `rewind`) are force-closed past
  `STAGE_MAX_AGE_SECS` by `force_close_aged` (the SONJQA/G3 law). Metric series
  `degenbot.stage.publish_cycle`, `degenbot.stage.rewind`,
  `degenbot.stage.rewind_duration` are label-free (epoch context rides spans).
- `arb_engine/engine_stages.rs` — `EngineStages` (SZJUKL): the arb engine's
  `StageHandlers` implementation — the dissolved coordinator/fan-out/wrapper types
  collapsed into one. Owns the two genuine-async-boundary channels retained from
  ADR-006/027: the block clock (`BlockClockPipe`, header ticks, never queued behind
  solver work) and the result batch written at the Published edge.
- `arb_engine/solver_dispatch.rs` — the detached solve cycle (unconditional
  since the WFF6MM cutover; the `DEGENBOT_DETACHED_SOLVES` stance retired with
  the in-cycle arm): the solve cycle enqueues and returns, collapsing the
  engine-`Mutex` hold on the solve path to enqueue-end.

**`degenbot-config` — typed config (KAHU5W).** `BotConfig` is declared exactly once
in `schema::SCHEMA` (one declaration ⇒ the typed field, the `DEGENBOT_*` env
mapping, the TOML path, and the generated `docs/rust-config-keys.md`); the loader
is fail-closed 12-factor (CLI > env > file > defaults) and the boot path installs
it in the process-wide holder every call site reads (`bot_core/stance.rs`,
`stance::config()`).

**`degenbot-core` — `block_clock_pipe.rs`.** `BlockClockPipe` / `BlockNotification`:
the shared, engine-neutral block-clock channel (ADR-027's direct pipe, relocated
out of the retired coordinator). This live *channel* type is unrelated to the
retired `BlockClock` *machine*.

**`degenbot-python` — the driver shell.** The PyO3 layer subscribes like any other
sink: `PyBot` owns the pump lifecycle (`PumpState`, `bot/pump.rs`), the result-batch
consumer (`bot/engine/result_channel.rs`) and the header `block_stream` hang off
the Published/block-clock edges, and `PySubscriberAdapter` (`bot/subscriber.rs`)
bridges `PoolStateSubscriber` callbacks. No raw WS stream reaches Python.

### The as-built epoch cycle

1. **Subscribe** — `WsIngestor::subscribe_with_handshake` → boundary `W` from
   log-stream liveness (header-confirmed fallback past the settle window).
2. **Backfill** — `[S+1..W]` via `fetch_logs` applied through
   `BotState::process_backfill_logs`; the resume drops WS logs for blocks ≤ `W`.
3. **Resume loop** — per log: `StageMachine::observe_log` → `Bot::dispatch_log`
   (decode → write under the Streaming-confined guard → release → ledger record →
   notify). Per header: `observe_header` (a header alone NEVER advances the cursor;
   the D1 tombstone-by-successor rule survives unchanged).
4. **Stages** — `on_streaming_complete` (quiesce + completeness classify +
   debounce/early-slice gates) → `on_resolve` (`EpochDelta::take_keys` → affected
   paths) → `on_solve` → `on_simulate` (in-process revm, the sole executor) →
   `on_gate` (deliver/reject buckets, ADR-040) → `on_publish` (exactly one per
   quiesce cycle) → `on_finalize` (delivery cutoff stamped, monotone, I7).
5. **Published edge** — `StageHandlers::on_publish` is where every sink hangs:
   the delivery channel → Python's result batch; the block-clock pipe → Python's
   header clock; and the ADR-021 upstream **RPC-disagreement verification**
   (`CompletenessDecision::Verify` → `assert_ws_block_complete` in `block_pump.rs`),
   which is the surviving kernel of that tripwire ADR — the in-process desync-detection
   half was retired because write confinement makes it unrepresentable.
6. **Rewind** — `Rewind{to_epoch}` may fire from any stage (I6, at most one in
   flight): `Epoch::rewind_to` bumps `seq` exactly once, `ReorgCoordinator` runs
   the per-pool journal restores, the ledger is relabeled, and stale contexts
   fail fast.

### Cheap-read posture

Writers to `StateLock<RwLock<BotState>>` exist **only** in the Streaming stage of
the active epoch; Quiesced..Simulated read through cheap snapshots (spike
`KWKEVV`). With the detached cycle unconditional (WFF6MM cutover), engine-`Mutex`
holds on the solve path collapse to enqueue-length, so the solve is uncontended by construction
— there is no `drain_lock` and no FIFO, and the
`drain_lock → engine Mutex → BotState RwLock` lock-order narration of
[ADR-037](../adr/ADR-037-engine-mutex-sharding.md) is historical.

### Evidence

- Final-integration validation: the capture-replay regression sweep (zero
  divergences) and the live Jaeger soak A/B against the pre-epic operator
  baselines, recorded in the `PLRGIN` task result; the percentile replay harness
  lives at `scripts/soak_percentiles.py`.
- Spike-derived bounds behind the stage table (write-burst window, state-lock p99,
  rewind-restore costs): the stateview feasibility doc §3/§5.1
  ([`docs/architecture/stateview-feasibility.md`](stateview-feasibility.md)).
- Telemetry semantics (spans, metrics, force-close law):
  [docs/telemetry-latency-playbook.md](../telemetry-latency-playbook.md).

### Legacy telemetry names (retired)

The proto-pump waterfall names are fully retired: `degenbot.pump.block`,
`degenbot.pump.log_wait`, and `degenbot.pump.apply_stream` no longer exist as
spans; the per-header beat became the `degenbot.epoch.run` root and the opaque
children became `degenbot.stage.<stage>` rows. The operational lesson they
encoded survives as the watchdog's span force-close bound
(`LOG_WAIT_MAX_AGE_SECS` / `STAGE_MAX_AGE_SECS`).
