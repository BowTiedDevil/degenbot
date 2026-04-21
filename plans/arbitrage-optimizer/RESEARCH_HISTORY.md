# Research History: Arbitrage Optimizers

Complete record of the arbitrage optimization research effort, from initial benchmarks to the final 1021x speedup achieved through Möbius transformations and Rust acceleration.

---

## Executive Summary

**Original Problem**: Brent optimization (scipy) at ~200μs was too slow for MEV-scale arbitrage.

**Final Result**: Rust Möbius at 0.19μs — **1021x faster**, with zero iterations and closed-form exact solutions. Pool cache `solve_cached()` at ~2.5μs end-to-end (~0.73μs Rust-only), eliminating all Python object construction on the solve path.

**Key Innovations**:
1. **Möbius Transformations** — Constant-product AMM paths reduce to single rational function
2. **Unified ArbSolver** — Thin Rust wrapper dispatches all supported types; Python fallback for Solidly/Balancer/Brent
3. **Rust Acceleration** — Sub-microsecond solves with EVM-exact integer validation
4. **Pool State Cache** — RustPoolCache stores state at registration time, solves by ID reference only (~0.73μs Rust-only)
5. **Full AMM Coverage** — V2, V3, V4, Aerodrome, Balancer weighted, Solidly stable

---

## Timeline

| Date | Milestone |
|------|-----------|
| 2026-04-04 | Project started, benchmark suite created |
| 2026-04-04 | Newton's method: 26x faster than Brent |
| 2026-04-05 | V3 tick prediction, V2V3Optimizer: 2-4.5x faster |
| 2026-04-14 | Möbius transformation: 225x faster (Python) |
| 2026-04-15 | Rust Möbius: 1021x faster (0.19μs) |
| 2026-04-15 | Unified ArbSolver integration complete |
| 2026-04-15 | Balancer Eq.9 multi-token solver: 576μs N=3 |
| 2026-04-15 | V3-V3 Rust solver: ~10-50μs (golden section, dual crossing) |
| 2026-04-15 | MobiusSolver dispatch fix: rejects multi-range V3 hops |
| 2026-04-16 | V3-V3 test suite: 28 Python + 6 Rust tests, range bounds bug fix |
| 2026-04-16 | Rust test suite fixes: 7 tests, 3 root causes, convergence + tolerance improvements |
| 2026-04-17 | ArbSolver Rust dispatch: thin wrapper replaces Python 6-solver chain |
| 2026-04-17 | Raw array marshalling: solve_raw() eliminates RustIntHopState objects (1.13x) |
| 2026-04-17 | Pool state cache: RustPoolCache + solve_cached() (~2.5μs, 1.5x faster) |
| 2026-04-18 | V3-V3 range validation tolerance fix: asymmetric multi-range 56% mismatch resolved |

---

## Phases of Development

### Phase 1-2: Foundation & Numerical Improvements

Established benchmark suite comparing Brent vs CVXPY. Key finding: after fixing a bug in Brent's direction logic, both optimizers found identical profits (within 0.3 bps).

**Critical Bug Fix**: Brent optimizer was using wrong arbitrage direction (ROE-based). Fixed to use correct direction based on price comparison.

### Phase 3: V3/V4 Research

- Piecewise V2 approximation for V3 tick ranges
- Dual decomposition with bounded product CFMMs
- **Key insight**: V3 tick ranges are bounded product CFMMs with closed-form solutions

### Phase 6: Closed-Form V2 (Newton's Method)

Newton's method converges in 3-4 iterations vs Brent's 15-30:
- **Result**: 26x faster (~7μs vs ~200μs)
- Identical profit (within 1 wei)

### Phase 7: Production Hardening

- Batch Newton optimizer for multiple paths
- Chain rule optimizer for mixed V2/V3
- HybridOptimizer with automatic method selection

### Phase 9: Multi-Token Routing

Dual decomposition for simultaneous multi-path optimization:
- Handles shared pools correctly
- Convergence: ~5-12ms for 3-5 paths
- Proper shadow price initialization critical

### Phase 10: Möbius Transformation (Breakthrough)

**Mathematical insight**: Every constant-product swap is a Möbius transformation:
```
y = (γ·s·x) / (r + γ·x)  →  l(x) = K·x / (M + N·x)
```

Möbius transformations form a group under composition. An n-hop path reduces to single rational function via O(n) recurrence:
```python
K_new = K * γ_i * s_i
M_new = M * r_i
N_new = N * r_i + K * γ_i
```

**Optimal input** (zero iterations):
```
x_opt = (√(K·M) - M) / N
```

**Result**:
- Python: 5.8μs (**38x faster** than Brent)
- Rust f64: 0.19μs (**1021x faster**)
- Rust u256: 0.88μs (EVM-exact)

### Phase 11-12: V3 Generalization

**V3 single-range**: Same Möbius form with effective reserves:
```
r_eff = L / sqrt_p,  s_eff = L * sqrt_p
```

**V3 multi-range (tick crossing)**: Piecewise-Möbius with explicit crossing computation:
- Crossing amounts are **additive**, not compositional
- Cannot compose into single Möbius
- Golden section search on bracketed profit: ~25μs

---

## Key Discoveries

### 1. Möbius is Optimal for V2

Zero iterations, O(1) for single-path, O(n) for multi-hop. Python 38x faster, Rust 1021x faster than Brent.

### 2. V3 Tick Crossings Are Additive

Unlike V2 multi-hop (compositional), V3 tick crossings sum outputs from crossed ranges. Fundamental algebraic difference prevents Möbius composition.

### 3. Float64 Sufficient for V2

Newton and Möbius find exact solutions for reserves up to uint128 scale (87 bits). Ratios preserve relative precision regardless of absolute magnitude.

### 4. Direct Integer Neighbor Check Beats Golden Section

±1 check around float optimum: 5.8μs vs 47μs (25-iter golden section). Same accuracy, 8x faster.

### 5. Rust Integer Möbius Gives EVM-Exact Results

uint256 arithmetic at 0.88μs, byte-perfect match with contract simulation. Not-profitable rejection at 0.32μs via K>M check.

### 6. Feature Flags Enable Safe Rollout

`USE_SOLVER_FAST_PATH` allows instant rollback to Brent if issues found.

### 7. ArbSolver Rust Dispatch Eliminates Python Overhead

Replacing the 6-step Python dispatch chain (MobiusSolver → PiecewiseMobiusSolver → ... → BrentSolver) with a single `RustArbSolver.solve()` call eliminates method-selection overhead. The Rust solver returns `supported=False` for hop types it can't handle, triggering Python fallback seamlessly.

**Remaining bottleneck**: Integer refinement (3× `_simulate_path` = 3.7μs) dominates the V2-V2 path. Moving this to Rust would bring end-to-end from ~5.8μs to ~1μs.

### 8. Pool Cache Pattern: Register Once, Solve by ID

The pool cache (`RustPoolCache`) is the natural endpoint for eliminating FFI marshalling overhead. Pool states are registered in Rust at update time (once per block, ~0.34μs/pool), then solved by passing only integer IDs. This eliminates all Python object construction, per-item extraction, and list construction on the solve path.

**Performance**: `solve_cached()` at ~2.5μs (Rust-only ~0.73μs). The ~1.87μs remaining overhead is Python method dispatch + SolveResult construction — an FFI marshalling problem becomes a Python dispatch problem.

---

## 30 Lessons Learned

### Testing

**1. Always Use Full Precision Reserves**
```python
# WRONG: 2_000_000 (6 decimals implied)
# RIGHT:  2_000_000_000_000 (actual wei)
```
Using human-readable values led to 3-hour investigation into non-existent "continuous vs discrete" problem.

**2. Test Optimizers Against Each Other**
```python
assert abs(result_mobius.profit - result_brent.profit) < 10  # Within 10 wei
```

**3. Validate Against Integer Math, Not Against Yourself**
Compare against production contract code (`compute_swap_step`), not another implementation that might share the same bugs.

### Optimization

**4. Profile Before Optimizing**
Adaptive initial guess seemed smart but: computing it took ~6μs, Newton only takes ~7μs total. Net slower.

**5. Zero-Iteration Methods Dominate When Available**
Entire performance hierarchy determined by iteration count:
- 0 iterations: 0.19-0.88μs (Möbius)
- 3-4 iterations: 7.5μs (Newton)
- 12-17 iterations: 57-178μs (Brent)

**6. Read Library Documentation Carefully**
scipy's `minimize_scalar` accepts `bracket` but ignores it — uses full bounds regardless.

### Implementation

**7. Numpy View Mutation Is Silent**
```python
M *= r_in        # BAD: mutates view, corrupts input
M = M * r_in     # GOOD: creates new array
```

**8. Log-Domain Overflow Handling**
For long paths with massive reserves, use log-domain arithmetic:
```python
log_K, log_M, log_N = log(K), log(M), log(N)
# Use log_sum_exp for combining, expm1 for profit
```

**9. Pool-Level Caching**
Cache shared data at the source (pool), not consumer (helper). Pool knows when to invalidate; helper would need to poll.

### AMM-Specific

**10. Indicator Functions vs Signed Variables**
```python
# WRONG: d_i = signature[i]  # gives -1, 0, +1
# RIGHT:  d_i = 1 if s_i == 1 else 0  # indicator function
```
Equation 9 from Willetts & Harrington uses `d_i = I_{s_i=1}` — always non-negative indicator.

**11. Token Decimals Must Be Normalized**
Balancer pools hold tokens with different decimals (ETH=18, USDC=6). Upscale all to 18-decimal before applying invariant formulas:
```python
upscaled = [r * 10**(18-d) for r, d in zip(reserves, decimals)]
```

**12. V2-V3 Speedup Expectations vs Reality**
Expected 7-10x, achieved 2-4.5x. Why: virtual reserve overhead, multiple tick ranges, scipy's optimized C code. Always benchmark theoretical improvements.

**13. Benchmark with Realistic Data**
Test data should match production: actual token decimals (USDC=6, WETH=18), realistic reserve magnitudes (millions of dollars), extreme imbalances, small price differences.

### Research Methodology

**14. Tolerance Values Matter**
Tolerance of `1.0` (1 wei) was too large — gradient at equilibrium often < 1.0. Changed to `1e-9` relative tolerance.

**15. Equilibrium Price Prediction**
```python
p_eq = sqrt(v2_price * v3_price) * (1 - avg_fee)**0.5
```
Enables O(1) tick range identification instead of O(n) iteration.

**16. Price Bounds Filtering**
90%+ of tick ranges can be eliminated using no-arbitrage bounds before optimization.

### Multi-Token Routing

**17. Shadow Price Initialization Matters**
Initializing shadow prices to 1.0 caused slow convergence. Initialize from pool reserves:
```python
pool_price = market.reserve_out / market.reserve_in
nu[out_idx] = geometric_mean(pool_prices)
```

**18. Closed-Form Market Solutions Enable Fast Dual Decomposition**
Using closed-form solutions for V2 CFMMs makes each market solve O(1) instead of iterative, making dual decomposition tractable.

**19. Multi-Path Optimization Has Different Trade-offs Than Single-Path**
When paths share pools, optimizing each independently can double-count liquidity. Dual decomposition naturally handles shared pools via shadow prices.
- Single path: Use Möbius (0.86μs Python, 0.19μs Rust)
- Independent paths: Vectorized Möbius batch
- Shared pools: MultiTokenRouter dual decomposition (~5-12ms)

### V3-Specific

**20. V3 Tick Crossings Are Additive, Not Compositional**
V3 tick crossing has fundamentally different algebraic structure than V2 multi-hop:
| Structure | Input/Output | Algebra |
|-----------|-------------|----------|
| V2 multi-hop | Output of hop i = Input of hop i+1 | Sequential composition (Möbius group) |
| V3 tick crossing | Input consumed across ranges, outputs SUM | Additive (not compositional) |

**Numerical proof**: Composing V2-equivalent ranges sequentially gives 11.7x wrong answer vs V3 integer math. Use piecewise-Möbius instead.

**21. V2-V3 Optimization Has a Circular Dependency**
Optimal swap amount → final V3 price → which tick range → bounded product parameters → optimal swap amount.

**Solution**: Break the cycle using equilibrium estimation:
1. Estimate equilibrium independently
2. Predict tick range
3. Solve assuming that range
4. Validate: Check if solution stays in predicted range
5. Iterate if needed: Try adjacent range

**22. Effective Reserves Are Virtual Reserves, Not Real Reserves Plus Bounds**
**Problem**: `to_hop_state()` double-counted alpha and beta by computing `r_eff = L/sqrt_p + alpha`.

**Root cause**: `R0 + alpha = L/sqrt_p` — virtual reserves already INCLUDE the bound parameters.

**Correct**: `r_eff = L/sqrt_p` (NOT `L/sqrt_p + alpha`)

**23. Fee Convention in Test Fixtures**
- `pool.fee` is the actual fee (e.g., `Fraction(3, 1000)` = 0.3%)
- The fee multiplier (gamma) is `1 - fee` (e.g., 0.997)
- In swap calculations: use `fee_mul = fee_denominator - fee_numerator`

### Rust & Performance

**24. Rust Integer Möbius Enables EVM-Exact Validation**
Rather than fighting float precision, use uint256 arithmetic for exact contract simulation. Pattern: Rust f64 for fast screening, then Rust u256 for final validation.

**25. Rust Acceleration Justifies Itself for Sub-μs Targets**
Rust Möbius f64 at 0.19μs vs Python at 0.86μs — 4.5x speedup seems modest, but:
- At MEV time scales, 0.67μs savings = ~1400 blocks of additional margin
- Integer arithmetic only possible in Rust — no Python uint256

**26. Float64 Handles uint128-Scale Reserves**
Despite 53-bit mantissa limits, Newton finds exact solutions for reserves up to 87 bits because gradients (ratios) preserve relative precision regardless of absolute magnitude.

### Engineering Practices

**27. Feature Flags Enable Safe Solver Rollout**
`USE_SOLVER_FAST_PATH = True` feature flag allows fast-path to run before Brent, with automatic fallback on failure. Zero-risk deployment — set to `False` to revert instantly.

**28. Direct Integer Neighbor Check Beats Golden Section**
The Möbius closed-form is so accurate that the best integer is within ±1 of `floor(x_opt)`:
```python
for candidate in range(max(1, x_floor - 1), x_floor + 3):
    # Check just 4 candidates instead of 25-iteration golden section
```
**Impact**: 47μs → 5.8μs (8x faster). Same accuracy.

**31. Symmetric Two-Hop Reserves Are Never Profitable**

For a two-hop arbitrage cycle where pool 2's reserves mirror pool 1's (r₂=s₁, s₂=r₁):
```
K/M = (γ₁·s₁)(γ₂·s₂) / (r₁·r₂) = γ² · (s₁·s₂)/(r₁·r₂) = γ² < 1
```
The pools agree on price, so the round-trip marginal rate is just γ² ≈ 0.994 — always unprofitable. Tests must use reserves where pools *disagree* (asymmetric price quotes).

**32. V3 Range Capacity Must Be Checked in Tests**

V3 tick ranges have bounded capacity: `max_gross_input = L·(1/√P_lower - 1/√P_current)/γ`. Test inputs that exceed this produce final sqrt prices outside the range, which the solver correctly rejects but the test incorrectly expects to pass. Always verify test inputs stay within `max_gross_input_in_range()`.

**33. Float64 Boundary Precision Needs Explicit Handling**

When the constrained max input equals the exact range capacity, float64 arithmetic in `estimate_v3_final_sqrt_price` can produce a final price barely outside range. Two fixes needed:
1. Shrink factor: `max_v3_input * (1 - 1e-12)` on the constrained max
2. Tolerance: `eps = 1e-12 * sqrt_price_current` in range validation instead of strict `contains_sqrt_price()`

**34. Golden Section Convergence Must Scale With Interval**

A fixed `MIN_ABS_INTERVAL = 1e-6` is impractical for large search intervals. When x_min=0 and x_max=1e14, convergence requires ~96 iterations. Fix: `abs_tol = max(1e-6, initial_interval * 1e-10)`.

**35. Return Floats From Rust to Avoid i64 Overflow**

Rust `RustArbResult` returns `optimal_input` and `profit` as `f64`, not `i64`. V2 reserves in wei (1e21+) exceed `i64::MAX` (~9.2e18). Integer refinement is done in Python using arbitrary-precision ints. Attempting `x_opt.floor() as i64` in Rust silently wraps for large values.

**37. Pool Cache Eliminates Python Object Overhead Entirely**

The performance bottleneck moved through four stages:
1. Python dispatch overhead (6 solver chain) → eliminated by Rust dispatch
2. Python→Rust conversion (RustIntHopState objects) → eliminated by raw array marshalling
3. Per-item PyO3 list extraction → eliminated by pool cache (state stored in Rust at registration time)
4. Python method dispatch + SolveResult construction (~1.87μs) → remaining bottleneck

The pool cache pattern (register once, solve by ID) is the natural endpoint for eliminating FFI marshalling overhead. The remaining overhead is Python-side, not FFI-side.

**38. Raw Array Marshalling vs Pool Cache: Different Bottlenecks**

`solve_raw()` (flat int list) and `solve_cached()` (pool IDs) target different layers:
- `solve_raw()` eliminates Python object construction but still has per-item PyO3 extraction
- `solve_cached()` eliminates both by storing state in Rust; the solve call passes only integer IDs
- For the standard `solve()` path, `solve_raw()` is still valuable (used when pool cache isn't set up)
- Item #19 (binary buffer) was superseded by #20 because the pool cache solves the same problem more completely

**36. BoundedProductHop Reserves Use Q96 Scaling**

Python `_v3_virtual_reserves()` scales virtual reserves by Q96 (2^96) to match V2 wei-scale magnitudes. Rust `V3TickRangeHop.to_hop_state()` uses unsealed `L/√P` and `L·√P`. This scale mismatch means the Rust V3 sequence solver can't directly use `BoundedProductHop` reserves as base hops — `solve_piecewise` replaces the hop at `v3_hop_index` internally. When the replacement causes a scale mismatch with other hops, the Python `PiecewiseMobiusSolver` fallback handles it correctly.

**39. Range Validation Tolerance Must Account for Multi-Step Float64 Accumulation**

The range validation tolerance in `compute_v3_v3_profit` was `1e-12 * sqrt_price_current`, matching the tolerance used in the single-range fast path. But the golden section search for multi-range paths converges to the boundary where `estimate_v3_final_sqrt_price()` accumulates rounding from multiple arithmetic operations (`sqrt_p + amount_in * gamma / liquidity` for ofz, or `sqrt_p * liquidity / (liquidity + amount_in * gamma * sqrt_p)` for zfo). At the boundary, the accumulated error (~1.06e-12 relative) can exceed `1e-12`.

**Lesson**: Tolerance values for float64 boundary checks should account for the *number of arithmetic operations* in the estimation function, not just the magnitude of the boundary. A single multiplication has ~0.5 ULP error, but a chain of 3-4 operations can accumulate ~1-2 ULPs. Use `1e-10` for multi-step estimators, `1e-12` for single-step.

**Debugging approach**: When a numerical optimizer silently returns wrong results, add logging to the *validation function* (not the optimizer itself). The golden section search was working correctly — it was the validation inside `compute_v3_v3_profit` that was rejecting valid points. The `log::debug!` instrumentation on the rejection paths immediately revealed the ULP overshoot.

### Balancer-Specific

**29. Trade Signatures Enumerate All Deposit/Withdraw Patterns**
For N tokens: `3^N - 2^(N+1) + 1` valid signatures:
- N=3: 12 signatures
- N=4: 50 signatures
- N=5: 180 signatures

The formula naturally rejects uneconomic signatures (gives wrong-sign trades).

**30. Profit Computation Requires Token-Unit Amounts**
```python
# WRONG: mixing wei scales
profit = -sum(market_prices[i] * trades_in_wei[i])

# CORRECT: convert to token units first
profit = -sum(market_prices[i] * (trades_upscaled[i] / 1e18))
```
Dimensional analysis applies to financial calculations — quantity must be in token units, not platform-specific wei.

---

## AMM Coverage Matrix

| Pool Type | Invariant | Solver | Status |
|-----------|-----------|--------|--------|
| Uniswap V2 | x×y=k | Möbius | ✅ |
| Uniswap V3/V4 | Bounded x×y=k | Möbius (single), Piecewise (multi) | ✅ |
| Aerodrome V2 (volatile) | x×y=k | Möbius | ✅ |
| Aerodrome V2 (stable) | x³y+xy³≥k | SolidlyStableSolver | ✅ |
| Camelot (volatile) | x×y=k | Möbius (asymmetric fees) | ✅ |
| Camelot (stable) | x³y+xy³≥k | SolidlyStableSolver | ✅ |
| Balancer V2 | ∏xᵢ^wᵢ≥k | Eq.9 closed-form | ✅ |
| Curve | StableSwap | ❌ | Not implemented |

---

## Performance Summary

### V2-V2 Single Path

| Method | Time | vs Brent |
|--------|------|----------|
| Rust Möbius (f64) | 0.19μs | **1021x** |
| Python Möbius | 5.8μs | **38x** |
| Newton | 4.5μs | **49x** |
| Brent | 223μs | baseline |
| CVXPY | 1.3ms | 7x slower |

### V2-V2 ArbSolver End-to-End

| Method | Time | vs Standard solve() |
|--------|------|---------------------|
| solve_cached (pool cache) | **~2.5μs** | **1.5x faster** |
| solve (raw array, default) | ~3.9μs | baseline |
| solve (object hops) | ~4.4μs | 0.89x |
| Rust cache.solve() (no Python) | **~0.73μs** | **5.3x faster** |

### Batch 1000 Paths (2-hop)

| Method | Per-Path | Speedup |
|--------|----------|---------|
| Rust Batch Möbius | 0.09μs | **2155x** |
| Python Vectorized | 0.14μs | **1386x** |
| Python Serial | 3.2μs | **62x** |

### V3

| Method | Time | Use Case |
|--------|------|----------|
| Möbius single-range | ~5μs | Within one tick |
| Möbius piecewise | ~25μs | Multi-range crossing |
| Brent V3-V3 | ~390μs | Complex crossings |

### Balancer Multi-Token

| N | Time | Signatures |
|---|------|------------|
| 3 | 576μs | 12 |
| 4 | 1.3ms | 50 |
| 5 | 2.9ms | 180 |

---

## References

### Papers Implemented

1. **Möbius Transformations for Multi-Hop AMMs**
   > Hartigan, J. (2026). "The Geometry of Arbitrage: Generalizing Multi-Hop DEX Paths via Möbius Transformations."

2. **Balancer Multi-Token Arbitrage (Equation 9)**
   > Willetts, M. & Harrington, T. (2024). "Closed-form solutions for generic N-token AMM arbitrage." arXiv:2402.06731

3. **Dual Decomposition for CFMMs**
   > Diamandis, T., Resnick, M., Chitra, T., Angeris, G. (2023). "An Efficient Algorithm for Optimal Routing Through CFMMs."

4. **CFMM Geometry**
   > Angeris, G., Chitra, T., Diamandis, T., et al. (2023). "The Geometry of Constant Function Market Makers."

---

## Archive

Original documents preserved in [`archive/`](archive/):
- `implementation-phases.md` — Detailed phase breakdown
- `progress-log.md` — Day-by-day development log
- `lessons-learned.md` — Original 30 lessons (full content)
- `mobius-full-amm-coverage.md` — Technical gap analysis
- `research/*.md` — Deep dive research notes

---

## Test Status

**694 tests passing, 9 skipped** (as of 2026-04-18)

Includes 17 raw array marshalling tests, 18 pool cache tests, 14 merged integer refinement tests, 15 Rust integer refinement tests, 28 V3-V3 accuracy tests, 6 Rust range bounds unit tests, 49 Rust Möbius optimizer tests, and 209 total Rust unit tests.

```bash
uv run pytest tests/arbitrage/test_optimizers/ -x -q
```

---

*End of Research History*
