# LCJDUC (M3, epic QECVJZ): thread the tally through crossings.rs + word_profile.rs; collapse the swap entry

M3 of the walk-telemetry collector migration (`docs/architecture/walk-telemetry-collector-sizing.md`
§1-§3, §5 M3 row). M1+M2 landed; this chunk threads the per-solve `&mut WalkTally` through the last
ambient writers the walk reaches and collapses the temporary `simulate_v3_range_swap_tallied`
variant back into the public entry.

## What changed (14 files, all under `rust/crates/engine/degenbot-solvers/`)

- **`src/cl/hop_sim.rs`** — the M2 `pub(crate) simulate_v3_range_swap_tallied` and the
  throwaway-tally wrapper collapse into ONE `pub fn simulate_v3_range_swap(amount_in, v3_hop,
  tally: &mut WalkTally)`, keeping the `#[must_use]` and the `cl_solve.int_simulate_v3_swap`
  hotpath label. Every caller migrates (below).
- **`src/cl/word_profile.rs`** — both writers (`:131` profile build, `:181` profile swap) now take
  `&mut WalkTally`; `ClWordProfile::build`/`swap` signatures gain the param. The hot-path
  `bump_word_steps` stays a plain `#[inline]` stack write — no atomic, no lock.
- **`src/cl/crossings.rs`** — all 10 remaining writers move onto the tally: `add_pred_ns`
  (`walk_event_first_above_predicted`), `bump_left_edge_sims` ×3, `bump_right_edge_sims` ×3,
  `bump_event_solver_ok`, `bump_event_solver_fallbacks`, and `observe_max_dense_words`
  (`build_word_profiles`). `walk_event_first_above_predicted`, `cl_hop_min_input_for_output`, and
  `build_word_profiles` gain the tally param; the pub table builders
  (`build_cl_word_profiles`/`_from_crossings`) and `cl_walk_hop_cached`'s unprofiled arm pass a
  throwaway tally, preserving the (already-zero at exit) dense-max readout. The ambient telemetry
  import block is gone.
- **`src/cl/active_set.rs`** — entry `PieceView::step`/`min_input_for_output` thread the tally; the
  M2 staging fold is deleted for every migrated field — `stats` is now `tally.walk_stats()` plus the
  single still-ambient `census` fold (`peek_walk_stats()` reads only the census now). `WalkOutcome.stats`
  stays byte-identical by construction.
- **`src/cl/path_sim.rs`** — `ClPathSim::output` reaches the collapsed entry with its existing
  throwaway tally; `ClPathSim::new` threads a throwaway into `build_word_profiles`.
- **`src/cl/telemetry.rs`** — the six migrated per-thread `Cell`s (`WALK_WORD_STEPS`,
  `WALK_LEFT_EDGE_SIMS`, `WALK_RIGHT_EDGE_SIMS`, `WALK_EVENT_SOLVER_OK`, `WALK_EVENT_SOLVER_FALLBACKS`,
  `WALK_MAX_DENSE_WORDS`), their `gated_cell_bump!` helpers, `observe_max_dense_words`, the
  `gated_atomic_add!` helper + `add_pred_ns`, and the now-dead reset/peek lines are deleted. The
  event census pair and the process-wide timing statics (still read by `cl_cache_lab`'s swap sweep
  until the M6 cut-over) remain.
- **Callers migrated** (mechanical, compiler-driven): `src/profit_envelope/gate.rs` (2 test sites),
  `src/cl/tests/mod.rs` (24 `simulate_v3_range_swap` sites + `ClWordProfile::build`/`swap` +
  `build_word_profiles` + `walk_event_first_above_predicted`), `tests/v3_crossing_solver_vs_sim_parity.rs`,
  `tests/v3_iia_fixture_reproduction.rs`, `tests/v4_crossing_solver_vs_sim_parity.rs`,
  `tests/v4_stale_state_confirmation.rs`, `tests/v4_word_boundary_solver_divergence.rs`, and
  `examples/v2_cl_parity_probe.rs`.

`tests/cl_cache_lab_goldens.rs` is UNMODIFIED and passes in both feature configurations.

## A/B/C evidence (mandatory, §5)

**A — numbers parity (feature-off, the baseline's own configuration):**

```
cd rust && cargo run --release -p degenbot-solvers --example cl_solve_replay -- \
  crates/engine/degenbot-solvers/tests/fixtures/heavy_cl_solve_captures.jsonl.zst
```

Compared against `/tmp/dmrsry-baseline/cl_solve_replay.txt`, cargo banner + wall-clock fields
normalized out (a profiled run is noise-sensitive per §5 M0):

- normalized unified diff: **0 lines**
- profit/stat-bearing lines (`input_delta`, `envelope_derivation`, `bound=`, `sims=/…/wsteps=`,
  `golden=OK`) identical: **True** (sorted diff 0)
- summary: `replayed 156 path(s) | golden 26/26 exact match | gate-skipped 130 | deterministic
  156/156 | iters=9`

**B — readout parity (`--features telemetry`):**

```
cd rust && cargo run --release --features telemetry -p degenbot-solvers --example cl_cache_lab -- \
  crates/engine/degenbot-solvers/tests/fixtures/heavy_cl_solve_captures.jsonl.zst
```

- strategy matrix vs `/tmp/dmrsry-baseline/cl_cache_lab.txt` (per-path strategy lists + mismatch
  vectors + `counters` section + trailing `total divergences 0`): **MATRIX_IDENTICAL** (diff 0).
- section ns sums sane: `edge + redge + dir + refine == 0.0 ms ≤ solve 0.0 ms` (all `0 sims`).
  Note: the ns readouts are already zero post-M2 (the writers moved onto the tally, whose merge is
  `None` for this example); `predictions` drops `0.2 ms → 0.0 ms` here as `add_pred_ns` leaves the
  atomic. The M6 cut-over makes `cl_cache_lab` drain the collector instead; only the non-ns matrix
  is asserted at M3, per the protocol.

**C — zero-ambient audit shrinks monotonically:**

```
grep -rn "thread_local!|Cell<" rust/crates/engine/degenbot-solvers/src/cl/ | grep -v tests | wc -l
```

`10` at M2 → **`3`** at M3 (the remaining three lines are the `EVENT_CENSUS` thread-local block and
its two cells — the census migrates in a later chunk).

## Gates run (foreground, scoped)

- `cargo fmt -p degenbot-solvers` → applied (rustfmt clean).
- `cargo test -p degenbot-solvers` → ok, 0 failed (lib 177 passed / 3 ignored; all integration
  targets green incl. `cl_cache_lab_goldens` 1 passed; 0 doc-tests).
- `cargo test -p degenbot-solvers --features telemetry` → ok, 0 failed (lib **178** passed / 3
  ignored; all integration targets green incl. `cl_cache_lab_goldens` 1 passed; 0 doc-tests).
- `cargo clippy -p degenbot-solvers --all-targets` → clean (rustc/clippy 1.98.1, `-D warnings`).
  Also `--all-targets --features telemetry` → clean (extra confidence, not a required gate).
- Extra confidence (not a declared gate): `cargo check -p degenbot-bot -p degenbot-substrate` → ok
  (the changed `simulate_v3_range_swap` public signature has no out-of-crate callers).

## PENDING

None. No command detached past the harness bound.

## Foreign edits / tree

Tree clean at start (`git status --porcelain` empty). No foreign edits observed. Working-tree
changes are exactly the fourteen `degenbot-solvers` files above (nothing committed — left dirty for
the manager). No other crate touched. `.ergo/plans.jsonl` untouched.

## Scope note

The brief's caller list named "~12 sites in `cl/tests/mod.rs`"; the file actually carries 24
`simulate_v3_range_swap` call sites (plus `ClWordProfile::build`/`swap` and `build_word_profiles`
uses) — all migrated. The brief also named five out-of-crate call sites it did not list
(`profit_envelope/gate.rs` tests and four `tests/v*` integration files); the public-signature change
forced them, and they were migrated mechanically. The brief's "`ClPathSim::simulate`" is the
`ClPathSim::output` call site (M2 already gave it a throwaway tally), kept unchanged externally.
