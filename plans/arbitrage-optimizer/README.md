# Arbitrage Optimizer Research

Research and development of high-performance arbitrage optimization methods for DEX trading. This effort achieved **1021x speedup** over the original Brent baseline through mathematical innovations (Möbius transformations) and systems optimization (Rust).

## Quick Start

### Production Usage
```python
from degenbot.arbitrage.optimizers.solver import ArbSolver, Hop, SolveInput

solver = ArbSolver()
hops = (
    Hop(reserve_in=r0_in, reserve_out=r0_out, fee=fee0),
    Hop(reserve_in=r1_in, reserve_out=r1_out, fee=fee1),
)
result = solver.solve(SolveInput(hops=hops))
if result.success:
    print(f"Optimal: {result.optimal_input}, Profit: {result.profit}")
```

### Performance by Use Case

| Arbitrage Type | Recommended Method | Time | Speedup |
|----------------|-------------------|------|---------|
| V2-V2 single | Rust Möbius (via ArbSolver) | 0.19μs | **1021x** |
| V2-V2 batch 1000 | Rust Batch Möbius | 0.09μs/path | **2155x** |
| V2 multi-hop | Rust Möbius (via ArbSolver) | ~1.5μs | **129x** |
| V3 single-range | Möbius closed-form | ~0.86μs | **453x** |
| V3 multi-range (1 V3 hop) | Piecewise-Möbius (Rust) | **~9μs** | **43x** |
| V3-V3 both single-range | V3-V3 Rust solver | **~0.19μs** | **2053x** |
| V3-V3 multi-range | V3-V3 Rust solver | **~10-50μs** | **8-39x** |
| V3-V3 complex (fallback) | Brent | ~390μs | baseline |
| Balancer N=3 | Eq.9 closed-form | ~576μs | — |

### ArbSolver Architecture

`ArbSolver.solve()` is a thin wrapper that defers to Rust for all supported path types:

```
ArbSolver.solve()
    ├── RustArbSolver.solve_raw()  ← flat int array (default, V2/single-range V3)
    │     └── Möbius + U256 integer refinement (~3.9μs end-to-end)
    ├── RustArbSolver.solve()      ← object-based (V3 multi-range, V3-V3)
    │     ├── V3 multi-range (1 hop) → solve_v3_sequence (~9μs)
    │     └── V3-V3 (2 hops) → solve_v3_v3 (~0.19μs single, ~10-50μs multi)
    ├── PiecewiseMobiusSolver (Python fallback for V3 scale mismatches)
    ├── SolidlyStableSolver (~15-25μs)
    ├── BalancerMultiTokenSolver (~576μs)
    └── BrentSolver (~223μs, ultimate fallback)

ArbSolver.solve_cached([id0, id1])  ← Fastest path (no Python objects)
    └── RustPoolCache.solve()      ← HashMap lookup → Möbius + U256
            ~2.5μs end-to-end (V2-V2, EVM-exact)
```

**Key insight**: The Rust dispatch + merged U256 integer refinement + pool cache eliminates Python object overhead on the solve path. End-to-end V2-V2 via `solve_cached()` is ~2.5μs (Rust-only ~0.73μs), and profits are truly EVM-exact.

### Rust vs Python Performance

**Key Finding**: Merging integer refinement into RustArbSolver.solve() eliminated the second Python→Rust conversion. The RustPoolCache (Item #20) eliminated all Python object construction on the solve path by storing state in Rust at registration time. The remaining ~1.77μs overhead in `solve_cached()` is Python method dispatch + SolveResult construction.

| Component | Python Only | With Rust | Speedup |
|-----------|-------------|-----------|---------|
| Raw computation | ~100μs | **~1μs** | **100x** |
| Integer refinement (merged) | ~3.7μs (float) | **~0.6μs** (U256) | **6x** |
| End-to-end V2-V2 (solve) | ~6.1μs | **~3.9μs** | **1.6x** |
| End-to-end V2-V2 (solve_cached) | — | **~2.5μs** | **2.4x** |
| Rust-only cache.solve() | — | **~0.73μs** | **8.2x** |

**Breakdown of ~2.5μs V2-V2 `solve_cached` (pool cache, fastest path):**
- Rust HashMap lookup: ~0.03μs
- Rust float Möbius solve: ~0.2μs
- Rust U256 integer refinement: ~0.4μs
- ArbSolver method dispatch + SolveResult: ~1.87μs

**Breakdown of ~3.9μs V2-V2 `solve` (standard path):**
- Rust float solve: ~0.2μs
- Rust U256 integer refinement: ~0.4μs
- Flat list construction: ~0.1μs
- Per-item PyO3 extraction (8 items): ~0.8μs
- Other Python dispatch overhead: ~2.4μs

**Lesson**: The pool cache (`solve_cached`) is the fastest path because it passes only integer IDs to Rust — no list construction, no per-item extraction. The remaining bottleneck is Python method dispatch + SolveResult construction (~1.87μs). To go below ~1μs, the Rust cache would need to be called more directly, bypassing ArbSolver's Python wrapper.

## Documentation

| Document | Purpose |
|----------|---------|
| **[PRODUCTION_GUIDE.md](PRODUCTION_GUIDE.md)** | How to use the optimizers in production |
| **[NEXT_STEPS.md](NEXT_STEPS.md)** | Current status and recommended next steps |
| **[RESEARCH_HISTORY.md](RESEARCH_HISTORY.md)** | Complete research history, lessons learned, and technical deep dives |

## Archive

Historical documents are preserved in [`archive/`](archive/):
- Day-by-day progress logs
- Phase-by-phase implementation records  
- Early research notes and alternative approaches
- Superseded planning documents

## Key Achievements

1. **Möbius Transformation** — Zero-iteration closed-form solution for V2 multi-hop paths
2. **Piecewise-Möbius for V3** — Multi-range V3 support with 10 optimizations (~3.75x speedup)
3. **V3-V3 Rust Solver** — Two-V3-hop paths with simultaneous tick crossings via golden section search
4. **Unified `ArbSolver`** — Thin Rust wrapper dispatches all supported types in a single call; Python fallback for Solidly/Balancer/Brent
5. **Rust Acceleration** — Sub-microsecond solves with EVM-exact integer arithmetic
6. **Merged Integer Refinement** — `RustArbSolver.solve()` does float solve + U256 integer refinement in one call, eliminating second Python→Rust conversion
7. **Raw Array Marshalling** — `RustArbSolver.solve_raw()` accepts flat int list, eliminating Python object construction
8. **Pool State Cache** — `RustPoolCache` + `ArbSolver.solve_cached()` — pool states registered in Rust, solved by ID reference (~2.5μs, no Python objects on solve path)
9. **Full AMM Coverage** — V2, V3, V4, Aerodrome, Balancer weighted, Solidly stable
10. **V3-V3 Validated** — 34 tests (28 Python + 6 Rust) with three-layer validation against Brent and V3 integer math. Range bounds bug found and fixed.

## Test Status

**694 tests passing, 9 skipped** (as of 2026-04-18)

Includes 17 raw array marshalling tests, 18 pool cache tests, 14 merged integer refinement tests, 15 Rust integer refinement tests, 28 V3-V3 accuracy tests, 6 Rust unit tests for range bounds validation, 49 total Rust Möbius optimizer tests, and 209 total Rust unit tests (all passing).

## Citation

The Möbius transformation approach for multi-hop AMM arbitrage is documented in:
> Hartigan, J. (2026). "The Geometry of Arbitrage: Generalizing Multi-Hop DEX Paths via Möbius Transformations."

The Balancer multi-token solver implements Equation 9 from:
> Willetts, M. & Harrington, T. (2024). "Closed-form solutions for generic N-token AMM arbitrage." arXiv:2402.06731
