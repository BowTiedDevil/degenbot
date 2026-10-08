# WalkMemo cross-block reuse readout: the honest A/B and the gated follow-ups

**Status: readout landed (ergo N754F3). The three follow-up decisions below stay
explicitly gated — two need live data this static replay cannot produce, one is
closed as a doc note.**

The cross-block composition memo (`WalkMemo` in
`rust/crates/engine/degenbot-solvers/src/cl/memo.rs`) is live on the engine path:
`arb_engine::solve_cycle` advances `begin_block(solve_block)` before the per-path
probes (solve_cycle.rs:1176) and threads the engine-owned handle into every solve
(`gate_deps.walk_memo = Some(&memo)`, solve_cycle.rs:2087). This note records the
before/after readout the epic asked for, plus what each follow-up decision still
needs before anyone acts on it.

## Method

`cargo run -p degenbot-solvers --example walkmemo_readout`
(`rust/crates/engine/degenbot-solvers/examples/walkmemo_readout.rs`, new in this
change) loads the same capture fixture the other replays use
(`heavy_cl_solve_captures.jsonl` via `degenbot_solvers::capture_fixture`), derives
`ClSolveTables` ONCE per captured path (the production shape — the A/B must not
measure table derivation), then runs `E` epochs (default 5,
`DR_READOUT_EPOCHS` to override) over the FULL path set — 156 captured paths, 12
distinct `path_id`s, 2 hops each — in two arms:

- **Arm A (memo off, the "before")**: `solve_cl_piecewise` with `None` memo every
  epoch; per-epoch total walk sims + wall time.
- **Arm B (memo on, stats on, the "after")**: one `WalkMemo::new(true, true)`,
  `begin_block(epoch)` at each epoch start BEFORE the per-path probes (the engine
  contract); every path solved with `Some(&memo)`; `take_stats()` closes the
  epoch.

### The honesty caveat (read before quoting any number)

The fixture is per-path, not per-block — there is no natural epoch structure in a
static capture. This replay therefore measures the memo at **SYNTHETIC MAXIMUM
recurrence**: every composition in the set recurs every epoch by construction, so
epoch 1 is the only cold epoch and epochs 2+ are pure cache replay. It does NOT
measure live block-to-block recurrence (real traffic revisits a composition only
when its pools re-appear in a later block). Every counter that WOULD decide the
live questions — `probes` vs `hits` vs `cache_plays` divergence across blocks,
`negative_entries` as a running gauge — is in place and reported here; what is
missing is a capture with real epoch structure, not instrumentation.

A second corpus-specific caveat: the walk's sim counters (`sims`,
`probes_sims`, `hits_sims`) read **0 on this corpus**. That is the corpus, not a
bug: the existing baseline harness `cl_solve_replay` reports the same all-zero
`sims/refine/pieces/wsteps` row set for these exact paths (with goldens matching),
so the current walk resolves these compositions without full-path
`simulate_walk_path` calls. The sim columns are reported as recorded and carry no
signal here; the wall-time column is the cost signal.

## The readout (real run)

From an actual run of the example (exit 0; log kept at `/tmp/walkmemo_readout_run.log`):

```text
WalkMemo readout: 156 captured path(s) (12 distinct path_id(s)) x 5 epoch(s), tables derived once per path, fixture=/workspaces/degenbot/rust/crates/engine/degenbot-solvers/tests/fixtures/heavy_cl_solve_captures.jsonl

== Arm A: memo off (baseline) ==
epoch  paths  walk_sims  wall_ms
    1    156          0     26.8
    2    156          0     26.8
    3    156          0     26.9
    4    156          0     26.6
    5    156          0     26.5
TOTAL                 0    133.5

== Arm B: memo on, stats on (begin_block per epoch, take_stats at epoch end) ==
epoch  probes  hits  distinct  cache_plays  neg_played  neg_entries  probe_sims  hit_sims  walk_sims  wall_ms
    1     156     0        99          156          47           83           0         0          0     19.5
    2     156   156        99          156         130           83           0         0          0      2.8
    3     156   156        99          156         130           83           0         0          0      2.8
    4     156   156        99          156         130           83           0         0          0      2.8
    5     156   156        99          156         130           83           0         0          0      2.8
TOTAL     780   624     (last)          780         567           83           0         0          0     30.7
(distinct + neg_entries are per-epoch cardinality / a running gauge — the TOTAL row shows the final epoch's values, not sums)

VERDICT: epoch 1 = cold (hits=0; A 26.8ms vs B 19.5ms — within-epoch duplicate probes already play the cache); epochs 2+ = memo arms (624/624 probes hit, 780 cache plays, 567 cached negatives skipped); memo arm ran 30.7ms of 133.5ms across all 5 epoch(s) (77% wall avoided) and 0 of 0 walk sims (0% avoided)
NOTE: static-fixture replay = SYNTHETIC MAXIMUM recurrence (every composition recurs each epoch), NOT a live block-to-block measurement.
```

How to read it:

- **Epoch 1 is cold by construction** (`hits` counts only previous-epoch census
  membership, and the previous-epoch census starts empty). Even so the memo earns
  its keep within the epoch: 156 probes cover 99 distinct compositions, so 57
  duplicate probes play the cache — 47 of them cached negatives (`neg_played=47`),
  which is why Arm B epoch 1 (19.5 ms) undercuts Arm A (26.8 ms).
- **Epochs 2+ are the synthetic-maximum regime**: 156/156 probes hit, 130/156
  answered from cached negatives (83 of the 99 distinct compositions solve to
  `None` under this corpus and are skipped without a walk), per-epoch cost drops
  26.8 ms → 2.8 ms. `negative_entries` holds at 83 across the census-scoped
  evictions — the gauge mirrors the live cache exactly as designed.
- **77% wall avoided over 5 epochs** is the synthetic-maximum number. It is an
  upper bound, not a live estimate; do not quote it as expected production
  savings.

## Gated follow-up decisions

### (a) Census retain vs LRU ring — STILL GATED (needs a live multi-block run)

Cannot be decided from synthetic maximum recurrence: under max recurrence every
cached composition recurs every epoch, so census retain trivially wins (it keeps,
by definition, everything that recurs) and an LRU ring could only lose. The
deciding signal is **live `cache_plays` vs `hits` divergence across blocks**:
`cache_plays` counts every consult, `hits` counts only previous-epoch census
membership. When a real block sequence shows consults served from entries the
census would have evicted (or the reverse — census-retained entries that never
get played again), the retention window is wrong in a measurable direction. The
counters are in place; what is needed is an **engine-driven multi-block capture
run** (a capture with real per-block epoch structure replayed through the engine
path so `begin_block` advances on real block boundaries). No task spun yet by
this worker — the PM should spin it when a live capture is available.

### (b) Per-hop `source_fingerprint` gate share — PENDING (measurement, not decision)

The concern: the memo entries re-derive fingerprints the resolve intake already
computed (`ClSolveTables::source_fingerprint` is checked against
`walk_path_fingerprint(&[seq])` at every solve entry, entries.rs:87-93 and the
mixed equivalent), roughly three passes over the tick data per solve, before the
memo probe can even run.

Facts established from code (no measurement):

- `hotpath::measure` is an **external crate attribute**
  (`hotpath = { version = "^0.28.5", default-features = false }` in
  `degenbot-solvers/Cargo.toml`), not workspace-local code; the spans expand to
  nothing unless the `hotpath` Cargo feature is enabled
  (`hotpath = ["hotpath/hotpath", "hotpath/threads"]`).
- The span labels that exist and would decide this:
  - `cl_solve.int_solve_cl_path` — `solve_cl_piecewise` (entries.rs:73),
  - `cl_solve.exact_solve_mixed_path_n` — `solve_mixed_piecewise` (entries.rs:160).

The share measurement is **PENDING**: it needs a telemetry-enabled live run and
cannot be produced cheaply offline (the spans are compile-time no-ops in default
builds, and the guard is engine-lifetime: `degenbot-bot` is a Python-driven
cdylib, so the report comes from the pump's `HotpathGuard`, not from a unit
harness). The exact command a future telemetry-enabled run should use (the
documented pattern from `degenbot-bot/src/profiling.rs`):

```text
DEGENBOT_HOTPATH=1 \
HOTPATH_OUTPUT_FORMAT=json \
HOTPATH_OUTPUT_PATH=hp.json \
HOTPATH_REPORT=functions-timing,threads \
HOTPATH_SHUTDOWN_MS=30000 \
uv run python ...   # bot runner over a live/multi-block capture; build with: cargo build -p degenbot-bot --features hotpath
```

Then read `hp.json`'s functions-timing section, filter the two `cl_solve.*`
labels, and compute their share of the solve path before touching a deliberate
safety gate (e.g. pairing the tables to positions by index instead of
re-folding).

### (c) Substrate intake asymmetry — CLOSED AS DOC NOTE (do not thread the memo)

Code finding: the `None`-memo + `SolveRuntimeConfig::default()` call sites in
`degenbot-substrate/src/resolve/mod.rs` (`all_cl_solve`, solve_cl_piecewise at
line ~949; `mixed_solve`, solve_mixed_piecewise at ~1006) are **inside
`#[cfg(test)] mod tests` (mod.rs:519)** — they are the winner-promotion parity
gates (mod.rs:776), which verify that the fused-epoch projection memo changes
build cost only, with solver intake byte-exact memo on/off. They are an
**offline/verification intake, not the live hot path**.

The live hot path already threads both: the engine passes its own runtime config
and the engine-owned memo handle (`gate_deps.walk_memo = Some(&memo)` +
`runtime_cfg`, solve_cycle.rs:2062-2087), and the production mixed solver
forwards the memo through (`mixed/solve.rs:471,479`).

Recommendation (picked from what the code shows, not preference): **document why
the substrate intake stays memo-less** — it must stay memo-less, because the
parity gate's whole value is solving the same compositions with and without the
projection memo under a fixed, memo-less solver stance; threading a `WalkMemo`
through it would defeat the byte-exactness check it exists to make. There is no
production asymmetry to close, so no follow-up task is needed for this item.

## What this change did and did not do

- Added: `examples/walkmemo_readout.rs` (example-grade, relaxed-lint header,
  exits nonzero on fixture/parse failure) and this doc. No production `src/`
  changes, no test changes.
- Did not do: any ergo state change (the PM owns it), any live-recurrence
  claim, and the (b) share measurement (PENDING as recorded above).
