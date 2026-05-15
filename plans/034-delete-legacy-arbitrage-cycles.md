# Plan 034: Delete Legacy Arbitrage Cycle Classes

## Status: REJECTED

**Superseded by Plan 038** — hard delete replaced by gradual deprecation with `_legacy/` sub-package, underscore class renaming, migration guide, and optional `cvxpy` dependency group.

## Overview

Remove the four deprecated legacy arbitrage cycle classes (`UniswapLpCycle`, `UniswapMultipoolCycleTesting`, `UniswapCurveCycle`, `Uniswap2PoolCycleTesting`), the vestigial `AbstractArbitrage` class, and the `get_arbitrage_helpers()` method on pool classes. All functionality is already provided by `ArbitragePath` + `generate_payloads()` + the typed solver hierarchy.

**This plan is a prerequisite for Plan 033** — the legacy cycles are the only `src/` consumers of `solver_hop_builders.py`.

## Files Involved

**Primary (delete):**
- `src/degenbot/arbitrage/uniswap_lp_cycle.py` (721 lines) — `UniswapLpCycle`
- `src/degenbot/arbitrage/uniswap_multipool_cycle_testing.py` (842 lines) — `UniswapMultipoolCycleTesting`
- `src/degenbot/arbitrage/uniswap_curve_cycle.py` (891 lines) — `UniswapCurveCycle`
- `src/degenbot/arbitrage/uniswap_2pool_cycle_testing.py` (3616 lines) — `Uniswap2PoolCycleTesting`
- `src/degenbot/types/abstract/arbitrage.py` (8 lines) — `AbstractArbitrage`

**Primary (update — remove `get_arbitrage_helpers()` and `AbstractArbitrage` imports):**
- `src/degenbot/uniswap/v2_liquidity_pool.py` — remove `get_arbitrage_helpers()`, remove `AbstractArbitrage` import
- `src/degenbot/uniswap/v3_liquidity_pool.py` — same
- `src/degenbot/uniswap/v4_liquidity_pool.py` — same
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — same
- `src/degenbot/types/abstract/__init__.py` — remove `AbstractArbitrage` re-export
- `src/degenbot/arbitrage/__init__.py` — remove `UniswapCurveCycle`, `UniswapLpCycle` re-exports
- `src/degenbot/__init__.py` — remove `UniswapCurveCycle`, `UniswapLpCycle` re-exports

**Secondary (delete test files for legacy classes):**
- `tests/arbitrage/integration/test_uniswap_lp_cycle.py`
- `tests/arbitrage/integration/test_uniswap_curve_cycle.py`
- `tests/arbitrage/integration/test_uniswap_2pool_cycle.py`
- `tests/arbitrage/integration/test_v3_only_legacy_equivalence.py`
- `tests/arbitrage/test_optimizers/test_cvxpy_optimizer.py`
- `tests/arbitrage/test_optimizers/test_cvxpy_multipool.py`
- `tests/arbitrage/mock_pools.py` — mock pool infrastructure for `UniswapLpCycle` only
- `tests/arbitrage/verify_legacy_equivalence.py` — verification script

**Secondary (update test files):**
- `tests/arbitrage/test_offline_integration.py` — heavy `UniswapLpCycle` usage; rewrite against `ArbitragePath` or delete
- `tests/arbitrage/test_mock_pools.py` — tests mock pools with `UniswapLpCycle`; delete `TestUniswapLpCycleIntegration` class
- `tests/arbitrage/test_optimizers/test_solver_integration.py` — imports `cvxpy` for comparison; may need cvxpy removed from test body

## Problem

Four legacy cycle classes (~6,070 lines) remain in the codebase despite `UniswapLpCycle` being explicitly deprecated (it raises `DeprecationWarning` at construction). They duplicate functionality already provided by:

| Legacy class | Replacement |
|---|---|
| `UniswapLpCycle` | `ArbitragePath` + `ArbSolver` |
| `UniswapMultipoolCycleTesting` | `ArbitragePath` + `ArbSolver` (Rust-accelerated mobius solver) |
| `UniswapCurveCycle` | `ArbitragePath` + `ArbSolver` (CurveStableswapHop support) |
| `Uniswap2PoolCycleTesting` | `ArbitragePath` + `ArbSolver` (2-pool fast path in mobius solver) |

These classes pass the **deletion test**: if deleted, no complexity needs to reappear elsewhere.

### The `AbstractArbitrage` / `get_arbitrage_helpers()` problem

`AbstractArbitrage` is a two-field class (`id: str`, `swap_pools: Sequence[AbstractLiquidityPool]`) that serves two purposes:

1. **Base class for legacy cycles** — `UniswapLpCycle` and `UniswapCurveCycle` inherit it.
2. **Filter in pool `get_arbitrage_helpers()`** — V2, V3, V4, and Curve pools each have a `get_arbitrage_helpers()` that does `isinstance(subscriber, AbstractArbitrage)` over their subscribers.

`ArbitragePath` does **not** inherit `AbstractArbitrage`, so `get_arbitrage_helpers()` returns empty for `ArbitragePath` subscribers — making it a dead method when only `ArbitragePath` is in use.

The only caller of `get_arbitrage_helpers()` outside the pool classes is `tests/arbitrage/integration/test_uniswap_lp_cycle.py` — a legacy test file being deleted. The method is vestigial: the pub/sub system already tracks all subscribers in `_subscribers`, and any code that needs to find arbitrage paths among subscribers can filter by the `ArbitragePathPool` protocol or a simple `hasattr` check.

**Solution**: Delete `AbstractArbitrage` and `get_arbitrage_helpers()` together. No replacement needed — the method serves no one.

### cvxpy / scipy dependency status

**In `src/`**: `cvxpy` is only imported by the two legacy cycle files being deleted. `scipy` is also imported by the legacy cycles, plus one live file: `src/degenbot/arbitrage/optimizers/brent_solver.py` uses `scipy.optimize.minimize_scalar`. So `cvxpy` becomes fully removable; `scipy` must stay as a runtime dependency for the Brent solver.

**In `tests/`**: `cvxpy` is imported by `test_cvxpy_optimizer.py` and `test_solver_integration.py` (both test the legacy cvxpy optimizer). `scipy` is imported by several test files including the Brent solver tests. After deleting the cvxpy-specific test files, `cvxpy` can be removed from test dependencies too. `scipy` remains in test dependencies.

## Solution

### Step 1: Delete the four legacy cycle source files

Delete:
- `src/degenbot/arbitrage/uniswap_lp_cycle.py`
- `src/degenbot/arbitrage/uniswap_multipool_cycle_testing.py`
- `src/degenbot/arbitrage/uniswap_curve_cycle.py`
- `src/degenbot/arbitrage/uniswap_2pool_cycle_testing.py`

This removes ~6,070 lines and the only `src/`-level `cvxpy` imports.

### Step 2: Remove `AbstractArbitrage` and `get_arbitrage_helpers()`

Delete `src/degenbot/types/abstract/arbitrage.py`.

Remove the `AbstractArbitrage` import and `get_arbitrage_helpers()` method from each pool class:

- `src/degenbot/uniswap/v2_liquidity_pool.py` — remove import + method (~7 lines)
- `src/degenbot/uniswap/v3_liquidity_pool.py` — remove import + method (~7 lines)
- `src/degenbot/uniswap/v4_liquidity_pool.py` — remove import + method (~7 lines)
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — remove import + method (~7 lines)

Remove `AbstractArbitrage` from `src/degenbot/types/abstract/__init__.py` re-exports.

### Step 3: Update re-exports

- `src/degenbot/arbitrage/__init__.py` — remove `UniswapCurveCycle`, `UniswapLpCycle` imports and `__all__` entries
- `src/degenbot/__init__.py` — remove `UniswapCurveCycle`, `UniswapLpCycle` imports and `__all__` entries

### Step 4: Delete legacy test files

Delete tests that directly test deleted classes:
- `tests/arbitrage/integration/test_uniswap_lp_cycle.py`
- `tests/arbitrage/integration/test_uniswap_curve_cycle.py`
- `tests/arbitrage/integration/test_uniswap_2pool_cycle.py`
- `tests/arbitrage/integration/test_v3_only_legacy_equivalence.py`
- `tests/arbitrage/test_optimizers/test_cvxpy_optimizer.py`
- `tests/arbitrage/test_optimizers/test_cvxpy_multipool.py`
- `tests/arbitrage/mock_pools.py` — mock pool infrastructure solely for `UniswapLpCycle`

Update tests that partially reference legacy classes:
- `tests/arbitrage/test_offline_integration.py` — heavy `UniswapLpCycle` usage; rewrite against `ArbitragePath` or delete the legacy test class
- `tests/arbitrage/test_mock_pools.py` — delete `TestUniswapLpCycleIntegration` class; keep `MockV2Pool`/`MockV3Pool`/`MockV4Pool` if other tests use them
- `tests/arbitrage/verify_legacy_equivalence.py` — delete (verification script for legacy parity)
- `tests/arbitrage/test_optimizers/test_solver_integration.py` — remove cvxpy-based comparison section if present; keep non-cvxpy tests

### Step 5: Remove cvxpy from dependencies

After Steps 1 and 4, `cvxpy` has zero imports in `src/` and `tests/`. Remove from project dependencies (likely `pyproject.toml` or `setup.cfg`).

`scipy` stays — `brent_solver.py` uses `scipy.optimize.minimize_scalar`.

### Step 6: Run full test suite

```bash
just test-python
```

## Implementation Order

1. Step 1: Delete legacy source files (4 files, ~6,070 lines)
2. Step 2: Remove `AbstractArbitrage` + `get_arbitrage_helpers()` (5 files, ~35 lines removed)
3. Step 3: Update re-exports (2 files)
4. Step 4: Delete/update legacy test files
5. Step 5: Remove cvxpy dependency
6. Step 6: Full test suite

Steps 1–3 can be one commit. Step 4 is a separate commit.

## Testing

### After deletion

`just test-python` must pass with no reference to the deleted classes, `AbstractArbitrage`, or `get_arbitrage_helpers()` anywhere in `src/` or `tests/`.

### ArbitragePath coverage checklist

Ensure existing `ArbitragePath` tests cover the scenarios the legacy tests were exercising:

- [ ] Multi-pool V2 paths (2-pool, 3-pool, N-pool)
- [ ] V2 + V3 mixed paths
- [ ] V4 pool paths
- [ ] Curve + Uniswap mixed paths
- [ ] Aerodrome stable + volatile mixed paths
- [ ] State override simulation
- [ ] Process pool execution
- [ ] Pub/sub state update notifications

If any checkbox is uncovered, add a test to `tests/arbitrage/test_path/` before deleting the legacy files.

## Benefits

- **~6,070 lines removed** from `src/degenbot/arbitrage/`.
- **Locality**: Arbitrage path solving and encoding concentrated in `ArbitragePath` + `encoding.py`. No dual maintenance.
- **Dependency reduction**: `cvxpy` fully removed from runtime and test dependencies. Install size and startup time improve.
- **Simpler module surface**: `degenbot.arbitrage` exports become `ArbitragePath`, `ArbSolver`, encoding types, and solver types.
- **Dead code removed**: `AbstractArbitrage` was a two-field class serving only as an isinstance filter. `get_arbitrage_helpers()` returned empty tuples for the new `ArbitragePath`. Both gone.

## Risks

- **Test coverage regression**: The legacy test files exercise solver scenarios that `ArbitragePath` tests may not cover. The checklist above mitigates this. If uncertain, keep `verify_legacy_equivalence.py` temporarily as an oracle — delete in a follow-up once confidence is established.
- **`mock_pools.py` reuse**: `MockV2Pool`, `MockV3Pool`, `MockV4Pool` may be used by tests independent of `UniswapLpCycle`. Before deleting the entire file, check if other test files import from it. If so, extract the mock pool classes into a shared fixture.
- **`scipy` stays as a runtime dependency**: `brent_solver.py` uses `scipy.optimize.minimize_scalar`. If the Brent solver is later replaced by a pure-Rust implementation, `scipy` can be removed entirely — but that's outside this plan's scope.

## Relationship to Other Plans

- **Plan 033** (Consolidate Dual Pool-to-Hop Conversion): **This plan is a prerequisite.** The legacy cycles are the only `src/` consumers of `solver_hop_builders.py`. After this plan, Plan 033 becomes pure deletion.
- **Plan 011** (Unify UniswapLpCycle._calculate() Behind ArbSolver Seam): Complete. Created `ArbitragePath` as the replacement. This plan finishes the job.
- **Plan 021** (Extract SwapEncoder from UniswapLpCycle): Complete. Created `encoding.py` with `generate_payloads()`. This plan removes the inline encoding that remained in the legacy classes.
- **Plan 028** (Builder Registry): Complete. The legacy classes don't use the builder registry; they construct pools directly (another reason they're obsolete).
