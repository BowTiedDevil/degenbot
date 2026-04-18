# Möbius Transformation Optimizer

## Theoretical Foundation

Every constant product swap `y = (f * s * x) / (r + f * x)` is a Möbius transformation (fractional linear transform) that fixes the origin. Möbius transformations form a group under composition, so any n-hop constant product path reduces to a single rational function:

```
l(x) = K * x / (M + N * x)
```

The coefficients K, M, N are computed via an O(n) recurrence (three scalar updates per hop):

```
Initialize: K = gamma_1 * s_1, M = r_1, N = gamma_1

Per hop i (i >= 2):
    K_new = K * gamma_i * s_i
    M_new = M * r_i
    N_new = N * r_i + K * gamma_i    (uses K before update)
```

The optimal input follows from `d(l(x) - x)/dx = 0`:

```
x_opt = (sqrt(K * M) - M) / N
```

Free profitability check: `K / M > 1` (no simulation needed).

**Reference**: Hartigan, J. (2026). "The Geometry of Arbitrage: Generalizing Multi-Hop DEX Paths via Möbius Transformations." Medium.

## Phase 10: V2 Multi-Hop ✅ COMPLETE

**File**: `src/degenbot/arbitrage/optimizers/mobius.py`

### Performance vs Brent

| Pools | Möbius (μs) | Brent (μs) | Speedup | Möbius Iters | Brent Iters |
|-------|-------------|------------|---------|---------------|-------------|
| 2 | 1.35 | 57.90 | **43x** | 0 | 12 |
| 3 | 1.53 | 64.47 | **42x** | 0 | 13 |
| 4 | 1.68 | 68.94 | **41x** | 0 | 13 |
| 5 | 1.82 | 78.60 | **43x** | 0 | 14 |
| 10 | 2.84 | 108.36 | **38x** | 0 | 15 |
| 20 | 4.50 | 177.57 | **40x** | 0 | 17 |

### Performance vs Chain Rule Newton

| Pools | Möbius (μs) | Chain (μs) | Speedup | Möbius Iters | Chain Iters |
|-------|-------------|------------|---------|---------------|-------------|
| 3 | 1.47 | 8.36 | **5.7x** | 0 | 4 |
| 4 | 1.64 | 8.02 | **4.9x** | 0 | 3 |
| 5 | 1.84 | 12.71 | **6.9x** | 0 | 4 |
| 6 | 2.05 | 14.71 | **7.2x** | 0 | 4 |

### Key Findings

1. **Zero iterations** regardless of path length (closed-form)
2. **~40x faster** than Brent across all path lengths
3. **~5-7x faster** than chain rule Newton
4. Scales linearly O(n) — the only way to be faster is to not read the reserves
5. Both solvers agree to within 0.01% on optimal input and profit
6. Free profitability check (K/M > 1) avoids simulation for unprofitable paths

### Comparison

| Aspect | Möbius | Chain Rule Newton | Brent |
|--------|--------|------------------|-------|
| Iterations | **0** | 3-50 | 12-30 |
| Per-iteration cost | N/A | O(n) gradient | O(n) simulation |
| Convergence | Exact | Quadratic | Linear |
| Profitability check | K/M > 1 | Run solver | Run solver |
| Path length scaling | O(n) | O(n × iters) | O(n × iters) |

### Rust Implementation

| Variant | Time | vs Brent |
|---------|------|----------|
| Rust Möbius (f64) | 0.19μs | **1021x faster** |
| Python Möbius | 0.86μs | **225x faster** |
| Rust Integer Möbius (u256) | 0.88μs | **220x faster** (EVM-exact) |

**Rust f64 is 4.5x faster** than Python Möbius for single-path.

**Rust integer Möbius** uses uint256 arithmetic for byte-perfect EVM simulation. Not-profitable rejection at 0.32μs via exact K>M check.

### Batch Implementation

| Optimizer | Total (1000 paths) | Per-Path | vs Python Serial |
|-----------|-------------------|----------|------------------|
| Rust Batch Möbius | 93μs | 0.09μs | **35x** |
| Rust Vec Batch Möbius | 104μs | 0.10μs | **31x** |
| Python Vec Möbius | 140μs | 0.14μs | **23x** |
| Python Vec Newton | 528μs | 0.53μs | 6x |
| Python Serial Möbius | 3229μs | 3.2μs | — |

### Limitations

- Only applies to constant product AMMs (x × y = k)
- Does not handle V3 concentrated liquidity or stableswap
- Float64 overflow risk for very long paths with massive reserves (K×M product)

### Integration

The `HybridOptimizer` dispatches pure V2 paths >= 2 hops to `MobiusV2Optimizer`:

```python
# In HybridOptimizer.solve():
if total_pools >= 3 and v2_count == total_pools:
    return self._get_mobius().solve(pools, input_token, max_input)

if v2_count == total_pools and v3_count == 0 and v4_count == 0 and total_pools >= 2:
    return self._get_mobius().solve(pools, input_token, max_input)
```

---

## Phase 11: V3 Single-Range Generalization ✅ COMPLETE

Generalized `MobiusV2Optimizer` to `MobiusOptimizer` (V2+V3 support).

### V3 as Möbius Hop

V3 tick ranges are Möbius transforms with effective reserves:
- `R0 + alpha = L / sqrt_p` (effective R0)
- `R1 + beta = L * sqrt_p` (effective R1)

Single-range V3 is O(1) closed-form, same as V2.

### New Types and Functions

| Component | Purpose |
|-----------|---------|
| `V3TickRangeHop` | Bounded product CFMM as Möbius hop |
| `estimate_v3_final_sqrt_price` | Range validation after swap |
| `solve_v3_candidates` | Multi-range V3 optimization (~5-15μs) |

### Bug Fixes

1. **`to_hop_state()` double-counting**: Code computed `r_eff = L/sqrt_p + alpha` but `R0+alpha = L/sqrt_p` by definition. Fixed to `r_eff = L/sqrt_p`.
2. **`estimate_v3_final_sqrt_price()` missing `*sqrt_p`**: Zero_for_one formula needed `denom = liquidity + amount_in * gamma * sqrt_p`.

---

## Phase 12: Piecewise-Möbius with Explicit Tick Crossing ✅ COMPLETE

### V3 Tick Crossing is Additive

V3 tick crossings have **ADDITIVE** (not compositional) structure. Input consumed across ranges, outputs SUM. Cannot compose into a single Möbius transform.

### Piecewise Structure

```
total_output(x) = crossing_output(0..K-1) + mobius(x - crossing_input, range_K)
                  \------- FIXED --------/   \-------- VARIABLE -----------/
```

Crossing amounts are **FIXED** — independent of total input.

### Design Decision: Golden Section Search

The profit function `profit(x) = g(C_K + M_K(f(x) - c_K)) - x` has additive constants from the fixed crossing that break the pure Möbius closed form.

Options:
1. **Closed-form with constants**: Complex polynomial, error-prone.
2. **Golden section search**: Simple, robust, ~25 iterations. ✅ Chosen

### New Types and Functions

| Component | Purpose |
|-----------|---------|
| `TickRangeCrossing` | Pre-computed crossing: `crossing_gross_input`, `crossing_output`, `ending_range` |
| `V3TickRangeSequence` | Ordered range sequence with `compute_crossing(k)` |
| `piecewise_v3_swap()` | Total output = fixed crossing + Möbius(remaining, ending_range) |
| `MobiusOptimizer.solve_piecewise()` | Full optimizer using golden section search per candidate |

### Validation Results

| Comparison | Error |
|-----------|-------|
| Crossing amounts vs V3 integer `get_amount0/1_delta` | <0.1% |
| Piecewise swap output vs multi-step `compute_swap_step` | <0.5% |
| Optimized profit vs brute-force V3 integer search | <5% |

### Performance Comparison

| Optimizer | Typical Time | Method |
|----------|-------------|--------|
| V2V3Optimizer (Newton) | ~5-15ms | Iterative with tick prediction |
| Piecewise-Möbius (golden section) | ~25μs | ~25 iterations, bracketed search |
| Single-range Möbius (closed form) | ~5μs | Zero iterations, O(1) |

### Research Documents

| File | Purpose |
|------|--------|
| `plans/mobius-v3-v4-research.md` | Möbius extension to V3/V4 bounded liquidity |
| `plans/v3-tick-crossing-decomposition.md` | V3 tick crossing algebraic structure research |

### Test Files

| File | Tests | Purpose |
|------|-------|---------|
| `test_mobius_optimizer.py` | 40 | Unit, cross-solver agreement, benchmarks |
| `test_mobius_v3.py` | 38 | V3 tick range hop unit tests |
| `test_mobius_v3_accuracy.py` | 23 | Validate against V3 compute_swap_step integer math |
| `test_piecewise_mobius.py` | 16 | Piecewise-Möbius crossing, swap, and optimizer tests |
