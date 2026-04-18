# Unified Solver Plan

## Problem

There are 9+ optimizer classes with different APIs:

| Optimizer | Input Type | Key Method |
|-----------|-----------|------------|
| `NewtonV2Optimizer` | `list[Pool], Erc20Token` | `solve()` |
| `MobiusOptimizer` | `list[Pool/V3TickRangeHop], Erc20Token` | `solve()` / `solve_v3_candidates()` / `solve_piecewise()` |
| `ChainRuleNewtonOptimizer` | `list[Pool], Erc20Token` | `solve()` |
| `HybridOptimizer` | `list[Pool], Erc20Token` | `solve()` |
| `V2V3Optimizer` | `V2PoolState, V3PoolState` | `optimize()` |
| `MultiTokenRouter` | `list[PathInfo], list[TokenInfo]` | `solve()` |
| `BatchNewtonOptimizer` | `list[(pool_buy, pool_sell, token)]` | `solve_batch()` |
| `BatchMobiusOptimizer` | `list[BatchMobiusPathInput]` | `solve_batch()` |
| `BrentOptimizer` | N/A (placeholder) | N/A |

**Consequences**:
1. The production cycle class (`uniswap_2pool_cycle_testing.py`) calls `scipy.optimize.minimize_scalar` directly — no optimizer is used
2. Each optimizer converts pool objects to its own internal format (reserves, fees) — duplicated work
3. `HybridOptimizer` tries to unify but classifies pools by class name strings (`"V2" in type(pool).__name__`)
4. The Rust PyO3 boundary needs a clean, serializable input format
5. Testing is harder because each optimizer has different input requirements

## Design

### Core Types

```python
@dataclass(frozen=True, slots=True)
class Hop:
    """
    A single pool hop in an arbitrage path.
    
    Encodes everything needed for swap math: reserves, fee, and optional
    V3 bounded-liquidity data. Every optimizer accepts a list of Hops.
    """
    reserve_in: int        # Input reserve in wei
    reserve_out: int        # Output reserve in wei
    fee: Fraction           # Fee as Fraction (e.g. Fraction(3, 1000) for 0.3%)
    
    # V3/V4 bounded liquidity (None for V2)
    liquidity: int | None = None
    sqrt_price: int | None = None        # X96 sqrt price
    tick_lower: int | None = None
    tick_upper: int | None = None

@dataclass(frozen=True, slots=True)  
class SolveInput:
    """
    Unified input for all optimizers.
    """
    hops: tuple[Hop, ...]
    max_input: int | None = None

@dataclass(frozen=True, slots=True)
class SolveResult:
    """
    Unified output from all optimizers.
    """
    optimal_input: int     # Optimal input in wei
    profit: int            # Expected profit in wei
    success: bool
    iterations: int
    method: SolverMethod    # Which solver was used
    error: str | None = None
```

### Solver Interface

```python
class Solver(ABC):
    """Base class for all optimizers."""
    
    @abstractmethod
    def solve(self, input: SolveInput) -> SolveResult:
        """Solve for optimal arbitrage."""
        ...
    
    @abstractmethod  
    def supports(self, input: SolveInput) -> bool:
        """Whether this solver can handle the given input."""
        ...
```

### Method Selection (the new "Hybrid")

```python
class ArbSolver(Solver):
    """
    Top-level solver that dispatches to the best method.
    
    Selection logic:
    1. BalancerMultiTokenHop → BalancerMultiTokenSolver (closed-form Eq.9)
    2. All V2, no V3 data → Möbius (closed-form, zero iterations)
    3. V3 single-range → Möbius with V3 validation
    4. V3 multi-range → Piecewise-Möbius (golden section)
    5. V3-V3 complex → Brent (fallback)
    """
    
    def solve(self, input: SolveInput) -> SolveResult:
        method = self._select_method(input)
        return method.solve(input)
```

### Conversion from Pool Objects

```python
def pool_to_hop(pool, input_token) -> Hop:
    """Convert a pool object to a Hop for the solver."""
    if isinstance(pool, UniswapV2Pool):
        if input_token == pool.token0:
            return Hop(
                reserve_in=pool.state.reserves_token0,
                reserve_out=pool.state.reserves_token1,
                fee=pool.fee,
            )
        else:
            return Hop(
                reserve_in=pool.state.reserves_token1,
                reserve_out=pool.state.reserves_token0,
                fee=pool.fee,
            )
    elif isinstance(pool, UniswapV3Pool):
        # Extract current tick range + liquidity
        ...
```

## Implementation Steps

### Step 1: Create `Hop`, `SolveInput`, `SolveResult`, `SolverMethod`, `Solver` types
- New file: `src/degenbot/arbitrage/optimizers/solver.py`
- `SolverMethod` enum: MOBIUS, NEWTON, BRENT, PIECEWISE_MOBIUS
- `Solver` ABC with `solve()` and `supports()`

### Step 2: Implement `MobiusSolver(Solver)`
- Wraps existing `mobius_solve()`, `compute_mobius_coefficients()`
- Accepts `SolveInput` → extracts `list[HopState]` → runs closed-form → returns `SolveResult`
- V3 validation: check sqrt price stays in range
- `supports()`: True when all hops have V2 or V3 single-range data

### Step 3: Implement `PiecewiseMobiusSolver(Solver)`
- Wraps existing `solve_piecewise()` logic
- `supports()`: True when V3 hops have multiple tick ranges

### Step 4: Implement `NewtonSolver(Solver)`
- Wraps existing `NewtonV2Optimizer` for 2-hop V2
- `supports()`: True for exactly 2 V2 hops

### Step 5: Implement `BrentSolver(Solver)`
- Wraps `scipy.optimize.minimize_scalar` with profit function
- `supports()`: Always True (fallback)

### Step 6: Implement `ArbSolver(Solver)` — the dispatcher
- Method selection based on hop types
- Returns `SolveResult` with `method` field indicating which solver ran

### Step 7: Conversion utilities
- `pool_to_hop(pool, input_token) -> Hop`
- `v3_tick_range_to_hop(range_data) -> Hop`

### Step 8: Wire into cycle class
- `uniswap_2pool_cycle_testing.py._calculate_v2_v2` → use `ArbSolver`
- Other branches follow as they're validated

### Step 9: Batch solver
- `ArbBatchSolver` accepting `list[SolveInput]`
- Groups by hop count, uses vectorized Möbius

### Step 10: Rust boundary
- `Hop` and `SolveInput` are pure data — trivial to serialize to Rust
- Rust `ArbSolver` mirrors Python method selection

## Key Design Decisions

1. **`Hop` uses int wei values, not float** — EVM-exact from the start. Internal math can convert to float, but the interface is integer.

2. **`fee` is `Fraction`, not float** — Exact representation. `Fraction(3, 1000)` not `0.003`. Avoids float precision issues at the boundary.

3. **`SolveInput.hops` is a tuple, not list** — Frozen, hashable, cacheable.

4. **V3 data is optional fields on `Hop`** — A V2 hop just has `reserve_in/out/fee`. A V3 hop also has `liquidity`, `sqrt_price`, `tick_lower`, `tick_upper`. One type, not a type hierarchy.

5. **Solver ABC is minimal** — Just `solve()` and `supports()`. No `optimizer_type` property. The `SolveResult.method` field tells you what ran.

6. **Old optimizers remain** — This is additive. The old classes stay for backward compatibility. New code uses `ArbSolver`.
