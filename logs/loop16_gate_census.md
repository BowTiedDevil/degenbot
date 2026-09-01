# Loop-16: profit-envelope gate — census + T2 optimization

## T1 census (DEGENBOT_GATE_CENSUS=1, gate_bench heavy mixed fixtures)

Session totals over the heavy fixtures (9 reps):
- prune_calls=19,089; in_lines=3,220,407; s1_survivors=846k; hull_survivors=546k
- in_buckets(<=8,<=64,<=256,<=1k,<=4k,>4k)=[5580, 8199, 2979, 558, 1773, 0]
- hop_lines all <=64; lines2 all <=64 → product sets (avg ~1800 lines) carry essentially all line volume
- eval saturation at x=upper: 0.1% — NOT the cost driver (hypothesis falsified)

Stage-1 interior split (evals / sort / sweep): 577ms / 232ms / 26ms — the endpoint evals dominate.
Isolated eval cost: 126ns/eval of which the I512 division is 109ns.

Hull interior split after the first fix round: reduce 232ms (Signed::bits() + division-based shifts), sort 25ms, stack 362ms (4-8 wide mults + ceil_div per push).

## T2 changes (all byte-identical by construction)

1. **Approximate ordering keys with exact fallbacks**: f64 ratio keys (~2^-48 relative error) with a 1e-6 band (`approx_cmp_ceil` for ceil-division endpoint keys — the margin covers the ceiling's 1-absolute shift; `approx_cmp_ratio` for pure-ratio slope keys). Outside the band the approximation orders confidently; inside it the exact comparator runs. Saturation channels modeled by min-clipping at `max_f` exactly as `eval` saturates.
2. **Stage-1 endpoint evals** replaced by key construction (109ns eval → ~10ns key); exact evals only on band collisions. Sort and min-sweep use the same discipline. Evals 577→105ms.
3. **Hull**: limb-scan bit-length + shift-based `ceil_shr_i512` (reduce 232→50ms); approx slope keys gate the same-slope cross-mult check; first pop-loop iteration reuses the computed bp pair; pop compare via `ceil(n/d) <= B ⟺ n <= B·d` (multiplication instead of division).

## Verification

- Differential sentinel: randomized line sets (256 seeds) + adversarial ceil/saturation boundary cases vs a FROZEN REFERENCE copy of the pre-optimization prune — byte-identical survivor sets (caught and fixed a real hole during development: ceil semantics on small ratios).
- E2E A/B: mixed_solve_replay over 369 paths, normalized (timing-stripped) diff vs the pre-optimization build = **0 lines** — identical walk tuples, gate decisions, and statuses.
- Suite 147 passed (dropped the temporary micro-bench test); CL solve replay goldens 104/104, deterministic 104/104.

## Measured (gate_bench, sum of per-path medians over 76 paths)

| | baseline | after | Δ |
|---|---|---|---|
| session total | 54,202us | 29,933–34,306us | −37..−45% |
| heaviest path | 1,881us | ~1,050–1,190us | −38% |


Hull stack (234ms) and s1 sort (180ms) remain the resident costs; the slope-sort fallbacks and the per-push bp division are the next targets if more is needed.