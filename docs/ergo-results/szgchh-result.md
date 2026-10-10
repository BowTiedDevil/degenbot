# SZGCHH (M2, epic QECVJZ): thread the tally through active_set.rs

M2 of the walk-telemetry collector migration (`docs/architecture/walk-telemetry-collector-sizing.md`
§1-§3, §5 M2 row). The active-set walk now owns its per-solve accumulator on the solving stack and
threads it by `&mut` to every writer it owns; the walk entry gains `tele: Option<&WalkTelemetry>` and
the ONE cfg'd merge into the engine collector. M1's skeleton is now consumed.

## What changed (8 files, all under `rust/crates/engine/degenbot-solvers/src/cl/`)

- **`active_set.rs`** — `solve_active_set_path` creates a `WalkTally`, threads `&mut` through
  `solve_active_set_path_inner`, `walk_refine_window` (now 8 args — `#[expect(too_many_arguments)]`),
  `refine_at_stop`, `landed_beyond`, `WalkRecorder::eval_and_record`, `simulate_walk_path`/`_inner`,
  `PieceView::step`, and the anchor block. The entry folds the still-ambient counters (see staging
  note) and merges the tally once at exit under `#[cfg(feature = "telemetry")]`. `Mark` now carries
  the tally (`Mark::start(tally)` / `Mark::commit(tally, WalkSection)`), replacing the six
  `WALK_CENSUS_*` static writes. `anchor_sims` is a `tally.sims` delta now (was
  `path_simulations_now()`), same arithmetic. The hot-path `word_steps` bump is a plain `#[inline]`
  stack write — no atomic, no lock.
- **`hop_sim.rs`** — `simulate_v3_range_swap` keeps its public shape (M3 owns its signature) and
  delegates to a new `pub(crate) simulate_v3_range_swap_tallied(amount_in, v3_hop, tally)`, so the
  `hop_sim.rs:102` word-step writer lands in the per-solve tally instead of the ambient cell.
- **`crossings.rs`** — the walk-internal helpers the walk reaches (`piece_window_left_edge`,
  `piece_window_right_edge`, `piece_window_right_edge_evented`, `piece_window_right_edge_seeded`)
  gain a `&mut WalkTally` pass-through so `simulate_walk_path` compiles; their own counter writers
  stay ambient (M3).
- **`telemetry.rs`** — `Mark` rewritten to carry the tally; `event_census_record` threads the tally
  through the verify probes; the ambient table shrinks: the six per-solve cells whose writers M2
  migrated to the tally (`WALK_PIECES_VISITED`, `WALK_PATH_SIMULATIONS`, `WALK_REFINE_SIMS`,
  `WALK_TERNARY_SIMS`, `WALK_GRID_SIMS`, `WALK_ANCHOR_SIMS`) plus their `gated_cell_bump!`/reset/peek
  lines and the six now-unused `gated_atomic_add!` helpers are deleted. `reset_walk_stats` /
  `peek_walk_stats` / `take_event_census_pieces` survive (peek now only reports the still-ambient
  fields).
- **`tally.rs`** — added the two snapshot accessors `sims()` and `sim_ns()` the timed-section helper
  needs.
- **`entries.rs`** — the two `solve_active_set_path` call sites pass `None` for the collector (M4
  threads the real engine handle through `GateDeps`).
- **`tests/mod.rs` / `path_sim.rs`** — ambient TLS reads become `WalkOutcome.stats` reads, and the
  `simulate_walk_path` call sites pass a throwaway/default tally.

## Staging note (why `peek_walk_stats` survives)

`crossings.rs`'s own counters (`left_edge_sims`, `right_edge_sims`, `event_solver_*`),
`word_profile.rs`'s `word_steps`, and `max_dense_words` still write the ambient cells — those
writers are M3's `crossings.rs` + `word_profile.rs` chunk. To keep `WalkOutcome.stats` byte-identical
in the telemetry build, the entry drains the ambient table at entry and folds only those
still-ambient fields into the tally projection (see the entry body). M3 threads them; M6 deletes
`peek`/`reset`.

## D3 note (pre-settled — no behavior delta)

BPH3ZW already landed the per-solve `max_dense_words` reset (`telemetry.rs` `reset_walk_stats_inner`),
so the entry still drains it per solve; the per-solve tally merely carries that current behavior
forward. No alert-firing delta is recorded at M2.

## A/B evidence (mandatory, §5)

**A — numbers parity (feature-off, the baseline's own configuration):**

```
cd rust && cargo run --release -p degenbot-solvers --example cl_solve_replay -- \
  crates/engine/degenbot-solvers/tests/fixtures/heavy_cl_solve_captures.jsonl.zst
```

Output compared against `/tmp/dmrsry-baseline/cl_solve_replay.txt` (program output only; the cargo
compile banner lines are dropped). Only wall-clock fields (`…us`, `median/p95/min`) differ; after
normalizing those (a profiled run is noise-sensitive per §5 M0), the diff is **0 lines**:

- normalized unified diff: `0`
- profit-bearing lines (`input_delta`, `envelope_derivation`, `bound=`) identical: `True`
- `WalkStats` lines (`sims=/refine=/pieces=/wsteps=/left=/right=/anchor=/evtslv=`) identical: `True`
- summary: `replayed 156 path(s) | golden 26/26 exact match | gate-skipped 130 | deterministic
  156/156 | iters=9`

(The default build reports zeroed `WalkStats`, exactly as the baseline does.)

**C — zero-ambient audit shrinks:**

```
rg "thread_local!|Cell<" rust/crates/engine/degenbot-solvers/src/cl/ | grep -v tests | wc -l
```

`16` at HEAD (pre-M2) → **`10`** at M2. (Still 10: the event census pair, the remaining
word-profile/edge/event-solver/dense cells, and their `thread_local!` blocks — M3/M6 close them.)

## Gates run (foreground, scoped)

- `cargo fmt -p degenbot-solvers` → applied (rustfmt clean).
- `cargo test -p degenbot-solvers` → ok, 0 failed (lib 177 passed / 3 ignored; all integration
  targets green; 0 doc-tests).
- `cargo test -p degenbot-solvers --features telemetry` → ok, 0 failed (lib **178** passed / 3
  ignored — the telemetry-only per-path `max_dense_words` test included; all integration targets
  green).
- `cargo clippy -p degenbot-solvers --all-targets` → clean (rustc/clippy 1.98.1, `-D warnings`).
  Also `--all-targets --features telemetry` → clean (extra confidence, not a required gate).
- Extra confidence (not a declared gate): `cargo check -p degenbot-bot -p degenbot-substrate` → ok
  (the public solver API is unchanged, so downstream is unaffected).

## PENDING

None.

## Foreign edits / tree

Tree clean at start (`git status --porcelain` empty). No foreign edits observed. Working-tree changes
are exactly the eight `degenbot-solvers/src/cl/` files above (nothing committed — left dirty for the
manager). No other crate touched. `.ergo/plans.jsonl` untouched.

## Scope note

M2's brief named `active_set.rs`, `simulate_walk_path`, and the `hop_sim.rs:102` writer. Threading
`simulate_walk_path`'s new `&mut WalkTally` parameter forces pass-through signature changes in the
`crossings.rs` walk-internal helpers (their writers stay ambient for M3) and in the in-crate test
call sites; `hop_sim` is threaded via a `pub(crate)` tallied variant so `simulate_v3_range_swap`'s
public signature (M3's item) is untouched.
