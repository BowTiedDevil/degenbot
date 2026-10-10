# KPIUCR (M6, epic QECVJZ): `cl_cache_lab` cut-over + the ambient table demolished

M6 of the walk-telemetry collector migration (`docs/architecture/walk-telemetry-collector-sizing.md`
§5 M6 row). M1-M5 landed: the per-solve `WalkTally` threads every walk writer, the solve entries take
`Option<&WalkTelemetry>`, and the bot's `BlockScope` owns the collector `Arc`. This chunk is the
cut-over and the demolition — after it, `src/cl/` holds no ambient counter of any kind.

## What changed (5 files, all under `rust/crates/engine/degenbot-solvers/`)

- **`src/cl/telemetry.rs`** — the whole ambient surface deleted: the `thread_local!` block
  (`EVENT_CENSUS` + `EVENT_CENSUS_PIECES`), the twenty process `AtomicU64` statics (the nineteen
  live ns counters plus the dead `WALK_CENSUS_SIMNS`), and `reset_walk_stats` /
  `peek_walk_stats` / `take_event_census_pieces` (with their `_inner` halves and the now-unused
  `WalkStats` import). `event_census_record_inner` writes the census straight into
  `tally.census_mut()` and the piece log through `tally.record_census_pieces` — no reset, no
  thread-lifetime carry, no `EVENT_CENSUS` pair. What survives is the census value type
  (`WalkEventCensus` + `accumulate_event_census`, still used by `cl_solve_replay`) and the
  `Mark` section helper, now purely a tally-delta helper.
- **`src/cl/active_set.rs`** — the entry no longer resets or peeks anything: `tally.walk_stats()`
  projects the census directly and `tally.take_census_pieces()` drains the per-solve log; the
  single collector merge stays where M2/M5 put it. Import block trimmed to
  `{event_census_record, Mark, WalkEventCensus}`. The `WalkStats` counter-policy doc no longer
  cites the deleted macros.
- **`src/cl/mod.rs`** — the twenty static re-exports leave the public surface; `telemetry` now
  re-exports `WalkEventCensus` only. Module map line updated.
- **`examples/cl_cache_lab.rs`** — the nineteen `swap(0, Relaxed)` reads become one
  `tele.drain_ns()` plus the field reads that feed the SAME four readout lines (identical shape:
  anchor split / walk-sim+anchors+predictions+solve / sections / in-section sim total). A collector
  is constructed in `main` and threaded into `solve_prepared` (`Some(tele)`) and into a new
  `solve_reference` helper — `derive_and_solve_cl_piecewise`'s body with the collector threaded
  through, so the "(all strategies+reference)" line stays true and the ns census keeps the same
  population it counted before the writers moved onto the tally. The offline wrapper itself is
  untouched (it still threads `None`), as §2.3 requires. Under `--features telemetry` the readout
  asserts `ns.solve > 0` — the epic's "prove the merge path is live" line.
- **`src/cl/tests/mod.rs`** — the two comment/message references to `reset_walk_stats` reworded to
  the per-solve tally contract. (No TLS helper reads survived M1-M5; the file already read
  `WalkOutcome.stats`.)

Net: **101 insertions / 226 deletions** across the five files (−125 lines).

## A/B/C evidence (mandatory, §5)

**A — numbers parity (default build, the baseline's own configuration):**

```
cd rust && cargo run --release -p degenbot-solvers --example cl_solve_replay -- \
  crates/engine/degenbot-solvers/tests/fixtures/heavy_cl_solve_captures.jsonl.zst
```

vs `/tmp/dmrsry-baseline/cl_solve_replay.txt`, cargo banner dropped, wall-clock fields normalized
out (`median/p95/min=…us`, `envelope_derivation=…us`):

- 345 harness lines on both sides; sorted normalized diff = **one line**, and it is the pure
  timing-summary tail (`heaviest median: path 7 = 1298us` vs `1400us`).
- every stat-bearing line is byte-equal (`sims=`, `refine=`, `pieces=`, `wsteps=`, `left=`, `right=`,
  `anchor=`, `evtslv=`, `golden=OK`, `bound=`, `floor …` blocks, `EVENT CENSUS` block).
- summary: `replayed 156 path(s) | golden 26/26 exact match | gate-skipped 130 | deterministic
  156/156 | iters=9`.

**B — readout parity (`--features telemetry`):**

```
cd rust && cargo run --release --features telemetry -p degenbot-solvers --example cl_cache_lab -- \
  crates/engine/degenbot-solvers/tests/fixtures/heavy_cl_solve_captures.jsonl.zst
```

vs `/tmp/dmrsry-baseline/cl_cache_lab.txt`, all float tokens masked (every remaining float on these
lines is a timing):

- matrix **IDENTICAL** — 65 non-blank harness lines each, sorted masked diff = **0 lines**.
  Per-path strategy lists, mismatch vectors, per-transition averages, class rebuild deltas,
  `counters` block, and `total divergences 0` all byte-equal; exit code 0.
- the ns lines are now collector-drained and agree within noise:

  | line | baseline | this run |
  |---|---|---|
  | walk-sim | 56.4 ms | 57.0 ms |
  | anchors | 1.6 ms | 1.7 ms |
  | predictions | 0.2 ms | 0.2 ms |
  | solve | 65.0 ms | 65.4 ms |
  | sections (edge/redge/dir/refine) | 736 / 1312 / 1392 / 117472 sims | 736 / 1312 / 1392 / 117472 sims |

  The sims counts are exact; the ms values move <12 % — the same spread the M0 run shows run-to-run.
- `assert!(ns.solve > 0)` fired clean under the telemetry feature (rc=0), proving the per-solve merge
  path is live. In the default build the same lines print zeros, matching the crate's documented
  zero-overhead stance.

**C — zero-ambient audit (the epic's acceptance line) hits ZERO:**

```
rg "thread_local!|Cell<" rust/crates/engine/degenbot-solvers/src/cl/ | grep -v tests
```

→ **empty output** (rg exit 1). Full-tree form without the filter is also empty — the census pair
was the last remaining hit. Progression: 10 at M2 → 3 at M3 → **0** here.

## Gates run (foreground, scoped)

- `cargo fmt -p degenbot-solvers` → applied, no reformat needed.
- `cargo test -p degenbot-solvers` → ok, **177 passed / 3 ignored, 0 failed** across all targets
  (13 result blocks; 0 doc-tests).
- `cargo test -p degenbot-solvers --features telemetry` → ok, **178 passed / 3 ignored, 0 failed**
  (the extra test is the `#[cfg(feature = "telemetry")]` per-path dense-max test).
- `cargo clippy -p degenbot-solvers --all-targets` → **clean** (rc 0, no warnings) on the pinned
  1.98.1 toolchain. Also `--all-targets --features telemetry` → clean (extra confidence, not a
  declared gate).
- `cargo check -p degenbot-solvers --all-targets` in both feature configurations → ok.

## PENDING

None. No command detached past the harness bound.

## Foreign edits / tree

Tree clean at start (`git status --porcelain` empty). No foreign edits observed during the run; the
working tree carries exactly the five files above, nothing committed (left dirty for the manager).
No other crate touched. `.ergo/plans.jsonl` untouched.

## Scope notes

- The brief's "convert `cl/tests/mod.rs`'s TLS helpers to tally reads" was already satisfied by
  M1-M5 — those helpers read `WalkOutcome.stats` on the current tree; only two stale
  `reset_walk_stats` mentions remained in comments and are reworded.
- The brief's "both gating macros" and `clear_walk_pieces_and_sims` / `path_simulations_now` are
  already gone (deleted in M2/M3); this chunk removes the last of the table they gated.
- The example's new `solve_reference` mirrors the offline wrapper's body with `Some(tele)` instead
  of `None`. Without it the printed "(all strategies+reference)" line would have silently dropped
  the reference arm's cost (the wrapper threads `None` by design), and B's "compare within noise"
  would have read as a real regression. The wrapper's own signature and callers are unchanged.
