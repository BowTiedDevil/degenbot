# 7MRCQW (ADR-071 T6) — the conductor rewrite + glossary reconciliation

Docs-only chunk. ADR-071 D7: `docs/architecture/adding-a-hop-family.md`
becomes "one module, one row, one enum arm" (plus the `HopType::ALL` entry and
the glossary entry), and GLOSSARY.md's family vocabulary is reconciled to the
landed code.

HEAD at claim and at sign-off: `014a11134` (unchanged). Tree clean at claim;
only my two touched files dirty at sign-off.

## What changed

### `docs/architecture/adding-a-hop-family.md` (rewritten, 230 lines)

The eight-site census is **removed and marked SUPERSEDED** in the intro — the
explicit reason ADR-071 exists ("a conductor for an eight-site change is
evidence of a wide seam"). The new conductor:

1. **The change census** — one new `src/mixed/families/<family>.rs` (registered
   in `src/mixed/families/mod.rs:4`) plus the ADR-071 D4 residual sites.
2. **The family module — the export surface** — state slice, gate projection
   (`to_math`), V2-equivalent projection, simulate triple, solve entry on the
   shared golden template, envelope lines; anchored to all five landed modules
   (`cl.rs`, `solidly.rs`, `balancer_weighted.rs`, `balancer_stable.rs`,
   `curve_stableswap.rs`). The CL module's `refine_bounds` seam and the
   `RefineBound` kernel type are named here.
3. **The dispatch row** — `FamilyRow`'s four fn pointers, the two rows
   (narrow accept + wide refusal), the load-bearing order, and the
   `family_row` / `family_row_for` set-membership selection.
4. **The residual central sites** — one `HopType` variant + one `HopType::ALL`
   entry, one `ResolvedHop` arm (+ `hop_type` + the four non-CL accessor arms),
   one `HopMath` arm (+ the one-line `family_row()` delegation). Explains why
   `HopType::ALL` is required: the pin iterates `ALL`, so a variant missing from
   `ALL` escapes the pin (`assert_all_variants_listed` counts it; `expected_entry`
   restores the compiler's wildcard-free nag).
5. **The soundness stances** — concave? entry tangent or reserve-cap-only? where
   does the leaf's arithmetic wrap? With the *why* pointers to
   `stableswap-gate-bounds-and-solve-method-evaluation.md` §1/§4/§5 kept intact.
6. **The acceptance surface** — the pin, the shared dominance harness
   (`DominanceFamily` / `dominance_case` / `run_dominance_suite`), the drift net,
   and the family module's own `mod tests` (ADR-071 D6).
7. **Worked checklist** and an appendix recording the superseded eight-site
   census as prose (no stale anchors).

The old doc's stale citations (`solidly_stable_lines` `:345`,
`solidly_stable_arithmetic_faithful`, `FamilyArm`, `hop_lines_and_cap_cached`,
`GLOSSARY.md:577`) are gone; the rewrite cites the landed paths.

### `GLOSSARY.md`

- **`Family module`** (:437) — reconciled to the landed code: added the gate
  projection (`to_math`) to the owned list (each landed module owns its
  `*_to_math`), and corrected the residual-site count from "one dispatch row and
  one `ResolvedHop` / `HopMath` enum arm" to "one dispatch row and one enum arm
  in each of `ResolvedHop` and `HopMath`" (ADR-071 D4 = three). The
  envelope-lines gloss now reads "its rise line beside its cap, or a CL family's
  tangent fan" (CL contributes a fan, not a rise).
- **`Concentrated Liquidity Kernel`** (:524) — verified accurate as landed: the
  kernel is the solver crate's `cl` tree, carries no envelope vocabulary, and the
  fan is derived by the family module's bound side. No change.
- **`Refine bound`** (new, :543, under "Piecewise CL solving") — the walk-side
  refine-window bound in the kernel's own vocabulary (`(a + b·x)/c ≥ best`,
  `c > 0`), the seam translation via `refine_bounds`, with the house-style
  `_Avoid_` line. This is the named concept that was absent.
- **`Profit envelope`** (:621) — **out-of-named-scope addition, flagged for
  review**: the entry enumerated four families but omitted Balancer V2 weighted.
  Added one clause ("A Balancer V2 weighted hop contributes its entry tangent …
  beside its asymptotic reserve cap.") to match the landed `HopMath::Weighted`
  bound. One-clause docs-only fix; revert if the manager wants strict scope.

## Anchors spot-checked

Every `file:line` anchor in the rewritten conductor was checked mechanically
against the tree at `014a11134`: **79/79 resolve** to the cited token on the
cited line (script asserted the expected symbol substring at each line). The set
covers all five family modules, `mixed/mod.rs`, `mixed/solve.rs`,
`profit_envelope/derive.rs` / `capture.rs` / `gate.rs`, `cl/window.rs`, and
`tests/profit_envelope_tests.rs` / `tests/common/mod.rs`. The evaluation-doc
section anchors (§1/§4/§5) were checked against its heading list and match.

## Gates (scoped, foreground, green)

- `npx --yes markdownlint-cli2 "docs/architecture/adding-a-hop-family.md"
  "GLOSSARY.md"` → **0 issues** (the repo's `.markdownlint.yaml` config is
  picked up automatically). Ran the check-only form, not `just lint-markdown`'s
  whole-tree `--fix` sweep (that is the manager's).

No Rust/Python touched; no cargo/pytest gate in scope.

## Foreign activity

None. HEAD is still `014a11134`; `git status --porcelain` shows only the two
docs files I touched. No foreign commits or foreign working-tree edits observed.

## Out of scope / not touched

- The open solver bugs OJ2MMF / YYHWVQ / A47GNH: not referenced by the
  conductor (no doc claim depends on them).
- No code change.
