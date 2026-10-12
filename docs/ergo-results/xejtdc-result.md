# XEJTDC — unify the `VerifySweep` orders with parity evidence

## What changed

- `rust/crates/engine/degenbot-solvers/src/mixed/solve.rs`: deleted `enum
  VerifySweep`, deleted `outward_sweep`, and dropped the `sweep` argument from
  `solve_golden_section_path_int` / `verify_best`. All four stable arms now share
  one integer-verification order — the ascending `center-3 ..= center+3` scan in
  `ascending_sweep`.
- `rust/crates/engine/degenbot-solvers/tests/stable_golden_drift_net.rs`: the
  Balancer stable and Curve snapshots are re-pinned (those two arms moved off
  the outward walk); the module doc and each re-pinned array carry the
  provenance note.
- `docs/architecture/adding-a-hop-family.md`: the Site 4 sentence that described
  the template as taking a `VerifySweep` argument was updated — the template's
  family-specific parts are now just the two closures, and the ±3 order is
  shared.

## Measurement

Instrument: a **temporary** integration test (deleted before sign-off) that ran
each arm's corpus through the committed dispatch twice — once with the dispatch
as-is, once with all four arms' sweep argument flipped — and compared each
path's returned `(optimal_input, profit)` pairwise. For the Solidly arm the
288-path golden-parity matrix was also brute-forced at unit resolution (±8000
wei around the arm's answer, via the public `simulate_solidly_path`) to get the
discrete-optimum residual per order.

`delta = profit(baseline order) − profit(flipped order)`. Baseline was
Ascending for Solidly/weighted and Outward for stable/Curve; the flip inverted
all four.

### Per-arm order comparison (24-path drift corpora)

| Arm | baseline order | flipped order | worst \|Δ\| (wei) | mean \|Δ\| (wei) | paths with Δ≠0 | paths with different `optimal_input` |
| --- | --- | --- | --- | --- | --- | --- |
| Solidly | Ascending | Outward | 0 | 0.000 | 0 / 24 | 12 / 24 |
| Balancer weighted | Ascending | Outward | 0 | 0.000 | 0 / 24 | 18 / 24 |
| Balancer stable | Outward | Ascending | 1 | 0.042 | 1 / 24 (path 20) | 24 / 24 |
| Curve | Outward | Ascending | 0 | 0.000 | 0 / 24 | 24 / 24 |

Where the `optimal_input` differs, the profit is equal: the two orders land on
different points of a flat, equal-profit plateau.

### Solidly 288-path golden-parity matrix (residual vs. brute-force optimum)

| Order | profitable paths | worst residual (wei) | mean residual (wei) | residual histogram |
| --- | --- | --- | --- | --- |
| Ascending | 143 | 2 | 0.5524 | 0:74, 1:59, 2:10 |
| Outward | 143 | 2 | 0.5664 | 0:72, 1:61, 2:10 |

The two orders differ on 2 of 288 paths (262, 278), where the ascending scan is
+1 wei better (residual 1 wei lower). 70 of 288 paths report a different
`optimal_input` at equal profit.

## Decision

**Unify on the ascending `center-3 ..= center+3` scan.**

Justification: the two orders are within noise of each other on every arm — the
worst profit delta is a single wei, on one stable path, and every other arm is
bit-identical in profit. That triggers the settled rule: pick the simpler order.
Ascending is the simpler scan (a fixed candidate window, no moving-best state),
and it is also the order Solidly and weighted already used, so only two arms
move. The measurement gives ascending a weak strict edge on top of that: it is
never worse on any measured path, +1 wei better on 3 paths in total (2 Solidly,
1 stable), and its worst-case residual ties the walk's at 2 wei with a slightly
better mean (0.5524 vs. 0.5664). Nothing argues for the outward walk, so it is
deleted.

## Re-pinned snapshots

The Balancer stable and Curve arms moved from the outward walk to the ascending
scan, so their 24-path drift-net snapshots were re-pinned:

- **stable**: profit unchanged on 23/24 paths and **+1 wei** on path 20 (the
  ascending scan's answer); `optimal_input` shifts within the equal-profit
  plateau (−3 wei on 23 paths, +4 wei on path 20).
- **curve**: profit unchanged on all 24 paths; only `optimal_input` shifts
  (−3 wei on all 24 paths), again within the equal-profit plateau.

Solidly and weighted did not move and their snapshots are untouched.

Provenance: the drift-net module doc gained a paragraph recording that the
stable/Curve snapshots were re-pinned when the four arms were unified on the
ascending scan, that the re-pin rests on the measured order comparison above,
and that the input shifts are moves between equal-profit candidates on a flat
plateau. Each re-pinned array carries the same note beside it.

The Solidly golden-parity tolerance test is unchanged and stays green: the
Solidly arm's order did not move, and its measured worst-case residual is 2 wei
inside the 8-wei bound (expected ~1 wei).

## Gates

| Gate | Result |
| --- | --- |
| `cargo nextest run --locked --manifest-path rust/Cargo.toml -p degenbot-solvers` | **PASS** — 243 tests run, 243 passed, 3 skipped (includes `solidly_golden_parity::golden_arm_lands_within_tolerance_of_the_discrete_optimum` and all four re-pinned drift-net arms) |
| `cargo clippy --locked -p degenbot-solvers --all-targets --manifest-path rust/Cargo.toml -- --deny warnings` | **PASS** — exit 0, no warnings |
| `cargo fmt -p degenbot-solvers -- --check` (from `rust/`) | **PASS** — exit 0 |
| `npx --yes markdownlint-cli2 docs/architecture/adding-a-hop-family.md` | **PASS** — 0 issues (run because the doc edit is not covered by the three scoped Rust gates) |

## PENDING

None.

## Foreign activity

None observed. `git status --short` shows only this chunk's three files; no
other worker's edits or untracked files were present at any point.

## Notes for review

- `ascending_sweep`'s window is `center-3 ..= center+4` (its `end` is
  `center + radius + 1` with a `<=` loop), i.e. 8 candidates, not 7. This is
  pre-existing behavior, preserved verbatim; only the outward walk was removed.
- The temporary measurement harness was deleted; it left no file in the tree.
- The result doc's `docs/architecture/adding-a-hop-family.md` edit is the one
  change outside the two solver files. It is a direct consequence of deleting
  the enum (the checklist named it as a template argument); the PM can drop it
  if the doc is owned by another chunk, but the sentence was factually wrong
  after this change.
