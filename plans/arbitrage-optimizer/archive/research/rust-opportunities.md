# Rust Implementation Opportunities

## Key Advantages of Rust

| Python Limitation | Rust Advantage |
|-------------------|----------------|
| GIL prevents thread parallelism | Native threads, no GIL |
| NumPy SIMD is opaque | Explicit SIMD via `portable_simd` |
| Float precision concerns | U256/U512 integer arithmetic |
| GC pauses | Deterministic memory |
| C extension overhead | Native performance |

## Expected Performance Improvements

| Optimizer | Python | Rust (Actual) | Speedup |
|-----------|--------|---------------|----------|
| V2 Möbius (f64) | 0.86μs | 0.19μs | **4.5x** |
| V2 Möbius (u256) | N/A | 0.88μs | EVM-exact |
| Batch Möbius (1000 paths) | 140μs (0.14μs/path) | 93μs (0.09μs/path) | **1.5x** |
| V2 Newton | 7.5μs | N/A (superseded by Möbius) | — |
| Vectorized Newton (1000 paths) | 528μs (0.53μs/path) | N/A (superseded by Möbius) | — |
| Not-profitable rejection | N/A | 0.32μs | Instant K>M check |

## 1. Newton's Method for V2-V2 (Target: <1μs)

Pure integer arithmetic matching EVM behavior exactly — no floating point.

```rust
pub struct V2NewtonOptimizer {
    fee_numer: u64,
    fee_denom: u64,
    max_iters: usize,
}

impl V2NewtonOptimizer {
    pub fn solve(
        &self,
        buy_r0: U256, buy_r1: U256,
        sell_r0: U256, sell_r1: U256,
    ) -> (U256, U256, U256) {
        let mut x = buy_r0 / U256::from(100);
        let gamma = U256::from(self.fee_numer);
        let gamma_denom = U256::from(self.fee_denom);

        for _ in 0..self.max_iters {
            // Newton iterations with scaled integer arithmetic
            // ...
        }

        (x, y, profit)
    }
}
```

## 2. Vectorized Batch Processing with SIMD (Target: <0.1μs/path)

```rust
use std::simd::*;

type F64x8 = Simd<f64, 8>;

impl VectorizedNewtonSolver {
    /// Solve 8 paths simultaneously using SIMD.
    pub fn solve_batch_8(&self, paths: &[PathState; 8]) -> [ArbitrageResult; 8] {
        let buy_r0 = F64x8::from_array([paths[0].buy_r0, ...]);
        // Vectorized Newton iterations
        // Convergence mask for early termination per lane
    }
}
```

## 3. True Parallelism with Rayon (No GIL!)

```rust
use rayon::prelude::*;

pub fn solve_parallel(paths: &[PathState]) -> Vec<ArbitrageResult> {
    paths.par_iter()
        .map(|path| V2NewtonOptimizer::new(path.fee).solve(...))
        .collect()
}

// With work-stealing and SIMD chunks
pub fn solve_parallel_chunked(paths: &[PathState]) -> Vec<ArbitrageResult> {
    paths.par_chunks(100)
        .flat_map(|chunk| {
            chunk.array_chunks::<8>()
                .flat_map(|arr| simd_solver.solve_batch_8(arr))
        })
        .collect()
}
```

Python multiprocessing: 0.41ms/problem (spawn overhead dominates).
Rust Rayon: ~10-50μs/problem (no spawn overhead, work-stealing).

## 4. V3 Tick Bitmap with Cache-Friendly Layout

```rust
pub struct V3TickCache {
    ranges: BTreeMap<i32, TickRangeInfo>,
    lut: Vec<TickRangeInfo>,         // O(1) for hot paths
    lut_spacing: i32,
    current_tick: i32,
    current_sqrt_price: U256,
}
```

## 5. Bounded Product CFMM for V3 (Closed-Form)

```rust
pub struct BoundedProductCFMM {
    alpha: U256,  // L / sqrt(P_upper)
    beta: U256,   // L * sqrt(P_lower)
    liquidity: U256,
    sqrt_price: U256,
}
```

## 6. Dual Decomposition with Parallel Markets

```rust
impl DualDecompositionSolver {
    pub fn solve(&self, initial_prices: Vec<f64>) -> Vec<f64> {
        let mut nu = initial_prices;
        for _ in 0..self.max_iterations {
            // Parallel market solving — NO GIL!
            let trades: Vec<_> = self.markets.par_iter()
                .map(|market| market.find_arbitrage(&nu))
                .collect();
            // Update shadow prices using L-BFGS-B step
        }
        nu
    }
}
```

## 7. V2-V3 Optimizer with Tick Prediction

```rust
impl V2V3Optimizer {
    pub fn optimize(&self) -> ArbitrageResult {
        let p_eq = self.estimate_equilibrium(p_v2, p_v3);
        let candidates = self.filter_tick_ranges(p_lower, p_upper);
        // Check top 3 candidates with bounded product CFMM
        // Fallback to Brent
    }
}
```

## 8. Async Pool State Updates

```rust
use tokio::sync::RwLock;

pub struct PoolStateManager {
    pools: HashMap<Address, Arc<RwLock<PoolState>>>,
}
```

## Recommended Crate Structure

```
src/
├── lib.rs
├── optimizers/
│   ├── mod.rs
│   ├── v2_newton.rs        # Integer Newton for V2
│   ├── v3_bounded.rs       # Bounded product CFMM
│   ├── v2_v3.rs            # V2-V3 hybrid
│   ├── vectorized.rs       # SIMD batch processing
│   ├── parallel.rs         # Rayon parallel solver
│   └── dual_decomposition.rs
├── pools/
│   ├── mod.rs
│   ├── v2.rs
│   ├── v3.rs
│   └── tick_cache.rs
├── types/
│   ├── mod.rs
│   ├── u256_ext.rs
│   └── results.rs
└── state/
    ├── mod.rs
    └── manager.rs
```

## Key Rust Libraries

| Purpose | Crate |
|---------|-------|
| Big integers | `primitive-types`, `ethers` |
| SIMD | `portable_simd` (nightly) or `packed_simd` |
| Parallelism | `rayon` |
| Async | `tokio` |
| Blockchain types | `ethers` or `alloy` |
| LRU cache | `lru` or `moka` |
| Numerics | `ndarray` |
| Python bindings | `pyo3` |

## Python Extension Integration (PyO3)

```rust
use pyo3::prelude::*;

#[pymodule]
fn degenbot_optimizers(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<V2NewtonOptimizer>()?;
    m.add_class::<VectorizedNewtonSolver>()?;
    m.add_class::<BoundedProductCFMM>()?;
    m.add_class::<V2V3Optimizer>()?;
    Ok(())
}
```

```python
from degenbot_optimizers import V2NewtonOptimizer, VectorizedNewtonSolver

# Single path (Python: 7.88μs → Rust: ~1μs)
optimizer = V2NewtonOptimizer(fee_numer=997, fee_denom=1000)
result = optimizer.solve(buy_r0, buy_r1, sell_r0, sell_r1)

# Batch paths (Python: 0.5μs/path → Rust: ~0.1μs/path)
batch_solver = VectorizedNewtonSolver(fee=0.003)
results = batch_solver.solve_batch(paths_data)
```

## Implementation Priority

| Priority | Component | Python Baseline | Rust Result | Effort | Status |
|----------|-----------|-----------------|-------------|--------|--------|
| 1 | V2 Möbius (f64) | 0.86μs | **0.19μs** | Medium | ✅ DONE |
| 2 | V2 Möbius (u256) | N/A | **0.88μs** | Medium | ✅ DONE |
| 3 | Batch Möbius | 0.14μs/path | **0.09μs/path** | High | ✅ DONE |
| 4 | V3 Möbius | ~5-25μs | In development | Medium | 🔄 In Progress |
| 5 | Parallel (Rayon) | N/A | N/A | Low | Pending |
| 6 | Tick Cache | 0.67μs | 0.05μs (target) | Low | Pending |
| 7 | Dual Decomposition | 5-12ms | 500μs-1ms (target) | High | Pending |
| 8 | V2-V3 Optimizer | 5-15ms | 1-3ms (target) | Medium | Pending |
| 9 | Async State Manager | N/A | N/A | Medium | Pending |
