# Rust development tools — catalog and migration ledger

**Status: task 01 of epic `BY3RRJ` ("Separate Rust consumer examples from development
labs"), ergo `2VLTDD`. This file is the plan; no source moved with it.**

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
| `degenbot-solver-lab` binary | `rust/tools/solver-lab/src/bin/` | 9 | `04` (X65SDH) |
| `degenbot-engine-lab` binary | `rust/tools/engine-lab/src/bin/` | 18 | `05` (SXHCWQ) |
| Owning-crate integration test (the example target itself is deleted) | `rust/crates/**/tests/` | 2 | `06` (assertion promotion) |
| **Removed executable** (no destination) | — the file is deleted once its durable evidence is preserved (§5) | 3 | `06`; tasks `04`/`05` leave the files in place |
| **Total** | | **44** | |

The six rows above are a partition of the 44 *original example targets*: every target
appears in exactly one row, and `3 + 9 + 9 + 18 + 2 + 3 = 44`. The partition is deliberately
not the same thing as *source-task ownership*, which is what the §4 sections are organised by.
Task `04` owns the eleven §4.3 rows, but only nine of them become solver binaries; task `05`
owns the nineteen §4.4 rows, but only eighteen become engine binaries. The three targets whose
final disposition is a removed executable are `scale_demo` (task `04` owns it, task `06`
removes it), `walkmemo_readout` (same), and `mirror_coverage_probe` (task `05` owns it, task
`06` extracts its assertion and removes it). §5 gives the evidence for each, and the rule the
three share is: **no task moves a file that a later chunk deletes.** Moving one first buys a
manifest stanza, a path re-anchoring, and a verification run for a file that is about to
disappear, and task `06` cannot tell a deliberate move from an accident.

Extracted assertions never change a target's assigned final disposition; they only add task
`06` test rows. `v2_concentrated_liquidity_parity_probe` is *retained*: it keeps its
`degenbot-solver-lab` binary **and** gains extracted `degenbot-pools` integration tests, so
its row stays `solver-lab (04)` in the table above (§5.5). `mirror_coverage_probe` is
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

<!-- ledger:begin -->

### 4.1 Retained public examples (3 rows)

See §3 for the full rows; repeated here only so the marked region holds all 44 rows.

| Target | Package | Destination | Current path (under `rust/`) | Purpose | Features | Needs | I/O and write behavior | Verification route |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `standalone_consumer` | `degenbot` | retain | `crates/facade/degenbot/examples/standalone_consumer.rs` | Tier-0 `cargo add degenbot` consumer proof (see §3) | — | committed `parity.db`, optional `SMOKE_RPC_URL` | none | `just test-standalone` |
| `solve_path_probe` | `degenbot-pools` | retain | `crates/foundation/degenbot-pools/examples/solve_path_probe.rs` | consumer (ii) solve-door probe (see §3) | — | none | stdout | `just check-consumer-probes` |
| `foreign_consumer` | `degenbot-execution-sample` | retain | `examples/degenbot-execution-sample/examples/foreign_consumer.rs` | foreign `ExecutionAdapter` seam sample (see §3) | — | none | stdout | `cargo run -p degenbot-execution-sample --example foreign_consumer` |

### 4.2 Fixture recorders and test-input generators — `degenbot-fixture-tools` (task 03, 9 rows)

| Target | Package | Destination | Current path (under `rust/`) | Purpose | Features | Needs | I/O and write behavior | Verification route |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `capture_path_fixture` | `degenbot` | fixture-tools (03) | `crates/facade/degenbot/examples/capture_path_fixture.rs` | Recapture a path-investigation fixture's recorded state from the chain at its `target_block` (V3 ticks word-wise via `TickLens` or bitmap, V4 through the pool StateView, all batched over Multicall3); authored identity and narrative stay verbatim | — | LIVE archive node resolved per ADR-062 (`--node`, `DEGENBOT_RPC_*`, `nodes.*`), plus the committed path fixtures | Default mode REWRITES each fixture JSON with canonical serde formatting (`fs::write`); `--check` writes nothing and exits 1 on drifted state keys | `cargo run -p degenbot-fixture-tools --bin capture_path_fixture -- --check <fixture.json>...`; the fixture schema is owned by `crates/facade/degenbot/src/investigation/fixture.rs` and documented in `docs/architecture/investigation-harness.md` |
| `generate_aave_adversarial_captures` | `degenbot` | fixture-tools (03) | `crates/facade/degenbot/examples/generate_aave_adversarial_captures.rs` | Generate the wave-3 aave-side adversarial goldens node-free (three scenarios over the ScratchEvm/frame-replay seam, real `run_aave_update` chunk loop); determinism asserted by generating twice byte-identically | `sql-ledger` | offline; committed `tests/fixtures/cassettes/wave3` + `tests/fixtures/sql_goldens/wave3` | (Re)writes both fixture homes (`create_dir_all` + `fs::write`) in default mode; `--check` regenerates into a temp home and exits 1 on any byte drift | `just check-aave-captures` (the recipe carries the `sql-ledger` passthrough); offline consumer = `crates/integrations/degenbot-aave/tests/wave3_capture_replay.rs` |
| `generate_aave_config_windows` | `degenbot` | fixture-tools (03) | `crates/facade/degenbot/examples/generate_aave_config_windows.rs` | Record the wave-5 Aave config-dispatch span cassettes from the live chain (six formerly-fenced events), each window driving the real multi-chunk loop over `RecordingTransport`, then replaying through the statement ledger | `sql-ledger` | LIVE node (`--node`, `DEGENBOT_RPC_HTTP_CHAINID_1`); committed wave-5 cassette + golden homes | (Re)writes cassettes, per-chunk statement ledger, and the canonical DB dump; every window recorded twice for byte-identity; `--check <cassette_home> <golden_home>` | Offline owner: `crates/integrations/degenbot-aave/tests/wave5_config_span_replay.rs` (plus `tests/fixtures/cassettes/wave5/CONFIG-SPAN-MAP.md` provenance). The drift gate re-records from a live node, so in CI this row is a **compile lane only** — never claim a live revalidation from it |
| `generate_aave_upgrade_windows` | `degenbot` | fixture-tools (03) | `crates/facade/degenbot/examples/generate_aave_upgrade_windows.rs` | Record the wave-4 Aave upgrade-span cassettes from the live chain; substrate from the chain's current reserve set (`resolve_aave_substrate`), same record-twice determinism contract | `sql-ledger` | LIVE node (`--node`, `DEGENBOT_RPC_HTTP_CHAINID_1`); committed wave-4 homes | Same write/`--check` shape as the wave-5 recorder | Offline owner: `crates/integrations/degenbot-aave/tests/wave4_upgrade_span_replay.rs`; **compile lane only** in CI |
| `generate_evm_oracle_captures` | `degenbot` | fixture-tools (03) | `crates/facade/degenbot/examples/generate_evm_oracle_captures.rs` | Generate the wave-2 EVM-oracle goldens node-free: the real canonical `UniswapV3Pool` harness artifact against the scratch chain, real `run_pool_update` chunk loop over a recording transport, SQL goldens replayed through the statement ledger; determinism is the acceptance gate | `sql-ledger` | offline; committed `tests/fixtures/cassettes/wave2` + `tests/fixtures/sql_goldens/wave2` | (Re)writes both homes; `--check` regenerates into a temp home and exits 1 on byte drift | `just check-evm-captures`; offline consumer = `crates/foundation/degenbot-pool-updater/tests/wave2_capture_replay.rs` |
| `record_py_oracle_corpus` | `degenbot` | fixture-tools (03) | `crates/facade/degenbot/examples/record_py_oracle_corpus.rs` | Record the offline oracle-answer corpus for the fork-bound on-chain parity tests into the per-block `OfflineProvider` JSON home; every value comes off the wire, a revert at the pin is stored as `null` | — | LIVE archive node: `TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI` / `TEST_BASE_ARCHIVE_NODE_HTTP_URI` (repeatable `--ethereum-node` / `--base-node` / `--arbitrum-node` form an ordered endpoint pool with bounded backoff rotation); committed golden-key sources under `tests/fixtures/chain_data` | Writes `tests/fixtures/chain_data/<chain_id>/py_oracle_<scenario>_block<N>.json`; every scenario recorded twice; `--check` requires byte-identity with the corpus on disk | Replay consumers: `tests/golden/test_oracle_replay_offline.py`; live drift guard `tests/golden/test_live_drift.py`; provenance `tests/fixtures/chain_data/README.md` |
| `record_updater_cassette` | `degenbot` | fixture-tools (03) | `crates/facade/degenbot/examples/record_updater_cassette.rs` | Record one updater chunk span cassette from the configured node (pool `PoolCreated` + V3 liquidity scan, or an Aave market chunk), including the `--verify` gate surface and the full `--kind aave-run` loop | — | LIVE node resolved per ADR-062; committed cassettes under `tests/fixtures/cassettes/` | Writes cassette JSON via the recorder's canonical writer; `--check <cassette.json>...` recomputes and exits 1 on drift; seeds throwaway harness DBs in temp dirs | Replay owners: `crates/foundation/degenbot-pool-updater/tests/cassette_replay_run.rs`, `.../tests/verification_gate_probes.rs`, `crates/integrations/degenbot-aave/tests/aave_cassette_replay.rs`; writer contract `crates/foundation/degenbot-rpc/src/cassette.rs`; **compile lane only** in CI |
| `concentrated_liquidity_capture_gen` | `degenbot-rpc` | fixture-tools (03) | `crates/foundation/degenbot-rpc/examples/concentrated_liquidity_capture_gen.rs` | The ONLY sanctioned producer of exact-wei goldens for the CL replay corpus: fetch curated real V3 pools at a pinned block, backfill ticks through `v3_simulate_swap`, build the production sequences, solve, and write JSONL | — | LIVE archive node `DEGENBOT_CLCAP_RPC` (+ `DEGENBOT_CLCAP_BLOCK`, `DEGENBOT_CLCAP_PATH_CAP`, `DEGENBOT_CLCAP_MAX_FETCHES`) | Writes the capture JSONL **and** a `.block` sidecar recording the pinned block + regen command (`File::create` + `fs::write`); never run by tests | Offline consumer of its product: `just test-golden-replay` (both committed corpora). Regeneration is a manual, live-only operation — record it as **not executed** in any offline gate report |
| `encode_three_hop` | `degenbot-executor` | fixture-tools (03) | `crates/foundation/degenbot-executor/examples/encode_three_hop.rs` | Encode one three-hop `PathInfo` through the production command grammar so the Ape execution matrix exercises production bytes instead of a second test encoder | — | none | stdout JSON — the machine-readable contract and the binary name must stay stable | `just build-executor-three-hop-encoder` then `just test-executor-offline` (`tests/executor/test_cmd_executor_three_hop_production_calldata.py` consumes it) |

### 4.3 Solver and order-index laboratories — `degenbot-solver-lab` (task 04 owns 11 rows: 9 move, 2 stay in place for task 06)

| Target | Package | Destination | Current path (under `rust/`) | Purpose | Features | Needs | I/O and write behavior | Verification route |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `concentrated_liquidity_cache_lab` | `degenbot-pools` | solver-lab (04) | `crates/foundation/degenbot-pools/examples/concentrated_liquidity_cache_lab.rs` | Replay a capture JSONL as deterministic pool-state transitions and solve every state through every registered cache strategy, checking byte-equality against the full-rebuild reference and printing the rebuild-cost matrix | `capture-fixtures`, `cache-lab` | committed corpus (default) or a capture path argument; `DRCLAB_MAX_PATHS`, `DRCLAB_TRANS`, `DRCLAB_MICRO`, `DRCLAB_DIGEST` | stdout report; writes nothing; exits 1 on any strategy divergence | `cargo run -p degenbot-solver-lab --features capture-fixtures,cache-lab --bin concentrated_liquidity_cache_lab -- <capture.jsonl>`; golden/regression coverage in the pools `concentrated_liquidity_cache_lab_goldens` suite; report `docs/cache-lab-report.md` |
| `concentrated_liquidity_solve_replay` | `degenbot-pools` | solver-lab (04) | `crates/foundation/degenbot-pools/examples/concentrated_liquidity_solve_replay.rs` | Offline heavy-CL replay with N-repeat stable timing; the two-sided golden gate (under-shoot past `PROFIT_EPS`, over-shoot past the few-wei tolerance, or nondeterminism across reps exits 1) | `capture-fixtures` | committed corpora `concentrated_liquidity_capture_offline.jsonl` and `heavy_concentrated_liquidity_solve_captures.jsonl[.zst]`; `DR_REPLAY_ITERS` | stdout; `DR_REPLAY_REGEN=1` rewrites stale over-shoot-only goldens — a write mode, never run by a gate | `just test-golden-replay` (both commands, unchanged in intent); task 06 promotes the durable assertion into pools integration-test coverage and keeps this binary as the lab |
| `entry_ab_readout` | `degenbot-pools` | solver-lab (04) | `crates/foundation/degenbot-pools/examples/entry_ab_readout.rs` | Post-unification readout: the retained `all-concentrated-liquidity` intake (prepared hops cloned) against `solve_piecewise` directly on real captured paths, byte-equality gated; the row census classifies the traffic mix | `capture-fixtures` (`hotpath` opt-in for the functions-timing run) | capture path argument (recipe defaults to `heavy_concentrated_liquidity_solve_captures.jsonl`); `DR_AB_ITERS`, `DR_AB_MEMO` | stdout; writes nothing except the hotpath JSON when `HOTPATH_OUTPUT_PATH` is set; exits 1 on any arm divergence | `just entry-ab [corpus]`. Its forwarding-parity contract is **already** pinned in-crate by `all_concentrated_liquidity_intake_forwards_to_the_unified_entry_byte_exact`, `key_front_doors_agree_for_concentrated_liquidity_only_compositions`, and `both_solve_entries_refuse_paths_shorter_than_two_hops` (`crates/foundation/degenbot-pools/src/solve/concentrated_liquidity/tests/memo.rs`) — see §5.7 |
| `mixed_solve_replay` | `degenbot-pools` | solver-lab (04) | `crates/foundation/degenbot-pools/examples/mixed_solve_replay.rs` | Offline heavy mixed V2+CL replay with N-repeat stable timing and the same two-sided golden gate; the bottleneck-localization note separates the envelope gate from the walk | `capture-fixtures` | `heavy_mixed_solve_captures.jsonl[.zst]`; `DR_REPLAY_ITERS` | stdout; writes nothing; exits 1 on golden drift or nondeterminism | `cargo run -p degenbot-solver-lab --features capture-fixtures --bin mixed_solve_replay -- <capture.jsonl>` (no recipe today); gate behaviour is the exit code |
| `rayon_scale_probe` | `degenbot-pools` | solver-lab (04) | `crates/foundation/degenbot-pools/examples/rayon_scale_probe.rs` | Solver-parallelism scaling: production `solve_path_with_min_profit` under controlled thread counts, measuring wall / sum-CPU / efficiency per configuration | `capture-fixtures` | committed heavy-CL corpus; `--paths`, `--serial-passes` | stdout CSV; writes nothing; a golden-lite check reproduces each path's recorded profit within `PROFIT_EPS` and excludes (never silently keeps) a flagged path | `cargo run --release -p degenbot-solver-lab --features capture-fixtures --bin rayon_scale_probe`; report `docs/rayon-parallelism-lab.md`. Measurement, not a gate |
| `stateview_feasibility_probe` | `degenbot-pools` | solver-lab (04) | `crates/foundation/degenbot-pools/examples/stateview_feasibility_probe.rs` | The signed-off **KEEP**: reusable budget-regression tool for the block-epoch data plane — tickmap clone sweep vs N, journal-replay materialization, `Arc` COW, V2 scalar costs, memory anchors, RSS | — | none — synthetic `HashMap<i32, TickInfo>` of the production entry type; reads `/proc/self/statm` for RSS (Linux only) | stdout tables; writes nothing | `cargo run --release -p degenbot-solver-lab --bin stateview_feasibility_probe`; the budget rule and the historical numbers are `docs/architecture/stateview-feasibility.md` §2/§5. Its `// Throwaway spike harness ... deleted after supervisor sign-off` header is **stale** — see §5.7 |
| `synth_corpus_gen` | `degenbot-pools` | solver-lab (04) | `crates/foundation/degenbot-pools/examples/synth_corpus_gen.rs` | Generator of the giant-liquidity family shapes observed live (walk-heavy 27817, gate-heavy 10760/15012/26030); writes the two committed synthetic corpora in the replay and mixed schemas | — | none | **Writes** `tests/fixtures/synth_giant_concentrated_liquidity.jsonl` and `tests/fixtures/synth_giant_mixed.jsonl` under `CARGO_MANIFEST_DIR`; no `--check`, no dry-run | Both files are committed and consumed by the pools `profit_envelope` kernel tests (`src/solve/profit_envelope/kernel.rs`); verification after the move: regenerate and require an empty `git diff` on both files, then run `cargo test -p degenbot-pools profit_envelope`. The `CARGO_MANIFEST_DIR` write anchor must be re-pointed (see §10.1) |
| `v2_concentrated_liquidity_parity_probe` | `degenbot-pools` | solver-lab (04) | `crates/foundation/degenbot-pools/examples/v2_concentrated_liquidity_parity_probe.rs` | ADR-060 evidence record: V2 constant-product hop vs the same state projected as a degenerate single-range CL tick sequence, compared per swap step over a real + synthetic corpus with a zero-tolerance verdict | `capture-fixtures` | `heavy_mixed_solve_captures.jsonl.zst` plus the three committed single-path pool fixtures | stdout: one `DIVERGENCE {...}` line per non-zero delta, then one `SUMMARY {...}` line; writes nothing | `cargo run -p degenbot-solver-lab --features capture-fixtures --bin v2_concentrated_liquidity_parity_probe`; decision record `docs/architecture/v2-cl-parity-probe.md`; task 06 extracts the durable assertions — see §5.5 |
| `walkmemo_readout` | `degenbot-pools` | remove in 06 — 04 leaves it in place | `crates/foundation/degenbot-pools/examples/walkmemo_readout.rs` | Offline `WalkMemo` cross-block A/B at **synthetic maximum recurrence** (every composition recurs every epoch); reports probes/hits/cache_plays/wall per epoch | `capture-fixtures` | committed heavy-CL corpus; `DR_READOUT_EPOCHS` | stdout; exits non-zero only on fixture/parse failure — it asserts nothing about the memo | **Delete as redundant** — see §5.6. Task 04 does not move it and nothing is preserved |
| `bench_strategies` | `degenbot-order-index` | solver-lab (04) | `crates/foundation/degenbot-order-index/examples/bench_strategies.rs` | Spike-S2 microbench of the order index's cost components (build, insert, non-vertex update/remove, per-block `top_k`) — the instrument the incremental-maintenance design is justified by, and the source of ADR-024's addendum numbers | `envelope` | none — synthetic points in source | stdout report; writes nothing; timing only, never asserted | `cargo run --release -p degenbot-solver-lab --features envelope --bin bench_strategies`; numbers recorded in `docs/adr/ADR-024-net-profit-order-index.md` §Addendum. **Benchmark role — not a gate, not a demo** (see §5.7) |
| `scale_demo` | `degenbot-order-index` | remove in 06 — 04 leaves it in place | `crates/foundation/degenbot-order-index/examples/scale_demo.rs` | 1M-point end-state load demo for `EnvelopeIndex`: build time, hull vertices, hot-set share, `top_k(50)` time, top-5 ids | `envelope` | none — deterministic LCG in source | stdout only; **no assertion of any kind** | **Delete as redundant** — see §5.2; task 04 does not move it |

### 4.4 Simulation, arbitrage, and bot laboratories — `degenbot-engine-lab` (task 05 owns 19 rows: 18 move, `mirror_coverage_probe` stays in place for task 06)

| Target | Package | Destination | Current path (under `rust/`) | Purpose | Features | Needs | I/O and write behavior | Verification route |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `aot_prepare_executor` | `degenbot-simulation` | engine-lab (05) | `crates/engine/degenbot-simulation/examples/aot_prepare_executor.rs` | Production executor AOT prep lane: compile the injected `cmd_executor` runtime bytecode once into the durable artifact store the process-shared JIT holder opens (`<persistence.state_dir>/aot-artifacts`) and prove it loads back as `ProgramKind::Aot` | `evm2-jit` | committed `crates/foundation/degenbot-executor/releases/v2/deployment/cmd_executor_runtime_bytecode.txt` | **Writes** the artifact store (temp sibling + rename, idempotent) under the resolved state dir | `just aot-prepare-executor`; the compile lane is `just check-dev-tools --engine-jit` (task 02 onward). Operational tool, explicitly **not** a production CLI |
| `bal_candidate_scaling_probe` | `degenbot-simulation` | engine-lab (05) | `crates/engine/degenbot-simulation/examples/bal_candidate_scaling_probe.rs` | Analytical BAL candidate-scaling report: two-corpus calibration closure, candidate-count × family × novelty matrix, W/F sensitivity — pure formula over committed corpus aggregates, no candidate executed | — | committed corpus accounting embedded in the shared model | stdout Markdown; writes nothing; `BAL_SCALING_PLANT_MISMATCH=1` fails the gate and exits non-zero (red probe) | `just bal-candidate-scaling-probe`; behavior pins in `crates/engine/degenbot-simulation/tests/bal_candidate_scaling.rs` |
| `bal_carry_probe` | `degenbot-simulation` | engine-lab (05) | `crates/engine/degenbot-simulation/examples/bal_carry_probe.rs` | Parent-linked BAL carry-forward against live reth: every canonical block of a captured range advanced through `ReadHubCache::advance_bal`, each candidate anchor's carried values compared against independent cold reads | — | LIVE reth/archive node (`DEGENBOT_RPC_HTTP_CHAINID_1` / `TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI`); a recorded `fanout-trace` file via `BAL_CARRY_TRACE` (`BAL_CARRY_FIRST`/`BAL_CARRY_LAST`) | stdout; writes nothing; any value mismatch is a hard failure (non-zero exit) | `just bal-carry-probe`; its per-anchor accounting is the reference `bal_production_relevance_probe` and `bal_wall_ceiling_probe` cross-check against. **Live-only: compile lane in CI, never claim a refreshed measurement** |
| `bal_delta_rounds_probe` | `degenbot-simulation` | engine-lab (05) | `crates/engine/degenbot-simulation/examples/bal_delta_rounds_probe.rs` | Round-coalesced delta read replay over two live-captured corpora: full-clone vs delta chain vs idle-block retention, with the re-read gap distribution the retention decision needs | — | **EXTERNAL CORPUS, absent from the tree**: `BAL_DELTA_ROUNDS_TRACES` defaults to `/tmp/fanout_trace.log` and `/tmp/bal_broader_trace.log`, both REQUIRED (absent corpus = hard failure, never an empty replay); matching real-coverage fixtures default by corpus index | stdout; measurements land in `logs/bal_delta_rounds_result.md` (gitignored, `logs/*`) | Compile lane only until the traces are supplied: `cargo check -p degenbot-engine-lab --bin bal_delta_rounds_probe`. Unexecutable offline — report as such, do not fabricate a run |
| `bal_delta_snapshot_probe` | `degenbot-simulation` | engine-lab (05) | `crates/engine/degenbot-simulation/examples/bal_delta_snapshot_probe.rs` | Full-clone vs parent-linked delta-chain snapshot measurement at realistic working-set sizes (advance latency, lookup, compaction, walk depth, RSS) with a correctness leg and a planted-divergence negative probe | — | committed `tests/fixtures/bal_real_coverage.json` via `delta_parity`; `BAL_DELTA_SIZES`, `BAL_DELTA_REPS`, `BAL_DELTA_LOOKUPS`, `BAL_DELTA_MAX_DEPTH` | stdout; `BAL_DELTA_PLANT_DIVERGENCE=1` must exit non-zero; results in `logs/bal_delta_snapshot_result.md` | `just bal-delta-snapshot-probe` (offline, moderate cost); the parity leg runs in `crates/engine/degenbot-simulation/tests/bal_delta_snapshot.rs`. Bounded default sizes only — never the largest matrix in a routine gate |
| `bal_head_lifecycle_probe` | `degenbot-simulation` | engine-lab (05) | `crates/engine/degenbot-simulation/examples/bal_head_lifecycle_probe.rs` | Canonical-head lifecycle: prepare (BAL fetch/decode) and apply stages measured separately, plus the offline waiter leg over a controllably deferred transport; every settled write compared to an independent cold read | — | LIVE node for the historical and fresh-head legs; committed capture for the waiter leg; `BAL_HEAD_*` knobs | stdout report **and** a machine-readable JSON summary when `BAL_HEAD_SUMMARY_OUT` names a path (`schema: bal-head-lifecycle/1`) | `just bal-head-lifecycle-probe` + `just bal-head-lifecycle-compare` (`scripts/bal_head_lifecycle_compare.py`, which refuses another schema identity or a leg that measured nothing); offline pins in `crates/engine/degenbot-simulation/tests/bal_head_lifecycle.rs`, `.../bal_head_lifecycle_probe.rs`, `.../bal_head_pipeline.rs`. Live legs = **compile lane only** |
| `bal_production_relevance_probe` | `degenbot-simulation` | engine-lab (05) | `crates/engine/degenbot-simulation/examples/bal_production_relevance_probe.rs` | Paired relevance replay: production-derived relevance vs the recorded-read oracle on the same captured corpus, measuring what the difference costs; four gates including exact reproduction of the live `bal_carry_probe` accounting | — | **EXTERNAL CORPUS**: `BAL_PROD_TRACE` defaults to `/tmp/bal_production_trace.log` and is REQUIRED; committed `tests/fixtures/bal_real_coverage_production.json` via `BAL_PROD_FIXTURE` | stdout; `BAL_PROD_PLANT_DIVERGENCE=1` must exit non-zero; results in `logs/bal_production_relevance_result.md`; the fixture is regenerated by a separate live capture (`BAL_WALL_FIXTURE_CAPTURE=1 just bal-real-coverage-capture`) | Compile lane only until the trace exists; offline pins in `crates/engine/degenbot-simulation/tests/bal_production_relevance.rs` |
| `bal_real_read_oracle_probe` | `degenbot-simulation` | engine-lab (05) | `crates/engine/degenbot-simulation/examples/bal_real_read_oracle_probe.rs` | Real-candidate read-set oracle replay: the post-fix soak's `fanout-trace` rows replayed through exact-anchor BAL generations, every recorded read compared against an independent cold read | — | LIVE node (`DEGENBOT_RPC_HTTP_CHAINID_1` / `TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI`); the soak trace `BAL_READ_ORACLE_TRACE` defaults to `logs/bal_postfix_soak_trace.log`, which is **gitignored and not in the tree** | stdout; `BAL_READ_ORACLE_PLANT_WEI=1` must exit non-zero | `just bal-real-read-oracle-probe`; recorded result `docs/architecture/bal-real-read-oracle-result.md`. **Live + external corpus: compile lane here, measurements not refreshed** |
| `bal_retention_probe` | `degenbot-simulation` | engine-lab (05) | `crates/engine/degenbot-simulation/examples/bal_retention_probe.rs` | Idle-block retention measurement over both committed real write-coverage captures and one synthetic long run, at thresholds 1/2/4/8 against the unbounded control; every identity verified against an independent cold read | — | committed `tests/fixtures/bal_real_coverage*.json`; `BAL_RETENTION_THRESHOLDS`, `BAL_RETENTION_SYNTH_*` | stdout; `BAL_RETENTION_PLANT_DIVERGENCE=1` must exit non-zero; results in `logs/bal_retention_result.md` | `just bal-retention-probe` (offline); the parity pins live in `crates/engine/degenbot-simulation/tests/` and the recipe is the only route to the report |
| `bal_wall_ceiling_probe` | `degenbot-simulation` | engine-lab (05) | `crates/engine/degenbot-simulation/examples/bal_wall_ceiling_probe.rs` | Deterministic wall-clock ceiling benchmark of BAL carry-forward: the recorded read walks under three cache policies, candidate wall attributed to six reconciled buckets, the eliminable storage-wait ceiling per price point, in both write-coverage modes | — | **EXTERNAL CORPUS**: `BAL_WALL_TRACE` defaults to `/tmp/fanout_trace.log`; committed `tests/fixtures/bal_real_coverage.json` / `..._broader.json` via `BAL_WALL_FIXTURE` | stdout + `logs/` results; `BAL_WALL_FIXTURE_CAPTURE=1` is the live fixture-write mode (its own recipe `just bal-real-coverage-capture`); `BAL_WALL_PLANT_DIVERGENCE=1` must exit non-zero | `just bal-wall-ceiling-probe`; instrumentation pins in `crates/engine/degenbot-simulation/tests/bal_wall_ceiling.rs`. Measurement — timing is reported, never asserted |
| `capture_seam_probe` | `degenbot-simulation` | engine-lab (05) | `crates/engine/degenbot-simulation/examples/capture_seam_probe.rs` | JIT-composition probe: does the production capture (access-list slots + swap events) survive OFF the `Inspector` hook, i.e. can compiled execution carry it? Three configurations A/B/C with a set-equality PASS bar | `evm2-jit` | offline; the corpus comes from the shared bench corpus module | stdout; writes nothing; prints PASS/FAIL | `cargo run --release -p degenbot-engine-lab --features evm2-jit --bin capture_seam_probe`; the durable JIT verdict parity it feeds lives in the simulation test suite (`crates/engine/degenbot-simulation/tests/`) |
| `cold_miss_fanout_probe` | `degenbot-simulation` | engine-lab (05) | `crates/engine/degenbot-simulation/examples/cold_miss_fanout_probe.rs` | Cold-miss cost and coroutine fan-out: ten legs over a deterministic offline RPC stand-in charging per modeled round trip; the decision instrument for the evm2 coroutine-fan-out chunk | — | offline — snapshot-derived state, `COLD_MISS_LATENCY_US`, `COLD_MISS_FRAMES` | stdout; writes nothing; timing reported, never asserted | `cargo run --release -p degenbot-engine-lab --bin cold_miss_fanout_probe` (no recipe); compile lane + its own leg-10 verdict-identity check |
| `mirror_coverage_probe` | `degenbot-simulation` | remove in 06 — 05 leaves it in place | `crates/engine/degenbot-simulation/examples/mirror_coverage_probe.rs` | BotState-mirror overlap probe: of a real mainnet candidate's cold SLOAD set, how many keys the sim anchor already carries, and whether the carried word is *serveable* or only *comparable* | — | offline — two frames from the committed `degenbot-strategy/tests/fixtures/frame_replay_capture.jsonl`, prestate-tracer slot sets pinned in source | stdout classification tables; **asserts** that a registered pool projects its scalar rows into the anchor (an empty projection is a build defect) | See §5.4: task 05 leaves it where it is; task 06 extracts a durable offline regression test in `degenbot-simulation`, records the overlap table in a durable doc, and only then removes the executable |
| `session_fanout_probe` | `degenbot-simulation` | engine-lab (05) | `crates/engine/degenbot-simulation/examples/session_fanout_probe.rs` | Session-scheduler benchmark: the recorded fan-out read walks driven through the committed `ReadHub` under the committed W/F bound, walled against serial / hybrid / fibers / per-lane reads under one latency model | — | **EXTERNAL CORPUS**: `SESSION_FANOUT_TRACE` defaults to `/tmp/fanout_trace.log` and its absence is a hard failure | stdout; `SESSION_FANOUT_PLANT_DIVERGENCE=1` must exit non-zero (per-candidate read-digest gate) | `just session-fanout-probe`; the real-frame scheduler behaviour it approximates is owned by `crates/engine/degenbot-simulation/tests/async_fanout.rs` and `.../session_async.rs`. Compile lane only without the trace |
| `sim_mode_bench` | `degenbot-simulation` | engine-lab (05) | `crates/engine/degenbot-simulation/examples/sim_mode_bench.rs` | ADR-089 phase-6 offline engine benchmark: one corpus, engine modes (interp / jit-block / jit-pool / aot / replay / replay-jit), first-run vs steady-state latency, JIT compile telemetry, cross-mode verdict identity | `evm2-jit` | offline — committed fixture spine (real `cmd_executor` creation artifact + tier-3 stubs); `SIM_BENCH_ITERS` | stdout; writes nothing (the AOT mode's scratch store is a temp path) | `just bench-sim-modes [args]` (release); the decision instrument for the AOT/worker-pool tasks — measurement only |
| `worker_fanout_probe` | `degenbot-simulation` | engine-lab (05) | `crates/engine/degenbot-simulation/examples/worker_fanout_probe.rs` | Per-worker EVM fan-out probe (ADR-089 concurrency trio item 4): serial shared `&mut evm` vs per-worker threads vs per-candidate fibers vs warm fibers, cross-leg verdict identity gated | — | offline — shape-matched corpus built in code; `WORKER_FANOUT_SHAPE`, `WORKER_FANOUT_CANDIDATES`, `WORKER_FANOUT_WORKERS`, `COLD_MISS_LATENCY_US`; `WORKER_FANOUT_TRACE` optionally points at a recorded trace | stdout; `WORKER_FANOUT_PLANT_DIVERGENCE=1` must exit non-zero | `just worker-fanout-probe`; results' provenance `logs/bal_matched_parity_matrix.md`. Measurement, non-gating |
| `bal_dispatch_scaling_probe` | `degenbot-arbitrage` | engine-lab (05) | `crates/engine/degenbot-arbitrage/examples/bal_dispatch_scaling_probe.rs` | Controlled BAL dispatch-scaling probe: the production dispatch entry point over one scripted head, one candidate list, one deployment and one provider state, from one candidate to a burst fan-out, with only the storage representation varying | — | offline — scripted chain/market in source; `BAL_DISPATCH_SCALING_COUNTS`, `BAL_DISPATCH_SCALING_REPS` | stdout Markdown report; never asserts timing; **fails closed (exit 1)** on a broken fixture (wrong lane, cold control fetching a BAL, missing prefetch identity, missing read key, cross-lane verdict difference) | `just bal-dispatch-scaling-probe`; the durable scripted value-sensitive parity matrix is `crates/engine/degenbot-arbitrage/tests/bal_dispatch_scaling.rs`; regeneration notes `logs/bal_dispatch_scaling_result.md`, result `docs/architecture/bal-matched-parity-result.md` |
| `bal_live_acquisition_probe` | `degenbot-arbitrage` | engine-lab (05) | `crates/engine/degenbot-arbitrage/examples/bal_live_acquisition_probe.rs` | Live reth BAL acquisition: what it costs to acquire ONE fixed historical block's EIP-7928 access list in both official representations, plus per-fan-out packet prices at that anchor | — | LIVE archive node (`TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI` / `DEGENBOT_RPC_HTTP_CHAINID_1`); `BAL_ACQ_ANCHOR`, `BAL_ACQ_REPS`, `BAL_ACQ_CORPUS_COUNTS` | stdout; writes nothing; seed-parity disagreement and settled-value mismatch are hard failures | `just bal-live-acquisition-probe`; priced conclusions in `docs/architecture/bal-matched-parity-result.md`. **Live-only: compile lane in CI** |
| `hotpath_prometheus_probe` | `degenbot-bot` | engine-lab (05) | `crates/engine/degenbot-bot/examples/hotpath_prometheus_probe.rs` | Smallest end-to-end proof that a guard-enabled build serves profiler metrics on `:6772`: installs the typed config from the environment, constructs the same guard the pump holds, scrapes `/metrics` and asserts the always-present process families plus a `measure_block!`-sampled family | `hotpath-prometheus` *(runtime-derived: the example declares no `required-features` in metadata today — task 05 must declare it on the new `[[bin]]`, see §4.7)* | offline but binds a local TCP port (`HOTPATH_PROMETHEUS_PORT`, default 6772); `DEGENBOT_HOTPATH=1` required | stdout; writes nothing; non-zero exit on any wiring breakage | `DEGENBOT_HOTPATH=1 cargo run -p degenbot-engine-lab --features prometheus --bin hotpath_prometheus_probe`. No recipe and no test names it today — its CI obligation is the **Prometheus compile lane** (`just check-dev-tools --engine-prometheus`), documented in `docs/grafana/HOTPATH.md` |

### 4.5 Assertion-only smoke → owning-crate integration tests (task 06, 2 rows)

| Target | Package | Destination | Current path (under `rust/`) | Purpose | Features | Needs | I/O and write behavior | Verification route |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `config_smoke_boot` | `degenbot-bot` | integration test (06) | `crates/engine/degenbot-bot/examples/config_smoke_boot.rs` | The lightest start-up path of the bot engine driven by a config TOML file only: refuses a process that already carries any `DEGENBOT_*` variable, loads with the env layer disabled, installs the typed config, and observes the packed stances through `EngineStages` | — | a local test config file; **process-isolated** — `stance::install` is a process global, so this cannot share a test binary with anything that installs or reads a stance | writes nothing; returns an error (non-zero exit) on any wiring breakage | Becomes a `degenbot-bot` integration test with its own local config fixture, asserting the no-`DEGENBOT_*` guard, the file-layer load, and the `streaming_delivery=false` / `min_profit_wei` / `metrics_addr` observations. Gate: `just test-rust-nextest -p degenbot-bot` |
| `headless_boot` | `degenbot-ingestion` | integration test (06) | `crates/foundation/degenbot-ingestion/examples/headless_boot.rs` | Headless standalone-Rust boot over the `degenbot-ingestion` public surface with NO Python and NO network: a synthetic fused event stream through the topic filter, the watchdog windows constructing and evaluating, and the backfill filter builder producing a server-side-filterable `eth_getLogs` filter | — | none — synthetic stream in source | writes nothing; `panic!`s on any check failure | Becomes a network-free `degenbot-ingestion` integration test; `just test-standalone` keeps its two-check scope by invoking the test explicitly (today it runs this binary as command two) |

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

### 4.7 Feature-column provenance — the one row that is not metadata-declared

`hotpath_prometheus_probe` is the only row whose `Features` cell is not a Cargo
`required-features` value: `cargo metadata` reports `required_features: null` for
`degenbot-bot`'s example today, so its `hotpath-prometheus` need lives in the source and in
the `DEGENBOT_HOTPATH=1` runtime wiring plus the `just check-dev-tools --engine-prometheus`
compile lane, **not** in the manifest. No row may be described as metadata-declared that the
metadata does not declare — that is exactly the kind of claim a later task inherits and
trusts.

Task 05 makes the declaration real when it lands the replacement binary: the new `[[bin]]
hotpath_prometheus_probe` in `rust/tools/engine-lab/Cargo.toml` carries
`required-features = ["prometheus"]`, forwarding
`prometheus = ["degenbot-bot/hotpath-prometheus"]`. Until that lands, the row stays marked
*(runtime)* and its CI obligation remains the Prometheus compile lane rather than an
execution claim.

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
then goes.

### 5.2 `scale_demo` → **delete as redundant** (task 04 leaves it in place, task 06 removes it)

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
no `--check` and no dry-run, and it writes into `CARGO_MANIFEST_DIR/tests/fixtures` — after
the move that anchor means "the tool package", which would silently create a second
fixtures directory. Task 04 must re-point the output path (an explicit argument or a
repo-root resolver) before the move is called behavior-preserving, and must prove it by
regenerating both files with an empty `git diff`.

### 5.4 `mirror_coverage_probe` → **archive after preserving durable evidence** (task 05 leaves it in place, task 06 extracts and removes)

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

Task 05 does not move, rename, or delete this target. `mirror_coverage_probe` stays at
`crates/engine/degenbot-simulation/examples/mirror_coverage_probe.rs` until task 06 has both
the durable doc and the extracted offline test in place; moving it into `degenbot-engine-lab`
first would buy one manifest stanza and one re-anchoring for a file that is about to be
deleted, and would put the 14 `#[path]`-coupled simulation test files at risk (§10.2) for no
gain.

### 5.5 `v2_concentrated_liquidity_parity_probe` → **retain as a tool, extract the assertions** (task 04 moves it into `degenbot-solver-lab`, task 06 extracts the assertions)

Evidence: it is the evidence record for ADR-060's verdict B ("keep the interface collapse;
the representation collapse fails the zero-tolerance bar"), and that verdict is durable in
`docs/architecture/v2-cl-parity-probe.md`. But the fact it establishes is arithmetic, not
historical: 1236 comparisons, `consumed_mismatch = 0` everywhere (the range is widened past
every input so the step stays target-unreachable), zero-for-one strictly non-positive
divergences, and 0% divergence in the sub-`1e-3` ratio regime. If either simulator's
rounding changed, those facts would change and nothing currently notices — the probe is
never run by a gate.

Disposition: **retain** in `degenbot-solver-lab` (offline, `capture-fixtures`, committed
corpora — cheap to run by hand) **and extract** the decision-critical invariants into
`degenbot-pools` integration-test coverage in task 06: `consumed_input` equality on every
comparison, the direction sign of the divergence (zero-for-one never positive), and the
zero-divergence claim for the deep-ratio regime. The full report stays a tool; the
assertions that a change must not silently break become durable.

### 5.6 `walkmemo_readout` → **delete as redundant** (task 04 leaves it in place, task 06 removes it)

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

### 5.7 The signed-off keeper and the two roles later tasks must not misclassify

- **`stateview_feasibility_probe` — KEEP, and its header is stale.** The file still opens
  `// Throwaway spike harness (StateView mechanism feasibility). // Lives only for this
  spike; deleted after supervisor sign-off per docs/architecture/stateview-feasibility.md §7`.
  That §7 caveat 5 records the opposite: "the harness is committed and lint-exempted with a
  dated throwaway header; **supervisor decision at 2UVG3E sign-off: KEEP it as the reusable
  budget-regression probe (deletion is no longer planned)**." The decision stands: the probe
  is the tool that re-measures the cheap-read budget (clone 40–50 ns at p90 density, ≤ 23 µs
  p99 for a full 32-block journal rewind, and the 262k-entry superlinear clone anomaly a
  future out-of-corpus growth would have to revisit). The header, the doc's stale
  `degenbot-solvers` package paths, and the `-p degenbot-solvers` reproduction commands are
  documentation debt for tasks 06/07 — the *disposition* is not up for reconsideration.
- **`entry_ab_readout` — post-unification role: a forwarding-parity harness, not an A/B.**
  After ergo `XW3H5V` the two arms share ONE solve body: the "intake" arm is
  `solve_concentrated_liquidity_piecewise`, now a forwarding convenience, and the "direct"
  arm is `solve_piecewise`. The ratio therefore measures the intake shape alone (measured
  costs: 0.9926x / 1.0068x — noise), and the harness's real job is the **byte-equality gate
  on the forwarding contract**. The historical decision tables stay in
  `docs/architecture/solve-entry-unification.md`; task 06 must preserve the forwarding role
  independently of those numbers — and the durable assertion already exists in-crate (three
  tests in `solve/concentrated_liquidity/tests/memo.rs`), so no new test is owed. Task 06
  records that mapping; it does not delete the tool and it does not resurrect the A/B.
- **`bench_strategies` — benchmark role, permanently.** It is the microbench whose numbers
  ADR-024's addendum quotes, and the instrument any incremental-maintenance change must beat.
  It is not a demo, not a gate, and it must never gain a pass/fail threshold. `scale_demo`
  was the demo; `bench_strategies` is the measurement. Keep them classified separately in
  every later manifest and doc edit.

## 6. Planned package graph

Three non-publishable tool packages, all workspace members, all excluded from
`default-members` (so unqualified `cargo build --manifest-path rust/Cargo.toml` keeps
selecting only the two product entry points):

| Package | Directory | Publish | Default member | Final targets | Forwarded features |
| --- | --- | --- | --- | --- | --- |
| `degenbot-fixture-tools` | `rust/tools/fixture-tools/` | `false` | no | lib (`src/lib.rs`) + `src/bin/<tool>/main.rs` × 9 | `sql-ledger = ["degenbot/sql-ledger"]` |
| `degenbot-solver-lab` | `rust/tools/solver-lab/` | `false` | no | lib + `src/bin/` × 9 | `capture-fixtures = ["degenbot-pools/capture-fixtures"]`, `cache-lab = ["degenbot-pools/cache-lab"]`, `hotpath = ["degenbot-pools/hotpath", "hotpath/hotpath", "hotpath/threads"]`, `envelope = ["degenbot-order-index/envelope"]` |
| `degenbot-engine-lab` | `rust/tools/engine-lab/` | `false` | no | lib + `src/bin/` × 18 | `evm2-jit = ["degenbot-simulation/evm2-jit"]`, `prometheus = ["degenbot-bot/hotpath-prometheus"]` |

The final binary counts are **fixture-tools 9, solver-lab 9, engine-lab 18 — 36 tool
binaries**, and `9 + 9 + 18 = 36`. The remaining eight original targets do not become
binaries: 3 public retainers (§3), 2 owning-crate integration tests (§4.5), and 3 removed
executables (§5.2, §5.4, §5.6); `36 + 3 + 2 + 3 = 44`. The solver-lab and engine-lab counts
are the *final* ones: tasks `04`/`05` move nine of their eleven/nineteen owned rows and leave
`scale_demo`, `walkmemo_readout`, and `mirror_coverage_probe` where they are for task `06`.
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
   separately named checks* (`--default`, `--fixture-sql-ledger`, `--solver-capture-fixtures`,
   `--solver-cache-lab`, `--engine-jit`, `--engine-prometheus`), never `--all-features`:
   `--all-features` on the union graph is exactly the monolith this epic exists to avoid, and
   it makes a small solver replay link LLVM, SQL, RPC, and fixture machinery.
2. **`required-features` per binary, not per package.** A heavy binary
   (`sim_mode_bench`, `aot_prepare_executor`, `capture_seam_probe` under `evm2-jit`;
   `hotpath_prometheus_probe` under `prometheus`; the SQL-ledger recorders under
   `sql-ledger`; the corpus readers under `capture-fixtures`) carries its own
   `[[bin]] required-features`, so a default `cargo check` of the package *deliberately*
   skips it and the skip is visible in the manifest rather than implied.
3. **No tool edge into a publishable product crate.** All edges point *from* tools *to*
   product crates. The one reverse edge that exists today is test-only and path-based:
   14 test files in `degenbot-simulation/tests/` `#[path]`-include `examples/shared/*.rs`
   (see §10.2) — task 05 must resolve it without adding a normal dependency.
4. **Isolation is a checkable claim**: `cargo tree -p <tool> -e normal` per lane must not
   show another family's feature-selected deps. Record those `cargo tree` commands in the
   `just check-dev-tools` recipe comments as the evidence form, mirroring the
   `docs/architecture/capability-restructure-plan.md` W3 acceptance shape.

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
- **Three example files are never moved.** `scale_demo`, `walkmemo_readout`, and
  `mirror_coverage_probe` stay in their current crate `examples/` directories until task 06
  has preserved what each still carries and then deletes them (§5.2, §5.4, §5.6). No task 04
  or 05 migration may list them, and no `src/bin/<name>/main.rs` is created for them.
- **Nothing lands in the tool packages that §2 counts as removed.** The final `src/bin/` counts
  are fixed: fixture-tools 9, solver-lab 9, engine-lab 18 (§6). A task that ends up with an
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
| `cargo run -p degenbot --features degenbot/sql-ledger --example generate_evm_oracle_captures -- --check <homes>` | `cargo run -p degenbot-fixture-tools --features sql-ledger --bin generate_evm_oracle_captures -- --check <homes>` (wrapped by `just check-evm-captures`) | 03 |
| `cargo run -p degenbot --features degenbot/sql-ledger --example generate_aave_adversarial_captures -- --check <homes>` | `cargo run -p degenbot-fixture-tools --features sql-ledger --bin generate_aave_adversarial_captures -- --check <homes>` (wrapped by `just check-aave-captures`) | 03 |
| `cargo build -p degenbot-executor --example encode_three_hop` | `cargo build -p degenbot-fixture-tools --bin encode_three_hop` (wrapped by `just build-executor-three-hop-encoder`; the JSON stdout contract is unchanged) | 03 |
| `cargo run -p degenbot-rpc --example concentrated_liquidity_capture_gen` | `cargo run -p degenbot-fixture-tools --bin concentrated_liquidity_capture_gen` | 03 |
| `cargo run -p degenbot-pools --features capture-fixtures --example concentrated_liquidity_solve_replay -- <corpus>` | `cargo run -p degenbot-solver-lab --features capture-fixtures --bin concentrated_liquidity_solve_replay -- <corpus>` (wrapped by `just test-golden-replay`) | 04 |
| `cargo run -p degenbot-pools --features capture-fixtures --example entry_ab_readout -- <corpus>` | `cargo run -p degenbot-solver-lab --features capture-fixtures --bin entry_ab_readout -- <corpus>` (wrapped by `just entry-ab`) | 04 |
| `cargo run -p degenbot-pools --features capture-fixtures,cache-lab --example concentrated_liquidity_cache_lab` | `cargo run -p degenbot-solver-lab --features capture-fixtures,cache-lab --bin concentrated_liquidity_cache_lab` | 04 |
| `cargo run -p degenbot-pools --example stateview_feasibility_probe` | `cargo run --release -p degenbot-solver-lab --bin stateview_feasibility_probe` | 04 |
| `cargo run --release -p degenbot-order-index --example bench_strategies` | `cargo run --release -p degenbot-solver-lab --features envelope --bin bench_strategies` | 04 |
| `cargo run -p degenbot-simulation --example sim_mode_bench --features evm2-jit` | `cargo run -p degenbot-engine-lab --features evm2-jit --bin sim_mode_bench` (wrapped by `just bench-sim-modes`) | 05 |
| `cargo run --release -p degenbot-arbitrage --example bal_dispatch_scaling_probe` | `cargo run --release -p degenbot-engine-lab --bin bal_dispatch_scaling_probe` (wrapped by `just bal-dispatch-scaling-probe`) | 05 |
| `DEGENBOT_HOTPATH=1 cargo run -p degenbot-bot --features hotpath-prometheus --example hotpath_prometheus_probe` | `DEGENBOT_HOTPATH=1 cargo run -p degenbot-engine-lab --features prometheus --bin hotpath_prometheus_probe` | 05 |
| `cargo run -p degenbot-bot --example config_smoke_boot -- <config.toml>` | *(no command)* — becomes a `degenbot-bot` integration test, run by `just test-rust-nextest -p degenbot-bot` | 06 |
| `cargo run -p degenbot-ingestion --example headless_boot` | *(no command)* — becomes a `degenbot-ingestion` integration test, invoked explicitly by `just test-standalone` | 06 |
| `cargo run -p degenbot-order-index --features envelope --example scale_demo` | *(no target command)* — the executable is deleted in 06 (`bench_strategies` is the measurement that survives, §5.2/§5.7) | 06 |
| `cargo run -p degenbot-pools --features capture-fixtures --example walkmemo_readout` | *(no target command)* — the executable is deleted in 06; the memo's durable evidence is the doc plus the in-crate memo tests (§5.6) | 06 |
| `cargo run -p degenbot-simulation --example mirror_coverage_probe` | *(no target command)* — deleted in 06 only after the overlap table reaches a durable doc and the projection assertion becomes an offline `degenbot-simulation` test (§5.4) | 06 |
| `cargo run -p degenbot-pools --features capture-fixtures --example solve_path_probe` | **unchanged** — public retainer | — |
| `cargo run -p degenbot --example standalone_consumer` | **unchanged** — public retainer (`just test-standalone`) | — |
| `cargo run -p degenbot-execution-sample --example foreign_consumer` | **unchanged** — public retainer | — |

### 8.2 Per-destination verification ownership

No move is done when the file compiles; it is done when the named route below passes with the
moved binary and the old target is gone.

| Destination | Default compile lane | Feature lanes | Behavioural route (the one that actually proves the move) |
| --- | --- | --- | --- |
| `degenbot-fixture-tools` | `cargo check -p degenbot-fixture-tools --all-targets` | `--features sql-ledger` | `just check-evm-captures`, `just check-aave-captures` (offline drift gates), `just test-executor-offline`, and the wave-2/3/4/5 replay suites; live recorders: compile only |
| `degenbot-solver-lab` | `cargo check -p degenbot-solver-lab --all-targets` | `--features capture-fixtures`, `--features capture-fixtures,cache-lab`, `--features envelope`, `--features hotpath` | `just test-golden-replay` (both corpora), `just entry-ab`, the cache-lab golden suite, `cargo test -p degenbot-order-index`, `cargo test -p degenbot-pools profit_envelope` (for `synth_corpus_gen` regeneration) |
| `degenbot-engine-lab` | `cargo check -p degenbot-engine-lab --all-targets` | `--features evm2-jit`, `--features prometheus` | `just test-rust-nextest -p degenbot-simulation` and `-p degenbot-arbitrage` (the 14 `#[path]`-coupled test files plus the parity suites must still run), `just bal-dispatch-scaling-probe`, `just bal-candidate-scaling-probe`; live/`/tmp`-corpus rows: compile only, reported as unexecuted |
| Owning-crate tests | `cargo check -p degenbot-bot --all-targets`, `-p degenbot-ingestion` | — | `just test-rust-nextest -p degenbot-bot`, `-p degenbot-ingestion`, then `just test-standalone` with its re-pointed second check |
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
| `degenbot-simulation`: `aot_prepare_executor`, `cold_miss_fanout_probe`, `worker_fanout_probe`, `bal_wall_ceiling_probe`, `bal_delta_snapshot_probe`, `bal_retention_probe` (×3), `bal_delta_rounds_probe` (×2), `bal_production_relevance_probe`, `bal_head_lifecycle_probe`, and `examples/shared/bench_corpus.rs` | `join("../../../..")` or a `format!`-built path | 05 |
| `degenbot` (facade): `generate_aave_adversarial_captures`, `generate_aave_config_windows`, `generate_aave_upgrade_windows`, `generate_evm_oracle_captures` (×2), `record_py_oracle_corpus` | `join("../../../..")` / `join("../../../../tests/fixtures")` | 03 |
| `degenbot-rpc`: `concentrated_liquidity_capture_gen` | manifest-relative path `format!` | 03 |
| `degenbot-pools`: `synth_corpus_gen` (**writes** into `tests/fixtures`), `v2_concentrated_liquidity_parity_probe` | `join("tests/fixtures")` / manifest-relative fixtures | 04 |

Required form: resolve the repository root once (an explicit `--repo-root`/`--out` argument
where a tool writes, or a single `ancestors()`-walking resolver in the tool library), then
derive every fixture path from it. Verification: the moved binary's read/write target must be
identical to today's — proven by a `--check` run (readers) or a regeneration with an empty
`git diff` (writers).

### 10.2 Fourteen simulation test files compile example-shared code through `#[path]`

`crates/engine/degenbot-simulation/tests/` contains 14 files with 54 `#[path =
"../examples/shared/..."]` attributes (`bal_candidate_scaling`, `bal_capability`,
`bal_corpus_segments`, `bal_delta_rounds`, `bal_delta_snapshot`, `bal_head_lifecycle`,
`bal_head_lifecycle_probe`, `bal_head_pipeline`, `bal_production_relevance`, and siblings).
Those tests are *why* the shared modules are durable (they pin the scaling model, the wall
ledger's bucket reconciliation, the read-digest gate, the corpus segmentation). Moving
`examples/shared/` into `degenbot-engine-lab`'s library breaks every one of them unless the
tests are re-pointed in the same task. Task 05 owns this: either the tests move beside the
new library, or they consume the tool library in a way that does not add a normal edge to a
publishable crate, or the shared modules become a test-support module the tests can reach
without a `#[path]` attribute. Whichever it picks, `just test-rust-nextest -p
degenbot-simulation` must run the same named tests afterwards — a silently emptied test file
is the failure mode to watch for.

`degenbot-arbitrage` has the same pattern in miniature: its two BAL examples `#[path]`-include
`examples/shared/{dispatch_probe,dispatch_scaling}.rs`.

### 10.3 External corpora that are not in the tree

`bal_delta_rounds_probe`, `session_fanout_probe`, and `bal_production_relevance_probe`
require `/tmp`-style trace files that are not committed (audited: `/tmp/fanout_trace.log` and
`/tmp/bal_broader_trace.log` absent; `logs/` is gitignored), and `bal_real_read_oracle_probe`
defaults to `logs/bal_postfix_soak_trace.log`, also untracked. These rows are
**compile-lane-only** until a trace is supplied; `bal_carry_probe`,
`bal_head_lifecycle_probe` (live legs), `bal_live_acquisition_probe`,
`capture_path_fixture`, the four live recorders, and `concentrated_liquidity_capture_gen`
need a node. Report them as unexecuted — never as passing, and never inferred from a
`cargo check`.

### 10.4 Stale package spellings in the decision records

The docs that authorize this epic's keepers still say `degenbot-solvers` (a package that no
longer exists) in their harness paths and reproduction commands:
`docs/architecture/stateview-feasibility.md` (spike header, §2, §6),
`walkmemo-cross-block-reuse-readout.md`, `solve-entry-unification.md`,
`v2-cl-parity-probe.md`. Their *decisions* stand; their commands must resolve to the new
homes. Task 07 owns the sweep (it may not rewrite the historical narratives — only the
paths), and tasks 04/05 fix the ones their own moves invalidate.

## 11. Continuation

Nothing here requires a further design decision from this task. The earliest open questions
already belong to named tasks: task 02 owns the scaffold shape (§6), task 03 the path
re-anchoring for recorders (§10.1), task 04 the `examples/common/` conversion and
`synth_corpus_gen`'s write anchor, task 05 the `#[path]`-coupled test resolution (§10.2),
task 06 the six dispositions of §5 and the doc extractions they imply, and task 07 the final
catalog rewrite of this file plus the stale-path sweep (§10.4).

Two constraints apply to every one of those tasks: the §2 partition (3 + 9 + 9 + 18 + 2 + 3)
is the accounting they inherit, and tasks `04` and `05` move only rows whose final
disposition is a tool binary — `scale_demo`, `walkmemo_readout`, and `mirror_coverage_probe`
stay in their crate `examples/` directories until task `06` preserves their evidence and
removes them.
