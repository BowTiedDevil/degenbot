# Adding a hop family — the mixed-solver and envelope-gate checklist

A hop family is a solve arm plus a gate bound: the mixed solver learns to solve
paths that contain the family, and the profit-envelope gate learns a rigorous
upper bound on the family's output curve so the pre-solve screen can skip a
path without ever under-cutting a real leaf. Adding one is an eight-site change
across five files: four in the solver crate plus the "Profit envelope" entry in
`GLOSSARY.md`. The family also gets a regression binary under `tests/`, built
on the shared corpus.

This is the implementer's conductor for that change, written from the code that
exists: every anchor below is a `file:line` copied from the tree. The *why*
behind each obligation — the concavity derivations, the measured allowances,
the counterexamples, the drift-net methodology — lives in
[the stableswap gate-bounds and solve-method evaluation](stableswap-gate-bounds-and-solve-method-evaluation.md);
this document is the *how*. The sibling runbook for the strategy plane is
[adding-a-strategy.md](adding-a-strategy.md).

## 1. The site census

The eight sites span four files in `rust/crates/engine/degenbot-solvers`:
`src/mixed/mod.rs` (the mixed-path types), `src/mixed/solve.rs` (the mixed
solve surface), `src/profit_envelope/derive.rs` (the profit-envelope
derivation), and `tests/common/mod.rs` (the shared test commons). The change's
fifth file is the "Profit envelope" entry in `GLOSSARY.md`, updated once per
family.

### Site 1 — the `HopType` variant

`src/mixed/mod.rs:61`.

The taxonomy-to-engine enum. Add one variant naming the family, with a doc
comment stating the invariant and whether the family is concentrated-liquidity.
`Ord` is the discriminant order, used only for deterministic key sorting
(`AffectedKey`), so append rather than reorder. `is_concentrated_liquidity`
(`:84`) needs no arm unless the family is CL.

### Site 2 — the `TryFrom<&Identity>` arm

`src/mixed/mod.rs:100`.

Projects a pool taxonomy `Identity` onto the solver's hop engine. Add the arm
inside the matching `Identity::*` variant. Binned liquidity has no solve branch
and stays the lagging leg (`Err(UnsupportedHopIdentity)`), so do not widen that.
A fork whose behavior diverges inside a taxonomy variant — Camelot's stable-swap
species — is derived at registration by the engine's `derive_hop_type`, not
from this projection.

### Site 3 — the `ResolvedHop` variant, its accessors, and `hop_type()`

`src/mixed/mod.rs:330` (enum), `:358` (impl), `:361` (`hop_type`).

The resolved-state enum bundles only what the family's hop needs; the variant
IS the hop type. Then:

- `hop_type()` (`:361`) — add the arm mapping the variant to its `HopType`.
- the non-CL accessors `as_int_sequence` (`:384`), `as_word_profiles` (`:400`),
  `as_crossing_table` (`:415`), and `as_prepared_cl_hop` (`:429`) list every
  non-CL variant explicitly in their `None` arm — add the new variant there.
- add a family accessor (mirror `as_solidly_state`, `:442`) only when a caller
  needs one. The Balancer and Curve arms read `ResolvedHop::<Family> { state }`
  directly and carry no accessor.

### Site 4 — the solve arm

`src/mixed/solve.rs`.

- a `FamilyArm` variant (`:222`) and a `FAMILY_DISPATCH` row (site 6) that
  claims the family;
- the dispatch arm in `solve_path_inner` (`:336`) that calls the family's
  entry;
- the entry itself, `solve_<family>_path_int` (`:588`, `:938`, `:1085`, `:1164`
  are the four landed examples), built on the shared golden template
  `solve_golden_section_path_int` (`:642`) where the family uses the
  stable-family search. The template's family-specific parts enter as closures:
  `simulate`, `simulate_outputs`, and a `VerifySweep` (`:625`) selecting the
  ±3 integer-verification candidate order. A family whose search genuinely
  differs does not reuse the template — but it must still own the whole search
  (precheck, bracket, refine, verify), never a fragment.

### Site 5 — the simulators, the V2-equivalent projection, and `hop_math_for`

`src/mixed/solve.rs`.

- `<family>_hop_v2_equiv` (`:819`, `:967`, `:992`, `:1185`) projects the hop
  onto a V2 `IntHopState` for the Möbius profitability precheck;
- `simulate_<family>_path` / `simulate_<family>_path_outputs` /
  `simulate_<family>_hop` (`:844` / `:857` / `:882`, and the weighted, stable,
  and Curve triples below them) evaluate a candidate input against the family's
  real math leaf;
- `hop_math_for` (`:126`) maps the resolved hop onto the gate's `HopMath` view
  (site 7). A family with no rigorous bound maps to `None`, which poisons the
  whole path — no skip without a proof.

### Site 6 — the `FAMILY_DISPATCH` row

`src/mixed/solve.rs:263`.

The ordered family-set table. The first row whose `allowed` set contains every
present family (and whose `required` family, when set, is present) selects the
arm; a path matching no row falls to `FamilyArm::Walkable`. Add two rows, in
order: a narrow `allowed: Some(&[HopType::V2, <Family>])` /
`required: Some(<Family>)` row selecting the arm, then a wide `allowed: None` /
`required: Some(<Family>)` `Unsupported` row refusing the family beside any
other non-V2 family. The order is load-bearing: the narrow row sits above the
wide refusal, and both above the mixed walk.

### Site 7 — the `HopMath` variant, the `family_label` arm, and the line-derivation arm

`src/profit_envelope/derive.rs:28` (enum), `:188` (`family_label`), `:1103`
(`hop_lines_and_cap_cached`).

- add the `HopMath` variant carrying the leaf state the bound needs — borrow
  any balance vector (`&[U256]`) so the pre-solve hot loop allocates nothing
  per hop per candidate path;
- add the `family_label` arm (`:188`) — the one labelling site, shared by the
  degenerate-path capture row and the gate's diagnostic;
- add the line-derivation arm in `hop_lines_and_cap_cached` (`:1103`),
  delegating to a `<family>_lines` builder (`solidly_stable_lines` `:345`,
  `balancer_stable_lines` `:616`, `curve_stableswap_lines` `:779`).

### Site 8 — the corpus builder

`tests/common/mod.rs`.

A `<family>_hop(...)` builder beside `solidly_hop` (`:62`), `weighted_hop`
(`:89`), `stable_hop` (`:114`), and `curve_hop` (`:137`), constructing the
`ResolvedHop` state directly so the corpus is byte-identical to the inline
definitions the drift-net snapshots were captured from. `path` (`:30`) wraps
hops into a resolved path; `faithful_base` (`:169`) is the family-specific floor
below which the leaf's arithmetic stops being faithful.

## 2. The obligations

These four are the discipline the stableswap families established as
load-bearing. Each is recorded with its derivation and its measurements in the
[evaluation doc](stableswap-gate-bounds-and-solve-method-evaluation.md).

### 2.1 The drift-net discipline

Before any refactor of the arm, pin its `(optimal_input, profit)` on a synthetic
corpus, and verify the pinned snapshots against the pre-refactor tree in an
isolated worktree — so the snapshots pin the old behavior rather than whatever
the new code produces. The snapshots, not the new code's output, are the
oracle. A family's corpus builder (site 8) and its regression binary under
`tests/` are the net; the two landed examples are
`tests/solidly_golden_parity.rs` and `tests/stable_golden_drift_net.rs`. The
committed captured mixed corpora carry no Solidly, Balancer, or Curve legs, so a
synthetic corpus IS the net. Why:
[evaluation doc §5](stableswap-gate-bounds-and-solve-method-evaluation.md#5-the-drift-net-pinned-pre-refactor-snapshots-for-behavior-identical-refactors).

### 2.2 The dominance proptest

Every gate bound is proven against the family's real math leaf, never a
reimplementation of it. One generic harness owns the shape strategy, the dense
log-grid sweep, the point-wise dominance assertion, the counters, and the
telemetry (`tests/profit_envelope_tests.rs`: `dominance_case`,
`run_dominance_suite`); a family supplies only its shape strategy, its hop view,
and its leaf oracle through the `DominanceFamily` trait. The run asserts
non-vacuity floors on four counters — `shapes`, `asserts`, `rise_active`, and
`guard_fired` — so a strategy that quietly stopped drawing the wide matrix
fails rather than passes silently. The case draws one shape through a real
`Strategy`, so `proptest` shrinks a failure to a minimal offending shape. Why:
[evaluation doc §1 Evidence and §4](stableswap-gate-bounds-and-solve-method-evaluation.md#1-solidly-stable-the-output-curve-is-concave-so-the-gate-carries-its-entry-tangent).

### 2.3 The faithful-arithmetic guard

Every tangent bound carries the leaf's rounding slack — derived from the leaf's
actual arithmetic and measured, never assumed. The rise is dropped (cap-only —
never `None`, never looser than the plain cap) wherever the leaf's integer
arithmetic could wrap across the whole `[0, reserve_out]` evaluation window: a
wrapped Newton denominator returns a garbage root that can inflate the leaf
above the tangent, which would make the gate skip a profitable path. The Solidly
guard is `solidly_stable_arithmetic_faithful` (scaled reserves ≥ 2^60, the
pre-trade `calc_k` product inside `u256`, the input upscaling inside `u256`, and
every post-trade `calc_d` term non-wrapping); the Balancer guard is the exact
I512 curvature test plus the deployed numerator's bound. Why:
[evaluation doc §1 the faithful-arithmetic window guard and §4](stableswap-gate-bounds-and-solve-method-evaluation.md#4-balancer-v2-stable-an-exact-curvature-test-and-a-zero-allowance-tangent).

### 2.4 One labelling site, one arm-selection site

`family_label` (`derive.rs:188`) is the single labelling site: the
degenerate-path capture row (`capture.rs:65`) and the gate's diagnostic
(`gate.rs:956`) both call it, so a new family is labelled in exactly one place
and the two consumers cannot diverge. `FAMILY_DISPATCH` (`solve.rs:263`) is the
single arm-selection site: the dispatch key is the set of hop families present,
computed once, and every arm is a membership test against the table — never a
per-call if-chain. Why:
[evaluation doc §6 Cross-references](stableswap-gate-bounds-and-solve-method-evaluation.md#6-cross-references),
which names `derive.rs` as the derivation home and `solve.rs` as the shared
solve-template home.

## 3. Worked checklist

1. Add the `HopType` variant (site 1) and its `TryFrom` arm (site 2).
2. Add the `ResolvedHop` variant, its accessor (if a caller needs one), and the
   `hop_type` arm; extend the four non-CL accessor lists (site 3).
3. Add the solve arm: the `FamilyArm` variant, the `FAMILY_DISPATCH` rows
   (site 6), the `solve_path_inner` dispatch, and the `solve_<family>_path_int`
   entry (site 4).
4. Add the V2-equivalent projection, the simulators, and the `hop_math_for` arm
   (site 5).
5. Add the `HopMath` variant, the `family_label` arm, and the line-derivation
   arm (site 7).
6. Prove the bound with a dominance suite against the real leaf, with the four
   non-vacuity counters and a shrinking strategy (§2.2).
7. Drop the rise wherever the leaf's arithmetic can wrap (§2.3), and measure the
   rounding slack the rise carries.
8. Add the corpus builder (site 8) and the family's regression binary, and pin
   the arm before touching it (§2.1).
9. Update the "Profit envelope" entry in `GLOSSARY.md`.

## Appendix — site inventory

| Site | File:line | What belongs there |
|---|---|---|
| `HopType` variant | `src/mixed/mod.rs:61` | The family's engine variant + doc comment |
| `TryFrom<&Identity>` arm | `src/mixed/mod.rs:100` | Taxonomy projection arm |
| `ResolvedHop` variant | `src/mixed/mod.rs:330` | Resolved-state variant |
| `hop_type()` arm | `src/mixed/mod.rs:361` | Variant → `HopType` |
| non-CL accessor lists | `src/mixed/mod.rs:384,400,415,429` | Add the variant to each `None` arm |
| solve arm | `src/mixed/solve.rs:222,336,588+` | `FamilyArm` + dispatch + `solve_<family>_path_int` |
| simulators + `*_hop_v2_equiv` | `src/mixed/solve.rs:819,844,857,882` | Möbius precheck + real-leaf simulation |
| `hop_math_for` | `src/mixed/solve.rs:126` | Resolved hop → gate `HopMath` |
| `FAMILY_DISPATCH` row | `src/mixed/solve.rs:263` | Ordered arm-selection row(s) |
| `HopMath` variant | `src/profit_envelope/derive.rs:28` | Gate view + borrowed leaf state |
| `family_label` arm | `src/profit_envelope/derive.rs:188` | The one labelling site |
| line-derivation arm | `src/profit_envelope/derive.rs:1103` | Delegates to `<family>_lines` |
| corpus builder | `tests/common/mod.rs:30,62+` | `<family>_hop` + path wrapper |
| Profit-envelope glossary | `GLOSSARY.md:577` | One sentence naming the new bound |

## Discrepancies between the brief and the landed sites

- The brief's prose enumerates six bullets for "eight sites"; expanded to the
  concrete edit locations above, the eight resolve as sites 1–8. The
  `HopType` variant and its `TryFrom` arm are two distinct edit locations, and
  the solve arm, its simulators and projection, and `hop_math_for` are
  likewise separate.
- The brief's "five-file" is exact: the four source files above plus
  `GLOSSARY.md`, whose "Profit envelope" entry gains one sentence per family.
  That glossary site is not in the brief's enumeration.
- The accessors are not a per-family obligation in the CL/V2/Solidly sense for
  every family: the Balancer and Curve variants carry no `as_*_state` accessor
  and are matched directly at their use sites. Only the four non-CL accessor
  `None` arms are unconditionally required.
