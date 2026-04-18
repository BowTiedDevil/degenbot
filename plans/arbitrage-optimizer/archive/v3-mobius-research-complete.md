# V3/V4 Möbius Transformation: Complete Research

**Research Period**: 2026-04-14  
**Implementation Status**: Complete (Phases 11-12)  
**Code Location**: `src/degenbot/arbitrage/optimizers/mobius.py`

This document consolidates two research phases: (1) proving V3 tick ranges are Möbius transformations, and (2) discovering why tick crossings cannot use sequential composition.

---

## Part 1: Theoretical Foundation — V3 as Bounded Product CFMM

### The Core Insight

**The bounded product CFMM (V3/V4 tick range) swap IS a Möbius transformation.**

This means the existing `MobiusV2Optimizer` could be generalized to handle V3 and V4 pools with a one-line conceptual change: replace `reserve_in` with `reserve_in + α` and `reserve_out` with `reserve_out + β`, where α and β are the bounded product parameters.

For mixed V2/V3 paths where V3 pools stay within a single tick range, the Möbius composition formula works directly — same O(n) recurrence, same closed-form optimal input, zero iterations.

### Mathematical Foundation

#### V2 Constant Product Swap

For a V2 pool with reserves (r, s) and fee multiplier γ = 1 - fee:

```
y = γ·s·x / (r + γ·x)
```

This is a **Möbius transformation** (fractional linear transformation) that fixes the origin. Möbius transformations form a group under composition, so any n-hop V2 path collapses to:

```
l(x) = K·x / (M + N·x)
```

with optimal input:

```
x_opt = (√(K·M) - M) / N
```

#### V3 Bounded Product CFMM Swap

Each V3 tick range has trading function:

```
φ(R) = (R₀ + α)(R₁ + β) ≥ L²
```

where:
- α = L / √P_upper (lower bound parameter on token0)
- β = L × √P_lower (lower bound parameter on token1)
- L = liquidity in this range

When we swap x units of token0 into this range, the new reserves satisfy:

```
(R₀ + γx + α)(R₁ - y + β) = L²
```

Solving for output y:

```
y = R₁ + β - L² / (R₀ + γx + α)
  = (R₁ + β) · γx / (R₀ + α + γx)
```

**This is exactly the Möbius form `γ·s_eff·x / (r_eff + γ·x)` with:**

```
s_eff = R₁ + β    (effective output reserve)
r_eff = R₀ + α    (effective input reserve)
```

The V2 case is recovered when α = 0, β = 0.

### Implications

Since each V3 tick range swap is a Möbius transformation:

1. **Composition**: A V2 hop followed by a V3 hop (within one range) composes into a single Möbius transformation
2. **Recurrence**: The same O(n) recurrence for (K, M, N) works unchanged — just use `r_eff` and `s_eff`
3. **Optimal input**: Same closed-form `x_opt = (√(K·M) - M) / N`
4. **Profitability check**: Same `K/M > 1` condition

### Implementation Approach

The key was creating effective reserves from V3 tick range parameters:

```python
def hop_state_from_v3_tick_range(
    liquidity: int,
    sqrt_price_current: int,  # X96 format
    sqrt_price_lower: int,
    sqrt_price_upper: int,
    fee: float,
    zero_for_one: bool,
) -> HopState:
    sqrt_price = sqrt_price_current / (2**96)
    sqrt_p_lower = sqrt_price_lower / (2**96)
    sqrt_p_upper = sqrt_price_upper / (2**96)
    
    L = float(liquidity)
    alpha = L / sqrt_p_upper
    beta = L * sqrt_p_lower
    
    if zero_for_one:
        R0 = L / sqrt_price - alpha
        R1 = L * sqrt_price - beta
        return HopState(
            reserve_in=R0 + alpha,   # r_eff
            reserve_out=R1 + beta,   # s_eff
            fee=fee,
        )
    # ...
```

**Key observation**: `reserve_in = R_current + bound_parameter`. No change to the dataclass fields — just different values for V3.

---

## Part 2: The Constraint — Why Tick Crossings Can't Use Sequential Composition

### The Question

The Möbius formula works for multi-hop V2 paths (sequential composition). Can a V3 pool with tick crossings be decomposed into a set of V2-equivalent sequential hops?

### Short Answer

**No.** V3 tick crossing has a fundamentally different algebraic structure than V2 multi-hop composition.

| Structure | Input→Output Relationship | Algebra |
|-----------|--------------------------|---------|
| V2 multi-hop | Output of hop i = Input of hop i+1 | **Sequential composition** (Möbius group) |
| V3 tick crossing | Input consumed across ranges, outputs ADD | **Additive** (not compositional) |

### Algebraic Analysis

#### V2 Multi-Hop: Sequential Composition

For a V2 path [Pool₁, Pool₂, Pool₃]:

```
x → Pool₁ → y₁ → Pool₂ → y₂ → Pool₃ → z
```

Output of hop *i* becomes the input of hop *i+1*. This is **function composition**: `z = l₃(l₂(l₁(x)))`, which is a Möbius transformation because Möbius transformations form a group under composition.

#### V3 Tick Crossing: Additive Structure

For a V3 swap that crosses from Range 1 into Range 2:

```
x → Range 1 (consumes x₁, outputs y₁)
  → remaining input (x - x₁ - fee₁) → Range 2 (consumes x₂, outputs y₂)

Total output = y₁ + y₂  (ADDITIVE, not compositional)
```

The **input** carries forward across boundaries (not the output), and the **outputs sum up**.

### Numerical Proof

Using V3 `compute_swap_step` with integer arithmetic:

| Component | Amount |
|-----------|--------|
| Total input | 100,000,000,000,000,000 |
| Range 1 consumed | 1,507,827,088,871,927 |
| Range 1 output | 1,496,554,276,216,581 |
| Remaining input | 98,492,172,911,128,073 |
| Range 2 consumed | 6,044,895,097,444,111 |
| Range 2 output | 5,972,765,609,197,073 |
| **V3 total output** | **7,469,319,885,413,654** |

If we incorrectly model this as sequential V2 composition (output of R1 feeds into R2), the result is **87,228,396,369,988,320** — **11.7x too large**.

The sequential model is catastrophically wrong because it treats the token1 output as a token0 input for the next range.

### Why "V2-Equivalent Hops" Don't Work

1. **Token mismatch**: Range 1 outputs token1 (for z0f1), but Range 2 expects token0 input. In V2 composition, the output token naturally matches the next pool's input token.

2. **Additive vs compositional**: The V3 contract accumulates outputs while consuming inputs — parallel operations on the same input, not sequential transformations.

3. **Fixed breakpoints**: The tick crossing amounts are FIXED (determined by liquidity and boundary prices, not by total input).

### The Solution: Piecewise-Möbius

For a swap that ends in Range K:

```
total_output(x) = Σ(fixed_outputsᵢ for i=1..K-1) + mobius(x - fixed_crossing_total, range_K_params)
```

This is a **shifted Möbius function** with a fixed offset. The profit function is:

```
P(x) = [fixed_output_sum + mobius_output(x - fixed_input_sum)] - x
```

Since the fixed parts don't depend on x, we can use golden section search starting from the single-range Möbius solution as an initial bracket.

**Key insight**: The crossing amounts are **fixed** (independent of total input), so the piecewise structure has fixed breakpoints.

---

## Part 3: Implementation Results

### What Was Implemented

The research findings were implemented in Phase 11-12:

```python
# From mobius.py

@dataclass(frozen=True)
class TickRangeCrossing:
    """Pre-computed crossing data (fixed for a given starting state)."""
    input_consumed: int  # Fixed amount to reach boundary
    output_produced: int  # Fixed output from crossed range
    fee_paid: int

@dataclass(frozen=True)
class V3TickRangeSequence:
    """Ordered sequence of V3 tick ranges."""
    ranges: tuple[V3TickRangeHop, ...]
    
    def compute_crossing(self, k: int) -> TickRangeCrossing:
        """Compute total crossing to reach range k."""
        # Sum of fixed amounts for ranges 0..k-1
        
def piecewise_v3_swap(x: int, sequence: V3TickRangeSequence, end_range: int) -> int:
    """Compute V3 swap output including fixed crossings."""
    crossing = sequence.compute_crossing(end_range)
    remaining_input = x - crossing.input_consumed - crossing.fee_paid
    final_output = mobius_output(remaining_input, sequence.ranges[end_range])
    return crossing.output_produced + final_output

def solve_piecewise(
    self,
    v3_sequence: V3TickRangeSequence,
    other_hops: list[HopState],
    max_ranges: int = 3,
) -> MobiusResult:
    """Optimize using golden section search."""
    # Try each candidate ending range
    for k in range(min(max_ranges, len(v3_sequence.ranges))):
        # Golden section search on shifted profit function
        # Starting bracket from single-range Möbius solution
```

### Performance Results

| Method | Per-Candidate Time | 3 Candidates Total |
|--------|-------------------|--------------------|
| V2V3Optimizer (Newton) | ~5ms | ~15ms |
| V2V3Optimizer (Brent) | ~5ms | ~15ms |
| **Möbius single-range** | **~5μs** | **~5μs** |
| **Möbius piecewise** | **~25μs** | **~25μs** |
| **Speedup** | | **~600x** |

### V4 Considerations

Uniswap V4 uses the same tick-based concentrated liquidity model:

- **Same tick math**: V4 uses the same `1.0001^tick` price formula
- **Hooks caveat**: Custom hooks may modify swap behavior, invalidating the constant product assumption
- **Different fees**: V4 supports dynamic fees, but the Möbius recurrence handles per-hop fees transparently

---

## Summary Table: What Works When

| Scenario | Approach | Time | Status |
|----------|----------|------|--------|
| V2 only | Möbius closed-form | ~5μs | ✅ Implemented |
| V2 + V3 (single range) | Möbius closed-form | ~5μs | ✅ Implemented |
| V3 only (single range) | Möbius closed-form | ~5μs | ✅ Implemented |
| V3 (multi-range, crossing) | Piecewise-Möbius + golden section | ~25μs | ✅ Implemented |
| V3-V3 (both with crossings) | Candidate pairs + closed-form | ~25-125μs | ✅ Implemented |
| V4 (no hooks) | Same as V3 | Same as V3 | ✅ Supported |
| V4 (with hooks) | Fallback to pool simulation | Variable | ⚠️ Hook-dependent |

---

## Key Lessons from This Research

1. **V3 tick ranges ARE Möbius transformations** — but only within a single range
2. **Tick crossings are ADDITIVE, not compositional** — cannot use Möbius group property across boundaries
3. **Crossing amounts are FIXED** — independent of total input, enabling piecewise optimization
4. **Golden section on shifted function** — ~25 iterations at ~25μs, still 600x faster than iterative
5. **Sequential V2 decomposition FAILS** — 11.7x error demonstrated numerically

---

## References

- **Hartigan, J. (2026)**. "The Geometry of Arbitrage: Generalizing Multi-Hop DEX Paths via Möbius Transformations."
- **Angeris, G. et al. (2023)**. "The Geometry of Constant Function Market Makers." — Shows V3 tick ranges are bounded product CFMMs
- **Adams, H. et al. (2021)**. "Uniswap v3 Core." — Original tick-based concentrated liquidity specification
- **Implementation**: `src/degenbot/arbitrage/optimizers/mobius.py`
- **Tests**: `tests/arbitrage/test_optimizers/test_mobius_v3.py`, `test_piecewise_mobius.py`
