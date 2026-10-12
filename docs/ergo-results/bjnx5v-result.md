# BJNX5V — the adding-a-hop-family checklist, and the dangling patch pointer

Status: **DONE.** Tree left dirty for review; nothing committed. Scoped lint gate
green; no Rust touched.

Docs-only chunk. Added the implementer's checklist for adding a hop family to
the mixed solver and the profit-envelope gate, and reworded one dangling
scratch-pointer passage in an earlier result doc. No code, no tests.

## What changed

- **New `docs/architecture/adding-a-hop-family.md`** — the HOW to the
  stableswap evaluation doc's WHY. Eight-site census with `file:line` anchors,
  the four load-bearing obligations, a worked checklist, a seam-inventory
  appendix, and a brief-vs-tree discrepancy log. Cross-linked to
  `docs/architecture/stableswap-gate-bounds-and-solve-method-evaluation.md`
  and to the sibling runbook `adding-a-strategy.md`.
- **`docs/ergo-results/gmdx5v-result.md`** — one blockquote passage reworded
  (lines 43-44). Nothing else in that doc touched.

## The checklist's outline

1. **The site census** — eight numbered sites, each with the current file and
   what belongs there.
2. **The obligations** — four:
   - *drift-net discipline*: pin the arm's `(optimal_input, profit)` on a
     synthetic corpus before any refactor, verified against the pre-refactor
     tree in an isolated worktree;
   - *dominance proptest*: the oracle is the real math leaf, never a
     reimplementation, with non-vacuity counters (`shapes`, `asserts`,
     `rise_active`, `guard_fired`) and a strategy that shrinks;
   - *faithful-arithmetic guard*: every tangent bound carries the leaf's
     rounding slack, derived and measured, dropped (cap-only — never `None`,
     never looser) wherever the leaf's integer arithmetic could wrap across the
     whole evaluation window;
   - *one labelling site, one arm-selection site*: `family_label` and
     `FAMILY_DISPATCH`.
3. **Worked checklist** — nine steps, site-ordered.
4. **Appendix — site inventory** — the seam table.
5. **Discrepancies between the brief and the landed sites.**

Each obligation carries an anchor link into the evaluation doc for its why.

## The verified site census

Verified against the tree as it stands (read, not trusted from the brief). All
anchors re-checked after writing; the concurrent `rust/` worker's edits did not
shift any cited line.

| Site | File:line (verified) |
|---|---|
| `HopType` variant | `rust/crates/engine/degenbot-solvers/src/mixed/mod.rs:61` |
| `TryFrom<&Identity>` arm | `src/mixed/mod.rs:100` |
| `ResolvedHop` variant | `src/mixed/mod.rs:330` |
| `hop_type()` arm | `src/mixed/mod.rs:361` |
| non-CL accessor `None` arms | `src/mixed/mod.rs:384,400,415,429` |
| solve arm (`FamilyArm` + dispatch + `solve_<family>_path_int`) | `src/mixed/solve.rs:222,336,588,938,1085,1164` |
| simulators + `*_hop_v2_equiv` | `src/mixed/solve.rs:819,844,857,882` (+ weighted/stable/Curve triples) |
| `hop_math_for` | `src/mixed/solve.rs:126` |
| `FAMILY_DISPATCH` row | `src/mixed/solve.rs:263` |
| `HopMath` variant | `src/profit_envelope/derive.rs:28` |
| `family_label` arm | `src/profit_envelope/derive.rs:188` |
| line-derivation arm | `src/profit_envelope/derive.rs:1103` |
| corpus builder | `tests/common/mod.rs:30,62,89,114,137,169` |
| "Profit envelope" glossary entry | `GLOSSARY.md:577` |

**What the brief missed.** The brief's "eight-site, five-file" prose enumerates
six bullets; the fifth file is `GLOSSARY.md`, whose "Profit envelope" entry
gains one sentence per family (confirmed by the two entry-tangent commits).
That glossary site is not named in the brief. The brief's six bullets expand to
eight concrete edit locations once the `HopType` variant and its `TryFrom` arm
are counted separately, and the solve arm's simulators, projection, and
`hop_math_for` are counted separately — which is the grouping the doc's census
uses.

**One brief approximation corrected.** The brief wrote the scratch module as
`sr2-solidly-t.rs` (hyphen); the actual file name is `sr2-solidly_t.rs`
(underscore). The reworded passage uses the real name.

## The reworded pointer passage

Old (`docs/ergo-results/gmdx5v-result.md`, lines 43-44):

> Rejected-patch reference copy: `.scratch/sr2-rejected.patch` +
> `.scratch/sr2-solidly_t.rs` (session-scratch, not committed).

New (lines 43-47):

> Design record: this doc IS the record of the rejected patch. The patch and
> module lived only as session-local scratch (`.scratch/sr2-rejected.patch`,
> `.scratch/sr2-solidly_t.rs`) that was never committed and does not survive;
> the derivation, the measured regression, and the disposition above are the
> durable reference.

The passage no longer points at the scratch files as the "reference copy"; it
states the design record carried in the doc is the record and the scratch copies
were session-local. Nothing else in that doc changed (diff is exactly the one
blockquote replacement).

## Gates

- `npx --yes markdownlint-cli2 docs/architecture/adding-a-hop-family.md docs/ergo-results/gmdx5v-result.md`
  → `Summary: 0 issues in 0 files`, `EXIT=0`. Re-run after the appendix edits:
  still `0 issues`. **PASS.**
- No Rust gates run (no Rust touched).

## Foreign activity

The tree carries uncommitted edits by the concurrent dispatched worker, under
`rust/` — left completely alone:

- `rust/crates/engine/degenbot-solvers/src/profit_envelope/derive.rs`
- `rust/crates/engine/degenbot-solvers/tests/common/mod.rs`
- `rust/crates/foundation/degenbot-math/src/solidly/mod.rs`

Their line numbers were stable across this run; every anchor in the new
checklist was re-verified against the tree after writing.
