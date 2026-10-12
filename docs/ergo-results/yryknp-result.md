# YRYKNP — consolidate the stableswap gate-bounds / solve-method findings into an evaluation doc

Status: **DONE.** Docs-only chunk. Tree left dirty for review; nothing committed.

## What changed

Three files, no code, no tests:

1. `docs/architecture/stableswap-gate-bounds-and-solve-method-evaluation.md` (**new**) —
   the durable evaluation doc consolidating the four findings plus the drift-net
   methodology.
2. `GLOSSARY.md` — one cross-link appended to the "Profit envelope" entry (same
   paragraph, no stray blank lines).
3. `rust/crates/engine/degenbot-solvers/src/profit_envelope/mod.rs` — one
   cross-link paragraph in the module docs (`//!` header), after the soundness
   bullets.

## The doc's outline

- **Intro** — what the gate is and why the stableswap families needed their own
  derivation.
- **Verdict (TL;DR)** — the five one-line conclusions.
- **§1 Solidly-stable: the output curve is concave, so the gate carries its entry
  tangent** — the invariant derivation (`d/dt` of the marginal rate
  `3(t²−1)²/(1+3t²)² ≥ 0`, `t` strictly decreasing in the input), the entry-slope
  formula in native units, the fee-floor slack term (`~1e12` native units on a
  fair 6/18 pool, measured `9.3e11` at slope `9.997e11`) that dominates the naive
  `ceil(4·decimals_out/ONE)` allowance, the two-term allowance, and the
  faithful-arithmetic window guard (scaled reserves `≥ 2^60`, pre-trade `calc_k`
  inside u256, `reserve_out·ONE` inside u256, every post-trade `calc_d` term
  inside u256 across `[0, reserve_out]`). Evidence: ~250k dominance assertions
  over 9,600 shapes against the real leaves; skip count `0 → 32` on a 64-path
  synthetic corpus. Decision + revisit triggers.
- **§2 The `t`-parametrized Solidly solve: designed, measured, rejected** — the
  design, the measured `44.3 ms` vs `542 µs` (81×, manager-reproduced) on an
  18-path corpus, the root cause (a U2048 fourth-root probe costs more than the
  leaf's U256 Newton solve), the nil quality delta (288 paths, 143 profitable, 93
  strictly better by a few wei, 0 regressions; 7 vs 36 path sims), the verdict
  (worst observed residual 1 wei; golden-section effectively optimal), the
  salvaged regression test (`±8,000` native units at unit resolution, 8-wei
  tolerance, `PROFITABLE_SIZE = 143`), and the disposition (session-scratch
  reference copy; design record in the per-task result doc). Decision + revisit
  triggers.
- **§3 Curve stableswap: measured non-concavity keeps the reserve cap** — the two
  pinned counterexamples with the corrected magnitudes (Standard/Standard: 1
  native unit at x≈9.88e11; Variant1-y/Standard-D: ~9.799e23 native units at
  x=1), why a single unit is still an unsound skip, and the corrected-magnitudes
  note (the earlier "hundreds" / "1e5+ Variant1/VariantAlpha" claims were wrong;
  Variant1/VariantAlpha is concave). Decision + revisit triggers.
- **§4 Balancer V2 stable: an exact curvature test and a zero-allowance tangent**
  — the closed-form curvature test (`G = (x+y+k)² + (y−x)(y+k)`, concave iff
  `2y+k > 0` and `G ≥ 0`, evaluated exactly in I512 with `k` rational), the entry
  marginal rate, the derived zero allowance (every leaf rounding pool-favoring),
  validated at measured `min_margin = 1` over ~23k assertions, the guard/shape
  test, and the borrowed-slice hot-path note. Decision + revisit triggers.
- **§5 The drift net: pinned pre-refactor snapshots for behavior-identical
  refactors** — the methodology (pre-refactor literals verified against the
  pre-refactor tree in an isolated worktree), the evidence (four arms → one
  shared template; 4×24 = 96 pinned pairs byte-identical, 0 wei drift, net −254
  lines), and the salvaged brute-force corpus as the optimality analogue.
  Decision + revisit triggers.
- **§6 Cross-references** — code paths, module docs, glossary, related
  evaluations, tests.

Style follows `docs/architecture/mobius_v3_ending_range_enumeration_evaluation.md`
and `docs/architecture/env-read-gate-scanner-evaluation.md`: each finding states
its DECISION and the evidence that backs it, plus what would justify revisiting
it. No task/epic IDs anywhere (verified by grep).

## Where it is cross-linked from

- `GLOSSARY.md`, "Profit envelope" entry — link appended to the entry's closing
  paragraph.
- `rust/crates/engine/degenbot-solvers/src/profit_envelope/mod.rs` — module docs
  (`//!`), a new paragraph after the soundness bullets.

The doc also cross-links to `GLOSSARY.md`, the derivation docs in
`derive.rs`, the shared solve template in `mixed/solve.rs`, the CL evaluation doc,
and the three test files.

## Notes on interpretation

- The brief's settled shape names the "`profit_envelope` module docs" as a
  cross-link target; the extra context also says "do not touch anything under
  `rust/`". The only `profit_envelope` module docs live under `rust/`. I treated
  the acceptance criterion (cross-linked from the module docs) as authoritative
  and made a doc-comment-only edit to `mod.rs` — no code, no tests. Flagging the
  tension for the manager.
- The brief asks to note that "the design record lives in the gmdx5v result doc".
  The style rule forbids task IDs anywhere, so the doc points at the per-task
  result docs under `docs/ergo-results/` and names the record by its title
  ("simulation-free `t`-parametrized solve for one-Solidly-hop mixed paths")
  rather than by ID.

## Gates run (foreground)

| Gate | Result |
| --- | --- |
| `npx --yes markdownlint-cli2 docs/architecture/stableswap-gate-bounds-and-solve-method-evaluation.md GLOSSARY.md` | **PASS** — "Summary: 0 issues in 0 files" (markdownlint-cli2 v0.23.3 / markdownlint v0.41.1, repo `.markdownlint.yaml`) |

No Rust gates (no Rust code touched — the `mod.rs` edit is a doc comment only).
The repo's whole-tree markdown sweep (`just lint-markdown`, `npx --yes
markdownlint-cli2 --fix "**/*.md" ...`) is a whole-workspace sweep and is not a
dispatched-worker gate; it was not run.

## Foreign activity

None observed. `git status --short` for the whole run showed exactly:

```
 M GLOSSARY.md
 M rust/crates/engine/degenbot-solvers/src/profit_envelope/mod.rs
?? docs/architecture/stableswap-gate-bounds-and-solve-method-evaluation.md
```

The tree was clean at claim time; the concurrent worker editing Rust test files
and `src/profit_envelope/` left no visible modification during this run. Nothing
outside the three files above was touched or reverted.
