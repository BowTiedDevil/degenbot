# Möbius Solver: Full AMM Coverage Gap Analysis

## Goal

Support all known AMM types in any path length and mix with the unified Möbius/ArbSolver framework.

## Current Coverage

### Pool Types in the Codebase

| Pool Class | Invariant | Solver Support | `pool_to_hop` | Cycle Class |
|---|---|---|---|---|
| `UniswapV2Pool` | x×y=k | ✅ Möbius closed-form | ✅ | ✅ `_calculate_v2_v2` etc. |
| `UniswapV3Pool` | Bounded x×y=k (concentrated) | ✅ Möbius single-range, piecewise multi-range | ✅ | ✅ All V3 pairings |
| `UniswapV4Pool` | Bounded x×y=k (concentrated) | ✅ Same as V3 | ✅ | ✅ All V4 pairings |
| `AerodromeV2Pool` (volatile) | x×y=k | ✅ Möbius (is V2 subclass) | ✅ | ✅ (via AerodromeV2Pool match) |
| `AerodromeV2Pool` (stable) | x³y + xy³ ≥ k (Solidly) | ✅ SolidlyStableSolver (golden section) | ✅ SolidlyStableHop + swap_fn | ✅ (falls to Brent) |
| `AerodromeV3Pool` | Bounded x×y=k | ✅ Inherits V3 | ✅ (inherits V3 handling) | ✅ (inherits V3 handling) |
| `CamelotLiquidityPool` (volatile) | x×y=k | ✅ Asymmetric fees via fee_out | ✅ ConstantProductHop with fee_out | ❌ Not in cycle type union |
| `CamelotLiquidityPool` (stable) | x³y + xy³ ≥ k (Solidly) | ✅ SolidlyStableSolver (golden section) | ✅ SolidlyStableHop + swap_fn | ❌ Not in cycle type union |
| `BalancerV2Pool` | ∏xᵂⁱ ≥ k (weighted geometric mean) | ✅ Eq.9 closed-form | ✅ BalancerMultiTokenHop | ❌ Not in cycle type union |
| `CurveStableswapPool` | AnΣxᵢ² + D = AnD + (∏xᵢ)^(D/n) | ❌ | ❌ | ❌ Not in cycle type union |
| PancakeSwap V2/V3 | x×y=k / bounded | ✅ (V2/V3 subclasses) | ✅ (subclass match) | ✅ |
| SushiSwap V2/V3 | x×y=k / bounded | ✅ (V2/V3 subclasses) | ✅ (subclass match) | ✅ |
| SwapbasedV2Pool | x×y=k | ✅ (V2 subclass) | ✅ (subclass match) | ✅ |

### Summary: What's Missing

| # | Gap | Severity | Difficulty | Status |
|---|-----|----------|-----------|--------|
| 1 | **Solidly stable invariant** (Aerodrome stable, Camelot stable) | High — common on L2s | Medium | ✅ Done (SolidlyStableSolver + swap_fn) |
| 2 | **Asymmetric fees** (Camelot, potentially others) | Medium — breaks single `fee` field | Low | ✅ Done (fee_out on ConstantProductHop) |
| 3 | **Balancer weighted pools** | Medium — multi-token, different invariant | High | ✅ Done (closed-form Eq.9) |
| 4 | **Curve stableswap** | Low-medium — complex invariant, 2-8 tokens | High | ❌ Remaining |
| 5 | **PiecewiseMobiusSolver** not in unified interface | Medium — already built, just needs wiring | Low | ✅ Done |
| 6 | **V3 buy-pool** | Medium — uses actual pool calc | Medium | ✅ Done |
| 7 | **Balancer/Curve not in cycle class** | Low — cycle class only handles 2-pool Uniswap-family | High | ❌ Remaining |
| 8 | **Multi-token pools** (Balancer 3-8 tokens) | Low — different arbitrage structure | Very High | ✅ Done (BalancerMultiTokenSolver, N=3-5) |

---

## Detailed Gap Analysis

### Gap 1: Solidly Stable Invariant (x³y + xy³ ≥ k)

**Affected pools**: `AerodromeV2Pool(stable=True)`, `CamelotLiquidityPool(stable_swap=True)`

**Current state**: 
- `pool_to_hop()` converts AerodromeV2Pool using V2 logic (reserves + fee), which is **wrong** for stable pools because the invariant is not constant product
- The `Hop` dataclass has no field to indicate the invariant type
- The solver runs Möbius on constant-product data → **incorrect result for stable pools**
- The cycle class already supports AerodromeV2Pool stable (via `calculate_tokens_out_from_tokens_in` which dispatches to `calc_exact_in_stable`), but only through the Brent path

**The invariant**: `k = x³y + xy³` (scaled to 18-decimal fixed point)

This is **not** a Möbius transformation. The swap output function `y(x)` comes from solving a cubic equation, not a rational function. So Möbius composition does not apply.

**Options**:

| Approach | Time | Feasibility |
|---|---|---|
| **A. Treat as opaque (Brent fallback)** | ~200μs | Already works. Easy. |
| **B. Analytical first-order condition** | ~5-10μs | The profit function `π(x) = g_buy(x) - x` with `g_buy` from Solidly invariant. Derivative is available (quadratic formula from dπ/dx = 0). Newton's method would converge in 3-5 iterations. |
| **C. Closed-form arbitrage for Solidly** | ~1μs | Research needed. The Solidly swap function involves solving a cubic `get_y()`. The optimal condition `dπ/dx = 0` is higher-order. May not have a clean closed form. |
| **D. Numerical approximation** | ~10-20μs | Fit a rational function approximation to the Solidly swap curve, then apply Möbius. Risk: approximation errors. |

**Recommendation**: **Option B** (Newton's method with analytical gradient). The Solidly `get_y()` function is iterative (Newton's method internally), but the outer arbitrage Newton's method can use numerical differentiation if analytical gradient is complex. Expected ~10-20μs, still 10-20x faster than Brent.

**Required changes**:
1. Add `Hop.invariant` field: enum `CONSTANT_PRODUCT | SOLIDLY_STABLE | BALANCER_WEIGHTED | CURVE_STABLESWAP`
2. Add `Hop.decimals_in` / `Hop.decimals_out` fields (Solidly needs decimal scaling)
3. `pool_to_hop()` sets `invariant=SOLIDLY_STABLE` for `AerodromeV2Pool(stable=True)`
4. New `SolidlyStableSolver(Solver)` in the unified interface
5. `ArbSolver` dispatches based on `Hop.invariant`

### Gap 2: Asymmetric Fees

**Affected pools**: `CamelotLiquidityPool` (different fee for token0 vs token1)

**Current state**:
- `Hop.fee` is a single `Fraction` — assumes same fee in both directions
- `pool_to_hop()` uses `pool.fee` which is correct for UniswapV2Pool (same fee both ways)
- Camelot stores `(fee_token0, fee_token1)` as a tuple
- For a hop with direction token_in → token_out, the fee depends on which token is input

**Impact**: Möbius composition is still valid — each hop just uses its directional fee. The coefficient recurrence uses `gamma_i` per hop, which already accommodates different fees per hop. The issue is only in the `Hop` data structure.

**Required changes**:
1. Add `Hop.fee_out: Fraction | None = None` — if None, use `fee` for both directions
2. `Hop.gamma` property already exists; add logic to select the correct fee based on swap direction
3. Update `pool_to_hop()` for CamelotLiquidityPool to set both fees
4. The Möbius coefficient computation `gamma_i` already uses the per-hop fee — no algorithm change needed

**Difficulty**: Low. Structural change only.

### Gap 3: Balancer Weighted Pools ✅ RESOLVED

**Affected pools**: `BalancerV2Pool`

**Current state**:
- ✅ Closed-form solver implemented (Equation 9 from Willetts & Harrington 2024)
- ✅ `BalancerMultiTokenHop` in unified solver interface
- ✅ `BalancerMultiTokenSolver` in ArbSolver dispatch chain
- Multi-token (3+ tokens) basket arbitrage supported

**Möbius analysis**: The Balancer pairwise swap function `g(x) = B_out * (1 - (B_in/(B_in + gamma*x))^(w_in/w_out))` is **NOT** a Möbius transformation. The exponent `w_in/w_out` makes it a power-law function.

**However**: For **N-token basket arbitrage**, Willetts & Harrington (2024) derived a closed-form solution (Equation 9) that computes optimal deposit/withdrawal amounts per token for a given trade signature. This is fundamentally different from pairwise swap optimization — it optimizes the entire basket simultaneously.

**Implemented approach**: Signature enumeration + closed-form per signature:

| N tokens | Signatures | Full Solver Time |
|----------|------------|----------------|
| 3 | 12 | 576 μs |
| 4 | 50 | 1.3 ms |
| 5 | 180 | 2.9 ms |

**Key implementation fixes**:
1. `d_i = I_{s_i=1}` (indicator: 1 for deposit, 0 for withdraw) — NOT `d_i = signature[i]`
2. Reserves with different decimals must be upscaled to 18-decimal before formula
3. Profit computed in token-unit amounts, not raw wei

**Previous options (superseded by closed-form)**:

| Approach | Time | Status |
|---|---|---|
| ~~A. Brent fallback~~ | ~200μs | Superseded |
| ~~B. Newton with Balancer gradient~~ | ~10-20μs | Superseded |
| ~~C. Approximate as V2~~ | ~1μs | Superseded |
| ~~D. Mixed Möbius-Newton~~ | ~5-15μs | Superseded |
| **E. Closed-form Eq.9 (implemented)** | 576μs (N=3) | ✅ COMPLETE |

**Required changes**:
1. Add `Hop.invariant = BALANCER_WEIGHTED`
2. Add `Hop.weight_in: int | None` and `Hop.weight_out: int | None` (18-decimal fixed point)
3. Add `Hop.token_in_index: int | None` and `Hop.token_out_index: int | None` for multi-token pools
4. New `BalancerWeightedSolver(Solver)`
5. `ArbSolver` dispatches paths with Balancer hops to the mixed solver

### Gap 4: Curve Stableswap

**Affected pools**: `CurveStableswapPool`

**Current state**:
- Not in cycle class or `pool_to_hop()`
- Uses invariant: `A * n^n * Σxᵢ + D = A * n^n * D + (D^(n+1) / n^n / ∏xᵢ)`
- 2-8 tokens, with the `A` amplification coefficient
- `get_y()` uses Newton's method internally (5-50 iterations)
- Multiple D-variant groups for different contract implementations
- The swap function is inherently iterative — no closed-form output

**Möbius analysis**: Completely incompatible. The Curve invariant mixes a linear term (stablecoin-like) with a product term (constant-product-like). The parameter `A` controls the blend:
- A → ∞: pure stablecoin (1:1 price)
- A → 0: pure constant product

**Options**:

| Approach | Time | Feasibility |
|---|---|---|
| **A. Brent fallback** | ~500μs+ | Works but Curve's `get_y` is already iterative inside, making Brent very slow |
| **B. Newton outer, Curve get_y inner** | ~50-100μs | Newton on the arbitrage profit function, calling Curve's `get_y` as a subroutine. 3-5 outer iterations × ~5-50 inner iterations each. |
| **C. Linear approximation** | ~1μs | Near 1:1 price, Curve ≈ linear. `output ≈ input * (1 - fee)`. Only works for small price deviations. |
| **D. Piecewise approximation** | ~5-10μs | Curve has a "flat" region (≈1:1) and a "curved" region (≈constant product). Use linear in flat, Möbius in curved. |

**Recommendation**: **Option B** (Newton outer, Curve get_y inner). Curve's `get_y` is unavoidable — it's iterative by design. The outer Newton reduces iterations from Brent's 20-50 to 3-5. For mixed paths (Curve + V2/V3), use the same mixed Möbius-Newton approach as Balancer.

**Required changes**:
1. Add `Hop.invariant = CURVE_STABLESWAP`
2. Add `Hop.curve_A: int | None` (amplification coefficient)
3. Add `Hop.curve_n_coins: int | None`
4. Add `Hop.curve_precisions: tuple[int, ...] | None` (decimal scaling per token)
5. New `CurveStableswapSolver(Solver)` that calls `pool.get_dy()` as subroutine
6. Or: accept `Callable[[int], int]` on the Hop for opaque swap simulation

### Gap 5: PiecewiseMobiusSolver Not in Unified Interface

**Current state**: `MobiusOptimizer.solve_piecewise()` exists in the old optimizer API but `PiecewiseMobiusSolver(Solver)` is not implemented in `solver.py`. The `ArbSolver` falls back to Brent for V3 multi-range.

**Required changes**:
1. New `PiecewiseMobiusSolver(Solver)` wrapping `MobiusOptimizer.solve_piecewise()`
2. `SolverMethod.PIECEWISE_MOBIUS` already defined in the enum
3. `ArbSolver` dispatches V3 multi-range to `PiecewiseMobiusSolver` instead of Brent
4. Expected improvement: ~25μs vs ~400μs Brent

**Difficulty**: Low. The logic exists, just needs wiring.

### Gap 6: V3 Buy-Pool Skipped in Fast-Path

**Current state**: ✅ FIXED. The cycle class `_solver_fast_path` now uses actual V3 pool calculations (`calculate_tokens_out_from_tokens_in`) when the buy pool is V3/V4. This ensures tick crossings are handled correctly.

**What was done**:
1. Modified `_solver_fast_path` to detect V3/V4 buy pools via `isinstance` check
2. For V3/V4 buy pools: call actual pool `calculate_tokens_out_from_tokens_in` to get exact forward_token_amount
3. For V2 buy pools: use the constant-product formula (which is exact for V2)
4. If V3 calculation throws (tick crossing, insufficient liquidity), return None to fall back to Brent

**Result**: V3-V2 paths now use the fast solver (~5-15μs) instead of falling back to Brent (~400μs) when no tick crossing occurs. If tick crossing is detected, automatically falls back to Brent.

**Difficulty**: ✅ Done.

### Gap 7: Balancer/Curve Not in Cycle Class

**Current state**: The cycle class `Pool` type union is:
```python
type Pool = UniswapV2Pool | UniswapV3Pool | UniswapV4Pool | AerodromeV2Pool | AerodromeV3Pool
```

Balancer and Curve are not included. Adding them requires:
1. Extending the type union
2. Adding `_calculate_*` methods for each new pair combination
3. Adding ROE comparison logic for each new pool type
4. Handling multi-token pool structure (Balancer/Curve can have 2-8 tokens)

**Difficulty**: High. Architectural change. Multi-token pools have fundamentally different arbitrage structures — you're not always swapping between the same two tokens.

---

## Implementation Plan

### Phase A: Structural Changes to `Hop` (Foundation)

Extend `Hop` to represent all invariant types:

```python
class PoolInvariant(Enum):
    CONSTANT_PRODUCT = "constant_product"       # x*y=k (Uniswap V2, Aerodrome volatile)
    BOUNDED_PRODUCT = "bounded_product"          # V3/V4 concentrated liquidity
    SOLIDLY_STABLE = "solidly_stable"            # x³y + xy³ >= k
    BALANCER_WEIGHTED = "balancer_weighted"      # ∏(x^w) >= k
    CURVE_STABLESWAP = "curve_stableswap"        # A*n^n*Σx + D = ...

@dataclass(frozen=True, slots=True)
class Hop:
    reserve_in: int
    reserve_out: int
    fee: Fraction
    invariant: PoolInvariant = PoolInvariant.CONSTANT_PRODUCT
    
    # Asymmetric fee (Camelot)
    fee_out: Fraction | None = None
    
    # V3/V4 bounded liquidity
    liquidity: int | None = None
    sqrt_price: int | None = None
    tick_lower: int | None = None
    tick_upper: int | None = None
    
    # Solidly stable (Aerodrome stable, Camelot stable)
    decimals_in: int | None = None   # 10**decimals scaling
    decimals_out: int | None = None
    
    # Balancer weighted
    weight_in: int | None = None     # 18-decimal fixed point
    weight_out: int | None = None
    
    # Curve stableswap
    curve_A: int | None = None       # Amplification coefficient
    curve_n_coins: int | None = None
    curve_precisions: tuple[int, ...] | None = None
    curve_D: int | None = None       # Current invariant value
    curve_token_index_in: int | None = None
    curve_token_index_out: int | None = None
```

**Alternative (cleaner)**: Use a `HopVariant` tagged union instead of optional fields:

```python
@dataclass(frozen=True, slots=True)
class ConstantProductHop:
    reserve_in: int
    reserve_out: int
    fee: Fraction

@dataclass(frozen=True, slots=True)
class BoundedProductHop:
    reserve_in: int
    reserve_out: int
    fee: Fraction
    liquidity: int
    sqrt_price: int
    tick_lower: int
    tick_upper: int

@dataclass(frozen=True, slots=True)
class SolidlyStableHop:
    reserve_in: int
    reserve_out: int
    fee: Fraction
    decimals_in: int
    decimals_out: int

@dataclass(frozen=True, slots=True)
class BalancerWeightedHop:
    reserve_in: int
    reserve_out: int
    fee: Fraction
    weight_in: int
    weight_out: int

@dataclass(frozen=True, slots=True)
class CurveStableswapHop:
    reserve_in: int
    reserve_out: int
    fee: Fraction
    curve_A: int
    curve_n_coins: int
    curve_D: int
    token_index_in: int
    token_index_out: int
    precisions: tuple[int, ...]

Hop = ConstantProductHop | BoundedProductHop | SolidlyStableHop | BalancerWeightedHop | CurveStableswapHop
```

**Recommendation**: Tagged union. Cleaner, type-safe, no None-checking. The `SolveInput.hops` becomes `tuple[Hop, ...]` where `Hop` is the union type.

### Phase B: Wire PiecewiseMobiusSolver (Low-hanging fruit)

1. Create `PiecewiseMobiusSolver(Solver)` in `solver.py`
2. `supports()`: True when any hop is `BoundedProductHop` with multiple tick ranges
3. `solve()`: Delegate to `MobiusOptimizer.solve_piecewise()`
4. Update `ArbSolver` dispatch: V3 multi-range → `PiecewiseMobiusSolver` instead of Brent
5. **Expected improvement**: ~25μs vs ~400μs for V3 multi-range paths

### Phase C: Solidly Stable Solver

1. Create `SolidlyStableSolver(Solver)` using Newton's method
2. The outer profit function: `π(x) = g_solidly(x) - x` where `g_solidly` calls `get_y()` 
3. Analytical or numerical gradient for Newton
4. For mixed paths (Solidly + V2/V3): compose V2/V3 hops as Möbius, then Newton wrapper
5. Add `pool_to_hop()` support for `AerodromeV2Pool(stable=True)` and `CamelotLiquidityPool`
6. **Expected performance**: ~10-20μs (vs ~400μs Brent)

### Phase D: Asymmetric Fee Support

1. Add `fee_out` to `ConstantProductHop` (or use the tagged union `Hop`)
2. Update Möbius coefficient computation: each hop uses its directional `gamma`
3. Add `pool_to_hop()` for `CamelotLiquidityPool`
4. **Expected performance**: No change (Möbius already uses per-hop gamma)

### Phase E: Balancer Weighted Solver

1. Create `BalancerWeightedSolver(Solver)` using Newton's method
2. Analytical gradient: `dg_balancer/dx` has closed form from Balancer math
3. For mixed paths: compose V2/V3 hops as Möbius, then Newton wrapper with Balancer as opaque
4. Add `pool_to_hop()` for `BalancerV2Pool`
5. **Expected performance**: ~10-20μs

### Phase F: Curve Stableswap Solver

1. Create `CurveStableswapSolver(Solver)` using Newton's method
2. Inner `get_y()` is unavoidable (iterative). Outer Newton reduces from ~50 Brent iterations to 3-5
3. For mixed paths: same mixed Möbius-Newton pattern
4. Add `pool_to_hop()` for `CurveStableswapPool`
5. **Expected performance**: ~50-100μs (dominated by Curve's internal `get_y` iterations)

### Phase G: V3 Buy-Pool in Fast-Path

1. Enable V3 buy-pool virtual reserves in `_solver_fast_path_mixed`
2. Validate solver result with actual pool swap calculations
3. If validation shows tick crossing, fall back to Brent
4. **Expected improvement**: V3-V2 paths with no crossing go from ~400μs to ~5μs

### Phase H: Cycle Class Extension (Future)

1. Extend `Pool` type union to include `BalancerV2Pool`, `CurveStableswapPool`, `CamelotLiquidityPool`
2. Add `_calculate_*` methods for new pool pair combinations
3. This is the lowest priority — the solver infrastructure should be built first and validated independently

---

## Solver Dispatch Matrix (Target State)

After all phases, the `ArbSolver` dispatch logic:

| Path Composition | Solver | Time | Method |
|---|---|---|---|
| All constant-product V2 | MobiusSolver | ~1-5μs | Closed-form O(n) |
| V2 + V3 single-range | MobiusSolver | ~5μs | Closed-form O(n) with validation |
| V3 multi-range (no crossing) | MobiusSolver | ~5-15μs | Multi-candidate |
| V3 multi-range (with crossing) | PiecewiseMobiusSolver | ~25μs | Golden section ~25 iterations |
| V3-V3 complex | BrentSolver | ~400μs | Fallback |
| Any path with Solidly stable (swap_fn) | SolidlyStableSolver | ~283μs | Golden section + integer path eval |
| Any path with Solidly stable (float) | SolidlyStableSolver | ~257μs | Newton + float simulation |
| Any path with Balancer | BalancerMultiTokenSolver | ~576μs (N=3) | Closed-form Eq.9 per signature |
| Any path with Curve | CurveStableswapSolver | ~50-100μs | Newton + Curve get_y inner |
| Mixed V2/V3 + Solidly | SolidlyStableSolver | ~283μs | Golden section + mixed path eval |
| Mixed V2/V3 + Balancer | BalancerMultiTokenSolver (basket) | ~576μs (N=3) | Basket trades only; pairwise TBD |
| Mixed V2/V3 + Curve | MixedMobiusNewton | ~50-105μs | Möbius compose + Newton outer + Curve get_y |
| Camelot volatile (asymmetric fee) | MobiusSolver | ~1-5μs | Same as V2, directional gamma |
| Camelot stable | SolidlyStableSolver | ~283μs | Same as Aerodrome stable |

---

## Priority & Effort Estimate

| Phase | Impact | Effort | Priority | Status |
|---|---|---|---|---|
| **A**: Hop structural changes | Enables all other phases | Medium (3-5 days) | 🔴 P0 | ✅ Done |
| **B**: PiecewiseMobiusSolver wiring | High (25μs vs 400μs for V3 multi-range) | Low (1-2 days) | 🔴 P0 | ✅ Done |
| **C**: Solidly stable solver | High (Aerodrome stable is common on Base/Arbitrum) | Medium (3-5 days) | 🟠 P1 | ✅ Done |
| **D**: Asymmetric fees | Medium (Camelot volatile) | Low (1 day) | 🟠 P1 | ✅ Done |
| **G**: V3 buy-pool fast-path | Medium (extends Möbius to more V2-V3 paths) | Medium (2-3 days) | 🟡 P2 | ✅ Done |
| **E**: Balancer weighted | Low-medium (niche DEX) | High (5-7 days) | 🟢 P3 | ✅ Done (closed-form Eq.9) |
| **F**: Curve stableswap | Low-medium (niche DEX) | High (5-7 days) | 🟢 P3 | ❌ Remaining |
| **H**: Cycle class extension | Enables full integration | High (7-10 days) | 🔵 P4 | ❌ Remaining |

---

## Key Architectural Decision: Mixed Möbius-Newton Pattern

The most important insight is that **most paths will be mixed** — e.g., 3 V2 hops + 1 Solidly stable hop. The optimal architecture:

```
1. Partition the path into "Möbius-compatible" segments and "opaque" segments
2. Compose each Möbius segment into a single rational function l(x) = Kx/(M+Nx)
3. The profit function becomes: π(x) = l(remaining) - x, where "remaining" involves opaque hops
4. Use Newton's method on π(x) with analytical or numerical gradient
5. Each Newton iteration costs O(1) (one opaque pool evaluation + one Möbius evaluation)
```

This gives us:
- **Best case** (all V2/V3): Closed-form, zero iterations
- **Good case** (mostly V2 + 1 opaque): 3-5 Newton iterations, ~10-20μs
- **Worst case** (all opaque): Falls back to Brent, ~200-500μs

The `ArbSolver` automatically selects the best method based on the `Hop` invariant types.
