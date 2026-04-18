# Improvement Proposals

## Completed Proposals

### 1. Möbius Transformation for V2 Paths ✅ COMPLETE

**Result**: Python 0.86μs (225x faster), Rust 0.19μs (1021x faster), zero iterations.

See [`research/mobius-transformation.md`](research/mobius-transformation.md).

### 2. Newton's Method for V2-V2 ✅ COMPLETE

**Result**: 7.5μs (26x faster), 3-4 iterations. Useful fallback.

### 3. Möbius V3 Single-Range ✅ COMPLETE

**Result**: O(1) closed-form, ~5μs. V3 tick ranges as Möbius hops with effective reserves.

### 4. Piecewise-Möbius V3 Multi-Range ✅ COMPLETE

**Result**: ~25μs with golden section search. 200x faster than V2V3Optimizer.

### 5. Vectorized Batch Möbius ✅ COMPLETE

**Result**: 0.14μs/path (Python), 0.09μs/path (Rust) at 1000 paths.

### 6. Rust Integer Möbius ✅ COMPLETE

**Result**: 0.88μs with uint256 arithmetic, EVM-exact. Not-profitable rejection at 0.32μs.

### 7. Dual Decomposition Multi-Token Routing ✅ COMPLETE

**Result**: Multi-path simultaneous optimization at ~5-12ms.

### 8. Hybrid Optimizer with Auto-Selection ✅ COMPLETE

**Result**: Automatically selects optimal solver based on pool types.

### 9. Vectorized Newton Batch ✅ COMPLETE

**Result**: 0.53μs/path at 1000 paths. Superseded by vectorized Möbius (4.4x faster).

### 10. Closed-Form N-Token Balancer Weighted Pool ✅ COMPLETE

**Result**: Equation 9 from Willetts & Harrington (2024), ~3μs per signature evaluation.

Based on "Closed-form solutions for generic N-token AMM arbitrage" (arXiv:2402.06731).

**Two critical implementation bugs discovered and fixed**:
1. `d_i = I_{s_i=1}` (indicator: 1 for deposit, 0 for withdraw) — NOT `d_i = signature[i]`
2. Reserves with different decimals must be upscaled to 18-decimal before formula

**Performance**:

| Pool Size | Single Eq.9 | Full Solver | Signatures |
|-----------|-------------|-------------|------------|
| N=3 | 3.9 μs | 576 μs | 12 |
| N=4 | 3.4 μs | 1.3 ms | 50 |
| N=5 | 3.9 μs | 2.9 ms | 180 |

**Integration**: `BalancerMultiTokenHop` + `BalancerMultiTokenSolver` in ArbSolver dispatch.

See [`docs/balancer_solver_implementation.md`](../../docs/balancer_solver_implementation.md) for full details.

---

## Pending Proposals

### 10. Gas Cost Modeling

**Problem**: Optimizer maximizes gross profit without deducting gas costs.

**Solution**: Include gas cost in objective function:
```python
def net_profit(x, gas_cost_eth, gas_price, token_price):
    gross = swap_output(x) - x
    gas_eth = gas_cost_eth * gas_price
    return gross - gas_eth / token_price
```

**Effort**: Medium. Requires gas estimation per pool type and current gas price oracle.

**Priority**: High. Essential for MEV operations — some "profitable" trades are net-negative after gas.

### 11. Tick Bitmap Caching (Production)

**Problem**: Each V3 optimization requires RPC calls for tick bitmap (100ms+).

**Solution**: Cache tick bitmap in pool object with block-based invalidation.

**Effort**: Medium.

**Priority**: High. 100ms+ saved per optimization for V3 pools.

### 12. Pre-Computed Tick Transition Tables

**Problem**: Computing tick crossings at solve time is expensive.

**Solution**: Pre-compute crossing amounts for hot V3 pools. Store as lookup table indexed by tick range.

**Effort**: Medium. Requires maintaining table per pool, invalidating on liquidity changes.

**Priority**: Medium. Benefits hot pools (high TVL, frequent arbitrage).

### 13. L-BFGS-B for Dual Decomposition

**Problem**: Current dual decomposition uses simple gradient ascent. Convergence is slow.

**Solution**: Use L-BFGS-B (quasi-Newton) for shadow price updates:
```python
from scipy.optimize import minimize

def update_prices(nu, gradient, hessian_approx):
    result = minimize(lambda x: -lagrangian(x), nu, jac=gradient, method='L-BFGS-B')
    return result.x
```

**Expected improvement**: 5-10x fewer iterations.

**Effort**: Medium.

**Priority**: Medium.

### 14. GPU Acceleration (CuPy)

**Problem**: Even vectorized CPU processing has limits for massive batch (10K+ paths).

**Solution**: Use CuPy (CUDA NumPy) for GPU-accelerated batch processing:
```python
import cupy as cp

# Transfer pool states to GPU
gpu_paths = cp.asarray(path_states)
# All vectorized operations run on GPU
results = vectorized_mobius.solve(gpu_paths)
```

**Expected improvement**: 100x+ for batches >10K paths.

**Effort**: High. Requires CUDA GPU, CuPy installation, GPU memory management.

**Priority**: Low. Only justified for massive-scale MEV operations.

### 15. ML for Optimal Input Prediction

**Problem**: Finding the right starting point / candidate selection takes time.

**Solution**: Train a model to predict optimal input amount from pool state features.

**Expected improvement**: Could reduce iterations or candidate evaluation.

**Effort**: High. Research project with uncertain returns.

**Priority**: Low.

### 16. Slippage Bounds & Deadline Checks

**Problem**: No protection against sandwich attacks or stale prices.

**Solution**: Add minimum output and deadline checks to optimizer output.

**Effort**: Low.

**Priority**: Low. MEV protection for user-facing integrations.

### 17. Telemetry for Optimizer Performance

**Problem**: No visibility into which optimizers are used, solve times, or failure rates in production.

**Solution**: Add structured logging/metrics:
```python
import structlog
logger = structlog.get_logger()

def solve_with_telemetry(optimizer, pools, input_token):
    with logger.timer("optimizer.solve", optimizer=type(optimizer).__name__):
        result = optimizer.solve(pools, input_token)
    logger.info("optimizer.result", profit=result.profit, method=type(optimizer).__name__)
    return result
```

**Effort**: Low.

**Priority**: Low. Useful for production monitoring.

---

## Superseded / Tested & Rejected

| Item | Reason |
|------|--------|
| Analytical quartic solution | Superseded by Möbius (exact, O(n), faster) |
| Piecewise V3 convex approximation | Superseded by Piecewise-Möbius |
| Adaptive initial guess (Newton) | Overhead > savings at 7.5μs scale |
| Smart bracket (Brent) | scipy ignores bracket parameter |
| CVXPY for V2 production | 7x slower than Brent; Möbius 1021x faster |
| Thread parallelism (Python) | GIL prevents speedup |
| Serial batch processing | Superseded by vectorized Möbius (23x faster) |
| Integer binary search for EVM-exact | Superseded by Rust Integer Möbius (0.88μs vs 300μs) |
| Log-domain CVXPY formulation | Superseded by Möbius for V2; useful only for research |
| Unified scaling strategy | Superseded by Möbius for V2; CVXPY rarely used in production |
