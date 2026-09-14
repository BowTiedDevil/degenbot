# Rust settlement-bot consumer parity ledger

Epic: **RGZG4S** (ergo) — "Rust-owned settlement bot — consumer parity + gap discovery".

This document is the living census for the epic's core question: **can a
`cargo add degenbot` consumer build the same settlement-arbitrage bot that
`examples/eth_settlement_arbitrage_v2_v3_v4_rust.py` builds via the Python
driver (`src/degenbot/runner/`, ~4.8K lines) — with zero Python?** Per
AGENTS.md, Rust is the engine and Python is a driver shell; this ledger tests
that claim end-to-end instead of assuming it.

## Methodology (the gap-smoking loop)

The parity example (`rust/examples/settlement_bot`) is built **as** the test:

1. Port the next Python-driver phase against the umbrella crate's public
   surface and attempt to compile/run.
2. Where compilation or semantics fail, the failure is a **numbered gap**
   (G1..G5 below); each gap has an ergo task and a classified row in the
   ledger.
3. Close the gap in the core crates (never in the example), then advance.
4. Land the running dual-driver gate (RSP-8) so parity is continuously
   asserted, not re-audited.

The crate lives under `rust/examples/` (not `examples/rust/`) because cargo
rejects workspace members not hierarchically below the workspace root
("workspace member ... is not hierarchically below the workspace root").

Status vocabulary for the ledger:

- **REACHABLE** — a pyo3-off umbrella consumer reaches the same behavior today
  (evidence linked).
- **PARTIAL** — the leaf exists but the driver-level glue/semantics differ or
  are missing.
- **BLOCKED** — no public path exists (hard wall).
- **DRIVER-POLICY** — deliberately driver-owned ("stays-python" per
  `degenbot/runner` design); the Rust example reimplements it locally rather
  than the library owning it.

## Ledger

| # | Python-driver surface (source) | Rust status | Evidence / gap |
|---|---|---|---|
| 1 | CLI flags `--live/--permutation/--node-http/--node-ws` (`runner/cli.py`) | REACHABLE | reimplemented in `rust/examples/settlement_bot` (driver-owned by design: argparse ∩ std argv) |
| 2 | `ArbitrageConfig.from_env` — operator/executor envs, dispatch tunables, retry knobs, fail-fast parse errors (`runner/config.py`) | DRIVER-POLICY | reimplemented driver-side in the example (mirrors constants byte-for-byte); `degenbot-config`'s `BotConfigLoader` cascade does not cover these env keys by design |
| 3 | RPC URI cascade `resolve_rpc_uris`: CLI > `DEGENBOT_RPC_{HTTP,WS}_CHAINID_<id>` OS env > config.toml `rpc[id]`/`ws[id]` > error, no localhost default (`src/degenbot/config.py`) | DRIVER-POLICY | reimplemented in the example (toml read of `~/.config/degenbot/config.toml`) |
| 4 | DB path resolution (`_make_arbitrage_config`: config.toml `database.path` or `~/.config/degenbot/degenbot.db`) | DRIVER-POLICY | reimplemented in the example |
| 5 | DB snapshot load → seed block S (`Bot.load_snapshot_from_db`, `EngineRegistry.start` snapshot read) | REACHABLE | `Bot::new` + `load_snapshot_from_db` + `snapshot_seed_block` — proven by `standalone_consumer.rs` slice 7 (`fixture_snapshot_seed_block`) and by the example's boot slice |
| 6 | Engine handshake: `engine.subscribe(ws)` → first WS block W; set S on shared state; `resume()` with **auto-backfill S+1..W-1** inside the pump; stop/phase machine (`engine_registry.py`, `bot_runner.py`) | REACHED-via-EngineDriver | `degenbot::EngineDriver::start` (subscribe → verify-config, stops pre-`resume`) + `resume` (driver-owned `BlockPump::backfill_with_drain` = `S+1..W`, then spawns the live loop) + `stop` (any-phase, idempotent, terminal latch). Shipped by ergo **5XOGRK** (ADR-050) |
| 7 | Result-batch consumption (engine `__anext__` stream of `ResultBatch` per block) | REACHED-via-EngineDriver | `EngineDriver::take_result_receiver` hands out the unbounded `ResultBatch` receiver once (attach pre-`resume`); `stop` closes it so a pending recv sees end-of-stream. Shipped by ergo **5XOGRK** |
| 8 | Path registration `register_and_solve_path` (+ dedup, + path cap) | REACHED-via-EngineDriver | `EngineDriver::register_path`/`register_and_solve_path`/`deregister_path`/`set_path_cap`/`path_count`/`path_dedups` delegate to `EngineStages`; the typed `PathRegistrationError` propagates verbatim. Shipped by ergo **5XOGRK** |
| 9 | Registration verify lifecycles (quarantine → seed-verify → drain/pin → post-drain verify → live; sync + async entry points) | REACHABLE | The core lifecycles are re-exported by the umbrella and proven in `standalone_consumer.rs`; `EngineDriver::run_v3/v4_registration_lifecycle` (+ `_sync`) expose them driver-side (5XOGRK). The claim/TOCTOU discipline is now implemented driver-side under tokio in `rust/examples/settlement_bot/src/claims.rs` (`VerifyClaims`: claim-if-absent / await-if-present / release-on-failure), with the transient retry dance in `retry.rs` and the four memos + typed build-refusal classification in `ledger.rs` — ergo **XFEJUG**. Sweep **S4/S5/S7/S12**: the core still owes the key maps, claim policy, retry dance, and refusal taxonomy (LIFT-pending) |
| 10 | Pool construction from RPC (+ DB arm) | REACHABLE | umbrella re-exports `probe_pool_type` + `build_v2/build_v3/build_v4/build_erc20_metadata/build_aerodrome_v2/build_balancer_*/build_curve_pool`; proven in `standalone_consumer.rs` PoolBuilder slice |
| 11 | Candidate-pool enumeration from DB (`build_paths.py` discovery query) | **REACHED/VERIFIED** | `degenbot_db::discovery_read::DiscoveryPoolRow` + `DegenbotDb::fetch_discovery_rows` / `SnapshotDb::fetch_discovery_rows` (held-deferred-tx). One read-only SELECT per family (V2 `UNION ALL` over every V2 subclass table, V3 likewise, + the V4 `uniswap_v4_pools` × `managed_pools` × `pool_managers` join) carrying base `pools` fields, token0/token1 `erc20_tokens` (address+decimals), the `exchanges` row, and per-family fee/`tick_spacing`/Aerodrome `stable`/V4 `pool_hash`+`hooks`+`state_view` columns. Fixture evidence: `rust/crates/degenbot-db/tests/discovery_read_parity.rs` over `fixtures/parity.db` (chain 8453, aerodrome_v3 V3 + uniswap_v4 V4) plus the V2-stable unit test in `discovery_read.rs`; umbrella reach proven by the `settlement_bot` boot slice → Gap **G2**, ergo **YFIOSF** |
| 12 | Path discovery (`find_paths_async`, `discovery_batch_size` batching) | REACHABLE | `PathGraph`/`find_paths`/`OwnedPathFinder` reachable via the umbrella (`degenbot-pathfinding`); the driver-side batched wrapper (`batch_size<=1` per-path mode, bounded batches, one cooperative async hop per batch) is implemented in `rust/examples/settlement_bot/src/discovery.rs` over the umbrella iterator, with the candidate-token degree filter + V4 graph-id namespacing + `prune_dead_ends` mirroring `build_path_graph` — ergo **XFEJUG** |
| 13 | Path-composition policy (token allowlist, hop bounds, duplicate pool, permutation filter — `arbitrage/policy.py` + `ArbitrageConfig`) | DRIVER-POLICY | example-implemented in `rust/examples/settlement_bot/src/policy.rs` (hop bounds pinned to the discovery 2/3 floor/cap, allow/deny token sets, duplicate-pool guard, permutation parse + per-depth pool-kind filter) and applied as the discovery allowlist graph filter in `src/discovery.rs`; both Python sets are mirrored (the 11-token `config.py` field AND the 15-token `_driver_constants.ETH_MAINNET_ALLOWED_TOKENS` discovery set, which includes WETH) |
| 14 | Sim context + in-process sim (`SimulateContext`, overrides, 7-call bundle) | REACHABLE | `degenbot::arbitrage::{SimulateContext, SimulatePath, FailBuckets, simulate_in_process_with_db, compute_priority_fee}` + `degenbot::simulation::apply_simulation_overrides`; proven in `standalone_consumer.rs` sim slice; behavioral parity proven by the `inspector_cafebabe_revert` dual-driver fixture pair |
| 15 | Dispatch selection + encoding (`dispatch_profitable_results`, `DispatchCandidate`, `composers::PathInfo`, thin-margin filter) | REACHABLE | `degenbot::arbitrage::{dispatch_profitable_results, filter_thin_margin_results}` and `degenbot::cmd_executor::composers::*` re-exported (umbrella). Gap **G4** driver policy now lives in `rust/examples/settlement_bot/src/dispatch.rs`: `plan_batch` emits the typed `DispatchDecision` (skip-empty-hops / suppressed / thin-margin / sim) via `PathSuppression::is_suppressed` + the core thin-margin pre-filter; `run_sim_fanout` wraps the core fan-out. Ergo **L4E7RI** |
| 16 | Sim fan-out + ordered single submitter (`_sim_submit_pipeline.py`, `max_simulate_concurrent=50`) | DRIVER-POLICY → SPLIT (sweep **S9/S11**) | `rust/examples/settlement_bot/src/sim_submit.rs`: tokio `Semaphore(max_simulate_concurrent)` + single ordered FIFO submitter + the fail-loud `raise_if_failed` contract (unit-tested offline). `consume.rs` consumes `EngineDriver::take_result_receiver` per-block in order, closing end-of-stream exactly once on `stop()` (ADR-050 D6). Nothing lifted into the cores. Ergo **L4E7RI** |
| 17 | Fee determination: `eth_feeHistory` percentiles + `next_base_fee` (`runner/_consume.py`, `dispatch.fetch_fee_history`) | REACHABLE | `degenbot_core::eip_1559::next_base_fee`, `degenbot::arbitrage::compute_priority_fee`, `degenbot::rpc::fetch_priority_fee_percentiles` (`AlloyProvider::eth_fee_history`), and `degenbot::submission::fetch_fee_history` are all reachable through the umbrella; `dispatch.rs::priority_fee` wraps the fee seam (unit-tested). Ergo **L4E7RI** |
| 18 | Live submission: EIP-1559 sign + send + receipt monitor; dry-run guard that never signs | REACHABLE | `degenbot::submission::{TxSigner, dispatch_and_submit, monitor_pending_transaction, Dispatcher, PathSuppression, ReceiptProbe}` reachable through the umbrella. `submission.rs` owns the driver guard order (mutual-exclusivity / dry-run / inject-code) behind a `SubmissionSeam`; the live seam delegates to `dispatch_and_submit`. The dry-run path short-circuits before the seam — pinned by `submission.rs::tests::dry_run_never_reaches_the_seam`. Ergo **L4E7RI** |
| 19 | Session watch / stuck-loop watchdog + session-end verdict (`_session_watch.py`) | DRIVER-POLICY → SPLIT (sweep **S14**) | `rust/examples/settlement_bot/src/session_watch.rs`: typed `SessionEndVerdict` (`PumpEnded`/`RegistrationFailed`/`WatchdogTripped`, byte-for-byte) + `Heartbeat`/`stall_watchdog` supervising the live consume loop (watch-as-observer: cancels the consumer, never owns the process; the same-batch registration-over-watchdog ranking) — ergo **KPLWUM** |
| 20 | Operator Unix-socket channel (`add_path`/`discover`/`fleet_posture` — example + `operator_channel`) | DRIVER-POLICY | `rust/examples/settlement_bot/src/operator_channel.rs`: tokio `UnixListener` JSON-lines server honoring `--operator-socket`; `add_path`/`discover` through the driver `RegistrationPipeline`, `set`/`get_fleet_posture` through `degenbot::workers::posture::process` (reachable via the umbrella — no new gap), Python-compatible framing/response/error/unknown-op shapes, graceful `close()`; documented `--operator-inert` offline-serve mode — ergo **KPLWUM** |
| 21 | Process diagnostics: GIL probe, tracemalloc, faulthandler, /proc-mem sampler (`eth_settlement_arbitrage_v2_v3_v4_rust.py` startup) | DEPARTURE (documented) | Python-interpreter-specific by construction; the Rust example substitutes tokio/tracing-native equivalents. Not a parity item. |
| 22 | Logging/telemetry boot (`degenbot.logging`, telemetry facade) | REACHABLE | umbrella re-exports `telemetry`, `diag!`/`op_info!`/`op_warn!`/`op_error!`/`op_span!` |

## Gap inventory

- **G1 — engine driver exposure** (ergo **5XOGRK**, rows 6–8 + downstream 16): **CLOSED** by ADR-050. The public `degenbot::EngineDriver` (`degenbot_bot::arb_engine::EngineDriver`) composes the one public `EngineStages` seam with the pump session state; `ArbitrageEngine` stays `pub(crate)` (the one-door invariant). `EngineRegistry.start` + the `BotRunner` phase machine remain Python-side policy over the Rust-owned sequencing contract. The `PyArbEngine`/`PumpState` pair now delegates the ritual to the same driver.
- **G2 — DB discovery reads** (ergo **YFIOSF**, row 11): **CLOSED**. `degenbot-db` ships the additive, read-only `discovery_read` surface (`DiscoveryPoolRow` + `fetch_discovery_rows` / `fetch_discovery_rows_on_conn`) covering every column `build_paths.py`'s construction path reads, on the `SnapshotDb` held-deferred-tx handle; verified against the frozen `parity.db` fixture (V3 aerodrome_v3 + V4 uniswap_v4, chain 8453) and the umbrella `settlement_bot` example. The SQLAlchemy/Alembic layer is untouched (0.7 kill list stands). Balancer/Curve are outside the candidate graph by construction and are intentionally not enumerated.
- **G3 — discovery batching + registration pipeline** (ergo **XFEJUG**, rows 9, 12, 13 + claim TOCTOU): **CLOSED (driver-side, offline)**. `rust/examples/settlement_bot/src/` ships `discovery.rs` (graph build from the G2 discovery rows on the held-tx snapshot + batched lazy `OwnedPathFinder`), `policy.rs` (allowlist/hop-bounds/duplicate/permutation), `ledger.rs` (the four memos + typed build-refusal classification), `claims.rs` (tokio at-most-once verify claims), `retry.rs` (bounded RPC-only retry/backoff), `pipeline.rs` (the `_registration_unit` prep stages + offline-dry run + the claim/retry verification helper), and `live.rs` (the per-candidate `build_v2/v3/v4` → `BotState` registration → claim/retry verify lifecycle → `register_and_solve_path` arm, gated on `SMOKE_RPC_URL`), with 27 offline unit tests. The live arm is exercised only against a live node.
- **G4 — consume/dispatch/submission** (ergo **L4E7RI**, rows 15–18): **CLOSED (driver-side, offline)**. `rust/examples/settlement_bot/src/` ships `consume.rs` (per-block ordered result-batch consumption + `BlockClock` + single end-of-stream on driver stop), `dispatch.rs` (typed `DispatchDecision` planning + `priority_fee`/`next_base_fee` wrappers + the `classify_revert`/`FailureKind` taxonomy), `sim_submit.rs` (bounded `Semaphore` fan-out + single ordered FIFO submitter + fail-loud), and `submission.rs` (the dry-run-safe `SubmissionSeam` over `dispatch_and_submit` + the config-window `monitor_with_config` nonce-expiry accounting), with 22 offline unit tests. All RPC-bound arms compile but are only exercised against a live node; the two new reach claims (row 17 `eth_feeHistory`, row 18 `TxSigner`) are compile-verified through the umbrella and none required a new G-row. The example now depends on `alloy` directly for the `U256`/`Address`/`Bytes` value types those public seams name (recorded as a nuance, not a gap: the umbrella exposes the functions but not the primitive aliases).
- **G5 — session watch + operator channel + reconnect** (ergo **KPLWUM**, rows 19–20): **CLOSED (driver-side, offline)**. `session_watch.rs` ships the typed end-state verdict set + the heartbeat/stall watchdog wired over the live `EngineDriver` result-consumption loop, with the same-batch ranking and the observer-only cancellation discipline; `operator_channel.rs` ships the `--operator-socket` JSON-lines channel (the four ops, Python byte-compatible response shapes, unknown-op/error framing, graceful `close()`, plus the `--operator-inert` RPC-free serve mode the integration check drives). Fleet posture is reachable standalone through `degenbot::workers::posture::{process, PosturePolicyPatch}` (row 20 is DRIVER-POLICY, not a new G-row). The WS reconnect/abort-policy sub-item was not part of KPLWUM's landed slice (rows 19–20): the live arm keeps the existing `EngineDriver::start`/`stop` sequencing, and the SIGINT→stop→typed-consumer-report shutdown is covered by the inert mode + the live arm's `driver.stop()` ordering.
- **E2E running gate** (ergo **23DLCY**): **CLOSED (offline)**. The ledger is executable: the CI-safe fixture boot gate runs on both axes (Rust `boot_gate.rs` + Python `test_settlement_bot_boot_gate.py`) against the shared `fixtures/settlement_bot_boot.json` oracle, the recorded dual-driver decision diff (`dual_driver_gate.py` + `test_settlement_bot_dual_driver_gate.py`) diffs the Python/Rust streams modulo the documented permitted-divergence list, and seeded-divergence tests prove both comparators have teeth. The live anvil arm is wired behind `DEGENBOT_DUAL_DRIVER_GATE=1` + `DEGENBOT_FORK_RPC` (skip-by-default in CI). See [Running parity gate](#running-parity-gate-rsp-8-ergo-23dlcy).

## Running parity gate (RSP-8, ergo 23DLCY)

The ledger is executable. The gate has two CI-safe, offline halves and one
opt-in live half; the extractor contract is `grep '^parity-ledger row='`.

### 1. Fixture boot gate (offline, no RPC)

Shared oracle: `tests/standalone_parity/fixtures/settlement_bot_boot.json`.
Both consumers read the same JSON and must reproduce it:

- **Rust consumer** — `rust/examples/settlement_bot/tests/boot_gate.rs`
  shells the built example against
  `rust/crates/degenbot-db/tests/fixtures/parity.db` with `--smoke-offline`
  and parses the machine-checkable stdout.
- **Python consumer** — `tests/standalone_parity/test_settlement_bot_boot_gate.py`
  drives the PyO3 seams (`Bot.load_snapshot_from_db`, `build_path_graph`).

The machine-checkable contract is the boot report itself: the
`parity-ledger row=<id> status=<status> note=<note>` lines, the
`parity-ledger snapshot-seed-block S=<None|u64>` line, the
`[boot] discovery enumerated <n> candidate pools` line, the
`[g3] graph built: <n> nodes, <n> candidate tokens, <n> requested kinds [...]`
line, and the `[g3] offline-dry pipeline: key=value ...` line. The
consume/dispatch decision rows the gate pins are:

| Row | Pinned status | Decision contract |
|---|---|---|
| `06-engine-subscribe-resume` | `REACHED-via-EngineDriver` | `EngineDriver::start → subscribe → verify-config` (stops pre-resume); `resume` owns the `S+1..W` auto-backfill |
| `07-result-batch-stream` | `REACHED-via-EngineDriver` | `EngineDriver::take_result_receiver` (attach pre-resume); `ResultBatch` end-of-stream once on `stop` |
| `08-register-and-solve-path` | `REACHED-via-EngineDriver` | `EngineDriver::register_and_solve_path` delegates to `EngineStages` |
| `15-dispatch-selection` | `REACHABLE` | core `dispatch_profitable_results` / `filter_thin_margin_results` + driver `DispatchDecision` planning |
| `16-sim-fanout-submitter` | `DRIVER-POLICY` | `Semaphore(max_simulate_concurrent)` + single ordered FIFO submitter |
| `18-live-submission` | `REACHABLE` | dry-run seam never signs; live seam is `dispatch_and_submit` |

**Seeded-divergence proof (teeth).** The Rust test mutates one expected
ledger status in an in-memory copy of the oracle and asserts the comparator
fails; it also re-runs the real binary with the `DEGENBOT_DISCOVERY_CHAIN_ID`
seam removed (enumeration drops 2 → 0) and asserts the comparator catches the
live divergence. The Python test mutates `expected.snapshot_seed_block` and
`python_reachable.graph_nodes` in memory and asserts the real PyO3 decisions
do not match. The checked-in oracle is never modified.

### 2. Dual-driver decision diff (recorded; anvil opt-in)

`tests/standalone_parity/dual_driver_gate.py` diffs the Python driver's and
the Rust driver's decision streams against the recorded fixture
`tests/standalone_parity/fixtures/dual_driver_decisions.json`, modulo the
fixture's `permitted_divergence` list (currently `graph.candidate_tokens`:
the Rust boot applies the 15-token ETH-mainnet discovery allowlist while the
Python probe reads the unfiltered graph — the documented row-13 split). The
pytest half is `test_settlement_bot_dual_driver_gate.py`.

Live mode (`--live`) requires `DEGENBOT_DUAL_DRIVER_GATE=1` +
`DEGENBOT_FORK_RPC` (+ `DEGENBOT_FORK_BLOCK`): it starts
`anvil --fork-url ... --fork-block-number ...`, runs both drivers dry-run
against the pinned fork, and reads the per-batch decision streams named by
`DEGENBOT_DECISION_STREAM` (JSONL
`{block, path_id, decision}`), which are **not emitted by either driver
yet** — so live mode fails loudly on a missing stream rather than passing
silently. `--record` regenerates the recorded fixture from the offline
probes (no RPC).

### Invocation

- `just test-settlement-parity` — Rust boot gate + pytest gates + recorded diff.
- `uv run pytest tests/standalone_parity -q` — the standalone-parity axis.
- `DEGENBOT_DUAL_DRIVER_GATE=1 DEGENBOT_FORK_RPC=<rpc> DEGENBOT_FORK_BLOCK=<n> uv run python tests/standalone_parity/dual_driver_gate.py --live`
- `uv run python tests/standalone_parity/dual_driver_gate.py --record`
## Rust-ownership sweep (RSP-9 / ergo `IUGFLH`)

The horizontal census sibling to the vertical RSP-2..RSP-8 slices. Every
Python-owned driver surface is classified as one of:

- **LIFT** — the core owns it once; both the pure-Rust and the Python driver
  call in through the same seam.
- **KEEP-DRIVER** — it must remain host-side (asyncio loop ownership, SIGINT
  policy, OS/env cascade, display rendering).
- **SPLIT** — one named seam; the mechanism/core fact lifts, the policy or
  host binding stays.

`(landed)` marks a LIFT whose core implementation already exists (ADR-050 /
ergo `5XOGRK` plus the driver-side `XFEJUG` / `L4E7RI` / `KPLWUM` slices).
`(pending)` marks a LIFT decided here whose core work is not yet landed; see
[Lift follow-ups](#lift-follow-ups).

| # | Swept item (source) | Decision | Rationale | Consumers affected |
|---|---|---|---|---|
| S1 | `BotRunner._Phase` FSM (`New → Started → Running → Closed`) + `start()`/`run()` attach-consumer-before-`resume` sequencing (`runner/bot_runner.py`) | LIFT (landed) | ADR-050 D2/D7 moved the sequencing contract into `degenbot_bot::arb_engine::EngineDriver`; `BotRunner._Phase` is now a thin cockpit wrapper (config/SIGINT/trim policy) over the Rust-owned ritual, not a second implementation. | Python `BotRunner`; `rust/examples/settlement_bot`; ADR-050 |
| S2 | `EngineRegistry.start()` pre-pump ritual (S-read → subscribe → verify-config, stops pre-`resume`) (`arbitrage/engine_registry.py`) | LIFT (landed, `5XOGRK`) | ADR-050 D2: `EngineDriver::start` owns subscribe + verify-config; ledger rows 6–8. `EngineRegistry.start` is now driver-side policy over the same ritual. | `BotRunner.start`; `EngineDriver` |
| S3 | Startup backfill/resume ordering (auto-backfill S+1..W, single result-batch gate) | LIFT (landed, `5XOGRK`) | ADR-050 D2/D6: `EngineDriver::resume` awaits `BlockPump::backfill_with_drain` then spawns the live loop; the consumer attaches the receiver between `start` and `resume`. | `BotRunner.run`; `consume.rs` |
| S4 | Address→`pool_id` key maps (`_v2_keys`/`_v3_keys`/`_v4_keys`, `knows_pool`) (`engine_registry.py`) | LIFT (pending, `XFEJUG`) | Pool identity is already core-owned in the shared `BotState`; the Python maps are a convenience cache the file itself flags "plain dicts — NOT thread-safe". The core should expose derived lookups so drivers stop mirroring identity. | `EngineRegistry`; `build_paths`; dispatch/encode path |
| S5 | `VerifyClaims` at-most-once policy + claim tables (`arbitrage/_claims.py`; `claims.rs`) | LIFT (pending, `XFEJUG`) | The policy has two full twins (Python asyncio/threading adapters; the example's tokio `claims.rs`). The core owns the policy + table once; each driver keeps a thin wake adapter (asyncio.Future / threading.Event / tokio Notify). | `EngineRegistry`; `build_paths`; `claims.rs` |
| S6 | `register_path` pre-checks: pool-registered guard vs `path_predicate.evaluate` | SPLIT | The guard is LIFT-landed — the core's `register_path` rejects a `pool_id` not in the `BotState` (`register_path_rejects_pool_id_not_in_bot`), so the Python key-map `KeyError` path can retire. `path_predicate` is deployment policy (ADR-006 D7KMQO) and stays KEEP-DRIVER. | `EngineRegistry.register_path`; `_registration_unit` |
| S7 | Verification retry dance (bounded retry-with-backoff loop) (`arbitrage/verification_retry.py`; `retry.rs`) | LIFT (pending, `XFEJUG`) | Policy values are config (S8), but the dance itself is a twin: `retry_verification_call` and the example's `retry.rs`. Promote `retry.rs` into a core (beside `VerifyError`) with the policy injected. | `build_paths`; `retry.rs`; `XFEJUG` |
| S8 | `VerificationRetryPolicy.from_env` knob values (`VERIFICATION_RETRY_*`) | KEEP-DRIVER | Deployment tuning (attempts/backoff/jitter); each driver parses its own env and injects the value into the lifted dance (S7). | `ArbitrageConfig`; example config |
| S9 | Sim fan-out bounded-concurrency mechanism (`_sim_submit_pipeline.py`; `sim_submit.rs`) | LIFT (pending, `L4E7RI`) | `degenbot-workers` already owns the generalized bounded fleet: the `SimDriver` role, bounded per-role queues, and the posture-derived `sim_intake_cap` (`FleetHost`). The per-driver `Semaphore` is a second sizing mechanism; the fleet host should own it. | `_sim_submit_pipeline`; `consume`; `FleetHost` |
| S10 | Sim fan-out policy numbers (`max_simulate_concurrent=50`, `DEGENBOT_SIM_PIPELINE_CONCURRENCY`) | KEEP-DRIVER | Config knobs; the fleet sizes its seats from typed config and drivers may pass a cap. | `ArbitrageConfig`; `SimSubmitPipeline` |
| S11 | Ordered single submitter (FIFO fan-in; one nonce fetch per submit) (`_sim_submit_pipeline.py`; `sim_submit.rs`) | LIFT (pending, `L4E7RI`) | The ordering invariant is needed by every driver (submission order = nonce order). The bounded host models sim concurrency but not an ordered submit lane; the core/fleet should own the FIFO fan-in. | `_sim_submit_pipeline`; `sim_submit.rs` |
| S12 | Registration build-refusal taxonomy (`RegistrationOutcome`, `BuildRefusal`, `classify_build_refusal`) (`_registration_ledger.py`; `ledger.rs`) | LIFT (pending, `XFEJUG`) | A closed typed vocabulary already twinned in the example's `ledger.rs`; the precedent is `degenbot-decoders::revert::classify_revert`, core-owned and parity-tested. The core owns the taxonomy; the bounded metric tags become core constants. | `build_paths`; `ledger.rs` |
| S13 | Registration memos (`_registered_paths`/`_verified_pools`/`_unregistrable_pools`/`_rejected_paths`) + progress rendering | KEEP-DRIVER | Bookkeeping and display layered over core facts, shaped per pipeline instance. | `PathRegistrationPipeline`; `_render` |
| S14 | `_session_watch` verdicts (`SessionEndVerdict`, watch-set, ranking, teardown) | SPLIT | Stall/pump-finished detection is core-adjacent — the fleet already has the typed lane-death/fault posture (`FleetPosture`, `lane_death_held`) plus `EngineDriver::pump_finished`, so detection LIFTs. Verdict ranking, consumer-cancel ordering, and SIGINT-adjacent teardown stay KEEP-DRIVER. | `_session_watch`; `KPLWUM`; `FleetHost::observe_cause` |
| S15 | Config cascades (`ArbitrageConfig.from_env`, `resolve_rpc_uris`, DB path) | KEEP-DRIVER | OS/env/TOML deployment policy; `degenbot-config` is the typed Rust loader but does not own these arbitration keys (ledger rows 2–4). No ADR assigns Rust a shared env-file contract. | `runner/config.py`; `BotRunner`; example |
| S16 | Process diagnostics (GIL probe, tracemalloc, faulthandler, `/proc` mem sampler) | KEEP-DRIVER | Python-interpreter-specific by construction (audit ledger DEPARTURE row 21); the Rust driver substitutes tracing-native equivalents. | example startup |
| S17 | SIGINT binding + shutdown ordering | SPLIT | The stop-before-cancel ordering contract is LIFT-landed (ADR-050 D6: `EngineDriver::stop` closes the channels before the consumer cancels); the signal-handler binding is process/OS policy and stays KEEP-DRIVER. | `BotRunner`; `EngineDriver` |
| S18 | Result-batch consumption loop + `BlockClock` + end-of-stream (`_consume.py`; `consume.rs`) | SPLIT | The receiver contract (hand out once; close on `stop` so `recv()` sees end-of-stream exactly once) is LIFT-landed via `EngineDriver::take_result_receiver` (ADR-050 D3/D6). The per-block dispatch loop + clock stay driver. | `consume_result_batches`; `consume.rs` |
| S19 | Pool build + registration lifecycle ordering (`_registration_unit`) | SPLIT | Builds and the ADR-022 verify choreography are core-owned/reachable, and the sync lifecycles sit on `EngineDriver`; pipeline orchestration, retry-policy injection, and the memo policy stay driver. | `PathRegistrationPipeline`; `EngineDriver::run_v3/v4_registration_lifecycle_sync` |
| S20 | Nonce expiry accounting (`blocks_before_nonce_expires` window) | KEEP-DRIVER (mechanism landed) | The window accounting is already core-owned (`degenbot_submission::monitor_pending_transaction`); only the block-window value is config. Consistency correction to any reading of ledger row 18 as driver-owned. | `degenbot-submission`; `submission.rs` |
| S21 | Operator Unix-socket channel (`add_path`/`discover`/`fleet_posture`) | KEEP-DRIVER | Wire protocol + host deployment surface (ledger row 20); the underlying `degenbot-workers` posture API is already reachable. | `operator_channel.rs`; `BotRunner.enqueue_path`/`trigger_discovery` |
| S22 | Dispatch selection/encoding policy (candidate shaping, thin-margin, suppression) | KEEP-DRIVER | The sim/submit arithmetic and taxonomy leaves are core-owned; only candidate-list shaping + display rendering remain (ledger row 15). | `_dispatch`; `dispatch.rs` |
| S23 | Pool-cache trim / `release_python_state` | KEEP-DRIVER | Python-object-lifetime concern with no Rust counterpart (ADR-050 D8). | `BotRunner._trim_python_state` |
| S24 | DB snapshot load + V3 tracker pre-population (`get_snapshots`) | SPLIT | The engine's DB snapshot load is core-owned/landed (`Bot::load_snapshot_from_db`, ledger row 5); the V3 `UniswapV3PoolTracker` pre-population is Python-construction scaffolding and stays driver. | `get_snapshots`; `ConstructionContext` |

### Lift follow-ups

The pending LIFTs (S4, S5, S7, S9, S11, S12, and the S14 detection half)
are noted on their consuming done tasks so a later implementing task can pick
them up; no new task was opened by this census.

- **S4 key maps** → `XFEJUG` (core-derived address→`pool_id` lookups).
- **S5 `VerifyClaims`** → `XFEJUG` (core claim policy + table; wake adapters stay per-driver).
- **S7 retry dance** → `XFEJUG` (promote `retry.rs` into a core; inject the S8 policy).
- **S9/S11 sim fan-out + ordered submitter** → `L4E7RI` (fleet `SimDriver` ownership + an ordered submit lane; `degenbot-workers`).
- **S12 registration taxonomy** → `XFEJUG` (core typed refusal vocabulary, parity-checked like `classify_revert`).
- **S14 stall detection** → `KPLWUM` (fleet `FleetPosture`/`EngineDriver::pump_finished` owns detection; ranking stays driver).

### RSP-1 ledger designation deltas

The sweep refines three RSP-1 rows; the rest stand. No row remains
`BLOCKED` after its lift landed (all lift-landed rows above cite their
ADR-050 / task evidence).

| Ledger row | Before | After |
|---|---|---|
| 9 (verify lifecycles / claim TOCTOU) | `REACHABLE` driver-side | `REACHABLE` + LIFT-pending **S4/S5/S7/S12** (`XFEJUG`) |
| 16 (sim fan-out + ordered submitter) | `DRIVER-POLICY` | `DRIVER-POLICY → SPLIT` **S9/S11** (fleet mechanism; policy stays) |
| 19 (session watch verdicts) | `DRIVER-POLICY` | `DRIVER-POLICY → SPLIT` **S14** (detection lifts; ranking stays) |

Supersedure: the two recorded "stays-python" statements that this sweep
reverses in part are (a) `runner/bot_runner.py`'s module docstring, which now
carries an ADR-050 pointer beside the doctrine, and (b) the epic `5TBT7L` Q2b
crate-private-engine note in `CONTEXT.md` ("Engine seam deepening"), which now
carries a one-line ADR-050 supersedure. The `pub(crate)` one-door invariant
itself stands — ADR-050 adds the `EngineDriver` *driver* seam above
`EngineStages`, not a second engine door.

## Guardrails

- AGENTS.md 0.7 kill list: nothing in this epic may delete/stub
  `src/degenbot/migrations`, the alembic/sqlalchemy deps,
  `DatabaseSessionManager`, `ALEMBIC_HEAD`, the `ensure_schema` Alembic branch,
  or the `query_only` pragma. All Rust-side DB work is **additive, read-only**.
- Strategy semantics stay the shared contract: `classify_revert` labels and the
  7-call bundle are compared via fixtures (per-leaf dual-driver parity tests
  exist, see `tests/standalone_parity/`); the running gate compares
  *decisions*, not log bytes.
