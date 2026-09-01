# Loop-16: profit-envelope gate — final summary

## Delivered

1. **T1 census** (committed 4c67e2489): stage-1 evals 69% / sort 28% / sweep 3%; hull reduce 43% / sort 5% / stack 52%; saturation falsified as a driver (0.1%). I512 division = 109ns of a 126ns eval.
2. **T2 approx-key ordering** (4c67e2489): f64 ratio keys with a 1e-6 band and exact fallbacks (`approx_cmp_ceil` models the ceiling's 1-absolute shift; `approx_cmp_ratio` for slopes), saturation channels modeled by min-clipping. Caught and fixed one real hole via the differential (ceil semantics on small ratios). Fast limb bit-length + shifts replace `Signed::bits()` + U512 division in reduce. Hull stack: bp-pair reuse, mult-based pop compare, band-gated same-slope checks.
3. **T3 Möbius-hop chains** (8d6f06a4 + this): V2 hops join the prefix cache chain (content-hash keys + full-content fingerprints); the domain was added then CORRECTLY removed from the key after live telemetry showed prefix_hits=0 — cross-domain reuse shifts tightness but never soundness (every stored line globally dominates the true output; smaller-domain entries are subsets → looser → skips less; larger → supersets → tighter, each skip still justified).
4. **T4 static-TLS fix**: 24 separate timer thread-locals (loop-16 diagnostics + legacy counters) exhausted the dlopen static-TLS surplus ("cannot allocate memory in static TLS block" on import). Consolidated into ONE `GateTls` struct behind a single thread-local, removed the served-their-purpose census + interior split timers. TLS segment 1896→1608 bytes; import restored. The bench prints remain (s1=/hull= core columns).

## Measured

- gate_bench (76 heavy paths, sum of per-path medians): 54.2ms → ~30-34ms (−38..−45%); heaviest path 1.88ms → ~0.95-1.05ms (−45%).
- Live dry-run: per-path stage1 217μs → 135μs (−38%), hull 86μs → 47μs (−45%), compose 438μs → 334μs (−24%). Prefix hits restored (3.9k-6.1k per block) and now flow through V2 hops too.
- Suite 148 green; CL goldens 104/104; E2E mixed replay byte-identical to the pre-loop-16 baseline (timing-stripped diff = 0 over 369 paths) across all changes.

## Follow-ups

- The hull stack (~234ms/session, per-push bp ceil_div) and s1 sort (~180ms) are the residual gate costs.
- Sampled caps could absorb the remaining s1 volume if tightness margin is acceptable.
