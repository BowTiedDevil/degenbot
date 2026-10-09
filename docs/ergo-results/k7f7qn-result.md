# K7F7QN — C4.2: PathCostLedger absorbs the live per-path cost sinks

Commit 2 of PR C4 (epic ZKHO3T), after C4.1 (CRHNZ3) removed the eight
write-only accumulators. A new `PathCostLedger` module now owns the live
per-path cost sinks as ONE field on `SolveCycleShared`; the lane walk makes ONE
`record(...)` call and the fan-out lives inside the ledger.

## What changed

- **New** `degenbot-bot/src/arb_engine/path_cost_ledger.rs`
  - `PathCostLedger` — ONE field on `SolveCycleShared`, owning:
    - the two pipeline instruments (`observe_per_path_solve_duration` /
      `observe_per_path_gate_duration`),
    - the K-slowest retention (`parking_lot::Mutex<PathTimesHeap>`),
    - the two heavy-path capture hooks (`capture` / `capture_mixed`).
  - `PathTimeRecord` — the former ten-slot positional tuple becomes a NAMED
    struct (`micros`, `pieces`, `sims`, `word_steps`, `refine_sims`, `gate_us`,
    `gate_derive_us`, `gate_compose_us`, `gate_search_us`, `path_id`). Field
    order is load-bearing: `Ord`/`PartialOrd` derive in declaration order,
    byte-matching the tuple's lexicographic ordering so the `Reverse` heap ranks
    rows identically.
  - `PathTimeRecord::record(pid, resolved, outcome, elapsed)` — the walk's ONE
    ledger call. `SLOWEST_PATHS_K` (5) moves here; the heap push/pop logic is
    byte-identical to the walk's former inline block.
- `degenbot-bot/src/arb_engine/solve_cycle.rs`
  - `SolveCycleShared`: the three fields `capture`, `capture_mixed`, and
    `path_times` collapse to one `path_cost_ledger: PathCostLedger`.
  - The `PathTimesHeap` type alias (and its `lane_walk::PathTimeRecord` alias
    reference) is deleted — the heap type now lives, private, in the ledger.
  - Production construction builds `PathCostLedger::new(solve_block, capture,
    capture_mixed)` instead of a local heap + two field slots.
- `degenbot-bot/src/arb_engine/lane_walk.rs` (`solve_one_path`)
  - The walk measures `elapsed = t0.elapsed()` ONCE (still feeding the
    `degenbot.arb.path` span attributes `path.us` / `path.sims` /
    `path.pieces` / `gate.us` / `path.profit` — tracing state, NOT moved) and
    then makes ONE `ctx.path_cost_ledger.record(pid, resolved, &outcome,
    elapsed)` call in place of the pipeline-instrument block, the K-slowest
    heap push, the gate derive/compose/search split, and the two capture
    hooks.
  - The tuple alias and the `SLOWEST_PATHS_K` const are gone from the module;
    the Q3-DENSE alert (`WALK_DENSE_ALERTED`) stays here (C4.4's scope).
  - The now-under-threshold `#[expect(clippy::too_many_lines)]` on
    `solve_one_path` is removed (the fold shortened the body).
- `degenbot-bot/src/arb_engine/mod.rs`: declares `mod path_cost_ledger;`.
- Fixture/probe contexts (`executor_ab_probe.rs`, `fleet_sim_executor.rs`,
  `tests/clamp_merge_worker_tests.rs`): the `capture` / `capture_mixed` /
  `path_times` literal slots become `PathCostLedger::new(<solve_block>, None,
  None)`.

NOT in this chunk (unchanged): the LPT history (`sims_recorder` /
`gate_recorder`) → C4.3; the Q3-DENSE alert → C4.4.

## Acceptance

- `rg "PathTimeRecord" --type rust rust/crates` → matches only the NAMED
  struct definition/usages in `path_cost_ledger.rs`; the tuple alias is gone.
- The span attributes stayed on the span; the K-slowest retention logic
  (K = 5, min-heap, `len < K || micros > worst`, pop-past-K) is unchanged, and
  `PathTimeRecord`'s derived ordering byte-matches the former tuple ordering.
- `cargo test -p degenbot-bot` and the `--features otel` variant (which
  compiles the `lane_walk` otel/telemetry tests) both green, unmodified.

## Gates (foreground, from `rust/`)

- `cargo test -p degenbot-bot` → all green: lib 715 passed / 3 ignored / 0
  failed, plus the integration binaries (9, 1, 2, 1 passed; the rest 0). No
  failures.
- `cargo clippy -p degenbot-bot --all-targets` → clean (finished with no
  warnings), pinned 1.98.1 toolchain.
- Extra scoped verification (same crate): `cargo test -p degenbot-bot
  --features otel` → lib 753 passed / 3 ignored / 0 failed, including
  `arb_engine::lane_walk::solve_path_span_tests::*`; `cargo clippy -p
  degenbot-bot --all-targets --features otel` → clean.
- `cargo fmt -p degenbot-bot` → applied, no remaining diff.

PENDING: none.

## Tree hygiene

Foreign modification observed and left completely alone:
`.devcontainer/Dockerfile` (28 lines changed, unrelated to this chunk; present
at run start and untouched by me). All other changes are this chunk's files.
