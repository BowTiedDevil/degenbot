# ADR-045: The solve cycle as a deep module — ArbEngine regroups into registry + cycle + config

**Status: accepted** (2026-09-12; architecture review candidate #1, grilling rounds 1–3).
Landing tracked by ergo epic below (recorded at cutover into each task body).

## Context

`rebuild_and_solve_affected` (`arb_engine/solver_dispatch.rs`, ~890 lines) owns the
whole per-block solve cycle — dirty-path fan-out, admission draw/shed, chunked
resolve, LPT binning, detached enqueue — *and* the wiring that makes it correct
is smeared across `engine_stages.rs` and ~20 fields on `ArbitrageEngine`: the
admission-draw stash (`admission_draw_zero`), the `cycle_arm` string, the
`pending_new_paths` carry written by `register_and_solve_path` and consumed by a
different function, the seven `for_test` knobs the engine carries but never
reads, and the cursor advance contract. Sibling entries (`solve_all_paths`,
`register_path`, `register_and_solve_path`) share the stash by convention. The
file is the repo's #1 churn site; each fix is a cross-module safari.

## Decision

`ArbitrageEngine` regroups to **`registry: PathRegistry` + `cycle: SolveCycle` +
config**. Two seams, typed by the borrow:

- **`PathRegistry`** (`arb_engine/path_registry.rs`) — path *identity* only:
  path_pools, the `pool_to_paths` reverse index, signatures, next_id, cap,
  dedups. Shallow by design: `commit`/`remove`/`lookup`/`paths_for`; no
  resolve, no solve, no deps beyond solver value types. The hot cycle takes a
  **shared** `&PathRegistry` (cycles never mutate identity — the type says it);
  registration takes `&mut`.
- **`SolveCycle`** (`arb_engine/solve_cycle.rs`) — the leaf of ADR-041's
  Solved stage and its three sibling entrances. Owns the resolve companions
  (`path_resolved`, `path_status`, the projection caches, the same-state
  snapshots), the **cycle-transient** stash (admission draw verdict, the
  `pending_new_paths` carry, the latched arm), the cursor, the results map,
  the `DetachedCycle` collaborator (unchanged module, ADR-041-adjacent by
  prior decision), and the seven `for_test` knobs. Interface: `draw(delta,
  head) -> Vec<AffectedKey>` (Resolved row), `run_epoch(entry, affected, block,
  metadata, &PathRegistry) -> CycleOutcome` (Solved row, enqueue-end semantics),
  `register_path` / `register_and_solve_path(hops, &mut PathRegistry) ->
  Result<Registration, PathRegistrationError>`, `solve_all_paths(block,
  &PathRegistry) -> CycleOutcome`, `merge_detached_item`, `forget(path_id)`.
- **`Registration { path_id, created, resolved: Option<_> }`** — a dedup hit and
  a fresh register are typable facts, not `pending_new_paths` timing.
- **`CycleOutcome { solved_block, census: ResolveCensus, arm: CycleArm }`** —
  typed `Shed | SkippedEmpty | Solved{ seq, bins, ... } | Dissolved`;
  `label()` reproduces the ADR-043 `cycle.arm` vocabulary
  (`"shed" | "skipped_empty" | "detached"`) **byte-for-byte**. Stage hooks read
  the outcome; the engine's string-stash fields are deleted.
- The StageMachine (ADR-041) stays the pipeline owner; stage hooks thin to
  `draw` + `solve_dirty` + a 3-line sidecar spawn, and their own
  cross-interface drift (architecture review candidate #2) becomes a mechanical
  follow-up, not a precondition.
- One `core.read()` window per cycle, dropped before any solve submits
  (ADR-005 slice 15b-1 discipline, unchanged).

## Considered options rejected

- **Deps-struct of engine field refs** (cycle stays stateless): the interface
  spelled out in 12–18 refs, ownership claim unenforced. Rejected in grilling
  Q5 for the type-enforced regroup.
- **`SolveDispatch` port + `SolveEnv` bundle (replay-harness-first)**: one
  real adapter today; reintroduce when the second consumer exists.
- **Resolve state inside `PathRegistry`**: deepens the registry but smears the
  cycle's own dependencies into identity-land and forces `&mut` on the hot
  path. Rejected: identity and resolve-companions are different axis splits.

## Consequences

- White-box tests re-index (`engine.cycle.*` / `engine.registry.*`,
  mechanical, same crate). The pinned `for_test` setter names are preserved as
  engine delegators — ADR-041's "for_test knobs must not change" holds at the
  call site.
- `PathRegistrationError` moves to the registry module, re-exported at the old
  path so the PyO3 mapper is untouched.
- Architecture-review candidates #2 (StageHandlers control plane), #5 (capture
  twins) and #8 (epoch-ledger ownership) are unblocked as follow-up mechanics,
  not prerequisites.
- Hard cutover, no feature flag; the existing `arb_engine` suite is the
  characterization net (kept green every task), new red tests name
  `SolveCycle`/`CycleOutcome` first.
