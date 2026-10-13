# ADR-071: Family modules and the Concentrated Liquidity Kernel — one module, one row, one enum arm supersedes the eight-site family change

**Status: accepted (2026-10-12). Predecessors: ADR-059 (D3 species stay typed, D6 hot loops monomorphic), ADR-060 (the walk representation collapse), ADR-069 (the prepared hop becomes one value), ADR-035 (math consolidation).**

## Context

`docs/architecture/adding-a-hop-family.md` states the cost of the current shape in
its own words: adding a hop family is *"an eight-site change across five files."*
What a family *is* — its state slice, its `*_hop_v2_equiv` projection, its
`simulate_*` triple, its solve arm entry, and its envelope line derivation — is
scattered as parallel function sets across `mixed/mod.rs`, `mixed/solve.rs`, and
`profit_envelope/derive.rs`. Understanding one family means bouncing between three
modules and stitching four siblings together by hand; a missed site is not a crash
but a silently wrong bound or a refused path.

The shared kernels have already converged — one golden-section template, one
integer-verification order, one shrinking dominance harness, a set-based
`FAMILY_DISPATCH` table — so the per-family fragments are what remains spread out.
The checklist document was written as the mitigation for the scatter; this ADR
deliberately supersedes that mitigation, because a conductor for an eight-site
change is evidence of a wide seam, not a deepening of it.

CL is the extreme case: it has no concrete home. Its kernel (crossings, the
active-set walk, hop/path simulation) is an unowned tree inside the solver crate,
while its envelope lines sit in `derive.rs` beside unrelated families. Yet V3 and
V4 already converge on the solve side — ADR-069 folded four representations into
one prepared hop, and `hop_math_for` maps `ResolvedHop::V3` and `ResolvedHop::V4`
onto the same `HopMath::Cl`. CL is a mild generic over the V3/V4 designs (they
diverge in storage slots, event shapes, and state reads — slot0 vs state-view,
hooks hashed into the PoolId — not in solve structure); the generic exists, it
just has no name and no home.

## Decision

**D1 — The family module owns one family end to end.** Per GLOSSARY.md "Family
module": one module per pool family carrying the state slice, the V2-equivalent
projection, the simulate triple, the solve entry (built on the shared golden
template where the stable-family search applies), and the family's envelope lines
(its rise line beside its cap). Ownership of *both* sides — solve arm and bound
derivation — is deliberate: the entry tangent a family simulates against and the
rise line it is bounded by cannot drift apart. Moves are byte-identical code
relocations; no arithmetic changes in the same change.

**D2 — The Concentrated Liquidity Kernel is the CL family's home and a library
the V3/V4 adapters call.** Per GLOSSARY.md "Concentrated Liquidity Kernel": the
data-parameterized module owning crossing derivation, the active-set walk over
piecewise-Möbius segments, CL hop/path simulation, and the word-profile build. It
is generic over *data* — the prepared hop value of ADR-069 D2 — never over a trait
per pool, so the hottest walk in the solver stays monomorphic (ADR-059 D6). The
concrete V3 and V4 adapters stay typed code (ADR-059 D3) and call the kernel at
the resolve seam to produce the one prepared hop. The kernel carries no envelope
vocabulary (no `Line`, verdict, or cap); the tangent fan is derived from its
crossing tables by the CL family module's bound side, and the engine-owned fan
cache stays borrowed, never owned. Home tiering: `degenbot-math`'s `cl` module
(sibling of the `v2` and `balancer` leaves, ADR-035) once a second tier consumer
reaches it — the solver crate's `cl` tree until then. No crate move for a consumer
that does not call yet.

**D3 — The dispatch row carries the entry points; the row IS the registration.**
`FAMILY_DISPATCH` rows hold fn pointers (`solve`, `to_math`, `lines`, `label`) to
the family module's exports, so adding a family touches no central `match` at all.
The call happens once per solve (dispatch), never per candidate input — the
monomorphism rule is untouched. `FamilyArm` becomes vestigial and is **deleted**:
by the deletion test, removing it concentrates dispatch complexity into the table
rather than duplicating it across enum + table. The existing pin
(`family_dispatch_table_selects_the_row_the_old_ladder_selected`) grows an
"every family module appears in the table" assertion: exhaustiveness moves from
the compiler to a named test.

**D4 — Residual central sites are exactly three, and no more.** One enum arm per
family in `ResolvedHop`, one in `HopMath` (a one-line delegation into the family
module), and one dispatch row. Rust enums stay single-sited; macro-generating the
variants from family modules is rejected as over-engineering.

**D5 — Sequencing: CL first, then the rest, each move byte-identical.** CL lands
first — it is the largest surface and the most frequent arbitrage-path citizen —
as two commits: (1) the kernel extraction with zero behavior change, (2) the
family module + fn-pointer row wrapped around it. Then Solidly, Curve,
Balancer-weighted, Balancer-stable in the same pattern. The acceptance bar is
ADR-060 D2's own: `WalkOutcome.result` byte-identical across the solver-vs-sim
parity corpus, with `v3/v4_crossing_solver_vs_sim_parity`,
`v3_iia_fixture_reproduction`, `v4_word_boundary_solver_divergence`,
`cl_cache_lab_goldens`, `cl_cache_patch_regressions`, `offline_heavy_replay`, and
the stable drift-net / golden-parity binaries green unchanged.

**D6 — Tests relocate, never delete.** The per-family dominance cases in
`mixed/solve.rs`'s `mod gate_tests` move into the family modules as unit tests —
the family module's interface is their test surface — while the shared corpus in
`tests/common/` and the cross-family shrink harness stay as the shared pins.

**D7 — The conductor is rewritten.** `docs/architecture/adding-a-hop-family.md`
becomes "one module, one row, one enum arm" (plus the glossary entry per family),
and GLOSSARY.md's "Family module" and "Concentrated Liquidity Kernel" entries are
the vocabulary of record for this shape.

## Why this is safe

Every move is a relocation of code the parity corpus already pins; the shared
kernels (golden template, verification order, envelope composition, caches) are
touched not at all. ADR-059 D6 is honored by construction (data-parameterized
kernel; fn-pointer dispatch at solve frequency only), and ADR-060 / ADR-069's
representation collapses are prerequisites this builds on, not choices reopened.
The bounds' soundness discipline is preserved by D1: the I512-exact derivations
move byte-identical and keep the same dominance tests.

## Consequences

- Adding a hop family becomes one family module + one dispatch row + one enum arm
  per enum + a glossary entry. The eight-site checklist is superseded, not
  amended.
- The compiler no longer nags about a missing dispatch arm; the table assertion
  (D3) is the named, load-bearing substitute.
- The review candidate "one solve-scoped bound session" becomes a natural
  follow-up: with each family's bound derivation in its family module, a
  `BoundSession` can import them without re-crossing module seams.
- Out of scope and unchanged: the envelope composition kernel and its caches
  (shared, family-blind), the Python companion's behavior, and the crate layout
  until the D2 second-consumer condition fires.
