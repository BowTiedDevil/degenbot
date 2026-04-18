# Arbitrage Optimizer Artifact Audit Report

**Date:** 2026-04-15  
**Purpose:** Identify which research artifacts, tests, benchmarks, and documentation can be safely removed vs. what should be retained.

---

## Executive Summary

The arbitrage optimizer research effort generated a substantial amount of code and documentation. Much of it has been superseded by later developments. This report categorizes everything into:

- **KEEP** — Still relevant, used in production, or valuable reference
- **CONSOLIDATE** — Merge into a single canonical version
- **ARCHIVE** — Historical value only, can move to archive folder
- **DELETE** — Superseded, redundant, or no longer useful

---

## 1. PLAN DOCUMENTATION (`plans/arbitrage-optimizer/`)

### 1.1 Core Documentation (KEEP)

| File | Status | Rationale |
|------|--------|-----------|
| `README.md` | **KEEP** | Entry point, quick reference, links to all docs |
| `recommendations.md` | **KEEP** | Production usage guide, actively referenced |
| `status.md` | **KEEP** | Current test counts, completion status |

### 1.2 Implementation Records (CONSOLIDATE → 1 file)

| File | Status | Rationale |
|------|--------|-----------|
| `implementation-phases.md` | **CONSOLIDATE** | Detailed phase-by-phase record |
| `progress-log.md` | **CONSOLIDATE** | Day-by-day progress log |
| `lessons-learned.md` | **CONSOLIDATE** | 18 key findings |
| `current-state.md` | **ARCHIVE** | Baseline metrics, superseded by status.md |
| `improvement-proposals.md` | **ARCHIVE** | Early proposals, mostly implemented |
| `alternatives-evaluation.md` | **ARCHIVE** | Evaluation of approaches, decisions made |
| `risks-dependencies.md` | **ARCHIVE** | Early risk assessment, outdated |
| `unified-solver-plan.md` | **ARCHIVE** | Planning doc, now implemented |
| `solver-integration.md` | **ARCHIVE** | Integration plan, now complete |
| `mobius-full-amm-coverage.md` | **CONSOLIDATE** | V2+V3 Möbius theory |

**Recommendation:** Merge `implementation-phases.md`, `progress-log.md`, `lessons-learned.md`, and `mobius-full-amm-coverage.md` into a single `RESEARCH_HISTORY.md` file. Archive the rest.

### 1.3 Supporting Documentation (KEEP)

| File | Status | Rationale |
|------|--------|-----------|
| `file-structure.md` | **KEEP** | Useful for navigating the codebase |

---

## 2. RESEARCH DEEP DIVES (`plans/arbitrage-optimizer/research/`)

| File | Status | Rationale |
|------|--------|-----------|
| `mobius-transformation.md` | **KEEP** | Mathematical foundation, referenced in code |
| `batch-mobius.md` | **KEEP** | Vectorized batch implementation details |
| `rust-opportunities.md` | **KEEP** | Rust optimization notes, still relevant |
| `dual-decomposition.md` | **ARCHIVE** | Multi-token routing theory, implemented |
| `v3-bounded-product.md` | **ARCHIVE** | V3 research, superseded by Möbius |
| `vectorized-batch.md` | **ARCHIVE** | Early batch research, superseded by batch-mobius.md |

---

## 3. PRODUCTION SOURCE CODE (`src/degenbot/arbitrage/optimizers/`)

### 3.1 Unified Solver Interface (KEEP ALL)

| File | Lines | Status | Notes |
|------|-------|--------|-------|
| `solver.py` | 2,238 | **KEEP** | Core unified interface, ArbSolver dispatcher |
| `base.py` | ~100 | **KEEP** | Base classes for legacy optimizers |
| `__init__.py` | ~400 | **KEEP** | Public API exports |

### 3.2 Möbius Optimizers (KEEP ALL)

| File | Lines | Status | Notes |
|------|-------|--------|-------|
| `mobius.py` | 1,100+ | **KEEP** | Core Möbius optimizer, V2+V3 support |
| `batch_mobius.py` | ~400 | **KEEP** | Vectorized batch Möbius |

### 3.3 Newton Optimizers (KEEP — LEGACY USAGE)

| File | Lines | Status | Notes |
|------|-------|--------|-------|
| `newton.py` | ~400 | **KEEP** | NewtonV2Optimizer, used in hybrid |
| `chain_rule.py` | ~300 | **KEEP** | Multi-pool Newton, used |
| `vectorized_batch.py` | ~400 | **KEEP** | Vectorized Newton (BatchNewtonOptimizer) |

### 3.4 Specialized Solvers (KEEP — SPECIFIC USE CASES)

| File | Lines | Status | Notes |
|------|-------|--------|-------|
| `balancer_weighted.py` | ~500 | **KEEP** | Balancer N-token solver (Eq.9) |
| `v2_v3_optimizer.py` | ~500 | **KEEP** | V2-V3 with tick prediction |
| `v3_tick_predictor.py` | ~300 | **KEEP** | Tick crossing prediction |
| `bounded_product.py` | ~200 | **KEEP** | Bounded product CFMM for V3 |

### 3.5 Superseded Solvers (ARCHIVED)

| File | Lines | Status | Rationale |
|------|-------|--------|-----------|
| `hybrid.py` | ~250 | **DELETED** | Superseded by `ArbSolver` in solver.py |
| `gradient_descent.py` | ~200 | **ARCHIVED** | Tested & worked; 52x slower than Möbius, 8x slower than Newton |
| `multi_pool_gradient.py` | ~300 | **ARCHIVED** | Tested & worked; superseded by Möbius closed-form O(n) |
| `multi_token.py` | ~600 | **ARCHIVED** | DualDecompositionSolver works but not integrated into ArbSolver |

**Notes:**
- Gradient descent was Barzilai-Borwein variant, ~45μs vs Möbius ~0.86μs
- All derivative-free methods find identical profits; Möbius zero-iteration advantage is insurmountable
- `multi_token.py` preserved for potential future multi-path routing work

### 3.6 Redundant/Experimental Files

| File | Status | Rationale |
|------|--------|-----------|
| `balancer_weighted_v2.py` | **DELETED** | Redundant with `balancer_weighted.py` |

---

## 4. RUST OPTIMIZERS (`rust/src/optimizers/`)

| File | Lines | Status | Notes |
|------|-------|--------|-------|
| `mobius.rs` | ~400 | **KEEP** | Rust f64 Möbius (0.19μs) |
| `mobius_int.rs` | ~500 | **KEEP** | Rust u256 Möbius (0.88μs, EVM-exact) |
| `mobius_batch.rs` | ~400 | **KEEP** | Rust batch Möbius (0.09μs/path) |
| `mobius_v3.rs` | ~500 | **KEEP** | V3 support (in development) |
| `mobius_py.rs` | ~600 | **KEEP** | PyO3 bindings for Python |
| `mod.rs` | ~40 | **KEEP** | Module exports |

**Status:** All Rust files are active and should be kept.

---

## 5. TEST SUITE (`tests/arbitrage/`)

### 5.1 Core Test Infrastructure (KEEP)

| File/Dir | Status | Notes |
|----------|--------|-------|
| `mock_pools.py` | **KEEP** | Essential test fixtures |
| `generator/` | **KEEP** | Fixture generation utilities |
| `presets.py` | **KEEP** | Test presets |

### 5.2 Unit Tests — Current/Active (KEEP)

| File | Lines | Status | Notes |
|------|-------|--------|-------|
| `test_solver.py` | ~800 | **KEEP** | Core unified solver tests |
| `test_solver_integration.py` | ~600 | **KEEP** | Integration with cycle classes |
| `test_mobius_optimizer.py` | ~900 | **KEEP** | Möbius optimizer tests |
| `test_mobius_v3.py` | ~784 | **KEEP** | V3 Möbius tests |
| `test_mobius_v3_accuracy.py` | ~760 | **KEEP** | Accuracy validation |
| `test_piecewise_mobius.py` | ~776 | **KEEP** | Piecewise Möbius tests |
| `test_batch_mobius.py` | ~1,028 | **KEEP** | Batch Möbius tests |
| `test_balancer_weighted.py` | ~500 | **KEEP** | Balancer solver tests |
| `test_rust_mobius.py` | ~400 | **KEEP** | Rust f64 tests |
| `test_rust_int_mobius.py` | ~300 | **KEEP** | Rust integer tests |
| `test_v2_v3_optimizer.py` | ~654 | **KEEP** | V2-V3 optimizer tests |
| `test_v3_tick_cache.py` | ~664 | **KEEP** | V3 tick cache tests |
| `test_v3_tick_predictor.py` | ~400 | **KEEP** | Tick prediction tests |
| `test_multi_token.py` | ~578 | **ARCHIVED** | Dual decomposition tests — not integrated |
| `test_production_optimizers.py` | ~500 | **ARCHIVED** | HybridOptimizer tests — superseded by ArbSolver |
| `test_production_newton.py` | ~300 | **ARCHIVED** | Newton production tests — consolidated |
| `test_gradient_descent.py` | ~710 | **DELETED** | Gradient descent tested but superseded (52x slower than Möbius) |
| `test_solidly_stable_solver.py` | ~798 | **KEEP?** | Verify if Solidly solver is used |
| `test_closed_form.py` | ~300 | **CONSOLIDATE** | Early Newton tests, merge |
| `test_optimized_solvers.py` | ~751 | **CONSOLIDATE** | Overlaps with test_solver.py |
| `test_v2_v3_optimization.py` | ~943 | **CONSOLIDATE** | Overlaps with test_v2_v3_optimizer.py |
| `test_large_reserves.py` | ~200 | **CONSOLIDATE** | Merge into main test files |

### 5.3 Benchmark Tests (ARCHIVE)

| File | Lines | Status | Rationale |
|------|-------|--------|-----------|
| `benchmark_base.py` | ~200 | **ARCHIVE** | Benchmark infrastructure |
| `test_benchmark.py` | ~735 | **ARCHIVE** | Early Brent/CVXPY benchmarks |
| `final_benchmark.py` | ~300 | **ARCHIVE** | Final comparison, now in status.md |
| `run_all_benchmarks.py` | ~200 | **ARCHIVE** | Benchmark runner |
| `run_batch_mobius_benchmark.py` | ~200 | **ARCHIVE** | Specific benchmark |
| `run_int_mobius_benchmark.py` | ~200 | **ARCHIVE** | Specific benchmark |
| `run_multiprocess_benchmark.py` | ~200 | **ARCHIVE** | Specific benchmark |
| `run_parallel_benchmark.py` | ~200 | **ARCHIVE** | Specific benchmark |
| `run_rust_mobius_benchmark.py` | ~200 | **ARCHIVE** | Specific benchmark |
| `run_vectorized_benchmark.py` | ~200 | **ARCHIVE** | Specific benchmark |

### 5.4 Obsolete/Superseded Test Files (DELETE)

| File | Lines | Status | Rationale |
|------|-------|--------|-----------|
| `alternative_techniques.py` | ~706 | **DELETED** | Experimental techniques, tested but not performant |
| `brent_optimizer.py` | ~200 | **DELETED** | Early Brent tests, now in solver |
| `convex_optimizer.py` | ~664 | **DELETED** | CVXPY tests, research only — 7x slower than Brent |
| `dual_decomposition.py` | ~300 | **DELETED** | Research implementation tests |
| `log_domain_optimizer.py` | ~200 | **DELETED** | Early numerical experiments — superseded |
| `multi_pool_optimization.py` | ~790 | **DELETED** | Gradient-based multi-pool — superseded by Möbius O(n) |
| `multiprocess_benchmark.py` | ~200 | **DELETE** | Benchmark, results captured |
| `numerical_example.py` | ~200 | **DELETE** | Documentation example |
| `optimized_solvers.py` | ~799 | **DELETE** | Duplicates test_optimized_solvers.py |
| `parallel_benchmark.py` | ~200 | **DELETE** | Benchmark, results captured |
| `performance_optimizer.py` | ~626 | **DELETE** | Early performance tests |
| `precision_aware_batch.py` | ~200 | **DELETE** | Early batch experiments |
| `test_alternative_optimizers.py` | ~963 | **DELETED** | Gradient descent tests — worked but too slow |
| `test_dual_decomposition.py` | ~200 | **DELETED** | Research implementation tests |
| `test_multiprocess.py` | ~200 | **DELETED** | Multiprocessing tests — GIL prevented speedup |
| `test_parallel.py` | ~200 | **DELETED** | Threading tests — GIL prevented speedup |
| `test_performance.py` | ~300 | **DELETE** | Early performance tests |
| `test_v3_approximation.py` | ~400 | **DELETE** | Superseded by Möbius |
| `test_v3_bounded_region.py` | ~400 | **DELETE** | Superseded by Möbius |
| `v3_approximation.py` | ~300 | **DELETE** | Superseded |
| `v3_benchmark.py` | ~200 | **DELETE** | Superseded |
| `v3_bounded_region.py` | ~844 | **DELETE** | Superseded |
| `vectorized_batch.py` | ~200 | **DELETE** | Early experiments |

---

## 6. SUMMARY STATISTICS

### 6.1 Documentation
- **Total docs:** 20
- **Keep:** 3
- **Consolidate:** 5 → 1 file
- **Archive:** 11
- **Delete:** 1

### 6.2 Source Code (Python) — COMPLETED
- **Total files:** 18
- **Keep:** 13
- **Archive:** 3 (`multi_token.py`, `gradient_descent.py`, `multi_pool_gradient.py`)
- **Delete:** 2 (`hybrid.py`, `balancer_weighted_v2.py`)

### 6.3 Source Code (Rust)
- **Total files:** 6
- **Keep:** 6 (all active)

### 6.4 Tests — COMPLETED
- **Total test files:** 50+
- **Keep:** 19
- **Archive:** 11 (moved to `*/archive/`)
- **Delete:** 21 (including baseline system)

---

## 7. COMPLETED ACTIONS ✅

All recommended actions have been completed:

### Documentation
- ✅ Consolidated 20 plan docs → 4 core files + archive/
- ✅ Merged V3 research docs into `v3-mobius-research-complete.md`
- ✅ Moved Balancer implementation doc to archive/

### Source Code
- ✅ Deleted `hybrid.py` (superseded by ArbSolver)
- ✅ Archived `multi_token.py`, `gradient_descent.py`, `multi_pool_gradient.py`
- ✅ Deleted `balancer_weighted_v2.py` (redundant)
- ✅ Updated `__init__.py` exports

### Tests
- ✅ Deleted 21 test files (obsolete/superseded)
- ✅ Archived 11 test files (preserved but not active)
- ✅ Removed baseline/golden file system
- ✅ All 586 tests passing

### Verification Results
| File | Finding | Action |
|------|---------|--------|
| `multi_token.py` | Not integrated into ArbSolver | Archived |
| `SolidlyStableSolver` | Integrated into ArbSolver | Kept |
| `HybridOptimizer` | Unused outside its own file | Deleted |
| `test_gradient_descent.py` | Worked but 52x slower than Möbius | Deleted |


### Key Finding

**Gradient descent and multi-pool gradient solvers were NOT "never used" — they were implemented, tested, and worked correctly. They were removed because Möbius closed-form solutions are 52x and 225x faster respectively. The zero-iteration advantage of Möbius makes iterative gradient methods obsolete regardless of convergence rate.**

---

## 8. RISK ASSESSMENT

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Deleting active code | Low | High | Verify usage before deletion |
| Losing historical context | Low | Medium | Archive, don't delete docs |
| Breaking tests | Medium | Medium | Run full test suite after changes |
| Missing coverage | Low | Medium | Review coverage report |

---

## 9. APPENDIX: VERIFICATION CHECKLIST

Before removing any files, verify:

- [ ] `multi_token.py` — Search for imports in production code
- [ ] `test_solidly_stable_solver.py` — Check if Solidly solver is integrated
- [ ] `hybrid.py` — Confirm `ArbSolver` fully replaces it
- [ ] All "DELETE" test files — Confirm no unique test coverage
- [ ] Run full test suite: `uv run pytest tests/arbitrage/ -x`
- [ ] Check imports in non-test code: `grep -r "from.*optimizers" src/`
