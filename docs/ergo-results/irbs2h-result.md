# IRBS2H — entry-tangent gate bounds for Balancer-stable and Curve-stableswap (fix round)



## What this round is

The previous one-shot agent implemented the src-side half and settled early with a

corrupted sign-off. The manager accepted the DESIGN (Balancer-stable carries an

entry-tangent rise guarded by an exact curvature test; Curve stays on the reserve cap

by a measured non-concavity finding) and dispatched this fix round. The dirty tree was

inherited AS-IS; the four findings below were fixed without redesigning.



## Fixes landed (vs inherited)



- **FIX 1 — clippy.** The tree did not compile under `--deny warnings` (4 errors).

  - `derive.rs` `stableswap_concave_at`: `br_num`/`b_num` renamed to descriptive math

    names (`two_y_plus_k`, `x_plus_y_plus_k`, `y_times_y_plus_k`, `curvature_term`);

    the doc comment now spells the cleared expressions.
  - `derive.rs` `balancer_stable_rise`: five single-char bindings (`w`,`d`,`n`,`b`,`c`)

    reduced to the two that mirror `Line`'s own fields — `w` → `w_scaled`,

    `d`/`n` → `invariant_wide`/`n_tokens_wide`.
  - `mixed/solve.rs` `solve_path_with_min_profit` (110 lines): the per-hop

    `ResolvedHop -> HopMath` mapping is lifted into a private `hop_math_for` helper.
  - `derive.rs` `hop_lines_and_cap_cached` (122 lines): the long inline `Weighted` arm

    is extracted into a `weighted_lines` helper. No `#[expect]` was added for either

    `too_many_lines`; both were fixed by extraction, per repo convention.
- **FIX 2 — hot-path allocation.** `HopMath::BalancerStable { balances }` and

  `HopMath::CurveStableswap { xp }` are now borrowed `&'a [U256]` slices;

  `hop_math_for` maps `state.balances.as_slice()` / `state.xp.as_slice()`. The gate

  path allocates nothing per hop per candidate path (the removed `ReserveCap` mapping

  allocated nothing; `Weighted` carries scalars only). `capture.rs`/`gate.rs`

  diagnostic arms use `.len()` through the slice unchanged.
- **FIX 3 — tests.** `tests/profit_envelope_tests.rs` was unmodified; the src docs

  referenced it twice (dangling). Added a Balancer-stable dominance proptest, pinned

  Curve non-concavity counterexamples, and a Balancer guard/shape test.
- **FIX 4 — GLOSSARY.md.** The "Profit envelope" entry now states the landed reality

  as one continuous block: Balancer-stable contributes its entry tangent wherever the

  closed-form curvature test holds (beside its reserve cap); the Curve family keeps the

  cap alone because its leaf's out-curve was measured non-concave.



## Proptest matrix (Balancer-stable dominance)



`balancer_stable_entry_tangent_dominates_real_leaf`, oracle = the REAL

`balancer::stable_math::calc_out_given_in` with `calculate_invariant` /

`calculate_invariant_deployed` for D (mirroring `simulate_balancer_stable_hop`).

Matrix: token counts 2..=4, `amp = A·1000` for A in 10..=5000, upscaled balances

log-uniform `10^18..10^24` with imbalance ratios up to `1e4` in either direction, swap

fees {0, 1e15, 1e16, 1e17}, the three scaling-factor pairs (1,1), (1e12,1), (1,1e12),

token index pairs, and both invariant entry points. Dense log grid from the reserve cap

down ~8 decades, 40 steps × 2 probes.

Representative per-case counters (24 proptest cases):

```

balancer_stable proptest: shapes=320..342  asserts=22_600..24_900

                          rise_active=4_000..5_800  guard_fired_cap_only=216..259

                          leaf_wrapped=0  leaf_undefined=0  min_margin=1

```

Assertions: `shapes >= 300`, `asserts >= 2_000`, `rise_active >= 200`,

`guard_fired_cap_only > 0` — all non-vacuity floors are cleared by a wide margin.



### Zero-allowance derivation: VALIDATED

The inherited design derives `a = 0` for the Balancer rise (every leaf rounding is

pool-favoring: `mul_up` fee, `div_up` Newton root, the `-1` subtraction, the floored

downscale). The proptest is the authority and **confirms it**: `bound >= leaf` on every

one of ~23 000 assertions across the matrix, with a **minimum margin of exactly 1 native

unit** — never 0, never negative. No slack constant was needed, and none was added. The

src doc's claim ("measured margin ≥ 1 native unit on the proptest matrix") is now backed

by the test that makes it, not a dangling reference.



## Curve counterexamples — measured vs the inherited doc's claims



The inherited src doc claimed: a coherent **Standard/Standard** pool with a nearly-empty

co-coin overshoots the entry tangent by **hundreds** of native units, and a

**Variant1/VariantAlpha** case reaches **1e5+**. The pinned tests measure the truth and

the src doc was CORRECTED to it:

| case | y/d variant | measured overshoot | at input | src doc (before) |

| --- | --- | --- | --- | --- |

| Standard/Standard, co-coin `1e19`, A=10_000 | Standard / Standard | **1** native unit | x≈9.88e11 | "hundreds" |

| Variant1 / Standard, co-coin `1`, A=10 | Variant1 / Standard | **~9.799e23** native units (≈ the pool's whole output reserve) | x=1 | "Variant1/VariantAlpha 1e5+" |

Measured facts that drive the correction:

- The **Standard y-iteration** (the coherent Standard/Standard pool) is essentially

  concave — the tangent dominates except for a 1–2 native-unit rounding overshoot in

  extreme configurations (top-of-range amplification, co-coin `1e19`+). A single unit is

  still an unsound skip, so it is a valid soundness counterexample, but it is NOT

  "hundreds".
- **Variant1 / VariantAlpha** (both omit the `A_PRECISION` divisor) is CONSISTENT and

  therefore CONCAVE — it never overshoots in the swept matrix. The inherited doc named

  the wrong pairing.
- The large non-concavity lives in the **`Variant0`/`Variant1` y-iterations paired with a

  `Standard` D-step** — a pairing `project_curve` reads INDEPENDENTLY from the pool

  identity (`y_variant` and `d_variant` are decoded from separate identity bytes and

  `amp = a_coefficient * a_precision` always). There the y-root is not the invariant's and

  the leaf overshoots the entry tangent by up to the pool's entire output reserve at a

  1-wei input on a nearly-empty co-coin.

Both counterexample tests inline the entry-tangent derivation the Solidly/Balancer arms

use (same closed-form marginal rate `(m·u + P·x_out)/(m·u + P·x_in)`, same

fee/scaling conversion, ceil-rounded like `Line::eval`) and assert the REAL

`stableswap_get_y` leaf exceeds it. The design conclusion is unchanged and remains sound:

Curve stays on the reserve cap.



## Balancer guard/shape test



`balancer_stable_curvature_guard_shape`: a 10× imbalanced 2-token pool at `10^21` with

A=10 keeps the rise (unit-input bound strictly below the reserve cap); the same 10×

imbalance at `10^30`/`10^29` trips the faithful-arithmetic window guard and derives the

flat cap alone (unit-input bound exactly the reserve).



## Gates



- `cargo clippy --locked -p degenbot-solvers --all-targets --manifest-path rust/Cargo.toml -- --deny warnings` — **PASS** (0 warnings).
- `cargo fmt --manifest-path rust/Cargo.toml --all -- --check` — **PASS** (no diffs).
- `just test-rust-nextest -p degenbot-solvers` — **PENDING-foreign**. The workspace recipe fails to BUILD on a FOREIGN crate: `rust/crates/shells/degenbot-python/src/pool/mod.rs:677,745` call `run_pool_update_injected` with 9 args while the foreign stream's commit `989e30e98` ("feat(pool): decouple the eth_getLogs block-range clamp from --chunk") made it take 10 (missing `max_block_range`). That file is clean vs HEAD, so the foreign stream committed a broken state, not a dirty tree of mine. Documented deviation: the crate was verified directly with `cargo nextest run --locked --manifest-path rust/Cargo.toml -p degenbot-solvers` — **237 passed, 3 skipped, 0 failed**. The known PRE-EXISTING `degenbot-config::no_stray_env_reads::no_stray_python_env_reads` failure is in another crate and was not run.



## Foreign activity



- Committed foreign breakage: `989e30e98` (degenbot-python `run_pool_update_injected`

  arity) — the workspace recipe cannot build until the foreign stream fixes it.
- No unrelated tracked file is dirty in this workspace; `git status` shows only the six

  files this task owns (plus the sibling task's untracked artifacts).
- Left untouched: the sibling task's untracked `rust/crates/engine/degenbot-solvers/tests/solidly_golden_parity.rs`

  and `docs/ergo-results/{gmdx5v,ywhy7s}-result.md`.



## Files changed



- `rust/crates/engine/degenbot-solvers/src/profit_envelope/derive.rs` — FIX 1 (renames,

  `weighted_lines` extraction), FIX 2 (borrowed slices), corrected Curve docs.
- `rust/crates/engine/degenbot-solvers/src/mixed/solve.rs` — FIX 1 (`hop_math_for`),

  FIX 2 (`as_slice()`).
- `rust/crates/engine/degenbot-solvers/src/profit_envelope/{capture,gate}.rs` — diagnostic

  arms (inherited; unchanged this round beyond the slice-type compatibility).
- `rust/crates/engine/degenbot-solvers/tests/profit_envelope_tests.rs` — FIX 3 (new tests).
- `GLOSSARY.md` — FIX 4.
- `docs/ergo-results/irbs2h-result.md` (this file).

Tree left dirty for review. No commit, no push.
