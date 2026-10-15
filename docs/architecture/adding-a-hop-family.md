# Adding a hop family — the family-module and envelope-gate checklist

A hop family is a solve arm plus a gate bound: the mixed solver learns to solve
paths that contain the family, and the profit-envelope gate learns a rigorous
upper bound on the family's output curve, so the pre-solve screen can skip a
path without ever under-cutting a real leaf.

**The eight-site checklist that stood here is SUPERSEDED** (ADR-071 D7). It
described one family's state, projections, simulators, solve entry, and line
derivation as parallel function sets scattered across `mixed/mod.rs`,
`mixed/solve.rs`, and `profit_envelope/derive.rs` — an eight-site change across
five files. A conductor for an eight-site change is evidence of a wide seam, not
a deepening of it, so ADR-071 collapsed the seam: the change is now **one family
module, one dispatch row, one enum arm per enum, one `HopType::ALL` entry, and
one glossary entry per new concept.** The old census survives only as the
superseded record in the appendix; do not reintroduce it.

**Extension in flight (ADR-073, proposed):** the one-module census above is solve-only.
ADR-073 extends the family seam to the read side — the **family read module** in
`degenbot-pools` (state slice, swap-read arm, resolve projection, family-local
apply/get/calc) beside this doc's solve-side **family module**, with dispatch rows at
the central sites and `concentrated_liquidity` naming throughout. Until it lands, this
conductor governs the solve half; the read half's conductor is part of the ADR-073 work.

This is the implementer's conductor for the new shape, written from the code that
exists: every anchor below is a `file:line` copied from the tree, relative to
`rust/crates/engine/degenbot-solvers`. The *why* behind each obligation — the
concavity derivations, the measured allowances, the counterexamples, the
drift-net methodology — lives in
[the stableswap gate-bounds and solve-method evaluation](stableswap-gate-bounds-and-solve-method-evaluation.md);
this document is the *how*. The sibling runbook for the strategy plane is
[adding-a-strategy.md](adding-a-strategy.md).

## 1. The change census

One new file in `rust/crates/engine/degenbot-solvers` — `src/mixed/families/<family>.rs`,
registered in the family tree `src/mixed/families/mod.rs:4` — plus the residual
central sites ADR-071 D4 permits: one `HopType` variant (and its `HopType::ALL`
entry), one `ResolvedHop` arm, one `HopMath` arm, and one `FAMILY_DISPATCH` row.
The glossary gains one entry per new concept. The five landed modules are the
template: `concentrated_liquidity.rs`, `solidly.rs`, `balancer_weighted.rs`, `balancer_stable.rs`,
and `curve_stableswap.rs` under `src/mixed/families/`.

## 2. The family module — the export surface

One module owns everything the family contributes to a solve and its bound
(GLOSSARY.md "Family module"). Its exports are:

- **the state slice** — the family's resolved state, carried by the `ResolvedHop`
  variant: `SolidlyHopState` (`src/mixed/families/solidly.rs:57`),
  `BalancerWeightedHopState` (`balancer_weighted.rs:44`), `BalancerStableHopState`
  (`balancer_stable.rs:38`), `CurveStableswapHopState` (`curve_stableswap.rs:39`).
- **the gate projection `to_math`** — the family's `ResolvedHop -> Option<HopMath>`
  arm, registered on the row: `solidly_to_math` (`solidly.rs:84`),
  `weighted_to_math` (`balancer_weighted.rs:69`), `stable_to_math`
  (`balancer_stable.rs:63`), `curve_to_math` (`curve_stableswap.rs:73`),
  `concentrated_liquidity_to_math` (`concentrated_liquidity.rs:46`).
- **the V2-equivalent projection** — the Möbius profitability precheck the shared
  template consumes: `solidly_hop_v2_equiv` (`solidly.rs:167`),
  `balancer_weighted_hop_v2_equiv` (`balancer_weighted.rs:134`),
  `balancer_stable_hop_v2_equiv` (`balancer_stable.rs:119`),
  `curve_hop_v2_equiv` (`curve_stableswap.rs:130`).
- **the simulate triple** — path / path-outputs / single-hop over the family's
  real math leaf: `simulate_solidly_path` (`solidly.rs:192`) /
  `simulate_solidly_path_outputs` (`:205`) / `simulate_solidly_hop` (`:230`), and
  the weighted (`balancer_weighted.rs:158` / `:166` / `:194`), stable
  (`balancer_stable.rs:140` / `:146` / `:167`), and Curve
  (`curve_stableswap.rs:151` / `:157` / `:178`) triples below them.
- **the solve entry** — the family's path-level solve, registered on the row: the
  dispatch-row adapter (`solve_solidly_entry` `solidly.rs:122`) over the family's
  own search (`solve_solidly_path_int` `:139`), built on the shared golden
  template `solve_golden_section_path_int` (`src/mixed/solve.rs:497`) where the
  stable-family search applies. The template's family-specific parts enter as two
  closures (`simulate`, `simulate_outputs`); the ±3 integer-verification order
  (`verify_best` `:576`, `ascending_sweep` `:608`) is shared by every arm, not a
  per-family choice. A family whose search genuinely differs does not reuse the
  template — but it must still own the whole search (precheck, bracket, refine,
  verify), never a fragment.
- **the envelope lines** — the family's line builder, registered on the row:
  `solidly_hop_lines` (`solidly.rs:280`) delegating to `solidly_stable_lines`
  (`:372`) / `solidly_stable_rise` (`:420`); `weighted_hop_lines`
  (`balancer_weighted.rs:228`) / `weighted_lines` (`:261`);
  `balancer_stable_hop_lines` (`balancer_stable.rs:195`) /
  `stableswap_concave_at` (`:250`) / `balancer_stable_lines` (`:329`) /
  `balancer_stable_rise` (`:371`); `curve_hop_lines` (`curve_stableswap.rs:213`) /
  `curve_stableswap_lines` (`:247`).

CL is the extreme case: V3 and V4 share one solve (ADR-069), so `concentrated_liquidity.rs` owns
`ConcentratedLiquidityHop` (`concentrated_liquidity.rs:37`), the walkable-path entry `solve_walkable_path_int` (`:84`),
and the tangent-fan builder `concentrated_liquidity_hop_lines` (`:463`). It also owns `refine_bounds`
(`:62`), the seam that translates the envelope's composed lines into the kernel's
own refine-window vocabulary (`RefineBound`, `src/concentrated_liquidity/window.rs:18`) — the kernel
names no envelope type.

## 3. The dispatch row — the row IS the registration

`src/mixed/solve.rs` holds only the shared kernels plus the table. Add a
`FamilyRow` (`:178`) whose four fn pointers (`solve`, `to_math`, `lines`,
`label`) point at the family module's exports, and register it in the ordered
`FAMILY_DISPATCH` (`:203`). Add **two** rows, in order:

- a narrow accept row `allowed: Some(&[HopType::V2, <Family>])` /
  `required: Some(<Family>)` selecting the family's entry;
- a wide refusal row `allowed: None` / `required: Some(<Family>)` refusing the
  family beside any other non-V2 family (`refuse_path` / `refuse_to_math` /
  `refuse_lines`).

The order is load-bearing, not cosmetic: the narrow row sits above the wide
refusal, and both above the mixed walk. A path is classified once by the SET of
hop families present (`family_row`, `:320`), and a single hop's row is looked up
by `family_row_for` (`:336`); both are set-membership tests against the table —
never a per-call if-chain.

## 4. The residual central sites — one enum arm per enum, one ALL entry

ADR-071 D4 permits exactly three residual central sites plus the `HopType`
variant and its `ALL` entry:

- **one `HopType` variant** (`src/mixed/mod.rs:58`) with a doc comment stating
  the invariant and whether the family is concentrated-liquidity, and **one
  `HopType::ALL` entry** (`:89`). `Ord` is the discriminant order, used only for
  deterministic key sorting (`AffectedKey`), so append rather than reorder.
  `is_concentrated_liquidity` (`:103`) needs no arm unless the family is CL.
- **one `ResolvedHop` arm** (`:201`) plus its `hop_type()` arm (`:232`); add the
  variant to the four non-CL accessor `None` arms (`as_int_sequence` `:255`,
  `as_word_profiles` `:271`, `as_crossing_table` `:286`, `as_prepared_concentrated_liquidity_hop`
  `:300`). Add a family accessor (mirror `as_solidly_state` `:313`) only when a
  caller needs one — the Balancer and Curve variants carry none and are matched
  directly at their use sites.
- **one `HopMath` arm** (`src/profit_envelope/derive.rs:29`) plus the one-line
  delegation in `HopMath::family_row()` (`:176`), which maps the arm to its
  `family_row_for` lookup. `HopMath::lines` (`:194`) and `family_label` (`:208`)
  then read straight off the returned row, so the labelling site (`family_label`,
  consumed by the degenerate-path capture row `capture.rs:65` and the gate's
  diagnostic `gate.rs:956`) stays single.

The pin is the acceptance surface for this exhaustiveness. `expected_entry`
(`src/mixed/solve.rs:754`) is a wildcard-free `match` over `HopType`, so a new
variant fails compilation of the test until its registration is named;
`assert_all_variants_listed` (`:790`) then counts `HopType::ALL` against the same
or-pattern, so a variant left out of — or duplicated in — `ALL` fails the count.
**That is why a new family needs its `HopType::ALL` entry: the pin iterates
`ALL`, so a variant missing from `ALL` escapes the pin entirely.** The test
`family_dispatch_table_selects_the_row_the_old_ladder_selected` (`:826`) then
asserts, per variant, that the row registers the exact `to_math` / `lines` /
`solve` fn the family owns — positive identity, so a dropped row, a refusal stub,
or a cross-wired builder fails.

## 5. The soundness stances a new family must decide

Every family decides two things, and both are load-bearing for the bound:

1. **Is the family's output curve concave in the input?** If yes, the entry
   tangent at zero input point-wise dominates the curve (the tangent-line
   property of concave functions), so the family gets an entry-tangent rise
   beside its reserve cap — as Solidly stable and Balancer weighted do, and
   Balancer stable does wherever its closed-form curvature test
   (`stableswap_concave_at`, `balancer_stable.rs:250`) holds. If the curve is not
   concave, the family keeps the **reserve-cap bound alone** — the entry tangent
   would under-cut a real leaf and skip a profitable path. Curve stableswap is
   the measured counterexample: its out-curve was MEASURED non-concave
   (`curve_stableswap_lines`, `curve_stableswap.rs:247`). The derivations and
   counterexamples are in
   [the evaluation doc §1 and §4](stableswap-gate-bounds-and-solve-method-evaluation.md#1-solidly-stable-the-output-curve-is-concave-so-the-gate-carries-its-entry-tangent).

2. **Where does the leaf's integer arithmetic stop being faithful?** Every
   tangent bound carries the leaf's rounding slack, derived from the leaf's
   actual arithmetic and measured, never assumed. Drop the rise (cap-only — never
   `None`, never looser than the plain cap) wherever the leaf's integer
   arithmetic could wrap across the whole `[0, reserve_out]` evaluation window: a
   wrapped Newton denominator returns a garbage root that can inflate the leaf
   above the tangent, which would make the gate skip a profitable path. See
   [the evaluation doc §1 the faithful-arithmetic window guard and §4](stableswap-gate-bounds-and-solve-method-evaluation.md#4-balancer-v2-stable-an-exact-curvature-test-and-a-zero-allowance-tangent).

A family also declares whether it is concentrated-liquidity (it drives
`is_concentrated_liquidity` and the non-CL accessor arms): CL and V2 are the only
families with accessors; the rest carry none.

## 6. The acceptance surface

- **the pin** (§4) — the compiler's exhaustiveness nag restored as a named test,
  with positive fn-identity assertions.
- **the shared dominance harness** — every gate bound is proven against the
  family's real math leaf, never a reimplementation. One generic harness owns the
  shape strategy, the dense log-grid sweep, the point-wise dominance assertion,
  the counters, and the telemetry (`tests/profit_envelope_tests.rs`: the
  `DominanceFamily` trait `:1168`, `dominance_case` `:1221`, `run_dominance_suite`
  `:1300`); a family supplies only its shape strategy, its hop view, and its leaf
  oracle. The run asserts non-vacuity floors on four counters — `shapes`,
  `asserts`, `rise_active`, and `guard_fired` — so a strategy that quietly stopped
  drawing the wide matrix fails rather than passes silently.
- **the drift net** — before any refactor of the arm, pin its
  `(optimal_input, profit)` on a synthetic corpus, and verify the pinned
  snapshots against the pre-refactor tree in an isolated worktree, so the
  snapshots pin the old behavior rather than whatever the new code produces. The
  committed captured mixed corpora carry no Solidly, Balancer, or Curve legs, so
  a synthetic corpus IS the net. The two landed binaries are
  `tests/solidly_golden_parity.rs` and `tests/stable_golden_drift_net.rs`; the
  family's corpus builders live in `tests/common/mod.rs` (`path` `:30`,
  `solidly_hop` `:62`, `weighted_hop` `:89`, `stable_hop` `:114`, `curve_hop`
  `:137`, `faithful_base` `:169`). Why:
  [evaluation doc §5](stableswap-gate-bounds-and-solve-method-evaluation.md#5-the-drift-net-pinned-pre-refactor-snapshots-for-behavior-identical-refactors).
- **the family module's own `mod tests`** — ADR-071 D6 relocates a family's
  dominance cases into its module (the module's interface is their test surface);
  the shared corpus and the cross-family shrink harness stay shared pins.

## 7. Worked checklist

1. Create `src/mixed/families/<family>.rs` and register it in
   `src/mixed/families/mod.rs` (§2). Move the family's state slice, `to_math`,
   V2-equivalent projection, simulate triple, solve entry, and envelope lines in
   **byte-identical** (ADR-071 D5).
2. Add the `HopType` variant, its doc comment, and its `HopType::ALL` entry (§4).
3. Add the `ResolvedHop` arm, its `hop_type()` arm, and the four non-CL accessor
   `None` arms (§4).
4. Add the `FamilyRow` and its two `FAMILY_DISPATCH` rows, in order (§3).
5. Add the `HopMath` arm and its one-line `family_row()` delegation (§4).
6. Decide the family's soundness stance — concave? entry tangent or
   reserve-cap-only? where does the leaf's arithmetic wrap? (§5).
7. Prove the bound with a dominance suite against the real leaf, with the four
   non-vacuity counters and a shrinking strategy (§6).
8. Pin the arm's `(optimal_input, profit)` on a synthetic corpus before touching
   it, and add the family's regression binary (§6).
9. Update `GLOSSARY.md`: the family's bound concept gets an entry in the
   glossary's house style.

## Appendix — the superseded checklist, as record

The checklist this document replaced enumerated eight sites across five files:
the `HopType` variant, the `TryFrom<&Identity>` arm, the `ResolvedHop` variant
and its accessors, the solve arm, the simulators and V2-equivalent projection,
the `FAMILY_DISPATCH` row, the `HopMath` variant with its `family_label` and
line-derivation arms, and the corpus builder — spread across `mixed/mod.rs`,
`mixed/solve.rs`, `profit_envelope/derive.rs`, and `tests/common/mod.rs`, plus
the "Profit envelope" entry in `GLOSSARY.md`. Every one of those sites is now
inside the family module or the residual sites of §4; the census is kept here
only so a reader meeting the old document in history knows what it described.
