# Architecture Decision Records

ADRs record the architecture-level decisions behind the Rust core once they are considered settled (accepted / implemented / proposed); the crate sources remain the last word on what the code actually does today. New ADRs are proposed and reviewed through [ergo](https://github.com/sandover/ergo) tasks and committed via their own review flow — a table row is added when the file lands.

## Start here

The load-bearing decisions behind the two-consumer architecture:

- {doc}`ADR-005-polars-inspired-three-layer-architecture` — why a Rust engine with a thin Python driver
- {doc}`ADR-003-botcore-state-layer` — where all pool/token state lives
- {doc}`ADR-019-in-process-revm-sole-simulation-executor-strategy-engine-separation` — the sole simulation executor + strategy/engine separation
- {doc}`ADR-008-block-state-machine` — the pump's block clock
- {doc}`ADR-025-execution-strategy-seam` — how user code plugs into execution

## Index

| ADR | Title | Status |
|---|---|---|
| [001](ADR-001-io-free-pools.md) | I/O-Free Pool Architecture | accepted |
| [002](ADR-002-pool-type-registry-singleton.md) | Pool Type Registry as Module-Level Singleton | accepted |
| [003](ADR-003-botcore-state-layer.md) | BotCore as the state layer, peer to ArbitrageEngine | accepted |
| [004](ADR-004-cl-tickmap-typed-boundary.md) | Typed TickMap boundary for CL verifier + liquidity-apply seam | accepted |
| [005](ADR-005-polars-inspired-three-layer-architecture.md) | Polars-Inspired Three-Layer Architecture | accepted |
| [006](ADR-006-bot-as-per-chain-orchestrator.md) | Bot as the per-chain orchestrator | implemented |
| [007](ADR-007-pool-unregister-seam.md) | Pool unregister seam | accepted |
| [008](ADR-008-block-state-machine.md) | Per-block state machine for the pump's block clock | implemented |
| [009](ADR-009-single-source-of-truth-versioning.md) | Single-Source-of-Truth Versioning | accepted |
| [010](ADR-010-alembic-retention-and-rust-schema-cutover.md) | Alembic Retention Through 0.6.x and Rust Schema Cutover | accepted |
| [011](ADR-011-auto-healed-alembic-retirement.md) | Auto-healed Alembic Retirement (Dump-and-Restore Cutover) | **proposed** |
| [012](ADR-012-spec-bound-pool-admission.md) | Spec-Bound Pool Admission Contract | accepted |
| [013](ADR-013-ffi-seam-is-private.md) | The `_ffi` Seam Is Private (Pydantic Barrier) | accepted |
| [014](ADR-014-pool-state-deepening-layer.md) | Pool-State Deepening — Where the Trait Seams Live | accepted |
| [015](ADR-015-solver-seam-relocation.md) | Solver-Seam Relocation — the Resolve→Solve Boundary | accepted |
| [016](ADR-016-reorg-pool-state-trait.md) | ReorgPoolState — Pool-Owned Reorg Rollback | accepted |
| [017](ADR-017-forward-apply-pool-state-traits.md) | Forward-Apply Pool-State Traits | accepted |
| [018](ADR-018-tracked-debt-bot-core-solvers-fusion.md) | Tracked Debt — `bot_core`↔`solvers` Fusion | accepted |
| [019](ADR-019-in-process-revm-sole-simulation-executor-strategy-engine-separation.md) | In-Process revm as the Sole Simulation Executor | accepted |
| [020](ADR-020-tier3-onchain-accuracy-oracle.md) | Tier-3 On-Chain Accuracy Oracle | accepted |
| [021](ADR-021-solver-state-accuracy-is-a-fail-fast-tripwire.md) | Solver-state accuracy is a fail-fast tripwire | accepted |
| [022](ADR-022-registration-verify-lifecycle-core-ownership.md) | Registration verify-lifecycle is core-owned | accepted |
| [023](ADR-023-construction-io-retirement-disposition.md) | `PyBotIo` end-state disposition | accepted |
| [024](ADR-024-net-profit-order-index.md) | Net-profit order index (`degenbot-order-index`) | accepted |
| [025](ADR-025-execution-strategy-seam.md) | The `ExecutionStrategy` seam | accepted |
| [026](ADR-026-backrun-to-settlement-arbitrage-terminology.md) | Retire the "backrun" label — "settlement arbitrage" | accepted |
| [027](ADR-027-block-pump-dispatch-seam.md) | The block-pump dispatch seam | accepted |
| [028](ADR-028-block-pump-pumpdecision-seam.md) | The block-pump `PumpDecision` seam | accepted |
| [029](ADR-029-executor-command-grammar-axes.md) | The executor command grammar | accepted |
| [030](ADR-030-derivation-outcome-tri-state.md) | The derivation outcome is a tri-state | accepted |
| [031](ADR-031-executor-plan-walker.md) | The executor grammar as a facts-driven Plan walker | accepted; implemented |
| [032](ADR-032-pyclass-python-naming.md) | `#[pyclass]` Python naming convention | accepted |
| [033](ADR-033-encode-funnel-intake-contract.md) | The encode funnel's intake contract | accepted |
| [034](ADR-034-erc6909-vault-capture-wiring.md) | ERC6909-vault profit capture wiring | accepted |
| [035](ADR-035-math-consolidation-and-umbrella-aliases.md) | Consolidate the AMM math family into `degenbot-math` | accepted |
| [036](ADR-036-window-guards-not-relanded.md) | Do not re-land the V3/V4 window guards | accepted |
| [037](ADR-037-engine-mutex-sharding.md) | Engine Mutex sharding (RAYPAR) | accepted |
| [037](ADR-037-swap-simulation-gate.md) | The swap-simulation gate | accepted |
| [038](ADR-038-cl-event-routing-fsm.md) | The CL event-routing FSM | accepted |
| [039](ADR-039-state-lock-sim-anchor-projection.md) | State-lock — enumerated sim-anchor projection | pending status |
| [040](ADR-040-per-bucket-failure-reactions.md) | Failure reactions are per-bucket | accepted |

Numbering note: ADR-037 was assigned twice (engine mutex sharding; swap-simulation gate) — both kept as filed.

```{toctree}
:hidden:
:maxdepth: 1
:glob:

ADR-*
```
