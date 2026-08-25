# ADR-036: Do not re-land the V3/V4 window guards — the refined terminal subsumes them

**Status:** Accepted
**Date:** 2026-08-25
**Task:** — (F1 solver adversarial-review follow-up)

## Context

- A solver review cycle proposed "window guard" logic to stop the active-set
  walk's terminal refine from under-shooting a sharp bounded piece corner, and
  part of that work was subsequently reverted. Whether the V3/V4 window
  guards should be re-landed is a pending review item (the re-land branch
  lives on `degenbot/degenbot`, not this local clone).

- Independently, the F1 adversarial review surfaced a real instance of that
  under-shoot: a single-piece CL path whose exact unclamped smooth argmax
  overshoots the piece's chain-saturation corner, so the old `anchor ± 2`
  probe + break landed in the negative post-cliff region and the path
  silently returned `None` — skipping a measured 9.56e9 wei of profit. That
  is exactly the sharp bounded corner the window guards were meant to defend.

## Decision

Do not re-land the V3/V4 window guards. The corner safety they provided now
lives in the core refine path, in both cases the guards covered:

1. **Single-piece paths** — the F1 terminal refine (`305141729`,
   `a89cc1587`) probes the chain-saturation corner (and the wei below it) and
   refines the terminal window `[0, max(corner, anchor)]`, so the sharp
   bounded corner is always bracketed *by construction*, not by an added guard
   layer.
2. **Multi-piece paths** — `walk_refine_window` (with the coarsened 1e6-wei
   bracket, `56b3bdf21`) pins `hi` to the piece's right edge and resolves with
   a ternary + grid to profit-ε; a bounded downstream kink is bracketed via
   the smooth anchor and resolved to the coarsening ε, satisfying the same
   profit-ε contract the guards enforced.

Re-landing would re-introduce a superseded, parallel guard over a corner the
core refine now guarantees, adding review surface and dead code with no new
safety. (The "single-piece terminal now self-covers" framing is the reviewer's
own, agreed during the F1 exchange.)

## If revisited

Were a future review to want the guards back, encode the Q1 anchor-bound proof
as a regression: for any hop-j kink that is a true peak (right-marginal < 1
just past it, left-marginal ≥ 1 just before it), the unclamped smooth anchor is
≥ the kink input, so the refine window `max(corner, anchor)` contains it.
`single_piece_hop1_binding_kink_is_not_dropped` already pins this empirically
for the hop1 case; a hop0-corner sibling is
`single_piece_saturation_kink_is_not_missed`.

## Consequences

- V3/V4 guard branches stay un-merged; no re-land.
- Corner safety is enforced by the core refine and pinned by two regression
  tests (single-piece saturation corner + hop1-binding kink).
