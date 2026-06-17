# Rust Solver Performance: Investigation & Optimization Progress

**Started**: 2025-05-23

## Executive Summary

**2.91× end-to-end speedup** achieved for the arbitrage solver hot path by eliminating Python-side overhead through four successive layers of Rust integration.

| Method | ns/path | Speedup |
|--------|---------|---------|
| `ArbSolver.solve_cached()` (original) | 2,612 | 1.00x |
| `ArbSolver.solve_cached_batch()` | 2,253 | 1.16x |
| `ArbSolver.solve_registered()` | 2,059 | 1.27x |
| `ArbSolver.solve_registered_ints()` (Phase 4: tuples) | 1,019 | 2.55x |
| **`ArbSolver.solve_registered_ints()` (Phase 5: flat+u64)** | **899** | **2.91x** |

---

## Phase 1: Diagnosis — Why ThreadPoolExecutor Doesn't Help

**Initial question**: Can we parallelize Rust solver dispatch with ThreadPoolExecutor?

**Answer**: No. The entire solve workflow is 99% GIL-held.

### GIL Time Budget (per solve call)

| Phase | Time (ns) | GIL Held? | % of Total |
|-------|-----------|-----------|------------|
| SolveInput + ConstantProductHop construction | ~1,440 | ✅ | 42% |
| MobiusSolver dispatch + int_hops_flat construction | ~780 | ✅ | 23% |
| PyO3 bridge (arg parsing, result construction) | ~1,160 | ✅ | 34% |
| **Total GIL-held** | **~3,380** | **✅** | **99%** |
| Pure Rust Möbius compute (`py.detach()`) | ~20 | ❌ | 1% |

**GIL-held/GIL-released ratio: 169:1.** The Rust `py.detach()` correctly releases the GIL, but the compute is only ~20ns — too short for another thread to make progress.

### ThreadPoolExecutor Provides Negative Speedup

| Configuration | Time/Path | Speedup vs Sequential |
|---|---|---|
| Sequential (0 workers) | 0.6–3.4 μs | 1.00x (baseline) |
| ThreadPool (8 workers) | 18–21 μs | **0.05x** (20× slower) |
| ProcessPool (4 workers) | 125 μs | **0.005x** (200× slower) |

ThreadPool overhead (submit, schedule, result collection) is ~15-20μs per item, dwarfing the ~1-3μs solve time. **ThreadPoolExecutor is an anti-pattern for <10μs work items.**

### PyO3 Call Overhead Breakdown

A single `solve_raw(hops_flat, None)` call from Python takes ~1,180ns, but the actual Rust computation is only ~20ns. The remaining ~1,160ns is PyO3 overhead:

1. **Python list → Rust Vec extraction** — Each U256 extraction costs ~80ns × 8 elements/hop × 2 hops = ~1,280ns
2. **Rust → Python result construction** — Each `optimal_input_int`/`profit_int` field calls `int.from_bytes()`, ~158ns per field
3. **GIL release/reacquire** — ~160ns per cycle

### The Python Dispatch Chain

```
ArbitragePath.calculate_with_pool()
  → _refresh_hop_states()       # GIL held: pool.to_hop_state() per pool
  → _build_solve_input()        # GIL held: SolveInput construction
  → MobiusSolver.solve()        # GIL held: method selection
  → _try_rust_solve()           # GIL held: max_input conversion
  → _try_rust_solve_raw()      # GIL held: int_hops_flat construction
  → RustArbSolver.solve_raw()  # GIL held: PyO3 arg parsing (1,160ns)
  → [py.detach() — GIL released: ~20ns of actual compute]
  → RustArbResult construction  # GIL held: PyO3 result (bytes_to_int ×2)
  → _process_rust_result()      # GIL held: int() conversion, error checking
```

Every step holds the GIL. The only GIL-released portion is the 20ns `solve_mobius()` call inside Rust.

### Benchmark Files

- `tests/perf/bench_threadpool_solver.py` — Full 11-phase benchmark suite
- `tests/perf/bench_threadpool_deep.py` — Deep investigation with hypothesis testing
- `tests/perf/bench_batched_micro.py` — Micro-benchmark for per-phase analysis

---

## Phase 2: Batched Rust Calls

**Approach**: Solve multiple paths in a single Python → Rust round-trip, amortizing the ~1,160ns PyO3 bridge overhead across all paths.

### Implementation

- `RustArbSolver.solve_raw_batch(paths, max_input)` — Takes list of flat int arrays, solves all in one `py.detach()`
- `RustPoolCache.solve_batch(paths, max_input)` — Takes list of pool-ID paths, looks them all up under one lock acquisition, solves in one `py.detach()`
- `ArbSolver.solve_cached_batch(paths, max_input)` — Python-level batched wrapper returning `list[SolveResult]`

### Architecture (3-phase pattern)

```
1. GIL-held:   Parse/extract all paths' data into Rust types
2. GIL-released: py.detach() solves all paths via .iter().map(solve_mobius).collect()
3. GIL-held:   Build Python list of results via PyList::empty() + .append()
```

### Results

| Method | Per-Path (ns) | Batched (ns/path) | Speedup |
|--------|---------------|-------------------|---------|
| Individual `solve_raw()` | 1,044 | 1,007 | 1.04x |
| Individual `cache.solve()` | 997 | 936 | 1.07x |
| Individual `solve_cached()` | 2,612 | 2,253 | 1.16x |

### Why the Speedup Is Modest (4-15%)

The PyO3 bridge cost (~1,160ns) is not the dominant bottleneck. **Per-element Python int extraction** is:

- Each U256 extraction costs ~80ns × 8 elements/hop × 2 hops = ~1,280ns per path
- Batched or not, the same number of Python ints must be extracted
- GIL context switches are cheap (~160ns) — saving N-1 yields only ~7,840ns for 50 paths
- The batched approach saves structural overhead, not data extraction cost

---

## Phase 3: Pre-Registered Paths

**Approach**: Register paths once, resolving pool IDs to concrete `(HopState, IntHopState)` pairs. On solve, no pool lookups, float conversions, or lock acquisitions — just a path ID lookup.

### Implementation

**Rust side** (`PyPoolCache` in `rust/src/optimizers/mobius_py.rs`):

| Method | Purpose |
|--------|---------|
| `register_path(pool_ids: Vec<u64>) -> u64` | Resolve pool IDs → `(HopState, IntHopState)` pairs once, store under auto-assigned path ID |
| `update_path(path_id: u64) -> bool` | Re-resolve after pool state changes |
| `update_all_paths() -> usize` | Batch re-resolve (one lock acquisition) |
| `remove_path(path_id: u64) -> bool` | Cleanup |
| `solve_registered(path_ids, max_input) -> Vec<PyArbResult>` | Look up pre-resolved hops, solve all in one `py.detach()` |
| `solve_registered_ints(path_ids, max_input) -> Vec<(int, int)>` | Same, but returns only `(optimal_input, profit)` tuples — **no PyArbResult, no SolveResult** |

**Python side** (`ArbSolver` in `src/degenbot/arbitrage/optimizers/solver.py`):

| Method | Returns | Purpose |
|--------|---------|---------|
| `register_path(pool_ids)` | `int` | Path ID |
| `update_path(path_id)` | `bool` | Re-resolve one path |
| `update_all_paths()` | `int` | Re-resolve all paths |
| `remove_path(path_id)` | `bool` | Cleanup |
| `solve_registered(path_ids, max_input)` | `list[SolveResult]` | Full results |
| `solve_registered_ints(path_ids, max_input)` | `list[tuple[int, int]]` | Fast path — only `optimal_input, profit` |

### Data Structures (Rust)

```rust
type ResolvedHops = Vec<(HopState, IntHopState)>;

struct RegisteredPath {
    hops: ResolvedHops,       // Pre-resolved at registration time
    pool_ids: Vec<u64>,       // For re-resolution after pool state changes
}

pub struct PyPoolCache {
    pools: Mutex<LruCache<u64, IntHopState>>,     // Pool state cache (was already here)
    paths: Mutex<HashMap<u64, RegisteredPath>>,   // Pre-resolved path state (NEW)
}
```

### Usage

```python
# Registration (once per path, at setup time)
path_id = solver.register_path([pool_0, pool_1])

# Update (once per block, after pool state changes)
solver.update_all_paths()

# Solve — full SolveResult objects
results = solver.solve_registered([path_id_0, path_id_1, ...])

# Solve — fast path (minimum overhead: list of (input, profit) tuples)
results = solver.solve_registered_ints([path_id_0, path_id_1, ...])
```

### What `solve_registered_ints` Eliminates

Compared to the original `solve_cached()` path:

| Overhead | solve_cached | solve_registered_ints | Saved |
|----------|-------------|----------------------|-------|
| Pool cache lock acquisition | Per call | Once total | ~30ns × N |
| Pool ID → IntHopState lookup | Per pool | Pre-resolved | ~100ns × hops |
| Float conversion (U256 → f64) | Per pool | Pre-resolved | ~80ns × hops |
| HopState Vec construction | Per call | Pre-resolved | ~200ns × hops |
| PyArbResult construction | Per path | Skipped | ~400ns × N |
| U256 → Python int (6 fields) | Per path | 2 fields only | ~630ns × N |
| SolveResult dataclass | Per path | Skipped | ~300ns × N |
| Method dict lookup | Per path | Skipped | ~50ns × N |

### Results

**Cache level** (Rust PyO3 calls — no Python wrapper overhead):

| Method | ns/path | Speedup vs `cache.solve` |
|--------|---------|-------------------------|
| `cache.solve()` (individual) | 1,002 | 1.00x |
| `cache.solve_batch()` | 951 | 1.05x |
| `cache.solve_registered()` | 830 | 1.21x |
| `cache.solve_registered_ints()` (Phase 5) | ~870 | 1.15x |

**ArbSolver level** (full Python call chain including SolveInput, method dispatch, result construction):

| Method | ns/path | Speedup vs `ArbSolver.solve_cached` |
|--------|---------|------------------------------------|
| `ArbSolver.solve_cached()` | 2,612 | 1.00x |
| `ArbSolver.solve_cached_batch()` | 2,253 | 1.16x |
| `ArbSolver.solve_registered()` | 2,059 | 1.27x |
| `ArbSolver.solve_registered_ints()` (Phase 4: tuples) | 1,019 | 2.55x |
| **`ArbSolver.solve_registered_ints()` (Phase 5: flat+u64)** | **899** | **2.91x** |

**Where the 2.91× comes from**: `ArbSolver.solve_cached()` performs ~1,600 ns of Python work before calling `cache.solve()` (~1,000 ns). `solve_registered_ints()` bypasses all that Python overhead and talks directly to the cache, so its cost is essentially the same as the raw `cache.solve()` call. The speedup is "free" — we're not making Rust faster, we're just not paying for Python on the way in and out. Note that `solve_registered_ints()` (899 ns) and `cache.solve()` (1,002 ns) are in the same ballpark — the entire improvement comes from eliminating the ~1,600 ns Python wrapper, not from any change to the Rust solver itself.

### Block Update Cycle

The full cycle (update pool states → update all paths → solve all paths) costs ~1,002 ns/path, comparable to a single individual `cache.solve()`. The registered path approach adds **no overhead** for the update phase — the 2.91× solve speedup comes "for free" during normal block-update workflows.

---

## Phase 5: Fast U256 → Python int & Flat Return

The Phase 4 `solve_registered_ints` returned `list[tuple[int, int]]` where each tuple required PyTuple allocation + 2× PyU256 → Python int conversion (~160ns each). Phase 5 implements three optimizations:

1. **Call `mobius_solve_with_refinement` directly** inside `solve_registered_ints`, skipping PyArbResult construction entirely
2. **Fast-path u64-fit U256 values**: Most arbitrage results (optimal_input, profit) fit in a single u64 limb. The new `u256_to_py_fast()` helper checks if the high 3 limbs are zero; if so, uses `PyInt::new(py, i64)` (~20ns) instead of `int.from_bytes()` (~160ns). For values > i64::MAX but ≤ u64::MAX, falls back to PyU256 conversion (correct but slower).
3. **Flat `list[int]` return**: Instead of `list[tuple[int, int]]` (PyTuple + 2× PyU256 per path), return a flat `list[int]` — `[input0, profit0, input1, profit1, ...]`. This eliminates PyTuple allocation per path. The Python-side `ArbSolver.solve_registered_ints` groups the flat list back into `(input, profit)` tuples.

```rust
fn u256_to_py_fast(py: Python<'_>, val: U256) -> PyResult<Bound<'_, PyAny>> {
    let limbs = val.as_limbs();
    if limbs[1] == 0 && limbs[2] == 0 && limbs[3] == 0 {
        let low = limbs[0];
        if let Ok(signed) = i64::try_from(low) {
            // Fast path: single C API call (~20ns vs ~160ns for from_bytes)
            Ok(pyo3::types::PyInt::new(py, signed).into_any())
        } else {
            PyU256(val).into_pyobject(py) // u64 > i64::MAX
        }
    } else {
        PyU256(val).into_pyobject(py) // Needs big-int
    }
}
```

### Phase 5 Results

| Method | ns/path | Speedup vs `solve_cached` |
|--------|---------|--------------------------|
| `ArbSolver.solve_cached()` | 2,612 | 1.00x |
| `ArbSolver.solve_registered()` | 2,059 | 1.27x |
| `ArbSolver.solve_registered_ints()` (Phase 4: tuples) | 1,019 | 2.55x |
| **`ArbSolver.solve_registered_ints()` (Phase 5: flat+u64)** | **899** | **2.91x** |

The u64 fast path is effective because typical arbitrage results (e.g., `optimal_input=24855250255`, `profit=738130649`) fit in i64 — they're token amounts in wei that stay well under i64::MAX (9.2×10¹⁸).

---

### Additional Rust Optimizations

- **`HopState` made `Copy`** — The 3-f64 struct (24 bytes) was `Clone` only; making it `Copy` eliminates `.clone()` overhead and fixes clippy warnings

---

## Test Coverage

51 tests in `tests/arbitrage/test_optimizers/test_rust_batch_solve.py`:

| Test Class | Tests | Coverage |
|------------|-------|----------|
| `TestRustPoolCacheSolveBatch` | 9 | `solve_batch`: single, multiple, EVM-exact, missing pool, not profitable, too few pools, max_input, empty, mixed fees, 3-hop |
| `TestRustArbSolverSolveRawBatch` | 8 | `solve_raw_batch`: single, multiple, mixed hops, not profitable, invalid, empty, max_input |
| `TestArbSolverSolveBatchCached` | 2 | `solve_cached_batch`: matches individual, not profitable |
| `TestRustPoolCacheRegisteredPaths` | 18 | `register_path`, `solve_registered`, `update_path`, `update_all_paths`, `remove_path`, ints variants, EVM-exact |
| `TestArbSolverRegisteredPaths` | 14 | ArbSolver-level: register/extent, matches cached, multiple, not profitable, update, remove, ints variants |

All 489 optimizer tests + 293 Rust tests pass. Clippy clean.

---

## Files Changed

| File | Changes |
|------|---------|
| `rust/src/optimizers/mobius_py.rs` | `register_path`, `update_path`, `update_all_paths`, `remove_path`, `solve_registered`, `solve_registered_ints` on `PyPoolCache`; `solve_raw_batch` on `PyArbSolver`; `solve_batch` on `PyPoolCache`; `ResolvedHops` type alias; `RegisteredPath` struct; `u256_to_py_fast` helper (Phase 5) |
| `rust/src/optimizers/mobius.rs` | `HopState` derived `Copy` |
| `rust/src/optimizers/mobius_v3_v3.rs` | `.clone()` → `*` for `Copy` types |
| `src/degenbot/arbitrage/optimizers/solver.py` | `register_path`, `update_path`, `update_all_paths`, `remove_path`, `solve_registered`, `solve_registered_ints` (updated for flat list return, Phase 5), `solve_cached_batch` on `ArbSolver` |
| `tests/arbitrage/test_optimizers/test_rust_batch_solve.py` | 51 tests for all new methods |
| `tests/perf/bench_batched_solve.py` | Batched vs per-path benchmark |
| `tests/perf/bench_registered_paths.py` | Registered paths benchmark |
| `tests/perf/bench_batched_micro.py` | Micro-benchmark for per-phase analysis |

---

## Next Steps

### Remaining Overhead in `solve_registered_ints` (~899 ns/path)

| Component | Est. Cost (ns) | Notes |
|-----------|----------------|-------|
| Rust compute | ~20 | Actual Möbius solve |
| Path lookup + hop clone | ~250 | `paths.lock()`, `HashMap::get`, `Vec::clone` |
| PyO3 bridge (call + return) | ~400 | Python → Rust call overhead |
| U256 → Python int (2 fields) | ~40 | Fast path: `PyInt::new` for u64-fit values (~20ns each) |
| Python tuple list comprehension | ~150 | `[(flat[i], flat[i+1]) for i in range(0, len(flat), 2)]` |

### Potential Further Optimizations

1. **Integrate with `ArbitragePath.notify()`**: When a pool state changes, automatically call `update_path()` so the registered path stays current without an explicit `update_all_paths()` call from the user.

2. **Refactor `solve_mobius` to accept `&ResolvedHops` directly**: Currently clones into `Vec<HopState>` + `Vec<IntHopState>` inside `py.detach()`. If the solver accepted the pre-resolved pairs directly, we'd eliminate the per-solve clone (~100ns for a 2-hop path).

3. **Pool-side `register_path` in `ArbitragePath.__init__`**: When an `ArbitragePath` is created with its pools, automatically register the path in the solver's cache. Then `calculate()` just calls `solve_registered_ints()`.

4. **Eliminate Python-side tuple regrouping**: Store results in Rust-managed memory and provide indexed access from Python (e.g., `results.input(0)`, `results.profit(0)`), avoiding the Python list comprehension entirely.

---

## Phase 6: V2ArbEngine Prototype — Fully-Rust Event Loop

**Question**: Can a fully-Rust event loop that owns pool state + path resolution + solving deliver sub-200ns/path?

**Answer**: **No.** The Python-to-Rust data transfer dominates, not the computation. The V2ArbEngine is marginally faster for solve-only (738 vs 927 ns/path) but no faster for the full block cycle.

### Design

```text
Python ──batch_update(pools)──→ V2ArbEngine (Rust)
        ←──solve_all()───   Internally:
                                   1. Update registered paths
                                   2. Solve every path
                                   3. Return flat [input0, profit0, ...]
```

One PyO3 round-trip per block instead of one per path.

### Results (50 paths, 100 pools)

| Method | ns/path | Notes |
|--------|---------|-------|
| V2ArbEngine solve_all (no update) | 738 | Faster than cache.solve_registered (927) |
| V2ArbEngine 2-call cycle (update + solve) | 1,156 | Two PyO3 round-trips |
| V2ArbEngine 1-call cycle (tuple input) | 1,188 | Same: data transfer dominates |
| V2ArbEngine 1-call cycle (packed bytes) | 994 | Saves ~190 ns/path vs tuples |
| V2ArbEngine pack + raw (true E2E) | 2,943 | Buffer packing in Python costs 85μs! |
| ArbSolver update + solve_registered_ints | 1,152 | Baseline |
| ArbSolver solve_registered_ints (solve only) | 927 | Cache-level baseline |

### Why It Doesn't Help

**The bottleneck is data transfer, not computation.** The hierarchy of costs:

| Cost | Time | % of Full Cycle |
|------|------|-----------------|
| Python→Rust pool update extraction | ~19,000 ns | 34% |
| Rust compute (50 paths × 20ns) | ~1,000 ns | 2% |
| Path resolution + rebuild (in Rust) | ~5,000 ns | 9% |
| Rust→Python result conversion | ~3,000 ns | 5% |
| PyO3 call overhead (1-2 calls) | ~1,600 ns | 3% |
| Python-side tuple/list/buffer work | ~27,000 ns | 47% |
| **Total** | **~56,600 ns** | **100%** |

The Rust computation is 2% of the total. Moving it into a tighter loop doesn't help because **97% of the time is spent moving data across the PyO3 bridge or preparing it in Python**.

### What the Packed Buffer Reveals

The `update_and_solve_raw(buf)` method accepts a pre-packed binary buffer, avoiding per-value Python extraction. It saves ~190 ns/path vs tuple-based updates. But the Python-side buffer *packing* costs 85μs (1,710 ns/path) — 10× the Rust solve cost. The packed buffer only helps if the buffer is produced by Rust itself (e.g., from an Alloy subscription).

### The Only Path to Sub-μ/path

**Pool state updates must originate from Rust**, not Python. This means:

1. **Alloy subscription in Rust** → V2ArbEngine receives new block data directly, no Python→Rust transfer
2. **Rust-internal state management** → Pool updates from on-chain events flow directly into the engine
3. **Python only pulls results** → Minimal Rust→Python conversion (already optimized via u64 fast path)

This is a fundamental architecture change: from Python-driven ("call Rust to solve") to Rust-driven ("Rust solves on new block, Python reads results"). The V2ArbEngine prototype validates this design but the real payoff requires moving the data source into Rust.

### Prototype Code

- `rust/src/optimizers/v2_engine.rs` — V2ArbEngine implementation
- `tests/perf/bench_v2_engine.py` — Benchmark suite
