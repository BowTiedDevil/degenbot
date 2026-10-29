# Capability catalog

One page answering, for each capability: where it lives, what its door items
are, which consumer's task proves it (C5), and what a user of that capability
must **not** have to pull. Decided by
[ADR-084](adr/ADR-084-capability-homes.md) (D1): one capability, one home, one
door. The homes below are pinned by
`architecture_gates.rs::capability_catalog_covers_every_public_crate` — every
publishable crate appears at least once, every path resolves to real code, and
every umbrella alias lands on a catalogged crate.

Consumers: **(i)** `cargo add degenbot` · **(ii)** `cargo add degenbot-pools` ·
**(iii)** Python driver (`pip install degenbot`) · **(iv)** `ExecutionAdapter`
implementor · **(v)** console operator (`degenbot`).

| Capability | Rust home | Python name | Door items | C5 consumer · probe | Must not pull |
| --- | --- | --- | --- | --- | --- |
| **Configuration** | `degenbot_config` | `degenbot.config` | crate root (`Config`) | (i) typed config load in `examples/settlement_bot`; consumer probe (v) `degenbot config show --resolved` (W8) | no tokio, no alloy, no pyo3 |
| **Engine session & workers** | `degenbot_workers` | Rust-only | crate root (`WorkerRole`, `FleetBudget`, telemetry) | (i) pure-Rust consumer in `examples/settlement_bot` | no pyo3; no C-build dep |
| **Core runtime & telemetry** | `degenbot_core` | Rust-only | `runtime`, `telemetry`, `block_clock_pipe`, `retry` | (i) pure-Rust consumer in `examples/settlement_bot` | no pyo3; no C-build dep |
| **Pool builder & registration lifecycle** | `degenbot_bot::bot_core::pool_builder` | Rust-only | `build_v2`, `build_v3`, `build_v4`, `probe_pool_type`, `ConstructionVerdict`, `PoolBuilderError`, `degenbot_pools::sim::registration_refusal::SpecRefusal` | (i) `run_registration_lifecycle` used by the pure-Rust driver | no pyo3 in `pools`; the builder owns no family match arms (ADR-078) |
| **Pool state & swap simulation** | `degenbot_pools::families` | Rust-only | `sim::registry::PoolEntry`, `simulate_swap`, family read modules (ADR-073) | (ii) pools-path solve probe: `just check-consumer-probes` (`solve_path_probe` example) | no tokio, no registry/db, no pyo3; no C-build dep outside `capture-fixtures` |
| **Multi-hop solve** | `degenbot_pools::solve` | Rust-only | `solve_path_with_min_profit`, `SolveOutcome`, `GateDeps`, `WalkMemo` (the W2 door, pinned by `solve_door_census.txt`) | (ii) pools-path solve probe: `just check-consumer-probes`; production caller `bot` `solve_cycle` | no pyo3, no db, no async runtime; fixture I/O gated behind `capture-fixtures` |
| **CL walk memo & telemetry** | `degenbot_pools::solve::concentrated_liquidity` | Rust-only | `WalkMemo`, `WalkTelemetry`, `PreparedConcentratedLiquidityHop` | production: `bot` lane walk + telemetry | no I/O; telemetry shapes pinned by the pipeline census (ADR-083) |
| **Path search** | `degenbot_pathfinding` | Rust-only | crate root | production: `bot` path registry | no tokio, no db, no pyo3 |
| **Arbitrage orchestration** | `degenbot_arbitrage` | Rust-only | crate root | production: engine stage consumers | no pyo3 |
| **The engine** | `degenbot_bot` | `degenbot.bot` (driver shell) | crate root; `arb_engine` is internal | (i) pure-Rust consumer: `examples/settlement_bot` | engine owns orchestration, not family math (ADR-073); no solver math duplication |
| **Shared substrate (state owner)** | `degenbot_substrate` | Rust-only | `BotState`, `PoolLookup`, `planning` | (i) `examples/settlement_bot` drives through it | the state owner carries no strategy policy (ADR-003) |
| **Strategy layer & adapter seam inputs** | `degenbot_strategy` | Rust-only | `CmdExecutorAdapter`, `ExecutionContext` | (iv) `ExecutionAdapter` implementor in `examples/degenbot-execution-sample` | no pyo3 |
| **Execution adapter seam** | `degenbot_execution` | `degenbot._ffi.execution` | crate root (`ExecutionAdapter` contract side) | (iv) `examples/degenbot-execution-sample` implements the seam | no C-build dep, no pyo3, no tokio of its own |
| **Batch execution** | `degenbot_batch_executor` | Rust-only | crate root | production: submission path | no pyo3 |
| **Submission** | `degenbot_submission` | `degenbot._ffi.submission` | crate root | production: engine submit stage | no pyo3 |
| **Transaction simulation** | `degenbot_simulation` | `degenbot._ffi.simulation` | crate root | production: sim gate | no pyo3 |
| **Log decoding** | `degenbot_decoders` | Rust-only | crate root | production: ingestion + updaters | no db, no async runtime |
| **Log ingestion plane** | `degenbot_ingestion` | Rust-only | crate root | production: `substrate`/`bot` consumers | foundation-resident (W4): nothing above the engine depends down |
| **RPC transport** | `degenbot_rpc` | Rust-only | crate root | production: every chain-facing consumer | no pyo3; no C-build dep outside `capture-fixtures` |
| **Persistence** | `degenbot_db` | `degenbot._ffi.db` | crate root | production: updaters; Aave update in pure Rust | no pyo3 |
| **Run artifacts** | `degenbot_runs` | Rust-only | crate root | production: session logging | std-only |
| **Aave updater** | `degenbot_aave` | `degenbot.aave` | `run_aave_update` | (i) `cargo add degenbot` consumer runs `run_aave_update` with no Python | no pyo3 |
| **Standalone pool updater** | `degenbot_pool_updater` | Rust-only | crate root | production: Aave-shaped update in pure Rust | foundation-resident (W4) |
| **Fork management** | `degenbot_fork` | `degenbot._ffi.fork` | crate root | production: fork lanes | no pyo3 |
| **External command executor** | `degenbot_executor` | `degenbot._ffi.executor` | crate root | production: strategy `CmdExecutorAdapter` | no pyo3 |
| **Price feeds** | `degenbot_price` | `degenbot._ffi.price` | crate root | production: engine pricing | no pyo3 |
| **Event hub** | `degenbot_eventhub` | Rust-only | crate root | production: engine event fan-out | no pyo3 |
| **Worker fleet** | `degenbot_workers` | `degenbot._ffi.fleet` | crate root (`WorkerRole`, `FleetBudget`) | production: engine resource posture (ADR-042) | no pyo3 |
| **Arithmetic leaves** | `degenbot_math` | `degenbot._ffi.v2_math` (+ family math modules) | crate root | every pool/solve consumer | no I/O, no async, no pyo3 |
| **Uniswap family encoding & DEX identity** | `degenbot_uniswap::dex_identity` | `degenbot._ffi.dex_identity` | `DexIdentity`, `DexVariant`, `preset_for_variant` | production: every V2/V3 path | no pyo3 |
| **ABI encoding** | `degenbot_abi` | `degenbot._ffi.abi` | crate root | production: submission/decoders | no pyo3 |
| **Orderbook index** | `degenbot_order_index` | Rust-only | crate root | production: orderbook consumers | one minimal default-features-off dep |
| **The umbrella capability map** | `degenbot` | `degenbot` (the Python package) | the 54 `pub use degenbot_*` aliases + curated items | (i) `cargo add degenbot`; (iii) Python driver probe: `just check-consumer-probes` (`test_solve_surface_probe.py`) | aliases only; no new logic |
| **The console** | `degenbot_cli` + `degenbot_cli_core` | `degenbot` console shim (`uv run degenbot`) / native binary | `Command` tree, `config show --resolved` | (v) `degenbot config show --resolved` — asserted in `just check-consumer-probes` | the CLI is a thin caller of `degenbot_cli_core::run_args` |

## Notes

- "Rust-only" rows have no Python binding; the binding surface is generated
  under `degenbot._ffi` (see `rust/crates/shells/degenbot-python`).
- The C5 probes run under `just check-consumer-probes` (also part of
  `just test`): the pools-path solve example, the Python FFI solve probe, and
  the console's offline `config show --resolved` task.
- **W5 (ADR-084/W5a+W5b):** the `degenbot-pools` root surface is now the four
  capability modules - `state` / `families` / `sim` / `solve` - with the
  umbrella-grade re-exports (`TickInfo`, the storage-slot encoders, the seam
  traits) kept at the crate root. The executed mapping is pinned by
  `rust/crates/facade/degenbot/tests/pools_root_surface_census.txt` (the
  phase-0 34-row record is preserved in the ADR-084 plan). ADR-080's
  concentrated-liquidity state module lives at
  `families/concentrated_liquidity/` with `v3_state`/`v4_state` as its
  storage adapters; the shared state verbs live once in
  `state_journal`/`state_seeding`/`state_walk` over the
  `JournalHost`/`WalkHost` seams (the ADR README's decisions table briefly
  lagged at ADR-074, which mis-gated W5b on a landed ADR).
- The door discipline (ADR-084 D2): `pub` items are a curated surface, the
  trees beneath are `pub(crate)` unless a named consumer needs them, and a
  type reachable through a door item's public fields is itself a door item.
  The solve tree's door and census land with W2
  (`solve_door_census.txt`).
- The dependency charter (ADR-084 D3) is the "must not pull" column turned
  into a gate: `just check-dependency-charter` (ratchet #7).
- Breaking path cuts ship with a "was → is" migration table — the ADR-084
  solve-door cut's table is in [rust/CHANGELOG.md](rust/CHANGELOG.md).
