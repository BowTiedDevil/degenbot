# Rust development tools — catalog and migration ledger

**Status: epic `BY3RRJ` ("Separate Rust consumer examples from development labs").**
The catalog landed as task `01` (ergo `2VLTDD`); the package scaffold landed as
task `02` (ergo `CUP3KB`) — three non-publishable tool packages, their workspace
membership, their support libraries, and the `just check-dev-tools` lanes, all
described in §6.2. Seven fixture slices have landed since, all in task `03`:

| Slice | Rows landed | Where |
| --- | --- | --- |
| `4W2ZUK` | the two offline capture generators — `generate_evm_oracle_captures`, `generate_aave_adversarial_captures` | §6.3 |
| `6DAEEE` | the two live Aave span recorders — `generate_aave_upgrade_windows`, `generate_aave_config_windows` | §6.4 |
| `MCV3JU` | the live updater cassette recorder — `record_updater_cassette` | §6.5 |
| `FT3KUP` | the Python oracle corpus recorder — `record_py_oracle_corpus` | §6.6 |
| `ZWV3MH` | the path-fixture recapture — `capture_path_fixture` | §6.7 |
| `03f` | the concentrated-liquidity capture generator — `concentrated_liquidity_capture_gen` | §6.8 |
| `03g` (`B4IKBC`) | the three-hop production-calldata helper — `encode_three_hop` (a recipe/staging row; no §6.x section of its own) | §4.2, §8.1, §8.2 |

Cargo metadata now reports **7 example targets**, `degenbot-fixture-tools` reports its lib
plus exactly **nine** `[[bin]]` targets, `degenbot-solver-lab` its lib plus exactly
**eight**, and `degenbot-engine-lab` its lib plus exactly **eighteen** — fourteen default-lane
binaries (`bal_carry_probe`, `bal_candidate_scaling_probe`, `bal_delta_rounds_probe`,
`bal_delta_snapshot_probe`, `bal_dispatch_scaling_probe`, `bal_head_lifecycle_probe`,
`bal_live_acquisition_probe`, `bal_production_relevance_probe`, `bal_real_read_oracle_probe`,
`bal_retention_probe`, `bal_wall_ceiling_probe`, `cold_miss_fanout_probe`,
`session_fanout_probe`, `worker_fanout_probe`) beside `aot_prepare_executor`,
`capture_seam_probe`, and `sim_mode_bench` under `required-features = ["evm2-jit"]` and
`hotpath_prometheus_probe` under `required-features = ["prometheus"]`.
No §4.2 row is still an example — no other recorder, no other program, and no other
capture generator moved — and **no fixture byte changed** in any of the seven slices.
Each offline participant was re-proved byte-identical by its own re-derivation (the two capture recipes plus, for the cassette
recorder, `record_updater_cassette --check <cassette.json>...` over every committed
cassette in `tests/fixtures/cassettes/`), and every red path was shown against a **temporary
mutated copy**, never against the committed corpus. The seventh slice moved no fixture at
all (`git status --short -- tests/fixtures` empty): its recapture arms read the chain and
need a live archive node, so the honest report there is a compile lane, not a
re-derivation. The eighth slice moved no fixture either: the capture generator is the
exact-wei golden producer, so its rows and `.block` sidecar are re-derived only against a
live archive node at the pinned block — a run this slice did **not** execute, and the
committed corpus plus sidecars are byte-identical. The ninth slice writes no fixture at
all: it is the offline JSON-to-production-calldata helper, whose only output is the hex
command stream on stdout, so its evidence is the build recipe, its failure-path pins, and
the Ape matrix that executes the bytes it emits.

Task `04`'s first slice has landed since: it moved three §4.3 rows into
`degenbot-solver-lab` — `concentrated_liquidity_solve_replay` and `mixed_solve_replay`
(both `required-features = ["capture-fixtures"]`) and `synth_corpus_gen` (a
default-features target whose write anchor is re-pointed at the pools fixture home) — and
built that package's support library from the `degenbot-pools` `examples/common/` modules
the two replays consumed. Its fixture evidence is split and stated on the row: the
concentrated-liquidity corpus re-derives byte-identically; the committed synthetic *mixed*
corpus does not, for a reason that predates the move (§5.3). Task `04`'s second slice has
landed since: it moved `concentrated_liquidity_cache_lab` into `degenbot-solver-lab` as a
`required-features = ["capture-fixtures", "cache-lab"]` binary, with its cache strategies,
transition schedule, `DRCLAB_*` controls, readout text, and divergence exit code unchanged,
and its golden/regression coverage left where it belongs — in `degenbot-pools`'s
integration tests. No fixture byte changed, and no cache algorithm or metric definition
did. Task `04`'s third slice has landed since: it moved `entry_ab_readout` into
`degenbot-solver-lab` as a `required-features = ["capture-fixtures"]` binary, rewired onto
that package's support modules instead of the `#[path]`-included `examples/common/` files,
with its `DR_AB_ITERS` / `DR_AB_MEMO` controls, capture argument, readout text, and
divergence exit code unchanged; the `just entry-ab` recipe moved with it. `degenbot-pools`'s
`examples/common/` modules were deleted in 06g with `walkmemo_readout`, their final
consumer (`solve_path_probe` never used them, pools tests read `tests/common`, and
solver-lab's own `concentrated_liquidity_rows`/`capture_rows` support modules remain the
owner of that logic). No fixture byte changed. Task `04`'s fourth slice
has landed since: it moved `rayon_scale_probe` into `degenbot-solver-lab` as a
`required-features = ["capture-fixtures"]` binary, with its thread sweep (default
`1,2,3,4,6,8`), its serial calibration passes, its `--paths` / `--serial-passes` /
`--bare-only` / `--skip-gate` / `--skip-control` / `--ns` arguments, its CSV columns, and
its golden-lite `PROFIT_EPS` exclusion unchanged. The two edges the instrument needs —
the `rayon` scheduler it sweeps and the `tracing` span its diagnostics arm enters — are
OPTIONAL dependencies of that package, activated by the existing `capture-fixtures` lane,
so the default tree gained no crate: `rayon` is the lane's addition, and the `tracing`
facade already rides along through `degenbot-pools`'s own non-optional edge. No timing
threshold was added, and the largest matrix stays an on-demand measurement. Task `04`'s
fifth slice has landed since: it moved `stateview_feasibility_probe` into
`degenbot-solver-lab` as a DEFAULT-lane binary — it declares NO `required-features`, because
it builds its own synthetic in-memory V2/V3 journals and tick maps and reads no capture, no
node, no environment key, and no database — with its hard-coded M1–M6 matrices, stdout
tables, `/proc/self/statm` RSS read, and its preserved `// Throwaway spike harness` lifecycle
header unchanged. Its 1001-rep/large-memory matrix is a non-routine measurement: it was
compiled, reviewed against the unchanged source, and **not executed** — that non-execution is
the verification truth for the instrument, not a passing performance result. Task `04`'s
sixth slice has landed since: it moved `bench_strategies` into `degenbot-solver-lab` as a
`required-features = ["envelope"]` binary, its point generation, `EnvelopeIndex` operations,
measurement order, stdout report, and failure behaviour unchanged. The
`alloy_primitives::U256` its points are typed in is a direct, OPTIONAL dependency of that
package under the same `envelope` lane, so the lane resolves the pools graph plus the order
index and adds nothing else — no capture/cache, hotpath, RPC, JIT, or SQL-ledger forward,
and no bot exporter — while the default tree gained no crate. Its hard-coded 1M and 10M legs
were compiled, reviewed against the unchanged source, and **not executed**: there is no
smaller mode to run and no timing threshold anywhere.

Task `05`'s first slice has landed since: it moved `capture_seam_probe` into
`degenbot-engine-lab` as a `required-features = ["evm2-jit"]` binary and built that
package's support library from the `examples/shared/bench_corpus.rs` module the capture
probe shares with `cold_miss_fanout_probe`, `worker_fanout_probe`, and `sim_mode_bench` —
one corpus-building implementation, imported instead of `#[path]`-included, with its read
anchors resolved through `degenbot_engine_lab::paths` (marker-resolved root) instead of
`CARGO_MANIFEST_DIR/../../../..` arithmetic. The corpus names no JIT type, so the support
and the crates it links are DEFAULT-feature code and the later non-JIT consumers inherit it
without a JIT lane; `evm2-jit` stays the capture binary's own forward. The three other
consumers keep the old-home helper byte-intact until their own slices land, and no BAL
helper, BAL test, or simulation test changed. The binary ran offline from its new home and
printed its documented `PASS` line (both seam configurations' slot/swap sets equal the
inspector's, `540` compiled-code lookup hits, and the reverted-subcall leg's
`seam logs=0, inspector committed=0, inspector reverted-diagnostic=1`). The corpus
support's failure contract is a **two-class error** (`bench_corpus::CorpusError`): the
fixture class (an unresolvable repository root, an unreadable/undecodable/parse-failing
committed artifact, a quote outside `u128`) aborts the probe at the stage's own message —
exit 101 — while the runtime class (a reverted deploy or setup frame, an unencodable
workload) stays an ordinary reported error and exits 1. Both classes are values in the
library, and each instrument decides which of them aborts its own run.

Task `05`'s second slice has landed since: it moved `cold_miss_fanout_probe` into
`degenbot-engine-lab` as a DEFAULT-lane binary — it declares no `required-features`,
because it compiles no JIT program, so `capture_seam_probe` stays the package's only
JIT-gated target — and replaced its `#[path]`-included corpus module with
`degenbot_engine_lab::bench_corpus`; the original `examples/shared/bench_corpus.rs`
module stays byte-intact for `worker_fanout_probe` and `sim_mode_bench` until their own
slices. All eleven legs, the latency-injecting RPC stand-in, the coroutine/fan-out
scheduling, the `COLD_MISS_LATENCY_US` / `COLD_MISS_FRAMES` controls and their defaults
(200 us, 8 frames), the readout, and every verdict assertion and negative control are
unchanged; the probe's own bottom stub loader re-anchors its root through
`degenbot_engine_lab::paths` while keeping its two generic `expect` messages and its
panic, and the corpus support's two failure classes keep the instrument's old split —
a fixture-class error panics at the stage's own message (exit 101), a runtime-class
error prints `corpus build failed: <message>` and returns (exit 0). The package's default
tree gained the plain edges that probe reads through (`degenbot-rpc`, the `tower`
transport seam, `serde_json`, `parking_lot`, `tokio`), with no `evm2-jit`, SQL-ledger,
bot-exporter, or solver-lab forward. Its bounded routine run
(`COLD_MISS_FRAMES=2 COLD_MISS_LATENCY_US=1`) passed every leg and assertion; the
default N=8 sweep is a non-routine measurement and was **not executed**, and no timing
value became a threshold. No fixture byte changed.

Task `05`'s third slice has landed since: it moved `worker_fanout_probe` into
`degenbot-engine-lab` as a DEFAULT-lane binary — no `required-features` either, so
`capture_seam_probe` stays the package's only JIT-gated target — and replaced its
`#[path]`-included corpus module with `degenbot_engine_lab::bench_corpus`. The original
`examples/shared/bench_corpus.rs` module now serves only `sim_mode_bench`, and stays
byte-intact for it until that slice. All four legs (serial shared `&mut evm`, per-worker
threads, per-candidate fibers on the batched-round scheduler, the warm re-drive), the three
corpus shapes (`production`, `lockstep`, `disjoint`), the recorded-trace replay with its
chunking, debug dump, exec-sleep and planted-divergence controls, the ten per-run env knobs
and their defaults (700 us storage, 1200 us account, 21 candidates, 2 workers,
`production`), the stdout readout, and the cross-leg verdict-identity gate are unchanged.
`just worker-fanout-probe` keeps its recipe name and `--release` posture and now runs
`-p degenbot-engine-lab --bin worker_fanout_probe`. The corpus support's two failure
classes preserve this instrument's exact split: the local `build_corpus` / `deploy_world` /
`build_candidate` builders still return `Result<_, String>` and `main` still prints
`corpus build failed: <message>` then exits 2, so the shared `deploy`'s runtime-class
`Failure` message maps onto that `String` path while its fixture-class `Panic` panics at the
stage's own message (exit 101); the local artifact read/hex/parse and the V2 quote overflow
keep their own panics and messages, replay errors still print to stderr and exit 1, and the
probe's own `repo_root` helper now resolves through `degenbot_engine_lab::paths`
(marker-resolved root) with every artifact path, the `/tmp` trace semantics, the output and
the defaults unchanged. The package's default tree gained no crate — the probe reads only
edges the corpus support already named, plus `serde_json` — and no `evm2-jit`, SQL-ledger,
bot-exporter, or solver-lab forward arrived with it. Its bounded routine run
(`WORKER_FANOUT_CANDIDATES=2 WORKER_FANOUT_WORKERS=2 COLD_MISS_LATENCY_US=1
WORKER_FANOUT_ACCOUNT_US=1`, production shape) passed every leg and left the identity gate
green, the default 21-candidate sweep is a non-routine measurement and was **not executed**,
and no timing value became a threshold. No fixture byte changed.

Task `05`'s fourth slice has landed since: it moved `sim_mode_bench` into
`degenbot-engine-lab` as its second `required-features = ["evm2-jit"]` binary (the same lane
the capture probe opened) and replaced its `#[path]`-included corpus module with
`degenbot_engine_lab::bench_corpus`. That move left the old home's
`examples/shared/bench_corpus.rs` with no consumer, so the module is deleted — the other
`examples/shared/` modules stay exactly as they are. All six modes (`interp`, `jit-block`,
`jit-pool`, `aot`, `replay`, `replay-jit`), the `SIM_BENCH_ITERS` control and its default
(100), the `all|interp|jit-block|jit-pool|aot|replay|replay-jit` filter with its exit-2
unknown-mode refusal, the stdout report, the temp-path AOT scratch, and the cross-mode
verdict-identity checks are unchanged. The corpus support's two failure classes preserve
this instrument's exact split: `run_mode` still returns `Result<_, String>` and `main` still
prints `<mode> FAILED: <message>` then exits 1, so the shared `build_corpus`'s runtime-class
`Failure` message arrives through a narrow local `load_corpus` adapter as that `String`,
while its fixture-class `Panic` panics at the stage's own message (exit 101). `just
bench-sim-modes` keeps its name and `--release` posture and now runs `-p
degenbot-engine-lab --features evm2-jit --bin sim_mode_bench`. The bounded all-mode run is
the routine evidence: `SIM_BENCH_ITERS=10` exercises every mode and both verdict-identity
families, because the `jit-pool` row only dispatches a compile once a program's observed
lookup count reaches the production pool's hot threshold (`jit_hot_threshold` = the sim
fan-out width, 8) — an iteration budget below that ends with the timed loop finished and
the upgrade still pending, so `jit-pool` waits out its deadline and exits 1 (unchanged pool
behaviour, which is why the routine bound is above the threshold and not 2). The default
100-iteration performance matrix was **not executed**, and no timing value became a
threshold. No fixture byte changed, and the package's manifest gained no dependency edge
(the JIT lane already named `evm2-jit`) and no Prometheus, SQL-ledger, or solver-lab
forward.

Task `05`'s `05j` slice has landed since: it moved `aot_prepare_executor` into
`degenbot-engine-lab` as its third `required-features = ["evm2-jit"]` binary (the same lane
`capture_seam_probe` and `sim_mode_bench` opened) and deleted the old
`crates/engine/degenbot-simulation/examples/aot_prepare_executor.rs` target with its
manifest row. The production executor bytecode source, the exact repo-relative
`EXECUTOR_RUNTIME_ARTIFACT`, the durable artifact-store location and format, the
`ProgramKind::Aot` load-back proof, the `argv[1]` artifact-path override and its
canonicalize fallback, the stdout report, and every failure message and exit path are
unchanged; the derived repository root — the one `join("../../../..")` the example carried —
now resolves through `degenbot_engine_lab::paths::repo_root()` (marker-resolved, no `..`
arithmetic) with the same `EXECUTOR_RUNTIME_ARTIFACT` joined to it. `just
aot-prepare-executor` keeps its name, its `*ARGS`, and its `--release` posture and now runs
`-p degenbot-engine-lab --features evm2-jit --bin aot_prepare_executor`. The move added no
dependency edge (the JIT lane already named `evm2-jit`, and the package already reads
`degenbot-simulation` through its default tree) and no direct env read: the state root
stays owned by the `degenbot-config` loader that `default_artifact_dir()` consults. The
isolated proof ran `DEGENBOT_STATE_DIR` at a fresh temporary directory outside the
repository — never a shared durable store — and the preparation, the store listing, and
the `proof: get_compiled -> Aot` line passed there. No fixture byte changed, and no
Prometheus, SQL-ledger, or solver-lab forward arrived with it.

The workspace accumulated 44 Cargo example targets whose purposes are three different
things: a *supported consumer starting point*, a *durable assertion someone must gate*, and
a *development instrument* that measures or generates something on demand. This file is the
durable catalog of those 44 targets and the ledger of where each one goes. Later tasks
(`03` fixtures, `04` solver/order-index, `05` engine, `06` assertion promotion, `07`
surface prune + final catalog) execute against these rows; nothing here is decided again by
whack-a-mole in a manifest.

Provenance, not a hand-maintained count:

```bash
cargo metadata --manifest-path rust/Cargo.toml --format-version 1 --no-deps --locked
```

Audited at commit `9c609a24b` (2026-11-14): **44 example targets across 10 packages** —
`degenbot` 8, `degenbot-simulation` 16, `degenbot-pools` 10, `degenbot-arbitrage` 2,
`degenbot-bot` 2, `degenbot-order-index` 2, `degenbot-execution-sample` 1,
`degenbot-executor` 1, `degenbot-ingestion` 1, `degenbot-rpc` 1. Feature facts in the
`Features` column have two provenances, and every row says which. For 16 of the 44 targets it
is exactly the `[[example]] required-features` Cargo metadata reports (the four `sql-ledger`
recorders, the seven `degenbot-pools` capture-fixtures/cache-lab replays, the two `envelope`
binaries, and the three `evm2-jit` binaries); the other 28 targets declare none, so a `—`
means the target needs nothing beyond its package's default feature set. The one row that is
*not* manifest-declared is marked *(runtime)* in its cell: `hotpath_prometheus_probe` takes
its `hotpath-prometheus` need from the source and the runtime wiring today, and task 05 must
make it a real declaration on the replacement binary (§4.7).

**Working directory — read this before running any command below.** Every `just` recipe, the
`cargo metadata` command above, and the §4.6 audit run from the **repository root**. A direct
`cargo ... -p <package> ...` command is run from **`rust/`** unless it carries
`--manifest-path rust/Cargo.toml`, in which case it also runs from the repository root.
Argument paths in the examples below are relative to wherever the command runs, which is why
§8.1 keeps an explicit `--manifest-path rust/Cargo.toml` on the recipe-equivalent commands
and §10.1 requires tool packages to take an explicit fixture path argument instead of
inferring one.

## 1. The taxonomy — what each home means

| Home | What belongs there | What must not |
| --- | --- | --- |
| `rust/examples/` and crate `examples/` directories | A small, supported, consumer-facing starting point: something a reader runs to see the public surface work. | A measurement harness, a fixture recorder, or a program whose lint waiver exists because it is diagnostics. |
| Owning-crate integration tests (`rust/crates/**/tests/`, in-crate `#[cfg(test)]`) | Durable assertions that must gate a change: parity, golden, smoke, capture replay. | Reported timings, printed tables, "the numbers are the report". |
| `rust/tools/fixture-tools/` (`degenbot-fixture-tools`) | Record / check / regenerate workflows for committed fixtures, and test-input generators. | Anything a product build needs; anything that must run in an ordinary `cargo test` lane. |
| `rust/tools/solver-lab/` (`degenbot-solver-lab`) | Solver and order-index replay, measurement, and stress instruments. | Gates CI blocks merge on (those belong in tests); a reason to pull RPC, JIT, or SQL-ledger into a small replay. |
| `rust/tools/engine-lab/` (`degenbot-engine-lab`) | Bot, simulation, arbitrage, JIT, fan-out, and BAL instruments. | A production operator CLI; a new product code path. |
| Durable docs (`docs/**`, ADRs) plus Git history | Concluded investigations with no continuing executable role. | — |

**Rule — concluded reports never become a package.** An instrument that has stopped
producing executable value ends by moving its *conclusions* into a durable doc under
`docs/` (or an ADR), with the code left to Git history. No `attic/`, `archive/`,
`concluded/`, or `old-examples/` crate is created: an uncompiling archive package is not a
record of a decision, it is a workspace member nobody can build, gate, or trust. The only
survivors are tools whose numbers somebody still re-measures.

## 2. Final-disposition summary — a partition of the 44 original targets

| Final disposition | Where the target ends up | Rows | Owning task |
| --- | --- | --- | --- |
| Public retainer (stays an `examples/` target) | the crate `examples/` directories of §3, including the 2 complete packages under `rust/examples/` | 3 | none — fixed |
| `degenbot-fixture-tools` binary | `rust/tools/fixture-tools/src/bin/` | 9 | `03` (4W2ZUK) |
| `degenbot-solver-lab` binary | `rust/tools/solver-lab/src/bin/` | 8 | `04` (X65SDH) |
| `degenbot-engine-lab` binary | `rust/tools/engine-lab/src/bin/` | 18 | `05` (SXHCWQ) |
| Owning-crate integration test (the example target itself is deleted) | `rust/crates/**/tests/` | 2 | `06` (assertion promotion) |
| **Removed executable** (no destination) | — the file is deleted once its durable evidence is preserved (§5) | 4 | `06`; tasks `04`/`05` leave the files in place |
| **Total** | | **44** | |

The six rows above are a partition of the 44 *original example targets*: every target
appears in exactly one row, and `3 + 9 + 8 + 18 + 2 + 4 = 44`. The partition is deliberately
not the same thing as *source-task ownership*, which is what the §4 sections are organised by.
Task `04` owns the eleven §4.3 rows, but only eight of them become solver binaries; task `05`
owns the nineteen §4.4 rows, but only eighteen become engine binaries. The four targets whose
final disposition is a removed executable are `scale_demo` (task `04` owns it, task `06`
removes it), `walkmemo_readout` (same), `v2_concentrated_liquidity_parity_probe` (task `04`
owns it, task `06` extracts its assertions and removes it), and `mirror_coverage_probe` (task
`05` owns it, task `06` extracts its assertion and removes it). §5 gives the evidence for
each, and the rule the four share is: **no task moves a file that a later chunk deletes.** Moving one first buys a
manifest stanza, a path re-anchoring, and a verification run for a file that is about to
disappear, and task `06` cannot tell a deliberate move from an accident.

Extracted assertions never change a target's assigned final disposition; they only add task
`06` test rows. `v2_concentrated_liquidity_parity_probe` therefore stays a **removed
executable**: task `04` leaves it in its current example home (a move would buy a manifest
stanza and a re-anchoring for a file task `06` deletes) and task `06` extracts the decision
assertions before removing it (§5.5). `mirror_coverage_probe` is
*removed*: task `06` writes its replacement offline test and durable doc first and then
deletes the executable (§5.4) — its assertion outlives it, its binary does not, so its row
stays `remove in 06`. The partition counts targets, never the tests they produce.

## 3. Public retainers (fixed)

These three targets are the final public example surface (task 07 asserts exactly this set
in Cargo metadata). They are not moved, renamed, or converted to tests.

| Target | Package | Destination | Current path (under `rust/`) | Purpose | Features | Needs | I/O and write behavior | Verification route |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `standalone_consumer` | `degenbot` | retain | `crates/facade/degenbot/examples/standalone_consumer.rs` | ADR-005 Tier-0 standalone proof: `BotState` + `UNISWAP_V2` preset + swap calc with no Python/pyo3 in the graph; DB snapshot → auto-backfill → resume; one-file operator config with `cli > env > file > default` layering; in-process evm2 sim | — | committed `parity.db` fixture, read-only (relocatable via `DEGENBOT_FIXTURE_DB`); optional live WS endpoint via `SMOKE_RPC_URL`; unset = CI-runnable | writes nothing; `panic!` and non-zero exit on any failed check | `just test-standalone` — first command of the recipe, which *is* the standalone gate |
| `solve_path_probe` | `degenbot-pools` | retain | `crates/foundation/degenbot-pools/examples/solve_path_probe.rs` | ADR-084 D1/C5 consumer (ii): solve a path through the root solve door using door items only; doubles as the catalog's documented pools-consumer snippet | — (the recipe enables `capture-fixtures`; the target declares no `required-features` and builds its hops in code) | none — synthetic hops in source | stdout only; `panic!` when the door returns no solution | `just check-consumer-probes` — first command of the recipe |
| `foreign_consumer` | `degenbot-execution-sample` | retain | `examples/degenbot-execution-sample/examples/foreign_consumer.rs` | User-defined `ExecutionAdapter` for a FOREIGN contract: projects a solved path into the sealed `SolveResult` view and encodes one transaction through the four-part seam (Encode + Probe reads + Assess + Fee), then repeats it through `ValidatedAdapter` (ADR-100 tier 1) — no `degenbot-arbitrage`, no pyo3 | — | none — `sol!` call types in source | stdout only | `cargo run -p degenbot-execution-sample --example foreign_consumer` (the command `docs/execution-adapters.md` documents); no recipe and no test names it today |

The two complete packages under `rust/examples/` stay whole: `degenbot-execution-sample`
(its lib plus the `foreign_consumer` example) and `degenbot-settlement-bot-example`
(`examples/settlement_bot/`, a binary package with **zero** example targets — it appears
nowhere in this ledger's 44 rows because Cargo reports no example target for it, and it
must keep reporting none). Neither becomes a tool home; neither is emptied of its tests.

## 4. The catalog — one row per current example target

One row per current example target, one destination each; every row names its target
and its current package in backticks. The tables in §4.1–§4.5 sit between two marker
comments, and §4.6 carries the inline audit that proves the marked region is the complete
44-target set with no duplicates (run it from the repo root — it is the whole check, and
no script file is added for it).

**A landed row names where its target lives NOW.** For a moved row the `Package` cell is the
destination package and the `Destination` cell says `landed`, because that is the state a
reader (and the §4.6.1 union audit) has to be able to check. Every not-yet-moved row keeps
its current package and its planned destination, exactly as before. The move also means the
*original* population is still 44 rows while the *example* population is 11 — the
thirty-three diagnostics that differ are exactly the thirty-three landed rows (§4.6).

<!-- ledger:begin -->

### 4.1 Retained public examples (3 rows)

See §3 for the full rows; repeated here only so the marked region holds all 44 rows.

| Target | Package | Destination | Current path (under `rust/`) | Purpose | Features | Needs | I/O and write behavior | Verification route |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `standalone_consumer` | `degenbot` | retain | `crates/facade/degenbot/examples/standalone_consumer.rs` | Tier-0 `cargo add degenbot` consumer proof (see §3) | — | committed `parity.db`, optional `SMOKE_RPC_URL` | none | `just test-standalone` |
| `solve_path_probe` | `degenbot-pools` | retain | `crates/foundation/degenbot-pools/examples/solve_path_probe.rs` | consumer (ii) solve-door probe (see §3) | — | none | stdout | `just check-consumer-probes` |
| `foreign_consumer` | `degenbot-execution-sample` | retain | `examples/degenbot-execution-sample/examples/foreign_consumer.rs` | foreign `ExecutionAdapter` seam sample (see §3) | — | none | stdout | `cargo run -p degenbot-execution-sample --example foreign_consumer` |

### 4.2 Fixture recorders and test-input generators — `degenbot-fixture-tools` (task 03: 9 rows landed, none still an example)

| Target | Package | Destination | Current path (under `rust/`) | Purpose | Features | Needs | I/O and write behavior | Verification route |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `capture_path_fixture` | `degenbot-fixture-tools` | fixture-tools (03, **landed**) | `tools/fixture-tools/src/bin/capture_path_fixture/main.rs` | Recapture a path-investigation fixture's recorded state from the chain at its `target_block` (V2 scalars, V3 ticks word-wise via `TickLens` or bitmap, V4 through the pool StateView, all batched over Multicall3); authored identity and narrative stay verbatim — unchanged by the move from `degenbot`'s examples | — (**no `required-features`**: it reads no statement ledger and opens no operator DB, so it is a DEFAULT-features target that `just check-dev-tools default` builds) | LIVE archive node resolved per ADR-062 (`--node`, `DEGENBOT_RPC_*`, `nodes.*`), plus the committed path fixtures, each named as an explicit positional argv path (no fixture home is derived, so no `--fixtures-home` exists) | Default mode REWRITES each named fixture JSON with canonical serde formatting (`fs::write`); `--check` writes nothing and exits 1 on drifted state keys | `cargo run -p degenbot-fixture-tools --bin capture_path_fixture -- [--check] <fixture.json>...` (no recipe: both arms read the chain, so in CI this row is a **compile lane only**). The fixture schema and the recapture library stay owned by `crates/facade/degenbot/src/investigation/{fixture,capture,chain_capture}.rs`, documented in `docs/architecture/investigation-harness.md`; offline consumers of that schema: `crates/facade/degenbot/tests/tier3_*` |
| `generate_aave_adversarial_captures` | `degenbot-fixture-tools` | fixture-tools (03, **landed**) | `tools/fixture-tools/src/bin/generate_aave_adversarial_captures/main.rs` | Generate the wave-3 aave-side adversarial goldens node-free (three scenarios over the ScratchEvm/frame-replay seam, real `run_aave_update` chunk loop); determinism asserted by generating twice byte-identically — unchanged by the move from `degenbot`'s examples | `sql-ledger` (the `[[bin]]` `required-features` on the tool package) | offline; committed `tests/fixtures/cassettes/wave3` + `tests/fixtures/sql_goldens/wave3`, resolved once from the repository root (`--fixtures-home <dir>` re-points both homes at a copy) | (Re)writes both fixture homes (`create_dir_all` + `fs::write`) in default mode; `--check` compares the regenerated bytes and exits 1 on any drift, writing nothing — red-path evidence runs against a temporary mutated copy | `just check-aave-captures` → `cargo run -p degenbot-fixture-tools --features sql-ledger --bin generate_aave_adversarial_captures -- --check tests/fixtures/cassettes/wave3 tests/fixtures/sql_goldens/wave3`; offline consumer = `crates/integrations/degenbot-aave/tests/wave3_capture_replay.rs` |
| `generate_aave_config_windows` | `degenbot-fixture-tools` | fixture-tools (03, **landed**) | `tools/fixture-tools/src/bin/generate_aave_config_windows/main.rs` | Record the wave-5 Aave config-dispatch span cassettes from the live chain (six formerly-fenced events), each window driving the real multi-chunk loop over `RecordingTransport`, then replaying through the statement ledger | `sql-ledger` | LIVE node (`--node`, `DEGENBOT_RPC_HTTP_CHAINID_1`); committed wave-5 cassette + golden homes | (Re)writes cassettes, per-chunk statement ledger, and the canonical DB dump; every window recorded twice for byte-identity; `--check <cassette_home> <golden_home>` | `cargo run -p degenbot-fixture-tools --features sql-ledger --bin generate_aave_config_windows -- [--check] [--node <uri>]` (no recipe: the recorder needs a node). Offline owner: `crates/integrations/degenbot-aave/tests/wave5_config_span_replay.rs` (plus `tests/fixtures/cassettes/wave5/CONFIG-SPAN-MAP.md` provenance). The drift gate re-records from a live node, so in CI this row is a **compile lane only** — never claim a live revalidation from it |
| `generate_aave_upgrade_windows` | `degenbot-fixture-tools` | fixture-tools (03, **landed**) | `tools/fixture-tools/src/bin/generate_aave_upgrade_windows/main.rs` | Record the wave-4 Aave upgrade-span cassettes from the live chain; substrate from the chain's current reserve set (`resolve_aave_substrate`), same record-twice determinism contract | `sql-ledger` | LIVE node (`--node`, `DEGENBOT_RPC_HTTP_CHAINID_1`); committed wave-4 homes | Same write/`--check` shape as the wave-5 recorder | `cargo run -p degenbot-fixture-tools --features sql-ledger --bin generate_aave_upgrade_windows -- [--check] [--node <uri>]` (no recipe: the recorder needs a node). Offline owner: `crates/integrations/degenbot-aave/tests/wave4_upgrade_span_replay.rs`; **compile lane only** in CI |
| `generate_evm_oracle_captures` | `degenbot-fixture-tools` | fixture-tools (03, **landed**) | `tools/fixture-tools/src/bin/generate_evm_oracle_captures/main.rs` | Generate the wave-2 EVM-oracle goldens node-free: the real canonical `UniswapV3Pool` harness artifact against the scratch chain, real `run_pool_update` chunk loop over a recording transport, SQL goldens replayed through the statement ledger; determinism is the acceptance gate — unchanged by the move from `degenbot`'s examples | `sql-ledger` (the `[[bin]]` `required-features` on the tool package) | offline; committed `tests/fixtures/cassettes/wave2` + `tests/fixtures/sql_goldens/wave2`, resolved once from the repository root (`--fixtures-home <dir>` re-points both homes at a copy); the `tier3-oracle/artifacts` harness input is read from the repository root and is never re-pointed | (Re)writes both homes; `--check` compares the regenerated bytes and exits 1 on drift, writing nothing — red-path evidence runs against a temporary mutated copy | `just check-evm-captures` → `cargo run -p degenbot-fixture-tools --features sql-ledger --bin generate_evm_oracle_captures -- --check tests/fixtures/cassettes/wave2 tests/fixtures/sql_goldens/wave2`; offline consumer = `crates/foundation/degenbot-pool-updater/tests/wave2_capture_replay.rs` |
| `record_py_oracle_corpus` | `degenbot-fixture-tools` | fixture-tools (03, **landed**) | `tools/fixture-tools/src/bin/record_py_oracle_corpus/main.rs` | Record the offline oracle-answer corpus for the fork-bound on-chain parity tests into the per-block `OfflineProvider` JSON home; every value comes off the wire, a revert at the pin is stored as `null` — unchanged by the move from `degenbot`'s examples | — (**no `required-features`**: it reads no statement ledger and opens no operator DB, so it is a DEFAULT-features target that `just check-dev-tools default` builds) | LIVE archive node: `TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI` / `TEST_BASE_ARCHIVE_NODE_HTTP_URI` (repeatable `--ethereum-node` / `--base-node` / `--arbitrum-node` form an ordered endpoint pool with bounded backoff rotation); committed golden-key sources under `tests/fixtures/chain_data`, resolved once from the repository root (`--fixtures-home <dir>` re-points the corpus home and the cassettes the golden keys read) | Writes `tests/fixtures/chain_data/<chain_id>/py_oracle_<scenario>_block<N>.json`; every scenario recorded twice; `--check` requires byte-identity with the corpus on disk and writes nothing | `cargo run -p degenbot-fixture-tools --bin record_py_oracle_corpus -- --check --scenario <name> --ethereum-node <archive-url>` (both arms need a node serving the pins: **compile lane only** in CI). Replay consumers: `tests/golden/test_oracle_replay_offline.py`; live drift guard `just live-drift` (`tests/golden/test_live_drift.py`); provenance `tests/fixtures/chain_data/README.md`; node-free path-wiring evidence: the binary's own `#[cfg(test)] mod tests` (`cargo test -p degenbot-fixture-tools --bin record_py_oracle_corpus`) |
| `record_updater_cassette` | `degenbot-fixture-tools` | fixture-tools (03, **landed**) | `tools/fixture-tools/src/bin/record_updater_cassette/main.rs` | Record one updater chunk span cassette from the configured node (pool `PoolCreated` + V3 liquidity scan, or an Aave market chunk), including the `--verify` gate surface and the full `--kind aave-run` loop — unchanged by the move from `degenbot`'s examples | — (**no `required-features`**: it reads no statement ledger, so it is a DEFAULT-features target that `just check-dev-tools default` builds) | LIVE node resolved per ADR-062 for the record arm; the `--check` arm is fully offline; committed cassettes under `tests/fixtures/cassettes/` | Writes cassette JSON via the recorder's canonical writer (record arm: canonical flush, then `fs::write` to `--out`); `--check <cassette.json>...` re-serializes each committed cassette through that same writer, re-checks every entry digest, writes nothing, and exits 1 on drift; seeds throwaway harness DBs in temp dirs | Offline arm: `cargo run -p degenbot-fixture-tools --bin record_updater_cassette -- --check tests/fixtures/cassettes/*.json` (no feature lane; green = every committed cassette byte-identical). Replay owners: `crates/foundation/degenbot-pool-updater/tests/cassette_replay_run.rs`, `.../tests/verification_gate_probes.rs`, `crates/integrations/degenbot-aave/tests/aave_cassette_replay.rs`, `.../aave_config_cassette_replay.rs`; writer contract `crates/foundation/degenbot-rpc/src/cassette.rs`; the RECORD arm is **compile lane only** in CI |
| `concentrated_liquidity_capture_gen` | `degenbot-fixture-tools` | fixture-tools (03, **landed**) | `tools/fixture-tools/src/bin/concentrated_liquidity_capture_gen/main.rs` | The ONLY sanctioned producer of exact-wei goldens for the CL replay corpus: fetch curated real V3 pools at a pinned block, backfill ticks through `v3_simulate_swap`, build the production sequences, form the 2-hop all-concentrated-liquidity paths, solve, and write JSONL — unchanged by the move from `degenbot-rpc`'s examples | — (**no `required-features`**: it reads no statement ledger and opens no operator DB, so it is a DEFAULT-features target that `just check-dev-tools default` builds) | LIVE archive node `DEGENBOT_CLCAP_RPC` (+ `DEGENBOT_CLCAP_BLOCK`, `DEGENBOT_CLCAP_PATH_CAP`, `DEGENBOT_CLCAP_MAX_FETCHES`); the default output is the committed corpus fixture under `crates/foundation/degenbot-pools/tests/fixtures/`, resolved once from the repository root, and an explicit positional argv path overrides it | Writes the capture JSONL **and** a `.block` sidecar recording the pinned block + regen command (`File::create` + `fs::write`); never run by tests | `cargo run -p degenbot-fixture-tools --bin concentrated_liquidity_capture_gen -- [<out.jsonl>]` (no recipe: it needs a live node, so this row is a **compile lane only**). Offline consumer of its product: `just test-golden-replay` (both committed corpora). Regeneration is a manual, live-only operation — record it as **not executed** in any offline gate report. The default output path is pinned by the binary's own `#[cfg(test)] mod tests` |
| `encode_three_hop` | `degenbot-fixture-tools` | fixture-tools (03, **landed**) | `tools/fixture-tools/src/bin/encode_three_hop/main.rs` | Encode one three-hop `PathInfo` through the production command grammar so the Ape execution matrix exercises production bytes instead of a second test encoder — unchanged by the move from `degenbot-executor`'s examples | — (**no `required-features`**: it reads no statement ledger and opens no database of any kind, so it is a DEFAULT-features target that `just check-dev-tools default` builds) | none — one JSON request document as `argv[1]`, no node, no fixture home, no environment key | stdout only: one hex line, the command stream `encode_cmd_3_hop` produced; writes nothing; a missing/invalid request exits non-zero with the reason on stderr | `just build-executor-three-hop-encoder` (builds `-p degenbot-fixture-tools --bin encode_three_hop` and stages it at `<target-dir>/debug/examples/encode_three_hop`, the path the matrix reads) then `just test-executor-offline` (`tests/executor/test_cmd_executor_three_hop_production_calldata.py` consumes it, including the invalid-request failure paths) |

### 4.3 Solver and order-index laboratories — `degenbot-solver-lab` (task 04 owns 11 rows: 8 move, 3 stay in place for task 06)

| Target | Package | Destination | Current path (under `rust/`) | Purpose | Features | Needs | I/O and write behavior | Verification route |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `concentrated_liquidity_cache_lab` | `degenbot-solver-lab` | solver-lab (04, **landed**) | `tools/solver-lab/src/bin/concentrated_liquidity_cache_lab/main.rs` | Replay a capture JSONL as deterministic pool-state transitions and solve every state through every registered cache strategy, checking byte-equality against the full-rebuild reference and printing the rebuild-cost matrix | `capture-fixtures`, `cache-lab` (`telemetry` optional: the ambient walk telemetry the drain readout reads, an additional build shape of the same binary, never a bot-exporter lane) | committed corpus (default) or a capture path argument; `DRCLAB_MAX_PATHS`, `DRCLAB_TRANS`, `DRCLAB_MICRO`, `DRCLAB_DIGEST` | stdout report; writes nothing; exits 1 on any strategy divergence | `cargo run -p degenbot-solver-lab --features capture-fixtures,cache-lab --bin concentrated_liquidity_cache_lab -- <capture.jsonl>` (unchanged by the move: same argument, same four `DRCLAB_*` controls, same readout text and counters, same exit code); golden/regression coverage stays in the pools `concentrated_liquidity_cache_lab_goldens` and `concentrated_liquidity_cache_patch_regressions` suites; report `docs/cache-lab-report.md` |
| `concentrated_liquidity_solve_replay` | `degenbot-solver-lab` | solver-lab (04, **landed**) | `tools/solver-lab/src/bin/concentrated_liquidity_solve_replay/main.rs` | Offline heavy-CL replay with N-repeat stable timing; classifies every path through the shared `degenbot_pools::solve::golden_gate` evaluator (under-shoot past `PROFIT_EPS`, over-shoot past the few-wei tolerance, or nondeterminism across reps) for the mismatch readout and the over-shoot-only regeneration — measurement/readout only, never a gate: the owning pools integration tests gate the contract | `capture-fixtures` | committed corpora `concentrated_liquidity_capture_offline.jsonl` and `heavy_concentrated_liquidity_solve_captures.jsonl[.zst]`; `DR_REPLAY_ITERS` | stdout; `DR_REPLAY_REGEN=1` rewrites stale over-shoot-only goldens — a write mode, never run by a gate | `just test-golden-replay` (both corpora: the owning pools tests gate, this binary follows as the non-gating measurement/readout pass) |
| `entry_ab_readout` | `degenbot-solver-lab` | solver-lab (04, **landed**) | `tools/solver-lab/src/bin/entry_ab_readout/main.rs` | Post-unification readout: the retained `all-concentrated-liquidity` intake (prepared hops cloned) against `solve_piecewise` directly on real captured paths, byte-equality gated; the row census classifies the traffic mix | `capture-fixtures` (`hotpath` opt-in for the functions-timing run) | capture path argument (recipe defaults to `heavy_concentrated_liquidity_solve_captures.jsonl`); `DR_AB_ITERS`, `DR_AB_MEMO` | stdout; writes nothing except the hotpath JSON when `HOTPATH_OUTPUT_PATH` is set; exits 1 on any arm divergence | `just entry-ab [corpus]` → `cargo run -p degenbot-solver-lab --features capture-fixtures --bin entry_ab_readout -- <capture.jsonl>` (unchanged by the move: same argument, same two `DR_AB_*` controls, same readout text and counters, same parity exit code; its row parsing now imports `degenbot_solver_lab`'s support modules instead of the `#[path]`-included pools example helpers). Its forwarding-parity contract is **already** pinned in-crate by `all_concentrated_liquidity_intake_forwards_to_the_unified_entry_byte_exact`, `key_front_doors_agree_for_concentrated_liquidity_only_compositions`, and `both_solve_entries_refuse_paths_shorter_than_two_hops` (`crates/foundation/degenbot-pools/src/solve/concentrated_liquidity/tests/memo.rs`) — see §5.7 |
| `mixed_solve_replay` | `degenbot-solver-lab` | solver-lab (04, **landed**) | `tools/solver-lab/src/bin/mixed_solve_replay/main.rs` | Offline heavy mixed V2+CL replay with N-repeat stable timing and the same two-sided golden gate; the bottleneck-localization note separates the envelope gate from the walk | `capture-fixtures` | `heavy_mixed_solve_captures.jsonl[.zst]`; `DR_REPLAY_ITERS` | stdout; writes nothing; exits 1 on golden drift or nondeterminism | `cargo run -p degenbot-solver-lab --features capture-fixtures --bin mixed_solve_replay -- <capture.jsonl>` (no recipe today); gate behaviour is the exit code |
| `rayon_scale_probe` | `degenbot-solver-lab` | solver-lab (04, **landed**) | `tools/solver-lab/src/bin/rayon_scale_probe/main.rs` | Solver-parallelism scaling: production `solve_path_with_min_profit` under controlled thread counts, measuring wall / sum-CPU / efficiency per configuration | `capture-fixtures` | committed heavy-CL corpus; `--paths`, `--serial-passes`, `--ns`, `--bare-only`, `--skip-gate`, `--skip-control` | stdout CSV; writes nothing; a golden-lite check reproduces each path's recorded profit within `PROFIT_EPS` and excludes (never silently keeps) a flagged path | `cargo run --release -p degenbot-solver-lab --features capture-fixtures --bin rayon_scale_probe` (unchanged by the move: same arguments, same thread sweep and defaults, same CSV columns, same golden-lite exclusion; its `rayon` and `tracing` edges are OPTIONAL dependencies enabled by that same lane, so a default build compiles neither edge and never builds the probe); report `docs/rayon-parallelism-lab.md`. Measurement, not a gate — no timing threshold, and the largest matrix is never a routine gate |
| `stateview_feasibility_probe` | `degenbot-solver-lab` | solver-lab (04, **landed**) | `tools/solver-lab/src/bin/stateview_feasibility_probe/main.rs` | The signed-off **KEEP**: reusable budget-regression tool for the block-epoch data plane — tickmap clone sweep vs N, journal-replay materialization, `Arc` COW, V2 scalar costs, memory anchors, RSS | — (**no `required-features`**: it reads no capture fixture and no statement ledger, so it is a DEFAULT-features target that `just check-dev-tools default` builds) | none — synthetic in-memory V2/V3 journals and tick maps of the production entry types; no CLI argument, no fixture, no node, no environment key, no database; reads `/proc/self/statm` for RSS (Linux only) | stdout tables (the hard-coded M1–M6 feasibility/performance matrices); writes nothing | `cargo run --release -p degenbot-solver-lab --bin stateview_feasibility_probe` (unchanged by the move: same matrices, same stdout tables, same failure behaviour, source byte-identical apart from its path). Its 1001-rep/large-memory matrix is a non-routine measurement and is **compile-lane only** — never executed by a routine gate, never reported as a passing threshold. The budget rule and the historical numbers are `docs/architecture/stateview-feasibility.md` §2/§5. Its header states the keeper role directly (the stale throwaway header is retired — §5.7) |
| `synth_corpus_gen` | `degenbot-solver-lab` | solver-lab (04, **landed**) | `tools/solver-lab/src/bin/synth_corpus_gen/main.rs` | Generator of the giant-liquidity family shapes observed live (walk-heavy 27817, gate-heavy 10760/15012/26030); writes the two committed synthetic corpora in the replay and mixed schemas | — (**no `required-features`**: a DEFAULT-features target, so `just check-dev-tools default` builds it) | none — the two outputs are resolved from the repository root through `degenbot_solver_lab::paths`, which names the **pools** fixture home, so the corpora stay where their consumer reads them | **Writes** `crates/foundation/degenbot-pools/tests/fixtures/synth_giant_concentrated_liquidity.jsonl` and `..._mixed.jsonl`; no `--check`, no dry-run | `cargo run -p degenbot-solver-lab --bin synth_corpus_gen`, then `git diff --exit-code` on both outputs and `just test-rust-nextest -p degenbot-pools -E 'test(profit_envelope)'` (the kernel test that reads the concentrated-liquidity corpus). The concentrated-liquidity file re-derives byte-identically; the mixed file does not, for a reason that predates the move — see §5.3 |
| `v2_concentrated_liquidity_parity_probe` | `degenbot-pools` | removed (06e — **landed**) | *(deleted)* `crates/foundation/degenbot-pools/examples/v2_concentrated_liquidity_parity_probe.rs` | ADR-060 evidence record: V2 constant-product hop vs the same state projected as a degenerate single-range CL tick sequence, compared per swap step over a real + synthetic corpus with a zero-tolerance verdict | `capture-fixtures` | `heavy_mixed_solve_captures.jsonl.zst` plus the three committed single-path pool fixtures | stdout: one `DIVERGENCE {...}` line per non-zero delta, then one `SUMMARY {...}` line; writes nothing | §5.5 — **landed**: the decision-critical invariants are pinned by `degenbot-pools`' `tests/v2_degenerate_cl_parity.rs` (exactly 1,236 comparisons, `consumed_input == x` on every one with zero range-boundary hits, zero-for-one divergence strictly non-positive, zero divergence in the sub-`1e-3` regime, plus the non-vacuous direction/regime/dataset census); the evidence record is `docs/architecture/v2-cl-parity-probe.md`; the executable is deleted |
| `walkmemo_readout` | `degenbot-pools` | removed (06g — **landed**) | *(deleted)* `crates/foundation/degenbot-pools/examples/walkmemo_readout.rs` | Offline `WalkMemo` cross-block A/B at **synthetic maximum recurrence** (every composition recurs every epoch); reports probes/hits/cache_plays/wall per epoch | `capture-fixtures` | committed heavy-CL corpus; `DR_READOUT_EPOCHS` | stdout; exits non-zero only on fixture/parse failure — it asserts nothing about the memo | §5.6 — **landed**: nothing was extracted — the replay has no epoch structure and asserts nothing about the memo (its only failure mode is a fixture/parse error); live retention stays owned by production walk telemetry (`degenbot-bot/src/arb_engine/walk_telemetry.rs` → `logs/walkmemo_stats.jsonl` plus the `degenbot.solver.walk_memo_*` counters/gauge), memo semantics by the in-crate memo tests (`src/solve/concentrated_liquidity/tests/memo.rs`), contention by `benches/walkmemo_contention.rs`, and the historical tables by `docs/architecture/walkmemo-cross-block-reuse-readout.md`; the executable is deleted |
| `bench_strategies` | `degenbot-solver-lab` | solver-lab (04, **landed**) | `tools/solver-lab/src/bin/bench_strategies/main.rs` | Spike-S2 microbench of the order index's cost components (build, insert, non-vertex update/remove, per-block `top_k`) — the instrument the incremental-maintenance design is justified by, and the source of ADR-024's addendum numbers | `envelope` (both `degenbot-order-index` and the `alloy-primitives` its points are typed in are OPTIONAL dependencies behind that one lane) | none — synthetic points hard-coded in source, both the N=1M run and the separate 10M target leg; no CLI argument, no environment key | stdout report; writes nothing; timing only, never asserted | `cargo run --release -p degenbot-solver-lab --features envelope --bin bench_strategies` (source byte-identical apart from its path and its `Run:` line; same binary name, same point generation, same measurement order, same report text, same failure behaviour). The 1M/10M matrix is **compiled and never executed** by a routine gate — no smaller mode exists and none was invented; numbers recorded in `docs/adr/ADR-024-net-profit-order-index.md` §Addendum. **Benchmark role — not a gate, not a demo** (see §5.7) |
| `scale_demo` | `degenbot-order-index` | removed (06f — **landed**) | *(deleted)* `crates/foundation/degenbot-order-index/examples/scale_demo.rs` | 1M-point end-state load demo for `EnvelopeIndex`: build time, hull vertices, hot-set share, `top_k(50)` time, top-5 ids | `envelope` | none — deterministic LCG in source | stdout only; **no assertion of any kind** | §5.2 — **landed**: nothing was extracted — the file held no assertion of any kind; differential hot/cold correctness stays owned by `degenbot-order-index`'s `tests/envelope_invariant.rs` (proptest against the brute `ScanTopK` reference) and scale measurement stays owned by `degenbot-solver-lab`'s `bench_strategies` (ADR-024 §Addendum); the executable is deleted |

### 4.4 Simulation, arbitrage, and bot laboratories — `degenbot-engine-lab` (task 05 owns 19 rows: 18 move, `mirror_coverage_probe` preserved and removed by 06)

| Target | Package | Destination | Current path (under `rust/`) | Purpose | Features | Needs | I/O and write behavior | Verification route |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `aot_prepare_executor` | `degenbot-engine-lab` | engine-lab (05j, **landed**) | `tools/engine-lab/src/bin/aot_prepare_executor/main.rs` | Production executor AOT prep lane: compile the injected `cmd_executor` runtime bytecode once into the durable artifact store the process-shared JIT holder opens (`<persistence.state_dir>/aot-artifacts`) and prove it loads back as `ProgramKind::Aot` | `evm2-jit` | committed `crates/foundation/degenbot-executor/releases/v2/deployment/cmd_executor_runtime_bytecode.txt`, rooted through `degenbot_engine_lab::paths::repo_root()` | **Writes** the artifact store (temp sibling + rename, idempotent) under the resolved state dir | `just aot-prepare-executor` (unchanged recipe name, ARGS pass-through and `--release` posture, now `-p degenbot-engine-lab --features evm2-jit --bin aot_prepare_executor`); the compile lane is `just check-dev-tools --engine-jit`. Operational tool, explicitly **not** a production CLI |
| `bal_candidate_scaling_probe` | `degenbot-engine-lab` | engine-lab (05g1, **landed**) | `tools/engine-lab/src/bin/bal_candidate_scaling_probe/main.rs` | Analytical BAL candidate-scaling report: two-corpus calibration closure, candidate-count × family × novelty matrix, W/F sensitivity — pure formula over committed corpus aggregates, no candidate executed | — (no `required-features`: the default lane builds it) | committed corpus accounting embedded in the product-owned `degenbot_simulation::probe_support` (`candidate_scaling`) consumed directly | stdout Markdown; writes nothing; `BAL_SCALING_PLANT_MISMATCH=1` fails the gate and exits non-zero (red probe) | `just bal-candidate-scaling-probe` (offline; unchanged recipe name and `--release` posture, now `-p degenbot-engine-lab --bin bal_candidate_scaling_probe`); behavior pins in `crates/engine/degenbot-simulation/tests/bal_candidate_scaling.rs` |
| `bal_carry_probe` | `degenbot-engine-lab` | engine-lab (05e1, **landed**) | `tools/engine-lab/src/bin/bal_carry_probe/main.rs` | Parent-linked BAL carry-forward against live reth: every canonical block of a captured range advanced through `ReadHubCache::advance_bal`, each candidate anchor's carried values compared against independent cold reads | — (no `required-features`: the default lane builds it) | LIVE reth/archive node (`DEGENBOT_RPC_HTTP_CHAINID_1` / `TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI`); a recorded `fanout-trace` file via `BAL_CARRY_TRACE` (`BAL_CARRY_FIRST`/`BAL_CARRY_LAST`); the recorded corpus comes from the product-owned `degenbot_simulation::probe_support` consumed directly | stdout; writes nothing; any value mismatch is a hard failure (non-zero exit) | `just bal-carry-probe` (unchanged recipe name, RPC declaration and fails-closed shell gate, `--release` posture and env controls; it now runs `-p degenbot-engine-lab --bin bal_carry_probe`); its per-anchor accounting is the reference `bal_production_relevance_probe` and `bal_wall_ceiling_probe` cross-check against. **Live-only: compile lane in CI, never claim a refreshed measurement** |
| `bal_delta_rounds_probe` | `degenbot-engine-lab` | engine-lab (05f1, **landed**) | `tools/engine-lab/src/bin/bal_delta_rounds_probe/main.rs` | Round-coalesced delta read replay over two live-captured corpora: full-clone vs delta chain vs idle-block retention, with the re-read gap distribution the retention decision needs | — (no `required-features`: the default lane builds it) | **EXTERNAL CORPUS, absent from the tree**: `BAL_DELTA_ROUNDS_TRACES` defaults to `/tmp/fanout_trace.log` and `/tmp/bal_broader_trace.log`, both REQUIRED (absent corpus = hard failure, never an empty replay); matching real-coverage captures (`rust/crates/engine/degenbot-simulation/tests/fixtures/bal_real_coverage{,_broader}.json`) default by corpus index, rooted through `degenbot_engine_lab::paths::repo_root()` from the product-owned `degenbot_simulation::probe_support` (`modeled_chain`/`real_coverage`/`recorded_corpus`/`session_drive`/`wall_ledger`) consumed directly | stdout; `BAL_DELTA_ROUNDS_PLANT_DIVERGENCE=1` must exit non-zero; measurements land in `logs/bal_delta_rounds_result.md` (gitignored, `logs/*`) | Compile lane only until the traces are supplied: `cargo check -p degenbot-engine-lab --bin bal_delta_rounds_probe`; `just bal-delta-rounds-probe` (unchanged recipe name and `--release` posture, now `-p degenbot-engine-lab --bin bal_delta_rounds_probe`). Unexecutable offline — report as such, do not fabricate a run |
| `bal_delta_snapshot_probe` | `degenbot-engine-lab` | engine-lab (05e2, **landed**) | `tools/engine-lab/src/bin/bal_delta_snapshot_probe/main.rs` | Full-clone vs parent-linked delta-chain snapshot measurement at realistic working-set sizes (advance latency, lookup, compaction, walk depth, RSS) with a correctness leg and a planted-divergence negative probe | — (no `required-features`: the default lane builds it) | committed `rust/crates/engine/degenbot-simulation/tests/fixtures/bal_real_coverage.json` via the product-owned `degenbot_simulation::probe_support` (`delta_parity`/`modeled_chain`/`real_coverage`) consumed directly, rooted through `degenbot_engine_lab::paths::repo_root()`; `BAL_DELTA_SIZES`, `BAL_DELTA_REPS`, `BAL_DELTA_LOOKUPS`, `BAL_DELTA_MAX_DEPTH` | stdout; `BAL_DELTA_PLANT_DIVERGENCE=1` must exit non-zero; results in `logs/bal_delta_snapshot_result.md` | `just bal-delta-snapshot-probe` (offline, moderate cost; unchanged recipe name and `--release` posture, now `-p degenbot-engine-lab --bin bal_delta_snapshot_probe`); the parity leg runs in `crates/engine/degenbot-simulation/tests/bal_delta_snapshot.rs`. Bounded default sizes only — never the largest matrix in a routine gate |
| `bal_head_lifecycle_probe` | `degenbot-engine-lab` | engine-lab (05e3, **landed**) | `tools/engine-lab/src/bin/bal_head_lifecycle_probe/main.rs` | Canonical-head lifecycle: prepare (BAL fetch/decode) and apply stages measured separately, plus the offline waiter leg over a controllably deferred transport; every settled write compared to an independent cold read | — (no `required-features`: the default lane builds it) | LIVE node for the historical and fresh-head legs; committed capture for the waiter leg, its real-coverage fixture root resolved through `degenbot_engine_lab::paths::repo_root()` from the product-owned `degenbot_simulation::probe_support` (`head_legs`/`real_coverage`) consumed directly; `BAL_HEAD_*` knobs | stdout report **and** a machine-readable JSON summary when `BAL_HEAD_SUMMARY_OUT` names a path (`schema: bal-head-lifecycle/1`) | `just bal-head-lifecycle-probe` (unchanged recipe name, RPC declaration and fails-closed gate; now `-p degenbot-engine-lab --bin bal_head_lifecycle_probe`) + `just bal-head-lifecycle-compare` (unchanged; `scripts/bal_head_lifecycle_compare.py`, which refuses another schema identity or a leg that measured nothing); offline pins in `crates/engine/degenbot-simulation/tests/bal_head_lifecycle.rs`, `.../bal_head_lifecycle_probe.rs`, `.../bal_head_pipeline.rs`. Live legs = **compile lane only** |
| `bal_production_relevance_probe` | `degenbot-engine-lab` | engine-lab (05f2, **landed**) | `tools/engine-lab/src/bin/bal_production_relevance_probe/main.rs` | Paired relevance replay: production-derived relevance vs the recorded-read oracle on the same captured corpus, measuring what the difference costs; four gates including exact reproduction of the live `bal_carry_probe` accounting | — (no `required-features`: the default lane builds it) | **EXTERNAL CORPUS, absent from the tree**: `BAL_PROD_TRACE` defaults to `/tmp/bal_production_trace.log` and is REQUIRED (absent corpus = hard failure, never an empty replay); committed `rust/crates/engine/degenbot-simulation/tests/fixtures/bal_real_coverage_production.json` via `BAL_PROD_FIXTURE`, rooted through `degenbot_engine_lab::paths::repo_root()`; the corpus/relevance/report support is the product-owned `degenbot_simulation::probe_support` (all seven modules: `modeled_chain`/`probe_report`/`real_coverage`/`recorded_corpus`/`relevance_pairs`/`session_drive`/`wall_ledger`) consumed directly | stdout; `BAL_PROD_PLANT_DIVERGENCE=1` must exit non-zero; measurements land in `logs/bal_production_relevance_result.md` (gitignored, `logs/*`); the fixture is regenerated by a separate live capture (`BAL_WALL_FIXTURE_CAPTURE=1 just bal-real-coverage-capture`) | Compile lane only until the trace exists; offline pins in `crates/engine/degenbot-simulation/tests/bal_production_relevance.rs` |
| `bal_real_read_oracle_probe` | `degenbot-engine-lab` | engine-lab (05f3, **landed**) | `tools/engine-lab/src/bin/bal_real_read_oracle_probe/main.rs` | Real-candidate read-set oracle replay: the post-fix soak's `fanout-trace` rows replayed through exact-anchor BAL generations, every recorded read compared against an independent cold read | — (no `required-features`: the default lane builds it) | LIVE node (`DEGENBOT_RPC_HTTP_CHAINID_1` / `TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI`); the soak trace `BAL_READ_ORACLE_TRACE` defaults to `logs/bal_postfix_soak_trace.log`, which is **gitignored and not in the tree** (absent corpus = hard failure, never an empty replay); the selection/comparison/seed support is the product-owned `degenbot_simulation::probe_support` (`read_oracle`) consumed directly | stdout; `BAL_READ_ORACLE_PLANT_WEI=1` must exit non-zero | `just bal-real-read-oracle-probe` (unchanged recipe name, RPC declaration and fails-closed gate; now `-p degenbot-engine-lab --bin bal_real_read_oracle_probe`); recorded result `docs/architecture/bal-real-read-oracle-result.md`. **Live + external corpus: compile lane here, measurements not refreshed** |
| `bal_retention_probe` | `degenbot-engine-lab` | engine-lab (05e4, **landed**) | `tools/engine-lab/src/bin/bal_retention_probe/main.rs` | Idle-block retention measurement over both committed real write-coverage captures and one synthetic long run, at thresholds 1/2/4/8 against the unbounded control; every identity verified against an independent cold read | — (no `required-features`: the default lane builds it) | committed `rust/crates/engine/degenbot-simulation/tests/fixtures/bal_real_coverage{,_broader}.json` via the product-owned `degenbot_simulation::probe_support` (`modeled_chain`/`real_coverage`) consumed directly, rooted through `degenbot_engine_lab::paths::repo_root()`; `BAL_RETENTION_THRESHOLDS`, `BAL_RETENTION_SYNTH_*` | stdout; `BAL_RETENTION_PLANT_DIVERGENCE=1` must exit non-zero; results in `logs/bal_retention_result.md` | `just bal-retention-probe` (offline; unchanged recipe name, `--release` posture and env controls; it now runs `-p degenbot-engine-lab --bin bal_retention_probe`); the parity pins live in `crates/engine/degenbot-simulation/tests/` |
| `bal_wall_ceiling_probe` | `degenbot-engine-lab` | engine-lab (05g2, **landed**) | `tools/engine-lab/src/bin/bal_wall_ceiling_probe/main.rs` | Deterministic wall-clock ceiling benchmark of BAL carry-forward: the recorded read walks under three cache policies, candidate wall attributed to six reconciled buckets, the eliminable storage-wait ceiling per price point, in both write-coverage modes | — (no `required-features`: the default lane builds it) | **EXTERNAL CORPUS**: `BAL_WALL_TRACE` defaults to `/tmp/fanout_trace.log`; committed `rust/crates/engine/degenbot-simulation/tests/fixtures/bal_real_coverage.json` / `..._broader.json` via `BAL_WALL_FIXTURE`, the default capture rooted through `degenbot_engine_lab::paths::repo_root()` from the product-owned `degenbot_simulation::probe_support` (`modeled_chain`/`probe_report`/`real_coverage`/`recorded_corpus`/`session_drive`/`wall_ledger`) consumed directly | stdout + `logs/` results; `BAL_WALL_FIXTURE_CAPTURE=1` is the live fixture-write mode (its own recipe `just bal-real-coverage-capture`); `BAL_WALL_PLANT_DIVERGENCE=1` must exit non-zero | `just bal-wall-ceiling-probe` (unchanged recipe name and `--release` posture, now `-p degenbot-engine-lab --bin bal_wall_ceiling_probe`); instrumentation pins in `crates/engine/degenbot-simulation/tests/bal_wall_ceiling.rs`. Measurement — timing is reported, never asserted |
| `capture_seam_probe` | `degenbot-engine-lab` | engine-lab (05a, **landed**) | `tools/engine-lab/src/bin/capture_seam_probe/main.rs` | JIT-composition probe: does the production capture (access-list slots + swap events) survive OFF the `Inspector` hook, i.e. can compiled execution carry it? Three configurations A/B/C with a set-equality PASS bar | `evm2-jit` | offline; the corpus comes from `degenbot_engine_lab::bench_corpus`, the tool library's corpus support (read anchors resolved from the repository root, no `CARGO_MANIFEST_DIR` arithmetic) | stdout; writes nothing; prints PASS/FAIL | `cargo run --release -p degenbot-engine-lab --features evm2-jit --bin capture_seam_probe` (unchanged command shape from the catalog row: same binary name, same feature lane, same three configurations, same PASS/FAIL bar, same reverted-subcall leg); the durable JIT verdict parity it feeds is enforced automatically by `crates/engine/degenbot-simulation/tests/jit_spine.rs` (runner parity in `Seam` capture mode, the production configuration) and `.../frame_replay_jit.rs`; the probe itself is diagnostic-only (§5.7 audit) |
| `cold_miss_fanout_probe` | `degenbot-engine-lab` | engine-lab (05b, **landed**) | `tools/engine-lab/src/bin/cold_miss_fanout_probe/main.rs` | Cold-miss cost and coroutine fan-out: eleven legs over a deterministic offline RPC stand-in charging per modeled round trip; the decision instrument for the evm2 coroutine-fan-out chunk | — (no `required-features`: the default lane builds and runs it) | offline — snapshot-derived state, `COLD_MISS_LATENCY_US`, `COLD_MISS_FRAMES`; the corpus comes from `degenbot_engine_lab::bench_corpus`, read anchors from `degenbot_engine_lab::paths` | stdout; writes nothing; timing reported, never asserted | `cargo run --release -p degenbot-engine-lab --bin cold_miss_fanout_probe` (no recipe; unchanged command shape, same env controls, same output); bounded routine smoke `COLD_MISS_FRAMES=2 COLD_MISS_LATENCY_US=1`, whose legs keep their own verdict-identity and prefetch assertions — the default N=8 sweep is a non-routine measurement |
| `mirror_coverage_probe` | `degenbot-simulation` | removed (06 — **landed**) | *(deleted)* `crates/engine/degenbot-simulation/examples/mirror_coverage_probe.rs` | BotState-mirror overlap probe: of a real mainnet candidate's cold SLOAD set, how many keys the sim anchor already carries, and whether the carried word is *serveable* or only *comparable* | — | offline — two frames from the committed `degenbot-strategy/tests/fixtures/frame_replay_capture.jsonl`, prestate-tracer slot sets pinned in source | stdout classification tables; **asserts** that a registered pool projects its scalar rows into the anchor (an empty projection is a build defect) | §5.4 — **landed**: the measured table lives in `docs/architecture/sim-anchor-mirror-coverage.md` and the offline projection/classification regression in `degenbot-simulation`'s `tests/mirror_anchor_coverage.rs`; the executable is deleted |
| `session_fanout_probe` | `degenbot-engine-lab` | engine-lab (05d3, **landed**) | `tools/engine-lab/src/bin/session_fanout_probe/main.rs` | Session-scheduler benchmark: the recorded fan-out read walks driven through the committed `ReadHub` under the committed W/F bound, walled against serial / hybrid / fibers / per-lane reads under one latency model | — (no `required-features`: the default lane builds and runs it) | **EXTERNAL CORPUS**: `SESSION_FANOUT_TRACE` defaults to `/tmp/fanout_trace.log` and its absence is a hard failure; the fixture support is the product-owned `degenbot_simulation::probe_support` (modeled chain, corpus, session drive, wall ledger, report) consumed directly — no `#[path]` wiring and no duplicated support | stdout; `SESSION_FANOUT_PLANT_DIVERGENCE=1` must exit non-zero (per-candidate read-digest gate) | `just session-fanout-probe` (unchanged recipe name, `--release` posture and env controls; it now runs `-p degenbot-engine-lab --bin session_fanout_probe`); the real-frame scheduler behaviour it approximates is owned by `crates/engine/degenbot-simulation/tests/async_fanout.rs` and `.../session_async.rs`. Compile lane only without the trace |
| `sim_mode_bench` | `degenbot-engine-lab` | engine-lab (05d, **landed**) | `tools/engine-lab/src/bin/sim_mode_bench/main.rs` | ADR-089 phase-6 offline engine benchmark: one corpus, engine modes (interp / jit-block / jit-pool / aot / replay / replay-jit), first-run vs steady-state latency, JIT compile telemetry, cross-mode verdict identity | `evm2-jit` | offline — committed fixture spine (real `cmd_executor` creation artifact + tier-3 stubs); `SIM_BENCH_ITERS` | stdout; writes nothing (the AOT mode's scratch store is a temp path) | `just bench-sim-modes [args]` (release); the decision instrument for the AOT/worker-pool tasks — measurement only. Bounded routine smoke `SIM_BENCH_ITERS=10 just bench-sim-modes all` (above the pool's hot threshold, which is the sim fan-out width 8, so `jit-pool`'s upgrade lands inside the run) — every mode green, both verdict-identity families OK |
| `worker_fanout_probe` | `degenbot-engine-lab` | engine-lab (05c, **landed**) | `tools/engine-lab/src/bin/worker_fanout_probe/main.rs` | Per-worker EVM fan-out probe (ADR-089 concurrency trio item 4): serial shared `&mut evm` vs per-worker threads vs per-candidate fibers vs warm fibers, cross-leg verdict identity gated | — (no `required-features`: the default lane builds and runs it) | offline — shape-matched corpus built in code; `WORKER_FANOUT_SHAPE`, `WORKER_FANOUT_CANDIDATES`, `WORKER_FANOUT_WORKERS`, `COLD_MISS_LATENCY_US`, `WORKER_FANOUT_ACCOUNT_US`; `WORKER_FANOUT_TRACE` optionally points at a recorded trace; the corpus comes from `degenbot_engine_lab::bench_corpus`, read anchors from `degenbot_engine_lab::paths` | stdout; `WORKER_FANOUT_PLANT_DIVERGENCE=1` must exit non-zero | `just worker-fanout-probe` (unchanged recipe name, `--release` posture and env controls; it now runs `-p degenbot-engine-lab --bin worker_fanout_probe`); bounded routine smoke `WORKER_FANOUT_CANDIDATES=2 WORKER_FANOUT_WORKERS=2 COLD_MISS_LATENCY_US=1 WORKER_FANOUT_ACCOUNT_US=1`, whose identity gate stays green — the default 21-candidate sweep is a non-routine measurement. Results' provenance `logs/bal_matched_parity_matrix.md`. Measurement, non-gating |
| `bal_dispatch_scaling_probe` | `degenbot-engine-lab` | engine-lab (05h2, **landed**) | `tools/engine-lab/src/bin/bal_dispatch_scaling_probe/main.rs` | Controlled BAL dispatch-scaling probe: the production dispatch entry point over one scripted head, one candidate list, one deployment and one provider state, from one candidate to a burst fan-out, with only the storage representation varying | — (no `required-features`: the default lane builds it) | offline — scripted chain/market in source; `BAL_DISPATCH_SCALING_COUNTS`, `BAL_DISPATCH_SCALING_REPS`; the dispatch/chain/scaling support is the product-owned `degenbot_arbitrage::probe_support` (`dispatch_probe`/`dispatch_scaling`) consumed directly | stdout Markdown report; never asserts timing; **fails closed (exit 1)** on a broken fixture (wrong lane, cold control fetching a BAL, missing prefetch identity, missing read key, cross-lane verdict difference) | `just bal-dispatch-scaling-probe` (unchanged recipe name and `--release` posture; it now runs `-p degenbot-engine-lab --bin bal_dispatch_scaling_probe`); the durable scripted value-sensitive parity matrix is `crates/engine/degenbot-arbitrage/tests/bal_dispatch_scaling.rs`; regeneration notes `logs/bal_dispatch_scaling_result.md`, result `docs/architecture/bal-matched-parity-result.md` |
| `bal_live_acquisition_probe` | `degenbot-engine-lab` | engine-lab (05i, **landed**) | `tools/engine-lab/src/bin/bal_live_acquisition_probe/main.rs` | Live reth BAL acquisition: what it costs to acquire ONE fixed historical block's EIP-7928 access list in both official representations, plus per-fan-out packet prices at that anchor | — (no `required-features`: the default lane builds it) | LIVE archive node (`TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI` / `DEGENBOT_RPC_HTTP_CHAINID_1`); `BAL_ACQ_ANCHOR`, `BAL_ACQ_REPS`, `BAL_ACQ_CORPUS_COUNTS`; the dispatch/runtime support is the product-owned `degenbot_arbitrage::probe_support` (`dispatch_probe`/`dispatch_scaling`) consumed directly | stdout; writes nothing; seed-parity disagreement and settled-value mismatch are hard failures | `just bal-live-acquisition-probe` (unchanged recipe name, declared-RPC gate, `--release` posture and env controls; it now runs `-p degenbot-engine-lab --bin bal_live_acquisition_probe`); priced conclusions in `docs/architecture/bal-matched-parity-result.md`. **Live-only: compile lane in CI** |
| `hotpath_prometheus_probe` | `degenbot-engine-lab` | engine-lab (05, **landed**) | `tools/engine-lab/src/bin/hotpath_prometheus_probe/main.rs` | Smallest end-to-end proof that a guard-enabled build serves profiler metrics on `:6772`: installs the typed config from the environment, constructs the same guard the pump holds, scrapes `/metrics` and asserts the always-present process families plus a `measure_block!`-sampled family | `prometheus` (manifest-declared: the replacement `[[bin]]` carries `required-features = ["prometheus"]`) | offline but binds a local TCP port (`HOTPATH_PROMETHEUS_PORT`, default 6772); `DEGENBOT_HOTPATH=1` required | stdout; writes nothing; non-zero exit on any wiring breakage | `DEGENBOT_HOTPATH=1 cargo run -p degenbot-engine-lab --features prometheus --bin hotpath_prometheus_probe`. No recipe and no test names it today — its CI obligation is the **Prometheus compile lane** (`just check-dev-tools --engine-prometheus`), documented in `docs/grafana/HOTPATH.md` |

### 4.5 Assertion-only smoke → owning-crate integration tests (task 06, 2 rows)

| Target | Package | Destination | Current path (under `rust/`) | Purpose | Features | Needs | I/O and write behavior | Verification route |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `config_smoke_boot` | `degenbot-bot` | integration test (06b, **landed**) | `crates/engine/degenbot-bot/tests/config_smoke_boot.rs` | The lightest start-up path of the bot engine driven by a config TOML file only: the test spawns its own test executable twice — a guard child planted with one `DEGENBOT_*` variable must exit non-zero naming it, and a clean child (environment constructed empty apart from the child-mode flag, so the no-`DEGENBOT_*` guard runs as a real process-environment check) performs the sole `.without_env()` file-layer load, the sole typed-config install, the `EngineStages::with_core_cfg` construction, and prints the four observations | — | the committed fixture `tests/fixtures/config_smoke_boot.toml`; **process-isolated by construction** — `stance::install` is a process global, so both boot runs happen in spawned child processes and the test runner's own process never installs a stance | writes nothing; the parent asserts the guard child's non-zero exit plus refusal text and the clean child's `streaming_delivery=false` / `min_profit_wei` / `solve_cpus` unset / `metrics_addr` observation line (children run `--nocapture` so their libtest harness cannot swallow the output) | `just test-rust-nextest -p degenbot-bot -E 'binary(config_smoke_boot)'` (one dedicated integration binary with ONE test, identical under nextest and `cargo test`). Gate: `just test-rust-nextest -p degenbot-bot` |
| `headless_boot` | `degenbot-ingestion` | integration test (06a, **landed**) | `crates/foundation/degenbot-ingestion/tests/headless_boot.rs` | Headless standalone-Rust boot over the `degenbot-ingestion` public surface with NO Python and NO network: a synthetic fused event stream through the topic filter, the watchdog windows constructing and evaluating, and the backfill filter builder producing a server-side-filterable `eth_getLogs` filter | — | none — synthetic stream in source | writes nothing; named test assertions fail the run on any check failure | `just test-rust-nextest -p degenbot-ingestion -E 'binary(headless_boot)'`; `just test-standalone` keeps its two-check scope with this as its re-pointed second check |

<!-- ledger:end -->

### 4.6 The **baseline** exact-set audit (inline, no script file)

The acceptance check for *this* ledger as it stands before any move, run from the repo root:
it compares the complete Cargo example `(package, target)` set with the marked region above —
44 current targets, 44 unique ledger rows, exact-set equality, no duplicates.

**BASELINE, not a forever check.** The `44` in this block is the pre-task-`06` target count,
and the block is only meaningful while no row has been moved, converted, or deleted. Once
task `06` converts a target to an integration test or deletes a removed executable, the
correct result of *this* block is FAIL: the hard-coded `44` and the baseline set are stale by
construction. Do not "fix" it by editing the constant in place — the post-removal check in
§8.2 reuses this block's comparison logic against the then-current `cargo metadata` and the
then-current ledger, with the expected set and count supplied for that revision.

```bash
python3 - <<'PY'
import json, re, subprocess, sys
meta = json.loads(subprocess.run(
    ["cargo", "metadata", "--manifest-path", "rust/Cargo.toml",
     "--format-version", "1", "--no-deps", "--locked"],
    capture_output=True, text=True, check=True).stdout)
current = {(p["name"], t["name"]) for p in meta["packages"]
           for t in p["targets"] if "example" in t["kind"]}
body = open("rust/tools/README.md").read()
body = body.split("<!-- ledger:begin -->")[1].split("<!-- ledger:end -->")[0]
rows = re.findall(r"^\| `([A-Za-z0-9_-]+)` \| `([A-Za-z0-9_-]+)` \|", body, re.M)
ledger = {(pkg, target) for target, pkg in rows}
dupes = sorted({r for r in rows if rows.count(r) > 1})
print("current targets:", len(current))
print("ledger rows:", len(rows), "unique:", len(ledger))
print("missing from ledger:", sorted(current - ledger))
print("stale in ledger:", sorted(ledger - current))
print("duplicates:", dupes)
ok = len(current) == 44 and len(rows) == 44 and len(ledger) == 44 and current == ledger and not dupes
print("exact-set audit:", "PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
PY
```

A `PASS` line is the acceptance of the **baseline** audit, and the same three diagnostic
lines (`missing from ledger`, `stale in ledger`, `duplicates`) must print empty lists for any
revision's exact-set check; anything else is an unfinished row. For the post-removal
revision, the acceptance is §8.2's restatement — same logic and same empty diagnostics, with
`len(current)` and `len(rows)` compared to that revision's own count instead of `44`.

### 4.6.1 The post-migration audit — one ledger row, one live target

§4.6's block compares the *example* set against the ledger, so it is the right check only
while no row has moved. After task `03`, task `04`'s first six slices, and task `05`'s first
slice eighteen rows **have** moved, and the honest form of
the same comparison unions the two target kinds a row can now name: a Cargo example target
(rows not yet moved) and a tool-package `[[bin]]` target (rows whose destination has landed).
That union is what §2's partition promises — every one of the 44 original targets exists
exactly once — and it is what a reader can actually verify today:

```bash
python3 - <<'PY'
import json, re, subprocess, sys
meta = json.loads(subprocess.run(
    ["cargo", "metadata", "--manifest-path", "rust/Cargo.toml",
     "--format-version", "1", "--no-deps", "--locked"],
    capture_output=True, text=True, check=True).stdout)
TOOL_PACKAGES = {"degenbot-fixture-tools", "degenbot-solver-lab", "degenbot-engine-lab"}
current = set()
for p in meta["packages"]:
    for t in p["targets"]:
        if "example" in t["kind"]:
            current.add((p["name"], t["name"]))
        elif "bin" in t["kind"] and p["name"] in TOOL_PACKAGES:
            current.add((p["name"], t["name"]))
body = open("rust/tools/README.md").read()
body = body.split("<!-- ledger:begin -->")[1].split("<!-- ledger:end -->")[0]
rows = re.findall(r"^\| `([A-Za-z0-9_-]+)` \| `([A-Za-z0-9_-]+)` \|", body, re.M)
ledger = {(pkg, target) for target, pkg in rows}
dupes = sorted({r for r in rows if rows.count(r) > 1})
print("ledger rows:", len(rows), "unique:", len(ledger))
print("missing from ledger:", sorted(current - ledger))
print("stale in ledger:", sorted(ledger - current))
print("duplicates:", dupes)
ok = len(rows) == 44 and len(ledger) == 44 and current == ledger and not dupes
print("union audit (examples + tool binaries):", "PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
PY
```

At this revision the acceptance is: `44` ledger rows, `44` unique, empty `missing`, empty
`stale`, no duplicates, and `PASS` — over `11` example targets plus the `9`
`degenbot-fixture-tools`, `8` `degenbot-solver-lab`, and `16` `degenbot-engine-lab`
binaries. §4.6's baseline block is
expected to print `FAIL` here and to name exactly those thirty-three rows as `stale in ledger`:
they are not stale, they moved, and this block is where that is visible instead of argued.

**Boundary.** This audit stays valid while every ledger row still exists as *some* target.
Task `06` deletes four executables and converts two examples into integration tests, so its
change must delete those six rows in the same edit; from then on the union is the three
retainers plus the 35 tool binaries, and the count is that revision's, not `44`.

### 4.7 Feature-column provenance — the row that was not metadata-declared

`hotpath_prometheus_probe` was the only row whose `Features` cell was not a Cargo
`required-features` value: `cargo metadata` reported `required-features: null` for
`degenbot-bot`'s example, so its `hotpath-prometheus` need lived in the source and in
the `DEGENBOT_HOTPATH=1` runtime wiring plus the `just check-dev-tools --engine-prometheus`
compile lane, **not** in the manifest. No row may be described as metadata-declared that the
metadata does not declare — that is exactly the kind of claim a later task inherits and
trusts.

Task 05 landed the declaration with the replacement binary: the `[[bin]]
hotpath_prometheus_probe` in `rust/tools/engine-lab/Cargo.toml` carries
`required-features = ["prometheus"]`, forwarding
`prometheus = ["dep:degenbot-bot", "dep:degenbot-config", "dep:degenbot-substrate",
"dep:hotpath", "degenbot-bot/hotpath-prometheus"]`. The row is metadata-declared now; its
CI obligation remains the Prometheus compile lane (`just check-dev-tools --engine-prometheus`),
and execution claims come only from bounded local scrape runs.

## 5. The six review dispositions

Each of these was flagged as ambiguous in review. Each is resolved here from **current**
source and decision evidence — never from age, and never from a lack of references (an
instrument with no inbound reference is not thereby dead: `stateview_feasibility_probe` has
no `justfile` recipe either and is a signed-off keeper).

### 5.1 `config_smoke_boot` → **promote to a `degenbot-bot` integration test** (task 06)

Evidence: 77 lines, no RPC needed, `#![expect(clippy::print_stdout)]`. It carries three real
assertions a probe-only life would lose: the clean-environment guard (any visible
`DEGENBOT_*` variable is a violation), the file-layer load with `.without_env()`, and the
end-to-end stance observation (`streaming_delivery_probe()`, `cfg.solve.min_profit_wei`,
`solve_cpus.is_none()`, `telemetry.metrics_addr`) printed from the engine the pump would
build. Its failure mode is a `Result<(), String>` — an exit code, exactly what a test
asserts.

Disposition: **assertion-only smoke, so it becomes an assertion.** Task 06 moves it into
`crates/engine/degenbot-bot/tests/` with a local config fixture. The hard constraint is
process isolation: `degenbot_substrate::stance::install` installs a process-global config and
`EngineStages::with_core_cfg` builds the engine from it, so the test must own its own test
binary (or be otherwise isolated from anything that installs a stance) and must not leak the
installed config into sibling tests. Preserve verbatim: the no-`DEGENBOT_*` environment
proof, the `.without_env()` loader attachment, and the four observed values. The executable
then goes. **Landed (06b)**: the test lives at `crates/engine/degenbot-bot/tests/config_smoke_boot.rs`
(one dedicated integration binary, one test) with the committed fixture
`tests/fixtures/config_smoke_boot.toml`; because this devcontainer's ambient environment
carries `DEGENBOT_*` variables, both boot runs execute in self-spawned child processes —
the guard child (planted variable refused, non-zero exit naming it) and the clean child
(environment built empty apart from the child-mode flag), so the guard stays a real
process-environment check and the test runner's process never installs a stance.

### 5.2 `scale_demo` → **delete as redundant** (DONE — nothing to extract, executable removed)

Evidence: it prints and returns — there is **no assertion of any kind** in the file; "the
demo's stdout report is its output". Its two claims are already owned elsewhere:
the differential correctness of the hot/cold split is the `degenbot-order-index` invariant
suites (`tests/envelope_invariant.rs` drives `EnvelopeIndex` against the brute `ScanTopK`
reference through proptests plus `pruning_shrinks_hot_set_for_dominated_data` and
`envelope_matches_scan_topk`), and ADR-024's scale numbers are **`bench_strategies`'**
numbers (§Addendum: per-block `top_k` 80.2 ms → 21.9 ms at 1M, 145 ms at 10M, insert
169 ns → 198 ns). No doc, test, or recipe cites `scale_demo`; its stdout is recorded
nowhere.

Disposition: **delete as redundant** — nothing unique to preserve. The one thing it does
that the suites do not is run the differential *at end-state scale* (N=1M); if that ever
matters, it is a new bounded integration test (`envelope` feature, N=100k, `top_k` vs
`ScanTopK`, `hot_len` ≪ N), not a resurrected demo. Task 06 deletes the file; task 04 does
not move it into `degenbot-solver-lab` at all — a file that task 06 deletes is not worth a
manifest stanza, a path re-anchoring, and a verification run, so it stays where it is in
`crates/foundation/degenbot-order-index/examples/` until then.

**Done (06f):** the pre-deletion re-check confirmed the evidence: the file held **no assertion of
any kind** (print-and-return; its stdout is recorded nowhere), no recipe, test, or doc cited it,
and both owners were live in the tree — `tests/envelope_invariant.rs` owns the differential
hot/cold correctness (proptests driving `EnvelopeIndex` against `ScanTopK`, plus
`pruning_shrinks_hot_set_for_dominated_data` and `envelope_matches_scan_topk`), and
`degenbot-solver-lab`'s `bench_strategies` owns the scale measurement (ADR-024 §Addendum).
The example file and its `[[example]]` stanza left the tree in the same change; the speculative
bounded large-N test stays unbuilt and the 1M/10M matrix was never executed.

### 5.3 `synth_corpus_gen` → **retain as a `degenbot-solver-lab` tool** (task 04)

Evidence: it is the producer of two *committed* corpora —
`crates/foundation/degenbot-pools/tests/fixtures/synth_giant_concentrated_liquidity.jsonl`
and `synth_giant_mixed.jsonl` (4.2 MB each, tracked) — and the first is consumed by the pools
kernel tests (`src/solve/profit_envelope/kernel.rs` names the file at its fixture path). The
only alternative producer of heavy corpus material,
`concentrated_liquidity_capture_gen`, needs a live archive node and produces *real* pools,
not the giant-liquidity family shapes these corpora exercise. `path_sim.rs`'s test helper
also documents its construction pattern as the reference geometry.

Disposition: **retain**, as a documented generator with an explicit write contract. It has
no `--check` and no dry-run, and it wrote into `CARGO_MANIFEST_DIR/tests/fixtures` — after the
move that anchor means "the tool package", which would silently create a second fixtures
directory. Task 04's first slice landed the re-point: the output home is
`degenbot_solver_lab::paths::pools_fixtures_dir()`, a marker-resolved repository root plus the
pools fixture path, so both corpora keep their committed location.

The regeneration proof is split, and the split is a finding, not a threshold:
`synth_giant_concentrated_liquidity.jsonl` re-derives **byte-identically** (`git diff
--exit-code` empty) and its pools kernel consumer passes. `synth_giant_mixed.jsonl` does
**not** re-derive: the committed bytes spell the hop discriminant `"kind":"CL"`, while the
generator has emitted `"kind":"concentrated_liquidity"` since `1cec49bf6` renamed the wire
kind ("CL sweep step 6"), and that rename did not regenerate the corpus. The drift therefore
predates the move and is not created by it — the moved binary reproduces its source, not the
stale artifact. Nothing in the tree consumes the mixed corpus (the mixed replay reads
`heavy_mixed_solve_captures.jsonl`, and it would skip `"CL"` rows as an unknown hop kind), so
the honest report is: **not regenerated green**, committed bytes left untouched, decision for
task 06/07.

### 5.4 `mirror_coverage_probe` → **archived after preserving durable evidence** (DONE — doc + offline test landed, executable removed)

Evidence: fully offline — the two read sets are pinned constants taken from the node's
`prestateTracer` over two frames of the committed
`degenbot-strategy/tests/fixtures/frame_replay_capture.jsonl`, and the pools are registered
at their measured pre-state. It answers a question with lasting value: of a real
candidate's cold SLOAD set, how many keys the sim anchor already carries, and whether the
carried word is *serveable* (byte-identical to the chain word) or only *comparable* (tracked
fields packed, untracked bits zeroed). That distinction is the reason the anchor cannot
serve state, and the contract itself lives durably in the
`degenbot_substrate::divergence_probe` module docs — but **the measured overlap table lives
nowhere**: no doc, test, or recipe cites this target (its only inbound reference is the
`too_many_lines` waiver census).

Disposition: **archive after preserving durable evidence.** Task 06 must (a) move the
overlap/classification result into a durable architecture doc, (b) extract the one real
assertion — a registered pool must project its scalar rows into `SimAnchorState`; an empty
projection means the registry's entry insert was compiled away and every anchor-served read
silently falls through — into an offline `degenbot-simulation` integration test over the two
committed frames, and then (c) delete the example. Do not delete the assertion with the
executable: step (b) is the replacement, and step (a) is what makes step (c) honest.

Task 05 did not move, rename, or delete this target, and task 06 did not move it into
`degenbot-engine-lab` first — that would have bought one manifest stanza and one re-anchoring
for a file that was about to be deleted, and would have put the 14 `#[path]`-coupled
simulation test files at risk (§10.2) for no gain.

**Done (task 06):** the measured overlap/classification table and its interpretation — frame
and block provenance, the account-row caveat, and why a comparable carried word is not
serveable — live in `docs/architecture/sim-anchor-mirror-coverage.md`; the offline
`degenbot-simulation` integration test is `tests/mirror_anchor_coverage.rs` (both frames,
exact tallies, serveable-vs-comparable with the untracked-bit distinction, and the
projection-alive assertion for the V3 pools and the V2 pair). The example is deleted; its
`too_many_lines` waiver row left the census in the same change.

### 5.5 `v2_concentrated_liquidity_parity_probe` → **leave in place, extract the assertions, then remove** (DONE — assertions landed as a pools integration test, executable removed)

Evidence: it is the evidence record for ADR-060's verdict B ("keep the interface collapse;
the representation collapse fails the zero-tolerance bar"), and that verdict is durable in
`docs/architecture/v2-cl-parity-probe.md`. But the fact it establishes is arithmetic, not
historical: 1236 comparisons, `consumed_mismatch = 0` everywhere (the range is widened past
every input so the step stays target-unreachable), zero-for-one strictly non-positive
divergences, and 0% divergence in the sub-`1e-3` ratio regime. If either simulator's
rounding changed, those facts would change and nothing currently notices — the probe is
never run by a gate.

Disposition: **leave it in its current `degenbot-pools` example home and extract** the
decision-critical invariants into that crate's integration-test coverage in task 06:
`consumed_input` equality on every comparison, the direction sign of the divergence
(zero-for-one never positive), and the zero-divergence claim for the deep-ratio regime. The
assertions that a change must not silently break become durable; the executable does not
outlive them.

Task 04's first slice does **not** move it, for the rule §2 states: no task moves a file a
later chunk deletes. Once the assertions are extracted the probe has no continuing role, so a
`degenbot-solver-lab` stanza, a re-anchoring of its three pool fixtures, and a verification run
would all be spent on a file that task 06 removes anyway — and task 06 could not tell that
deliberate move from an accident. Task 06 owns both halves: preserve the evidence, then delete.

**Done (06e):** the reconstruction — the same committed corpus (27 unique heavy-mixed V2
states + the three committed single-path fixtures, 36 synthetic fee/ratio/magnitude states),
the same degenerate-range projection, the same 8-point log grid plus recorded hotspot inputs,
both directions — is `degenbot-pools`' `tests/v2_degenerate_cl_parity.rs`, and it pins the
exact census the probe printed: 1,236 comparisons, `consumed_mismatch = 0` and zero
range-boundary hits, 225 divergent (192 negative / 33 positive; zero-for-one 618/103 all
non-positive, one-for-zero 618/122 two-sided), and the sub-`1e-3` regime 550 comparisons /
0 divergent. The overall zero-tolerance verdict stays B (refuted, never asserted cleared);
the evidence record `docs/architecture/v2-cl-parity-probe.md` names the test as its
durable owner and reproduction gate. The example, its `[[example]]` stanza, and its
`too_many_lines` waiver row left the tree in the same change (the census's reasonless
high-water dropped with the row).

### 5.6 `walkmemo_readout` → **delete as redundant** (DONE — no unique assertion, executable removed)

Evidence: every question it was built to answer is already resolved and already owned
elsewhere.

- Both gated follow-ups are closed in `docs/architecture/walkmemo-cross-block-reuse-readout.md`:
  (a) retention is immaterial at ~1% live recurrence — decided **by live capture data**, and
  (b) the per-hop fingerprint gate is kept **by bounds**.
- The durable instrument that decided (a) is **production telemetry**, not this replay:
  `degenbot-bot/src/arb_engine/walk_telemetry.rs` emits one JSON line per epoch boundary to
  `logs/walkmemo_stats.jsonl` plus the `degenbot.solver.walk_memo_*` counters/gauge, on any
  engine run that sets the two flags — "no fixture, no example, no special build".
- The memo's own semantics are pinned in-crate by `crates/foundation/degenbot-pools/src/solve/concentrated_liquidity/tests/memo.rs`
  (hit / negative / miss, cached replay without the walk, census eviction, reorg rewind,
  the `negative_entries` gauge, the mixed-family variants), and its contention floor by
  `crates/foundation/degenbot-pools/benches/walkmemo_contention.rs`.
- The readout's own numbers are disclaimed in the doc it lives beside: the fixture is
  per-path, so it measures synthetic maximum recurrence ("every composition in the set
  recurs every epoch"), and its `sims`/`probes_sims`/`hits_sims` columns read 0 on this
  corpus — "carry no signal here". Its only failure mode is a fixture/parse error.

Disposition: **delete as redundant.** Re-running it cannot answer a live question (no epoch
structure), and every assertion-shaped thing it touches has a stronger owner. Nothing is lost
from Git history that the doc's tables do not already carry.

**Done (06g):** the pre-deletion re-check confirmed the evidence: the replay had **no epoch
structure and no unique assertion** (the fixture is per-path, so it measured synthetic maximum
recurrence, and its only failure mode was a fixture/parse error — it asserted nothing about the
memo), and all four owners were live in the tree — live retention is decided by **production
walk telemetry** (`degenbot-bot/src/arb_engine/walk_telemetry.rs`: one JSON line per epoch
boundary to `logs/walkmemo_stats.jsonl` plus the `degenbot.solver.walk_memo_*` counters/gauge,
on any engine run that sets the two flags — no fixture, no example, no special build), memo
semantics by the **in-crate memo tests**
(`crates/foundation/degenbot-pools/src/solve/concentrated_liquidity/tests/memo.rs`), contention
by the existing bench (`crates/foundation/degenbot-pools/benches/walkmemo_contention.rs`), and
the historical decision tables by `docs/architecture/walkmemo-cross-block-reuse-readout.md`
(left intact). The example file, its `[[example]]` stanza, and its reasonless `too_many_lines`
waiver row left the tree in the same change (the census's reasonless high-water dropped with
the row). `entry_ab_readout` and all eight solver-lab binaries are untouched.

### 5.7 The signed-off keeper and the two roles later tasks must not misclassify

- **`stateview_feasibility_probe` — KEEP, and the header now says so.** Task 06 retired the
  historical throwaway/deletion-planned header: the file opens with the reusable
  budget-regression role (synthetic in-memory measurements over the production entry types;
  diagnostic-only, never ships; the heavy 1001-rep/large-memory matrix is a non-routine
  measurement, never executed by a routine gate), and
  `docs/architecture/stateview-feasibility.md` §2/§7 match it, with the corpus
  fixture/source paths current (`crates/foundation/degenbot-pools`). The decision stands: the
  probe is the tool that re-measures the cheap-read budget (clone 40–50 ns at p90 density,
  ≤ 23 µs p99 for a full 32-block journal rewind, and the 262k-entry superlinear clone
  anomaly a future out-of-corpus growth would have to revisit). The *disposition* is not up
  for reconsideration.
- **`entry_ab_readout` — post-unification role: a forwarding-parity harness, not an A/B.**
  After ergo `XW3H5V` the two arms share ONE solve body: the "intake" arm is
  `solve_concentrated_liquidity_piecewise`, now a forwarding convenience, and the "direct"
  arm is `solve_piecewise`. The ratio therefore measures the intake shape alone (measured
  costs: 0.9926x / 1.0068x — noise), and the harness's real job is the **byte-equality gate
  on the forwarding contract**. The historical decision tables stay in
  `docs/architecture/solve-entry-unification.md`; the forwarding role stands independently of
  those numbers — and the durable assertions already exist in-crate (three tests in
  `solve/concentrated_liquidity/tests/memo.rs`), so no new test is owed. Task 06 recorded
  that mapping (the §4.3 row names the three tests as the automatic owners); the tool is not
  deleted and the A/B is not resurrected.
- **`bench_strategies` — benchmark role, permanently.** It is the microbench whose numbers
  ADR-024's addendum quotes, and the instrument any incremental-maintenance change must beat.
  It is not a demo, not a gate, and it must never gain a pass/fail threshold. `scale_demo`
  was the demo; `bench_strategies` is the measurement. Keep them classified separately in
  every later manifest and doc edit.

**06h owner-to-test audit — capture/JIT and solver parity.** Every correctness assertion the
relocated instruments touch has a live automatic owner; the instruments themselves are
diagnostic/measurement/readout tools whose own exit status or timing is not the CI
correctness oracle — a maintained recipe may execute a readout (`test-golden-replay` does),
and the named owning tests gate correctness. The named owning tests were run green at
this revision; nothing was unowned, so nothing needed extraction or escalation.

| Instrument (diagnostic/measurement/readout tool; how it is *not* gated) | Automatic owner (what is enforced) |
| --- | --- |
| `capture_seam_probe` — `evm2-jit` compile lane only (`just check-dev-tools --engine-jit`); no recipe executes it in a gate | `degenbot-simulation/tests/jit_spine.rs` (runner-parity in `Seam` capture mode, the production configuration, plus the AOT override-key and preload legs) and `.../frame_replay_jit.rs` (compiled-path frame-replay gate) |
| `sim_mode_bench` — `just bench-sim-modes`; timing reported, never asserted | cross-mode verdict identity is the property `jit_spine.rs` pins (a compiled run returns the runner-less verdict) |
| `session_fanout_probe` / `worker_fanout_probe` / `cold_miss_fanout_probe` — external corpus / bounded smokes; measurements non-gating | fan-out parity and slot contracts pinned by `degenbot-simulation/tests/async_fanout.rs` (slot reuse, bounded scheduler, identity with indexed sequential execution) and `.../session_async.rs` |
| `concentrated_liquidity_cache_lab` — replay/readout; its divergence exit code is a local-run signal only, no recipe runs it in a gate | pools integration tests `concentrated_liquidity_cache_lab_goldens.rs` + `concentrated_liquidity_cache_patch_regressions.rs` (`capture-fixtures,cache-lab` ride every nextest lane) |
| `entry_ab_readout` — forwarding-parity readout; `just entry-ab` is not a gate | the three memo/forwarding tests in `crates/foundation/degenbot-pools/src/solve/concentrated_liquidity/tests/memo.rs` |
| `stateview_feasibility_probe` — compile lane only (`check-dev-tools default` builds it; no recipe runs it) | none owed — pure measurement; its budget numbers live in `docs/architecture/stateview-feasibility.md` §2 |

One enforcement nuance worth stating precisely: `jit_spine` and `frame_replay_jit` carry
`required-features = ["evm2-jit"]`, and no recipe enables that feature on a *narrowed*
`-p degenbot-simulation` run — they are enforced by the Linux workspace lanes, where the
facade's target-scoped `degenbot-simulation/evm2-jit` dependency turns the feature on (§6.1
consequence 4). A green narrowed package run therefore proves nothing about them; the
workspace lane is their owner.

## 6. Planned package graph

Three non-publishable tool packages, all workspace members, all excluded from
`default-members` (so unqualified `cargo build --manifest-path rust/Cargo.toml` keeps
selecting only the two product entry points):

| Package | Directory | Publish | Default member | Final targets | Forwarded features |
| --- | --- | --- | --- | --- | --- |
| `degenbot-fixture-tools` | `rust/tools/fixture-tools/` | `false` | no | lib (`src/lib.rs`) + `src/bin/<tool>/main.rs` × 9 | `sql-ledger = ["degenbot/sql-ledger"]` |
| `degenbot-solver-lab` | `rust/tools/solver-lab/` | `false` | no | lib + `src/bin/` × 8 | `capture-fixtures = ["degenbot-pools/capture-fixtures"]`, `cache-lab = ["degenbot-pools/cache-lab"]`, `hotpath = ["degenbot-pools/hotpath", "hotpath/hotpath", "hotpath/threads"]`, `envelope = ["degenbot-order-index/envelope", "dep:alloy-primitives"]` |
| `degenbot-engine-lab` | `rust/tools/engine-lab/` | `false` | no | lib + `src/bin/` × 18 | `evm2-jit = ["degenbot-simulation/evm2-jit"]`, `prometheus = ["degenbot-bot/hotpath-prometheus"]` |

The final binary counts are **fixture-tools 9, solver-lab 8, engine-lab 18 — 35 tool
binaries**, and `9 + 8 + 18 = 35`. The remaining nine original targets do not become
binaries: 3 public retainers (§3), 2 owning-crate integration tests (§4.5), and 4 removed
executables (§5.2, §5.4, §5.5, §5.6); `35 + 3 + 2 + 4 = 44`. The solver-lab and engine-lab
counts are the *final* ones: tasks `04`/`05` move eight of their eleven / eighteen of their
nineteen owned rows and leave `scale_demo`, `walkmemo_readout`,
`v2_concentrated_liquidity_parity_probe`, and `mirror_coverage_probe` where they are for task
`06`.
The `Final targets` column is the end state of the three move tasks, not a scaffold order and
not a claim about the day task `02` lands: task `02` creates the three manifests, their
workspace membership with the `default-members` exclusion above, and each `src/lib.rs`, and a
`src/bin/` entry appears only with the task (§7) that moves the row producing it.

`rust/tools/stubgen` (package `degenbot-stubgen`) stays exactly as it is: it is a
generator, not a lab, and it keeps its own membership and recipes.

### 6.1 Feature-isolation matrix — four families that must never be unified

| Feature family | Owner crate feature | Reaches a tool package only through | Must NOT appear in |
| --- | --- | --- | --- |
| SQL ledger | `degenbot/sql-ledger` → `degenbot-db/sql-ledger` | `degenbot-fixture-tools`'s `sql-ledger` forward | `degenbot-solver-lab`, `degenbot-engine-lab` |
| Pool fixture / cache lab | `degenbot-pools/capture-fixtures`, `degenbot-pools/cache-lab` | `degenbot-solver-lab`'s `capture-fixtures` / `cache-lab` forwards | `degenbot-fixture-tools`, `degenbot-engine-lab` |
| JIT | `degenbot-simulation/evm2-jit` (the `evm2-jit`, `-build`, `-runtime` deps) | `degenbot-engine-lab`'s `evm2-jit` forward | `degenbot-fixture-tools`, `degenbot-solver-lab` |
| Bot Prometheus exporter (`hotpath-prometheus`) | `degenbot-bot/hotpath-prometheus` = `["hotpath", "hotpath/hotpath-prometheus"]` (the profiler's `/metrics` listener) | `degenbot-engine-lab`'s `prometheus` forward | `degenbot-fixture-tools`, `degenbot-solver-lab`, and every lane that is not the bot exporter |

Read the `Must NOT appear in` column as a statement about **forwards**: no other tool
package declares a feature that turns that family on. It is **not** a statement about a
resolved tree's package list — see consequence 4 for the entries that appear in a lane's
tree without any tool forwarding them.

**Pool `hotpath` profiling is not the bot exporter lane.** `degenbot-solver-lab` deliberately
carries `hotpath = ["degenbot-pools/hotpath", "hotpath/hotpath", "hotpath/threads"]` for
solver-phase function profiling (`entry_ab_readout`'s functions-timing run is the consumer
that needs it). That is a *solver-local, per-invocation opt-in* over `degenbot-pools`'s
`hotpath = ["hotpath/hotpath", "hotpath/threads"]`; it never enables
`hotpath/hotpath-prometheus`, never starts the `/metrics` listener, and never depends on
`degenbot-bot`. The two lanes happen to share the `hotpath` crate, which is exactly why the
distinction has to be written down: no task may fold `prometheus` and `hotpath` into one
forward in either package, and no doc may describe solver-lab as "Prometheus-enabled" or
engine-lab as owning pool profiling.

Consequences the later tasks must honor:

1. **No catch-all package and no catch-all feature.** There is no `dev-tools` crate and no
   `full`/`all-tools` feature. A `just check-dev-tools` entry point is a *grouping of
   separately named checks* (`default`, `fixture-sql-ledger`, `solver-capture-fixtures`,
   `solver-cache-lab`, `solver-cache-telemetry`, `solver-hotpath`, `solver-envelope`,
   `engine-jit`, `engine-prometheus` — the `justfile` is the authority, §6.2), never `--all-features`:
   `--all-features` on the union graph is exactly the monolith this epic exists to avoid, and
   it makes a small solver replay link LLVM, SQL, RPC, and fixture machinery.
2. **`required-features` per binary, not per package.** A heavy binary
   (`sim_mode_bench`, `aot_prepare_executor`, `capture_seam_probe` under `evm2-jit`;
   `hotpath_prometheus_probe` under `prometheus`; the SQL-ledger recorders under
   `sql-ledger`; the corpus readers under `capture-fixtures`) carries its own
   `[[bin]] required-features`, so a default `cargo check` of the package *deliberately*
   skips it and the skip is visible in the manifest rather than implied.
3. **No tool edge into a publishable product crate.** All edges point *from* tools *to*
   product crates. The one reverse edge that existed — 14 test files in
   `degenbot-simulation/tests/` `#[path]`-including `examples/shared/*.rs` — is closed:
   those fixtures are product-owned now (`degenbot_simulation::probe_support`, §10.2),
   so the tests and this package read one implementation with no dependency added.
4. **Isolation is a checkable claim — about forwards, not about package absence.** Two
   statements must never be merged into one:

   - *Tool-owned feature-forward isolation* — what this scaffold establishes, and the only
     thing the per-lane trees prove: **no lane enables a family another tool package owns.**
     `cargo tree -p <tool> --features <lane> -e features -i <owning crate>` prints the
     `(command-line)` chain that turned the family on, so a lane that also showed a second
     tool's forward would be visible there.
   - *Resolved-tree absence* — **not claimed, and false if claimed.** A lane's tree is the
     tool plus whatever the product crates it reaches already pull in. Three inherited
     sources exist today and none of them is a tool forward: (a) the fixture SQL-ledger lane
     reaches the `degenbot` facade, whose **Linux target-scoped**
     `degenbot-simulation/evm2-jit` dependency puts the whole vendored JIT family in that
     tree (`cargo tree -p degenbot-fixture-tools --features sql-ledger -e normal` contains 20
     `evm2-jit` lines, with no fixture-tools feature anywhere in the chain); (b) `rusqlite`
     (bundled) is a **non-optional** `degenbot-db` dependency, so it appears wherever
     `degenbot-db` is reachable even though only the fixture lane enables the
     `degenbot-db/sql-ledger` *feature*; (c) the `hotpath` crate is **non-optional** in
     `degenbot-pools` (no-op stubs by default), so the solver capture lanes carry it even
     though only solver-lab's `hotpath` forward enables `hotpath/hotpath` +
     `hotpath/threads`. Removing any of the three is a product
     dependency/feature redesign, not a scaffold change.

   The sharpest form of the second check was the *default* tree: `cargo tree -p <tool> -e
   normal` printed one line — the package — for all three packages, which proved no optional
   product edge was active at default features. That held for the scaffold's three
   support-library-only packages. It no longer holds for `degenbot-fixture-tools` (§6.5) nor
   for `degenbot-solver-lab`, whose default-features `synth_corpus_gen` links pool state and
   therefore made the pools edge a plain one: each of those two packages' default tree is the
   product graph it reads, and its feature lane is the only difference between the lanes. It
   no longer holds for `degenbot-engine-lab` either: task `05`'s first slice made the corpus
   support (`bench_corpus`) DEFAULT-feature library code, and that support names the
   simulation engine, the command grammar, and evm2 — so this package's default tree is the
   simulation graph, and its `evm2-jit` forward adds the JIT spine (`evm2-jit` plus
   `degenbot-simulation/evm2-jit`) rather than changing which product graph is present. §6.5
   made that package's crates plain (non-optional) edges because its ONE default-features
   binary — the cassette recorder — links them, so its default tree is the facade graph and
   the `sql-ledger` feature is the only difference between its two lanes. Task `05`'s second
   slice grew the engine-lab default tree's RPC-facing edges too (`degenbot-rpc`, the `tower`
   transport seam, `serde_json`, `parking_lot`, `tokio`), because the default-lane cold-miss
   probe serves its snapshot through an offline `AlloyProvider`; they are plain for the same
   reason the corpus support's edges are, and no lane adds them. Record both forms in
   the `just check-dev-tools` recipe comments, mirroring the
   `docs/architecture/capability-restructure-plan.md` W3 acceptance shape, and never write a
   doc line that claims a `cargo tree` proves transitive absence.

### 6.2 The scaffold as landed — what exists, and the proof nothing moved

The ratified graph above now exists as three manifests, three support libraries,
and one check entry point. **No source file moved, and no binary exists.**

| Claim | How to check it |
| --- | --- |
| The three packages exist with the §6 names — `degenbot-fixture-tools`, `degenbot-solver-lab`, `degenbot-engine-lab` — each at `rust/tools/<dir>/` | `cargo metadata --locked --manifest-path rust/Cargo.toml --format-version 1 --no-deps` |
| Each is `publish = false`, inherits the `workspace.package` metadata and `[lints] workspace = true`, and has exactly one target: its `src/lib.rs` support library (`kind == ["lib"]`). No `[[bin]]`, no `[[example]]`, no test target **at the scaffold's revision only** — every package has since gained binaries in its own slices, and `degenbot-engine-lab` is its lib plus exactly four (`capture_seam_probe` and `sim_mode_bench` with `required-features = ["evm2-jit"]`, `cold_miss_fanout_probe` and `worker_fanout_probe` with none) after task `05`'s first through fourth slices | same metadata: `packages[].publish == []`, `packages[].targets` |
| Each is a workspace member and **not** a `default-member`: `workspace_default_members` is still exactly `degenbot` and `degenbot-cli`, so unqualified `cargo build --manifest-path rust/Cargo.toml` selects the same two product entry points it selected before | same metadata |
| The feature shells are separate forwards, one family each: `sql-ledger = ["degenbot/sql-ledger"]`; `capture-fixtures = ["degenbot-pools/capture-fixtures"]`, `cache-lab = ["degenbot-pools/cache-lab"]`, `telemetry = ["degenbot-pools/telemetry"]` (the cache lab's optional second build shape; never a bot-exporter lane), `hotpath = ["degenbot-pools/hotpath", "hotpath/hotpath", "hotpath/threads"]`, `envelope = ["degenbot-order-index/envelope", "dep:alloy-primitives"]`; `evm2-jit = ["degenbot-simulation/evm2-jit"]`, `prometheus = ["degenbot-bot/hotpath-prometheus"]` — and `degenbot-engine-lab`'s `evm2-jit` since task `05`'s first slice also names the `evm2-jit` crate itself, the capture probe's runner type | the three `Cargo.toml` files, plus one lane each in `just check-dev-tools` |
| Every product edge in the engine manifest is optional, so that package's **default** tree is the package alone — no product crate is linked — **at the scaffold's revision only**: all three packages have since gained plain crates. `degenbot-fixture-tools` since §6.5 (its default-features binary needs the facade), `degenbot-solver-lab` since task `04`'s first slice (its default-features `synth_corpus_gen` needs pool state), and `degenbot-engine-lab` since task `05`'s first slice (its DEFAULT-feature corpus support links the simulation graph), so each has a product-graph default tree | `cargo tree -p degenbot-fixture-tools -e normal`: the facade graph; `cargo tree -p degenbot-solver-lab -e normal`: the pools graph; `cargo tree -p degenbot-engine-lab -e normal`: the simulation graph |
| **Tool-owned feature-forward isolation**: no lane enables a family another tool package owns | the per-lane `cargo tree ... -e features -i <owning crate>` inspections below (§6.1 consequence 4) |
| `stubgen` and both `rust/examples/` packages are untouched, and the example surface is unchanged: 44 example targets across the same 10 packages, so §4.6's baseline audit still prints `exact-set audit: PASS` | §4.6's block, unmodified |
| `rust/Cargo.lock` grew three package entries and nothing else: no version move, no external-family change | `git diff rust/Cargo.lock` is purely additive |

**What the per-lane trees prove — and the facade JIT they inherit.** Isolation here is
tool-owned *feature-forward* isolation, **not resolved-package absence**. Every inspection
below is `cargo tree --locked --manifest-path rust/Cargo.toml …`; each proves which forwards
are active in one lane, and none of them proves that some other family's crates vanished from
the graph:

| Inspection | What it actually shows |
| --- | --- |
| `-p degenbot-fixture-tools --features sql-ledger -e normal` | the `degenbot` facade and its whole graph, **including 20 `evm2-jit` lines**: the facade's Linux target-scoped `degenbot-simulation/evm2-jit` dependency, inherited by the lane and forwarded by nothing in this package |
| `-p degenbot-fixture-tools --features sql-ledger -e features -i degenbot-db` | `degenbot-db feature "sql-ledger"` ← `degenbot feature "sql-ledger"` ← `degenbot-fixture-tools feature "sql-ledger" (command-line)` — the tool-owned forward, named |
| `-p degenbot-solver-lab --features capture-fixtures -e normal` (the `capture-fixtures,cache-lab` lane has the same shape) | `degenbot-pools` and its capture/cache dependencies, no `evm2-jit`, no `hotpath-prometheus`, and no `degenbot-db feature "sql-ledger"`; `rusqlite` and the `hotpath` no-op stub are present as **non-optional product dependencies** of `degenbot-db` / `degenbot-pools` |
| `-p degenbot-solver-lab --features hotpath -e features -i hotpath` | `hotpath feature "hotpath"`, `"threads"`, `"json"`, `"time"` — and **not** `hotpath-prometheus` |
| `-p degenbot-solver-lab --features envelope -e normal` | the pools graph plus exactly two names over the default tree: `degenbot-order-index` (the hull index the microbench measures) and the `alloy-primitives` edge its points are typed in, a crate every lane already resolves — no capture/cache, no profiler, no JIT, no SQL-ledger forward, no bot exporter (`degenbot-db`/`rusqlite` arrive as non-optional product dependencies of the pools graph, exactly as they do in the feature-free tree) |
| `-p degenbot-engine-lab --features prometheus -e features -i hotpath` | `hotpath feature "hotpath-prometheus"` enabled through `degenbot-bot/hotpath-prometheus`; no `evm2-jit` and no `degenbot-db feature "sql-ledger"` anywhere in the lane |
| `-p degenbot-engine-lab --features evm2-jit -e features -i degenbot-simulation` | `degenbot-simulation feature "evm2-jit"` ← `degenbot-engine-lab feature "evm2-jit" (command-line)`; no `hotpath-prometheus` and no `sql-ledger` in the lane |

Exactly three conclusions follow, and later tasks may rely on exactly these: (1) the
solver and engine packages' default resolution is the package alone, and
`degenbot-fixture-tools`'s default resolution is the facade graph its default-features binary
links (§6.5) — not a package-alone tree, never a lane union; (2) each lane turns on its own
family and no other tool package's family; (3) a lane's *package list* still contains the
product graph its crates already had, the facade's Linux JIT included. The fixture SQL-ledger lane therefore
records **its inherited facade JIT on Linux** as a known property of the product graph, not a
leak this scaffold can close: closing it would take a facade feature or dependency redesign,
which is outside task `02`'s authority (and `cargo tree` must not be cited as proof of
transitive absence).

**The check entry point.** `just check-dev-tools <lane>` runs **one lane per
invocation**, each its own `cargo check --locked --manifest-path rust/Cargo.toml
--all-targets` invocation:

| Lane | Package and feature selection the recipe adds |
| --- | --- |
| `default` | `cargo check -p degenbot-fixture-tools -p degenbot-solver-lab -p degenbot-engine-lab --all-targets` (this compiles fixture-tools' five default-feature programs — `record_updater_cassette`, `record_py_oracle_corpus`, `capture_path_fixture`, `concentrated_liquidity_capture_gen`, and `encode_three_hop` — and skips its four `sql-ledger` binaries) |
| `fixture-sql-ledger` | `-p degenbot-fixture-tools --features sql-ledger` |
| `solver-capture-fixtures` | `-p degenbot-solver-lab --features capture-fixtures` |
| `solver-cache-lab` | `-p degenbot-solver-lab --features capture-fixtures,cache-lab` |
| `solver-cache-telemetry` | `-p degenbot-solver-lab --features capture-fixtures,cache-lab,telemetry` (the same cache-lab binary with the pools ambient-walk telemetry on, so its collector-drain assertion and ns split are live; an additional build shape, never a `required-features` entry) |
| `solver-hotpath` | `-p degenbot-solver-lab --features hotpath` |
| `solver-envelope` | `-p degenbot-solver-lab --features envelope` |
| `engine-jit` | `-p degenbot-engine-lab --features evm2-jit` |
| `engine-prometheus` | `-p degenbot-engine-lab --features prometheus` |

A leading `--` is ignored, so the catalog's `just check-dev-tools --engine-jit`
spelling and the bare `just check-dev-tools engine-jit` form name the same lane.
An invocation with **no lane** runs `default` and prints this menu: it is the
feature-FREE check (three packages, plus the five binaries that declare no feature), never a
union of every lane, and no lane is spelled `--all-features`. A tool binary that needs a
feature joins the lane that owns it — the recipe accumulates the exact package checks as
`[[bin]]` entries land, and each such binary carries its own `required-features`, so its lane
builds it deliberately and every other lane skips it deliberately. The exceptions are the five
default-feature programs of §6.5–§6.8 and §4.2 (`record_updater_cassette`,
`record_py_oracle_corpus`, `capture_path_fixture`, `concentrated_liquidity_capture_gen`, and
`encode_three_hop`): none needs a feature, so `default` is their lane.

**What the scaffold deliberately does not contain**, so its absence reads as a
decision rather than an oversight: any `src/bin/` directory, any `[[bin]]`
stanza, any moved example source or deleted `[[example]]` stanza, a fourth
catch-all `dev-tools` package, a combined `all`/`full`/`profiler` feature, and
any product crate depending on a tool.

That paragraph describes the **scaffold** (`02`). Task `03` has since added nine `src/bin/`
entries, nine `[[bin]]` stanzas, and the nine deleted example stanzas (seven facade, one
`degenbot-rpc`, one `degenbot-executor`) — and nothing else in that list. §6.3, §6.4, §6.5,
§6.6, §6.7, and §6.8 record what landed and how each claim above was re-checked against it
(§6.5 is the slice that changed the dependency shape from optional to plain edges; §6.6
added one further plain edge, `serde`; §6.7 added no edge and no feature at all; §6.8 added
three plain edges — `degenbot-pools`, `degenbot-rpc`, and `hashbrown` — and no feature). The
ninth row, `encode_three_hop`, has no §6.x section of its own: it needs no node, no fixture
home, and no feature lane, so its record is the §4.2 row, the §8.1 transition, and the §8.2
verification route, and the move added two plain edges (`degenbot-execution`,
`degenbot-executor`).

## 6.3 The first binaries as landed — task `03`, ergo `4W2ZUK`

**Scope.** Two programs, both offline, both previously `degenbot` (facade) `[[example]]`
targets:

| Target | Was | Now |
| --- | --- | --- |
| `generate_evm_oracle_captures` | `crates/facade/degenbot/examples/generate_evm_oracle_captures.rs`, `[[example]] required-features = ["sql-ledger"]` | `rust/tools/fixture-tools/src/bin/generate_evm_oracle_captures/main.rs`, `[[bin]] required-features = ["sql-ledger"]` |
| `generate_aave_adversarial_captures` | `crates/facade/degenbot/examples/generate_aave_adversarial_captures.rs`, same stanza | `rust/tools/fixture-tools/src/bin/generate_aave_adversarial_captures/main.rs`, same declaration |

Nothing else moved **in this slice**: the §4.2 rows that had not landed yet —
`capture_path_fixture`, `generate_aave_config_windows`, `generate_aave_upgrade_windows`,
`record_py_oracle_corpus`, `record_updater_cassette`, `concentrated_liquidity_capture_gen`,
`encode_three_hop` — were still examples with their `[[example]]` stanzas at that point
(§6.4 moved two of them next, leaving five), and no other package's example surface changed.

**Manifest shape.** Cargo has no per-target dependency selection, so a `[[bin]]`-only need
has to be a feature-activated optional edge. The lane therefore reads:

```toml
# as landed in 03's first slice
sql-ledger = ["degenbot/sql-ledger", "dep:alloy", "dep:rusqlite", "dep:tempfile"]
# after 03b (§6.4) added the two span recorders' seed-manifest serialization
sql-ledger = [
    "degenbot/sql-ledger",
    "dep:alloy",
    "dep:rusqlite",
    "dep:tempfile",
    "dep:serde_json",
]
```

The `dep:` entries are the programs' own cargo (`alloy` for the scenario primitives and the
recording seam's `boxed()` chain, `rusqlite` for the throwaway harness DB each recorded chunk
runs against, `tempfile` for those throwaway homes, and — since §6.4 — `serde_json` with
`preserve_order` for the span recorders' seed manifests, whose key order is part of the
committed bytes), and they were optional for exactly the reason §6.2 gives: `cargo tree -p
degenbot-fixture-tools -e normal` printed **one line**, so the default tree was the package
alone. They are not extra forwards to a family another tool owns, and no JIT,
pool-profiling, or Prometheus forward was added — the lane's `cargo tree` still shows the
facade's **inherited** 20 `evm2-jit` lines with no fixture-tools feature in the chain (§6.1
consequence 4), which is the product graph the facade already had. Every binary landed by
§6.3/§6.4 declares `required-features = ["sql-ledger"]`, so `just check-dev-tools default`
skips them deliberately and `just check-dev-tools fixture-sql-ledger` compiles them
deliberately.

**Superseded in part by §6.5.** The `dep:` entries above are history: once a
default-features binary needed the same crates, they became plain edges and the feature kept
only the ledger, so `cargo tree -p degenbot-fixture-tools -e normal` no longer prints one
line. The `required-features` sentence also gained its one exception — the cassette recorder,
which declares none. §6.5 states the manifest as it reads today.

**Shared support (the pattern, not just the two programs).** The support library now owns:

- `paths` — the repository root is resolved ONCE, from the nearest ancestor carrying both
  [`justfile`, `rust/Cargo.toml`], and every fixture home derives from it. This closes §10.1's
  hazard for these two rows: `CARGO_MANIFEST_DIR/../../../..` named the repository root from
  `rust/crates/<role>/<crate>` and names `rust/` from `rust/tools/<package>`, so the naive
  port would have read and written a directory that exists. An unresolvable root is a returned
  error, never a fallback guess. `fixtures_home_from_args` honors `--fixtures-home <dir>` (both
  spellings), which is how the drift gates' red paths are demonstrated against a **copy**. A
  value that cannot be a directory — empty, or another flag such as `--fixtures-home --check`
  — is an argument error (`ARGUMENT ERROR: …` + exit 1 before any generation), never a home
  named `--check`: a swallowed override would run the gate against the committed corpus while
  the operator believed it pointed elsewhere.
- `ledger::goldens` — the statement ledger + canonical table dump are read through one
  function, so the wave-2 and wave-3 generators (and every later one) cannot drift in which
  tables they dump or when they take the lock.

**Behaviour preserved, and how it was proven.** The real chunk loops, the recorder writers
(`RecordingTransport` → `cassette().canonical_bytes()`), the two-pass byte-determinism gate,
the output homes (`tests/fixtures/cassettes/waveN` + `tests/fixtures/sql_goldens/waveN`), the
`--check` semantics (compare only, never write), and the argv surface (recipes keep their
names and their arguments — the two positional homes stay accepted-and-ignored exactly as
before) are unchanged. The evidence is in §4.2's rows and in the task's gate report:

| Claim | Evidence form |
| --- | --- |
| Committed bytes are unchanged | `just check-evm-captures`, `just check-aave-captures`: `--check: all committed wave-N artifacts byte-identical to regeneration`; empty `git status tests/fixtures` |
| `--check` is non-mutating and its red path is real | same binary, `--fixtures-home` a **temporary copy**: unmutated copy green, one appended byte → `DRIFT: …` + exit 1, one deleted golden → `MISSING: …` + exit 1, committed corpus untouched throughout |
| Write mode still targets the committed homes' bytes | write mode with `--fixtures-home` a temp dir: `diff -r` against both committed homes is empty for wave 2 and wave 3 |
| The offline consumers still pass | `just test-rust-nextest -p degenbot-pool-updater -E 'test(wave2)'` and `-p degenbot-aave -E 'test(wave3)'` |
| The resolver itself is pinned | the `paths` unit tests (nearest-marker root, both-marker requirement, override precedence, drift counting) via `cargo test -p degenbot-fixture-tools --lib` |

**Two example-era lint waivers did not survive the target-kind change** — worth recording
because it is a real consequence of the move, not cosmetic. The facade's `sql-ledger` examples
were never linted: the workspace clippy lane (`cargo clippy --workspace --all-targets`, no
`sql-ledger` feature) skipped them through `required-features`, so their waiver lists were
never checked against what the source uses. A `[[bin]]` in this package is linted by the
lane that builds it, so `--deny warnings` on
`cargo clippy -p degenbot-fixture-tools --all-targets --features sql-ledger` surfaced five
findings, all in `generate_aave_adversarial_captures`: three missing-backtick doc items, one
`if pinned.is_zero() { panic!(..) }` premise check (now `assert!(!pinned.is_zero(), ..)`, same
message), and an **unfulfilled** `clippy::unwrap_used` expectation (the file never unwraps;
the entry is gone). That lane is now clean under `--deny warnings`. `clippy::too_many_lines`
stays waived in both programs and its census rows moved with them
(`scripts/hooks/too-many-lines-waiver-census.txt`).

**The move asked the writer fence a new question; the answer is recorded, not assumed.** A

facade `[[example]]` is not production `src`, so ADR-070's fence

(`only_the_updater_shells_open_the_db_for_writes`, in

`rust/crates/facade/degenbot/tests/architecture_gates.rs`) never scanned these two programs

where they used to live. A `src/bin/` target of THIS package is production `src`, so the same

four calls the programs have always made became four reported breaches:

`DegenbotDb::open_for_writes` seeding the harness DB each recorded chunk's real chunk loop

applies to, and `LedgerDb::open_for_writes` opening the DB the SQL goldens are dumped from

(two each per generator).

The classification: `degenbot-fixture-tools` is the non-publishable **offline updater shell**

and it joins ADR-070 D2's owner set **by name** — D2's addendum states the boundary, and the

consequences section records this slice as recognition of behavior the relocation forced, not a

new production write path. The boundary is a rule, not a description of today's two programs:

**every write open in this package targets a throwaway harness DB under a `TempDir`**, while

the artifacts the programs produce are written by plain `fs::write` (and `--check` writes

nothing at all), so no operator DB is reachable from a tool binary. A tool program that opened

a *durable* DB needs its own D2 decision: the allowlist exempts this package by name — not "the

tools" and not "non-publishable packages", so `degenbot-solver-lab` and `degenbot-engine-lab`

stay outside the fence. And no product crate depends on this package, so the writer edge runs

tool → product (`degenbot`), never the reverse: §6.2's direction claim is untouched.

Verification route — every row is re-runnable offline:

| Claim | How to check it |

| --- | --- |

| The package is an owner by name, and nothing else joined it | `just test-rust-nextest -p degenbot -E 'test(only_the_updater_shells_open_the_db_for_writes)'` — green only because `WRITER_OWNERS` carries it; the array's inline comment IS the boundary |

| Its write opens are temporary-harness-only | `grep -rn open_for_writes rust/tools/fixture-tools/src` returns ten sites today (four when this slice landed; §6.5 added the recorder's two): the generators'/recorders' pairs reached through `seeded_db(..)`/`golden_pass` from a `TempDir::new()` path, and the recorder's two opening `dir.path().join("pool-verify.db")` / `dir.path().join("aave-run.db")` — every one under a `TempDir` |

| The programs still reproduce the committed bytes while writing only into temp homes | `just check-evm-captures` + `just check-aave-captures` (both `--check` lanes): byte-identical, and `git status --short -- tests/fixtures` empty |

| The offline consumers still pass | `just test-rust-nextest -p degenbot-pool-updater -E 'test(wave2)'` and `-p degenbot-aave -E 'test(wave3)'` |

**The `--example` → `--bin` transition, spelled out.** Every old invocation form and its
replacement is in §8.1; the two recipe bodies in the `justfile` are the only commands in the
tree that changed. A stale `--example generate_(evm_oracle|aave_adversarial)_captures` now
fails loudly (`no example target named …`), because the `[[example]]` stanza was deleted in the
same change that landed the `[[bin]]` — and only after the replacement ran green.

## 6.4 The live span recorders as landed — task `03b`, ergo `6DAEEE`

**Scope.** Two programs, both live-node recorders, both previously `degenbot` (facade)
`[[example]]` targets under `required-features = ["sql-ledger"]`:

| Target | Was | Now |
| --- | --- | --- |
| `generate_aave_upgrade_windows` | `crates/facade/degenbot/examples/generate_aave_upgrade_windows.rs`, `[[example]] required-features = ["sql-ledger"]` | `rust/tools/fixture-tools/src/bin/generate_aave_upgrade_windows/main.rs`, `[[bin]] required-features = ["sql-ledger"]` |
| `generate_aave_config_windows` | `crates/facade/degenbot/examples/generate_aave_config_windows.rs`, same stanza | `rust/tools/fixture-tools/src/bin/generate_aave_config_windows/main.rs`, same declaration |

Nothing else moved in this slice: the five §4.2 rows that remain — `capture_path_fixture`,
`record_py_oracle_corpus`, `record_updater_cassette`, `concentrated_liquidity_capture_gen`,
`encode_three_hop` — still carry their `[[example]]` stanzas, and no other package's example
surface changed. `cargo metadata` therefore reports **40** example targets (44 minus the
four landed rows) plus this package's library and its four `[[bin]]` targets.

**The two invocations, old → new.** The argv surface is unchanged; only the package, the
target kind, and the feature spelling move.

```bash
# wave-4 upgrade spans — was
cargo run --locked --manifest-path rust/Cargo.toml -p degenbot     --features degenbot/sql-ledger --example generate_aave_upgrade_windows
# now
cargo run --locked --manifest-path rust/Cargo.toml     -p degenbot-fixture-tools --features sql-ledger     --bin generate_aave_upgrade_windows

# drift gate (accepted-and-ignored positional homes kept verbatim)
cargo run --locked --manifest-path rust/Cargo.toml     -p degenbot-fixture-tools --features sql-ledger     --bin generate_aave_upgrade_windows --     --check tests/fixtures/cassettes/wave4 tests/fixtures/sql_goldens/wave4

# live endpoint override
cargo run --locked --manifest-path rust/Cargo.toml     -p degenbot-fixture-tools --features sql-ledger     --bin generate_aave_upgrade_windows -- --node <uri>
```

```bash
# wave-5 config-dispatch spans — was
cargo run --locked --manifest-path rust/Cargo.toml -p degenbot     --features degenbot/sql-ledger --example generate_aave_config_windows
# now
cargo run --locked --manifest-path rust/Cargo.toml     -p degenbot-fixture-tools --features sql-ledger     --bin generate_aave_config_windows

# drift gate (same shape; wave-5 homes)
cargo run --locked --manifest-path rust/Cargo.toml     -p degenbot-fixture-tools --features sql-ledger     --bin generate_aave_config_windows --     --check tests/fixtures/cassettes/wave5 tests/fixtures/sql_goldens/wave5

# live endpoint override
cargo run --locked --manifest-path rust/Cargo.toml     -p degenbot-fixture-tools --features sql-ledger     --bin generate_aave_config_windows -- --node <uri>
```

Neither recorder has a `just` recipe (it never did): both need a live archive node, so a
recipe would be a live-only gate. `DEGENBOT_RPC_HTTP_CHAINID_1` still supplies the default
endpoint when `--node` is absent.

**What changed, exactly — three mechanical things.** (1) The path anchor: `home()`
(`CARGO_MANIFEST_DIR/../../../..`) is gone, and `paths::fixtures_home_from_args(&args)`
resolves the corpus from the repository root's own markers **before** the first recording
pass, so a bad anchor fails before two live passes instead of after them; the
`--fixtures-home <dir>` override re-points both homes at a copy, exactly as §6.3 established
for the offline generators (§10.1's hazard, closed the same way). (2) The golden read:
`ledger::goldens(&ledger, DUMP_TABLES)` replaces each program's inline
`records()` + `ledger_golden_json` + `dump_tables_golden_json` sequence — the same two
writers through one call, so the tables a recorder dumps cannot drift from the offline
generators'. (3) The drift report: `paths::report_drift(&checks)` replaces the hand-rolled
`DRIFT:` / `MISSING:` loop, with the same lines on stderr and the same exit code.

**What did not change.** Each recorder keeps its own independent window list (the six wave-4
upgrade windows with their cursors, chunk sizes and to-blocks, and the three wave-5
config-dispatch windows including each one's `substrate_at` resolution block), the real
`run_aave_update` / `run_aave_update_on_db` chunk loops, the `RecordingTransport` writer and
its canonical bytes, the statement-ledger replay that produces the goldens, the two-pass
byte-determinism gate, chain id 1, every block pin, the `POOL_ADDRESS_PROVIDER` / `GHO_TOKEN`
addresses, the `PROVENANCE_SOURCE` string, the cassette and golden homes
(`tests/fixtures/cassettes/wave{N}`, `tests/fixtures/sql_goldens/wave{N}`) and their file
names, the seed-manifest bytes, and the argv surface — `--check <cassette_home>
<golden_home>` still regenerates and exits 1 on drift with both positional homes accepted and
ignored, and `--node <uri>` still overrides the default endpoint.

**Verification, and its honest limit.** The compile lane is
`just check-dev-tools fixture-sql-ledger`, which builds all four `sql-ledger` binaries plus
the library; `just check-dev-tools default` skips all four through their
`required-features`. The offline half of each recorder's contract is its replay suite —
`crates/integrations/degenbot-aave/tests/wave4_upgrade_span_replay.rs` and
`.../wave5_config_span_replay.rs` — which reproduces both committed SQL goldens from the
committed cassette bytes through a throwaway harness DB. The record and `--check` arms
themselves need a live endpoint, so this migration ran neither: they are
**compile-verified and unexecuted**, and the corpus is untouched (no cassette, golden, or
seed byte changed; the only file touched under `tests/fixtures` is the provenance map's
invocation line in `tests/fixtures/cassettes/wave5/CONFIG-SPAN-MAP.md`).

## 6.5 The live updater cassette recorder as landed — task `03c`, ergo `MCV3JU`

**Scope.** One program, a live-node recorder, previously a `degenbot` (facade) `[[example]]`
target with **no** `required-features` at all — the default-feature reachability this slice
had to preserve:

| Target | Was | Now |
| --- | --- | --- |
| `record_updater_cassette` | `crates/facade/degenbot/examples/record_updater_cassette.rs`, an auto-discovered `[[example]]` with no feature stanza | `rust/tools/fixture-tools/src/bin/record_updater_cassette/main.rs`, a `[[bin]]` with **no** `required-features` |

Nothing else moved in this slice: `capture_path_fixture`, `record_py_oracle_corpus`,
`concentrated_liquidity_capture_gen`, and `encode_three_hop` still carry their `[[example]]`
stanzas (§4.2), no capture generator and no other recorder was touched, and no other package's
example surface changed. `cargo metadata` therefore reports **39** example targets plus this
package's library and its **five** `[[bin]]` targets.

**The invocation, old → new.** The argv surface is unchanged; only the package and the
target kind move, and no feature flag appears:

```bash
# was
cargo run --locked --manifest-path rust/Cargo.toml -p degenbot     --example record_updater_cassette -- --check <cassette.json>...

# now (no feature lane: this binary reads no statement ledger)
cargo run --locked --manifest-path rust/Cargo.toml     -p degenbot-fixture-tools --bin record_updater_cassette     -- --check <cassette.json>...

# record arm (LIVE node): --kind pool|aave|aave-run, --verify, --node over configured-node
# resolution, --out <path>; all unchanged
cargo run --locked --manifest-path rust/Cargo.toml     -p degenbot-fixture-tools --bin record_updater_cassette     -- --kind pool --family v3 --factory 0x1F98431c8aD98523631AE4a59f267346ea31F984     --from 26102622 --to 26102626     --out tests/fixtures/cassettes/pool_update_chunk_26102622-26102626.json
```

There is no `just` recipe and none was added: the record arm needs a live archive node, so a
recipe would be a live-only gate. The `--check` arm is the offline one and is documented as
the row's verification route in §4.2.

**The manifest as it reads today — the one structural decision of this slice.** Cargo has no
per-target dependency selection, so a binary that must build at DEFAULT features forces its
crates to be plain edges. That is the whole change:

```toml
[dependencies]                       # plain edges: the default-features binary links them all
degenbot = { workspace = true }
alloy = { version = "^2.5", features = ["full"] }
rusqlite = { version = "^0.42" }
tempfile = { version = "^3.27" }
serde_json = { version = "^1.0", features = ["preserve_order"] }
tokio = { version = "^1.54", features = ["rt-multi-thread", "macros"] }   # the recorder's #[tokio::main]

[features]
default = []
sql-ledger = ["degenbot/sql-ledger"]   # the ledger, and nothing else
```

Consequences, stated so no later task inherits a false claim:

- The four SQL-ledger binaries keep `required-features = ["sql-ledger"]`; the recorders
  declare none. `just check-dev-tools default` builds the default-feature binaries (and
  skips the four), `just check-dev-tools fixture-sql-ledger` builds the whole package —
  six today, `record_py_oracle_corpus` included (§6.6). The two lanes differ by the
  ledger feature alone.
- The `dep:` entries are gone because `dep:` cannot name a non-optional dependency; the
  feature is a pure passthrough now.
- The package's default tree is **no longer the package alone** — it is the facade graph,
  exactly what the `fixture-sql-ledger` lane already resolved. That is a property of the
  default target's real dependency needs, not a new forward, and it is why §6.1's default-tree
  sentence and §6.2's table now split fixture-tools from the other two packages.
- No `required-features` was invented to "solve" the move: hiding a ledger-free recorder
  behind a feature would make `just check-dev-tools default` skip the very target that proves
  the package's default build works.
- The recorder needs no `paths` call and no `--fixtures-home`: every cassette path it touches
  is an argv path (`--out`, `--check`), so it never had `CARGO_MANIFEST_DIR` arithmetic to
  re-anchor. §10.1's hazard never applied to this row, and no fixture home moved.

**What did not change.** The three `--kind` arms (`pool` with its `PoolCreated` fetch plus the
whole-chain V3 liquidity scan, `aave` with the Pool-contract pass, `aave-run` with the real
`activate_aave_market` seed + `resolve_aave_substrate` warm boot + the real `run_aave_update`
loop over the recording transport), the `--verify` arm (the real `run_pool_update` chunk loop
with `verify_chunk = true` over a seeded temp DB, capturing the gate's Multicall3 tick/bitmap
reads), the configured-node resolution (ADR-062: `--node` / `DEGENBOT_RPC_*` / `nodes.*`,
nothing hardcoded), the ancillary `eth_chainId`/`eth_blockNumber` round trips, the
`web3_clientVersion` provenance probe on a side channel, the canonical writer
(`RecordingTransport` → `cassette().canonical_bytes()` → `fs::write`), the seed constants
(`AAVE_SEED_TOKEN_REVISION`, the V3 factory pin, the chain-1 market addresses), the file names
written, and the argv/exit-code surface (`--check` prints one verdict per file, exits 1 on any
drift, and writes nothing).

**Verification, and its honest split.** The compile lane is `just check-dev-tools default`,
which now compiles this binary. The `--check` arm is genuinely OFFLINE and was executed (no
live endpoint needed, no fixture byte written):

| Claim | How it was checked |
| --- | --- |
| Every committed cassette is byte-identical through the moved binary's writer | `cargo run -p degenbot-fixture-tools --bin record_updater_cassette -- --check tests/fixtures/cassettes/*.json` → seven `drift gate green …` lines, exit 0 |
| The red path is real and non-mutating | the same binary against a **temporary copy** of one cassette: one appended byte → `DRIFT GATE RED …: cassette is not canonical`, exit 1; a missing file → `DRIFT GATE RED …: read failed`, exit 1; `git status --short -- tests/fixtures` empty throughout |
| The offline consumers still pass | `just test-rust-nextest -p degenbot-pool-updater -E 'test(cassette_replay)'`, `-E 'test(verification_gate)'`, and `-p degenbot-aave -E 'test(cassette_replay)'` |
| The writer fence still holds | `just test-rust-nextest -p degenbot -E 'test(only_the_updater_shells_open_the_db_for_writes)'` — the package is already an owner by name, and the recorder's two write opens are temp-harness DBs (§6.3) |

The RECORD arm (`--kind pool|aave|aave-run`, live replay drift re-recording) needs a live
endpoint and was **not** executed: it is compile-verified and unexecuted, never claimed as
live-verified. ADR-070 D2's addendum needed no new decision — the recorder opens the same kind
of throwaway harness DB the four generators did, so the boundary it states already covers it;
its prose now says six programs and two no-feature default targets (§6.6 added the second, §6.7 the third).

## 6.6 The Python oracle corpus recorder as landed — task `03d`, ergo `FT3KUP`

**Scope.** One program, a live-node recorder, previously a `degenbot` (facade) auto-discovered
`[[example]]` target with **no** feature stanza — the same default-feature reachability §6.5
preserved for the cassette recorder:

| Target | Was | Now |
| --- | --- | --- |
| `record_py_oracle_corpus` | `crates/facade/degenbot/examples/record_py_oracle_corpus.rs`, an auto-discovered `[[example]]` with no feature stanza | `rust/tools/fixture-tools/src/bin/record_py_oracle_corpus/main.rs`, a `[[bin]]` with **no** `required-features` |

Nothing else moved in this slice: `capture_path_fixture`, `concentrated_liquidity_capture_gen`,
and `encode_three_hop` still carry their `[[example]]` stanzas (§4.2), no capture generator and
no other recorder was touched, and no other package's example surface changed. `cargo metadata`
therefore reported **38** example targets plus this package's library and its **six** `[[bin]]`
targets at that revision (§6.7 later moved `capture_path_fixture`, §6.8
`concentrated_liquidity_capture_gen`, and the last slice `encode_three_hop` — so today's
numbers are **35** and **nine**).

**The command, old → new.** The argv surface is unchanged apart from the path anchor below:

```bash
# was
cargo run --locked --manifest-path rust/Cargo.toml -p degenbot \
    --example record_py_oracle_corpus -- --scenario uniswap_v3_quoter

# now (no feature lane: this binary reads no statement ledger and opens no operator DB)
cargo run --locked --manifest-path rust/Cargo.toml \
    -p degenbot-fixture-tools --bin record_py_oracle_corpus \
    -- --scenario uniswap_v3_quoter

# drift gate (both arms need a node that serves the pins — there is no offline arm)
cargo run --locked --manifest-path rust/Cargo.toml \
    -p degenbot-fixture-tools --bin record_py_oracle_corpus -- --check \
    --scenario uniswap_v3_quoter --ethereum-node <archive-url>
```

There is no new `just` recipe: `just live-drift` (`pytest -m live_drift`) is the recipe that
drives this binary's `--check` per chain, and its recipe name, its marker, and its paced
per-chain shape are unchanged — only the child command inside
`tests/golden/test_live_drift.py` became the `--bin` form.

**The path anchor, and the one flag that changed.** The old source derived both homes from
`CARGO_MANIFEST_DIR`'s ancestors (`ancestors().nth(4)`, i.e. the facade's four-level depth),
which from `rust/tools/fixture-tools` names a different directory — §10.1's silent half. Both
homes are now resolved once, in `parse_args` and therefore **before** the first recording pass,
through `degenbot_fixture_tools::paths`:

- the **fixture home** — default `tests/fixtures`, derived from the repository root's own
  markers — holds the recordings (`<home>/chain_data/<chain_id>/py_oracle_<scenario>_block<N>.json`,
  exactly the pre-move path) and the pool cassettes the golden keys resolve their symbols and
  indices through (the same `chain_data/<chain_id>/` directory), so `--fixtures-home <dir>`
  re-points both at a copy and the drift gate's red path never touches the committed corpus;
- the **repository root** holds the committed golden-key sources (`tests/golden/data/...`).
  Those are inputs, and no override re-points them.

`--root` is gone: it was the old single-anchor override (undocumented in the tree — no recipe,
test, or doc passed it), and `--fixtures-home <dir>` is its replacement. Every other flag is
untouched: `--check`, repeatable `--scenario`, repeatable `--ethereum-node` / `--base-node` /
`--arbitrum-node` (an ordered per-tier endpoint pool), and `--pace-ms`. Exit codes keep their
meaning: an argv error is exit 2, a scenario failure (including `--check` drift) is exit 1.

**The dependency edge.** One: `serde` with `derive`, for the corpus document's
`#[derive(Serialize)]` whose field order IS the committed JSON byte order. `alloy`,
`serde_json`, `tokio`, and the facade were already plain edges after §6.5, so nothing else
moved; `rust/Cargo.lock` gained the one dependency line. No SQL ledger, no new forward, no
`dep:` entry, no invented `required-features`: the binary is default-built.

**What did not change.** The 21 wired scenarios (16 Ethereum + camelot + 4 Base, with
`curve_metapool_multiblock` expanding to one scenario per pinned block); every block and
address pin (`ETHEREUM_PARITY_BLOCK`, `BASE_PARITY_BLOCK`, `CAMELOT_PARITY_BLOCK`, the curve
metapool and multiblock pins, the quoter/router/`BalancerQueries` targets, the pool pins and
fees, `MIN`/`MAX_SQRT_RATIO`); the committed-golden-key derivation, including the multiblock
`blk<N>` tag validation and the per-method calldata encoders; the cassette symbol/index
resolution; the wire-answer projection — every value off the wire, a revert stored as `null`,
a non-revert node failure failing the recording, and the coverage check that every derived call
has an answer; the block-header pin check; the two-pass byte-identity gate; the record-mode
transport taxonomy and the ordered-pool rotation (`SCENARIO_RETRY_ATTEMPTS = 3`, 500 ms linear
backoff, one `[rotate]` line per event); the `--check` verdicts (byte-identity with the corpus
on disk, exit 1 on drift, nothing written); and the per-chain output paths and file names.

**Verification, and its honest limit.**

| Claim | How it was checked |
| --- | --- |
| The default-feature lane builds it (no feature may hide it) | `just check-dev-tools default` — compiles `record_py_oracle_corpus` and `record_updater_cassette`, skips the four `sql-ledger` binaries |
| The SQL lane still builds the package | `just check-dev-tools fixture-sql-ledger` — six binaries, green |
| The argv surface behaves | the built binary: unknown flag → exit 2, unknown `--scenario` → exit 1 with the wired-name list, `--fixtures-home` without a value → exit 2 |
| The corpus is untouched | `git status --short -- tests/fixtures` empty; no corpus or golden byte written or read differently |
| The recording format still agrees with the committed goldens | `tests/golden/test_oracle_replay_offline.py` (offline, no node) — the corpus decodes back to every golden int through the real `OfflineProvider` |
| The path wiring composes, with no node | `cargo test --locked --manifest-path rust/Cargo.toml -p degenbot-fixture-tools --bin record_py_oracle_corpus` — the binary's own `#[cfg(test)] mod tests`, four tests: the default `Homes::resolve(&[])` is the repository root plus `tests/fixtures`; EVERY `all_scenarios()` entry's golden file and every resolved cassette exists under the resolved homes; every scenario's `corpus_path` keeps `<repo>/tests/fixtures/chain_data/<chain_id>/py_oracle_<scenario>_block<N>.json`; and `--fixtures-home <temp dir>` moves the cassette and corpus paths beneath that home while the goldens stay under the root |
| The live recording arm | **NOT EXECUTED.** Recording and `--check` both need an archive endpoint serving the pinned state (the camelot pin and the Base tier rate-limit); the wire-answer projection, the drift verdict against the corpus bytes, and endpoint rotation are therefore compile-verified only. The path wiring the move could have broken is covered without a node by the row above |

Deriving the two homes from `paths` closes §10.1's hazard *loudly*: the resolver walks the
repository's own markers and returns an error rather than a guessed root, so a wrong anchor can
no longer name a directory that merely exists. What is not proven is a full recording against a
live endpoint — the same limit every live row in this ledger carries.

**The env-gate entry, and why it is not a new authority.** `degenbot-config`'s
`no_stray_env_reads` scan skips `examples/` as a test surface. The recorder's two reads —
`TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI` and `TEST_BASE_ARCHIVE_NODE_HTTP_URI`, the conftest names
it has always consulted when the matching node flag is absent (the Base read keeping the
keyless endpoint the aerodrome/pancakeswap tests fork) — were pre-existing behavior that
leaving `examples/` merely made visible. Exactly those two names are enumerated for exactly
this one file; no wildcard, no other key, no behavior change, and no route through the config
loader (which would hand an offline fixture tool the config crate's dependency graph for no
behavioral gain). This is the same disposition §6.4's recorders received for
`DEGENBOT_RPC_HTTP_CHAINID_1`.

**ADR-070.** The recorder opens no database at all — no operator DB, and no throwaway harness
DB either — so D2's subject is not engaged and the writer-owner rule is not broadened by its
arrival. The addendum's program count moved from five to six and says so explicitly, and the
fence's `WRITER_OWNERS` is unchanged.

## 6.7 The path-fixture recapture as landed — task `03e`, ergo `ZWV3MH`

**Scope.** One program, a live-node recapture, previously a `degenbot` (facade) auto-discovered
`[[example]]` target with **no** feature stanza — the same default-feature reachability §6.5
and §6.6 preserved for the two recorders:

| Target | Was | Now |
| --- | --- | --- |
| `capture_path_fixture` | `crates/facade/degenbot/examples/capture_path_fixture.rs`, an auto-discovered `[[example]]` with no feature stanza | `rust/tools/fixture-tools/src/bin/capture_path_fixture/main.rs`, a `[[bin]]` with **no** `required-features` |

Nothing else moved in this slice: `concentrated_liquidity_capture_gen` and `encode_three_hop`
still carry their `[[example]]` stanzas (§4.2), no other recorder or generator was touched, and
no other package's example surface changed. `cargo metadata` therefore reports **37** example
targets plus this package's library and its **seven** `[[bin]]` targets. The facade's example
surface is down to the one public retainer it keeps: `standalone_consumer`.

**The command, old → new.** The argv surface is unchanged — only the package and the target kind
moved, and there is no recipe for it (both arms need a node):

```bash
# was
cargo run --locked --manifest-path rust/Cargo.toml -p degenbot \
    --example capture_path_fixture -- --check <fixture.json>...

# now (no feature lane: this binary reads no statement ledger and opens no operator DB)
cargo run --locked --manifest-path rust/Cargo.toml \
    -p degenbot-fixture-tools --bin capture_path_fixture \
    -- [--check] [--scan lens|bitmap] [--tick-lens <addr>] \
    [--state-view <addr>] <fixture.json>...
```

Every flag is the one the example had: `--check`, `--scan lens|bitmap` (the V3 vehicle — V4
always rides the StateView), `--tick-lens <addr>` (required by `--scan lens`, refused without
it), `--state-view <addr>` (defaulting to the corpus's canonical deployment), and the
**repeatable positional fixture list**. Exit codes keep their meaning: an argv error is exit 2, a
failed read/parse/scrape/config/node is exit 1, and a clean run is exit 0.

**The path anchor: there was none to move, and inventing one would have changed the surface.**
Unlike the five facade recorders §10.1 lists, this program never derived a fixture home from
`CARGO_MANIFEST_DIR` — every fixture, for read and for rewrite, is an explicit positional argv
path, so the move cannot silently retarget a file. No `--fixtures-home` was added: it would have
changed the argv contract and, worse, changed what a *relative* positional path means. The
fixture identity and narrative are preserved because the recapture only ever rewrites the state
keys (`degenbot::investigation::capture::refresh_fixture`), which is exactly the library call
that stayed in the facade.

**What did not change.** The V2 scalar read (`getReserves`, with `blockTimestampLast` recorded
under the corpus's load-bearing `block_number` key), the V3 state read (slot0 prefix + liquidity)
and its tick scan in both vehicles (word-wise `TickLens` via `word_positions` /
`ticks_from_bitmap_word`, or the lens-free bitmap + tick-map walk, with the
`--tick-lens`/`--scan` pairing validated before any call is made), the V4 StateView read and its
`--state-view` override, every Multicall3 `aggregate3` batch, the ADR-062 configured-node
resolution (`--node` / `DEGENBOT_RPC_*` / `nodes.*`, nothing hardcoded), the authored
identity/narrative preservation, the canonical serde output (`preserve_order`, so a rewrite
keeps the family's declared key order instead of re-sorting the file), the write-versus-check
split (default arm REWRITES each named fixture with `fs::write`; `--check` computes the same
refresh, prints `ok`/`DRIFT` per file plus the drifted state-key paths, writes nothing, and exits
1 on any drift — the snapshot-marker note on `liquidity_update_block` included), and the
repeatable fixture arguments. **No fixture byte changed**, and no committed fixture was read
differently: the schema, the `capture` refresh/diff/emit functions, and the `chain_capture`
scrapers all stayed in `crates/facade/degenbot/src/investigation/`, so this slice moved an
executable and nothing else.

**The dependency edge: none.** `alloy` (its `Address`/`B256`), `serde_json` (`preserve_order`
included), `tokio` (its `#[tokio::main]`), and the facade were already plain edges after §6.5
and §6.6, so no manifest dependency, no feature, no forward, and no `Cargo.lock` line was added.
The manifest gained one `[[bin]]` stanza carrying no `required-features`, which is what puts the
binary in the `default` lane.

**Verification, and its honest limit.**

| Claim | How it was checked |
| --- | --- |
| The default-feature lane builds it (no feature may hide it) | `just check-dev-tools default` — compiles `capture_path_fixture`, `record_updater_cassette`, and `record_py_oracle_corpus`; skips the four `sql-ledger` binaries |
| The SQL lane still builds the package | `just check-dev-tools fixture-sql-ledger` — seven binaries, green |
| `cargo metadata` agrees with §4.2 | `37` example targets, `degenbot-fixture-tools` lib + `7` `[[bin]]`s, `capture_path_fixture` with no `required-features` |
| The argv surface still behaves | the built binary: no arguments → the usage line, exit 2; `--scan bogus` → exit 2; `--scan lens` without `--tick-lens` → exit 2; `--tick-lens` without `--scan lens` → exit 2; `--state-view nonsense` → exit 2; `--bogus-flag` → exit 2 |
| The library the tool calls is untouched and still passes its own tests | `just test-rust-nextest -p degenbot -E 'test(investigation)'` — the fixture/parser, capture refresh/diff/emit, and offline chain-capture (pseudo-golden `TickLens`/bitmap scrapes over recorded chain) suites |
| The fence is not engaged | `just test-rust-nextest -p degenbot -E 'test(only_the_updater_shells_open_the_db_for_writes)'` — the binary opens no DB at all |
| The committed fixtures are untouched | `git status --short -- tests/fixtures` empty |
| Both arms against a live archive node | **NOT EXECUTED.** The default rewrite arm and the `--check` arm both read the chain at each fixture's `target_block`, so drift against the committed fixtures is compile-verified only in an offline gate report. What *is* covered without a node is everything the move could have broken: the argv surface (row above), the resolved command line, and the library calls the binary makes |

**ADR-070.** The seventh program opens no database at all — no operator DB, and no harness DB
either: its only writes are the fixture JSON files, by `fs::write`. D2's subject (who may open
the operator's database for writing) is therefore not engaged, and the writer-owner rule is not
broadened. The addendum's program count moved from six to seven and says so explicitly; the
fence's `WRITER_OWNERS` is unchanged.

**Prose that moved with it.** The facade's `investigation` module docs no longer call the
recapture an "example" (`mod.rs`, `fixture.rs`), `docs/architecture/investigation-harness.md`
carries the new command beside the retired capture scripts, the fixture provenance those
`_doc` stamps record is unchanged (the stamps are committed bytes and were not edited), this
catalog's §4.2 row and §8.1 transition row say `landed`, the `too_many_lines` waiver census row
moved with the file, and the `just check-dev-tools` lane prose names the third default-feature
binary.

## 6.8 The concentrated-liquidity capture generator as landed — task `03f`

**Scope.** One program, a live-node generator, previously a `degenbot-rpc` auto-discovered
`[[example]]` target with **no** feature stanza:

| Target | Was | Now |
| --- | --- | --- |
| `concentrated_liquidity_capture_gen` | `crates/foundation/degenbot-rpc/examples/concentrated_liquidity_capture_gen.rs`, an auto-discovered `[[example]]` with no feature stanza | `rust/tools/fixture-tools/src/bin/concentrated_liquidity_capture_gen/main.rs`, a `[[bin]]` with **no** `required-features` |

Nothing else moved in this slice: `encode_three_hop` still carries its `[[example]]` stanza
(§4.2), no other recorder or generator was touched, and no other package's example surface
changed. `cargo metadata` therefore reports **36** example targets plus this package's library
and its **eight** `[[bin]]` targets — the ninth row, `encode_three_hop`, landed afterwards
without a section of its own, so today's numbers are **35** and **nine** (§4.2).
`degenbot-rpc` now has no `examples/` directory at all — this was its only example target.

**The command, old → new.** The argv surface is unchanged — one optional positional output
path, no flags:

```bash
# was
cargo run -p degenbot-rpc --example concentrated_liquidity_capture_gen -- [<out.jsonl>]

# now (no feature lane: this binary reads no statement ledger and opens no operator DB)
cargo run --locked --manifest-path rust/Cargo.toml \
    -p degenbot-fixture-tools --bin concentrated_liquidity_capture_gen -- [<out.jsonl>]
```

There is no `just` recipe for it and none was added: the only arm writes the corpus, so a
recipe would be a live-only write gate. `just test-golden-replay` remains the offline consumer
of its product, unchanged.

**The placement, and the cycle it breaks.** The old module docs explained why the generator
lived in `degenbot-rpc` rather than beside the kernel whose goldens it produces: the fetch is
RPC-backed, `degenbot-rpc` already depends on `degenbot-pools`, and a pools dev-dependency
would close the publish cycle `pools -> rpc -> pools`. A top-level tool package may depend on
**both** product crates, and no product crate depends on a tool, so
`degenbot-fixture-tools` is the home that removes the cycle instead of relocating it. Three
direct edges were missing and were transferred at their current workspace/manifest versions:

```toml
degenbot-rpc.workspace = true      # AlloyProvider, Slot0Prefix/liquidity reads, AlloyTickWordSource
degenbot-pools.workspace = true    # V3PoolState, tick-word source, v3_simulate_swap, build_int_v3_sequence, the solve entry
hashbrown.workspace = true         # the registration params' HashMap (same map type pools uses)
```

`alloy`, `serde_json` (with `preserve_order`), and `tokio` were already plain edges, so no
forward, no feature, and no `dep:` entry was invented; `rust/Cargo.lock` gained exactly those
three lines in the package's dependency list and nothing else.

**The path anchor: the one derived path, resolved instead of counted.** This program was one of
§10.1's 16 `CARGO_MANIFEST_DIR` sources — its default output was
`concat!(env!("CARGO_MANIFEST_DIR"), "/../degenbot-pools/tests/fixtures/concentrated_liquidity_capture_offline.jsonl")`,
which from `rust/tools/fixture-tools` would name a directory that does not exist (or, worse, a
sibling package that does). The default now resolves the SAME file through
`degenbot_fixture_tools::paths`: `paths::repo_relative("rust/crates/foundation/degenbot-pools/tests/fixtures/concentrated_liquidity_capture_offline.jsonl")`.
No `--fixtures-home` and no second fixture home were invented — the corpus file belongs to
`degenbot-pools`' tests, an explicit positional argv path still overrides it, and the
generator's `#[cfg(test)] mod tests` pins both the resolved path and the existence of the
committed JSONL and its `.block` sidecar without a node.

**The env-gate entry, and why it is not a new authority.** `degenbot-config`'s
`no_stray_env_reads` scan skips `examples/` as a test surface, so leaving it exposed this
program's four pre-existing reads to the production-source scan. Exactly those four names are
enumerated for exactly this one file — `DEGENBOT_CLCAP_RPC`, `DEGENBOT_CLCAP_BLOCK`,
`DEGENBOT_CLCAP_MAX_FETCHES`, `DEGENBOT_CLCAP_PATH_CAP` — with no wildcard, no other key, no
behavior change, and no route through the config loader. Their defaults (`25_826_800`, `320`,
`12`), their parse-and-fall-back shape, and the `DEGENBOT_CLCAP_RPC`-unset abort all travelled
with the source, and the inventory file
(`crates/foundation/degenbot-config/tests/degenbot_env_inventory.txt`) already carried all
four, so it did not change. Same disposition as §6.4's recorders and §6.6's corpus recorder.

**What did not change.** The eight curated mainnet V3 pool pins (address, tick spacing, fee,
token0/token1), the pinned block and its handling, the slot0-prefix + `liquidity()` reads, the
bidirectional `v3_simulate_swap` backfill with its `MAX_FETCHES` cap and the miss-path
`fetch_tick_word` mapping (including the fixed `tick_spacing: 10` descriptor key the program
has always used), each pool's production `build_int_v3_sequence(ts, fee, false)`, the shared-token
2-hop path formation and its `PATH_CAP` break, the
`derive_and_solve_concentrated_liquidity_piecewise` call with the default `SolveRuntimeConfig`,
the `hop_ranges` projection, the JSONL schema (key set and key order — `preserve_order` was
already on the edge), the `path_id = ia * 100 + ib` identity, the `.block` sidecar's `block=` /
`regen:` shape, the exit codes, and the single optional output argument. Apart from the recorded
regen command's package/bin form, the sidecar semantics are untouched; the committed sidecars
were **not** edited, and no committed JSONL, sidecar, or fixture byte changed.

**Verification, and its honest limit.**

| Claim | How it was checked |
| --- | --- |
| The default-feature lane builds it (no feature may hide it) | `just check-dev-tools default` — compiles `concentrated_liquidity_capture_gen`, `capture_path_fixture`, `record_py_oracle_corpus`, and `record_updater_cassette`; skips the four `sql-ledger` binaries |
| The SQL lane still builds the package | `just check-dev-tools fixture-sql-ledger` — eight binaries, green |
| `cargo metadata` agrees with §4.2 | `36` example targets, `degenbot-fixture-tools` lib + `8` `[[bin]]`s, `concentrated_liquidity_capture_gen` with no `required-features`, and no `degenbot-rpc` example target |
| The default output is the SAME committed file | `cargo test -p degenbot-fixture-tools --bin concentrated_liquidity_capture_gen` — the resolved `repo_root()/rust/crates/foundation/degenbot-pools/tests/fixtures/concentrated_liquidity_capture_offline.jsonl` equals the spelled-out path, the file **and** its `.block` sidecar exist, and the sidecar text names the tool package/bin with no `--example` remainder |
| The offline consumer of its product still passes | `just test-golden-replay` over both committed corpora — unchanged bytes, unchanged verdicts |
| The corpus is untouched | `git status --short -- rust/crates/foundation/degenbot-pools/tests/fixtures` empty |
| A generation run against a live archive node | **NOT EXECUTED.** There is no offline arm and no `--check`: a run rewrites the corpus and its sidecar. The curated pins, the backfill loop, the sequence construction, and the solver call are therefore compile-verified only, and the exact-wei goldens were **not** regenerated. What *is* covered without a node is everything the move could have broken — the resolved default path, the sidecar text, and the compile lanes |

**ADR-070.** The eighth program opens no database at all — no operator DB, and no harness DB
either: its only writes are the capture JSONL and its `.block` sidecar, by `File::create` and
`fs::write`. D2's subject (who may open the operator's database for writing) is therefore not
engaged, the fence's `WRITER_OWNERS` is unchanged, and the addendum's program count moved from
seven to eight with the no-DB count from two to three.

**Prose that moved with it.** The module docs carry the new home, the cycle it breaks, the
resolved default output, and the four environment keys; `degenbot-pools`' dev-dependencies note
no longer says the capture generator lives in `degenbot-rpc`; `docs/architecture/walk-telemetry-collector-sizing.md`'s
solver touch-point list names the new file; this catalog's §4.2 row, its §8.1 transition row,
§8.2's verification row, §10.1's hazard row, §10.3, and §11 say `landed`; the `too_many_lines`
waiver census row moved with the file; and the `just check-dev-tools` lane prose names the
fourth default-feature binary.

## 7. `src/bin/` migration shape

```text
rust/tools/
  README.md                     # this ledger (task 07 rewrites it as the final catalog)
  stubgen/                      # unchanged
  fixture-tools/
    Cargo.toml                  # publish = false; not a default member
    src/lib.rs                  # shared support: record/check plumbing, path resolution
    src/bin/<tool>/main.rs      # one directory per tool, so help text and assets can sit beside it
  solver-lab/
    Cargo.toml
    src/lib.rs                  # was examples/common/: capture-row parsing, harness schemas
    src/bin/<tool>/main.rs
  engine-lab/
    Cargo.toml
    src/lib.rs                  # was examples/{simulation,arbitrage}/shared/: corpus,
                                # modeled chain, dispatch, read oracle, wall ledger, reports
    src/bin/<tool>/main.rs
```

Rules for the mechanical move (tasks 03/04/05, which move only the 36 rows whose final
disposition is a tool binary):

- **Binary name = old example target name.** Every `--example <name>` becomes
  `--bin <name>` with the package changing, and nothing else about the invocation changes
  (`just` recipes keep their recipe names — §8).
- **`src/bin/<name>/main.rs`, not `src/bin/<name>.rs`**, so a tool can grow a module or an
  asset directory beside its entry point without another rename.
- **Shared helper modules become library code, not `#[path]` includes.** `examples/common/`
  (pools) and `examples/{simulation,arbitrage}/shared/` exist only because an example target
  cannot depend on its own crate's dev-only modules; as `src/lib.rs` of a tool package they
  become importable, unit-testable modules. The `#[path = "..."]` attribute blocks come out
  with them.
- **Manifest-relative writes and reads are re-anchored first** (§10.1). This is the one
  hazard that can make a "behavior-preserving move" silently write into a new directory.
- **No manifest stanza survives a move.** A package's `[[example]]` entry is deleted in the
  same task that lands the replacement `[[bin]]`, and only after the replacement command is
  green (§8.2).
- **Four example files are never moved.** `scale_demo`, `walkmemo_readout`,
  `v2_concentrated_liquidity_parity_probe`, and `mirror_coverage_probe` stay in their current
  crate `examples/` directories until task 06 has preserved what each still carries and then
  deletes them (§5.2, §5.4, §5.5, §5.6). No task 04 or 05 migration may list them, and no
  `src/bin/<name>/main.rs` is created for them.
- **Nothing lands in the tool packages that §2 counts as removed.** The final `src/bin/` counts
  are fixed: fixture-tools 9, solver-lab 8, engine-lab 18 (§6). A task that ends up with an
  extra directory there has either moved a file it should have left alone or invented a
  target the catalog does not have.

## 8. Stable recipe compatibility policy

**The `just` recipe name is the public interface; the command underneath is an
implementation detail.** Later tasks may change what a recipe runs, but not what a
contributor, a doc, or the pre-push hook types.

1. **Recipe names, default arguments, and pass-through behaviour are preserved.**
   `just test-golden-replay`, `just entry-ab`, `just bal-carry-probe`,
   `just bal-wall-ceiling-probe`, `just bench-sim-modes`, `just aot-prepare-executor`,
   `just worker-fanout-probe`, `just session-fanout-probe`, `just bal-delta-rounds-probe`,
   `just bal-delta-snapshot-probe`, `just bal-retention-probe`,
   `just bal-production-relevance-probe`, `just bal-real-read-oracle-probe`,
   `just bal-head-lifecycle-probe`, `just bal-real-coverage-capture`,
   `just bal-candidate-scaling-probe`, `just bal-dispatch-scaling-probe`,
   `just bal-live-acquisition-probe`, `just check-evm-captures`, `just check-aave-captures`,
   `just check-consumer-probes`, `just test-standalone`,
   `just build-executor-three-hop-encoder` keep their names, their optional `*ARGS`, and
   their `--release` posture where they have one.
2. **The recipe comment travels with the command.** Every recipe above carries a paragraph
   explaining what its program measures or gates; that paragraph must be updated in the same
   commit as the command, so the recipe's `just --list` help never describes a renamed
   target.
3. **Unqualified `--example` becomes qualified `--bin`.** Old:
   `cargo run --locked --manifest-path rust/Cargo.toml -p degenbot-pools --features capture-fixtures --example concentrated_liquidity_solve_replay -- <corpus>`.
   New: `cargo run --locked --manifest-path rust/Cargo.toml -p degenbot-solver-lab --features capture-fixtures --bin concentrated_liquidity_solve_replay -- <corpus>`.
   Both `--locked` and the explicit `--manifest-path` stay; the feature flag moves to the
   tool package's forward.
4. **A gate recipe keeps gating, a measurement recipe keeps measuring.** No recipe that runs
   a timing instrument may gain a pass/fail threshold (`bench-sim-modes`,
   `worker-fanout-probe`, `session-fanout-probe`, `bal-wall-ceiling-probe`,
   `bal-retention-probe`, `bal-delta-snapshot-probe`, `rayon_scale_probe` and
   `bench_strategies`). No gate recipe (`test-golden-replay`, `check-evm-captures`,
   `check-aave-captures`, `check-consumer-probes`, `test-standalone`,
   `build-executor-three-hop-encoder` + `test-executor-offline`) may be demoted to
   "run it by hand".
5. **`just check-consumer-probes` keeps running `solve_path_probe` from its crate example**
   (`degenbot-pools`'s `examples/`), because that is the consumer (ii) surface ADR-084 D1/C5
   names — it is not a tool and must not become one.
6. **Older invocations may be dropped only with a doc sweep.** When a recipe's command
   changes, the same task greps `docs/`, `README.md`, `rust/README.md`, `justfile` comments,
   `tests/`, and `scripts/` for the old `--example` form and updates every hit. Old
   `degenbot-solvers` package spellings (in `docs/architecture/stateview-feasibility.md`,
   `walkmemo-cross-block-reuse-readout.md`, `solve-entry-unification.md`,
   `v2-cl-parity-probe.md`) are corrected in the same sweep.

### 8.1 Consumer-visible transitions this ledger authorizes

| Current command (today) | Target command (after the owning task) | Task |
| --- | --- | --- |
| `cargo run -p degenbot --features degenbot/sql-ledger --example generate_evm_oracle_captures -- --check <homes>` | `cargo run -p degenbot-fixture-tools --features sql-ledger --bin generate_evm_oracle_captures -- --check <homes>` (wrapped by `just check-evm-captures`) | 03 — **landed** |
| `cargo run -p degenbot --features degenbot/sql-ledger --example generate_aave_adversarial_captures -- --check <homes>` | `cargo run -p degenbot-fixture-tools --features sql-ledger --bin generate_aave_adversarial_captures -- --check <homes>` (wrapped by `just check-aave-captures`) | 03 — **landed** |
| `cargo run -p degenbot --features degenbot/sql-ledger --example generate_aave_upgrade_windows -- <args>` | `cargo run -p degenbot-fixture-tools --features sql-ledger --bin generate_aave_upgrade_windows -- <args>` (no recipe: needs a live node) | 03b — **landed** |
| `cargo run -p degenbot --features degenbot/sql-ledger --example generate_aave_config_windows -- <args>` | `cargo run -p degenbot-fixture-tools --features sql-ledger --bin generate_aave_config_windows -- <args>` (no recipe: needs a live node) | 03b — **landed** |
| `cargo run -p degenbot --example record_updater_cassette -- <args>` | `cargo run -p degenbot-fixture-tools --bin record_updater_cassette -- <args>` (**no feature lane**: the `--check` arm is offline and needs no ledger; the record arm needs a live node, so no recipe) | 03c — **landed** |
| `cargo run -p degenbot --example record_py_oracle_corpus -- <args>` | `cargo run -p degenbot-fixture-tools --bin record_py_oracle_corpus -- <args>` (**no feature lane**; `--scenario` / `--check` / the three node flags / `--pace-ms` unchanged, `--root` replaced by the shared `--fixtures-home <dir>`; both arms need a node, so no recipe — `just live-drift` drives it per chain) | 03d — **landed** |
| `cargo run -p degenbot --example capture_path_fixture -- <args>` | `cargo run -p degenbot-fixture-tools --bin capture_path_fixture -- <args>` (**no feature lane**; `--check` / `--scan lens|bitmap` / `--tick-lens` / `--state-view` / the repeatable fixture list unchanged, no `--fixtures-home` added; both arms need a node, so no recipe) | 03e — **landed** |
| `cargo build -p degenbot-executor --example encode_three_hop` | `cargo build -p degenbot-fixture-tools --bin encode_three_hop` (wrapped by `just build-executor-three-hop-encoder`, which also stages the binary at `<target-dir>/debug/examples/encode_three_hop` — the unchanged path the Ape matrix reads; the JSON stdout contract, the failure behaviour, and the binary name are unchanged) | 03 — **landed** |
| `cargo run -p degenbot-rpc --example concentrated_liquidity_capture_gen` | `cargo run -p degenbot-fixture-tools --bin concentrated_liquidity_capture_gen -- [<out.jsonl>]` (**no feature lane**; the single optional positional output argument and the four `DEGENBOT_CLCAP_*` keys are unchanged, the default output is the same committed `degenbot-pools` corpus fixture now resolved from the repository root, and no fixture-home flag was added; the generator needs a live node, so no recipe) | 03f — **landed** |
| `cargo run -p degenbot-pools --features capture-fixtures --example concentrated_liquidity_solve_replay -- <corpus>` | `cargo run -p degenbot-solver-lab --features capture-fixtures --bin concentrated_liquidity_solve_replay -- <corpus>` (wrapped by `just test-golden-replay`; argv, output text, and the two-sided golden gate unchanged) | 04 — **landed** |
| `cargo run -p degenbot-solver-lab --features capture-fixtures --bin concentrated_liquidity_solve_replay -- <corpus>` (wrapped by `just test-golden-replay`) | `just test-rust-nextest -p degenbot-pools -E 'binary(concentrated_liquidity_golden_replay)'` — the two-sided golden gate is a `degenbot-pools` integration test (`tests/concentrated_liquidity_golden_replay.rs`: the single-block anchor and all 156 heavy rows, plus the under-shoot / excessive-over-shoot / nondeterminism negative probes), classified by the one shared evaluator `degenbot_pools::solve::golden_gate`; the lab replay keeps its recipe slot as the non-gating measurement/readout pass (argv, `DR_REPLAY_ITERS`, `DR_REPLAY_REGEN`, and the per-path readout text unchanged; the exit-code gate retires with the bridge) | 06c — **landed** |
| `cargo run -p degenbot-pools --example mixed_solve_replay -- [<capture.jsonl>]` | `cargo run -p degenbot-solver-lab --features capture-fixtures --bin mixed_solve_replay -- [<capture.jsonl>]` (**no recipe**: the golden gate is its exit code; the capture path argument and `DR_REPLAY_ITERS` unchanged) | 04 — **landed** |
| `cargo run --release -p degenbot-pools --example synth_corpus_gen` | `cargo run --release -p degenbot-solver-lab --bin synth_corpus_gen` (**no feature lane**; no argument, no `--check`, no dry-run; the two outputs are the same committed pools fixtures, now resolved from the repository root instead of `CARGO_MANIFEST_DIR`) | 04 — **landed** |
| `cargo run -p degenbot-pools --features capture-fixtures --example v2_concentrated_liquidity_parity_probe` | *(no target command)* — the executable is deleted in 06e; its decision-critical invariants are pinned by `degenbot-pools`' `tests/v2_degenerate_cl_parity.rs` (`just test-rust-nextest -p degenbot-pools -E 'binary(v2_degenerate_cl_parity)'`), evidence record `docs/architecture/v2-cl-parity-probe.md` (§5.5) | 06e — **landed** |
| `cargo run -p degenbot-pools --features capture-fixtures --example entry_ab_readout -- <corpus>` | `cargo run -p degenbot-solver-lab --features capture-fixtures --bin entry_ab_readout -- <corpus>` (wrapped by `just entry-ab`; argv, the two `DR_AB_*` controls, readout text, and the parity exit code unchanged) | 04 — **landed** |
| `cargo run -p degenbot-pools --features capture-fixtures,cache-lab --example concentrated_liquidity_cache_lab` | `cargo run -p degenbot-solver-lab --features capture-fixtures,cache-lab --bin concentrated_liquidity_cache_lab` (argv, readout text, the `DRCLAB_*` controls, and the exact-accuracy exit code unchanged; the two feature forwards move to the tool package) | 04 — **landed** |
| `cargo run -p degenbot-pools --example stateview_feasibility_probe` | `cargo run --release -p degenbot-solver-lab --bin stateview_feasibility_probe` (**no feature lane**: the matrix is synthetic and in-memory, so the binary declares no `required-features`; no argument, no fixture, no node, stdout tables and failure behaviour unchanged) | 04 — **landed** |
| `cargo run --release -p degenbot-order-index --example bench_strategies` | `cargo run --release -p degenbot-solver-lab --features envelope --bin bench_strategies` (same binary name, same hard-coded 1M/10M report, same failure behaviour; the lane now carries the order index and its `U256` edge as OPTIONAL dependencies, and the benchmark stays unexecuted by every routine gate) | 04 — **landed** |
| `cargo run -p degenbot-simulation --example sim_mode_bench --features evm2-jit` | `cargo run -p degenbot-engine-lab --features evm2-jit --bin sim_mode_bench` (wrapped by `just bench-sim-modes`) | 05d — **landed** |
| `cargo run --release -p degenbot-simulation --example session_fanout_probe` | `cargo run --release -p degenbot-engine-lab --bin session_fanout_probe` (wrapped by `just session-fanout-probe`; no feature lane — it declares no `required-features`, and its support is the product-owned `degenbot_simulation::probe_support`) | 05d3 — **landed** |
| `cargo run --release -p degenbot-simulation --example bal_carry_probe` | `cargo run --release -p degenbot-engine-lab --bin bal_carry_probe` (wrapped by `just bal-carry-probe`; no feature lane — it declares no `required-features`, and its corpus support is the product-owned `degenbot_simulation::probe_support`) | 05e1 — **landed** |
| `cargo run --release -p degenbot-simulation --example bal_delta_snapshot_probe` | `cargo run --release -p degenbot-engine-lab --bin bal_delta_snapshot_probe` (wrapped by `just bal-delta-snapshot-probe`; no feature lane — it declares no `required-features`, and its parity/chain/coverage support is the product-owned `degenbot_simulation::probe_support`) | 05e2 — **landed** |
| `cargo run --release -p degenbot-simulation --example bal_head_lifecycle_probe` | `cargo run --release -p degenbot-engine-lab --bin bal_head_lifecycle_probe` (wrapped by `just bal-head-lifecycle-probe`; no feature lane — it declares no `required-features`, and its head-leg/coverage support is the product-owned `degenbot_simulation::probe_support`) | 05e3 — **landed** |
| `cargo run --release -p degenbot-simulation --example bal_retention_probe` | `cargo run --release -p degenbot-engine-lab --bin bal_retention_probe` (wrapped by `just bal-retention-probe`; no feature lane — it declares no `required-features`, and its chain/coverage support is the product-owned `degenbot_simulation::probe_support`) | 05e4 — **landed** |
| `cargo run --release -p degenbot-simulation --example bal_delta_rounds_probe` | `cargo run --release -p degenbot-engine-lab --bin bal_delta_rounds_probe` (wrapped by `just bal-delta-rounds-probe`; no feature lane — it declares no `required-features`, and its corpus/coverage support is the product-owned `degenbot_simulation::probe_support`) | 05f1 — **landed** |
| `cargo run --release -p degenbot-simulation --example bal_production_relevance_probe` | `cargo run --release -p degenbot-engine-lab --bin bal_production_relevance_probe` (wrapped by `just bal-production-relevance-probe`; no feature lane — it declares no `required-features`, and its corpus/relevance/report support is the product-owned `degenbot_simulation::probe_support`) | 05f2 — **landed** |
| `cargo run --release -p degenbot-simulation --example bal_real_read_oracle_probe` | `cargo run --release -p degenbot-engine-lab --bin bal_real_read_oracle_probe` (wrapped by `just bal-real-read-oracle-probe`; no feature lane — it declares no `required-features`, and its read-oracle support is the product-owned `degenbot_simulation::probe_support`) | 05f3 — **landed** |
| `cargo run --release -p degenbot-simulation --example bal_candidate_scaling_probe` | `cargo run --release -p degenbot-engine-lab --bin bal_candidate_scaling_probe` (wrapped by `just bal-candidate-scaling-probe`; no feature lane — it declares no `required-features`, and its scaling-model support is the product-owned `degenbot_simulation::probe_support`) | 05g1 — **landed** |
| `cargo run --release -p degenbot-simulation --example bal_wall_ceiling_probe` | `cargo run --release -p degenbot-engine-lab --bin bal_wall_ceiling_probe` (wrapped by `just bal-wall-ceiling-probe`; no feature lane — it declares no `required-features`, and its chain/corpus/coverage/session/ledger/report support is the product-owned `degenbot_simulation::probe_support`; the default real-coverage fixture is rooted through `degenbot_engine_lab::paths::repo_root()`) | 05g2 — **landed** |
| `cargo run --release -p degenbot-arbitrage --example bal_dispatch_scaling_probe` | `cargo run --release -p degenbot-engine-lab --bin bal_dispatch_scaling_probe` (wrapped by `just bal-dispatch-scaling-probe`; no feature lane — it declares no `required-features`, and its dispatch/chain/scaling support is the product-owned `degenbot_arbitrage::probe_support`) | 05h2 — **landed** |
| `DEGENBOT_HOTPATH=1 cargo run -p degenbot-bot --features hotpath-prometheus --example hotpath_prometheus_probe` | `DEGENBOT_HOTPATH=1 cargo run -p degenbot-engine-lab --features prometheus --bin hotpath_prometheus_probe` | 05 — **landed** |
| `cargo run -p degenbot-bot --example config_smoke_boot -- <config.toml>` | *(no command)* — the config-only smoke boot is a `degenbot-bot` integration test (`tests/config_smoke_boot.rs`), run by `just test-rust-nextest -p degenbot-bot -E 'binary(config_smoke_boot)'` | 06b — **landed** |
| `cargo run -p degenbot-ingestion --example headless_boot` | *(no command)* — the headless boot is a `degenbot-ingestion` integration test (`tests/headless_boot.rs`), invoked explicitly by `just test-standalone` as its second check: `cargo test --locked --manifest-path rust/Cargo.toml -p degenbot-ingestion --test headless_boot` | 06a — **landed** |
| `cargo run -p degenbot-order-index --features envelope --example scale_demo` | *(no target command)* — the executable was deleted in 06f (`bench_strategies` is the measurement that survives, §5.2/§5.7) | 06f — **landed** |
| `cargo run -p degenbot-pools --features capture-fixtures --example walkmemo_readout` | *(no target command)* — the executable was deleted in 06g; the memo's durable evidence is the doc plus the in-crate memo tests (§5.6) | 06g — **landed** |
| `cargo run -p degenbot-simulation --example mirror_coverage_probe` | *(no target command)* — deleted in 06; the overlap table lives in `docs/architecture/sim-anchor-mirror-coverage.md` and the projection/classification regression is `degenbot-simulation`'s `tests/mirror_anchor_coverage.rs` (§5.4) | 06 — **landed** |
| `cargo run -p degenbot-pools --features capture-fixtures --example solve_path_probe` | **unchanged** — public retainer | — |
| `cargo run -p degenbot --example standalone_consumer` | **unchanged** — public retainer (`just test-standalone`) | — |
| `cargo run -p degenbot-execution-sample --example foreign_consumer` | **unchanged** — public retainer | — |

### 8.2 Per-destination verification ownership

No move is done when the file compiles; it is done when the named route below passes with the
moved binary and the old target is gone.

| Destination | Default compile lane | Feature lanes | Behavioural route (the one that actually proves the move) |
| --- | --- | --- | --- |
| `degenbot-fixture-tools` | `cargo check -p degenbot-fixture-tools --all-targets` (builds the five default-feature programs — `record_updater_cassette`, `record_py_oracle_corpus`, `capture_path_fixture`, `concentrated_liquidity_capture_gen`, and `encode_three_hop`; skips the four `sql-ledger` binaries) | `--features sql-ledger` (builds the whole package, nine binaries) | `just check-evm-captures`, `just check-aave-captures` (offline drift gates), `cargo run -p degenbot-fixture-tools --bin record_updater_cassette -- --check tests/fixtures/cassettes/*.json` (the cassette recorder's OFFLINE drift gate), `tests/golden/test_oracle_replay_offline.py` (the corpus recorder's offline recording-format gate), `just test-executor-offline`, `cargo test -p degenbot-fixture-tools --bin concentrated_liquidity_capture_gen` (the capture generator's resolved default output path and sidecar text — node-free), and the wave-2/3/4/5 plus pool/Aave cassette replay suites; the record arms of the live recorders — including `record_py_oracle_corpus`'s and `capture_path_fixture`'s default **and** `--check` arms — and the capture generator's only arm are compile only |
| `degenbot-solver-lab` | `cargo check -p degenbot-solver-lab --all-targets` (builds the lib plus `synth_corpus_gen` and `stateview_feasibility_probe`; skips the six feature-gated programs) | `--features capture-fixtures` (both replays, the entry A/B readout, and the rayon scale probe), `--features capture-fixtures,cache-lab` (the cache lab), `--features envelope` (the strategy microbench), `--features hotpath` | `just test-golden-replay` (both corpora — the owning pools tests gate; this package's replay is the non-gating measurement/readout pass), `just entry-ab`, the cache-lab golden suite, `cargo test -p degenbot-order-index`, and for `bench_strategies`: `just check-dev-tools solver-envelope` is its whole route — compile only, because its hard-coded 1M/10M matrix is a non-routine measurement that stays **unexecuted**; and for `synth_corpus_gen`: run it, require an empty `git diff` on both outputs, then `just test-rust-nextest -p degenbot-pools -E 'test(profit_envelope)'` (its concentrated-liquidity corpus's kernel consumer) — the mixed output drifts for a pre-existing reason (§5.3); and for `stateview_feasibility_probe`: `just check-dev-tools default` is its whole route — compile only, because its 1001-rep/large-memory matrix is a non-routine measurement and stays **unexecuted** |
| `degenbot-engine-lab` | `cargo check -p degenbot-engine-lab --all-targets` | `--features evm2-jit`, `--features prometheus` | `just test-rust-nextest -p degenbot-simulation` and `-p degenbot-arbitrage` (the 14 `#[path]`-coupled test files plus the parity suites must still run), `just bal-dispatch-scaling-probe`, `just bal-candidate-scaling-probe`; live/`/tmp`-corpus rows: compile only, reported as unexecuted |
| Owning-crate tests | `cargo check -p degenbot-bot --all-targets`, `-p degenbot-ingestion`, `-p degenbot-pools` | — | `just test-rust-nextest -p degenbot-bot`, `-p degenbot-ingestion`, `just test-golden-replay` (the two-sided golden gate's owning tests in `degenbot-pools`), then `just test-standalone` with its re-pointed second check |
| Removed executables (`06`) | nothing is built; `cargo metadata` must stop reporting the target | — | The §5 replacement lands *first*: the extracted `degenbot-simulation` offline test green (`just test-rust-nextest -p degenbot-simulation`) and the doc written for `mirror_coverage_probe`; §5.6's evidence re-checked for `walkmemo_readout`; §5.2's "nothing unique to preserve" re-checked for `scale_demo`. Then delete the file and re-run **only §4.6's exact-set comparison logic** — the same two-set comparison and the same `missing`/`stale`/`duplicates` diagnostics, but against the then-current `cargo metadata` and the then-current ledger, with *that revision's* expected set and count instead of §4.6's hard-coded baseline `44`. Do not rerun §4.6 itself: its `44` and its baseline set describe the pre-`06` tree by construction and must go red once a target is converted or deleted. Finally confirm `cargo metadata` reports the smaller example set with no other target renamed; the closing assert that exactly three public example targets remain is task `07`'s (§3) |

## 9. Non-goals (out of scope for this epic, not "unfinished work")

- No subcommand UX. Each tool keeps its own `argv` parsing; a unified
  `degenbot-tools <verb>` front end is separate product work.
- No production AOT or config CLI. `aot_prepare_executor` stays a documented operational
  tool; promoting it into the shipped CLI is a product decision, not a leftover.
- No fixture regeneration. Committed fixtures and goldens must stay byte-identical through
  the moves; drift is a defect to report, never something to regenerate green.
- No live revalidation claims. Rows marked live-only are verified by compilation here; their
  measurements are **not** refreshed by this epic.
- No new gating on timings. Performance instruments stay non-gating, in `--release` recipes.
- No fourth tool package: `stubgen` is not a lab, and a catch-all package is explicitly
  rejected by the epic.

## 10. Hazards found by this audit

These are why a "mechanical" move is not mechanical. Each must be closed by the task that
owns the row, and each is a verification obligation, not a note.

### 10.1 `CARGO_MANIFEST_DIR` path arithmetic breaks silently on every move

16 example sources resolve committed fixtures relative to `env!("CARGO_MANIFEST_DIR")`, and
most of them walk up four directories (`<manifest>/../../../..`) because they live at
`rust/crates/<role>/<crate>`. A tool package sits at `rust/tools/<package>` — two levels
shallower — so after the move that expression names a different directory and a recorder
would write (or a replay would read) somewhere else without failing.

| Affected source | Anchor | Task |
| --- | --- | --- |
| `degenbot-simulation`: `aot_prepare_executor` and `examples/shared/bench_corpus.rs` | `join("../../../..")` or a `format!`-built path | 05 — **partly closed by 05a, 05b, 05c, 05d, 05e2, 05e3, 05e4, 05f1, 05f2, 05g2, and 05j**: the moved `capture_seam_probe`, `cold_miss_fanout_probe`, `worker_fanout_probe`, and `sim_mode_bench` read their corpus artifacts through `degenbot_engine_lab::bench_corpus` / `paths` (marker-resolved root, no `..` arithmetic), the cold-miss and worker probes' own stub/artifact loaders included, and the original `examples/shared/bench_corpus.rs` — the last `..`-carrying corpus module — is deleted with its final consumer; `bal_delta_snapshot_probe`'s, `bal_head_lifecycle_probe`'s, `bal_retention_probe`'s, `bal_delta_rounds_probe`'s, `bal_production_relevance_probe`'s, and `bal_wall_ceiling_probe`'s real-coverage fixture roots likewise resolve through `degenbot_engine_lab::paths::repo_root()` at their new homes (05e2, 05e3, 05e4, 05f1, 05f2, 05g2), `aot_prepare_executor`'s production-artifact root resolves through the same `degenbot_engine_lab::paths::repo_root()` at its `tools/engine-lab` home (05j), and the remaining `degenbot-simulation` example row here (`mirror_coverage_probe`, which task `06` deletes) keeps its arithmetic until its own slice |
| `degenbot` (facade): `generate_aave_adversarial_captures`, `generate_aave_config_windows`, `generate_aave_upgrade_windows`, `generate_evm_oracle_captures` (×2), `record_py_oracle_corpus` | `join("../../../..")` / `join("../../../../tests/fixtures")` | 03 |
| `degenbot-rpc`: `concentrated_liquidity_capture_gen` | manifest-relative path `format!` | 03f — **closed**: the default output resolves through `degenbot_fixture_tools::paths` to the identical committed file (§6.8) |
| `degenbot-pools`: `synth_corpus_gen` (**writes** into `tests/fixtures`) | `join("tests/fixtures")` | 04 — **closed by the move**: the output home resolves through `degenbot_solver_lab::paths` (marker-resolved repository root + the pools fixture path) to the identical committed files, proven by regenerating the concentrated-liquidity corpus to an empty `git diff` (§5.3) |
| `degenbot-pools`: `v2_concentrated_liquidity_parity_probe` | manifest-relative fixtures | closed by 06e: the executable is deleted (§5.5), so the anchor leaves the tree with it and the durable test reads the same fixtures through the package's own fixture home |

Five of those six facade anchors **left the facade** in task `03`: the two offline capture
generators (§6.3), then the two live span recorders (§6.4), then the Python oracle corpus
recorder (§6.6). All five now resolve their homes through `degenbot_fixture_tools::paths`
(marker-resolved repository root, `--fixtures-home` override; the corpus recorder splits the
two — fixtures home for the corpus and its cassettes, repository root for the golden-key
inputs), and each one's offline consumer proves the resolved homes are the committed ones.

The last facade program in this population is `capture_path_fixture`, and task `03e` (§6.7)
moved it **without** re-anchoring anything: it is not one of the 16 `CARGO_MANIFEST_DIR`
sources — every fixture it reads or rewrites arrives as an explicit positional argv path — so
there was no manifest-relative arithmetic to break, and no `--fixtures-home` was invented for
it (`paths` is never called). The previous revision of this paragraph counted it among the rows
still sitting at their current paths; the correct statement is that it never belonged to this
hazard at all, whose subject is a *derived* home, and an argv path is already the explicit form
the "Required form" paragraph below prefers. The `degenbot-rpc` row is closed the other way:
task `03f` (§6.8) moved the concentrated-liquidity capture generator out of
`degenbot-rpc`'s examples, and its one derived default — the `degenbot-pools` corpus fixture —
now resolves through `degenbot_fixture_tools::paths` to the byte-identical path the old
manifest-relative `format!` named, pinned by the binary's own node-free test. The
`degenbot-pools` row that *moved* (`synth_corpus_gen`) is the one task `04`'s first slice
closed the required way: no `..` arithmetic, a single marker-resolved root in the tool library,
and a regeneration that proves the identical target. The parity probe stays at its current
path, so its manifest-relative arithmetic is still correct for the package it sits in (§5.5).

Required form: resolve the repository root once (an explicit `--repo-root`/`--out` argument
where a tool writes, or a single `ancestors()`-walking resolver in the tool library), then
derive every fixture path from it. Verification: the moved binary's read/write target must be
identical to today's — proven by a `--check` run (readers) or a regeneration with an empty
`git diff` (writers).

Task `03`'s first slice took **both** halves of that form, because a drift gate needs the
second one: `degenbot_fixture_tools::paths` resolves the root by walking ancestors for the
repository's own markers (so no program spells `..`), and `--fixtures-home <dir>` lets the
same binary run against a copied tree (so a gate can be shown going red without touching the
committed corpus). Every later row inherits the pattern instead of re-deriving it.

### 10.2 Simulation probe support is product-owned, not `#[path]`-shared

`crates/engine/degenbot-simulation/src/probe_support/` holds the thirteen fixture modules the
BAL and session probes and their behavior pins drive — the scripted chain stub, the modeled
chain, the recorded corpus readers, the read oracle, the session driver, the delta-parity
comparison, the head legs, the real-coverage accounting, the wall ledger and its report, the
relevance pairing, and the candidate-scaling model. The crate root exposes them as
`#[doc(hidden)] pub mod probe_support`, so the probes, the behavior pins and the tool package
read ONE implementation, and every edge still points from tests/tools to product.

That closes the test-support question this section used to hold open: the 14 simulation test
files, the nine BAL/session example probes, and `degenbot-batch-executor`'s startup probe
import `degenbot_simulation::probe_support::*`; no `#[path]` attribute and no
`examples/shared/` directory remains. `just test-rust-nextest -p degenbot-simulation` runs 600
tests: `recorded_corpus`'s four unit tests compile once in the library target instead of once
per path-including test target, and every other named test is unchanged.

`degenbot-arbitrage` has the same pattern in miniature: its dispatch probe support —
the scripted dispatch chain and the controlled same-head dispatch-scaling benchmark —
lives in `src/probe_support/{dispatch_probe,dispatch_scaling}.rs`, exposed as
`#[doc(hidden)] pub mod probe_support`; the dispatch unit suite, the
`bal_dispatch_scaling` pins, and the engine-lab `bal_dispatch_scaling_probe` and
`bal_live_acquisition_probe` binaries all read it through the crate export,
with no `#[path]` attribute and no `examples/shared/` directory, and the crate's
`examples/` directory is gone.

### 10.3 External corpora that are not in the tree

`bal_delta_rounds_probe`, `session_fanout_probe`, `bal_production_relevance_probe`, and
`bal_wall_ceiling_probe`
require `/tmp`-style trace files that are not committed (audited: `/tmp/fanout_trace.log` and
`/tmp/bal_broader_trace.log` absent; `logs/` is gitignored), and `bal_real_read_oracle_probe`
defaults to `logs/bal_postfix_soak_trace.log`, also untracked. These rows are
**compile-lane-only** until a trace is supplied; `bal_carry_probe`,
`bal_head_lifecycle_probe` (live legs), `bal_live_acquisition_probe`,
`capture_path_fixture`, `concentrated_liquidity_capture_gen`, and every fixture-tools RECORD
arm that drives a live node (`generate_aave_upgrade_windows`,
`generate_aave_config_windows`, `record_updater_cassette`'s record arm, and
`record_py_oracle_corpus`'s record **and** `--check` arms) need a node.
Report them as unexecuted — never as passing, and never inferred from a `cargo check`. A
program's `--check` arm is the exception only when it touches no node and no fixture write:
`record_updater_cassette`'s re-serialization arm is offline and IS executed (§6.5 does exactly
that), while `capture_path_fixture`'s `--check` reads the chain at every fixture's
`target_block` and is therefore **node-bound like its default arm** — compile-verified, never
reported as an executed check (§6.7). `concentrated_liquidity_capture_gen` has no `--check`
arm at all: its single mode writes the capture JSONL **and** its `.block` sidecar, so no
offline report may claim it ran, and the honest evidence for it is the compile lane plus the
node-free path/sidecar pins in its `#[cfg(test)] mod tests` (§6.8). Its committed product —
the corpus JSONL and sidecar — stays byte-identical precisely because the slice did not run it.

### 10.4 Stale package spellings in the decision records

The docs that authorize this epic's keepers still say `degenbot-solvers` (a package that no
longer exists) in their harness paths and reproduction commands:
`docs/architecture/stateview-feasibility.md` (spike header only: task `04`'s stateview slice
re-pointed its §2/§6 harness path and reproduction commands at the tool package),
`walkmemo-cross-block-reuse-readout.md`, `solve-entry-unification.md`,
`v2-cl-parity-probe.md`. Their *decisions* stand; their commands must resolve to the new
homes. Task 07 owns the sweep (it may not rewrite the historical narratives — only the
paths), and tasks 04/05 fix the ones their own moves invalidate.

## 11. Continuation

Nothing here requires a further design decision from this task. The earliest open questions
already belong to named tasks: task 02 owns the scaffold shape (§6), task 03 the path
re-anchoring for recorders (§10.1), task 05 the `#[path]`-coupled test resolution (§10.2),
task 06 the six dispositions of §5 and the doc extractions they imply, and task 07 the final
catalog rewrite of this file plus the stale-path sweep (§10.4).

Two constraints apply to every one of those tasks: the §2 partition (3 + 9 + 8 + 18 + 2 + 4)
is the accounting they inherit, and tasks `04` and `05` move only rows whose final
disposition is a tool binary. `scale_demo`, `walkmemo_readout`, and
`mirror_coverage_probe` stay in their crate `examples/` directories until task `06` preserves
their evidence and removes them, and the same holds for the parity probe (§5.5).

**Task `04`'s remaining scope: none.** All eight §4.3 tool rows have landed. The first slice landed the
`examples/common/` conversion and `synth_corpus_gen`'s write anchor (§4.3, §5.3, §10.1); the
second landed `concentrated_liquidity_cache_lab`, whose cache-lab lane is now the tool
package's own `capture-fixtures,cache-lab` forward, with `telemetry` an optional second build
shape of that same binary (the collector-drain assertion sits behind it and is never a
`required-features` entry); the third landed `entry_ab_readout` (`capture-fixtures`); the
fourth landed `rayon_scale_probe` (`capture-fixtures`, with the `rayon` scheduler it sweeps
and the `tracing` span its diagnostics arm enters as OPTIONAL dependencies behind that same
lane); the fifth landed `stateview_feasibility_probe` as a DEFAULT-lane binary with no
`required-features`, its synthetic in-memory M1–M6 matrices and preserved lifecycle header
unchanged, its large matrix compiled and explicitly unexecuted; and the sixth landed
`bench_strategies` (`envelope`), which is also where the §4.3 list ends — its 1M/10M legs
compiled and explicitly unexecuted, its `alloy-primitives` edge OPTIONAL behind that same
lane. `scale_demo`, `walkmemo_readout`, and the parity probe are not task `04` moves at all
(§5.2, §5.5, §5.6): `scale_demo` stays in `degenbot-order-index`'s `examples/` for task `06`.

**Task `03`'s remaining scope: none.** Seven slices have landed all nine §4.2 rows: the two
node-free generators (§6.3), the two live span recorders (§6.4), the live updater cassette
recorder (§6.5), the Python oracle corpus recorder (§6.6), the path-fixture recapture (§6.7),
the concentrated-liquidity capture generator (§6.8), and the three-hop production-calldata
helper (§4.2) — the last a build-recipe-and-staging move rather than a §6.x slice: it carries
its own `[[bin]]`, no `sql-ledger` requirement, and its §8.2 verification route (`just
build-executor-three-hop-encoder` plus `just test-executor-offline`, both offline), with the
recipe still placing the artifact at the path the Ape matrix reads. No §4.2 row remains an
example target.
`concentrated_liquidity_capture_gen` needs a live node, so its only honest verification in an
offline gate report is a compile lane plus its node-free path/sidecar test (§10.3) — which is
why the two node-free generators, whose `--check` gate can prove byte-identity, were their own
slice, the two live span recorders shipped without a live claim, the cassette recorder shipped
with an executed offline `--check` and an unexecuted record arm, the recapture (§6.7) shipped
with an exercised argv surface and both of its chain-reading arms unexecuted, and the capture
generator (§6.8) shipped with a compile lane, a node-free path/sidecar pin, and no live run at
all — its exact-wei goldens were **not** regenerated.
