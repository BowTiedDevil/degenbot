## Loop-11 FWB3SH — walk-refine probe budget: close-out

State at close: **zero code delta** (mobius_v3_int.rs identical to a3aa4e76a baseline). Loop discipline produced two well-measured negatives.

| Task | Change | Evidence | Verdict |
|------|--------|----------|---------|
| Q6DMHV | RED strict-dominance + baseline | guard deep refine 178 (ternary 112 + grid 66); 3-hop 259 (160+99) | recorded |
| D4DEBJ | golden-section narrowing | deep-guard probes 178→117 (−34%), profit equal 173876084 | green locally |
| 4BZNEU | hard-cutover A/B | heavy replay F2 gate 687 vs 692 golden-match; NEW real losses: 44361 −10.13M wei, 44366 −45.56M, 44367 −137.61M (window-edge bumps masked by staircase plateaus) | **REJECTED, reverted** |
| ECWSJM | climb-stop neighbor gate | unified coarse grid; corpus refine total 108,873 == legacy 108,873 (every gate opens; no skips exist on captured paths) | **NEUTRAL, reverted** |
| UJRJEO | audit | 11/11 lib+integration result files green; deterministic replay gate at known baseline | completed |

Net loop-11 outcome: classic ternary + direction-dependent... both candidate probe cuts falsified by the correct oracles (heavy replay F2 gate / corpus equality). The live 74k-ternary budget is irreducible AT the current final bracket (1e6-wei) without accepting either lost profit or zero savings.

## Consequence for live finance

No change; gate.prune_stage1_us + walk refine splits stay the sentinels. A future angle for the ternary budget:
- raise REFINE_BRACKET_WEI + REFINE_GRID_POINTS in lockstep on geometries where the final window is piece-CONCAVE-measured (needs per-window curvature evidence the oracle can certify):
  that is the only lever left with profit-ε intact;
- or cache refine argmax across AMM state and re-derive campaign-only deltas on state channel updates (reserve-stamp keyed memo: same lines2 as compose cache).
