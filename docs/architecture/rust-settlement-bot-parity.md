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
| 6 | Engine handshake: `engine.subscribe(ws)` → first WS block W; set S on shared state; `resume()` with **auto-backfill S+1..W-1** inside the pump; stop/phase machine (`engine_registry.py`, `bot_runner.py`) | **BLOCKED (G1)** | `ArbitrageEngine` is `pub(crate)` (`arb_engine/mod.rs`); `BlockPump::subscribe`/`resume_from_subscribe` are public but require `Arc<dyn StageHandlers>` + `Arc<dyn PumpControl>` + `ReorgCoordinator`, i.e. the engine itself. Gap → ergo **5XOGRK** |
| 7 | Result-batch consumption (engine `__anext__` stream of `ResultBatch` per block) | **BLOCKED (G1)** | `ResultBatch` type is public (`arb_engine/mod.rs`) but nothing public *produces* it for a consumer — ergo **5XOGRK** |
| 8 | Path registration `register_and_solve_path` (+ dedup, + path cap) | **BLOCKED (G1)** | engine method, crate-private — ergo **5XOGRK** |
| 9 | Registration verify lifecycles (quarantine → seed-verify → drain/pin → post-drain verify → live; sync + async entry points) | PARTIAL | `run_cl_v3_lifecycle`/`run_cl_v4_lifecycle`/`run_v3_registration_lifecycle`/`run_v4_registration_lifecycle` re-exported by the umbrella and proven in `standalone_consumer.rs` (`registration_lifecycle_standalone_slice`); the *engine-side* `run_v3/v4_registration_lifecycle` entry points are G1. The claim/TOCTOU discipline (`VerifyClaims`) is driver-policy to be redone under tokio — ergo **XFEJUG** |
| 10 | Pool construction from RPC (+ DB arm) | REACHABLE | umbrella re-exports `probe_pool_type` + `build_v2/build_v3/build_v4/build_erc20_metadata/build_aerodrome_v2/build_balancer_*/build_curve_pool`; proven in `standalone_consumer.rs` PoolBuilder slice |
| 11 | Candidate-pool enumeration from DB (`build_paths.py` SQLAlchemy query) | PARTIAL | SQLAlchemy layer is 0.7-kill-list-protected; `degenbot-db` has pool reads (`snapshot.rs`, `read.rs`) but the discovery enumeration surface is unverified for all families → Gap **G2**, ergo **YFIOSF** |
| 12 | Path discovery (`find_paths_async`, `discovery_batch_size` batching) | PARTIAL | `PathGraph`/`find_paths`/`OwnedPathFinder` reachable via umbrella (`degenbot-pathfinding`); the *async batched driver wrapper* is Python-side → Gap **G3**, ergo **XFEJUG** |
| 13 | Path-composition policy (token allowlist, hop bounds, duplicate pool, permutation filter — `arbitrage/policy.py` + `ArbitrageConfig`) | DRIVER-POLICY | example-implemented; `allowed_intermediate_tokens` list mirrored from `config.py` |
| 14 | Sim context + in-process sim (`SimulateContext`, overrides, 7-call bundle) | REACHABLE | `degenbot::arbitrage::{SimulateContext, SimulatePath, FailBuckets, simulate_in_process_with_db, compute_priority_fee}` + `degenbot::simulation::apply_simulation_overrides`; proven in `standalone_consumer.rs` sim slice; behavioral parity proven by the `inspector_cafebabe_revert` dual-driver fixture pair |
| 15 | Dispatch selection + encoding (`dispatch_profitable_results`, `DispatchCandidate`, `composers::PathInfo`, thin-margin filter) | REACHABLE | `degenbot::arbitrage::dispatch*` and `degenbot::cmd_executor::composers::*` re-exported (umbrella); suppression/categorization driver policy → Gap **G4**, ergo **L4E7RI** |
| 16 | Sim fan-out + ordered single submitter (`_sim_submit_pipeline.py`, `max_simulate_concurrent=50`) | DRIVER-POLICY (BLOCKED-ON-G1) | tokio `Semaphore` + ordered channel in the example; nothing to port into the library — but feeds from row 7's blocked stream → ergo **L4E7RI** |
| 17 | Fee determination: `eth_feeHistory` percentiles + `next_base_fee` (`runner/_consume.py`, `dispatch.fetch_fee_history`) | PARTIAL | `degenbot_core::eip_1559` exposed; `eth_feeHistory` standalone-RPC reachability unverified → verify inside ergo **L4E7RI** |
| 18 | Live submission: EIP-1559 sign + send + receipt monitor; dry-run guard that never signs | PARTIAL | `degenbot-submission` (receipt monitor) + core `eip_1559` re-exported; standalone signer (`TxSigner`) reachability unverified → verify inside ergo **L4E7RI** |
| 19 | Session watch / stuck-loop watchdog + session-end verdict (`_session_watch.py`) | DRIVER-POLICY | example-side tokio watchdog — ergo **KPLWUM** |
| 20 | Operator Unix-socket channel (`add_path`/`discover`/`fleet_posture` — example + `operator_channel`) | DRIVER-POLICY | example-side tokio `UnixListener` — ergo **KPLWUM** |
| 21 | Process diagnostics: GIL probe, tracemalloc, faulthandler, /proc-mem sampler (`eth_settlement_arbitrage_v2_v3_v4_rust.py` startup) | DEPARTURE (documented) | Python-interpreter-specific by construction; the Rust example substitutes tokio/tracing-native equivalents. Not a parity item. |
| 22 | Logging/telemetry boot (`degenbot.logging`, telemetry facade) | REACHABLE | umbrella re-exports `telemetry`, `diag!`/`op_info!`/`op_warn!`/`op_error!`/`op_span!` |

## Gap inventory

- **G1 — engine driver exposure** (ergo **5XOGRK**, blocks rows 6–8 and downstream 16): decide via ADR between making `ArbitrageEngine` public vs a public driver facade (`EngineDriver`) over the crate-private engine mirroring `EngineRegistry.start` + the `BotRunner` phase machine. *Note:* CONTEXT.md records the `pub(crate)` choice as deliberate (epic 5TBT7L Q2b); the ADR must resolve that tension against AGENTS.md's "Rust is the engine" claim.
- **G2 — DB discovery reads** (ergo **YFIOSF**, row 11).
- **G3 — discovery batching + registration pipeline** (ergo **XFEJUG**, rows 9, 12 + claim TOCTOU).
- **G4 — consume/dispatch/submission** (ergo **L4E7RI**, rows 15–18).
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
