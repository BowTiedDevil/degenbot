# V3 Bounded Product & Tick Crossing

## V3 Optimization Challenges

1. **Tick Crossings are Discrete**: V3/V4 swaps iterate through tick ranges — sequential process that cannot be expressed as convex constraints.
2. **Piecewise V2 Approximation**: Each tick range can be approximated as a virtual V2 pool, but accurate only for small swaps within a single tick range.
3. **Convex Relaxation Bounds**: Upper/lower bounds on V3 output. Useful for narrowing Brent search space.

## Bounded Product CFMM (The Breakthrough)

Each V3 tick range is a **bounded product CFMM** with closed-form optimal arbitrage:

```
Trading function: φ(R) = (R₀ + α)(R₁ + β) ≥ k

where:
α = L / sqrt(P_upper)   # Lower bound on R₀
β = L * sqrt(P_lower)   # Lower bound on R₁
k = L²                  # Effective constant product
```

**Closed-form optimal arbitrage**:
```python
# At optimum, pool's marginal price equals external price
R1_opt = L * sqrt(P_external) - β
R0_opt = L / sqrt(P_external) - α
```

**Complexity**: O(1) per range — same as V2's Newton method.

## V2-V3 Circular Dependency

Optimal swap amount → final V3 price → which tick range → bounded product parameters → optimal swap amount.

**Solution**: Break the cycle using equilibrium estimation:
1. Estimate equilibrium price: `p_eq = sqrt(v2_price * v3_price) * (1 - avg_fee)**0.5`
2. Predict tick range containing equilibrium
3. Solve assuming that range (bounded product CFMM)
4. Validate: Check if solution stays in predicted range
5. Iterate if needed: Try adjacent range

## Price Bounds Filtering

After arbitrage, prices must satisfy: `|P_v2_final - P_v3_final| <= total_fee`

Only tick ranges overlapping with `[p_lower, p_upper]` can be optimal.

**Impact**: 90%+ of tick ranges eliminated before optimization.

Example: 100 initialized ticks with 60 tick spacing → check only ~3-10 ranges.

## Tick Crossing Prediction

```python
# Price impact estimation
ΔP/P ≈ amount / (L * sqrt_price)

# For zero_for_one
new_sqrt_price = sqrt_price * L / (L + amount_in * gamma)

# For one_for_zero
new_sqrt_price = sqrt_price + amount_in * gamma / L
```

## V3 Tick Crossing Structure

V3 tick crossing has **ADDITIVE** structure, not COMPOSITIONAL:

| Structure | Input/Output | Algebra |
|-----------|-------------|----------|
| V2 multi-hop | Output of hop i = Input of hop i+1 | Sequential composition (Möbius group) |
| V3 tick crossing | Input consumed across ranges, outputs SUM | Additive (not compositional) |

**Numerical proof**: Composing V2-equivalent ranges sequentially gives 11.7x wrong answer vs V3 `compute_swap_step` integer math.

### Piecewise-Möbius Structure

For a V3 swap ending in range K:

```
total_output(x) = crossing_output(0..K-1) + mobius(x - crossing_input, range_K)
                  \------- FIXED --------/   \-------- VARIABLE -----------/
```

**Crossing amounts are FIXED** — determined by range boundaries and liquidity, independent of total input. The breakpoints of the piecewise function don't move as input changes.

## V2V3Optimizer Implementation

**File**: `src/degenbot/arbitrage/optimizers/v2_v3_optimizer.py`

**Algorithm**:
1. Estimate equilibrium price from V2 and V3 pool prices
2. Compute price bounds (fee-adjusted)
3. Filter impossible tick ranges
4. Sort candidates by equilibrium distance
5. Solve assuming each candidate range
6. Validate solutions

**Performance**:
- Single candidate: O(1) bounded product CFMM
- Multiple candidates: Check top 3, return best valid
- Crossing predicted: Fall back to Brent

## V3 Tick Range Cache

**Design**: Pool-level cache shared across all arbitrage helpers.

```python
class V3TickRangeCache:
    def find_range_at_price(price: float) -> TickRangeInfo:
        # Binary search: O(log n), ~0.67μs

    def invalidate() -> None:
        # Called when pool state changes
```

**Why pool-level**:
- Single source of truth — tick data belongs to the pool
- Shared across helpers — multiple arbitrage cycles share same pool
- Pool manages invalidation — already has subscriber pattern

| Operation | Time |
|-----------|------|
| Tick range lookup | 0.67μs (binary search) |
| Cache rebuild (100 ranges) | ~150μs |
| Cache rebuild (1000 ranges) | ~1.6ms |

## V3 Optimization Approaches Summary

| Approach | Complexity | Accuracy | Best Use Case |
|----------|------------|----------|---------------|
| Binary Search | O(log n) | High | Finding active range |
| Piecewise V2 | O(n) | Medium | Multi-range checking |
| Bounded Product | O(1) | High* | Single range, no crossing |
| Tick Crossing Pred | O(1) | Medium | Quick filter |
| Transition Table | O(log n) | High | Pre-computed state |
| Hybrid | Variable | High | Production use |

*Exact for non-crossing trades within single range.

## Test Files

| File | Tests | Coverage |
|------|-------|----------|
| `test_v3_tick_predictor.py` | 23 | Tick math, price impact, crossing prediction, bounded product |
| `test_v2_v3_optimizer.py` | 20 | Equilibrium estimation, price bounds, tick filtering, optimizer |
| `test_v3_bounded_region.py` | 28 | Tick range mathematics, bounded product optimization |
| `test_v3_approximation.py` | 9 | Virtual pool approximations |
| `test_mobius_v3.py` | 38 | V3 tick range hop unit tests |
| `test_mobius_v3_accuracy.py` | 23 | Validation against V3 compute_swap_step integer math |
| `test_piecewise_mobius.py` | 16 | Piecewise-Mobius crossing, swap, and optimizer tests |
