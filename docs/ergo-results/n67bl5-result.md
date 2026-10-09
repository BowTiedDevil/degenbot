# N67BL5 — C4.3: the LPT cost history moves into the ledger (snapshot seam)

Epic ZKHO3T (PR C4), commit 3. State: **done**.

## What changed

The LPT cost feedback loop (this cycle's measured per-path cost -> the next
cycle's bin packing) now lives inside `arb_engine/path_cost_ledger.rs` behind a
typed interface. The two raw `Arc<Mutex<HashMap<u64, u64>>>` fields on
`SolveCycleShared` (`sims_recorder` / `gate_recorder`) and the two engine fields
they aliased (`SolveCycle::last_walk_sims` / `last_gate_us`) are gone.

New ledger-owned types (`path_cost_ledger.rs`):

- `PathCost { sims, gate_us }` — one path's measured previous-cycle row.
- `PathCostHistory` — the durable carry-over (`Mutex<HashMap<u64, PathCost>>`),
  engine-owned; `record(pid, PathCost)` writes, `snapshot() -> CostSnapshot`
  takes the ONE lock per cycle.
- `CostSnapshot` — the typed, immutable-by-convention view the bin packer
  queries via `cost(pid) -> Option<PathCost>`.

`PathCostLedger::record` (the walk's ONE call) now also writes the path's LPT
row, so `lane_walk::solve_one_path` no longer touches any cost map itself.
`PathCostLedger::snapshot` is the only read path.

Call-site moves:

- `solve_cycle.rs`: `SolveCycleShared` loses the two recorder fields; the
  per-cycle ledger is built with `Arc::clone(&self.path_cost_history)`. The
  `compute_bins` closure takes `shared.path_cost_ledger.snapshot()` once and
  feeds `sims_aware_cost(proxy, measured.map(|c| c.sims), measured.map(|c| c.gate_us))`
  — same values, same single-lock timing, no `lock().clone()` idiom left.
- `workload_partition.rs`: `sims_aware_cost` keeps its exact signature
  (`Option<u64>`, `Option<u64>`), so the bin-packing unit tests are unmodified
  (acceptance). Only its doc note changes (the values now come from the
  ledger's snapshot).
- `mod.rs`: engine construction builds the one `PathCostHistory`.
- Test/aux `SolveCycleShared` literals (`fleet_sim_executor.rs`,
  `executor_ab_probe.rs`, `tests/clamp_merge_worker_tests.rs`) construct the
  ledger with a fresh history; the now-unused `hashbrown::HashMap` imports in
  the first two are deleted.
- `tests/curve_and_spans.rs` (`tokio_executor_merges_fast_paths_while_slow_path_solves`):
  the white-box poke at `engine.cycle.last_walk_sims` migrates to the ledger's
  own seed seam — `engine.cycle.path_cost_history.record(pid, PathCost::new(sims, 0))`.
  No test reaches a raw map or an engine cost field any more.

## Gates (foreground, scoped)

| gate | result |
| --- | --- |
| `cargo test -p degenbot-bot` | PASS — lib 715 passed / 0 failed / 3 ignored; integration backrun_resolver 9, metric_cardinality 1, observability_naming 2, with_core_packs_installed_fleet 1; 0 failed anywhere; doc-tests 0 |
| `cargo clippy -p degenbot-bot --all-targets` | PASS — finished clean, zero warnings (workspace lints deny warnings; pinned toolchain) |
| `cargo fmt -p degenbot-bot` | run (no further diff) |
| `bash scripts/hooks/comment-hygiene.sh` (extra, read-only) | PASS — exit 0 |

Acceptance evidence:

- Bin-packing behaviour unchanged: `workload_partition` unit tests pass
  unmodified, including `sims_aware_cost_prefers_measured_last_block_walk`
  (the same `Option<u64>` inputs) and the LPT determinism/load tests.
- No test reaches past the ledger: `rg 'sims_recorder|gate_recorder|last_walk_sims|last_gate_us'`
  over `rust/ src/ tests/` now hits only the `sims_aware_cost` parameter name
  (signature pinned by the unmodified unit tests) and historical result notes
  under `docs/ergo-results/`.
- The migrated streaming test was re-run explicitly and executed its body
  (8-core host, no self-skip): `cargo test -p degenbot-bot --lib -- tokio_executor_merges_fast_paths_while_slow_path_solves workload_partition`
  -> 13 passed.

## PENDING

None. Both gates completed in the foreground well inside the harness bound.

## Foreign tree activity (left alone)

- `.devcontainer/Dockerfile` — the unrelated modification named in the brief;
  untouched.
- `.agents/skills/rust-rebuild/SKILL.md` — appeared modified mid-run (another
  worker); untouched.

Not mine to run (dispatcher-owned): no nextest / whole-workspace ladder, no
whole-tree lint or format sweep, no commit (tree left dirty for review).
