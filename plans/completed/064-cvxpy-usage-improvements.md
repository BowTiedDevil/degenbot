# Plan 064: CVXPY Usage Improvements

## Overview

Improve the project's CVXPY code to follow current best practices — specifically DPP compliance and
canonicalization backend benchmarking. These changes affect the legacy cycle code (deprecated but
still importable, and used across pickle boundaries via multiprocessing) and the test comparison
oracle, which will persist beyond the legacy deletion.

## Problem

### Deletion test

If you deleted the CVXPY code in `_legacy/` and `test_solver_integration.py`, the legacy cycle
classes would lose their convex optimization path (they already fall back to
`scipy.optimize.minimize_scalar` / `ArbSolver` fast-path). The test suite would lose the
cvxpy-based comparison oracle that validates `ArbSolver` against a known-good convex solver. The
comparison oracle is the only part worth fixing — the legacy code is deprecated and will eventually
be deleted. Since `cvxpy` is an optional dependency (`degenbot[legacy-cycles]`), users who don't
install it are unaffected.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| 2-pool DPP assertion runs after warm-up `solve()`, not before | `_uniswap_2pool_cycle_testing.py:220-221` | A non-DPP problem would pass the warm-up solve (which runs before the assertion), populate internal state, and get pickled into worker processes — all before DPP compliance is verified. The assertion should catch violations *before* the warm-up solve prepares the object for pickling. |
| 2-pool runtime re-solve missing `enforce_dpp=True` | `_uniswap_2pool_cycle_testing.py:2394` | The pre-verified DPP problem is built at import time so it can be pickled and re-solved at runtime with updated parameters. But `enforce_dpp=True` is not passed to the re-solve call, so cvxpy re-canonicalizes from scratch every time — defeating the purpose of pre-building the problem. |
| String solver reference `"CLARABEL"` | `_uniswap_2pool_cycle_testing.py:220,2394,2413` | String form provides no IDE completion, no import-time typo detection, and breaks silently if solver names change between cvxpy versions. The multipool file and test code already use the `cvxpy.CLARABEL` constant. |
| No benchmark data for COO canonicalization backend | `_uniswap_multipool_cycle_testing.py` | CVXPY 1.9 added a COO canonicalization backend designed for DPP-compliant problems with large parameters. The multipool problem has N×N parameter matrices, but no data exists on whether COO outperforms the default CPP backend at the pool counts (2–5) used in practice. |
| Test code duplicates CVXPY problem construction | `test_solver_integration.py` | Three test methods (`test_cvxpy_known_value_wbtc_weth`, `test_cvxpy_finds_profit`, `test_cvxpy_no_profit_identical_pools`) each build a `Parameter` + `Variable` + `Problem` from scratch with nearly identical structure. A factory function would reduce duplication without introducing shared-state risks. |

## Solution

### Slice 1: Fix 2-pool DPP assertion, `enforce_dpp=True`, and solver constant

The 2-pool `_build_convex_problem` serves a specific lifecycle: it's called at import time to
pre-build and pre-verify a DPP-compliant `Problem` object, which is then pickled into worker
processes and re-solved at runtime with updated parameters. The warm-up `solve()` is intentional —
it pre-populates internal state before the pickle boundary. The multipool file already does
everything correctly (assertion before solve, `enforce_dpp=True`, `cvxpy.CLARABEL` constant).

Changes to `_uniswap_2pool_cycle_testing.py` only:

1. **Move DPP assertion before the warm-up solve** (lines 220–221):
   ```python
   # Before
   problem.solve(solver="CLARABEL")
   assert problem.is_dcp(dpp=True)

   # After
   assert problem.is_dcp(dpp=True)
   problem.solve(solver=cvxpy.CLARABEL)
   ```
   This catches non-DPP construction before the warm-up solve populates internal state.

2. **Add `enforce_dpp=True` to the runtime re-solve** (line 2394):
   ```python
   # Before
   problem.solve(solver="CLARABEL")

   # After
   problem.solve(solver=cvxpy.CLARABEL, enforce_dpp=True)
   ```
   Without this flag, the pre-verified DPP problem re-canonicalizes from scratch on every call,
   defeating the purpose of pre-building the problem for worker-process re-solve.

3. **Replace all `solver="CLARABEL"` with `solver=cvxpy.CLARABEL`** (lines 220, 2394, 2413).

### Slice 2: Benchmark COO canonicalization backend

CVXPY 1.9 added `canon_backend=cp.COO_CANON_BACKEND`, which uses O(nnz) 3D sparse tensors and is
recommended for DPP-compliant problems with large parameters. The multipool problem has N×N
parameter matrices, but N is typically 2–5 (4–25 elements). Whether COO outperforms the default
CPP backend at these sizes is unknown — benchmark data is needed.

Prerequisite: bump `cvxpy ~= 1.8` → `~= 1.9` in `pyproject.toml` (under the
`legacy-cycles` optional dependency group).

Actions:

1. Bump the cvxpy version pin.
2. Write a benchmark script that constructs the multipool problem for N=2,3,4,5 pools, then
   times **re-solve** (not first-solve) with both `canon_backend` values (default CPP vs
   `COO_CANON_BACKEND`). Re-solve is the path hit at runtime when the pre-built problem gets
   updated parameters.
3. Adopt COO in production code only if the benchmark shows meaningful improvement.
4. Record the benchmark results regardless of outcome — negative results (COO ≈ CPP at N≤5) are
   also valuable.

### Slice 3: Extract 2-pool CVXPY problem factory for test code

The three CVXPY test methods in `test_solver_integration.py` build structurally similar problems
from scratch each time. A factory function deduplicates construction logic without introducing
shared-mutable-state risks.

Key constraints:
- Each call returns a fresh `Problem` — no shared-instance fixture. Shared `Problem` instances
  carry stale `.value` on Variables from previous solves, creating test-isolation footguns.
- The factory must accept decimal counts as parameters because
  `test_cvxpy_known_value_wbtc_weth` uses (8, 18) while the other two use (18, 18).

Actions:

1. Extract `_build_2pool_cvxpy_problem(decimals0, decimals1)` in a test helper module.
2. `test_cvxpy_finds_profit` and `test_cvxpy_no_profit_identical_pools` call it as
   `_build_2pool_cvxpy_problem(18, 18)`.
3. `test_cvxpy_known_value_wbtc_weth` calls it as `_build_2pool_cvxpy_problem(8, 18)`.

### Design decisions

- **Keep the fee matrix as `(num_pools, num_tokens)` — do not collapse to scalar**: Camelot pools
  have genuinely asymmetric `fee_token0 ≠ fee_token1`. Different pools have different fee tiers.
  The zero entries in the fee matrix are load-bearing (they prevent fee deduction on non-swapped
  positions). A scalar `fee * deposits` would overdeduct fees and produce wrong results. The
  existing 2D fee Parameter is already DPP-friendly (affine Parameter used element-wise in
  `multiply`).
- **Keep `bmat`/`multiply` pattern — do not replace with `hstack`**: The `hstack` vs `bmat`
  difference is negligible for 3-variable problems (both create intermediate atoms). The
  `bmat` pattern is more readable — it preserves the clear "deposit into pool_hi, withdraw from
  pool_lo" semantics. The multipool file uses `hstack` for a different reason (variable-length
  token lists), not because it's a better pattern for the 2-pool case.
- **Don't restructure multipool `geo_mean(hstack([param_slice, ...]))`**: The `geo_mean` of
  parameter-affine slices computes `k_pre_swap` as `geo_mean(param_slice).value` at construction
  time. This is not truly DPP-compliant (the `k_pre_swap` Parameter value depends on a nonlinear
  function of another Parameter's slices), but restructuring would require a full rewrite. Since
  this code is deprecated, the effort isn't justified. The assertion passes because it checks
  DCP-under-DPP structural rules, and the warm-up solve validates that the problem works.
- **Don't add CVXPY 1.9 DNLP solver**: DNLP's IPOPT backend provides local optima only (no global
  guarantees), operates in float space, and would be ~1000× slower than the specialized solvers.
  Not suitable for MEV competition. See the appendix.
- **Keep `geo_mean` rather than replacing with `sqrt(x*y)`**: `geo_mean` is the idiomatic CVXPY
  way and maps directly to the constant-product invariant.

## Files Involved

**Primary:**
- `src/degenbot/arbitrage/_legacy/_uniswap_2pool_cycle_testing.py` — Slice 1
- `src/degenbot/arbitrage/_legacy/_uniswap_multipool_cycle_testing.py` — Slice 2 (benchmark target)
- `tests/arbitrage/test_optimizers/test_solver_integration.py` — Slice 3

**Secondary:**
- `pyproject.toml` — Slice 2 (cvxpy version bump)

**No change needed:**
- `src/degenbot/arbitrage/optimizers/` — The `ArbSolver` path doesn't use CVXPY at all.

## Implementation Order

### Slice 1: Fix 2-pool DPP assertion, `enforce_dpp=True`, and solver constant

1. In `_uniswap_2pool_cycle_testing.py`:
   - Move `assert problem.is_dcp(dpp=True)` before the warm-up `solve()` call (line 220→219)
   - Add `enforce_dpp=True` to the runtime re-solve call (line 2394)
   - Replace `solver="CLARABEL"` with `solver=cvxpy.CLARABEL` (lines 220, 2394, 2413)
2. Run: `just test-python` — expect all existing tests to pass (changes are structural, not
   behavioral)

### Slice 2: Benchmark COO canonicalization backend

1. Bump `cvxpy ~= 1.8` → `~= 1.9` in `pyproject.toml`
2. Write benchmark script comparing CPP vs COO re-solve time at N=2,3,4,5 pools
3. If COO is meaningfully faster, add `canon_backend=cvxpy.COO_CANON_BACKEND` to the multipool
   solve call; otherwise, record the negative result
4. Run: `just test-python` — expect all existing tests to pass

### Slice 3: Extract 2-pool CVXPY problem factory for test code

1. Extract `_build_2pool_cvxpy_problem(decimals0, decimals1)` in a test helper
2. Refactor the three test methods to call the factory
3. Run: `just test-python` — expect identical test results

## Testing

### Per-slice test runs

Each slice runs `just test-python`. If a migration requires a compatibility period, both old and
new paths must pass.

### New unit tests (Slice 1)

```python
# Verify the 2-pool problem is DPP-compliant at construction time
def test_2pool_dpp_compliant_before_solve():
    problem = _build_convex_problem(num_pools=2)
    assert problem.is_dcp(dpp=True)
```

### Benchmark (Slice 2)

```python
# Benchmark re-solve time (not first-solve) for CPP vs COO at N=2,3,4,5
def benchmark_canon_backend():
    for n_pools in [2, 3, 4, 5]:
        problem = _build_convex_problem(n_pools)
        # Update parameters with dummy values
        problem.solve(solver=cvxpy.CLARABEL, enforce_dpp=True)  # warm up
        # Time re-solve with CPP (default) and COO backends
```

### Integration tests

The existing `TestCVXPYComparison` and `TestCVXPYSolverAccuracy` classes in
`test_solver_integration.py` cover cvxpy vs ArbSolver agreement and cvxpy constraint satisfaction.
These must continue to pass after each slice.

## Benefits

- **Correctness**: Moving the DPP assertion before the warm-up solve catches non-DPP construction
  at import time, before the object is pickled into worker processes.
- **Performance**: Adding `enforce_dpp=True` to the 2-pool re-solve activates the DPP
  canonicalization cache, meaning parameter updates trigger only O(parameters) re-canonicalization
  instead of a full rebuild.
- **Data-driven**: The COO benchmark provides evidence rather than assumptions about
  canonicalization backend performance at the pool counts used in practice.
- **Maintainability**: The test factory deduplicates ~60 lines of near-identical problem
  construction across 3 test methods.

## Risks

- **COO backend may not be faster at N≤5**: The COO backend's advantage is for "large parameters."
  For 2–5 pools (4–25 parameter elements), the overhead of COO sparse tensor construction may
  outweigh the benefit. Mitigation: benchmark before committing to COO; negative results are
  valuable too.
- **Legacy code deprecation timeline**: These changes target code that is already deprecated (Plan
  038). The test oracle (Slice 3) is the only long-lived component. Mitigation: Slices 1–2 are
  minimal bug-fixes to the legacy code; Slice 3 is the forward-looking investment.
- **CVXPY 1.9 compatibility**: Bumping from 1.8→1.9 is a minor version bump with a stable public
  API, but the DNLP subsystem is new. Mitigation: CVXPY follows semantic versioning; 1.8→1.9 is
  backward-compatible since the existing code doesn't use DNLP.

## Relationship to Other Plans

- **Plan 038** (Deprecate Legacy Arbitrage Cycle Classes): Active dependency. The legacy code
  modified in Slice 1 is the same code deprecated by Plan 038. Eventually, `_legacy/` will be
  deleted entirely. Slice 3 (test factory) is the only change that persists.
- **Plan 011** (Unify UniswapLpCycle._calculate() Behind the ArbSolver Seam): Completed. The
  ArbSolver fast-path has already replaced cvxpy as the primary solver. CVXPY is now only a
  comparison oracle in tests.
- **Arbitrage Optimizer project** (active): Complementary. The optimizer project builds
  Rust-accelerated solvers; this plan improves the Python-side CVXPY comparison oracle that
  validates them.

## Status

[x] Slice 1: Fix 2-pool DPP assertion, `enforce_dpp=True`, and solver constant
[x] Slice 2: Benchmark COO canonicalization backend
[x] Slice 3: Extract 2-pool CVXPY problem factory for test code

---

## Appendix: DNLP Evaluation for Arbitrage Solvers

CVXPY 1.9 introduced **Disciplined Nonlinear Programming (DNLP)**, which extends DCP to allow
smooth nonconvex functions (e.g., `cp.nlp.multiply(x, y)` with two variables, `cp.nlp.exp`,
`cp.nlp.sin`) under a disciplined ruleset, solved by NLP backends (IPOPT, UNO, COPT, KNITRO).

**Conclusion: DNLP is not recommended for inclusion as an arbitrage solver.** Reasons:

1. **Local optima only**: DNLP's NLP solvers provide no global optimality guarantees for
   nonconvex problems. MEV arbitrage is winner-take-all — a solver that finds a local optimum when
   a global optimum exists elsewhere will lose. The existing specialized solvers (Möbius,
   QuantAMM, golden section, Brent) provide global optimality within their domain.

2. **Latency**: IPOPT solves in ~1–10ms. The existing solvers run in 0.19μs (Möbius/Rust) to
   390μs (Brent). This is a 10–1000× performance gap that is unacceptable for on-chain MEV
   competition.

3. **Integer precision**: All on-chain execution requires integer amounts. DNLP operates in float
   space. The existing solvers either work in integer space directly (`RustIntHopState`) or have
   an integer refinement step. Adding float→int conversion on top of a local optimizer compounds
   error.

4. **Already-solved problems**: The pool types where DNLP could theoretically help (Solidly stable
   x³y+xy³≥k, Curve stableswap) already have specialized solvers that use the pool's own
   `swap_fn` for exact EVM-level simulation — something an NLP formulation cannot replicate.

5. **Dependency concern**: Per Plan 038, `cvxpy` was moved to an optional `legacy-cycles`
   dependency because `ArbSolver` doesn't use it. Adding DNLP-based solvers would re-introduce a
   hard `cvxpy` dependency and add `ipopt` as a system-level C++ dependency, going against the
   project's direction of Rust-accelerated, dependency-minimal solvers.

**Where DNLP could be useful** (not part of this plan):
- Research/prototyping new pool invariants before building specialized solvers
- Multi-path optimization where portfolio effects create genuinely nonconvex landscapes
- Backtesting where latency isn't critical
