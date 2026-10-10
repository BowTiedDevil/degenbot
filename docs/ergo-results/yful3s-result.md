# YFUL3S — M7 close-out: walk-telemetry collector parity vs the M0 baseline

Task YFUL3S (epic QECVJZ / DMRSRY). Docs-only close-out of the walk-telemetry collector
migration. Baseline: `/tmp/dmrsry-baseline/` (M0, captured before the migration); final tree:
`main` @ d85dfa712 plus this task's docs edits.

## 1. Numbers parity — cl_solve_replay

Capture caveat (verified): the M0 baseline file `/tmp/dmrsry-baseline/cl_solve_replay.txt` was
captured with `2>&1` (stderr merged). The `path N: envelope_derivation=…` **and**
`path N: input_delta=…` lines are `eprintln!` **stderr**, not stdout. A fresh capture of the
final tree with the streams split shows fresh **stdout has 0 `input_delta` lines** while fresh
**stderr has 26** (`input_delta`, 13 iters x paths 4 and 105) plus 156 `envelope_derivation`
lines. So a naive byte-diff of the whole baseline file against stdout-only is impossible by
construction; the comparison was done **stdout-vs-stdout** (baseline stderr lines stripped) and
**stderr-vs-stderr** for the profit vector.

Command:
`cargo run --release -p degenbot-solvers --example cl_solve_replay -- crates/engine/degenbot-solvers/tests/fixtures/heavy_cl_solve_captures.jsonl.zst`

Result: **exact parity.** After normalizing the wall-clock fields only (`median/p95/min=NNus`,
`captured(t=NNus,`, the final `heaviest median:` figure):

- stdout deterministic content — 163 lines, `diff` empty.
- stderr profit vector (`input_delta`/`under_shoot`/`over_shoot`) + envelope `bound=` values —
  182 lines, `diff` empty.

Deterministic fields confirmed identical: `golden=OK` on every path, every `WalkStats` counter
(`sims=0 refine=0 pieces=0 wsteps=0 left=0 right=0 anchor=0 evtslv=0/0`, all zero because the
replay runs the default feature build — telemetry off — exactly as the baseline), every
`ranges/hop=[…]`, every `n_word_bounds=…`, the four `floor …` gate lines, and the summary
`replayed 156 path(s) | golden 26/26 exact match | gate-skipped 130 | deterministic 156/156 | iters=9`.
The only differences are the wall-clock medians (e.g. heaviest median path 7 1298us -> 1331us),
i.e. run-to-run timing noise, not a counter change.

## 2. Readout parity — cl_cache_lab

Command:
`cargo run --release --features telemetry -p degenbot-solvers --example cl_cache_lab -- crates/engine/degenbot-solvers/tests/fixtures/heavy_cl_solve_captures.jsonl.zst`

Baseline `/tmp/dmrsry-baseline/cl_cache_lab.txt` also merged stderr; cargo lines stripped.

Result: **matrix identical; ns sections collector-drained within noise.**

- Every path's `mismatches=[...]` row is all-zero and byte-identical to baseline.
- Class rebuild delta rows, the `---- counters ----` block (S0…S7 crossing/profiles/
  seq_rebuilds/partial/solves) and the `total divergences 0` line are byte-identical.
- Only the ms wall-clock values moved, all within ~1–8%: walk-sim 56.4 -> 57.3 ms (+1.6%),
  anchors 1.6 -> 1.7 ms, predictions 0.2 -> 0.2 ms, solve 65.0 -> 65.9 ms; sections edge
  0.1 -> 0.2, redge 0.9 -> 0.9, dir 0.8 -> 0.8, refine 60.1 -> 60.8 ms.

The ns sections are now **non-zero and drained from the collector** (`tele.drain_ns()` in
`cl_cache_lab.rs`): pre-migration they came from the ambient statics, so a live merge path is
the only way they can read non-zero now — the M6 plan's "assert ns > 0 to prove the merge path
is live under `--features telemetry`" check passes.

## 3. Zero-ambient audit

`grep -n 'thread_local!\|Cell<' rust/crates/engine/degenbot-solvers/src/cl/ | grep -v tests`

Result: **no matches (ZERO)**, grep exit 1. The ambient walk-telemetry table (TLS cells plus
process statics) is gone; the only read/reset seam left is the owned collector.

## 4. Contention canary — walkmemo_contention

Bench: `cargo bench -p degenbot-solvers --bench walkmemo_contention`
(baseline `/tmp/dmrsry-baseline/walkmemo_contention.txt`).

Baseline `walkmemo_hits/set64/w1` center 18.36 us (interval 18.16-18.57); current 19.55 us
(19.10-19.96), a ~6% center move whose intervals just separate — BUT the M0 baseline run itself
drifted +7.7/+9.6/+11.5% (p=0.03) against its own saved estimates, so this bench's run-to-run
band on this machine is about ±10%: the delta sits inside it and is not attributable to the
per-solve collector merge without a pinned-machine re-run (taskset + release profile, per the
sizing doc's M0 advisory). Verdict: no regression demonstrated; the merge is one parking_lot
lock per solve, off the per-path probe loop. Capture-convention note for future A/Bs: the
example's per-path lines are stderr (eprintln!) — capture with 2>&1 on both sides.

## 5. Documentation updates landed

- `rust/crates/engine/degenbot-solvers/src/cl/tally.rs` module docs: cross-linked
  [ADR-043](adr/ADR-043-observability-standard.md) — the collector and its drain are the
  solver-internal case of the observability standard (owned, drained state; never a per-event
  console emit, never a process-global ambient cell), naming `drain_ns`/`take_census_pieces`
  as the drain seam.
- `docs/cache-lab-report.md` §Method: noted the ns sections now drain from the
  `WalkTelemetry` collector.
- `GLOSSARY.md`: `WalkTally` and `WalkTelemetry` verified accurate against the final shape;
  `WalkNsCensus` updated to name the three anchor sub-timers (build/compose/argmax) the
  registered text omitted relative to the shipped 19-counter struct.

## Gates

- `npx --yes markdownlint-cli2 GLOSSARY.md docs/cache-lab-report.md` -> 0 issues (the
  pre-commit hook runs the same linter).
