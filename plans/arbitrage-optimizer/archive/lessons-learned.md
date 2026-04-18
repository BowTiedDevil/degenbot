# Lessons Learned

## 1. Always Use Full Precision Reserves in Tests

**Problem**: Using human-readable reserve values (e.g., `2_000_000` USDC instead of `2_000_000_000_000`) led to a false conclusion about continuous vs discrete optimization mismatch.

**Root Cause**: With small reserve numbers, integer rounding effects are magnified. The relative error from rounding `2_000_000` vs `2_000_000_000_000` is vastly different.

**Correct Pattern**:
```python
# USDC (6 decimals): 2M = 2_000_000 * 10^6 = 2_000_000_000_000
reserves_token0 = 2_000_000_000_000

# WETH (18 decimals): 1000 = 1000 * 10^18
reserves_token1 = 1_000 * 10**18
```

**Impact**: 3-hour investigation into a "continuous vs discrete" problem that didn't exist.

## 2. Test Optimizers Against Each Other

**Best Practice**: Every optimizer test should verify all methods find the same profit (within tolerance):

```python
def test_mobius_matches_brent():
    result_mobius = mobius_solve(pools, input_token)
    result_brent = brent_solve(pools, input_token)
    assert abs(result_mobius.profit - result_brent.profit) < 10  # Within 10 wei
```

## 3. Profile Before Optimizing

**Lesson**: The adaptive initial guess seemed like a good idea ("fewer iterations = faster"), but profiling revealed:
- Newton V1 total time: 6.9μs (4 iterations × ~2μs each)
- Computing adaptive guess: ~6μs (sqrt, arithmetic)
- Net result: V2 slower despite fewer iterations

**Takeaway**: Measure actual performance, not theoretical algorithmic complexity.

## 4. Read Library Documentation Carefully

**Issue**: scipy's `minimize_scalar(method="bounded")` documentation states it accepts a `bracket` parameter, but the bracket is only used for initial point selection; the algorithm still explores the full bounds.

**Solution**: To reduce iterations, narrow the bounds themselves, not the bracket.

## 5. Benchmark with Realistic Data

**Problem**: Tests with unrealistic data (tiny reserves) can produce misleading results.

**Guideline**: Test data should match production conditions:
- Use actual token decimals (USDC=6, WETH=18)
- Use realistic reserve magnitudes (millions of dollars)
- Include edge cases (extreme imbalances, small price differences)

## 6. Shadow Price Initialization Matters for Dual Decomposition

**Problem**: Initializing shadow prices to 1.0 for all tokens caused slow convergence or no solution.

**Solution**: Initialize shadow prices from pool reserves:
```python
def _initialize_prices(markets, n_tokens):
    pool_price = market.reserve_out / market.reserve_in
    nu[out_idx] = geometric_mean(pool_prices)
```

**Impact**: With proper initialization, L-BFGS-B converges in fewer iterations and finds better solutions.

## 7. Closed-Form Market Solutions Enable Fast Dual Decomposition

**Solution**: Use closed-form solutions for V2 constant product AMMs:
```python
sqrt_term = np.sqrt(gamma * k / shadow_price_ratio)
x = (sqrt_term - R_in) / gamma
```

**Benefit**: Each market solve is O(1) instead of iterative, making dual decomposition tractable.

## 8. Multi-Path Optimization Has Different Trade-offs Than Single-Path

**Key Insight**: When multiple paths share pools, optimizing each independently can double-count liquidity, miss global optimum, or create imbalanced trades.

**Solution**: Dual decomposition naturally handles shared pools — each market optimizes given shadow prices, shadow prices adjust to clear markets.

**Recommendation**:
- Single path: Use Möbius (0.86μs Python, 0.19μs Rust)
- Multiple independent paths: Vectorized Möbius batch (0.14μs/path Python, 0.09μs/path Rust)
- Shared pools across paths: MultiTokenRouter dual decomposition (~5-12ms)

## 9. V3 Tick Crossing Can Be Predicted Before Optimization

**Key Insight**: The equilibrium price can be estimated independently of tick ranges:
```python
p_eq = sqrt(v2_price * v3_price) * (1 - avg_fee)**0.5
```

This tells us which tick range will be active after arbitrage, enabling O(1) tick range identification instead of O(n) iteration.

## 10. Price Bounds Filtering Eliminates Most Tick Ranges

**Key Insight**: After arbitrage, prices must satisfy the no-arbitrage condition: `|P_v2_final - P_v3_final| <= total_fee`. Only tick ranges overlapping with `[p_lower, p_upper]` can be optimal.

**Impact**: In practice, 90%+ of tick ranges can be eliminated before optimization.

## 11. Bounded Product CFMM Enables Closed-Form V3 Optimization

**Key Insight**: Each V3 tick range is a bounded product CFMM:
```
φ(R) = (R₀ + α)(R₁ + β) ≥ L²
```

Closed-form optimal: `R1_opt = L * sqrt(P_external) - β`

**Benefit**: O(1) optimization per tick range.

## 12. V2-V3 Optimization Has a Circular Dependency

Optimal swap amount → final V3 price → which tick range → bounded product parameters → optimal swap amount.

**Solution**: Break the cycle using equilibrium estimation:
1. Estimate equilibrium independently
2. Predict tick range
3. Solve assuming that range
4. Validate: Check if solution stays in predicted range
5. Iterate if needed: Try adjacent range

**Complexity**: O(1) for typical cases, O(k) for k candidate ranges.

## 13. V3 Tick Crossings Are Additive, Not Compositional

V3 tick crossing has fundamentally different algebraic structure than V2 multi-hop:

| Structure | Input/Output | Algebra |
|-----------|-------------|----------|
| V2 multi-hop | Output of hop i = Input of hop i+1 | Sequential composition (Möbius group) |
| V3 tick crossing | Input consumed across ranges, outputs SUM | Additive (not compositional) |

**Numerical proof**: Composing V2-equivalent ranges sequentially gives 11.7x wrong answer vs V3 integer math.

**Implication**: V3 tick crossings cannot be composed into a single Möbius transform. Use piecewise-Möbius instead.

## 14. Effective Reserves Are Virtual Reserves, Not Real Reserves Plus Bounds

**Problem**: `to_hop_state()` double-counted alpha and beta by computing `r_eff = L/sqrt_p + alpha`.

**Root cause**: `R0 + alpha = L/sqrt_p` — the virtual reserves already INCLUDE the bound parameters.

**Correct**: `r_eff = L/sqrt_p` (NOT `L/sqrt_p + alpha`)

**Impact**: The double-counting inflated effective reserves by 2-4x, making swap outputs ~9% too high.

## 15. Validate Against Integer Math, Not Against Yourself

**Problem**: Early V3 tests compared Möbius float output against Brent on the same effective-reserve HopStates — circular validation that couldn't catch formula errors.

**Solution**: Compare against V3 `compute_swap_step` integer arithmetic (the actual contract implementation).

**Key insight**: When validating a new implementation, always compare against an independent reference — ideally the production contract code. Comparing two implementations that share the same formula bugs just validates that they're consistently wrong.

## 16. Fee Convention in Test Fixtures

**Convention**:
- `pool.fee` is the actual fee (e.g., `Fraction(3, 1000)` = 0.3%)
- The fee multiplier (gamma) is `1 - fee` (e.g., 0.997)
- In swap calculations: use `fee_mul = fee_denominator - fee_numerator`

## 17. V2-V3 Speedup Expectations vs Reality

**Expected**: 7-10x speedup for V2-V3 Binary+Newton.
**Actual**: 2-4.5x speedup.

**Why**: Virtual reserve conversion adds overhead, multiple tick ranges multiply Newton solves, scipy's Brent is highly optimized C code, and theoretical O(log n) vs O(1) analysis ignored constant factors.

**Lesson**: Always benchmark theoretical improvements. Algorithmic complexity analysis is necessary but not sufficient — constant factors and implementation details matter.

## 18. Pool-Level Caching for Shared Data

**Why pool-level, not helper-level**:

| Aspect | Pool-Level | Helper-Level |
|--------|------------|--------------|
| Sharing | ✅ All helpers share | ❌ Each helper duplicates |
| Invalidation | ✅ Pool knows when | ❌ Helper must poll |
| Source of truth | ✅ Pool owns data | ❌ Derivative copy |
| Performance | 0.67μs per lookup | O(n) iteration through ticks |

**Lesson**: Cache shared data at the source (pool), not at the consumer (helper).

## 19. Float64 Handles uint128-Scale Reserves

Despite theoretical 53-bit mantissa limits, Newton finds exact solutions for reserves up to 87 bits because it uses gradients (ratios), which preserve relative precision regardless of absolute magnitude.

## 20. Tolerance Values Matter

Initial tolerance of `1.0` (1 wei) was too large for Newton convergence. The gradient `dprofit_dx` at equilibrium is often < 1.0. Changed to `1e-9` (relative tolerance on gradient).

## 21. Numpy View Mutation Is a Silent Bug

**Problem**: `batch_mobius.py` used `M *= r_in` which mutated a numpy view of the input `hops_array`, corrupting subsequent calls.

**Root cause**: NumPy advanced indexing returns a view, not a copy. In-place operations on views silently modify the original.

**Solution**: Always use `M = M * r_in` (creates new array) instead of `M *= r_in` (mutates in-place) when operating on views.

## 22. Log-Domain Overflow Handling for Batch Möbius

**Problem**: For long paths with massive reserves, K×M overflow float64.

**Solution**: Use log-domain arithmetic:
- `log_K`, `log_M`, `log_N` instead of K, M, N
- `log_sum_exp` for combining N terms
- `expm1` for profit computation (avoids catastrophic cancellation)

**Impact**: Handles arbitrarily large reserves and path lengths without overflow.

## 23. Rust Integer Möbius Enables EVM-Exact Validation

**Key Insight**: Rather than fighting float precision, use uint256 arithmetic for exact contract simulation. The 0.88μs solve time (4.3x slower than Rust f64) is negligible for validation.

**Pattern**: Use Rust f64 for fast screening, then Rust u256 for final validation of profitable paths. Not-profitable paths rejected in 0.32μs via K>M check without any solve.

## 24. Zero-Iteration Methods Dominate When Available

**Key Insight**: The entire performance hierarchy is determined by iteration count:

| Iterations | Time (V2-V2) | Method |
|-----------|-------------|--------|
| 0 | 0.19-0.88μs | Möbius (closed-form) |
| 3-4 | 7.5μs | Newton |
| 12-17 | 57-178μs | Brent |
| N/A | 1300μs | CVXPY |

**Lesson**: When a closed-form solution exists, iterative methods cannot compete regardless of convergence rate.

## 25. Rust Acceleration Justifies Itself for Sub-μs Targets

**Finding**: Rust Möbius f64 at 0.19μs vs Python at 0.86μs — 4.5x speedup seems modest, but:
- At MEV time scales, 0.67μs savings = ~1400 blocks of additional margin
- Integer arithmetic (0.88μs) is only possible in Rust — no Python uint256
- Batch acceleration (0.09 vs 0.14μs/path) compounds at scale

**Lesson**: Don't compare relative speedups in isolation. Consider what the absolute times enable.

## 26. Direct Integer Neighbor Check Beats Golden Section for Möbius Refinement

**Problem**: The Möbius closed-form gives a float optimum. Converting to integer requires checking nearby values. The initial implementation used a 25-iteration golden section search followed by an 11-element neighbor check — total ~61 simulation calls, ~47μs.

**Solution**: The Möbius closed-form is so accurate that the best integer is always within ±1 of `floor(x_opt)`. A simple 4-element check (floor-1, floor, floor+1, floor+2) suffices:

```python
x_floor = int(x_opt)
for candidate in range(max(1, x_floor - 1), x_floor + 3):
    output = _simulate_path(float(candidate), hops)
    profit = int(output) - candidate
    if profit > best_profit:
        best_profit = profit
        best_input = candidate
```

**Impact**: 47μs → 5.8μs (8x faster). Same accuracy.

**Lesson**: When the float approximation is excellent (as with Möbius closed-form), don't over-engineer the integer refinement. A simple neighbor check is sufficient.

## 27. Feature Flags Enable Safe Solver Rollout

**Problem**: Replacing a proven Brent optimizer in production is risky. A bug in the new solver could cause incorrect trades.

**Solution**: `USE_SOLVER_FAST_PATH = True` feature flag. The solver fast-path runs before Brent, and on failure (exception or unprofitable), falls back to the existing Brent path unchanged.

**Impact**: Zero-risk deployment. If issues arise, set flag to `False` and all paths revert to Brent instantly.

**Lesson**: When replacing critical infrastructure, use feature flags with automatic fallback rather than big-bang replacement.

## 28. Indicator Functions vs Signed Variables in AMM Formulas

**Problem**: Implementing Equation 9 from Willetts & Harrington (2024), the paper defines `d_i = I_{s_i=1}` as an **indicator function** (1 for deposit, 0 for withdraw). The initial implementation used `d_i = signature[i]` (giving -1, 0, or +1), which caused all trade amounts to have inverted signs.

**Root cause**: The paper's notation `d = I_{s=1}` is a standard indicator function in mathematics, but was misread as a simple assignment of the signature value.

**Correct Pattern**:
```python
# WRONG: d_i = signature[i]  → gives -1 for withdrawals
def _compute_d(signature):
    return [s for s in signature]  # -1, 0, +1

# CORRECT: d_i = I_{s_i=1}  → gives 1 for deposits, 0 otherwise
def _compute_d(signature):
    return [1 if s == 1 else 0 for s in signature]
```

**Impact**: The sign error meant deposits became withdrawals and vice versa. All 12 signature evaluations gave trades with wrong signs, failing validation.

**Lesson**: When implementing formulas from papers, pay extreme attention to indicator function notation. `I_{condition}` means 1 if condition is true, 0 otherwise — it is NEVER negative.

## 29. Token Decimals Must Be Normalized in AMM Invariant Calculations

**Problem**: Balancer pools hold tokens with different decimal precisions (ETH=18, USDC=6). The invariant `prod(R_i^w_i)` multiplies reserve values. Without normalization, ETH reserves (~1e21 wei) and USDC reserves (~1e12 wei) create a 1e9 magnitude mismatch, causing the closed-form formula to produce wildly incorrect results.

**Root cause**: The formula is derived assuming all reserves are in the same unit scale. When reserves have different decimal precisions, the geometric mean in the invariant calculation is dominated by the high-precision token, making the formula "see" the pool as extremely unbalanced.

**Correct Pattern**:
```python
# Upscale all reserves to 18-decimal (Balancer Vault convention)
scaling_factors = [10 ** (18 - d) for d in decimals]
upscaled_reserves = [r * f for r, f in zip(reserves, scaling_factors)]

# Apply formula in 18-decimal space
trades_upscaled = compute_optimal_trade(upscaled_reserves, ...)

# Descale trades back to native token units
trades_native = [trade / scaling_factors[i] for i, trade in enumerate(trades_upscaled)]
```

**Impact**: Without upscaling, the formula gave trades of ~2500 ETH (entire pool reserve) at equilibrium — 6 orders of magnitude off from the correct ~0. With upscaling, equilibrium correctly gives ~0.

**Lesson**: Any AMM formula that uses products or powers of reserves requires all reserves to be in a consistent unit scale. The Balancer Vault itself uses 18-decimal upscaling for this reason.

## 30. Profit Computation Requires Token-Unit Amounts

**Problem**: Computing profit as `-sum(market_price_i * Phi_i)` where `Phi_i` is in native wei (ETH=1e18, USDC=1e6) produces nonsensical results because the different scales make ETH contributions ~1e12x larger than USDC.

**Solution**: Convert all trades to token-unit amounts before multiplying by market prices:
```python
# WRONG: mixing wei scales
profit = -sum(market_prices[i] * trades_in_wei[i] for i in range(n))

# CORRECT: use token-unit amounts
profit = -sum(market_prices[i] * (trades_upscaled[i] / 1e18) for i in range(n))
```

**Lesson**: Dimensional analysis applies to financial calculations just as much as physics. When multiplying price × quantity, the quantity must be in token units, not platform-specific wei units.
