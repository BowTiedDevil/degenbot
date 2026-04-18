# Vectorized Batch Möbius Optimizer

## Overview

Extends the Möbius transformation optimizer to batch evaluation using NumPy, following the same pattern as `BatchNewtonOptimizer` but with a key advantage: **zero iterations**. The Möbius approach computes a closed-form optimal input for each path via an O(n) coefficient recurrence followed by one `np.sqrt` call, so the vectorized version simply runs the recurrence across all paths simultaneously.

Additionally implements log-domain computation with log-sum-exp for N to handle float64 overflow in long paths with EVM-scale reserves.

**File**: `src/degenbot/arbitrage/optimizers/batch_mobius.py`

**Tests**: `tests/arbitrage/test_optimizers/test_batch_mobius.py` (56 tests)

## Implementation

### Core Solver: `VectorizedMobiusSolver`

Processes all paths with the same hop count simultaneously:

```python
# Initialize recurrence (vectorized across batch dimension)
K = gammas[:, 0] * reserves_out[:, 0]   # shape (num_paths,)
M = reserves_in[:, 0].copy()             # copy to avoid mutating input
N = gammas[:, 0].copy()

# Forward pass: one update per hop (no iterations!)
for j in range(1, num_hops):
    old_K = K.copy()
    K = old_K * gammas[:, j] * reserves_out[:, j]
    M *= reserves_in[:, j]
    N = N * reserves_in[:, j] + old_K * gammas[:, j]

# Closed-form optimal input (single vectorized call)
half_diff = 0.5 * (log_K - log_M)
x_opt = (sqrt(K*M) - M) / N            # direct (no overflow)
x_opt = exp(log_M + log(expm1(half_diff)) - log_N)  # log-domain (overflow)

# Profit at x_opt (simplified formula)
profit = x_opt * expm1(half_diff)       # works in both domains
```

### Log-Domain Overflow Handling

For paths with many hops and large reserves, K, M, and N can overflow float64 (max ~1.8e308). Example: 20 hops with 1e18 reserves gives M = (1e18)^20 = 1e360.

The solver computes log-domain counterparts simultaneously:
- `log_K`, `log_M`: Simple cumulative sums (never overflow)
- `log_N`: Requires **log-sum-exp** trick since N is a sum of products

N recurrence: `N_j = N_{j-1} * r_in_j + K_{j-1} * gamma_j`

In log domain:
```
a = log(N_{j-1}) + log(r_in_j)
b = log(K_{j-1}) + log(gamma_j)
log(N_j) = max(a, b) + log1p(exp(-|a - b|))
```

When overflow is detected (`~np.isfinite(K*M)`), the solver uses log-domain formulas:
- Profitability: `log_K > log_M`
- Optimal input: `x_opt = exp(log_M + log(expm1(half_diff)) - log_N)`
- Profit: `profit = x_opt * expm1(half_diff)` (exact at optimal input)

### Profit at Optimal Input

At x_opt, the denominator simplifies: `M + N*x_opt = sqrt(K*M)`, giving:

```
profit = x_opt * (sqrt(K/M) - 1) = x_opt * expm1(half_diff)
```

This formula works in both direct and log-domain computation, avoiding the need for K*x/(M+N*x) which overflows.

### Input Array Safety

The solver copies all extracted numpy array slices to avoid mutating the input `hops_array`. The original implementation had a bug where `M = reserves_in[:, 0]` created a view, and `M *= reserves_in[:, j]` modified the input array in place.

### High-Level API: `BatchMobiusOptimizer`

Groups paths by hop count and routes to vectorized or serial solver based on group size:

```python
optimizer = BatchMobiusOptimizer(min_paths_for_batch=20)
results = optimizer.solve_batch([BatchMobiusPathInput(hops=[...]), ...])
best_idx, best = optimizer.get_best_path(paths)
```

## Bug Fixes

1. **Input array mutation**: `M = reserves_in[:, 0]` created a numpy view. `M *= reserves_in[:, j]` overwrote `hops_array[:, 0, 0]` with the accumulated M product. Fixed by copying extracted slices: `reserves_in = hops_array[:, :, 0].copy()`.

2. **Log-domain N computation**: Initially tried `log_N = np.log(np.abs(N))` for the overflow case, but N also overflows. Fixed by computing log(N) during the forward pass using log-sum-exp.

## Performance Results

### Table 1: Vectorized vs Serial Möbius (3-hop paths)

| Paths | Serial (μs) | Vectorized (μs) | Speedup | Per-Path (μs) |
|-------|------------|-----------------|---------|----------------|
| 1 | 5.9 | 62.8 | 0.09x | 62.833 |
| 10 | 59.2 | 67.7 | 0.87x | 6.768 |
| 20 | 84.0 | 61.0 | **1.38x** | 3.052 |
| 50 | 199.0 | 63.4 | **3.14x** | 1.267 |
| 100 | 412.3 | 93.7 | **4.40x** | 0.937 |
| 200 | 863.9 | 108.2 | **7.98x** | 0.541 |
| 500 | 2021.9 | 119.6 | **16.90x** | 0.239 |
| 1000 | 4453.5 | 182.0 | **24.48x** | 0.182 |

Crossover: ~20 paths (same as vectorized Newton).

### Table 2: Batch Möbius vs Batch Newton (2-hop V2-V2 paths)

| Paths | Möbius (μs) | Newton (μs) | Möbius Speedup | Per-Path Möbius (μs) |
|-------|------------|------------|-----------------|---------------------|
| 10 | 61.3 | 267.4 | **4.36x** | 6.128 |
| 50 | 91.3 | 263.3 | **2.88x** | 1.825 |
| 100 | 62.6 | 276.0 | **4.41x** | 0.626 |
| 500 | 94.5 | 434.8 | **4.60x** | 0.189 |
| 1000 | 148.8 | 578.3 | **3.89x** | 0.149 |

Möbius is 3-4x faster than Newton at batch because it needs zero iterations vs 3-4 Newton iterations.

### Table 3: Hop Count Scaling (1000 paths)

| Hops | Serial (μs) | Vectorized (μs) | Speedup | Per-Path (μs) |
|------|------------|-----------------|---------|----------------|
| 2 | 3117.3 | 145.0 | **21.50x** | 0.145 |
| 3 | 4174.6 | 187.1 | **22.31x** | 0.187 |
| 5 | 5908.1 | 290.3 | **20.35x** | 0.290 |
| 10 | 10256.2 | 533.1 | **19.24x** | 0.533 |
| 20 | 19153.9 | 1096.0 | **17.48x** | 1.096 |

### Table 4: Accuracy (Vectorized vs Serial)

| Hops | Max Input Rel Diff | Max Profit Rel Diff | Profitable Agree |
|------|-------------------|--------------------|-----------------|
| 2 | 0.00e+00 | 7.20e-14 | 100.0% |
| 3 | 0.00e+00 | 1.06e-13 | 100.0% |
| 5 | 0.00e+00 | 2.36e-13 | 100.0% |
| 10 | 0.00e+00 | 1.07e-12 | 100.0% |

Exact input match (0 relative diff), profit differences at float64 precision (~1e-13).

### Table 5: Log-Domain Overflow (20-hop, 1e18 reserves)

| Method | x_opt | profit | Profitable |
|--------|-------|--------|------------|
| Vectorized (log-domain) | 3.37e+15 | 2.43e+14 | True |
| Scaled reference (1e-6) | 3.37e+15 | 2.43e+14 | True |
| Scalar mobius_solve | 0.0 | 0.0 | False (overflows) |
| Relative difference | 3.01e-12 | 6.40e-12 | — |

The log-domain vectorized solver correctly handles cases where the scalar solver overflows to zero.

## Key Findings

1. **Zero-iteration advantage**: Möbius batch is 3-4x faster than Newton batch because Newton requires 3-4 iterations of vectorized compute per path, while Möbius needs one forward pass + one sqrt.

2. **Crossover at ~20 paths**: Below 20 paths, serial is faster (avoids NumPy array construction overhead). Above 20, vectorized dominates.

3. **Log-domain overflow handling**: The log-sum-exp trick for N computation and the simplified profit formula `x_opt * expm1(half_diff)` make the solver work correctly for paths where K, M, and N all overflow float64.

4. **Input array safety**: A subtle numpy view bug caused the input array to be silently mutated. Fix: copy all extracted slices.

5. **Profitability filter is free**: `K > M` (or `log_K > log_M`) requires no additional computation — the coefficients are already available from the recurrence. This eliminates unprofitable paths without any simulation.

## Comparison with Other Batch Optimizers

| Optimizer | Iterations | 1000 paths (μs) | Per-Path (μs) | Overflow Handling |
|-----------|-----------|-----------------|---------------|-----------------|
| Batch Newton | 3-4 | ~578 | ~0.58 | None |
| **Batch Möbius** | **0** | **~149** | **~0.15** | **Log-domain** |
| Serial Möbius | 0 | ~4454 | ~4.45 | N/A |
| Serial Newton | 3-4 | ~7767 | ~7.77 | N/A |

## Usage

```python
from degenbot.arbitrage.optimizers.batch_mobius import (
    BatchMobiusOptimizer,
    BatchMobiusPathInput,
    VectorizedMobiusSolver,
)
from degenbot.arbitrage.optimizers.mobius import HopState

# High-level API (auto-groups by hop count)
optimizer = BatchMobiusOptimizer(min_paths_for_batch=20)
paths = [
    BatchMobiusPathInput(hops=[HopState(...), HopState(...)]),
    BatchMobiusPathInput(hops=[HopState(...), HopState(...), HopState(...)]),
]
results = optimizer.solve_batch(paths)
best_idx, best = optimizer.get_best_path(paths)

# Low-level API (all paths same hop count)
solver = VectorizedMobiusSolver()
hops_array = np.array([...], dtype=np.float64)  # shape (P, H, 3)
max_inputs = np.full(P, np.inf)
result = solver.solve(hops_array, max_inputs)
print(f"Best path: {result.best_path_index()}")
print(f"Top 10: {result.top_paths(n=10)}")
```
