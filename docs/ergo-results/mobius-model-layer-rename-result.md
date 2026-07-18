# Möbius solver exactness-naming asymmetry resolution

## What was the asymmetry

The integer Möbius solver module (`rust/crates/degenbot-bot/src/solvers/
mobius_int_exact.rs`) had two public symbols both calling themselves "exact,"
but each exact at a different layer:

| Symbol                                              | Layer of exactness                                        |
|-----------------------------------------------------|-----------------------------------------------------------|
| `compute_exact_optimal_input_from_coeffs(coeffs)`   | Exact at the **Möbius-model layer** — the analytic argmax `x* = (√(K·M) − M) / N` of the smooth rational envelope `l(x) = K·x / (M + N·x)` (treats per-hop swap `y = γ·s·x / (D·r + γ·x)` as if its floor division were mathematical division) |
| `exact_mobius_solve(hops)`                          | Exact at the **discrete EVM-simulation layer** — the true argmax of `int_simulate_path(x) − x` over the floor-divided per-hop swap chain |

Because both names contained "exact," a reader could mistake the closed-form
call for "the exact optimum of the swap chain" (it isn't — it is the optimum
of the smooth rational envelope, which approximates the staircase the swap
chain actually follows). The ±2 sweep in `exact_mobius_solve` is the bridge
between the two: anchored at the model-optimum `x*` + corrected for the
floor-division staircase jog.

## Resolution

Rename `compute_exact_optimal_input_from_coeffs` →
`compute_mobius_model_optimal_input`. The new name says exactly what the
result is: the analytic argmax of the **Möbius model envelope** (not the
discrete on-chain profit optimum — that lives in `exact_mobius_solve`).

The module-level qualifier in `mobius_int_exact` (the module name) was
preserved as a discipline marker distinguishing the integer pathway (no
floats — pure `U512` + `isqrt_u512` + floor-division) from the now-deleted
f64 recurrence module. The per-symbol rename makes the layer of exactness
explicit at each public candidate.

### Doc-rewrite coverage

- Module doc: added a "Two layers of 'exact'" section explaining the
  model-layer vs discrete-EVM-sim-layer distinction with cross-references
  to [`compute_mobius_model_optimal_input`] and [`exact_mobius_solve`].
- Renamed fn's doc-comment: rewritten to state it returns the **analytic
  argmax of the smooth Möbius envelope** over the reals, NOT the discrete
  EVM-sim optimum; points at [`exact_mobius_solve`] as the bridge.
- `exact_mobius_solve`'s doc-comment: rewritten so the algorithm steps
  distinguish "compute the model-optimum anchor" from "EVM-simulate at
  `x*` ± 2 to find the discrete argmax" and so the result is labeled
  "discrete-EVM-sim-exact".
- `ExactMobiusResult` struct doc: rewritten to clarify it is the
  `exact_mobius_solve` *result* (discrete-EVM-sim-exact optimum — not the
  closed-form model-optimum), with cross-references to both fns for the
  two-layer narrative.

### Callsite updates

- `solvers/mobius_int_exact.rs:89` — internal anchor call inside
  `exact_mobius_solve`.
- `solvers/mobius_v3_int.rs:1296` — mixed V2-V3 integer solver (uses the
  model-optimum output as an anchor for its own EVM-sim sweep).
- `solvers/arb_engine/solver_dispatch.rs:367+389` — Solidly-bracketed
  dispatcher (uses the model-optimum output to narrow a golden-section
  search bracket `[x*/5, 5·x*]`).
- No standalone-Rust consumer (`standalone_consumer.rs`) reference (the
  example interacts with the solver via `exact_mobius_solve`, not the
  closed-form fn directly).
- No PyO3 seam (`rust/crates/degenbot-python/src/...`) reference (the
  Python wrapper exposes the higher-level `solve_*` engine fns, not the
  closed-form helper).
- No test directly names the closed-form fn (it is tested indirectly
  through `exact_mobius_solve`'s `test_exact_solve_best_in_neighborhood`,
  whose pin of "no `±3` neighbor beats the chosen `x_opt`" exercises the
  composition of the closed-form anchor and the ±2 discrete sweep).

### Acknowledged but deliberately not changed

- `ExactMobiusResult::used_closed_form: bool`: a related-but-separate
  misnomer (it names whether the result came from the post-anchor ±2
  search path or the micro-profit fallback at the `x* = 0` corner). The
  field name reads as "the closed form was the answer" when in fact it
  flags the path that anchors *to* the closed form. Fixing this requires
  renaming `used_closed_form` → something like `anchored_at_closed_form`
  (or restructuring the flag as a derivation-path enum). Out of scope for
  instruction #2's "resolve the asymmetry" (which was about the
  function-name collision), and out of scope for the WOYYS2 epic. Noted as
  a follow-up the next solver-naming pass can address.

## Validation

- `cargo test -p degenbot-bot --lib`: 375 passed, 0 failed (no behavioral
  change — purely rename + doc-rewrite; the renamed fn's body and call
  semantics are byte-identical).
- `cargo build -p degenbot-bot -p degenbot_rs`: clean.
- `cargo build -p degenbot --example standalone_consumer`: clean.
- `cargo fmt -p degenbot-bot --check`: clean.
- `cargo clippy -p degenbot-bot --all-targets`: clean.
- `just check-no-pyo3-in-cores`: green (the rename is core-only).
