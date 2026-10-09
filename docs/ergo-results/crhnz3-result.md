# CRHNZ3 — C4.1: delete the eight write-only cost accumulators

Commit 1 of PR C4 (epic ZKHO3T). The eight accumulators that `SolveCycleShared`
only ever wrote were deleted outright, with no replacement: the six
`walk_*_total` `AtomicU64`s plus `solve_cpu_us` (also `AtomicU64`) and
`gate_total` (`Mutex<GateStats>`).

## Pre-delete re-verification

`rg "walk_pieces_total|walk_sims_total|walk_word_steps_total|walk_refine_sims_total|walk_ternary_total|walk_grid_total|solve_cpu_us|gate_total" --type rust rust/crates`

Before deletion every hit was a declaration, a literal construction, or a
write (`fetch_add` / `lock().merge(...)`). Zero reads. After deletion the same
`rg` returns no matches (exit 1).

## What changed

- `degenbot-bot/src/arb_engine/solve_cycle.rs`
  - `SolveCycleShared`: removed the eight field declarations
    (`gate_total`, `solve_cpu_us`, and the six `walk_*_total` atomics).
  - Production construction in `SolveCycle`: removed the eight local
    bindings and their eight struct-literal slots. The `gate_total` slot
    built a fresh default `GateStats`; the other seven were shorthand fields
    fed by the removed locals.
- `degenbot-bot/src/arb_engine/lane_walk.rs` (`solve_one_path`)
  - Removed the `ctx.solve_cpu_us.fetch_add(...)` write (the separate
    per-path `micros` local remains — it still feeds the pipeline metric,
    the path span attribute, the slowest-path heap, and capture).
  - Removed the `ctx.gate_total.lock().merge(&gs)` write (`gs` still feeds
    `gate_us` and the gate derive/compose/search splits).
  - Removed the six `ctx.walk_*_total.fetch_add(...)` writes.
  - Narrowed the `outcome_stats` destructure to `(pieces, sims, word_steps,
    refine_sims)`; `ternary_sims` / `grid_sims` had no consumer other than
    the two deleted atomics. `pieces`/`sims`/`word_steps`/`refine_sims` are
    still consumed by the path span, the sims recorder, the slowest-path
    heap, and capture.
- `degenbot-bot/src/arb_engine/executor_ab_probe.rs`: dropped the eight
  fields from the `probe_ctx()` literal.
- `degenbot-bot/src/arb_engine/fleet_sim_executor.rs`: dropped the eight
  fields from the `make_ctx()` literal.
- `degenbot-bot/src/arb_engine/tests/clamp_merge_worker_tests.rs`: dropped
  the eight fields from the `worker_probe_ctx()` literal.

The `::degenbot_solvers::profit_envelope::GateStats` type is still used
elsewhere in the solver crate, so no import became unused.

## Gates (foreground, from `rust/`)

- `cargo test -p degenbot-bot` → all green: lib 715 passed / 3 ignored / 0
  failed, plus the integration binaries (9, 2, 1, 1 passed; the rest 0
  passed). No failing lines, no errors, no unused-item warnings.
- `cargo clippy -p degenbot-bot --all-targets` → clean (finished with no
  warnings), pinned 1.98.1 toolchain (`rustc 1.98.1`, build 48a229cea).

PENDING: none.

## Tree hygiene

Foreign modification observed and left completely alone:
`.devcontainer/Dockerfile` (28 lines changed, unrelated to this chunk;
present at run start and untouched by me). All other work in
`git diff --stat` is this chunk's five files.
