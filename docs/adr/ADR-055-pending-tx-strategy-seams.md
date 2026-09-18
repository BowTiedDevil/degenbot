# ADR-055: Pending-transaction strategy seams — `MarketContext`, `PendingTxStrategy`, `SubmissionTarget`, and the V4 closure

**Status: accepted** (2026-09-18). Basis: the seam synthesis
(`.scratch/strategy-arch-survey/strategy-seam-design.md`, reviewed and
approved), ADR-054's promoted evidence seams, ADR-019's strategy-vs-engine
doctrine. The engine-family generalization of ADR-018 is pulled here **by
decision**: a strategy suite over one core is the stated end-state, which
Phases B/C below schedule.

## Context

Two strategies (settlement arbitrage over sealed blocks; backrun over
observed pending transactions) shared a healthy lower substrate (solvers,
pools, simulation, executor grammar, RPC provider) but nothing above it:
`strategy` was not a config/console/launcher concept, the pending-transaction
pipeline was one monolithic function (`frame_pipeline::process_frame`) with
backrun selection and economics inline, `StrategyRuntime` misnamed shared
caches as strategy identity, V4 pools could neither be extracted from the
replay journal nor admitted into the per-trigger sandbox, and unknown pool
families were silently skipped at use sites.

## Decision

### D1 — Vocabulary (CONTEXT.md is canonical)

pending transaction, `PendingTxDriver`, `PendingTxStrategy`,
`SettledBlockStrategy`, `MarketContext`, `ComposedIntent`/`Decided`,
`SubmissionTarget`, strategy facet. Names carry plain meanings; the retired
"frame"/"lane" vocabulary survives only where a historical module name still
says it.

### D2 — The pending-transaction seam (landed, commit 024cad4d4)

- **`MarketContext`** (`degenbot-submission/src/market_context.rs`) — the
  frame-surviving caches: connector index + DFS graph, token id/address
  joins, warm code cache. Substrate, never strategy; the former
  `StrategyRuntime` is retired.
- **`PendingTxStrategy`** (`degenbot-submission/src/pending_tx.rs`) — one
  strategy reacting to observed pending transactions. Stages:
  `admit` → `discover` → `evaluate` → `compose` → `decide`, with
  strategy-neutral hand-off artifacts (`ComposedIntent`, `Decided`) and
  associated types (`Affected`/`Intents`/`Evaluated`) keeping strategy
  shapes out of the driver. Consumed via generic dispatch; the
  `async_fn_in_trait` expectation records the no-dyn contract.
- **`BackrunStrategy`** (`degenbot-submission/src/backrun_strategy.rs`) —
  the first implementation: WETH/quote-orientation gating, anchored DFS
  wiring, envelope-gated evaluation, net-bid economics, and the decision
  policy moved out of the driver. The driver's stage code names no WETH /
  MEVBlocker / wallet-economics vocabulary.
- **Driver side** — `frame_pipeline::process_frame` is re-expressed as the
  pending-transaction driver: replay (ADR-054 seam 1) → descriptors +
  journal extraction (seam 2) → the strategy's stage chain → bundle
  simulation gate → submission. The driver keeps replay/extraction/
  timings/liveness as strategy-neutral machinery; `PipelineConfig` and the
  honest observe vocabulary are driver-owned.
- **`SubmissionTarget { Bundle(BundleTarget), Public }`** at
  `dispatch_and_submit` — the typed channel vocabulary replacing
  `Option<&BundleTarget>`; the Python FFI wrapper passes `Public`.

### D3 — V4 is first-class in the pending-transaction substrate

- **Sandbox admission** (commit 90668c03e): `ExplicitPoolState::V4`
  delegates to the canonical `register_v4_pool`; a golden V4→V2 route solve
  (`workspace_v4_cycle_reaches_the_solver`) pins the behavior.
- **Journal extraction** (commit 024cad4d4): `PoolFamily::V4PoolManager`
  carries a `V4PoolSet` of known pool identities; extraction decodes slot0,
  liquidity, and touched tick words per poolId against the pinned
  PoolManager layout (`docs/architecture/v4_poolmanager_storage_layout.md`).
  An unknown poolId NEVER fabricates state — it stays explicitly
  `Unsupported`. The stale "layout nobody pinned" premise is revoked.
- Remaining lane-level gap (tracked): the connector index
  (`V2ConnectorIndex`) carries V2/V3 edges only, so production descriptors
  pass an empty `V4PoolSet` until the index learns V4 identities and
  `SuccessStrategy` compose gains V4 funding/capture shapes. Substrate is
  ready; lane wiring is a strategy task, not a substrate defect.

### D4 — Loud-abort posture for pool families at use sites

Standing rule (user directive): if code *tries to use* a pool of a family
the infrastructure cannot serve, that aborts loudly (typed fatal/tripwire),
never a silent skip. Transient RPC/timing/fetch failures keep existing skip
semantics. The audit (`.scratch/strategy-arch-survey/loud-abort-audit.md`)
found 24 family-at-use silent-skip sites vs 8 legitimate transient skips —
the five worst (silent pool-kind drop in DB pathfinding,
`#[non_exhaustive]` `PoolKind` walker abandonment, silent V4-half drop in
mixed frames, fabricated zero `V2SwapOutcome` for unknown pools, silent
exchange-name drop in the pool updater) are the fix order; each fix is a
typed-abort conversion with a named red test.

### D5 — Phases B/C (recorded, not built here)

- **Phase B (ergo PF37R7):** core-service consolidation toward the strategy
  host — one event hub over pending-tx feeds + newHeads/logs with fan-out
  to subscribed strategies; one pool-state tracker every strategy reads
  (the sidecar's leaked empty anchor retires); one route registry built
  once and shared by reference; both drivers consume services by handle.
- **Phase C (ergo GOTEEG):** the dynamic strategy host — register/enable/
  disable strategies at runtime (operator verbs + `.enabled` facets; the
  current `strategy.name` single-selector lets exactly one arm run until
  then); pulls the ADR-018 engine generalization for the settled-block
  family (parameterized stage payloads, generic `EngineDriver`, per-family
  fleet globals) — scheduled, not speculative.

### D6 — Amendments to ADR-054 (corrections of record)

- Seam 4's cited `path_selection::solve_witnesses` never landed; the
  witness role is played by `SolveStats`/`ChainOutcome` in the pipeline.
- Anchored discovery is depth-capped at 3 hops
  (`find_paths_iter(..., Some(2), ...)`); "3-hop and beyond by curve" is an
  aspiration, not a shipped capability.
- The decision gate consumed the per-frame liveness pair `(age_ms > stale)`
  historically; the finality-based liveness FSM (Tentative/revive/death;
  `NonceConsumed`/`MinedAt`/`SlotTakenAt`) replaced it and is deliberately
  strategy-agnostic channel machinery — its eventual home is the submission
  channel (Phase B), recorded jointly with the liveness task's own record.

## Consequences

- Adding a pending-transaction strategy is: one `PendingTxStrategy` impl +
  typed config facet readers + one launcher/console row. The four surveyed
  wedge buckets (driver/FFI/CLI/config/env) collapse to one new Rust module
  and values.
- V2/V3/V4 all admit, extract, and solve inside the per-trigger sandbox.
- The settlement path drives the same submission channel with
  `SubmissionTarget::Public`; its remaining Python-side candidate assembly
  is tracked (ergo GE5BE7).
- Loud-abort is the running audit-and-fix posture (ergo TYIXQ7, blocked on
  the fix pass).
