# 3VGIDY — Solidly parity oracle retirement (§4.3)

## Deleted
- `src/degenbot/arbitrage/solvers/solidly_stable.py`
- its `Sol��dlyStableSolver` re-export in `solvers/__init__.py` (+ docstring bullet + `__all__` entry)
- `tests/arbitrage/test_engine_vs_solidly_parity.py` (parity test)
- `tests/arbitrage/test_solvers/test_solidly_stable_solver.py` (dedicated solver suite)
- stale `SolidlyStableSolver suite` comment in `tests/arbitrage/test_solvers/conftest.py`

## Moved (not deleted)
Generic mixed-path simulators reused by kept Curve tests → `src/degenbot/arbitrage/solvers/_solver_utils.py`:
- `_simulate_mixed_path`, `_simulate_mixed_path_int`, `_solidly_swap_output_float` (+ `_CURVATURE_THRESHOLD`, `_NEWTON_CONVERGENCE_TOLERANCE`).

### `_solidly_swap_output_float` disposition: **(a) MOVE**
Per the orchestrator's rule, grep for `SolidlyStableHop(...)` constructions WITHOUT `swap_fn`:
- **Production** (2 sites) BOTH pass `swap_fn`: `aerodrome/pools.py:508` (`swap_fn=_stable_swap_fn`), `v2_liquidity_pool.py:613` (`swap_fn=_camelot_stable_swap_fn`).
- **Tests** (4 sites) ALL construct WITHOUT `swap_fn`: `test_solver_tagged_hops.py:107/122/223/297`.

Since non-`swap_fn` constructions exist (test tree), the float fallback is reachable → disposition **(a)**: moved `_solidly_swap_output_float` to `_solver_utils.py` alongside the helpers, behavior preserved. (Default per orchestrator instruction; did not simplify the Solidly branch to a raise-on-None guard.)

## Repointed (kept, live Curve coverage)
- `tests/arbitrage/test_fake_curve_pool.py`
- `tests/arbitrage/integration/test_curve_legacy_equivalence.py`

## Untouched (live production)
- `SolidlyStableHop` hop type (constructed in `aerodrome/pools.py`, `v2_liquidity_pool.py` with `swap_fn`).
- `has_solidly_stable` property in `hop_types.py`.
- Rust `solve_solidly_path_int` (production solve path); `solver_dispatch.rs` port-provenance COMMENTS left as historical.

## Docs updated
- `src/degenbot/arbitrage/CONTEXT.md` Solidly Solver row → `*(removed)*`.
- `docs/migration-guides/three-layer-transition.md` Fork B: added §4.3 retirement bullet.
- `rust/CONTEXT.md`: "Exception — SolidlyStableSolver" → `(retired)`.

## Verification
- `rg SolidlyStableSolver src/ tests/` → only the CONTEXT.md retirement record (non-live, mirrors "Mobius Solver *(removed)*" precedent) + Rust comment provenance; **no live refs**.
- `rg arbitrage.solvers.solidly_stable src/ tests/` → zero.
- `just test-python` → 3003 passed, 30 skipped.
- `just lint` → green (ruff + ty + clippy --deny warnings + markdownlint, 0 errors).
- `just format` → 325 files unchanged.
- `just check-no-pyo3-in-cores` → OK.

## Note
Did NOT run `just test-rust` — touched zero Rust source; concurrent worker_1/worker_2 Rust WIP (`Cargo.toml`, `Cargo.lock`, `degenbot/src/lib.rs`, `database/operations.py`, `degenbot-evm-math/` crate) left untouched in the working tree and not committed.