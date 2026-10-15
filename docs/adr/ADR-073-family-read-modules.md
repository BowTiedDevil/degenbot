# ADR-073: The Family Read Module — one family, two homes, rows at the seams

**Status: accepted / implemented** (drafted at the architecture-review grilling,
2026-10-14; ratified through the ergo review flow; landed across epic JE3DED
chunks C1–C6 — CL pilot, balance-vector, reserve-pair, Solidly, final sweep).
Supersedes the solve-only scope of the ADR-071 family census; ADR-059 D3/D6 and
ADR-071 stand unchanged in substance.

**Implementation dispositions (recorded at the C6 close):**

- **Zero-sentinel retirement (C6 item 1).** The `Ok(U256::ZERO)`
  not-yet-Rust-side sentinel on non-`STANDARD` Curve swap styles (crypto,
  live-admin, metapool `get_dy` base-pool dispatch) is retired to the typed
  `SimulateSwapError::NotComputable` at the Curve family read door. Precheck:
  the Python companion's Curve read path consumes `curve_get_dy` (the
  calculator path, which still computes those styles) — no consumer in
  `src/degenbot/` distinguishes zero-output from not-computable on this read
  door, and the pyo3 `simulate` seam flattens both verdicts to the same
  `not_computable` status. The sibling zero-input rule stays: `getAmountOut(0)
  = 0` is real semantics, not a sentinel.
- **`hop_sim` thin-forward — accepted divergence.** The kernel's
  `concentrated_liquidity::hop_sim::simulate_v3_range_swap` does NOT thin-
  forward to the `families::concentrated_liquidity::read_swap` door. The walk
  consumes PREPARED ranges (`IntV3TickRangeHop` — boundary prices, liquidity,
  fee scalars), no pool slice; a synthetic-state bridge (fabricating a slice
  whose tick map reproduces the prepared word boundaries) would move the
  byte-exact oracle surface rather than pin it — the tier-3 revm legs run
  through `v3_simulate_swap` (the pools-side walk behind the read door), and
  no drift net pins `hop_sim`'s per-word-boundary outcomes against a
  pre-refactor snapshot. The kernel's step math already delegates to the same
  canonical `compute_swap_step_v3` leaf the door's walk uses — one source of
  per-step parity — so the divergence is the walk FRAMING, not the math.
  Recorded as the accepted ADR-073 D4 residual: `read_swap` serves the
  registry-backed parity invocations; `hop_sim` serves the solver kernel's
  prepared-range invocations; both name the same step function.
- **Camelot-stable read-path gap — pre-existing, backlog RZ4XEX.** A Camelot
  stable pool registers as `PoolEntry::V2` with `stable_swap = true` but the
  pools-layer V2 row prices it with constant-product math while the solve
  branch and the Python companion use the Solidly-Camelot invariant. Dormant
  (unrouted), pre-existing before this ADR. A correct read needs a
  registry-decimals ruling (ADR-003 boundary: `V2PoolIdentity` carries no
  decimals); until that ruling the gap stays open in backlog RZ4XEX. The
  Solidly family door already carries the Camelot read arm
  (variant-carried scales) so the routed row has exactly one door to call
  when the ruling lands.

## Context

ADR-071 gave the solve arm a family module ("one family module, one dispatch row, one
enum arm per enum"), but the census it collapsed is solve-only. Below the solve, one hop
family's story is still told across five spreads, re-walked by hand at 25 `match entry`
sites:

- `degenbot-pools/src/simulate_swap.rs` — the central per-family swap dispatch;
- `degenbot-substrate/src/swap_simulation.rs` — per-family arms in
  `swap_simulation_ext`, with the recorded `TODO(extract-per-family-arms)`;
- `degenbot-substrate/src/resolve/<family>.rs` — the resolve projections (already
  one-file-per-family; the pattern's seed);
- `degenbot-substrate/src/{cl,balance_vector,reserve_pair}_orchestration.rs` — the
  family-shaped `impl BotState` method sets;
- per-family registration arms in `bot_core/build_register.rs` and the shell's
  `bot/pool.rs`.

A single literal home for the whole story is blocked by type ownership: the family
state slices live in `degenbot-pools` (ADR-001, I/O-free), the solve entry consumes the
family-blind kernels in `degenbot-solvers`, the resolve projection transforms the pools
state slice into the solver's hop vocabulary (`ResolvedHop` — GLOSSARY: the hop is the
solver's adapter, not a pool concept), and `BotState` is the state owner (ADR-003).
Any one-file-per-family home would close a dependency cycle across those laws.

Naming is split across conventions: the math leaf is
`degenbot-concentrated-liquidity-math` and the shell has `concentrated_liquidity_math`,
while the kernel is `cl/`, the resolve file `cl.rs`, the capability `ClOrchestration`,
and the Python shim `cl_companion.py`. `cl` is ambiguous as a code name.

## Decision

**D1 — Two homes per family.** The **family read module** (GLOSSARY: new term) lives in
the foundation layer (`degenbot-pools`, re-scoped as the family home) and owns everything
one family contributes to being read and resolved: the state slice, the swap-read arm
(every variant — miss-recovery and disarm-fetch included), the resolve projection as a
pure function of the family state slice (no `&BotState`), and the family-local
apply/get/calc behaviour over that slice. The **family module** (GLOSSARY: sharpened)
keeps the solve entry and the envelope lines in `degenbot-solvers`. Both homes carry the
family's name. The read module reads no state owner and consumes no solver kernel.

**D2 — Rows, not traits.** `PoolEntry` / `HopType` stay typed enums; each central site
keeps one dispatch row and one enum arm (ADR-059 D6's residual sites), and the shared
kernels stay monomorphic and family-blind. Rationale: exhaustiveness (a new family that
misses a seam fails to compile — the loud-abort discipline), the monomorphic hot path,
and a greppable closed taxonomy. Trait-like polymorphism remains legal *inside* a
family (the CL mild generic over the V3/V4 designs is the precedent); the family set
itself stays closed.

**D3 — `BotState` narrows to its named entries.** The single entries GLOSSARY defines
(`swap_simulation`, the registration entries, `resolve_path`) survive as the seams;
the family-shaped inherent method sets retire into the family read modules. Live state
stays with the state owner: `ClOrchestration` keeps its CL-local live state (event
buffers, `v4_pool_ids`, the V4 state-view registry, the registration gate, event
horizons) and loses only family behaviour. The orchestration capability is not retired
here.

**D4 — The CL read module's export surface is intent-keyed.** `families/concentrated_liquidity/`
is a tree (the family's honest unit — one family, two designs):

- `mod.rs` exports four doors: `capture_hop` (the resolve projection — one fresh
  observation of one hop, refused rather than defaulted),
  `read_swap` (all variants and both fetch modes behind one entry),
  `apply_event`, and `observe`;
- `v3.rs` / `v4.rs` hold the design-private projections, deliberately self-contained
  (fee convention, current-tick drain framing, net-sign direction) — the CL-projection
  guardrail becomes structural instead of a header comment, with no shared V3/V4
  constructor and no public door to one.

Design selection is the state slice type; the V3/V4 match happens inside the tree and
is compiler-exhaustive. The tier-3 parity gates and the revm runtime matrix run through
the production door — the gate and the pipeline are the same code (the drift-gate rule).
The solve-side `simulate triple` for CL delegates to `read_swap` over the prepared hop's
state: one source of swap truth.

**D5 — Expanded naming.** Code identifiers spell the family out:
`concentrated_liquidity` / `ConcentratedLiquidity…`. Renames: `cl/`, `cl.rs`,
`cl_route.rs`, `cl_orchestration.rs`, `ClOrchestration` → `ConcentratedLiquidityOrchestration`,
`ClHop`/`PreparedClHop` → `ConcentratedLiquidityHop`/`PreparedConcentratedLiquidityHop`,
`HopType::CL` → `HopType::ConcentratedLiquidity`, `BotState::cl()/cl_mut()` →
`concentrated_liquidity()/concentrated_liquidity_mut()`, `cl_companion.py` →
`concentrated_liquidity_companion.py`. "CL" survives only as the prose abbreviation
GLOSSARY defines. Metric and trace label renames move together with their consumers
(the Prometheus-name coupling the instruments state); ADR text is not rewritten.

**D6 — Landing order.** (1) The rename is a mechanical pass first: pure rename,
compile-green, zero residual `cl` code identifiers by grep, `just gen-stubs --check`
green — so every later diff contains only structure. (2) The CL pilot is the vertical
template: read module tree + the four doors + the resolve/swap-read dispatch rows +
`BotState` narrowing + hard cutover of the shell's CL surface, no aliases. (3) The
remaining families land in order of recorded friction: balance-vector (absorbing the
`TODO(extract-per-family-arms)`), reserve-pair, Solidly/Aerodrome, Balancer stable.
Each cutover: the pinned tests move with the family, the tier-3 oracle and the revm
matrix stay green, and the `match entry` census drops by exactly the moved arms — the
deletion test in executable form.

## Consequences

- The ADR-018 extraction sketch is pre-paid without touching its ruling: no trait is
  added and no crate is extracted (sample-of-one stands), but the family tree becomes
  free of the state owner, so when the second engine family fires the trigger the
  extraction is mechanical.
- The adding-a-hop-family census becomes: one family read module, one family module,
  one dispatch row per central site, one enum arm per enum, one glossary entry per new
  concept.
- The audit that anchors D4's doors: `project_v3`/`project_v4` have exactly one external
  caller (the resolve dispatch row) — a cheap door to reshape; `v3_simulate_swap` /
  `v4_simulate_swap` have ~30 call sites across eight crates, including the strategy
  simulator, the CL kernel's own hop sim, the executor's composers, the facade's
  investigation tools, and the tier-3 parity suites — the wide door, and the reason
  `read_swap` must serve design-specific parity invocations unchanged in outcome.
- **Not in scope here:** the kernel re-home (the one-file-per-family endgame),
  retiring `ClOrchestration` entirely, and any trait-ification of the family set.

## Amendment — the endgame landed (kernel re-home, door vocabulary, capability keep)

*Recorded at the T4 close of epic UGXLEG (T2/T3/T4 chunks). The status line above is
untouched — review flips it.*

**A1 — Two-axis family rule (T1/T2).** Reads route at the behavioral axis; state
lives at the structural axis (GLOSSARY.md "family read module" / "family module"):
the reserve-pair structural slice serves both V2 designs, the balance-vector slice
serves Curve + both Balancer invariants, and each family's routing decisions name the
behavioral family. One structural slice may back several behavioral families; the
dispatch rows stay typed enums (D2).

**A2 — One door vocabulary (T2).** The families share ONE family-blind door
contract at `families/door.rs`: one `SwapReadRequest`, one `ReadOutcome`, one
`Observe`/`Observed` probe pair, one `FamilyApply` apply contract. A family's design
bits ride as additive variants of these shapes, never as forks; the substrate
swap-simulation row adapts the outcome onto its typed contract.

**A3 — Capability thinned, then KEPT (T3, fork-2(b) judgment).**
`ConcentratedLiquidityOrchestration` retired its 21 design-named behavior arms into
the family-neutral `BotState` rows and the family read doors, keeping only CL-specific
live-state choreography: the routing funnel, the dual liquidity buffers + drains, the
delivery cutoff, the event horizons, the quarantine lifecycle, and the pin/provenance
verification. **Fork-2(b) is decided: KEEP the capability — do not fold it into
`RegistryCore`.** Its remainder is family behavior by construction (V3 address-keyed
vs V4 key-keyed buffers are inherently different maps); folding it into the core would
smuggle CL behavior into the family-agnostic registry table or push consumers back to
fine-grained accessor calls. A later fold stays cheap if a second buffered family
forces the generalization.

**A4 — The priors door: explicit exception (T3 → T4 decision).** The
tick-priors overlay (`swap_priors`) applies to DIRECT swap application only; buffered
events never carry priors. `FamilyApply`'s associated `Event` type therefore cannot
and should not carry priors: extending the door contract (option (i) at T4) would tax
every buffered event's shape to serve a caller class that never buffers, for zero
behavioral gain — a design change mid-re-home with byte-exact surfaces at stake. The
decision is **option (ii): the priors-carrying direct apply is a second, intentional
application door**, formalized here as the documented exception to the "everything
applies through `FamilyApply`" reading: `route_event`/`route_event_by_key` accept
priors and apply directly through the state's own typed apply methods; the buffered
drains keep using `FamilyApply`. Both doors share the state slices, the routing
table, and the acceptance witness (the armed oracle legs); neither drops or invents
priors.

**A5 — The endgame landed (T4): one file per family.** The family-blind kernels —
the golden-section template (`bounded_brent`), the integer-verification Möbius order
(`mobius_int`, `mobius_int_exact`, `mobius_shifted_piece`), the envelope composition
and its caches (`profit_envelope`), plus the mixed intake contract, the CL walk
kernel, the basket solver, capture fixtures, and the runtime config — moved DOWN into
`degenbot-pools/src/solve/`, beside the family read modules. Each solve entry moved
INTO its family module: `families/{balancer_stable,balancer_weighted,curve_stableswap,
solidly}.rs` carry a nested `pub(crate) mod solve`, and the CL tree carries
`families/concentrated_liquidity/solve.rs` — the D1 two-homes pattern is literally
ONE file per family. The `degenbot-solvers` crate is dissolved (workspace manifest
consistent; the facade keeps its `degenbot::solvers` re-export name, now pointing at
`degenbot_pools::solve`; the `hotpath`/`telemetry` features move to `degenbot-pools`).
The accepted `hop_sim` divergence disposition now travels with the code as the
`solve/concentrated_liquidity/hop_sim.rs` module doc. The C3/C5 routing helpers were
renamed to their mechanisms (`presence_by_address`, `presence_by_pool_id`) — no
design-named family arms remain anywhere.

**A6 — Byte-exact acceptance.** The endgame landed under the armed drift net: the
tier-3 oracle suites, the revm runtime matrix, the facade parity legs, and the
solvers' crossing/parity + golden-replay suites stayed byte-identical across the
move (recorded in the T4 sign-off).
