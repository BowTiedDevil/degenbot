# WalkMemo cross-block reuse readout: the honest A/B and the gated follow-ups

**Status: static readout landed (ergo N754F3); the live multi-block capture
landed (ergo 3S7HGF) and resolves two of the three follow-ups below — (a) with
data, (b) by bounds. (c) stays closed as a doc note. The synthetic-caveat
framing is unchanged: the fixture numbers measure synthetic maximum recurrence;
the live-capture section records what real traffic actually does, kept
explicitly separate.**

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

### (a) Census retain vs LRU ring — RESOLVED BY THE LIVE CAPTURE (ergo 3S7HGF): retention is immaterial at ~1 percent recurrence

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

Resolution (ergo 3S7HGF): the live capture below is exactly the run this
section asked for, and it closes the question — see "Decision (a) — resolved
with data" in the live-capture section.

### (b) Per-hop `source_fingerprint` gate share — RESOLVED BY BOUNDS (ergo 3S7HGF): keep the gate

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

Resolution (ergo 3S7HGF): the direct telemetry run stays blocked (the guard
lives in `BlockPump` and the umbrella does not forward `hotpath`), but the
probe-first A/B already measured bounds that answer the question. Verdict:
**keep the deliberate safety gate** — the arithmetic and the two enabling
follow-ups are in the live-capture section below.

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

## The live capture (ergo 3S7HGF): a real multi-block run through the engine path

This is the run decision (a) was gated on: `begin_block` advancing on real
mainnet block boundaries with the memo live on the engine path, feeding the
counters the fixture readout could only report at synthetic maximum. The
synthetic-caveat section above still governs the fixture numbers and is not
weakened here — the fixture replay remains a synthetic-maximum measurement;
this section is the live measurement the fixture could not produce. The two
are kept separate everywhere below.

### Method

The chassis is the pure-Rust settlement driver (`rust/examples/settlement_bot`
— the external-consumer parity twin whose entire surface is the umbrella
`degenbot` crate), driven via the public `degenbot::EngineDriver` (`start` →
`take_result_receiver` → `resume` → `stop`; the main.rs parity-ledger rows
6–8 handshake). The run is **non-live by posture**: no `--live` was passed
to the direct invocation (and the launcher form never implies it either) —
the driver observes the node's chain feed and solves; nothing is submitted.

The exact command — the direct invocation, from the repo root, DEV (debug)
profile, with `SMOKE_RPC_URL` exported by hand from the cascade-provided
`DEGENBOT_RPC_WS_CHAINID_1`. This is what produced the JSONL slice this
readout records:

```bash
SMOKE_RPC_URL="$DEGENBOT_RPC_WS_CHAINID_1" \
DEGENBOT_SMOKE_MAX_SECS=10800 \
DEGENBOT_SOLVER_WALK_MEMO=1 \
DEGENBOT_SOLVER_WALK_MEMO_STATS=1 \
cargo run --manifest-path rust/Cargo.toml -p degenbot-settlement-bot-example
```

The launcher form is the canonical reproduction alternative — the wrapper
equivalent, not what produced the recorded data: the launcher resolves the
four-layer config cascade, binds `SMOKE_RPC_URL` to the ws URI the cascade
resolves, builds `degenbot-settlement-bot-example` on demand in the
`release` profile by default (`RUST_PROFILE=dev` to match the recorded run),
and writes console + engine output to `logs/bot_run.log`:

```bash
DEGENBOT_RPC_WS_CHAINID_1='ws://host.containers.internal:8546' \
DEGENBOT_SMOKE_MAX_SECS=10800 \
DEGENBOT_SOLVER_WALK_MEMO=1 \
DEGENBOT_SOLVER_WALK_MEMO_STATS=1 \
./run_bot.sh --rust start
```

Environment and boot facts:

- Node: the environment **archive** node at
  `ws://host.containers.internal:8546` (`DEGENBOT_RPC_WS_CHAINID_1`,
  devcontainer-provided; the direct run exported `SMOKE_RPC_URL` by hand
  from this var, and the launcher's `arm_smoke_rpc` binds it from the same
  URI the Python cascade resolves).
- `DEGENBOT_SMOKE_MAX_SECS=10800` — the 3-hour bounded observation window
  (`run_loop.rs`; the session ends `WindowExpired` at the bound).
- `DEGENBOT_SOLVER_WALK_MEMO=1` + `DEGENBOT_SOLVER_WALK_MEMO_STATS=1` — the
  memo and its stats stances (`SolveRuntimeConfig::memo_on` / `memo_stats`,
  `degenbot-solvers/src/runtime.rs:39-43`). Both **default OFF**
  (`SolveRuntimeConfig::default()`, runtime.rs:69-70): a first drive without
  them proved the tap fully inert — a disabled memo never counts, and the
  tap's zero-activity gate emits no line, no instrument, no IO — so the JSONL
  records nothing until both flags are set.
- Boot: discovery over the snapshot DB (**768,314 pools**), then
  **1,000,000 paths registered** against the engine registry (the
  `pathfinding.max_registered_paths` cap; zero rejects). These two are
  chassis boot facts from the run's console, which IS retained: the console
  output was teed to `/tmp/drive-scaled.log` — a runtime artifact outside
  the repo, like the JSONL — and it holds the full run including the boot
  facts (the run was the direct `cargo run` invocation above, not
  `run_bot.sh`, so the launcher's `logs/bot_run.log` truncation-at-start
  never applied). Not JSONL-recomputable (the JSONL carries no boot fields
  by design), but recomputable from the retained console log.

### Regeneration path

The node is a persistent archive node, so the same direct command regenerates
an equivalent run at any time — the block range and the rates re-measure; what
reproduces is the shape (recurrence ~1 percent, bursty). The JSONL is
append-only across runs (writer contract in `arb_engine/walk_telemetry.rs`):
runs never truncate it and are separable by line content (`ts` / `epoch` /
`block`), so successive captures never alias one another's epochs. Delete
`logs/walkmemo_stats.jsonl` first for a single-run file; otherwise slice by
timestamp as this readout does.

### The per-epoch tap (the durable instrument)

The tap is new in this ergo
(`degenbot-bot/src/arb_engine/walk_telemetry.rs`): the engine-side drain the
memo always needed (`take_stats` existed with no engine caller). One boundary
drain per `begin_block` site, issued BEFORE the memo advances
(solve_cycle.rs:1182 — adjacent to the `begin_block(solve_block)` the first
readout cites at solve_cycle.rs:1176), plus one final drain at engine
teardown (engine_stages.rs:384) so the last epoch's counters are not lost.
Emission: one JSON line to `logs/walkmemo_stats.jsonl` (CWD-relative; the
launcher cds to the workspace root) plus the ADR-043 instruments
`degenbot.solver.walk_memo_probes` / `_hits` / `_cache_plays` /
`_negatives_played` (counters) and `degenbot.solver.walk_memo_negative_entries`
(gauge) behind the `otel` feature. Gate: only records with activity emit —
the first boundary of a run, a disabled memo, and quiet epochs emit nothing.

JSONL semantics (the module doc is the contract — "read before aggregating"):
each line carries `ts`, `reason` (`epoch_boundary` / `final_drain`), `epoch`
(the solved block whose probes these are), and `block` (on a boundary line,
the incoming solve block — normally `epoch + 1`; `block == epoch` marks an
equal-epoch `begin_block` no-op, which still drains once, so a live epoch can
appear on two lines, a partial then the remainder). Aggregation rule:
**counters sum per epoch; gauges (`distinct`, `negative_entries`)
last-value-wins.** The counters that decided (a) below are now always
available on any engine run that sets the two flags — no fixture, no example,
no special build.

### The numbers (recomputed from `logs/walkmemo_stats.jsonl`)

The scaled run is the JSONL slice from its 12th line on (the first 11 lines
are a shorter flags-on smoke drive — 00:32:00–00:33:37 UTC, ~8.1k probes,
zero hits — separated from the scaled run by a clean `final_drain` and a
12-minute restart gap). Slice: 883 lines, 00:46:01 → 03:45:54 UTC, inside the
10,800 s window.

| measure | value (scaled slice) |
|---|---|
| block span | 26151352 → 26152248 — an 896-block span (897 slots inclusive; 861 carried solver activity, 36 quiet slots emit nothing under the zero-activity gate) |
| solver epochs | 862 (21 double-flush through an equal-epoch no-op boundary) |
| memo probes | 593,523 |
| hits (previous-epoch census membership) | 2,634 — 0.444% of probes |
| cached negatives served (`negatives_played`) | 3,543 — 0.597% of probes |
| combined per-probe cross-block recurrence | 6,177 — 1.04% of probes |
| `cache_plays` | 593,523 — equals probes by construction (every probe consults the cache) |
| probes per epoch | median 675, min 48, max 2,185 (counters summed per epoch; last-flush-per-epoch instead: median 671.5, max 2,011 — only the 21 double-flush epochs differ, immaterially) |
| distinct compositions (per-epoch gauge, summed) | 591,965 = 99.74% of probes — **probes ≈ distinct, block after block** |
| epochs carrying any hits | 102 of 862 (11.8%) |
| hit concentration | top five hit-epochs hold 520 of 2,634 hits (19.7%); heaviest epoch 26151559 replays 145 of 569 probes = 25.5% of its compositions; the next four replay 6.5–14.1%; median share among hit epochs 2.3% (per-hour hit totals: 81 / 1,070 / 1,130 / 353) |
| `negative_entries` (running gauge) | 175–2,477 over the run, ends at 530 — census-scoped eviction visibly breathes |
| `probes_sims` / `hits_sims` | 0 — the same all-zero sim columns as the fixture corpus (the caveat above carries over: no signal) |

### Decision (a) — resolved with data: the retention window is immaterial

The live answer to the deciding signal decision (a) named — live `hits` vs
`cache_plays` behavior across blocks: cross-block composition recurrence on
live traffic is **about 1 percent per probe** (0.444% census hits + 0.597%
cached negatives served), and it is **bursty**: only 102 of 862 epochs carry
any hits at all; the top five hit-epochs concentrate 520 of the 2,634 hits
(~20%); the heaviest epochs replay a fifth to a quarter of their compositions
(epoch 26151559 replays 145 of 569 = 25.5%) while most epochs replay none
(median share among hit epochs: 2.3%).

At that recurrence the retention-window question (census retain vs LRU ring)
cannot bite in either direction: both windows retain far more — orders of
magnitude more — than the ~1% sliver that ever recurs, and the
`negative_entries` gauge (175–2,477 over the run) shows the census holding
thousands of entries against a per-epoch recurrence of a handful. There is no
measurable direction in which the "wrong" window loses anything. **Decision
(a) is resolved by data: census retain stays as implemented; no retention
experiment is worth running at this recurrence.**

The open question this data actually surfaces is upstream of retention: the
limiting factor is composition recurrence itself. The per-block dirty-path
set barely overlaps the previous block's — 591,965 distinct compositions
against 593,523 probes (99.74%) means the engine probes an almost-fresh
composition set block after block. The follow-up worth investigating is
**what shapes the per-block dirty-set overlap** — the admission draw (which
paths the registry feeds an epoch), event-driven dirtiness (which pools a
block's logs actually touch), and the path cap (1,000,000 registered paths
against a per-epoch probe budget of median 675) — NOT how long to retain.

### Decision (b) — resolved by bounds: keep the deliberate safety gate

The direct hotpath functions-timing run was attempted and cannot flush from
the corpus examples: the `HotpathGuard` lives in `BlockPump::run_with_stream`
(engine-lifetime, `degenbot-bot/src/profiling.rs`, env-gated
`DEGENBOT_HOTPATH=1`), behind the `hotpath` Cargo feature — and the umbrella
facade (`rust/crates/facade/degenbot`) forwards `otel` and `sql-ledger` but
**not** `hotpath`, which the settlement example depends on exclusively. The
solvers examples compile the feature (the `#[hotpath::measure]` spans
expand) but hold no guard, and without a live guard no report is generated.
Every hotpath report this repo has ever flushed came from that pump-lifetime
guard in a Python-driven run (the UYSAXS / KGXFT7 measurements in
`docs/hotpath-crossing-cache-verification.md`) — never from a corpus
example. That blockade is a fact, not a workaround candidate.

The bound that answers the question is already measured — by the probe-first
reorder itself (ergo FHU77O). The work the reorder removed from replay
epochs, the per-hop `source_fingerprint` re-derivation plus `PieceView`
assembly, costs **2.9 ms → 1.6 ms over the same 156 paths**, i.e. **8.3 µs
per path** (1.3 ms / 156). The arithmetic:

- 1.3 ms / 2.9 ms ≈ **45%** of a pure-replay epoch (Arm B epochs 2+; the
  first readout's own pre-reorder table records 2.8 ms — same magnitude).
- 1.3 ms / 26.8 ms ≈ **5%** of a fresh (miss-path) solve (Arm A's 156-path
  full-solve epoch from the readout table above).
- The capture above measured replays at **~1% of live probes** (1.04%
  combined recurrence), so ~99% of live probes take the miss path and the
  replay-side saving is a rounding error on live traffic.

Weighted: **the gate is roughly a 5 percent tax on the miss path** — the path
virtually every live probe takes. VERDICT: **keep the deliberate safety
gate.** A 5% tax does not justify removing the table-pairing correctness
check (pairing the tables to positions by index instead of re-folding);
nothing in the live capture indicts the check.

Two small enabling steps for a future DIRECT measurement, recorded as
follow-ups, not work:

1. a `#[hotpath::main]` attribute (or an explicit `HotpathGuard`
   construction — the pattern already exists in-repo:
   `degenbot-bot/examples/hotpath_prometheus_probe.rs` builds the pump's own
   `profiling::hotpath_guard` under `--features hotpath-prometheus`) on the
   corpus examples, so a telemetry-enabled example run can flush a report;
2. an umbrella feature-forward for `hotpath` (a parity-gap candidate
   alongside the existing `otel` forward), so the settlement example can
   compile the feature through the umbrella at all.

Resolution (landed, ergo LLTUSR): both steps collapsed into one — the umbrella
`hotpath` forward. The guard never needed mirroring: the pump constructs the
process's single guard itself under `DEGENBOT_HOTPATH=1`, and a second live guard
panics (hotpath 0.28.5), so an example-side construction would abort the run it
means to measure. The corpus examples were only missing the feature-forward to
compile the pump's instrumentation at all. Verified by a driven settlement run
(90-second window, clean cooperative expiry, exit 0): the pump-lifetime guard
flushed the JSON report (`HOTPATH_OUTPUT_FORMAT=json`; `HOTPATH_SHUTDOWN_MS`
forces a timed report for longer-running drivers).

### What the live capture did and did not do

- Landed: the tap (code, already committed in this ergo) and this doc
  section. No other code changes, no test changes, no stats files copied
  into the repo — `logs/walkmemo_stats.jsonl` is a runtime artifact; this
  doc records its path and the regeneration command above.
- Did not do: touch the retention implementation (decision (a) closes with
  census retain as-is), touch the safety gate (decision (b) closes with
  keep), run the whole-workspace gates (the PM owns them), or change any
  ergo state (the PM owns it).

## What this change did and did not do

- Added: `examples/walkmemo_readout.rs` (example-grade, relaxed-lint header,
  exits nonzero on fixture/parse failure) and this doc. No production `src/`
  changes, no test changes.
- Did not do: any ergo state change (the PM owns it), any live-recurrence
  claim, and the (b) share measurement (PENDING as of that change; resolved
  by bounds by the live capture below).
