# Dual Decomposition

Based on "An Efficient Algorithm for Optimal Routing Through Constant Function Market Makers" by Diamandis, Resnick, Chitra, and Angeris (2023).

## The Key Insight

Instead of solving the routing problem directly, **relax it and decompose**:

1. **Primal Problem** (what we want):
   ```
   maximize    U(Ψ)                    (utility of net trade)
   subject to  Ψ = Σᵢ AᵢΔᵢ              (trades must balance)
               Δᵢ ∈ Tᵢ                (each trade valid for that pool)
   ```

2. **Dual Problem** (relax the coupling):
   ```
   maximize    U(Ψ) - νᵀ(Ψ - Σᵢ AᵢΔᵢ)
   subject to  Δᵢ ∈ Tᵢ
   ```
   The dual variables ν are "shadow prices" for each token.

3. **Separation**: Decomposes into independent subproblems:
   - **Utility subproblem**: `maximize U(Ψ) - νᵀΨ`
   - **Arbitrage subproblems**: `maximize (Aᵢᵀν)ᵀΔᵢ subject to Δᵢ ∈ Tᵢ`

4. **Magic**: At optimal ν*, the gradient ∇g(ν*) = 0 means trades automatically balance!

## Bounded Product CFMM for V3

Each V3 tick range is a **bounded product CFMM**:

```python
# Trading function for a tick range
φ(R) = (R₁ + α)(R₂ + β)

# Where:
α = sqrt(k / p_upper)   # Upper price bound parameter
β = sqrt(k × p_lower)   # Lower price bound parameter
k = liquidity in that range
```

**Closed-form arbitrage** for bounded product:

```python
def find_arbitrage(R0, R1, alpha, beta, k, external_price):
    # New reserves at optimum: pool's marginal price = external price
    R1_new = sqrt(k / external_price)
    delta_1 = R1 - R1_new
    R0_new = k / R1_new
    delta_0 = (R0_new - R0) / fee
    return (-delta_0, delta_1)
```

## Numerical Example: Two-Pool Arbitrage

**Setup**:
- Pool A: 2M USDC / 1000 WETH → price = 2000 USDC/WETH
- Pool B: 2.04M USDC / 1000 WETH → price = 2040 USDC/WETH

**Equilibrium calculation**:
```python
sqrt_nu = (sqrt(2e9) + sqrt(2.04e9)) / 2000 = 44.94
nu_star = 2020 USDC/WETH
```

**Trades at ν* = 2020**:

| Pool | Trade |
|------|-------|
| Pool A | Pay 9,980 USDC → Receive 4.95 WETH |
| Pool B | Sell 4.97 WETH → Receive 10,050 USDC |
| **Net** | **69 USDC profit** (balanced WETH) |

## Algorithm

```python
def dual_decomposition(markets, initial_prices):
    ν = initial_prices
    
    for iteration in range(max_iterations):
        # 1. Solve arbitrage for each market (PARALLEL!)
        for market in markets:
            Δ[market] = find_arbitrage(market, ν)  # Closed-form for V2/V3
        
        # 2. Compute gradient (imbalance)
        gradient = sum(Δ)
        
        # 3. Update prices
        ν = ν - learning_rate × gradient
        
        # 4. Check convergence
        if |gradient| < tolerance:
            break
    
    return ν, Δ
```

## Complexity Comparison

| Method | V2 Pool | V3 Pool | Parallelizable? |
|--------|---------|---------|----------------|
| Brent | O(iterations) × pool_calc | O(iterations) × pool_calc | No |
| CVXPY | O(1) (closed-form) | Not applicable | Yes |
| **Dual Decomposition** | **O(1)** | **O(log n)** binary search | **Yes** |

## Production Implementation

**File**: `src/degenbot/arbitrage/optimizers/multi_token.py`

### Key Classes

- **`MultiTokenRouter`**: Main entry point for multi-path optimization
- **`DualDecompositionSolver`**: L-BFGS-B for fast convergence, gradient descent fallback
- **`solve_market_arbitrage()`**: Closed-form V2 market solution — `x = (sqrt(γ * k / m) - R_in) / γ`
- **`MarketInfo`**: Market (pool) representation with optimal delta/lambda
- **`PathInfo`**: Path representation with profit calculation

### Performance

| Paths | Solve Time |
|-------|------------|
| 1 | ~5ms |
| 5 | ~8ms |
| 10 | ~12ms |

### Shadow Price Initialization

```python
def _initialize_prices(markets, n_tokens):
    # Pool price: R_out / R_in gives price ratio
    pool_price = market.reserve_out / market.reserve_in
    # Use geometric mean of estimates for each token pair
    nu[out_idx] = geometric_mean(pool_prices)
```

With proper initialization, L-BFGS-B converges in fewer iterations and finds better solutions.

## Test Files

| File | Tests |
|------|-------|
| `tests/arbitrage/test_optimizers/dual_decomposition.py` | BoundedProductCFMM, UniV3AggregateCFMM, closed-form solutions |
| `tests/arbitrage/test_optimizers/test_dual_decomposition.py` | 10 tests |
| `tests/arbitrage/test_optimizers/numerical_example.py` | Step-by-step walkthrough |
| `tests/arbitrage/test_optimizers/test_multi_token.py` | 21 tests for MultiTokenRouter |

## Key Classes Implemented

- **`BoundedProductCFMM`**: V3 tick range as bounded product CFMM with closed-form arbitrage
- **`UniV3AggregateCFMM`**: Full V3 pool as aggregate of bounded products, binary search O(log n)
- **`DualDecompositionRouter`**: Optimal routing solver finding market-clearing prices
- **`compute_arbitrage_amount()`**: One-line closed-form: `delta = (sqrt(fee * price * k) - R0) / fee`
