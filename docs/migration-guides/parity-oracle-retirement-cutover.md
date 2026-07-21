# Parity-oracle retirement cutover — GO/NO-GO verdict

Audit task **`6C32UV`** (epic **`IJAQCF`** — *Parity-oracle retirement
cutover*). This document is the gate artifact for **`LMM2NB`** (*Retire Brent +
QuantAMM parity oracles + legacy hop shape, gated on audit*). `LMM2NB` is
explicitly blocked until this verdict is **GO**.

The audit scope was set against a candidate list of 8 test files (see
[`hop-encoding-relay-retirement.md`](hop-encoding-relay-relay-retirement.md)
§4.3, lines 198–203). **Five of the eight were already deleted** in a prior
retirement slice (`to_hop_state` / `extract_fee` / `build_swap_amount` cutover
under `LYP6L2`). The audit below covers the **3 survivors** + the Python solver
residue + the Rust solver arms.

---

## 1. Surviving candidate test files — disposition

| File | State | Classification | Disposition |
|---|---|---|---|
| `tests/arbitrage/test_engine_vs_brent_parity.py` | **MISSING (already deleted)** | oracle-vs-engine (Brent f64 ↔ engine) | n/a — retired |
| `tests/rust/test_quantamm_basket_parity.py` | EXISTS | **FFI-seam (shell-wiring parity)** | **KEEP (slimmed)** — see §3 |
| `tests/arbitrage/test_curve_solver.py` | MISSING | oracle-vs-engine (Curve) | n/a — retired |
| `tests/arbitrage/test_solvers/test_solver_tagged_hops.py` | EXISTS | **oracle-data-shape only** (tests the f64 Hop dataclasses' construction/frozen/`invariant` derivation; no engine math) | **DELETE** with `types/hop_types.py` |
| `tests/arbitrage/test_solvers/test_solver_hop_builders.py` | MISSING | oracle-data-shape | n/a — retired |
| `tests/arbitrage/test_path/test_arbitrage_path.py` | EXISTS | **live-behavior** (now 71 lines; only tests `v3_virtual_reserves` pure math. Docstring records that the `to_hop_state`/`extract_fee`/`build_swap_amount` surface was already retired) | **KEEP** (misclassified in the candidate list; no longer touches the oracle surface) |
| `tests/arbitrage/integration/test_curve_legacy_equivalence.py` | MISSING | oracle-vs-engine (Curve legacy) | n/a — retired |
| `tests/arbitrage/test_fake_curve_pool.py` | MISSING | oracle fixture | n/a — retired |

`tests/arbitrage/test_solvers/conftest.py` imports `ConstantProductHop` as a
fixture factory; it retires together with `test_solver_tagged_hops.py` (its only
consumer in that dir).

---

## 2. Rust solver-arm readiness assessment

`degenbot_solvers::mixed::solve_path` is the engine entry. Every arm dispatched
from it is rated against its math leaf:

| Arm (`solve.rs`) | Math leaf | Production path | Verdict |
|---|---|---|---|
| Solidly | `simulate_solidly_hop` → `degenbot_solidly_math::{calc_exact_in_stable_solidly, calc_exact_in_stable_camelot, calc_exact_in_volatile}` | dispatched from `solve_path`, reached by `register_and_solve` | **shipped** |
| Balancer weighted | `simulate_balancer_weighted_hop` → `degenbot_balancer_math::weighted_math::calc_out_given_in` | dispatched from `solve_path` | **shipped** |
| Balancer stable | `simulate_balancer_stable_hop` → `degenbot_balancer_math::stable_math::calc_out_given_in` | dispatched from `solve_path` | **shipped** |
| Curve | `simulate_curve_hop` → `degenbot_curve_math::stableswap::stableswap_get_y` | dispatched from `solve_path` | **shipped** |
| Mixed (Mobius) | `solve_mixed_path_int` + `compute_mobius_coefficients` | dispatched from `solve_path` | **shipped** |

The arms above are reached by `ArbitrageEngine` via
`arb_engine::lifecycle::register_and_solve` → `degenbot_solvers::solve_path`.
They are **not** reached through the Python `BrentSolver` oracle (which was an
*f64* reimplementation, not the production U512 path). Retiring `BrentSolver`
therefore removes **no production regression coverage** — the engine exercises
its own arms; the Brent oracle (whose parity test `test_engine_vs_brent_parity.py`
is already deleted) cross-checked a looser f64 reimplementation against the
engine and is gone.

### QuantAMM basket — a separate concern (NOT a `solve_path` arm)

`solve_balancer_weighted` (`rust/crates/degenbot-solvers/src/basket.rs`, 634
lines) is a **standalone** `pub mod basket`. It is **not wired into
`solve_path`** and **not reached by `ArbitrageEngine`**. Its only path to a
Python consumer is the `c_api.rs::solve_balancer_weighted_basket`
`#[pyfunction]`, whose only `src/` Python home is `degenbot.arbitrage`, whose
only production Python consumer is the `BalancerMultiTokenSolver` shell.

The Rust `basket.rs` `#[cfg(test)]` corpus is **thin**: 3 fns —
`tdd_red_three_token_basket_matches_python_oracle` (1 substantive 3-token case
on a *recorded* Python-oracle constant, multiple asserts), +
`generate_signatures_n3_has_12_valid` / `generate_signatures_n4_has_50_valid`
(signature enumeration only). There is **no N≥4 substantive math test** on the
Rust side. This is a **pre-existing coverage gap**, not a regression introduced
by the retirement (the Python `test_quantamm_basket_parity.py` also tests only
the single 3-token doctest fixture and adds only FFI-marshalling coverage, not
math coverage).

---

## 3. QuantAMM basket coupled set — the one real decision

The 4 symbols form a closed, coupled set independent of the Brent retirement:

```
BalancerMultiTokenSolver (PyO3 shell) ──wraps──▶ _ffi.solve_balancer_weighted_basket (#[pyfunction])
                                                        │
                                                        ▼
                                            basket.rs::solve_balancer_weighted  (Rust core + #[cfg(test)])
                                                        ▲
test_quantamm_basket_parity.py ──drives both halves───┘  (the only FFI-seam test for this pyfunction)
```

`BalancerMultiTokenSolver` is the **only** non-test consumer of the
`solve_balancer_weighted_basket` pyfunction (verified: no other `src/degenbot/`
importer; the `ArbitrageEngine` does not call it). ADR-013 makes `_ffi` private
to its home; the home here is `degenbot.arbitrage`.

**Disposition:** retire the *shell* (`BalancerMultiTokenSolver` + its `Solver`
ABC plumbing + its doctest), but **keep the pyfunction + `degenbot.arbitrage`
re-export**. The pyfunction is a first-class Python-reachable Rust core surface
(ADR-005 Tier-0/2), not a private helper of the shell — retiring it would strand
the basket solver behind a Python wall and delete the only FFI seam.

`test_quantamm_basket_parity.py` is the **only** test crossing the PyO3
boundary for `solve_balancer_weighted_basket` (an ADR-005 Tier-2 dual-driver
pair). It cannot retire wholesale without losing FFI-marshalling coverage.
**Slim it**: drop the `_python_solve` half (the dead-shell path); keep the
`_rust_solve` direct-to-pyfunction half as the Tier-2 FFI-seam assertion. The
existing doctest fixture suffices for the kept assertion.

---

## 4. `_solver_utils.py` generic-sim slice

The migration guide hedged "keep the `_simulate_mixed_path_int` Curve sim slice
if the Curve tests stay; retire with the oracle otherwise." Audit result:
`_simulate_mixed_path` / `_simulate_mixed_path_int` / `_simulate_path` have
**zero `src/` consumers outside `brent_solver.py`** (`to_hop_state` was the
other consumer and is already deleted; `test_curve_legacy_equivalence.py` is
gone). The hedge is moot. `_solver_utils.py` **retires wholesale** with
`BrentSolver`.

---

## 5. `types/hop_types.py` importers

Verified residual after the `LYP6L2` `to_hop_state` retirement: the only `src/`
importers of `types/hop_types.py` are inside the `arbitrage/solvers/` package
(`hop_types.py`, `_solver_utils.py`, `brent_solver.py`,
`balancer_multi_token_solver.py`) and the `types/__init__.py` re-export.
**Zero** `src/` importers outside the solvers package for any of
`ConstantProductHop` / `BoundedProductHop` / `SolidlyStableHop` /
`CurveStableswapHop` / `BalancerWeightedHop` / `BalancerStableHop` /
`V3TickRangeInfo`. `to_hop_state` retirement left **no dangling consumers**.

`types/hop_types.py` therefore retires **together with** the
`BalancerMultiTokenSolver` shell (its last `BalancerMultiTokenHop` /
`PoolInvariant` importer). The `types/__init__.py` re-export of all eight f64
hop types must be **deleted in the same slice** (public-surface shrink). The
top-level `degenbot/__init__.py` does **not** re-export any of these symbols —
no top-level churn.

---

## 6. Verdict: **GO** (with conditions)

### Unconditional GO — retire now

- `src/degenbot/arbitrage/solvers/brent_solver.py`
- `src/degenbot/arbitrage/solvers/hop_types.py` (`Solver` ABC, `SolverMethod`,
  `SolveInput`, `SolveResult`)
- `src/degenbot/arbitrage/solvers/_solver_utils.py` (the generic-sim slice has
  no surviving consumer; retires wholesale)
- `src/degenbot/types/hop_types.py` (zero external `src/` importers once the
  shell retires)
- `src/degenbot/types/__init__.py` re-export of the eight f64 hop types
- `src/degenbot/arbitrage/solvers/__init__.py` shrink (no re-exports of deleted
  symbols)
- `tests/arbitrage/test_solvers/test_solver_tagged_hops.py` (tests only the
  retiring data classes — asserts nothing about engine behavior)
- `tests/arbitrage/test_solvers/conftest.py` `ConstantProductHop` fixture (only
  consumer is the retiring test)

### Conditional GO — QuAL basket shell + pyfunction + parity test

- **Retire** `src/degenbot/arbitrage/solvers/balancer_multi_token_solver.py`
  (the delegating shell + its doctest).
- **Keep** `solve_balancer_weighted_basket` `#[pyfunction]`
  (`rust/crates/degenbot-python/src/c_api.rs`) + the `degenbot.arbitrage`
  re-export — it is the Python-reachable Rust core surface for the N-token
  basket, not a private helper of the shell.
- **Slim, do not delete** `tests/rust/test_quantamm_basket_parity.py`: drop the
  `_python_solve` (dead-shell) half; keep the `_rust_solve` direct-to-pyfunction
  half as the ADR-005 Tier-2 FFI-seam assertion on the existing doctest fixture.

### Pre-existing gap (NOT a gate on this retirement, but noted)

`basket.rs::solve_balancer_weighted` `#[cfg(test)]` corpus covers only 1
substantive 3-token math case (+ 2 signature-enumeration tests); there is no
N≥4 substantive case on either side. Track as a **separate** follow-up (expand
`basket.rs` corpus to a real multi-token regression set) — it predates and is
independent of this retirement.

### Keep (misclassified in the original candidate list)

- `tests/arbitrage/test_path/test_arbitrage_path.py` — now a
  `v3_virtual_reserves` pure-math test; the `to_hop_state`/`extract_fee`/
  `build_swap_amount` surface it once exercised was already retired under
  `LYP6L2`. **No deletion.**

---

## 7. Conditions satisfied → `LMM2NB` is unblocked

With this verdict recorded:

1. The audit task `6C32UV` deliverable (this document) is complete.
2. `LMM2NB` may proceed under **Conditional GO**: the unconditional-retire
   block (Brent + hop_types + _solver_utils + re-export shrinks +
   `test_solver_tagged_hops.py` + conftest fixture) is clear-cut deletion; the
   basket-shell retirement is gated on the slimming of
   `test_quantamm_basket_parity.py` into a direct-to-pyfunction FFI-seam test
   **in the same slice** (so the pyfunction never loses its Tier-2 seam).
3. Validation gates (`just test-rust`, `just test-python`, `just lint-python`,
   `uv run python -c 'import degenbot'`) per `LMM2NB`.

---

## Evidence provenance

- 8-file existence check + grep over `src/`, `tests/`, `examples/`, `docs/`.
- `_simulate_mixed_path_int` consumer grep (only `brent_solver.py`).
- `types/hop_types.py` importer grep (only the `arbitrage/solvers/` package +
  `types/__init__.py` re-export; zero external `src/` consumers).
- `solve_path` arm audit (`rust/crates/degenbot-solvers/src/mixed/solve.rs`).
- Basket coupling check: `solve_balancer_weighted` is **not** a `solve_path`
  arm; `c_api.rs::solve_balancer_weighted_basket` pyfunction's only `src/`
  home is `degenbot.arbitrage`, only prod consumer is `BalancerMultiTokenSolver`.
- `basket.rs` `#[cfg(test)]` corpus: 3 fns (line 549+).
