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
| V2-V2 single | Rust Möbius | 0.19μs | **1021x** |
| V2-V2 batch 1000 | Rust Batch Möbius | 0.09μs/path | **2155x** |
| V2 multi-hop | Python Möbius | 1.5μs | **129x** |
| V3 single-range | Möbius closed-form | ~0.86μs | **453x** |
| V3 multi-range (1 V3 hop) | Piecewise-Möbius (Rust) | **~9μs** | **43x** |
| V3 multi-range (1 V3 hop) | Piecewise-Möbius (Python) | ~50μs | **8x** |
| V3-V3 multi-range (2 V3 hops) | Rust V3-V3 solver | **~1-6μs** | **15-390x** |
| V3-V3 complex (fallback) | Brent | ~390μs | baseline |
| Balancer N=3 | Eq.9 closed-form | ~576μs | — |

### Rust vs Python Performance

**Key Finding**: Moving piecewise-Möbius to Rust achieved **5-10x speedup**, but Python-Rust data marshalling is now the bottleneck.

| Component | Python Only | With Rust | Speedup |
|-----------|-------------|-----------|---------|
| Raw computation | ~100μs | **~1μs** | **100x** |
| End-to-end solve | ~50μs | **~9μs** (profitable) | **5.5x** |
| End-to-end solve | ~500μs | **~140μs** (rejected) | **3.5x** |

**Breakdown of ~9μs solve:**
- Hop conversion: ~1.0μs
- Sequence building: ~1.9μs
- Rust solve: ~0.6μs
- Python dispatch overhead: ~5.5μs

**Lesson**: Raw Rust is 100x faster, but Python overhead limits end-to-end gain to 5-10x.

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
4. **Unified `ArbSolver`** — Single dispatcher selects optimal method (Mobius → Piecewise → V3-V3 → Solidly → Newton → Brent)
5. **Rust Acceleration** — Sub-microsecond solves with EVM-exact integer arithmetic
6. **Full AMM Coverage** — V2, V3, V4, Aerodrome, Balancer weighted, Solidly stable
7. **V3-V3 Validated** — 34 tests (28 Python + 6 Rust) with three-layer validation against Brent and V3 integer math. Range bounds bug found and fixed.

## Test Status

**474 optimizer tests passing, 9 skipped** (as of 2026-04-16)

Includes 28 V3-V3 accuracy tests, 6 Rust unit tests for range bounds validation, and 161 Rust unit tests (all passing).

### Rust Test Fixes (2026-04-16)

7 Rust unit tests were failing due to three root causes:

1. **Symmetric two-hop reserves are always unprofitable** — Mirrored reserves (r₂=s₁, s₂=r₁) give K/M = γ² < 1, meaning fees always exceed any marginal rate advantage. Tests `test_two_hop_profitable`, `test_simulate_path_matches_mobius_output`, and `test_profitability_check_free` used such reserves. Fixed with asymmetric reserves where pools disagree on price.

2. **V3 range capacity exceeded** — Test inputs (1e15, 1e10) far exceeded V3 tick range capacity (~1.1e14), pushing estimated final sqrt price out of range. Fixed by reducing inputs to stay within capacity.

3. **Float64 boundary precision** — `compute_range_constrained_max_input` lacked the `(1 - 1e-12)` shrink factor, and range validation used strict `contains_sqrt_price()` without tolerance. At boundary, float64 rounding pushed the final sqrt price barely outside range. Fixed by adding shrink factor and tolerance-based validation (`eps = 1e-12 * sqrt_price_current`), consistent with `solve_v3_candidates`. Also improved golden section convergence by scaling `abs_tol` to initial interval width.

## Citation

The Möbius transformation approach for multi-hop AMM arbitrage is documented in:
> Hartigan, J. (2026). "The Geometry of Arbitrage: Generalizing Multi-Hop DEX Paths via Möbius Transformations."

The Balancer multi-token solver implements Equation 9 from:
> Willetts, M. & Harrington, T. (2024). "Closed-form solutions for generic N-token AMM arbitrage." arXiv:2402.06731
