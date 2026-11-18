# Rust development tools — current catalog

This file is the **current catalog** of the workspace's development-tool surface and its
public example surface: one row per target, the command that runs it today, its feature
lane, its inputs, its write behavior, and whether an automatic gate runs it. Per-slice
history lives in the commit history and the ADRs; `just` recipes and Cargo metadata are
the executable truth this catalog mirrors.

## Public examples (3 rows)

Consumer-facing starting points. Not tools; never moved into a tool package.

| Target | Source | Purpose | Consumer gate | Inputs | Behavior |
| --- | --- | --- | --- | --- | --- |
| `standalone_consumer` | [`degenbot` examples](../crates/facade/degenbot/examples/standalone_consumer.rs) | Tier-0 `cargo add degenbot` proof: `BotState` + `UNISWAP_V2` preset + swap calc with no Python/pyo3; DB snapshot → auto-backfill → resume; one-file operator config layering; in-process evm2 sim | `just test-standalone` (first command of the recipe) | committed `parity.db` fixture, read-only (relocatable via `DEGENBOT_FIXTURE_DB`); optional live WS via `SMOKE_RPC_URL` — unset is CI-runnable | writes nothing; `panic!`/non-zero exit on any failed check |
| `solve_path_probe` | [`degenbot-pools` examples](../crates/foundation/degenbot-pools/examples/solve_path_probe.rs) | ADR-084 D1/C5 consumer (ii): solve a path through the root solve door using door items only; the documented pools-consumer snippet | `just check-consumer-probes` (first command of the recipe) | none — synthetic hops built in source | stdout only; `panic!` when the door returns no solution |
| `foreign_consumer` | [`degenbot-execution-sample` examples](../examples/degenbot-execution-sample/examples/foreign_consumer.rs) | User-defined `ExecutionAdapter` for a FOREIGN contract: projects a solved path into the sealed `SolveResult` view and encodes one transaction through the four-part seam, then repeats it through `ValidatedAdapter` (ADR-100 tier 1) — no `degenbot-arbitrage`, no pyo3 | `cargo run -p degenbot-execution-sample --example foreign_consumer` — the command `docs/execution-adapters.md` documents; no recipe names it today | none — `sol!` call types in source | stdout only |

## Lab binaries (37 rows)

Non-publishable tool packages: workspace members outside `default-members`, every edge
pointing from a tool to a product crate. Lane is the `[[bin]]` `required-features` value
(the `just check-dev-tools` lane that compiles it); `default` means no feature.

Status vocabulary: **gate** = an automatic recipe/test runs it and its exit code or bytes
block a change; **diagnostic** = a person or a measurement run invokes it; its owning
tests, where named, carry the durable assertions. **Compile-lane only** rows need a live
endpoint or an external corpus that is not in the tree: CI compiles them, never executes
them, and no offline report may claim a refreshed measurement from them.

### `degenbot-fixture-tools` (8 rows) — `fixture-tools/src/bin/`

Record / check / regenerate workflows for committed fixtures, and test-input generators.

| Binary | Purpose | Current command | Lane | Inputs | Writes | Status |
| --- | --- | --- | --- | --- | --- | --- |
| [`capture_path_fixture`](fixture-tools/src/bin/capture_path_fixture/main.rs) | Recapture a path-investigation fixture's recorded chain state at its `target_block` (V2 scalars, V3 ticks, V4 StateView, batched over Multicall3); authored identity stays verbatim | `cargo run -p degenbot-fixture-tools --bin capture_path_fixture -- [--check] <fixture.json>...` | default | LIVE archive node (ADR-062 resolution); the named committed fixtures | default mode REWRITES each fixture JSON; `--check` writes nothing, exits 1 on drift | diagnostic — **compile-lane only** (both arms read the chain). Schema/recapture library owned by `degenbot-simulation` (`degenbot_simulation::investigation`, re-exported by the `degenbot` umbrella); offline consumers: `tier3_*` tests |
| [`generate_aave_adversarial_captures`](fixture-tools/src/bin/generate_aave_adversarial_captures/main.rs) | Generate the wave-3 aave-side adversarial goldens node-free; determinism asserted by generating twice byte-identically | `just check-aave-captures` | `sql-ledger` | offline — committed `tests/fixtures/cassettes/wave3` + `sql_goldens/wave3` (`--fixtures-home` re-points at a copy) | (Re)writes both fixture homes; `--check` writes nothing, exits 1 on drift | **gate** — offline drift gate. Offline consumer: `degenbot-aave`'s `wave3_capture_replay.rs` |
| [`generate_aave_config_windows`](fixture-tools/src/bin/generate_aave_config_windows/main.rs) | Record the wave-5 Aave config-dispatch span cassettes from the live chain, replaying through the statement ledger | `cargo run -p degenbot-fixture-tools --features sql-ledger --bin generate_aave_config_windows -- [--check] [--node <uri>]` | `sql-ledger` | LIVE node; committed wave-5 cassette + golden homes | (Re)writes cassettes, statement ledger, canonical DB dump; every window recorded twice | diagnostic — **compile-lane only** (recorder needs a node). Offline owner: `degenbot-aave`'s `wave5_config_span_replay.rs` + `CONFIG-SPAN-MAP.md` |
| [`generate_aave_upgrade_windows`](fixture-tools/src/bin/generate_aave_upgrade_windows/main.rs) | Record the wave-4 Aave upgrade-span cassettes from the live chain; substrate from the chain's current reserve set | `cargo run -p degenbot-fixture-tools --features sql-ledger --bin generate_aave_upgrade_windows -- [--check] [--node <uri>]` | `sql-ledger` | LIVE node; committed wave-4 homes | same write/`--check` shape as the wave-5 recorder; record-twice determinism | diagnostic — **compile-lane only**. Offline owner: `degenbot-aave`'s `wave4_upgrade_span_replay.rs` |
| [`generate_evm_oracle_captures`](fixture-tools/src/bin/generate_evm_oracle_captures/main.rs) | Generate the wave-2 EVM-oracle goldens node-free: canonical `UniswapV3Pool` harness artifact, real `run_pool_update` chunk loop, SQL goldens replayed through the ledger | `just check-evm-captures` | `sql-ledger` | offline — committed `tests/fixtures/cassettes/wave2` + `sql_goldens/wave2` (`--fixtures-home` re-points at a copy) | (Re)writes both homes; `--check` writes nothing, exits 1 on drift | **gate** — offline drift gate. Offline consumer: `degenbot-pool-updater`'s `wave2_capture_replay.rs` |
| [`record_py_oracle_corpus`](fixture-tools/src/bin/record_py_oracle_corpus/main.rs) | Record the offline oracle-answer corpus for the fork-bound parity tests into the per-block `OfflineProvider` JSON home; every value comes off the wire | `cargo run -p degenbot-fixture-tools --bin record_py_oracle_corpus -- --check --scenario <name> --ethereum-node <archive-url>` | default | LIVE archive node(s) (ordered endpoint pool); committed golden-key sources under `tests/fixtures/chain_data` | writes `tests/fixtures/chain_data/<chain_id>/py_oracle_<scenario>_block<N>.json`; every scenario recorded twice; `--check` requires byte-identity, writes nothing | diagnostic — **compile-lane only** (no offline arm). Replay consumers: `tests/golden/test_oracle_replay_offline.py`, live drift guard `just live-drift`; provenance `tests/fixtures/chain_data/README.md` |
| [`record_updater_cassette`](fixture-tools/src/bin/record_updater_cassette/main.rs) | Record one updater chunk span cassette (pool `PoolCreated` + V3 liquidity scan, or an Aave market chunk), with the `--verify` gate surface and the `--kind aave-run` loop | `cargo run -p degenbot-fixture-tools --bin record_updater_cassette -- --check tests/fixtures/cassettes/*.json` (offline arm; the record arm adds `--node`/`--out`) | default | `--check` arm fully offline; record arm needs a LIVE node (ADR-062 resolution) | record arm writes cassette JSON canonically; `--check` re-serializes each cassette, re-checks entry digests, writes nothing, exits 1 on drift | **gate** on the offline `--check` arm (every committed cassette byte-identical); record arm compile-lane only. Replay owners: pool-updater `cassette_replay_run.rs`/`verification_gate_probes.rs`, aave `aave_cassette_replay.rs`/`aave_config_cassette_replay.rs`; writer contract `degenbot-rpc`'s `cassette.rs` |
| [`concentrated_liquidity_capture_gen`](fixture-tools/src/bin/concentrated_liquidity_capture_gen/main.rs) | The ONLY sanctioned producer of exact-wei goldens for the CL replay corpus: fetch curated V3 pools at a pinned block, backfill ticks, build and solve the 2-hop CL paths, write JSONL | `cargo run -p degenbot-fixture-tools --bin concentrated_liquidity_capture_gen -- [<out.jsonl>]` | default | LIVE archive node (`DEGENBOT_CLCAP_RPC`, `DEGENBOT_CLCAP_BLOCK`, `DEGENBOT_CLCAP_PATH_CAP`, `DEGENBOT_CLCAP_MAX_FETCHES`); explicit path overrides the committed corpus default | writes the capture JSONL **and** a `.block` sidecar (pinned block + regen command) | diagnostic — **compile-lane only**, never run by tests; regeneration is manual and live-only. Offline consumer of its product: `just test-golden-replay` |

### `degenbot-executor-fixture-tools` (1 row) — `executor-fixture-tools/src/bin/`

The offline three-hop production-calldata oracle, in its own minimal package: its only
edges are the five crates its source names (`alloy`, `degenbot-execution`,
`degenbot-executor`, `serde`, `serde_json`), so an independent encoder build resolves the
production executor's graph and nothing else.

| Binary | Purpose | Current command | Lane | Inputs | Writes | Status |
| --- | --- | --- | --- | --- | --- | --- |
| [`encode_three_hop`](executor-fixture-tools/src/bin/encode_three_hop/main.rs) | Encode one three-hop `PathInfo` through the production command grammar so the execution matrices exercise production bytes | `just build-executor-three-hop-encoder` (build; the trusted FFI suite consumes `<target-dir>/debug/encode_three_hop`), consumed by `just test-executor-production-calldata` (and therefore by `just test-executor` and `just test-executor-offline`) | default | none — one JSON request document as `argv[1]` | stdout only: one hex line; a missing/invalid request exits non-zero | **gate** — the trusted FFI suite (`tests/foundry_executor/test/CmdExecutorThreeHopProductionCalldata.t.sol`) consumes its output, including failure paths |

### `degenbot-solver-lab` (9 rows) — `solver-lab/src/bin/`

Solver and order-index replay, measurement, and stress instruments.

| Binary | Purpose | Current command | Lane | Inputs | Writes | Status |
| --- | --- | --- | --- | --- | --- | --- |
| [`bench_strategies`](solver-lab/src/bin/bench_strategies/main.rs) | Order-index cost-component microbench (build, insert, update/remove, per-block `top_k`); source of ADR-024's addendum numbers | `cargo run --release -p degenbot-solver-lab --features envelope --bin bench_strategies` | `envelope` | none — synthetic points in source, hard-coded N=1M run and 10M leg | stdout report only | measurement — **compiled, never executed** by a routine gate (no smaller mode); numbers in `docs/adr/ADR-024-net-profit-order-index.md` §Addendum |
| [`concentrated_liquidity_cache_lab`](solver-lab/src/bin/concentrated_liquidity_cache_lab/main.rs) | Replay a capture as pool-state transitions and solve every state through every cache strategy, checking byte-equality against full rebuild; prints the rebuild-cost matrix | `cargo run -p degenbot-solver-lab --features capture-fixtures,cache-lab --bin concentrated_liquidity_cache_lab -- <capture.jsonl>` | `capture-fixtures`, `cache-lab` (+ `telemetry` second build shape: `solver-cache-telemetry` lane) | committed corpus (default) or a capture path; `DRCLAB_MAX_PATHS`/`_TRANS`/`_MICRO`/`_DIGEST` | stdout; exits 1 on any strategy divergence | diagnostic — golden/regression coverage lives in the pools `concentrated_liquidity_cache_lab_goldens` and `concentrated_liquidity_cache_patch_regressions` suites; report `docs/cache-lab-report.md` |
| [`concentrated_liquidity_solve_replay`](solver-lab/src/bin/concentrated_liquidity_solve_replay/main.rs) | Offline heavy-CL replay with N-repeat stable timing; classifies every path through the shared `degenbot_pools::solve::golden_gate` evaluator | `just test-golden-replay` (follows the owning pools tests as the non-gating readout pass) | `capture-fixtures` | committed corpora `concentrated_liquidity_capture_offline.jsonl`, `heavy_concentrated_liquidity_solve_captures.jsonl[.zst]`; `DR_REPLAY_ITERS` | stdout; `DR_REPLAY_REGEN=1` rewrites stale over-shoot-only goldens — a write mode never run by a gate | diagnostic (measurement/readout) — the gating contract is the pools integration test `tests/concentrated_liquidity_golden_replay.rs`, run by `just test-golden-replay` |
| [`entry_ab_readout`](solver-lab/src/bin/entry_ab_readout/main.rs) | Post-unification readout: the retained all-CL intake against `solve_piecewise` on real captured paths, byte-equality checked; classifies the traffic mix | `just entry-ab [corpus]` | `capture-fixtures` (+ `hotpath` optional second build shape for functions timing) | capture path (recipe defaults to `heavy_concentrated_liquidity_solve_captures.jsonl`); `DR_AB_ITERS`, `DR_AB_MEMO` | stdout; writes the hotpath JSON only when `HOTPATH_OUTPUT_PATH` is set; exits 1 on arm divergence | diagnostic — its forwarding-parity contract is pinned by the pools memo tests (`crates/foundation/degenbot-pools/src/solve/concentrated_liquidity/tests/memo.rs`) |
| [`mobius_normalization_readout`](solver-lab/src/bin/mobius_normalization_readout/main.rs) | Interleaved exact-I1024/BigInt vs normalized-I512 shifted-piece arithmetic A/B, with full-result/profit/input divergence counts and optional anchor timing | `just mobius-normalization-ab [corpus]`; `just mobius-normalization-telemetry [corpus]` | `capture-fixtures` (+ `telemetry` second build shape) | capture path (recipe defaults to `heavy_concentrated_liquidity_solve_captures.jsonl`); `DR_NORMALIZATION_ITERS` | stdout only; divergences are reported, never hidden or converted into a timing gate | measurement — exact arithmetic remains the default; promotion requires a separate correctness decision |
| [`mixed_solve_replay`](solver-lab/src/bin/mixed_solve_replay/main.rs) | Offline heavy mixed V2+CL replay with N-repeat stable timing and the same two-sided golden gate | `cargo run -p degenbot-solver-lab --features capture-fixtures --bin mixed_solve_replay -- <capture.jsonl>` (no recipe) | `capture-fixtures` | `heavy_mixed_solve_captures.jsonl[.zst]`; `DR_REPLAY_ITERS` | stdout; exits 1 on golden drift or nondeterminism | diagnostic — its own exit code reports golden drift or nondeterminism when invoked; no maintained recipe runs it |
| [`rayon_scale_probe`](solver-lab/src/bin/rayon_scale_probe/main.rs) | Solver-parallelism scaling: production solve under controlled thread counts, wall / sum-CPU / efficiency per configuration | `cargo run --release -p degenbot-solver-lab --features capture-fixtures --bin rayon_scale_probe` | `capture-fixtures` (enables its optional `rayon`/`tracing` edges; a default build compiles neither) | committed heavy-CL corpus; `--paths`, `--serial-passes`, `--ns`, `--bare-only`, `--skip-gate`, `--skip-control` | stdout CSV; the golden-lite check excludes (never silently keeps) a flagged path | measurement — no timing threshold; report `docs/rayon-parallelism-lab.md` |
| [`stateview_feasibility_probe`](solver-lab/src/bin/stateview_feasibility_probe/main.rs) | Reusable budget-regression instrument for the block-epoch data plane: tickmap clone sweep, journal-replay materialization, `Arc` COW, V2 scalar costs, RSS anchors | `cargo run --release -p degenbot-solver-lab --bin stateview_feasibility_probe` | default | none — synthetic in-memory journals; reads `/proc/self/statm` (Linux) | stdout tables (M1–M6 matrices) | measurement — the 1001-rep/large-memory matrix is **compile-lane only**; budget rule and numbers in `docs/architecture/stateview-feasibility.md` |
| [`synth_corpus_gen`](solver-lab/src/bin/synth_corpus_gen/main.rs) | Generator of the giant-liquidity family shapes observed live; writes the two committed synthetic corpora | `cargo run -p degenbot-solver-lab --bin synth_corpus_gen`, then `git diff --exit-code` on both outputs and `just test-rust-nextest -p degenbot-pools -E 'test(profit_envelope)'` | default | none — outputs resolved from the repository root through `degenbot_solver_lab::paths` | **writes** `crates/foundation/degenbot-pools/tests/fixtures/synth_giant_concentrated_liquidity.jsonl` and `..._mixed.jsonl`; no `--check` | diagnostic — verification is regenerate + empty diff (the concentrated-liquidity file re-derives byte-identically; the mixed file's drift predates its current home, recorded in commit history) plus the pools kernel test |

### `degenbot-engine-lab` (18 rows) — `engine-lab/src/bin/`

Bot, simulation, arbitrage, JIT, fan-out, and BAL instruments. The BAL rows' corpus and
parity support is product-owned (`degenbot_simulation::probe_support`,
`degenbot_arbitrage::probe_support`) — never duplicated here.

| Binary | Purpose | Current command | Lane | Inputs | Writes | Status |
| --- | --- | --- | --- | --- | --- | --- |
| [`aot_prepare_executor`](engine-lab/src/bin/aot_prepare_executor/main.rs) | Production executor AOT prep: compile the injected `cmd_executor` runtime bytecode into the durable artifact store and prove it loads back as `ProgramKind::Aot` | `just aot-prepare-executor`; compile lane `just check-dev-tools --engine-jit` | `evm2-jit` | committed `crates/foundation/degenbot-executor/releases/v2/deployment/cmd_executor_runtime_bytecode.txt` (rooted via `degenbot_engine_lab::paths`) | **writes** the artifact store (`<persistence.state_dir>/aot-artifacts`, temp sibling + rename, idempotent) | diagnostic — operational tool, explicitly **not** a production CLI |
| [`bal_candidate_scaling_probe`](engine-lab/src/bin/bal_candidate_scaling_probe/main.rs) | Analytical BAL candidate-scaling report: two-corpus calibration, candidate-count × family × novelty matrix, W/F sensitivity — pure formula, no candidate executed | `just bal-candidate-scaling-probe` | default | committed corpus aggregates via product-owned `degenbot_simulation::probe_support` | stdout Markdown; `BAL_SCALING_PLANT_MISMATCH=1` exits non-zero (negative probe) | diagnostic — offline; behavior pins in `degenbot-simulation`'s `tests/bal_candidate_scaling.rs` |
| [`bal_carry_probe`](engine-lab/src/bin/bal_carry_probe/main.rs) | Parent-linked BAL carry-forward against live reth: every canonical block advanced through `ReadHubCache::advance_bal`, carried values vs independent cold reads | `just bal-carry-probe` | default | LIVE reth/archive node; a recorded `fanout-trace` file via `BAL_CARRY_TRACE` | stdout; any value mismatch is a hard failure | diagnostic — **compile-lane only** (live); per-anchor accounting is the reference the relevance and wall probes cross-check |
| [`bal_delta_rounds_probe`](engine-lab/src/bin/bal_delta_rounds_probe/main.rs) | Round-coalesced delta read replay: full-clone vs delta chain vs idle-block retention, with the re-read gap distribution | `just bal-delta-rounds-probe`; compile lane `cargo check -p degenbot-engine-lab --bin bal_delta_rounds_probe` | default | **EXTERNAL CORPUS, absent from the tree**: `BAL_DELTA_ROUNDS_TRACES` defaults to `/tmp/fanout_trace.log` + `/tmp/bal_broader_trace.log`, both REQUIRED; matching real-coverage captures committed | stdout; `BAL_DELTA_ROUNDS_PLANT_DIVERGENCE=1` must exit non-zero; results in gitignored `logs/bal_delta_rounds_result.md` | diagnostic — **compile-lane only** until the traces are supplied; unexecutable offline, never fabricate a run |
| [`bal_delta_snapshot_probe`](engine-lab/src/bin/bal_delta_snapshot_probe/main.rs) | Full-clone vs parent-linked delta-chain snapshot measurement at realistic working-set sizes, with a correctness leg and a planted-divergence negative probe | `just bal-delta-snapshot-probe` | default | committed `bal_real_coverage.json` via `degenbot_simulation::probe_support`; `BAL_DELTA_SIZES`/`_REPS`/`_LOOKUPS`/`_MAX_DEPTH` | stdout; plant-divergence env must exit non-zero; results in `logs/bal_delta_snapshot_result.md` | diagnostic — offline, moderate cost; bounded default sizes only. Parity leg pinned by `degenbot-simulation`'s `tests/bal_delta_snapshot.rs` |
| [`bal_head_lifecycle_probe`](engine-lab/src/bin/bal_head_lifecycle_probe/main.rs) | Canonical-head lifecycle: prepare and apply stages measured separately, plus the offline waiter leg; every settled write compared to an independent cold read | `just bal-head-lifecycle-probe` + `just bal-head-lifecycle-compare` | default | LIVE node for historical/fresh-head legs; committed capture for the waiter leg; `BAL_HEAD_*` knobs | stdout + machine-readable JSON summary when `BAL_HEAD_SUMMARY_OUT` names a path (`schema: bal-head-lifecycle/1`) | diagnostic — live legs are **compile-lane only**; offline pins in `degenbot-simulation`'s `bal_head_lifecycle*.rs` tests; compare script `scripts/bal_head_lifecycle_compare.py` refuses a foreign schema or an empty leg |
| [`bal_production_relevance_probe`](engine-lab/src/bin/bal_production_relevance_probe/main.rs) | Paired relevance replay: production-derived relevance vs the recorded-read oracle on the same captured corpus, with four gates incl. exact reproduction of `bal_carry_probe` accounting | `just bal-production-relevance-probe` | default | **EXTERNAL CORPUS, absent from the tree**: `BAL_PROD_TRACE` defaults to `/tmp/bal_production_trace.log`, REQUIRED; committed `bal_real_coverage_production.json` fixture | stdout; plant-divergence env must exit non-zero; results in gitignored `logs/bal_production_relevance_result.md` | diagnostic — **compile-lane only** until the trace exists; offline pins in `degenbot-simulation`'s `tests/bal_production_relevance.rs` |
| [`bal_real_read_oracle_probe`](engine-lab/src/bin/bal_real_read_oracle_probe/main.rs) | Real-candidate read-set oracle replay: soak-trace rows through exact-anchor BAL generations, every recorded read vs an independent cold read | `just bal-real-read-oracle-probe` | default | LIVE node; soak trace `BAL_READ_ORACLE_TRACE` (default `logs/bal_postfix_soak_trace.log`, gitignored, REQUIRED) | stdout; `BAL_READ_ORACLE_PLANT_WEI=1` must exit non-zero | diagnostic — **compile-lane only** (live + external corpus); recorded result `docs/architecture/bal-real-read-oracle-result.md` |
| [`bal_retention_probe`](engine-lab/src/bin/bal_retention_probe/main.rs) | Idle-block retention measurement over both committed real write-coverage captures and one synthetic long run, at thresholds 1/2/4/8 vs the unbounded control | `just bal-retention-probe` | default | committed `bal_real_coverage{,_broader}.json` via `degenbot_simulation::probe_support`; `BAL_RETENTION_*` knobs | stdout; plant-divergence env must exit non-zero; results in `logs/bal_retention_result.md` | diagnostic — offline; parity pins in `degenbot-simulation`'s tests |
| [`bal_wall_ceiling_probe`](engine-lab/src/bin/bal_wall_ceiling_probe/main.rs) | Deterministic wall-clock ceiling benchmark of BAL carry-forward: recorded read walks under three cache policies, wall in six reconciled buckets, eliminable storage-wait ceiling | `just bal-wall-ceiling-probe` | default | **EXTERNAL CORPUS**: `BAL_WALL_TRACE` defaults to `/tmp/fanout_trace.log`; committed real-coverage fixtures via `BAL_WALL_FIXTURE`; `BAL_WALL_FIXTURE_CAPTURE=1` is the live fixture-write mode (`just bal-real-coverage-capture`) | stdout + `logs/` results; plant-divergence env must exit non-zero | measurement — timing reported, never asserted; instrumentation pins in `degenbot-simulation`'s `tests/bal_wall_ceiling.rs` |
| [`bal_dispatch_scaling_probe`](engine-lab/src/bin/bal_dispatch_scaling_probe/main.rs) | Controlled BAL dispatch-scaling probe: the production dispatch entry over one scripted head/candidate list/deployment, one candidate to burst fan-out, only storage representation varying | `just bal-dispatch-scaling-probe` | default | offline — scripted chain/market in source; `BAL_DISPATCH_SCALING_COUNTS`/`_REPS`; support is product-owned `degenbot_arbitrage::probe_support` | stdout Markdown; **fails closed** on a broken fixture (wrong lane, cold control, missing prefetch identity, cross-lane verdict difference) | diagnostic — offline; durable parity matrix pinned by `degenbot-arbitrage`'s `tests/bal_dispatch_scaling.rs`; result `docs/architecture/bal-matched-parity-result.md` |
| [`bal_live_acquisition_probe`](engine-lab/src/bin/bal_live_acquisition_probe/main.rs) | Live reth BAL acquisition: the cost of acquiring ONE fixed historical block's EIP-7928 access list in both representations, plus per-fan-out packet prices | `just bal-live-acquisition-probe` | default | LIVE archive node; `BAL_ACQ_ANCHOR`/`_REPS`/`_CORPUS_COUNTS` | stdout; seed-parity disagreement and settled-value mismatch are hard failures | diagnostic — **compile-lane only** (live); priced conclusions in `docs/architecture/bal-matched-parity-result.md` |
| [`capture_seam_probe`](engine-lab/src/bin/capture_seam_probe/main.rs) | JIT-composition probe: does the production capture (access-list slots + swap events) survive off the `Inspector` hook? Three configurations with a set-equality PASS bar | `cargo run --release -p degenbot-engine-lab --features evm2-jit --bin capture_seam_probe` | `evm2-jit` | offline — corpus from `degenbot_engine_lab::bench_corpus` | stdout; prints PASS/FAIL | diagnostic — the durable JIT verdict parity it feeds is enforced by `degenbot-simulation`'s `tests/jit_spine.rs` and `tests/frame_replay_jit.rs` |
| [`cold_miss_fanout_probe`](engine-lab/src/bin/cold_miss_fanout_probe/main.rs) | Cold-miss cost and coroutine fan-out: eleven legs over a deterministic offline RPC stand-in charging per modeled round trip | `cargo run --release -p degenbot-engine-lab --bin cold_miss_fanout_probe` (bounded smoke: `COLD_MISS_FRAMES=2 COLD_MISS_LATENCY_US=1`) | default | offline — snapshot-derived state; `COLD_MISS_LATENCY_US`, `COLD_MISS_FRAMES`; corpus from `degenbot_engine_lab::bench_corpus` | stdout; timing reported, never asserted | measurement — the default N=8 sweep is a non-routine measurement; the bounded smoke keeps verdict-identity and prefetch assertions |
| [`session_fanout_probe`](engine-lab/src/bin/session_fanout_probe/main.rs) | Session-scheduler benchmark: recorded fan-out read walks through the committed `ReadHub` under the committed W/F bound, walled against serial / hybrid / fibers / per-lane reads | `just session-fanout-probe` | default | **EXTERNAL CORPUS**: `SESSION_FANOUT_TRACE` defaults to `/tmp/fanout_trace.log`, absence is a hard failure | stdout; `SESSION_FANOUT_PLANT_DIVERGENCE=1` must exit non-zero | diagnostic — **compile-lane only** without the trace; real-frame scheduler behavior owned by `degenbot-simulation`'s `tests/async_fanout.rs` and `session_async.rs` |
| [`sim_mode_bench`](engine-lab/src/bin/sim_mode_bench/main.rs) | ADR-089 phase-6 offline engine benchmark: one corpus, engine modes (interp / jit-block / jit-pool / aot / replay / replay-jit), first-run vs steady-state latency, JIT compile telemetry, cross-mode verdict identity | `just bench-sim-modes [args]`; bounded smoke `SIM_BENCH_ITERS=10 just bench-sim-modes all` | `evm2-jit` | offline — committed fixture spine (real `cmd_executor` creation artifact + tier-3 stubs); `SIM_BENCH_ITERS` | stdout; the AOT mode's scratch store is a temp path | measurement — the decision instrument for the AOT/worker-pool tasks; every mode's verdict identity gated inside the run, never by a timing threshold |
| [`worker_fanout_probe`](engine-lab/src/bin/worker_fanout_probe/main.rs) | Per-worker EVM fan-out probe (ADR-089 concurrency item): serial shared `&mut evm` vs per-worker threads vs per-candidate fibers vs warm fibers, cross-leg verdict identity | `just worker-fanout-probe`; bounded smoke `WORKER_FANOUT_CANDIDATES=2 WORKER_FANOUT_WORKERS=2 COLD_MISS_LATENCY_US=1 WORKER_FANOUT_ACCOUNT_US=1` | default | offline — shape-matched corpus built in code; `WORKER_FANOUT_*`, `COLD_MISS_LATENCY_US`; optional recorded trace via `WORKER_FANOUT_TRACE` | stdout; `WORKER_FANOUT_PLANT_DIVERGENCE=1` must exit non-zero | measurement (non-gating) — the default 21-candidate sweep is a non-routine measurement; provenance `logs/bal_matched_parity_matrix.md` |
| [`hotpath_prometheus_probe`](engine-lab/src/bin/hotpath_prometheus_probe/main.rs) | Smallest end-to-end proof that a guard-enabled build serves profiler metrics on `:6772`: installs typed config, constructs the pump's guard, scrapes `/metrics`, asserts the families | `DEGENBOT_HOTPATH=1 cargo run -p degenbot-engine-lab --features prometheus --bin hotpath_prometheus_probe` | `prometheus` | offline but binds a local TCP port (`HOTPATH_PROMETHEUS_PORT`, default 6772); `DEGENBOT_HOTPATH=1` required | stdout; writes nothing; non-zero exit on any wiring breakage | diagnostic — its CI obligation is the Prometheus compile lane (`just check-dev-tools --engine-prometheus`); documented in `docs/grafana/HOTPATH.md` |

### `degenbot-stubgen` (1 row) — `stubgen/src/`

The FFI stub generator (ADR-066).

| Binary | Purpose | Current command | Lane | Inputs | Writes | Status |
| --- | --- | --- | --- | --- | --- | --- |
| [`degenbot-stubgen`](stubgen/src/main.rs) | Generates the committed `src/degenbot/_ffi/` stub tree from the introspection chunks embedded in a cdylib built with the binding layer's `experimental-inspect` feature (ADR-066 D1/D2); splices the typed `__getattr__` overload face from the config projection's declaration tables (ADR-066 D3) | `just gen-stubs [--check]` | default | the introspected cdylib that `just gen-stubs` builds with the temporary `experimental-inspect` toggle in its own `stubgen-inspect` target dir (never the canonical cache); the seam's introspection chunks | regenerates the committed `src/degenbot/_ffi/` stub tree; regeneration is deterministic, so two consecutive runs must be diff-empty; `--check` writes nothing and exits 1 on drift | **gate** — the stub-drift gate (`just lint-stub-drift`) runs the `--check` arm and fails closed on any FFI-crate doc/signature touch; a `pyo3` pin bump implies a generator bump and a fresh stub diff (ADR-066) |

## Owning-crate integration tests (2 rows)

Boot-level smoke assertions owned by their product crates: the assertions are the gate.

| Test | Source | Purpose | Command | Status |
| --- | --- | --- | --- | --- |
| `headless_boot` | [`degenbot-ingestion` tests](../crates/foundation/degenbot-ingestion/tests/headless_boot.rs) | Headless standalone-Rust boot over the `degenbot-ingestion` public surface with no Python and no network: synthetic fused event stream through the topic filter, watchdog windows, server-side-filterable `eth_getLogs` backfill filter | `just test-rust-nextest -p degenbot-ingestion -E 'binary(headless_boot)'`; second check of `just test-standalone` | **gate** — named assertions fail the run |
| `config_smoke_boot` | [`degenbot-bot` tests](../crates/engine/degenbot-bot/tests/config_smoke_boot.rs) | The lightest start-up path of the bot engine driven by a config TOML only: spawned child processes prove the `DEGENBOT_*` guard refuses a planted variable and the clean child loads, installs, and constructs `EngineStages::with_core_cfg` | `just test-rust-nextest -p degenbot-bot -E 'binary(config_smoke_boot)'`; gate: `just test-rust-nextest -p degenbot-bot` | **gate** — process-isolated by construction (the stance install is a process global) |

## Feature isolation — forwards, not package absence

Nine lanes, each its own `just check-dev-tools <lane>` cargo invocation (a bare invocation
runs `default` and prints the menu; a leading `--` is ignored):
`default`, `fixture-sql-ledger`, `solver-capture-fixtures`, `solver-cache-lab`,
`solver-cache-telemetry`, `solver-hotpath`, `solver-envelope`, `engine-jit`,
`engine-prometheus`. There is no combined lane and no `--all-features` form.

Four feature families must never be unified across tool packages:

| Feature family | Owner crate feature | Reaches a tool only through | Must NOT appear in |
| --- | --- | --- | --- |
| SQL ledger | `degenbot-db/sql-ledger` | `degenbot-fixture-tools`'s `sql-ledger` forward | `degenbot-solver-lab`, `degenbot-engine-lab` |
| Pool fixtures / cache lab | `degenbot-pools/capture-fixtures`, `degenbot-pools/cache-lab` | `degenbot-solver-lab`'s forwards | `degenbot-fixture-tools`, `degenbot-engine-lab` |
| JIT | `degenbot-simulation/evm2-jit` | `degenbot-engine-lab`'s `evm2-jit` forward | `degenbot-fixture-tools`, `degenbot-solver-lab` |
| Bot Prometheus exporter (`hotpath-prometheus`) | `degenbot-bot/hotpath-prometheus` | `degenbot-engine-lab`'s `prometheus` forward | `degenbot-fixture-tools`, `degenbot-solver-lab`, and every lane that is not the bot exporter |

`degenbot-solver-lab`'s `hotpath` lane is pool-solve function profiling
(`hotpath/hotpath`, `hotpath/threads`), **not** the bot exporter: it never enables
`hotpath/hotpath-prometheus`, never starts a `/metrics` listener, and never depends on
`degenbot-bot`. No doc may describe solver-lab as Prometheus-enabled or engine-lab as
owning pool profiling.

Isolation is a claim about **tool-owned feature forwards** — no lane enables a family
another tool package owns (`cargo tree -p <tool> --features <lane> -e features -i <owning
crate>` names the `(command-line)` chain). It is **not** a claim of resolved-package
absence, which is false if claimed: (a) `rusqlite` is a
non-optional `degenbot-db` dependency wherever that crate is reached (so the fixture
SQL-ledger lane still resolves it); (b) the `hotpath`
no-op stub is non-optional in `degenbot-pools`. Removing any of these is a product
dependency redesign, not a tool-surface change. The fixture lanes resolve no
`degenbot` facade package and no facade-forced feature: `degenbot-fixture-tools`'s
product edges are default-features edges, so the Linux target-scoped
`degenbot-simulation/evm2-jit` carriage (a facade dependency) is absent from both
fixture lanes, and `degenbot-executor-fixture-tools` carries no facade edge at all.

Every tool package is `publish = false`, outside `default-members` (`degenbot` and
`degenbot-cli` remain the only defaults), and reachable only by explicit `-p` selection.
Heavy binaries carry their own `[[bin]]` `required-features`, so a default check skips
each one visibly in the manifest; a binary that needs no feature declares none and is the
`default` lane's target (fixture-tools' four, executor-fixture-tools' one, solver-lab's
`synth_corpus_gen` and `stateview_feasibility_probe`, engine-lab's fourteen no-feature
probes). Each package's default tree is the concrete product graph its default binaries
read (the named product edges for fixture-tools, the production executor's graph for
executor-fixture-tools, the pools graph for solver-lab, the simulation graph for
engine-lab); the feature lane is the only difference between lanes.

## Boundaries

- **No subcommand UX and no production CLI.** Each tool keeps its own argv parsing; a
  unified `degenbot-tools <verb>` front end is separate product work.
  `aot_prepare_executor` stays a documented operational tool; promoting it into the
  shipped CLI is a product decision, not a leftover.
- **No attic, archive, or wrapper package.** A concluded instrument's conclusions move to
  `docs/` (or an ADR) and its code lives in Git history; the only survivors are tools
  whose numbers somebody still re-measures.
- **Gate/measurement posture is fixed.** No timing instrument gains a pass/fail threshold;
  no gate recipe may be demoted to run-by-hand. Live-only rows are verified by
  compilation, and no offline report may claim a refreshed measurement from them.
- **Fixtures are byte-stable.** Committed fixtures and goldens stay byte-identical
  outside an explicit, recorded regeneration; drift is a defect to report, never
  something to regenerate green.
- **Recipe names are the public interface.** `just` recipe names, default arguments, and
  pass-through behavior are preserved even when the command underneath changes; the
  recipe's comment travels with its command.
