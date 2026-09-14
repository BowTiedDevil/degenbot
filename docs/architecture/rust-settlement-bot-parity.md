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
| 9 | Registration verify lifecycles (quarantine → seed-verify → drain/pin → post-drain verify → live; sync + async entry points) | REACHABLE | The core lifecycles are re-exported by the umbrella and proven in `standalone_consumer.rs`; `EngineDriver::run_v3/v4_registration_lifecycle` (+ `_sync`) expose them driver-side (5XOGRK). The claim/TOCTOU discipline is now implemented driver-side under tokio in `rust/examples/settlement_bot/src/claims.rs` (`VerifyClaims`: claim-if-absent / await-if-present / release-on-failure), with the transient retry dance in `retry.rs` and the four memos + typed build-refusal classification in `ledger.rs` — ergo **XFEJUG** |
| 10 | Pool construction from RPC (+ DB arm) | REACHABLE | umbrella re-exports `probe_pool_type` + `build_v2/build_v3/build_v4/build_erc20_metadata/build_aerodrome_v2/build_balancer_*/build_curve_pool`; proven in `standalone_consumer.rs` PoolBuilder slice |
| 11 | Candidate-pool enumeration from DB (`build_paths.py` discovery query) | **REACHED/VERIFIED** | `degenbot_db::discovery_read::DiscoveryPoolRow` + `DegenbotDb::fetch_discovery_rows` / `SnapshotDb::fetch_discovery_rows` (held-deferred-tx). One read-only SELECT per family (V2 `UNION ALL` over every V2 subclass table, V3 likewise, + the V4 `uniswap_v4_pools` × `managed_pools` × `pool_managers` join) carrying base `pools` fields, token0/token1 `erc20_tokens` (address+decimals), the `exchanges` row, and per-family fee/`tick_spacing`/Aerodrome `stable`/V4 `pool_hash`+`hooks`+`state_view` columns. Fixture evidence: `rust/crates/degenbot-db/tests/discovery_read_parity.rs` over `fixtures/parity.db` (chain 8453, aerodrome_v3 V3 + uniswap_v4 V4) plus the V2-stable unit test in `discovery_read.rs`; umbrella reach proven by the `settlement_bot` boot slice → Gap **G2**, ergo **YFIOSF** |
| 12 | Path discovery (`find_paths_async`, `discovery_batch_size` batching) | REACHABLE | `PathGraph`/`find_paths`/`OwnedPathFinder` reachable via the umbrella (`degenbot-pathfinding`); the driver-side batched wrapper (`batch_size<=1` per-path mode, bounded batches, one cooperative async hop per batch) is implemented in `rust/examples/settlement_bot/src/discovery.rs` over the umbrella iterator, with the candidate-token degree filter + V4 graph-id namespacing + `prune_dead_ends` mirroring `build_path_graph` — ergo **XFEJUG** |
| 13 | Path-composition policy (token allowlist, hop bounds, duplicate pool, permutation filter — `arbitrage/policy.py` + `ArbitrageConfig`) | DRIVER-POLICY | example-implemented in `rust/examples/settlement_bot/src/policy.rs` (hop bounds pinned to the discovery 2/3 floor/cap, allow/deny token sets, duplicate-pool guard, permutation parse + per-depth pool-kind filter) and applied as the discovery allowlist graph filter in `src/discovery.rs`; both Python sets are mirrored (the 11-token `config.py` field AND the 15-token `_driver_constants.ETH_MAINNET_ALLOWED_TOKENS` discovery set, which includes WETH) |
| 14 | Sim context + in-process sim (`SimulateContext`, overrides, 7-call bundle) | REACHABLE | `degenbot::arbitrage::{SimulateContext, SimulatePath, FailBuckets, simulate_in_process_with_db, compute_priority_fee}` + `degenbot::simulation::apply_simulation_overrides`; proven in `standalone_consumer.rs` sim slice; behavioral parity proven by the `inspector_cafebabe_revert` dual-driver fixture pair |
| 15 | Dispatch selection + encoding (`dispatch_profitable_results`, `DispatchCandidate`, `composers::PathInfo`, thin-margin filter) | REACHABLE | `degenbot::arbitrage::{dispatch_profitable_results, filter_thin_margin_results}` and `degenbot::cmd_executor::composers::*` re-exported (umbrella). Gap **G4** driver policy now lives in `rust/examples/settlement_bot/src/dispatch.rs`: `plan_batch` emits the typed `DispatchDecision` (skip-empty-hops / suppressed / thin-margin / sim) via `PathSuppression::is_suppressed` + the core thin-margin pre-filter; `run_sim_fanout` wraps the core fan-out. Ergo **L4E7RI** |
| 16 | Sim fan-out + ordered single submitter (`_sim_submit_pipeline.py`, `max_simulate_concurrent=50`) | DRIVER-POLICY | `rust/examples/settlement_bot/src/sim_submit.rs`: tokio `Semaphore(max_simulate_concurrent)` + single ordered FIFO submitter + the fail-loud `raise_if_failed` contract (unit-tested offline). `consume.rs` consumes `EngineDriver::take_result_receiver` per-block in order, closing end-of-stream exactly once on `stop()` (ADR-050 D6). Nothing lifted into the cores. Ergo **L4E7RI** |
| 17 | Fee determination: `eth_feeHistory` percentiles + `next_base_fee` (`runner/_consume.py`, `dispatch.fetch_fee_history`) | REACHABLE | `degenbot_core::eip_1559::next_base_fee`, `degenbot::arbitrage::compute_priority_fee`, `degenbot::rpc::fetch_priority_fee_percentiles` (`AlloyProvider::eth_fee_history`), and `degenbot::submission::fetch_fee_history` are all reachable through the umbrella; `dispatch.rs::priority_fee` wraps the fee seam (unit-tested). Ergo **L4E7RI** |
| 18 | Live submission: EIP-1559 sign + send + receipt monitor; dry-run guard that never signs | REACHABLE | `degenbot::submission::{TxSigner, dispatch_and_submit, monitor_pending_transaction, Dispatcher, PathSuppression, ReceiptProbe}` reachable through the umbrella. `submission.rs` owns the driver guard order (mutual-exclusivity / dry-run / inject-code) behind a `SubmissionSeam`; the live seam delegates to `dispatch_and_submit`. The dry-run path short-circuits before the seam — pinned by `submission.rs::tests::dry_run_never_reaches_the_seam`. Ergo **L4E7RI** |
| 19 | Session watch / stuck-loop watchdog + session-end verdict (`_session_watch.py`) | DRIVER-POLICY | example-side tokio watchdog — ergo **KPLWUM** |
| 20 | Operator Unix-socket channel (`add_path`/`discover`/`fleet_posture` — example + `operator_channel`) | DRIVER-POLICY | example-side tokio `UnixListener` — ergo **KPLWUM** |
| 21 | Process diagnostics: GIL probe, tracemalloc, faulthandler, /proc-mem sampler (`eth_settlement_arbitrage_v2_v3_v4_rust.py` startup) | DEPARTURE (documented) | Python-interpreter-specific by construction; the Rust example substitutes tokio/tracing-native equivalents. Not a parity item. |
| 22 | Logging/telemetry boot (`degenbot.logging`, telemetry facade) | REACHABLE | umbrella re-exports `telemetry`, `diag!`/`op_info!`/`op_warn!`/`op_error!`/`op_span!` |

## Gap inventory

- **G1 — engine driver exposure** (ergo **5XOGRK**, rows 6–8 + downstream 16): **CLOSED** by ADR-050. The public `degenbot::EngineDriver` (`degenbot_bot::arb_engine::EngineDriver`) composes the one public `EngineStages` seam with the pump session state; `ArbitrageEngine` stays `pub(crate)` (the one-door invariant). `EngineRegistry.start` + the `BotRunner` phase machine remain Python-side policy over the Rust-owned sequencing contract. The `PyArbEngine`/`PumpState` pair now delegates the ritual to the same driver.
- **G2 — DB discovery reads** (ergo **YFIOSF**, row 11): **CLOSED**. `degenbot-db` ships the additive, read-only `discovery_read` surface (`DiscoveryPoolRow` + `fetch_discovery_rows` / `fetch_discovery_rows_on_conn`) covering every column `build_paths.py`'s construction path reads, on the `SnapshotDb` held-deferred-tx handle; verified against the frozen `parity.db` fixture (V3 aerodrome_v3 + V4 uniswap_v4, chain 8453) and the umbrella `settlement_bot` example. The SQLAlchemy/Alembic layer is untouched (0.7 kill list stands). Balancer/Curve are outside the candidate graph by construction and are intentionally not enumerated.
- **G3 — discovery batching + registration pipeline** (ergo **XFEJUG**, rows 9, 12, 13 + claim TOCTOU): **CLOSED (driver-side, offline)**. `rust/examples/settlement_bot/src/` ships `discovery.rs` (graph build from the G2 discovery rows on the held-tx snapshot + batched lazy `OwnedPathFinder`), `policy.rs` (allowlist/hop-bounds/duplicate/permutation), `ledger.rs` (the four memos + typed build-refusal classification), `claims.rs` (tokio at-most-once verify claims), `retry.rs` (bounded RPC-only retry/backoff), `pipeline.rs` (the `_registration_unit` prep stages + offline-dry run + the claim/retry verification helper), and `live.rs` (the per-candidate `build_v2/v3/v4` → `BotState` registration → claim/retry verify lifecycle → `register_and_solve_path` arm, gated on `SMOKE_RPC_URL`), with 27 offline unit tests. The live arm is exercised only against a live node.
- **G4 — consume/dispatch/submission** (ergo **L4E7RI**, rows 15–18): **CLOSED (driver-side, offline)**. `rust/examples/settlement_bot/src/` ships `consume.rs` (per-block ordered result-batch consumption + `BlockClock` + single end-of-stream on driver stop), `dispatch.rs` (typed `DispatchDecision` planning + `priority_fee`/`next_base_fee` wrappers + the `classify_revert`/`FailureKind` taxonomy), `sim_submit.rs` (bounded `Semaphore` fan-out + single ordered FIFO submitter + fail-loud), and `submission.rs` (the dry-run-safe `SubmissionSeam` over `dispatch_and_submit` + the config-window `monitor_with_config` nonce-expiry accounting), with 22 offline unit tests. All RPC-bound arms compile but are only exercised against a live node; the two new reach claims (row 17 `eth_feeHistory`, row 18 `TxSigner`) are compile-verified through the umbrella and none required a new G-row. The example now depends on `alloy` directly for the `U256`/`Address`/`Bytes` value types those public seams name (recorded as a nuance, not a gap: the umbrella exposes the functions but not the primitive aliases).
- **G5 — session watch + operator channel + reconnect** (ergo **KPLWUM**, rows 19–20).
- **E2E running gate** (ergo **23DLCY**): the ledger becomes executable — fixture boot smoke + recorded/anvil dual-driver comparison, with a seeded-divergence assertion proving the comparator has teeth.

## Guardrails

- AGENTS.md 0.7 kill list: nothing in this epic may delete/stub
  `src/degenbot/migrations`, the alembic/sqlalchemy deps,
  `DatabaseSessionManager`, `ALEMBIC_HEAD`, the `ensure_schema` Alembic branch,
  or the `query_only` pragma. All Rust-side DB work is **additive, read-only**.
- Strategy semantics stay the shared contract: `classify_revert` labels and the
  7-call bundle are compared via fixtures (per-leaf dual-driver parity tests
  exist, see `tests/standalone_parity/`); the running gate compares
  *decisions*, not log bytes.
