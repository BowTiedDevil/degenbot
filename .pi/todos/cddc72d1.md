{
  "id": "cddc72d1",
  "title": "Slice 15: Retire pickle multiprocessing; land Rust-side parallel solve fan-out",
  "tags": [
    "polars-three-layer",
    "adr-005",
    "pickle",
    "multiprocessing",
    "rust-fan-out",
    "deferred",
    "slice-15"
  ],
  "status": "open",
  "created_at": "2026-06-17T20:37:47.254Z",
  "assigned_to_session": "019ed8af-4f02-7e42-8bab-a8df31e260a5"
}

**Slice 15 of the Polars three-layer migration (ADR-005).** Master: `TODO-7e24d695`.

## Status

**15a (pickle machinery + test removal) — DONE.** Landed ahead of slices 4–9 to
clear the `PyDexIdentity`-unpicklable failures (ADR-005 slice 6 moved
`DexIdentity` to a Rust `#[pyclass]`, which broke `pickle.dumps()` on every
pool holding one). Committed as `remove(arbitrage): retire Python-pickle
multiprocessing machinery + tests`.

**15b (Rust-side parallel solve fan-out) — DEFERRED.** Pairs with the unified
engine (ADR-006 slice 1, the shared `Arc<RwLock<Bot>>`). Not started.

## What 15a removed

- Deleted `src/degenbot/types/pool_pickle.py` (`PoolPickleMixin`).
- Stripped `PoolPickleMixin` from bases + removed `_pickle_drops` /
  `_pickle_reconstructs` on: `LiquidityPool`, `UniswapV3Pool`,
  `UniswapV4Pool`, `AerodromeV2Pool`, `BalancerV2Pool`,
  `BalancerV2StablePool`, `CurveStableswapPool`.
- Removed `Erc20Token.__getstate__`, `StateCache.__getstate__/__setstate__`,
  `PerBlockCache.__getstate__/__setstate__`.
- Removed `PoolPickleMixin` from `types/__init__.py` (`__all__` + import).
- Deleted / pruned pickle-round-trip tests across 15 test files (whole-file:
  `tests/types/test_pool_pickle_mixin.py`; pruned: `test_state_cache.py`,
  `test_cache.py`, the three `test_*_pool_io_free.py`, `test_per_block_cache.py`,
  `test_pool_strategies.py`, `test_curve_stableswap_pool.py`,
  `test_aerodrome_pools.py`, `test_offline_integration.py`,
  `test_v2_offline.py`, `test_v3_offline.py`,
  `test_uniswap_v2_liquidity_pool.py`, `test_uniswap_v3_liquidity_pool.py`).
- Removed the 2 failing legacy tests (`test_uniswap_curve_cycle.py::
  test_pickle_arb` + `test_process_pool_calculation`) and the 2 already-skipped
  legacy tests in `test_uniswap_lp_cycle.py` (skip→delete).
- Updated docstrings: Curve `swap_fn` note, `v2_pool_state.py` mixin list.

## Audit deviations from the original plan (intentional, recorded)

1. **`tests/exceptions/test_pickling_exceptions.py` KEPT.** The plan said delete
   it; audit shows exception pickling is still LIVE — `ArbitragePath.
   calculate_with_pool(ProcessPoolExecutor)` (retained in 15a) raises solver
   exceptions (`NoSolverSolution`, `OptimizationError`, …) across the process
   boundary, which requires exceptions to be picklable. Removing it would
   regress the retained multiprocessing path. Delete it in 15b when
   `calculate_with_pool` is itself retired.

2. **`ArbitragePath.calculate_with_pool(ProcessPoolExecutor)` KEPT.** The plan's
   `rg 'concurrent.futures|ProcessPoolExecutor' src/` empty acceptance line is a
   15b concern, not 15a. `calculate_with_pool` is live API; it does NOT pickle
   pools (submits module-level `_solve_in_subprocess` + fresh `ArbSolver()`),
   so it does not depend on the removed pool-pickle machinery. Verified by
   `tests/arbitrage/integration/test_calculate_with_pool.py` (6 passed).

3. **Legacy cycle `__getstate__` (`_uniswap_curve_cycle.py`,
   `_uniswap_lp_cycle.py`) LEFT.** Dead code on deprecated `_legacy/` modules
   slated for full removal (AGENTS.md `remove` type). Out of 15a scope; removing
   it is part of legacy-cycle retirement.

4. **Provider `__getstate__`/`__setstate__` (`sync_adapter.py`,
   `async_adapter.py`) LEFT.** Not pool-pickle machinery; the plan did not list
   it. Possibly dead now but out of 15a scope.

## Audit that made removal safe

- No `copy.deepcopy` / `copy.copy` of pool/cache/token objects anywhere in
  `src/` or `tests/` (all `.copy()` calls are on dicts/lists) — so removing
  `__getstate__`/`__setstate__` breaks no deepcopy path.
- No non-pickle callers of `__getstate__`/`__setstate__` in `src/` (only
  comments + the deleted pickle tests).

## Verification

- `ruff check src/` + `ruff format --check src/` + `ty check src/` all green.
- `cargo fmt --check` + `clippy --deny warnings` green.
- Offline Python suites green (551 tests across types/curve/cache/offline/
  io_free/exceptions/rust-wrapped/path).
- Net lint delta vs baseline: ruff 607→606, ty 1704→1696 (zero new errors).

## 15b (deferred) — Rust-side parallel solve fan-out

- Parallelize `solve_dirty`'s affected-path solves: `rayon::par_iter` (or
  `tokio::spawn`-per-batch) over the affected path set, writing results back
  under the existing result-channel discipline.
- Pairs with ADR-006 slice 1 (unified engine → one shared
  `Arc<RwLock<Bot>>` to fan out over).
- At that point: retire `ArbitragePath.calculate_with_pool` + the legacy
  `calculate_with_pool` + the provider `__getstate__` + the exception-pickling
  tests + the legacy cycle `__getstate__`.
- Acceptance: `rg 'PoolPickleMixin|pickle\.dumps|concurrent\.futures|
  ProcessPoolExecutor' src/` empty (modulo docs); engine path-solve tests pass
  with the Rust parallel fan-out; no deadlock regressions under stress.
