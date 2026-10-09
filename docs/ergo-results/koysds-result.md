# KOYSDS — C4.4: the dense-words alert moves into `record()` + the test constructor

PR C4 (epic 4EPEXR per the ergo record), commit 4. State: **done**.

## What changed

### 1. The Q3-DENSE one-shot alert moves into `PathCostLedger::record`

The walk-side policy and latch are gone from `lane_walk.rs`; the ledger owns
both now, beside the other per-path sinks.

- `lane_walk.rs`: deleted the `WALK_DENSE_ALERTED` `AtomicBool` static and the
  `max_dense_words >= DENSE_OBSERVE_THRESHOLD` check inside `solve_one_path`.
  The module doc no longer advertises a walk-side telemetry flag, the
  `op_warn` import is dropped (`diag` stays), and the comment on the
  telemetry-out block now points at `PathCostLedger::record`.
- `path_cost_ledger.rs`: new module static `DENSE_ALERTED` (same once-per-process
  `swap(true, Relaxed)` latch) and the alert block at the top of `record`
  (which already receives `outcome`), consuming `outcome.stats.max_dense_words`
  against `::degenbot_solvers::cl::DENSE_OBSERVE_THRESHOLD`. The threshold
  constant stays in the solver crate (`degenbot-solvers/src/cl/crossings.rs`);
  the module doc that said "NOT owned here" now claims it.
- Out of scope by design: the known never-reset `WALK_MAX_DENSE_WORDS` defect
  (ergo BPH3ZW, solver-side) — untouched; the alert consumes the field as
  before.

### 2. `SolveCycleShared::for_test` — the three literals collapse

New `#[cfg(test)] impl SolveCycleShared` in `solve_cycle.rs` with
`for_test(solve_block, metadata, core, pool_refs, worker_clamp, inline_sim)`.
The varied fields are the parameters; everything else takes the cold-cycle
default (epoch 0, no captures, fresh walk/prefix memos, zero min-profit,
default runtime stance, a fresh ledger whose `solve_block` mirrors the arg,
no test hooks). Sites now call the constructor:

- `executor_ab_probe.rs::probe_ctx` — the full hand-built literal → a 10-line call.
- `fleet_sim_executor.rs::make_ctx` — the full hand-built literal → a 13-line call.
- `tests/clamp_merge_worker_tests.rs::worker_probe_ctx` — the full hand-built literal → a 14-line call.

Each of the three files is a net deletion in the diffstat.

## Gates (foreground, scoped)

| gate | result |
| --- | --- |
| `cargo test -p degenbot-bot` | PASS — lib 715 passed / 0 failed / 3 ignored; integration backrun_resolver 9, metric_cardinality 1, observability_naming 2, with_core_packs_installed_fleet 1; 0 failed anywhere; doc-tests 0 |
| `cargo clippy -p degenbot-bot --all-targets` | PASS — finished clean, zero warnings (pinned 1.98.1) |
| `cargo fmt -p degenbot-bot` | run |
| `bash scripts/hooks/comment-hygiene.sh` (extra, read-only) | PASS — exit 0 |

Acceptance evidence:

- Once-per-process behaviour preserved: the same `AtomicBool` latch semantics
  move verbatim (only the static's home + name change); no test pins the latch,
  so every existing test passes unmodified.
- The three literal sites are constructor calls (diffstat: the three files are
  net deletions).
- Toolchain: `rustc 1.98.1` / `cargo 1.98.1`.

## Foreign tree activity

The two foreign modifications named in the brief (`.devcontainer/Dockerfile`,
`.agents/skills/rust-rebuild/SKILL.md`) are already committed on `main`
(`24dfa765e`, `7eedec311`) and are **not** present as working-tree changes;
`git status` shows only this task's six files. Left alone. Never hand-edited
`.ergo/plans.jsonl`.
