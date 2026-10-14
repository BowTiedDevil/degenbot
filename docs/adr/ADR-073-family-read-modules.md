# ADR-073: The Family Read Module — one family, two homes, rows at the seams

**Status: proposed** (drafted at the architecture-review grilling, 2026-10-14; ratify
through the ergo review flow). Supersedes the solve-only scope of the ADR-071 family
census; ADR-059 D3/D6 and ADR-071 stand unchanged in substance.

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
