# Architecture Decision Records

ADRs record the architecture-level decisions behind the Rust core. **The crate
sources are the last word on what the code does today** — an ADR records *why* a
decision was made, not a binding spec of what must be preserved. When an ADR and
the code disagree, the code is right and the ADR is stale: update the ADR's
status header rather than preserve the old choice.

New ADRs are proposed and reviewed through [ergo](https://github.com/sandover/ergo)
tasks and committed via their own review flow.

## The north star

One Rust core, two thin drivers: a pure-Rust consumer (`cargo add degenbot`) and
a Python driver shell (`pip install degenbot`) over the same engine. Decisions
that keep Python-side machinery or transitional seams alive past their recorded
lifetime are drag, not fidelity — the Historical section below exists so those
choices stay readable as provenance without binding new work.

## How to read a status

- **accepted / implemented** — live; safe to design against.
- **proposed** — recorded, not yet ratified or built.
- **historical / superseded** — provenance only. Do not implement from these,
  do not preserve what they describe, and do not re-litigate them; each names
  the ADR that replaced it.

## Start here

The load-bearing decisions behind the two-consumer architecture:

- {doc}`ADR-005-polars-inspired-three-layer-architecture` — why a Rust engine with thin drivers (naming convention superseded by ADR-032)
- {doc}`ADR-003-botcore-state-layer` — where all pool/token state lives (names have moved; see the header note)
- {doc}`ADR-019-in-process-revm-sole-simulation-executor-strategy-engine-separation` — the sole simulation executor + strategy/engine separation
- {doc}`ADR-041-block-epoch-pipeline` — the per-block pipeline (one stage machine over a cheap-read data plane; supersedes the ADR-008/027/028 pump machinery)
- {doc}`ADR-025-execution-strategy-seam` — how user code plugs into execution
- {doc}`ADR-050-rust-native-engine-driver` — the public Rust driver seam over the stage surface

## Current decisions

| ADR | Title | Status |
|---|---|---|
| [001](ADR-001-io-free-pools.md) | I/O-Free Pool Architecture | accepted (principle permanent; callback mechanism replaced — see header note) |
| [002](ADR-002-pool-type-registry-singleton.md) | Pool Type Registry as Module-Level Singleton | accepted (legacy-in-place — see direction note) |
| [003](ADR-003-botcore-state-layer.md) | BotCore as the state layer, peer to ArbitrageEngine | accepted (names have moved — see header note) |
| [004](ADR-004-cl-tickmap-typed-boundary.md) | Typed TickMap boundary for CL verifier + liquidity-apply seam | accepted |
| [005](ADR-005-polars-inspired-three-layer-architecture.md) | Polars-Inspired Three-Layer Architecture | accepted (naming superseded by ADR-032) |
| [006](ADR-006-bot-as-per-chain-orchestrator.md) | Bot as the per-chain orchestrator | implemented (partially superseded by ADR-041/ADR-047; orchestrator rows stand) |
| [007](ADR-007-pool-unregister-seam.md) | Pool unregister seam | accepted |
| [009](ADR-009-single-source-of-truth-versioning.md) | Single-Source-of-Truth Versioning | accepted |
| [012](ADR-012-spec-bound-pool-admission.md) | Spec-Bound Pool Admission Contract | accepted |
| [013](ADR-013-ffi-seam-is-private.md) | The `_ffi` Seam Is Private (Pydantic Barrier) | accepted |
| [014](ADR-014-pool-state-deepening-layer.md) | Pool-State Deepening — Where the Trait Seams Live | accepted |
| [015](ADR-015-solver-seam-relocation.md) | Solver-Seam Relocation — the Resolve→Solve Boundary | accepted |
| [016](ADR-016-reorg-pool-state-trait.md) | ReorgPoolState — Pool-Owned Reorg Rollback | accepted |
| [017](ADR-017-forward-apply-pool-state-traits.md) | Forward-Apply Pool-State Traits | accepted |
| [018](ADR-018-tracked-debt-bot-core-solvers-fusion.md) | Tracked Debt — `bot_core`↔`solvers` Fusion | accepted (debt tracking) |
| [019](ADR-019-in-process-revm-sole-simulation-executor-strategy-engine-separation.md) | In-Process revm as the Sole Simulation Executor | accepted |
| [020](ADR-020-tier3-onchain-accuracy-oracle.md) | Tier-3 On-Chain Accuracy Oracle | accepted |
| [022](ADR-022-registration-verify-lifecycle-core-ownership.md) | Registration verify-lifecycle is core-owned | accepted |
| [023](ADR-023-construction-io-retirement-disposition.md) | `PyBotIo` end-state disposition | accepted (disposition upgraded by its Post-adoption: finish the collapse) |
| [024](ADR-024-net-profit-order-index.md) | Net-profit order index (`degenbot-order-index`) | accepted |
| [025](ADR-025-execution-strategy-seam.md) | The `ExecutionAdapter` seam | accepted |
| [026](ADR-026-backrun-to-settlement-arbitrage-terminology.md) | Retire the "backrun" label — "settlement arbitrage" | accepted |
| [029](ADR-029-executor-command-grammar-axes.md) | The executor command grammar | accepted |
| [030](ADR-030-derivation-outcome-tri-state.md) | The derivation outcome is a tri-state | accepted |
| [031](ADR-031-executor-plan-walker.md) | The executor grammar as a facts-driven Plan walker | accepted; implemented |
| [032](ADR-032-pyclass-python-naming.md) | `#[pyclass]` Python naming convention | accepted |
| [033](ADR-033-encode-funnel-intake-contract.md) | The encode funnel's intake contract | accepted |
| [034](ADR-034-erc6909-vault-capture-wiring.md) | ERC6909-vault profit capture wiring | accepted |
| [035](ADR-035-math-consolidation-and-umbrella-aliases.md) | Consolidate the AMM math family into `degenbot-math` | accepted |
| [036](ADR-036-window-guards-not-relanded.md) | Do not re-land the V3/V4 window guards | accepted |
| [037](ADR-037-swap-simulation-gate.md) | The swap-simulation gate | accepted |
| [038](ADR-038-cl-event-routing-fsm.md) | The CL event-routing FSM | accepted |
| [039](ADR-039-state-lock-sim-anchor-projection.md) | State-lock — enumerated sim-anchor projection | accepted |
| [040](ADR-040-per-bucket-failure-reactions.md) | Failure reactions are per-bucket | accepted |
| [041](ADR-041-block-epoch-pipeline.md) | The block-epoch pipeline — one stage machine over a cheap-read data plane | implemented |
| [042](ADR-042-role-switching-worker-fleet.md) | One role-switching worker fleet | proposed (sign-off satisfied; fleet-core may begin) |
| [043](ADR-043-observability-standard.md) | The observability standard | accepted |
| [044](ADR-044-fleet-intake-liveness.md) | Fleet intake liveness — BackstopTick + PostureEdge | accepted |
| [045](ADR-045-solve-cycle-extraction.md) | The solve cycle as a deep module | accepted |
| [046](ADR-046-stage-handlers-pump-control-split.md) | StageHandlers / PumpControl split | accepted |
| [047](ADR-047-retire-subscriber-bus.md) | Retire the ADR-006 D4 subscriber bus | accepted (D4 reaffirmed and enforced by ADR-087) |
| [048](ADR-048-fleet-boot-registry.md) | Fleet boot registry | accepted |
| [049](ADR-049-engine-stage-driver-seam.md) | The engine's interface is one stage seam | accepted |
| [050](ADR-050-rust-native-engine-driver.md) | A public `EngineDriver` is the Rust driver seam | accepted |
| [051](ADR-051-rust-owned-console.md) | The degenbot console is a Rust binary | accepted |
| [052](ADR-052-db-auto-upgrade-alembic-retirement.md) | The database upgrades itself at open — Alembic retires in-tree | accepted (executed; supersedes ADR-010/011) |
| [054](ADR-054-strategy-plumbing-surface.md) | The strategy plumbing surface | accepted |
| [055](ADR-055-pending-tx-strategy-seams.md) | Pending-transaction strategy seams | accepted |
| [056](ADR-056-retire-gated-serving-seam.md) | Retire the gated serving seam | accepted |
| [057](ADR-057-strategy-host.md) | The strategy host — one process, many drivers | accepted |
| [058](ADR-058-strategy-crate-split.md) | The strategy crate split | accepted |
| [059](ADR-059-pool-family-kernel.md) | The pool family kernel | accepted |
| [060](ADR-060-collapse-v2-walk-representation-vs-interface.md) | Collapsing the V2 walk dispatch | accepted (Option B shipped; Option A refuted by probe) |
| [061](ADR-061-pool-ingress-plane-capability.md) | Pool-state provisioning is a plane capability | accepted (placement reopened by ADR-067) |
| [062](ADR-062-one-operator-file-four-layers.md) | One operator file, four layers | accepted |
| [063](ADR-063-config-env-interpolation.md) | `${env:NAME}` expansion for string-valued config keys | proposed |
| [064](ADR-064-session-object-registry-cutover.md) | The session object registry is the cutover | accepted |
| [065](ADR-065-verdict-single-configuration-authority.md) | The verdict is the single configuration authority | accepted |
| [066](ADR-066-deterministic-stub-generation.md) | Deterministic stub generation | accepted (supersedes ADR-053) |
| [067](ADR-067-strategy-substrate-home.md) | The strategy substrate gets one home | accepted |
| [068](ADR-068-updater-golden-capture-replay.md) | Golden capture replay is the updater coherence harness | proposed |
| [069](ADR-069-prepared-hop-entry-surface.md) | The prepared hop becomes one value | accepted |
| [070](ADR-070-updaters-are-the-sole-db-writers.md) | Updaters are the sole DB writers — the `writer` cargo fence, read-only opens, path-only schema init | accepted |
| [071](ADR-071-family-modules-and-concentrated-liquidity-kernel.md) | Family modules and the Concentrated Liquidity Kernel — one module, one row, one enum arm supersedes the eight-site family change | accepted |
| [073](ADR-073-family-read-modules.md) | The family read module — one family, two homes, rows at the seams; `concentrated_liquidity` naming | accepted / implemented (landed across epic JE3DED; A5 amendment dissolved `degenbot-solvers`) |
| [074](ADR-074-push-gate-manifest.md) | The push-gate manifest — `just pre-push` delegates to prek.toml's pre-push entries | accepted |
| [075](ADR-075-mirrored-engine-surfaces-stay.md) | The mirrored engine surfaces stay — the driver/stages mirror is accepted, policed by the drift gates | accepted |
| [076](ADR-076-epoch-phases.md) | The epoch phases — `SolveCycle::run_epoch` decomposes into named phases behind one interface | accepted |
| [077](ADR-077-refusal-vocabulary.md) | The refusal vocabulary — one `RefusalKind` owned by the registration outcome ledger, and the eligibility gate moves core-side | accepted |
| [078](ADR-078-family-write-door.md) | The family write door — one `PoolRegistration` request, one verdict, family as data | accepted |
| [079](ADR-079-seat-host-split.md) | The seat host split — two seat models behind one host loop | accepted |
| [080](ADR-080-cl-state-module.md) | The concentrated-liquidity state module — the Concentrated Liquidity Kernel's state-side sibling | accepted / implemented (journals, bitmap seeding, and viability/cache extracted into `families/concentrated_liquidity/{state_journal,state_seeding,state_walk}` over the `JournalHost`/`WalkHost` seams; test suites merged at `state_fixture`) |
| [081](ADR-081-one-pool-lookup.md) | One pool lookup at the state seam | accepted |
| [082](ADR-082-family-in-the-value.md) | Family lives in the value, not the name | accepted |
| [083](ADR-083-pipeline-census-event-vocabulary.md) | The pipeline census — one event vocabulary behind one dispatch door | accepted |
| [084](ADR-084-capability-homes.md) | Capability homes — doors, charters, one home per capability | accepted |
| [085](ADR-085-construction-request-one-door.md) | Pool construction is one family-tagged request at both tiers | accepted |
| [086](ADR-086-provider-one-contract-two-adapters.md) | Provider sync/async — one contract, two call-convention adapters | accepted |
| [087](ADR-087-deletion-is-the-retirement.md) | Deletion is the retirement — retirement tripwires rejected | accepted (reaffirms and enforces ADR-047 D4) |

## Historical — provenance only

These ADRs record decisions that have been executed past or replaced. Read them
for why the code looks the way it does; do not implement from them, preserve
what they describe, or re-litigate them.

| ADR | Title | Superseded by |
|---|---|---|
| [008](ADR-008-block-state-machine.md) | Per-block state machine for the pump's block clock | [ADR-041](ADR-041-block-epoch-pipeline.md) (folded into the one `StageMachine`) |
| [010](ADR-010-alembic-retention-and-rust-schema-cutover.md) | Alembic Retention Through 0.6.x and Rust Schema Cutover | [ADR-052](ADR-052-db-auto-upgrade-alembic-retirement.md) (executed early; the kill list is void) |
| [011](ADR-011-auto-healed-alembic-retirement.md) | Auto-healed Alembic Retirement (Dump-and-Restore Cutover) | [ADR-052](ADR-052-db-auto-upgrade-alembic-retirement.md) (heal shipped at open; gating moot) |
| [021](ADR-021-solver-state-accuracy-is-a-fail-fast-tripwire.md) | Solver-state accuracy is a fail-fast tripwire | [ADR-040](ADR-040-per-bucket-failure-reactions.md) (reaction) + [ADR-041](ADR-041-block-epoch-pipeline.md) §4 (in-process tripwire deleted; only the Published-edge RPC check survives) |
| [027](ADR-027-block-pump-dispatch-seam.md) | The block-pump dispatch seam | [ADR-041](ADR-041-block-epoch-pipeline.md) (dispatch layer retired) |
| [028](ADR-028-block-pump-pumpdecision-seam.md) | The block-pump `PumpDecision` seam | [ADR-041](ADR-041-block-epoch-pipeline.md) (`PumpFSM` folded into the `StageMachine`) |
| [037](ADR-037-engine-mutex-sharding.md) | Engine Mutex sharding (RAYPAR) | [ADR-041](ADR-041-block-epoch-pipeline.md) (no `drain_lock`; engine `Mutex` off the solve path) |
| [053](ADR-053-ffi-stub-generation.md) | FFI Stub Generation — hand-maintained `.pyi` set | [ADR-066](ADR-066-deterministic-stub-generation.md) (stubs are generated; only the `pyo3-stub-gen` rejection stands) |

## Numbering note

ADR-037 was assigned twice (engine mutex sharding; swap-simulation gate) — both
kept as filed. The mutex-sharding one is historical; the swap-simulation gate is
live. Cite by slug, not number.

```{toctree}
:hidden:
:maxdepth: 1
:glob:

ADR-*
```
