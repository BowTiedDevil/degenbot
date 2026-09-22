# ADR-059: The pool family kernel — one taxonomy, capability tiers, species as data

**Status: accepted** (2026-09-22, reviewed with the run-order amendment below). Basis: the touch-point survey
of 2026-09-22 (eight family enum definitions across the workspace) and the
roadmap drivers — LFJ (Trader Joe) binned-liquidity pools and
SushiSwap-V4/PancakeSwap-V4 manager species. Predecessors: ADR-014 (pool
registry projections), ADR-019 (strategy vs engine), ADR-040 (enum state
machines), ADR-043 (metric-label closure), ADR-058 (strategy crate split).

## Context

Two pool families ship today (V2/V3 plus a partially built V4), a third
arrives soon in two shapes: a genuinely new structure (LFJ binned
liquidity) and new species of an existing structure (Sushi/Pancake V4
manager deployments). The codebase already holds the ingredients of a
general answer — `degenbot-pools` `Structure`/`Identity` with its
structure-times-fork split, `ClSlotLayout`-parameterized fork storage,
the shared V3/V4 concentrated-liquidity solve dispatch, the V4 executor
opcodes (0x40-0x42) with golden-pinned shapes, and the address-keyed
registration gate. What it does not have is any force making these
agree. Eight separately maintained family enum definitions exist, each at a
different granularity with no compile-time relationship:

| Vocabulary | Home | Arms |
|---|---|---|
| `PoolFamily` | `degenbot-simulation/src/sim/evm/journal_pools.rs` | extraction (V2Pair/V3/V4PoolManager) |
| `PoolFamily` | `degenbot-bot/src/bot_core/pool_builder/builder.rs` | registration probes (V2/V3/Balancer/Curve) |
| `PoolFamily` | `degenbot-pool-updater/src/fetch.rs` | event decode (V2/AerodromeV2/V3/V4) |
| `Structure`/`Identity` | `degenbot-pools/src/pool.rs` | state model (3 structures, per-fork variants) |
| `RegisteredPoolFamily` | `degenbot-pools/src/registry.rs` | address-keyed readers (7, V4 missing) |
| `HopType` | `degenbot-solvers/src/mixed/mod.rs` | solve dispatch (7 families) |
| `PoolKind` | `degenbot-pathfinding/src/graph.rs` | discovery graph (V2/V3/V4 only) |
| `LaneFamily` | `degenbot-strategy/src/backrun_engine.rs` | backrun lanes (V2/V3 only) |

The strategy arms draw different amounts from this set: settlement
(arb_engine) solves 7 families and composes V4; the backrun arm is
V2/V3-only end to end, with V4 extraction stopping at a `v4_unsupported`
observe. Fork knowledge is additionally duplicated as per-language
constants (Rust `V3_FAMILIES`/`v2_v3_subclass_table`/`V3_VARIANT_TABLES`,
Python SQLAlchemy subclasses, trackers, two byte-compared
deployments.json copies). Adding LFJ by hand across these surfaces is
counted in the survey at ~14 Rust regions plus 6 Python ones; adding a
V4 species touches fewer but includes a structural gap (deployments are
factory-keyed, V4 managers are not) and the backrun arm's absent V4
capability.

The on-chain executor contract is a hard floor no abstraction removes:
compose can only emit opcodes `cmd_executor` embeds. LFJ needs new
opcodes and shapes; the kernel's job is to make that fact explicit,
not to hide it.

## Decision

### D1 — `degenbot-pools` is the taxonomy of record

`Structure`, `Identity` (variant + DexName), `PoolEntry`, and the
concentrated-liquidity pool traits are the single definition of "what a
pool is". Every other vocabulary (`PoolKind`, `HopType`, `LaneFamily`,
journal `PoolFamily`, builder `PoolFamily`, updater `PoolFamily`,
`RegisteredPoolFamily`) becomes a **projection of the taxonomy** with a
defined mapping from `Structure`/variant, not an independent enumeration.
A projection may lag the taxonomy (a family present in the taxonomy but
unsupported by that tier belongs on that tier's capability list, D2),
but may not define a family inconsistently with it. The office term for
a projected family is one the tier admits it cannot express.

### D2 — Families are a trait stack with explicit capabilities

A family is a contract over the pipeline tiers it can serve:

- `extract`: journal/storage decode of touched state into typed posts
  (slot layout, event shapes, tick/bin data)
- `admit`: typed posts into the workspace (`PoolEntry` arm, spec-bound
  registration, verdict caching on refusal)
- `discover`: graph participation (edge payload, liquidity ranking,
  anchors)
- `compose`: executor opcode(s) + grammar shape emission

Each tier records a `FamilyCapabilities` verdict — partial support is an
explicit, observable state (continuing the `v4_unsupported` observe
pattern), never a silent no-op or a guessed decode (ADR-014's
no-guessing rule preserved). A tier that gets ahead of its contract
(e.g. extraction for V4 shipped before admission) carries the burden of
the truthful observe reason, which is exactly what the frame pipeline
already does.

### D3 — Species (forks and manager deployments) are data

A fork that differs from its family only in identifiers (table name,
factory, init codehash, manager address, chain scope, fee denominator,
storage-layout id) is a **species entry in a single manifest**, not a new
type. The manifest is single-sourced and consumed by both the Rust core
and the Python driver (DB subclass identity strings, tracker
construction, deployments). Adding a species is a manifest entry plus a
Python model/table row where the species has its own DB subclass table.
The separately-maintained constants (`V3_FAMILIES`,
`v2_v3_subclass_table`, `V3_VARIANT_TABLES`, `resolve_static_config`,
Python `_POOL_VERSION_MAP` inputs, dual deployments.json) collapse into
or derive from the manifest.

Behavior that genuinely diverges (a fork with different storage slots or
event shapes — cf. `v3_pancakeswap_*`) remains typed code, defaulted by
the manifest's layout reference so a vanilla fork ships with zero
overrides.

### D4 — The discovery graph is family-generic over species

Graph edges stay `(token0, token1, pool, kind)` at the connectivity
layer; `kind` projects from the taxonomy (D1). Binned pools are
("token-pair, count-existence") edges for connectivity and liquidity
ranking — bin granularity is an extraction/solver concern, not a graph
concern. V4 manager species key their known-poolId sets through the
connector index per manager, which today is the concrete gap for the
backrun arm (`V4PoolSet` is empty-from-nowhere, so V4 extraction
currently decodes nothing).

### D5 — Compose capabilities name their executor ceiling

A family's `compose` capability is bounded by and named after the
executor opcodes it can emit. The 2-byte V4 fee cap (`V4_FEE_ENCODER_MAX`)
is the model: the contract's encoding limits are admission-side rules,
not post-hoc blockers. LFJ's composer work is a new opcode family plus
at least one grammar shape, golden-pinned like `two_hop_seed_v4`. No
family is `compose`-capable until executor support is deployed and the
bytecode files in `contracts/` are updated to match.

### D6 — Dispatch is at tier boundaries; hot loops stay monomorphic

The kernel resolves a family once (at registration or frame admission)
and hands downstream tiers monomorphic enum/sum data. No `dyn` trait
dispatch enters the solver workspace, the walk, or the path search.
Metric vocabulary stays closed (ADR-043): family projection into labels
only through the reviewed allowlist mechanism, and only where a
combinatorial set is provably small ("families participate" not
"species participate").

### D7 — Two arms, one taxonomy, no capability-hiding

Settlement and backrun arms remain free to support different tier
subsets, but the difference is represented in the family's capability
list per arm — not discovered by which code path happens to be wired.
"Backrun supports V4" is a future state of the backrun arm's capability
list reachable without touching family definitions in other tiers.

### D8 — Declared-unsupported is loud, never silent

A family on the roadmap (LFJ binned liquidity) or a species row in the
DB whose family a tier does not admit must surface as a typed, reason-
coded observation at the tier where support stops — the same
truthfulness D2 demands of partial capability, applied to
intentionally-absent capability. Concretely: a touched address matching
a registry row (`pools`/`managed_pools` kind) outside the tier's
supported set observes with a stable "family-unsupported" reason naming
the kind, instead of falling out of the descriptor map as an unexplained
no-candidate. Adding a family to the database must produce observable
frames, not silence.

## Non-goals

- No open-world registry: the closed, reviewable species list stays
  (reputational cost of an accidental family is higher than the friction
  of editing a list).
- No Python-side trait machinery: the Python driver consumes the
  manifest and the registry; it does not grow its own family
  abstractions past what the FFI surface already provides.
- No backfill of `PoolKind`/`HopType` gaps for Curve/Balancer/Solidly
  into the backrun arm — the capability lists start truthfully at
  today's actual coverage.
- No behavior change during the retrofit (E1): V2/V3/V4 paths must stay
  bit-identical, guarded by the existing suites.

## Epic order (summary)

1. **E1 — Kernel retrofit.** Landing D1/D2 as code in `degenbot-pools`
   + projections at every existing vocabulary, V2/V3/V4 behavior
   unchanged, `just test-rust` + pytest green. Includes the
   `RegisteredPoolFamily` V4 arm (a straight bug-shape hole).
2. **E2 — V4 species.** Manifest lands; Sushi/Pancake V4 manager entries;
   backrun V4 capability (connector poolId feed — admission composing,
   composing capability rides the existing opcodes/shapes).
3. **E3 — LFJ.** First new structure through the kernel, top to bottom:
   DB, decoders, `PoolEntry` arm, solve branch, profit envelope for bin
   ladders, executor opcode + shapes, Python surface. This epic is the
   kernel's acceptance test — a new family lands and the diff is
   bounded, reviewable, and free of scattered edits.

Each epic is ergo-planned separately; E1's task bodies carry the survey's
file inventory as their working checklists.

**Run-order amendment** (2026-09-22 review): the V4 backrun wiring epic
(ergo `L77LS2`) precedes and informs E1. Wiring V4 through the existing
seams — the same way V3 is wired today — exposes exactly where seam
placement hurts, and that evidence finalizes the E1 kernel contracts
before they harden. Retrofitting the kernel first would erase the signal.
Two integration markers ride the V4 epic: (a) every new seam it cuts is
annotated as a kernel hook point (a doc comment naming the D-ruling it
becomes), and (b) the D8 loud marker lands in the same descriptor seam,
so a planned-but-unsolved family (LFJ) is observably rejected the
moment its rows appear in the DB. LFJ remains E3, planned — no
taxonomy arm until its implementation epic.

## Consequences

- The eight-vocabulary survey becomes the checklist for making each
  vocabulary a projection — measurable completion criterion, not a
  judgement call.
- New-family onboarding cost becomes linear in genuinely-new behavior
  (one module per tier where the family diverges from defaults) rather
  than coincident with framework surgery.
- Cross-language drift moves from "difficult to notice" to "manifest
  parity check", one test/CI gate instead of two files to update in
  lockstep.
- The executor contract keeps its veto: capability claims that outrun
  `cmd_executor` are structurally impossible to express (D5), keeping
  the "registry that governs" property understood during design.
- A new vocabulary can no longer be introduced without touching the
  projection map — that friction is deliberate and leans on the
  cardinality/closure discipline of ADR-043 for good hygiene.

## Open questions

- LFJ profit envelope: does the staircase bound fold into the existing
  `profit_envelope` machinery with a new piece type, or does it need a
  sibling module? (E3 design question, not blocking.)
- Manifest format and home: TOML in `degenbot-pools` (Rust parses at
  compile time, Python reads the same file), or the existing
  deployments.json extended? Leaning toward TOML beside the taxonomy,
  with deployments.json becoming a generated projection — to be settled
  in E2 design.
- Whether the backrun arm's `zfo`-lane shape admits V4 paths without a
  new lane kind (bridge to the `two_hop_v4_led` shape vocabulary is the
  existing precedent of reuse).
- Exact placement of the Python manifest consumption boundary (which
  of `_POOL_VERSION_MAP` inputs / trackers / pool models read it before
  codegen is on the table).
