# Alternatives Evaluation

Systematic evaluation of alternative optimization methods across all arbitrage classes.

## V2-V2 Arbitrage

**Current Optimal**: Möbius Transformation (Python 0.86μs / Rust 0.19μs, zero iterations, exact)

### Alternative 1.1: Newton's Method

| Criterion | Möbius | Newton | Assessment |
|-----------|--------|--------|-------------|
| Time (Py) | 0.86μs | 7.5μs | Möbius 8.7x faster |
| Time (Rust) | 0.19μs | N/A | Möbius dominates |
| Iterations | 0 | 3-4 | Möbius zero iterations |
| Convergence | Exact | Quadratic | Both exact |

**Recommendation**: Möbius preferred. Newton is still useful as fallback for non-constant-product paths.

### Alternative 1.2: Analytical Quartic Solution

Solve equilibrium equation directly without iteration.

| Criterion | Möbius | Analytical | Assessment |
|-----------|--------|-----------|-------------|
| Time | 0.86μs | ~3-5μs (est) | Möbius faster |
| Complexity | Low (O(n)) | High | Möbius simpler |
| Multi-hop | Yes (O(n)) | No | Möbius advantage |

**Recommendation**: **Superseded by Möbius**. Not worth implementing.

### Alternative 1.3: NumPy Vectorization for Batch V2

**IMPLEMENTED** — Both Newton and Möbius vectorized batch solvers exist.

| Paths | Serial Möbius | Vec Möbius | Vec Newton | Rust Batch |
|-------|--------------|------------|------------|------------|
| 100 | 320μs | 28μs | 53μs | 19μs |
| 1000 | 3229μs | 140μs | 528μs | 93μs |
| Per-path | 3.2μs | 0.14μs | 0.53μs | 0.09μs |

Möbius vectorized is **4.4x faster** than Newton vectorized at 1000 paths.

### Alternative 1.4: Rust Integer Möbius

**IMPLEMENTED** — uint256 arithmetic at 0.88μs, EVM-exact results.

| Criterion | Rust f64 | Rust u256 | Assessment |
|-----------|----------|-----------|-------------|
| Time | 0.19μs | 0.88μs | f64 4.3x faster |
| EVM-exact | No | Yes | u256 for validation |
| Not-profitable check | N/A | 0.32μs | Instant via K>M |

---

## V2-V3 Arbitrage

**Current Optimal**: Möbius (closed-form for known range, piecewise for crossing)

### Alternative 2.1: Binary Search for Active Tick + Newton

**IMPLEMENTED** — 2-4.5x faster than Brent. Superseded by Möbius for most cases.

| Method | Time | vs Brent | Status |
|--------|------|----------|--------|
| Möbius single-range | ~5μs | ~40x faster | ✅ Preferred |
| Möbius solve_v3_candidates | ~5-15μs | ~15-40x faster | ✅ Preferred |
| Möbius solve_piecewise | ~25μs | ~8x faster | ✅ Preferred |
| Binary+Newton (single) | 69μs | 4.5x faster | Fallback |
| Binary+Newton (3 ranges) | 136μs | 2.3x faster | Fallback |
| Brent baseline | 309μs | — | Last resort |

### Alternative 2.2: Dual Decomposition for V2-V3

**IMPLEMENTED** — See [`research/dual-decomposition.md`](research/dual-decomposition.md).

Best for multi-path simultaneous routing (5-12ms), not single-path speed.

---

## V3-V3 Arbitrage

**Current Optimal**: Brent (~390-500μs)

### Alternative 3.1: Simultaneous Tick Search

Find intersection of tick ranges, solve within common ranges.

| Criterion | Brent | Intersection Search | Assessment |
|-----------|-------|---------------------|------------|
| Time | 390-500μs | ~100-200μs | Potentially 2-5x faster |
| Tick handling | Automatic | Manual | Brent simpler |

**Recommendation**: **Medium priority**. V3-V3 arbitrage is less common; complexity may not justify speedup.

### Alternative 3.2: Gradient-Based Tick Crossing

**Recommendation**: **Low priority**. Experimental approach with uncertain benefits.

---

## Multi-Pool V2 Routing

**Current Optimal**: Möbius closed-form (~1-5μs)

### Alternative 4.1: Sequential Newton with Greedy Selection

| Criterion | Möbius | Greedy Newton | Assessment |
|-----------|--------|---------------|-------------|
| Time | 1-5μs | ~50-100μs | Möbius 10-50x faster |
| Optimality | Global | Local greedy | Möbius better |
| Multi-token support | Via dual decomp | No | Dual decomp advantage |

**Recommendation**: Möbius for single-path, dual decomposition for multi-path.

### Alternative 4.2: Linear Programming Relaxation

**Recommendation**: **Low priority**. LP relaxation loses accuracy without sufficient speedup.

---

## Mixed V2/V3 Arbitrage

**Current Optimal**: Möbius (single/multi-range) or Brent (complex V3-V3)

### Alternative 5.1: Hybrid Decomposition

CVXPY for V2, Brent for V3, iterate to convergence.

**Recommendation**: **Low priority**. Möbius now handles V2+V3 mixed cases directly.

---

## Alternative Optimizer Methods (Historical Comparison)

| Method | Algorithm | Mean Time | vs Möbius (Py) | vs Brent | Accuracy |
|--------|-----------|-----------|-----------------|----------|----------|
| **Möbius (Py)** | Closed-form O(n) | 0.86μs | baseline | 225x faster | Exact |
| **Möbius (Rust f64)** | Closed-form O(n) | 0.19μs | 4.5x faster | 1021x faster | Exact |
| **Möbius (Rust u256)** | Closed-form O(n) | 0.88μs | ~same | 220x faster | EVM-exact |
| Newton | Newton's method | 7.5μs | 8.7x slower | 26x faster | Exact |
| Golden Section | Bracket reduction by φ | 17.5μs | 20x slower | 6.4x faster | Exact |
| Ternary Search | Bracket reduction by 1/3 | 28.7μs | 33x slower | 3.9x faster | Exact |
| Gradient Descent | Barzilai-Borwein | ~45μs | 52x slower | 2x faster | Exact |
| Brent (scipy bounded) | Parabolic interpolation | 194μs | 225x slower | baseline | Exact |
| CVXPY | Interior point | 1300μs | 1512x slower | 7x slower | <0.3 bps |

**All derivative-free methods find identical profits.** Difference is only in iteration count.

---

## Additional Techniques

| Technique | Time | Accuracy | Best Use Case |
|-----------|------|----------|---------------|
| Lookup Table | ~0.1μs | ~0.01% | High-frequency, fixed pools |
| Polynomial (degree 5) | ~0.01μs | ~2% | Quick approximation |
| Integer Binary Search | ~300μs | Exact | EVM-critical (superseded by integer Möbius) |
| Small Arb Approximation | ~0.01μs | Variable | Filtering only |
| Not-profitable rejection (K>M) | 0.32μs | Exact | Pre-solve filtering |

---

## Priority Ranking

| Priority | Class | Alternative | Potential Speedup | Effort | Status |
|----------|-------|-------------|-------------------|--------|--------|
| 1 | V2-V2 | Möbius (Python) | 225x | Done | ✅ COMPLETE |
| 2 | V2-V2 | Möbius (Rust f64) | 1021x | Done | ✅ COMPLETE |
| 3 | V2-V2 | Möbius (Rust u256) | 220x (EVM-exact) | Done | ✅ COMPLETE |
| 4 | Batch V2 | Vectorized Möbius | 23x vs serial | Done | ✅ COMPLETE |
| 5 | Batch V2 | Rust Batch Möbius | 35x vs serial | Done | ✅ COMPLETE |
| 6 | V2-V3 | Möbius closed-form | ~40x vs Brent | Done | ✅ COMPLETE |
| 7 | V2-V3 | Piecewise-Möbius | ~8x vs Brent | Done | ✅ COMPLETE |
| 8 | V3-V3 | Intersection Search | 2-5x vs Brent | High | Pending |
| 9 | Multi-pool | Dual Decomposition | 2-4x | High | Research |
| 10 | Mixed | Hybrid Decomposition | 2-3x | High | Low priority (Möbius handles mixed) |
| 11 | Balancer 3-token | Closed-form Eq.9 | 8x vs grid-search | Done | ✅ COMPLETE |
| 12 | Balancer 4-token | Closed-form Eq.9 | 15x vs grid-search | Done | ✅ COMPLETE |
| 13 | Balancer N>8 | CVXPY | poly(N) vs 3^N | Medium | Pending |

## Superseded Items

| Item | Reason |
|------|--------|
| Analytical quartic solution | Superseded by Möbius (exact, O(n), faster) |
| Piecewise V3 convex approximation | Superseded by Piecewise-Möbius |
| Adaptive initial guess (Newton) | Overhead > savings |
| Smart bracket (Brent) | scipy ignores bracket parameter |
| Integer binary search | Superseded by Rust Integer Möbius (0.88μs vs 300μs) |
| Serial batch processing | Superseded by vectorized Möbius (23x faster) |
| CVXPY for V2 production | Möbius is 1021x faster than Brent |
| Grid-search Balancer solver | Superseded by closed-form Equation 9 (576μs vs 4.6ms for N=3) |

---

## Balancer Weighted Pool Arbitrage

**Current Optimal**: Closed-form Equation 9 (~576μs for N=3)

Based on Willetts & Harrington (2024) "Closed-form solutions for generic N-token AMM arbitrage".

### Approach: Signature Enumeration + Closed-Form per Signature

The paper's Equation 9 gives the optimal basket trade for a *given* trade signature (deposit/withdraw pattern). The solver evaluates all valid signatures and picks the most profitable.

| N tokens | Signatures | Full Solver Time |
|----------|------------|----------------|
| 3 | 12 | 576 μs |
| 4 | 50 | 1.3 ms |
| 5 | 180 | 2.9 ms |

### Alternative B.1: Grid-Search Optimization

Previous implementation used grid search over deposit/withdraw amounts.

| Criterion | Closed-form | Grid-search | Assessment |
|-----------|-------------|-------------|------------|
| Time (N=3) | 576 μs | 4.6 ms | Closed-form 8x faster |
| Time (N=4) | 1.3 ms | 20 ms | Closed-form 15x faster |
| Time (N=5) | 2.9 ms | 76 ms | Closed-form 26x faster |
| Accuracy | Exact (per signature) | Grid resolution | Closed-form better |
| Multi-token | Full basket | Full basket | Both support |

**Recommendation**: Closed-form preferred. Grid search was a working fallback during development.

### Alternative B.2: CVXPY Convex Optimization

| Criterion | Closed-form | CVXPY | Assessment |
|-----------|-------------|-------|------------|
| Time (N=3) | 576 μs | ~500 μs | Comparable |
| Setup overhead | None | Problem construction | Closed-form simpler |
| Dependencies | None | scipy + cvxpy | Closed-form standalone |
| Scalability | O(3^N) signatures | O(poly(N)) | CVXPY better for N>8 |

**Recommendation**: Closed-form for N≤5 (typical Balancer pools). CVXPY may be better for N>8.

### Alternative B.3: Pairwise Decomposition

Decompose N-token basket into N-1 pairwise swaps and optimize each independently.

| Criterion | Closed-form | Pairwise | Assessment |
|-----------|-------------|----------|------------|
| Optimality | Global (basket) | Local (per pair) | Closed-form better |
| Time | 576 μs | N×5.8μs (Möbius) | Comparable for N=3 |
| Cross-token effects | Captured | Missed | Critical for mispriced stables |

**Recommendation**: Closed-form preferred. Pairwise decomposition misses cross-token arbitrage opportunities (e.g., when both USDC and DAI are mispriced relative to ETH).
