# ADR-019: In-Process revm as the Sole Simulation Executor; Strategy-vs-Engine Separation

**Status: accepted (architecture).** This ADR records a cumulative decision
reached through an architecture review
(`/improve-codebase-architecture`, 2026-07-20) and a subsequent grilling
session. It supersedes the "two adapters" framing implicitly carried by the
simulation crates and retires the RPC simulation surface. Some code change
ships with the acceptance; the bulk is sequenced as a multi-step refactor
(see Sequencing). The vocabulary is recorded in `CONTEXT.md` under
"Simulation engine vs. searcher strategy."

## Context

degenbot is a library consumed by many searchers with different on-chain
strategies (backrun, sandwich, JIT-L, liquidation, …). The Rust core
therefore owns only the constrained pieces — the in-process representation of
pool/token state, the solver methods (value-only swap math), and a simulation
executor — while each searcher's **transaction encoding**, **profit-detection
strategy**, and **operator policy** are their own code, assembled at runtime
from the tools the core exposes.

Two historical facts collided to violate this separation:

1. **The simulation crates grew a full backrun strategy.**
   `degenbot-simulation::simulate_one` (the `eth_simulateV1` RPC path) and
   `degenbot-evm::simulate_path_on_evm` (the revm `transact_one` path) each
   carried a verbatim copy of the same backrun-example profit-detection
   strategy — the 3-pre-balance / `execute()` / 3-post-balance vector over
   WETH9 `balanceOf` / Multicall3 `getEthBalance` / PoolManager ERC6909
   `balanceOf`, the `decode_balance` helper, the gross/net profit arithmetic,
   the `compute_priority_fee` market-aware age-decay sizing, the `dispatch_profitable_results`
   fan-out + categorization + thin-margin + suppression policy. This is
   strategy code shaped by **one** example bot's funding model and executor
   contract (`examples/eth_backrun_v2_v3_v4_rust.py` + its Vyper
   `tstore_executor.vy`), not a universal simulation surface.

2. **The two crates existed as an accidental split.** The shared sim
   primitives (`SimResult`, `FailBuckets`, `compute_priority_fee`,
   `SimulateContext`, `SimulatePath`, `BlockPriorityFees`, the constants) had
   to pick one home and be re-exported to the other, producing a forbidden
   back-compat bridge (`pub use degenbot_evm::{10+ symbols}`) and exiling
   `calldata` to `degenbot-evm` only to break the dependency cycle the split
   itself created. This is the same "split across two crates by an accidental
   line" pattern ADR-015 resolved for the solver seam.

The architecture review surfaced the duplication as a Candidate-A deepening
("collapse the dual-driver 7-call orchestration behind one seam"). Grilling
then inverted that framing: the two `simulate_*` methods are not duplicated
*engine* code — they are duplicated *settlement-strategy* code misplaced in
engine crates. The dedup target is the **strategy layer** (extraction /
deletion), not the simulation core. The simulation engine itself stays
deliberately thin.

### The standalone-Rust-core claim

A searcher building a non-backrun bot (`cargo add degenbot`) must not have
the backrun example's 7-call bundle, its profit arithmetic, or its operator
policy baked into the simulation surface. Today they are — the engine crates
re-export them as the simulation interface. This strands the backrun
strategy across the future crate boundary and wedges every other strategy
into the example's shape, contrary to AGENTS.md's standalone-Rust-core
directive and the "no permanent Python responsibility" framing's spirit.

## Decision

### D1 — One simulation executor: in-process revm. RPC simulation retires.

The sole simulation executor is the in-process revm path: `BlockSimHandle`
over the `CacheDB<WarmCodeCache<BotStateDb<WrapDatabaseAsync<AlloyDB>>>`
stack. The following **retire**:

- `degenbot-simulation::dispatch::simulate_v1` — the `eth_simulateV1`
  dispatcher (a remote node executes the tx).
- `degenbot-simulation::dispatch::create_access_list` — the
  `eth_createAccessList` RPC (see D3 for the replacement).
- `degenbot-simulation::payload` — `build_simulate_payload`,
  `SimulationParams`, `SIM_CALL_COUNT` (the `eth_simulateV1` JSON payload is
  dead with no node sim).
- `degenbot-simulation::lib::build_simulation_state_overrides` — the Alloy
  `StateOverride` (`stateOverrides` JSON) builder. See D2.
- `degenbot-simulation::simulate_one` — the entire RPC-path orchestration.
  Its revm counterpart (`simulate_path_on_evm` / `BlockSimHandle::simulate_path`)
  is the sole surviving shape, and per D4 it moves to `examples/`.
- The legacy `None` arm of `dispatch_profitable_results` (the
  `buffer_unordered` RPC fan-out). The `Some(bot_state)` revm arm survives,
  in `examples/`, collapsed to revm-only.

This realizes the long-term goal (AGENTS.md: "Rust is the engine," in-process
sim as the production default; "minimize the use of external RPC I/O"). The
RPC surface survives **only** for cold-miss state fetches (`AlloyDB`
underneath the revm `DatabaseRef` stack) and the non-sim market primitive
in D5 — never for whole-transaction execution or access-list creation.

The two-adapter rule does **not** justify retaining an RPC-sim seam: there
is one adapter (revm). Anything RPC-shaped that survives is a *primitive
the revm path calls underneath*, not a peer simulation executor.

### D2 — `CacheDB` insertion is the sole state-override mechanism. `StateOverride` retires.

`apply_simulation_overrides` in `degenbot-evm/src/state_override.rs` —
`CacheDB::insert_account_storage` / `insert_account_info` with the
**explicit-balance-wins** merge (an existing balance is preserved; only
absent balances are filled from warmup; the WETH9 `balanceOf` slot IS
overwritten to the operational amount) — is the sole override mechanism.
The Alloy `StateOverride` JSON shape and its builder retire (D1).

The override **mechanism** is engine-generic (backing-agnostic `CacheDB`
insertion; the merge discipline is a general property of `CacheDB`, not the
backrun example). The override **params** —
`SimulationOverrideParams { owner, injected_address, runtime_bytecode,
warmup, weth_address, pool_manager_address }` + the funding amounts — are
**strategy-supplied**: the backrun example decides which addresses, which
slots, how much ETH; the engine renders them to `CacheDB` inserts.

### D3 — The access list is an in-process byproduct of execution. `eth_createAccessList` retires.

The access list is collected **in-realtime**, as a byproduct of execute(),
via a revm `Inspector` attached to the first `transact_one` run — warmed
SLOAD/SSTORE slots/users collected through the `Inspector` trait hooks
(`step` / `call` / `call_end`, verified available on `revm-inspector-41`) +
the `InspectEvm::inspect` API. This retires:

- `eth_createAccessList` (D1) — no remote node computes the AL.
- The post-re-`transact` `emit_access_list_from_state` path in
  `degenbot-evm/src/access_list.rs` as the **primary** AL source. (It remains
  available as an engine-generic primitive — emitting an AL from a `State`
  journal is a general capability — but it is no longer the production AL
  path; the Inspector on the first run is.)

The AL output crosses the engine→strategy seam: the engine produces the
warmed-slot set via the Inspector; the strategy decides whether and how to
attach it to the submitted transaction.

### D4 — Backrun strategy extracts to `examples/`. The engine stays thin.

The backrun-example strategy code — the 7-call pre/post balance bundle,
`decode_balance`, `compute_priority_fee` (the `TARGET_PROFIT_RATIO` /
age-decay sizing), `SimResult`, `SimulateContext`, `SimulatePath`, `FailBuckets`,
the int128 guard, `dispatch_profitable_results` (collapsed to revm-only per
D1) and its thin-margin / suppression / categorization policy — moves **out
of the engine crates** to `examples/` (alongside the existing
`examples/eth_backrun_v2_v3_v4_rust.py` driver).

No new crate is created for the strategy. `examples/` enforces the
engine-vs-strategy distinction more cheaply and more honestly than a crate:
an example file is self-evidently an example, not a core surface, and it
carries no Cargo / PyO3-wrapper / standalone-reachability surface to
maintain. The dedup of the two `simulate_*` copies resolves by **deletion**
(D1 retires the RPC copy; D4 moves the revm copy once) — exactly the
extraction-not-unification correction from grilling.

The engine exposes a deliberately thin surface that the example composes
from engine primitives:
- `BlockSimHandle::build(ctx, bot_state, warm_cache) -> handle` — the
  per-block shared EVM.
- the handle's generic per-call execution (execute a tx / a sequence of
  txs → per-call outcomes: status, gas, output, revert, touched state). No
  7-call hardcoding, no balance-decode, no bundling.
- `apply_simulation_overrides(&mut cache_db, params)` — the engine's
  override mechanism.
- `WarmCodeCache`, `BotStateDb`, `emit_access_list_from_state` — engine
  internals.
- the Inspector-collected AL output (D3).

The example re-assembles these "more manually" (the grilling caveat): it
constructs its own 7-call vector, decodes balances itself, sizes its own
priority fee, runs its own fan-out policy. This is the intentional cost of
not wedging all searchers into the backrun shape.

### D5 — `fetch_priority_fee_percentiles` moves to `degenbot-rpc`.

`dispatch::fetch_priority_fee_percentiles` (the `eth_feeHistory` RPC +
`parse_block_priority_fees`) is a generic market-data RPC primitive (fetch
block p10/p50 priority-fee samples), not simulation logic and not strategy.
After D1–D4 it is the only surviving non-sim content of `degenbot-simulation`.
It moves to `degenbot-rpc` (the typed RPC surface crate, which already owns
`AlloyProvider`, `EthBlock`, the typed block fetchers). A sandwich or
liquidation searcher wanting the same market oracle reaches it there, not
through a backrun-shaped simulation crate.

### D6 — `degenbot-simulation` absorbs `degenbot-evm`; the re-export bridge retires.

After D1's retirements + D4's extraction + D5's leaf-move,
`degenbot-simulation` is empty of original content. `degenbot-evm` folds
in: the merged crate reuses the name `degenbot-simulation` ("simulation"
describes the domain; "evm" describes one implementation — naming the
umbrella after an implementation adapter is the same shallowness removed
from the code). The revm adapter + its DB stack + `apply_simulation_overrides`
+ `WarmCodeCache` + the AL emitter become an internal `sim/evm` submodule
of the merged crate.

The `pub use degenbot_evm::{10+ symbols}` re-export bridge
(`degenbot-simulation/src/lib.rs`) retires — it existed only because the
pipeline's two halves were placed in two crates and had to share a type
home. AGENTS.md forbids a backwards-compatibility layer for retired
implementations; the "eventual dispatch swap" the comments cited as
justification has shipped (in-process sim is the default). The bridge
outlived its reason.

### D7 — PyO3 surface decomposes into engine primitives. `dispatch_profitable_py` retires.

The PyO3 wrapper retires the monolithic `dispatch_profitable_py`
`#[pyfunction]` (and `PyDispatchCandidate` / `PyDispatchOutcome` /
`PySimulateContext`), which today bundles fan-out policy + suppression +
thin-margin + decode + priority-fee sizing + categorization into one opaque
Rust call from Python — exactly the shape that wedged the strategy into the
engine. In its place the engine exposes primitive `#[pyfunction]` /
`#[pyclass]` wrappers the Python driver composes: a `PyBlockSimHandle`
exposing `build` + the generic per-call execution; the override-application
primitive; `fetch_priority_fee_percentiles` (via D5); the AL output.

A PyO3 shell to drive the `CacheDB`-based override path from Python is in
scope: the recently-added `apply_simulation_overrides` mechanism needs the
wiring to cross the FFI so the example driver can supply
`SimulationOverrideParams` from Python. This wrapper is thin
(arg-extract → GIL release → core call → result wrap), per ADR-005 §3 C
and ADR-013 (the FFI seam is private).

### D8 — `BotStateDb` forwarder: tracked debt, not decided here.

`BotStateDb` (`degenbot-evm/src/bot_state_db.rs`) is a confessed no-op
`DatabaseRef` forwarder: every method delegates to `fallback`; the
`bot_state` borrow is `#[allow(dead_code)]`. The deletion test passes
(complexity vanishes — routes straight to `WrapDatabaseAsync<AlloyDB>`).
This ADR does **not** collapse it: the wrapper persists as the option-B
seam (the typed-state serving path) and is explicitly tracked debt.
Tier-1 per-block shared-EVM (`BlockSimHandle`, ergo epic `V5HCR5` —
pruned on completion) already shipped without collapsing the forwarder;
the `std::dead_code` allow + the unread borrow are the tell. Its
disposition — collapse to bare `WrapDatabaseAsync<AlloyDB>`, or wire
the typed-state serving path so the seam earns its keep — is a separate
decision gated on whether Tier-2 option B (the deferred ergo epic
`L4GJEA` — "cross-block CacheDB with event-driven invalidation"; its
measurement spike `BF2V3B` is canceled, superseded by the shipped
benchmark `5UPBGD`) is ever pursued. If that option is firmly rejected,
collapsing the forwarder is cleanup; if it is pursued, `BotStateDb`
deepens rather than retires.

## Sequencing

The steps land in an order chosen for independent testability — each step
leaves a green, verifiable codebase:

1. **Move `fetch_priority_fee_percentiles` → `degenbot-rpc`** (D5). Isolated
   leaf move; its tests move with it; zero behavior change.
2. **Wire the Inspector-based AL** (D3). Additive: a new AL collection path
   on the first `transact_one` run, built + tested alongside the existing
   post-re-`transact` `emit_access_list_from_state`, then switched as
   primary. No behavior gap.
3. **Retire the RPC simulation surface** (D1). Delete `simulate_v1`,
   `create_access_list`, `build_simulation_state_overrides`, `payload`,
   `simulate_one`, and the `None` arm of `dispatch_profitable_results`.
   Step 2 already shipped the Inspector AL — the AL source is uninterrupted
   by the retirement. The duplication this ADR originally targeted resolves
   by deletion here.
4. **Merge `degenbot-evm` into `degenbot-simulation`** (D6). After step 3
   there is no strategy code left in either crate; the engine code is a
   cohesive set moving to one home. Tests move with their code.
5. **Extract the strategy to `examples/`** (D4). Move the surviving revm
   7-call bundle + the revm-only `dispatch_profitable_results` +
   `compute_priority_fee` + the sim value types out of the merged engine.
6. **Decompose the PyO3 surface** (D7). Retire `dispatch_profitable_py` +
   its pyclasses; add the primitive wrappers. Lands once the engine is in
   final shape (step 5).
7. **Rewire the example Python bot** to compose the PyO3 primitives. The
   last step; depends on step 6's surface being final.

## Consequences

- **Engine surface shrinks dramatically.** `degenbot-simulation` exposes a
  thin per-call execution + override-application + AL-emission surface, not
  a 7-call profit-detection pipeline. A standalone Rust consumer reaching
  for `cargo add degenbot` to build a sandwich or liquidation bot is no
  longer forced through the backrun example's shape.
- **One adapter, no RPC seam.** The two-adapter justification for an
  RPC-sim trait is gone. The simulation engine has one executor (revm); the
  RPC surface survives only for cold-miss state fetches + the fees oracle.
- **Strategy code is honestly filed.** The backrun 7-call bundle, its
  profit arithmetic, and its operator policy live in `examples/`, where
  their status as example code is self-evident and cannot be re-read as
  core. The AGENTS.md directive ("do not introduce a Python mirror of
  Rust-owned state … do not strand standalone-usable logic on the Python
  side") is preserved: the example owns its strategy; the engine owns
  generic primitives.
- **PyO3 surface is composable.** A Python driver assembles engine
  primitives rather than calling one opaque `dispatch_profitable_py`. This
  realizes the "give them tools they can assemble at runtime to suit their
  setup" intent.
- **Performance.** In-process revm is the production default and, after the
  Inspector-based AL (D3), executes the profit-detection run once (no
  post-re-transact for the AL). The cross-block `WarmCodeCache` (already
  shipped) keeps cold-miss RPC off the per-path hot path.
- **Dependency graph stays a DAG.** `degenbot-simulation` (merged) depends
  on `degenbot-bot` (`BotState`), `degenbot-rpc` (`AlloyProvider` + the
  D5 fees leaf), `degenbot-executor` (calldata + warmup-slot compute),
  `degenbot-core` (errors), `degenbot-decoders` (revert classification),
  and revm. No cycle. The no-pyo3-in-cores invariant is preserved — the
  strategy code in `examples/` carries no `pyo3`, and the PyO3 surface
  (D7) lives in `degenbot-python/src/simulation/` per the three-layer
  discipline.
- **Retirements are irreversible.** Per AGENTS.md, no backwards-compatibility
  layer for retired implementations: the RPC sim path, the `StateOverride`
  builder, `eth_createAccessList`, and the re-export bridge are deleted,
  not feature-gated. A consumer wanting RPC sim must re-add it; degenbot
  does not carry it.

## Alternatives considered

- **Collapse the dual-driver 7-call pipeline behind one sim-core seam** (the
  original Candidate A). Rejected by grilling: it would have unified the
  backrun strategy *as the simulation core's interface*, wedging every
  searcher into the example's shape. The dedup target is the strategy
  layer, not the engine.
- **A `degenbot-settlement-strategy` crate.** Rejected: a crate for one
  example doesn't earn its Cargo / PyO3 / standalone-reachability surface.
  `examples/` enforces the distinction more cheaply.
- **Keep the RPC sim path as a fallback** (two adapters retained). Rejected:
  it contradicts the "minimize external RPC I/O" long-term goal and
  preserves a seam the two-adapter rule doesn't justify (there is one
  adapter).
- **Keep `build_simulation_state_overrides` as the engine's RPC-rendering
  override path** (both `StateOverride` and `CacheDB` insertion in the
  engine). Rejected by D1/D2 — the in-process revm path is the sole
  executor; there is no node to send a `stateOverrides` JSON to.
- **Retire `eth_createAccessList` but keep the post-re-`transact`
  `emit_access_list_from_state` as the AL primitive** (Candidate C's
  intermediate). Rejected for the production AL path: the Inspector on the
  first run executes the tx once (not twice) and is the actual perf win
  the example bot represents. The post-re-transact path remains available
  as an engine-generic primitive but is not the production AL source.
- **Fold the strategy into `degenbot-bot` as a bundled default.** Rejected:
  AGENTS.md's "no permanent Python responsibility" framing could pull any
  code living in `degenbot-bot` toward "core" again, re-wedging. Code in
  a crate named `settlement-strategy` or under `examples/` cannot accidentally
  be re-read as core.

## References

- `CONTEXT.md` — "Simulation engine vs. searcher strategy" section (the
  load-bearing vocabulary; kept current as the decision lands).
- ADR-003 (Bot as state owner) — the `BotState` the revm DB stack reads.
- ADR-005 (Polars-inspired three-layer architecture) — the standalone-core
  constraint + the PyO3-wrapper discipline (D7's thin shell).
- ADR-013 (FFI seam is private) — D7's PyO3 wrapper is private, no
  re-exported types.
- ADR-015 (solver seam relocation) — the precedent for "split across two
  crates by an accidental line" (D6 mirrors its collapse).
- ADR-018 (tracked debt) — the precedent for recording acknowledged debt
  without a code change (D8 follows this shape).
- `docs/spikes/revm-composition-api-and-cold-miss-latency.md` — the revm
  composition API, the verified `CacheDB` insertion surface, and the cold-
  miss latency profile that motivates the in-process path.
- `docs/architecture/rust-owned-bot.md` — "Rust is the engine, Python is
  the cockpit" (the backrun bot is one cockpit, not the engine).
