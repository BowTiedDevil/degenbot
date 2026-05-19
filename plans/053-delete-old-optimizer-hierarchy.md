# Plan 053: Delete Old Optimizer Hierarchy

## Overview

Remove the deprecated `ArbitrageOptimizer` ABC, its `OptimizerResult`/`OptimizerType` types, and all concrete implementations (`NewtonV2Optimizer`, `ChainRuleNewtonOptimizer`, `BoundedProductOptimizer`, `MobiusOptimizer`, `V2V3Optimizer`, `BatchNewtonOptimizer`, `BatchMobiusOptimizer`). The new `Solver`/`SolveResult`/`SolveInput` hierarchy is the sole solver interface. Pure-math functions currently interleaved with old optimizer wrappers are extracted into focused modules so the new `Solver` implementations can import them without pulling in dead code.

## Problem

### Deletion test

If you deleted `ArbitrageOptimizer`, `OptimizerResult`, `OptimizerType`, and the seven concrete old-protocol classes, nothing in production would break — `ArbitragePath` uses `Solver`/`SolveInput`/`SolveResult` exclusively. The only callers are tests that instantiate old-protocol classes directly. If the deleted code *concentrated* complexity (i.e., callers would have to re-implement it), that would signal it was earning its keep. Instead, the new hierarchy already provides equivalent functionality through a cleaner interface.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| Two ABC hierarchies in one directory | `optimizers/base.py` (old) vs `optimizers/hop_types.py` (new) | A reader must figure out which `solve()` signature they're looking at — `pools: list[Any]` or `solve_input: SolveInput` |
| `OptimizerResult` vs `SolveResult` overlap | `base.py` vs `hop_types.py` | Same data (optimal_input, profit, iterations, method), different types. Callers of `ArbitragePath` get `SolveResult`; old test callers get `OptimizerResult`. No conversion exists. |
| `MobiusOptimizer` interleaved with pure math | `mobius.py` 1042 lines | `MobiusOptimizer` (old, pool-object-taking) wraps `compute_mobius_coefficients` + `mobius_solve` (pure math). New `MobiusSolver` (in `mobius_solver.py`) imports the same pure math. The old wrapper adds ~200 lines of dead code in the same file. |
| 8 old-protocol classes, 0 production callers | `newton.py`, `chain_rule.py`, `bounded_product.py`, `mobius.py`, `v2_v3_optimizer.py`, `vectorized_batch.py`, `batch_mobius.py` | ~3800 lines of code that nothing outside `optimizers/` and tests imports |
| `OptimizerType` enum duplicates `SolverMethod` | `base.py` OptimizerType vs `hop_types.py` SolverMethod | NEwTON↔MOBIUS↔BRENT mappings are duplicated |
| Old classes import pool objects directly | `newton.py` imports `UniswapV2Pool` | Violates the I/O-free pool architecture — optimizers should operate on data, not live objects |

## Solution

### Step 1: Extract pure math from `mobius.py`

The pure-math functions (`MobiusFloatHop`, `V3TickRangeHop`, `V3TickRangeSequence`, `compute_mobius_coefficients`, `mobius_solve`, `simulate_path`, `_mobius_forward`, `_mobius_coeffs_for_hop`, `_compute_mobius_v3`) are intermixed with the `MobiusOptimizer` class. Extract them into `optimizers/_mobius_math.py`.

```python
# Before: mobius.py contains both pure math AND MobiusOptimizer (old)
class MobiusOptimizer(ArbitrageOptimizer):
    def solve(self, pools, input_token, ...):
        ...

# After: _mobius_math.py has only pure math; mobius.py deleted
# mobius_solver.py and piecewise_mobius_solver.py import from _mobius_math.py
```

### Step 2: Extract pure math from `v2_v3_optimizer.py`

The `V2PoolState`, `CandidateSolution`, `V2V3OptimizationResult` dataclasses are used only by `V2V3Optimizer`. The helper functions `solve_v2_v3_single_range`, `find_optimal_input_for_range`, `check_stays_in_range` are pure math. Move all of `v2_v3_optimizer.py` into the test directory since it has zero production callers — nothing in the new `Solver` hierarchy delegates to it.

### Step 3: Delete `ArbitrageOptimizer` base class and `OptimizerResult`/`OptimizerType`

Remove from `base.py`:
- `class OptimizerType(Enum)` — replaced by `SolverMethod`
- `class OptimizerResult` — replaced by `SolveResult`
- `class ArbitrageOptimizer(ABC)` — replaced by `Solver`

Keep `base.py` only if it contains anything the new hierarchy needs. After inspection it does not — all new solvers inherit from `hop_types.Solver`. Delete the entire file.

### Step 4: Delete old-protocol concrete classes

| File | Class to delete | Notes |
|------|-----------------|-------|
| `newton.py` | `NewtonV2Optimizer` | File also has pure gradient/hessian functions (`v2_profit_gradient_and_hessian`). Move those to test helper or delete if no test uses them. |
| `chain_rule.py` | `ChainRuleNewtonOptimizer` | File has `PoolState`, `compute_path_gradient`, `compute_path_hessian`. Check if any new solver uses them. Likely no — delete. |
| `bounded_product.py` | `BoundedProductOptimizer` | Same pattern. |
| `mobius.py` | `MobiusOptimizer` | After Step 1, the file is just the old wrapper. Delete the file. |
| `v2_v3_optimizer.py` | `V2V3Optimizer` + dataclasses | Move to tests if needed, else delete. |
| `vectorized_batch.py` | `VectorizedNewtonSolver`, `BatchNewtonOptimizer` | Delete. |
| `batch_mobius.py` | `VectorizedMobiusSolver`, `SerialMobiusSolver`, `BatchMobiusOptimizer` | Delete. |

### Step 5: Update imports in new `Solver` implementations

`mobius_solver.py` and `piecewise_mobius_solver.py` currently import from `mobius.py`. After Step 1, they import from `_mobius_math.py` instead. `solver.py` (ArbSolver) needs no changes — it already delegates to new-protocol solvers.

### Step 6: Migrate or delete old-protocol tests

Tests in `tests/arbitrage/test_optimizers/` that instantiate old-protocol classes (`MobiusOptimizer`, `BatchMobiusOptimizer`, `NewtonV2Optimizer`) should be migrated to test the new `Solver` interface, or deleted if they only tested old-protocol mechanics. Batch tests (`test_batch_mobius.py`, `test_cvxpy_*`) test functionality not yet ported to the new interface — migrate to use `ArbSolver` or `MobiusSolver` with `SolveInput`.

### Design decisions

- **Delete, don't deprecate**: The old hierarchy has zero production callers. A deprecation period adds noise without benefit. Delete outright.
- **Keep `_mobius_math.py` private**: The pure math module is an internal implementation detail of `MobiusSolver` and `PiecewiseMobiusSolver`. The prefix `_` signals this. The public seam is the `Solver` protocol.
- **Don't port batch solvers yet**: `BatchMobiusOptimizer` and `VectorizedNewtonSolver` provide batch-mode optimization not available through the `Solver` interface. Deleting them removes functionality. However, nothing in production calls them. If batch optimization is needed later, it should be designed as a batch extension to the `Solver` interface, not as a separate hierarchy. The test coverage is preserved by moving tests to a `tests/arbitrage/_archived/` directory with a `conftest.py` note.
- **Move `v2_v3_optimizer.py` entirely**: Unlike `mobius.py`, the V2V3 optimizer's pure functions are not reused by new solvers. Move the entire file to `tests/arbitrage/_archived/` rather than splitting it.

## Files Involved

**Primary:**
- `src/degenbot/arbitrage/optimizers/base.py` — deleted entirely
- `src/degenbot/arbitrage/optimizers/newton.py` — deleted (pure helpers moved to tests if needed)
- `src/degenbot/arbitrage/optimizers/chain_rule.py` — deleted
- `src/degenbot/arbitrage/optimizers/bounded_product.py` — deleted
- `src/degenbot/arbitrage/optimizers/mobius.py` — deleted (pure math extracted to `_mobius_math.py`)
- `src/degenbot/arbitrage/optimizers/v2_v3_optimizer.py` — deleted
- `src/degenbot/arbitrage/optimizers/vectorized_batch.py` — deleted
- `src/degenbot/arbitrage/optimizers/batch_mobius.py` — deleted
- `src/degenbot/arbitrage/optimizers/_mobius_math.py` — new file (extracted from `mobius.py`)

**Secondary:**
- `src/degenbot/arbitrage/optimizers/mobius_solver.py` — update imports from `mobius` to `_mobius_math`
- `src/degenbot/arbitrage/optimizers/piecewise_mobius_solver.py` — update imports from `mobius` to `_mobius_math`
- `src/degenbot/arbitrage/optimizers/hop_types.py` — no change (already has `Solver`, `SolveInput`, `SolveResult`, `SolverMethod`)
- `src/degenbot/arbitrage/optimizers/solver.py` — no change (already dispatches to new solvers)
- `src/degenbot/arbitrage/optimizers/__init__.py` — remove old-protocol re-exports
- `src/degenbot/__init__.py` — check for and remove any old-protocol re-exports

**No change needed:**
- `src/degenbot/arbitrage/optimizers/brent_solver.py` — already uses `Solver` interface
- `src/degenbot/arbitrage/optimizers/solidly_stable.py` — already uses `Solver` interface
- `src/degenbot/arbitrage/optimizers/balancer_multi_token_solver.py` — already uses `Solver` interface

## Implementation Order

### Slice 1: Extract `_mobius_math.py`

1. Create `src/degenbot/arbitrage/optimizers/_mobius_math.py` with all pure-math exports from `mobius.py`: `MobiusFloatHop`, `V3TickRangeHop`, `V3TickRangeSequence`, `compute_mobius_coefficients`, `mobius_solve`, `simulate_path`, `_mobius_forward`, `_mobius_coeffs_for_hop`, `_compute_mobius_v3`
2. Update `mobius_solver.py` imports from `mobius` → `_mobius_math`
3. Update `piecewise_mobius_solver.py` imports from `mobius` → `_mobius_math`
4. Run: `just test-python` — expect all tests green

### Slice 2: Delete `base.py` and old-protocol concrete classes

1. Delete `src/degenbot/arbitrage/optimizers/base.py`
2. Delete `newton.py`, `chain_rule.py`, `bounded_product.py`, `mobius.py`, `v2_v3_optimizer.py`, `vectorized_batch.py`, `batch_mobius.py`
3. Update `__init__.py` to remove old-protocol re-exports
4. Run: `just test-python` — expect test failures in old-protocol tests (expected)

### Slice 3: Migrate or archive old-protocol tests

1. Move tests that only test old-protocol classes to `tests/arbitrage/_archived/`
2. Migrate tests that test reusable math (e.g., `test_mobius_optimizer.py` → test `_mobius_math` functions via `MobiusSolver`)
3. Run: `just test-python` — expect all remaining tests green

### Slice 4: Validate and clean up

1. Run `just lint` + `just test-all`
2. Remove any stale imports or references to deleted classes
3. Update `src/degenbot/arbitrage/CONTEXT.md` if terminology changed (remove references to `ArbitrageOptimizer`, `OptimizerResult`, `OptimizerType`)
4. Verify `__init__.py` exports are correct

## Testing

### Per-slice test runs

Each slice runs `just test-python`. Slice 2 is expected to break old-protocol tests; Slice 3 fixes them.

### New unit tests

No new unit tests required — the existing `Solver`-interface tests (`test_mobius_solver_hypothesis.py`, `test_solver_hypothesis.py`, `test_solidly_stable_solver.py`) already cover the new hierarchy. The extracted `_mobius_math.py` is tested transitively through `MobiusSolver` and `PiecewiseMobiusSolver`.

### Integration tests

Existing `tests/arbitrage/` integration tests cover `ArbitragePath` → `ArbSolver` → sub-solvers. No changes needed.

## Benefits

- **Locality**: The optimizer directory shrinks from 21 files (~7800 lines) to ~10 focused files (~2800 lines). Pure math lives in `_mobius_math.py`; solver adapters in thin files.
- **Leverage**: The `Solver` interface is the single seam — one interface, multiple implementations. Deleting the old hierarchy removes a competing interface that offered less leverage (pool-object coupling).
- **Depth**: Each remaining solver file does one thing: adapt a mathematical approach to the `Solver` protocol.
- **I/O-free compliance**: New solvers operate on `SolveInput` (frozen data), never on live pool objects.

## Risks

- **Batch optimization removed**: `BatchMobiusOptimizer` and `VectorizedNewtonSolver` provide vectorized batch-mode that doesn't exist in the new hierarchy. Mitigation: these have zero production callers. If needed later, design a `BatchSolver` protocol as an extension of `Solver`.
- **V2V3 tick-range optimizer removed**: `V2V3Optimizer` provided an optimizer for V2-V3 mixed paths with tick-range prediction. Mitigation: `PiecewiseMobiusSolver` covers the same case via the new interface. If gap is found, extend `PiecewiseMobiusSolver`.
- **Test migration effort**: ~15 test files reference old-protocol classes. Mitigation: bulk-move to `_archived/` rather than individual migration.

## Relationship to Other Plans

- **Plan 011** (Arbitrage LP Cycle Solver Unification): Completed. Established the `Solver` interface that this plan completes the migration to.
- **Plan 038** (Deprecate Legacy Arbitrage Cycle Classes): Completed. Moved legacy cycles to `_legacy/`. This plan is the optimizer-side equivalent — removing the old solver hierarchy.
- **Plan 053** (this plan): Independent of active plans 014 and 048.

## Status

[ ] Slice 1: Extract `_mobius_math.py`
[ ] Slice 2: Delete old-protocol files
[ ] Slice 3: Migrate or archive old-protocol tests
[ ] Slice 4: Validate and clean up
