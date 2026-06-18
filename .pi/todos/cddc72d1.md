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
  "created_at": "2026-06-17T20:37:47.254Z"
}

**Slice 15 of the Polars three-layer migration (ADR-005).** Master: `TODO-7e24d695`.

## Why this slice exists

Pickle support (`PoolPickleMixin`, `StateCache`/`PerBlockCache` `__getstate__`, `Erc20Token.__getstate__`) exists solely to feed Python's `concurrent.futures`/`ProcessPoolExecutor` multiprocessing. **Verified zero in-repo consumers**: no `concurrent.futures`/`ProcessPoolExecutor`/`pickle.dumps(loads)` callsites in `src/` or `examples/`; the sole `ProcessPoolExecutor` mention is a docstring warning (`curve_stableswap_liquidity_pool.py:1090`). Once state + calc live in the Rust `Bot` (slices 3-9), path-solve fan-out goes cross-core in Rust (`tokio::spawn` / `rayon` over the shared `Arc<RwLock<Bot>>`) with no pickle cost. So the whole Python-pickle multiprocessing capability is disposable.

## Goal

Retire Python-pickle multiprocessing and replace it with Rust-side parallel solve fan-out, deleting the now-dead pickle machinery + tests.

## Rust-side parallel solve fan-out (the replacement)

- The solver (`exact_mobius_solve` / `int_solve_cl_path` / `exact_solve_mixed_path_n`) and the pump already run in Rust; the engine reads `Bot` state by reference (ADR-003).
- Parallelize `solve_dirty`'s affected-path solves across cores: `rayon::par_iter` (or `tokio::spawn`-per-batch) over the affected path set, writing results back under the existing result-channel discipline.
- Naturally pairs with Slice 10 (`UniswapEngine` lock unification → one shared `Arc<RwLock<Bot>>` to fan out over). Not a hard dependency — fan-out works on the engine's own `Bot` today; unification just makes it the canonical one.

## Remove the dead pickle machinery (per the user ruling: "remove the pickle tests as a slice of that effort")

- Delete `src/degenbot/types/pool_pickle.py` + `PoolPickleMixin` from every pool class's bases (V2/V3/V4/Aerodrome/Camelot/Curve/Balancer) + its `_pickle_drops`/`_pickle_reconstructs` declarations.
- Delete `Erc20Token.__getstate__` (Slice 3's transient hack) and any per-pool `__getstate__` hacks slices 4-9 add (each pool-family slice will carry one until this slice lands — that churn is expected + this slice cleans it all).
- Audit `StateCache`/`PerBlockCache` `__getstate__`/`__setstate__`: if purely in service of pool-pickle, delete; if any is used for non-multiprocessing snapshot persistence, keep (DB snapshots load via Rust custom deserializers, NOT Python pickle — so expect full removal).
- Delete the pickle tests: `tests/exceptions/test_pickling_exceptions.py`, `tests/types/test_pool_pickle_mixin.py`, `tests/uniswap/v2/test_v2_pool_io_free.py::test_io_free_pool_pickle`, `tests/uniswap/v3/test_v3_pool_io_free.py::test_io_free_pool_pickle`, `tests/uniswap/v4/test_v4_pool_io_free.py` pickle cases, `tests/curve/test_per_block_cache.py`/`test_pool_strategies.py`/`test_curve_stableswap_pool.py` pickle cases, `tests/aerodrome/test_aerodrome_pools.py` pickle cases, `tests/types/test_state_cache.py`, `tests/test_cache.py` — audit each: remove multiprocessing-bound ones; keep only genuinely-non-multiprocessing ones (expect near-total removal).
- Update `curve_stableswap_liquidity_pool.py:1090` `ProcessPoolExecutor` docstring → point at the Rust-side fan-out.

## Sequencing note

The **pickle-test/mixin removal sub-step is independent** and could be pulled EARLY (even now) to avoid the recurring `__getstate__`-hack churn in slices 4-9. The **Rust parallel solve fan-out** prefers Slice 10 (unified engine) for a clean single-`Bot` fan-out. If the user wants to avoid per-slice hacks during 4-9, split this slice: (15a) removal (now), (15b) Rust fan-out (after 10). Bundled here by default per the user's "remove the pickle tests as a slice of that effort" ruling.

## Dependencies

- Test/mixin-removal: none (delete dead code).
- Rust parallel solve fan-out: pairs with Slice 10; solver + engine already in Rust so not strictly blocked.

## Consistency at boundary

No in-repo code breaks (no pickle consumer). Pool-pickle tests gone (not skipped — removed). `concurrent.futures` consumers (if any external) break — documented API break (0.x). Rust fan-out verified by engine stress tests (path-solve throughput under contention, no deadlock).

## Acceptance

- `cargo`/`ruff`/`ty` green; engine path-solve tests pass with the Rust parallel fan-out; no deadlock regressions under stress.
- `rg 'PoolPickleMixin|pickle\.dumps|concurrent\.futures|ProcessPoolExecutor' src/` empty (modulo documentation references now pointing at the Rust fan-out).
- `tests/` contains no pickle-round-trip tests (modulo any kept-after-audit).
- The Curve docstring updated.
