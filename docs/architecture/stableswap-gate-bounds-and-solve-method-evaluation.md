# Evaluation: stableswap gate bounds and the solve-method decision

The profit-envelope gate derives a piecewise-linear concave upper bound on a
hop's output curve and skips a path when the bound's best possible gain falls
below the profit floor. The gate started with the constant-product and
concentrated-liquidity families and was then extended to the three stableswap
families. This document records the soundness derivations, the measured
evaluations, and the drift-net methodology that settled each family's line set,
so the next engineer does not re-derive the concavity, re-attempt the rejected
closed-form search, or re-measure the Curve counterexamples. It is the durable
record. The per-task working notes that produced it were removed in the
`docs/ergo-results/` cleanup and are recoverable from git history.

## Verdict (TL;DR)

- **Solidly/Aerodrome/Camelot stable** contributes an entry tangent beside its
  reserve cap: the invariant's output curve is concave in the input, and the
  rise carries an additive allowance for the leaf's integer rounding.
- **The `t`-parametrized simulation-free solve** for a one-Solidly-hop mixed
  path was designed, implemented, measured, and **rejected**: its closed-form
  probe costs more than the leaf's own Newton solve, and it ran **81× slower**
  in wall time for a few wei of extra profit on flat plateaus. The
  golden-section arm is effectively optimal for this family (worst observed
  residual 1 wei).
- **Curve stableswap** keeps the reserve cap alone: its leaf's out-curve was
  **measured non-concave**, so an entry tangent there would under-cut a real
  leaf and skip a profitable path.
- **Balancer V2 stable** contributes an entry tangent wherever an exact
  closed-form curvature test holds, with a **derived zero allowance** (every
  leaf rounding is pool-favoring), validated at a measured margin of 1 native
  unit.
- **The drift net** — pinned pre-refactor snapshots verified against the
  pre-refactor tree in an isolated worktree — is the pattern for
  behavior-identical refactors.

## 1. Solidly-stable: the output curve is concave, so the gate carries its entry tangent

### The derivation

The deployed invariant in the 1e18-scaled domain is

```text
f(x0, y) = x0·y·(x0² + y²) = K,   K = f(Ra, Rb) at the pre-trade scaled reserves
```

An exact-in swap sets `x0 = Ra + m` (`m` the post-fee scaled input) and the
output is `Rb − y`. Implicit differentiation gives the marginal output rate

```text
dy/dx0 = −y·(3x0² + y²) / (x0·(x0² + 3y²))
```

Writing `t = y/x0`, that rate is `price(x0) = t·(3 + t²)/(1 + 3t²)`. On the
level curve `x0⁴·(t + t³) = K`, so `t` strictly decreases as `x0` (the input)
grows. And

```text
d/dt[price] = 3·(t² − 1)² / (1 + 3t²)² ≥ 0
```

so the rate is non-increasing in the input. The output curve is therefore
**concave**, and a concave curve through the origin lies below its entry
tangent (`O(x) ≤ O'(0)·x`): the entry slope is a sound global rise line. The
same `[rise, flat cap]` shape is used, with the flat native reserve cap beside
the rise so the point-wise minimum stays a rigorous upper bound.

The entry slope in **native** token units folds in the leaf's scaling and fee:

```text
price_entry = Rb·(3·Ra² + Rb²) / (Ra·(Ra² + 3·Rb²))
slope       = price_entry · γ/fee_denom · decimals_out/decimals_in,  γ = fee_denom − fee_numer
```

### The fee-floor slack dominates the naive allowance

The leaf's fee floor retains
`x − ⌊x·fee_numer/fee_denom⌋ ≥ (1 − f)·x`, so the scaled post-fee input can
exceed its continuous value by up to `ONE/decimals_in`, worth up to
`price_entry·decimals_out/decimals_in` native output. On a fair (6,18) pool
that is ~1e12 native units — measured against the real leaf as
`max_x (leaf(x) − slope·x) = 9.3e11` at slope 9.997e11. The task brief's naive
allowance of `ceil(4·decimals_out/ONE)` (i.e. 4) is therefore short by ~1e12;
the implemented allowance carries **both** terms:

```text
a = ceil(price_entry·decimals_out/decimals_in) + ceil(4·decimals_out/ONE)
```

composed as `A = a·C` so the line evaluates to `a + ceil(b·x/c)`. The second
term absorbs the invariant-root floor (the leaf returns the floor of the
invariant root, and the Camelot convergence probe can stop one scaled unit
high — a few scaled units, each `decimals_out/ONE` native). With both terms the
dominance proptest shows zero under-cuts over its guarded regime.

### The faithful-arithmetic window guard

The gate sweeps a hop's lines over `x ∈ [0, reserve_out]`, but the leaf's
Newton runs at the **post-trade** scale `x0 = Ra + x·ONE/decimals_in`, which can
exceed the pre-trade `Ra` by `reserve_out·ONE/decimals_in` scaled units. There
the leaf's `calc_d` products wrap `u256`, and a wrapped Newton denominator
returns a garbage root that can inflate the leaf's output **above** the tangent.
The rise is therefore **dropped** (cap-only fallback — never looser than the
plain cap, never `None`, never `Envelope::Unsupported` for a non-degenerate hop)
unless the leaf's own arithmetic is exact across the whole evaluation window:

- scaled reserves `≥ 2^60` (below this the invariant helpers' `ONE` divisions
  are not faithful);
- the pre-trade `calc_k` product cannot wrap `u256`;
- the leaf's input upscaling `reserve_out·ONE` fits `u256`;
- every post-trade `calc_d` term at `x0_max = Ra + reserve_out·ONE/decimals_in`,
  `y ≤ Rb`, stays inside `u256`.

The guard is pinned on the **derived line-set shape**: a balanced 1e24/1e24
18-decimal pool keeps `[rise, flat]` (`lines[0].b > 0`), while a 6-decimal
in-reserve of ~1e12 native with an 18-decimal out-reserve of ~1e29 native
(`x0_max ~ 1e41`, `x0_max² ~ 1e82 > u256::MAX ~ 1.16e77`) derives the flat cap
alone.

### Evidence

- **Dominance proptest against the real leaves** (`calc_exact_in_stable_solidly`
  / `_camelot`): ~250k point-wise dominance assertions over 9,600 randomized
  shapes (reserves 1e6..1e30, imbalance ratio to 1e12, decimals pairs
  `(18,18)/(6,18)/(18,6)/(6,6)`, fees `{0, 3/10000, 30/10000}`, both `token_in`,
  Aerodrome + Camelot). Per case: `rise_active` 1,920–3,124 probes,
  `guard_fired_cap_only` 341–362 shapes (~89% of shapes), `n_asserts`
  ~9.6k–11.8k. Non-vacuity floors (`shapes ≥ 300`, `asserts ≥ 2000`,
  `rise_active ≥ 200`, `guard_fired > 0`) are all cleared.
- **Skip-count delta** on a synthesized 64-path Solidly corpus: the pre-solve
  skip count moves **0 → 32** (the unprofitable directions no longer pay a
  golden-section solve).
- The committed mixed corpora carry **no** Solidly leg (`heavy_mixed_solve_captures`
  = 369 paths, V2/concentrated liquidity only; `live_gatebursts_mixed` = 24 paths, concentrated liquidity only), so the
  Solidly coverage is synthesized in-test.

### Decision and what would change it

**Decision:** a Solidly/Aerodrome/Camelot stable hop maps to
`HopMath::SolidlyStable` — entry-tangent rise beside the native reserve cap,
with the two-term allowance and the window guard.

**Revisit if:** the leaf's integer rounding changes shape (e.g. a Camelot
rounding variant, which the eligibility test currently excludes), or the
faithful-arithmetic guard starts dropping the rise on pool regimes where it
would tighten, or the invariant itself changes.

## 2. The `t`-parametrized Solidly solve: designed, measured, rejected

### The design

`t = y/x0` reparametrizes the search onto the Solidly hop's post-trade reserve
ratio. The level curve `x0⁴·(t + t³) = K` makes every quantity closed-form in
`t`: `x0⁴ = K/(t + t³)` (two integer square roots), `m_scaled = x0 − Ra`,
`y = t·x0`, `n_scaled = Rb − y`. The prefix/suffix V2 hops compose to exact
integer Möbius maps (in a U2048 accumulator so a long composition cannot wrap),
exactly invertible. The smooth model's optimum is found by a coarse geometric
scan plus a golden section, costing **zero path simulations**; a 1024-sample
dense scan of the near-optimal interval then refines on the leaf's own
simulation-free arithmetic. Eligibility is narrow: exactly one Solidly hop, all
other hops V2, the Solidly/Aerodrome invariant (not Camelot's rounding variant),
and the faithful-arithmetic predicate.

### The measured regression

`cargo bench -p degenbot-solvers --bench solidly_t`, 18-path corpus:

| arm | wall time (18 paths) |
| --- | --- |
| `closed_form_t_search` | 44.3 ms |
| `golden_section` | 542 µs |

An **81×** wall-time regression, reproduced first-hand by the reviewer (the
implementing agent's own bench recorded 45.0 ms vs 0.537 ms — ~80×).

### The root cause

Every closed-form probe extracts a fourth root via two `isqrt_u2048` calls plus
U2048 Möbius arithmetic on ~1000-bit intermediates; that costs **more** than the
leaf's own U256 Newton solve. **Zero-simulation probes are not cheap probes.**
Even with the 1024-sample refine removed, the model search alone (a 97-point
geometric scan + ~192-probe golden section + ~192-probe edge bisection, ~670
big-integer probes) sits near 30× the golden arm's entire budget.

### The quality delta is economically nil

Parity on the synthesized 288-path corpus (4 shapes × 3 decimal scales × 3
Solidly fees × 2 V2 fees × 4 basis-point imbalances):
`288 paths, 143 profitable, 143 fast-path, 50 equal, 93 strictly better, 0 regressions`.
The 93 "strictly better" paths beat the golden arm by a few **wei** on flat
stable-pool plateaus (the golden arm's 25-iteration bracket midpoint plus ±3
sweep lands on a jagged point a few wei below the discrete max). The `t`-arm
spends 7 path simulations against the golden arm's 36, but 1e-18 of a token is
not a product feature.

### Verdict

The golden-section arm is effectively optimal for this family: the worst
observed residual is **1 wei** on flat plateaus. The chunk's lasting deliverable
is the evidence, salvaged as a permanent golden-arm regression test: the
288-path synthetic corpus, a deterministic 32-path sample, ±8,000 native input
units swept at unit resolution (16,001 candidates per path, 512,032 simulations
across the sample), tolerance **8 wei**, worst observed residual **1 wei**, and
`PROFITABLE_SIZE = 143` pinned as a canary. The production changes were
reverted (the crate is byte-identical to its pre-change state); the rejected
patch's reference copy is session-scratch (`.scratch/sr2-rejected.patch` and
`.scratch/sr2-solidly_t.rs`, not committed), and the design record is preserved
in the per-task result doc titled "simulation-free `t`-parametrized solve for
one-Solidly-hop mixed paths" (removed in the `docs/ergo-results/` cleanup;
recoverable from git history).

### Decision and what would change it

**Decision:** keep the shared golden-section arm; do not reintroduce the
`t`-search.

**Revisit if:** big-integer root extraction gets materially cheaper (a U2048
fourth-root probe costing less than the U256 Newton solve it replaces), or a
pool regime appears where the profit plateau is **not** flat (golden's few-wei
residual would then grow), or a probe representation is found that does not cost
at least as much as the leaf's own invariant solve.

## 3. Curve stableswap: measured non-concavity keeps the reserve cap

### The counterexamples (pinned, deterministic)

Both tests inline the entry-tangent derivation the Solidly/Balancer arms use
(the same closed-form marginal rate, fee/scaling conversion, and ceil rounding)
and assert the **real** `stableswap_get_y` leaf exceeds it.

| case | y / d variant | measured overshoot | at input |
| --- | --- | --- | --- |
| Standard/Standard, co-coin 1e19, A = 10,000 | Standard / Standard | **1** native unit | x ≈ 9.88e11 |
| Variant1 / Standard, co-coin 1, A = 10 | Variant1 / Standard | **~9.799e23** native units (≈ the pool's whole output reserve) | x = 1 |

### Why this is unsound, not merely loose

The leaf's y-iteration floors (`div`) and can settle a unit below the
invariant's root, so `dy_raw = xp_out − y − 1` reports **more** than the ideal
curve. An entry tangent that under-cuts a real leaf would skip a profitable
path. The gate's soundness is absolute, not proportional: a **single** native
unit of overshoot is already an unsound skip, so the family keeps the sound
reserve cap alone.

### The corrected magnitudes

An earlier draft of the code docs claimed "hundreds" of native units for
Standard/Standard and "1e5+" for a `Variant1`/`VariantAlpha` pairing. The pinned
tests are the authority and the docs carry the corrected numbers above:

- The **Standard y-iteration** (the coherent Standard/Standard pool) is
  essentially concave — the tangent dominates except for a 1–2 native-unit
  rounding overshoot in extreme configurations (top-of-range amplification,
  co-coin 1e19+).
- **Variant1 / VariantAlpha** (both omit the `A_PRECISION` divisor) is
  consistent and therefore concave — it never overshoots in the swept matrix.
  The inherited doc named the wrong pairing.
- The large non-concavity lives in the **`Variant0`/`Variant1` y-iterations
  paired with a `Standard` D-step**. `project_curve` reads `y_variant` and
  `d_variant` independently from separate identity bytes, so that pairing is
  reachable, and there the y-root is not the invariant's and the leaf overshoots
  by up to the pool's entire output reserve at a 1-wei input on a nearly-empty
  co-coin.

### Decision and what would change it

**Decision:** a Curve stableswap hop maps to `HopMath::CurveStableswap`, whose
derived line set is the native reserve cap **alone** (the hop still carries its
full resolve-time state so the family is gated through its own variant rather
than a bare cap).

**Revisit if:** the gate can restrict Curve to the coherent pairings
(Standard/Standard, or Variant1/VariantAlpha) so a guarded entry tangent becomes
sound, or the leaf's y-iteration stops flooring below the invariant root.

## 4. Balancer V2 stable: an exact curvature test and a zero-allowance tangent

### The exact closed-form curvature test

Both n-token StableSwap invariants the gate covers reduce, for a fixed invariant
`D` and fixed co-balances, to the same implicit out-curve
`y² + k·y = C/x` with `k = x + T` (`T` constant in the input-side balance `x`).
Writing `m` for the invariant's amplification coefficient and
`W = m·(y + k)`, the marginal rate is

```text
s(x) = y·(m·x + W) / (x·(m·y + W))
ds/dx = −2·y·G / (x²·(2y + k)²),   G = (x + y + k)² + (y − x)·(y + k)
```

The curve is concave at the entry exactly when `2y + k > 0` and `G ≥ 0`. The
sign of `G` is delicate (for an extreme imbalance the two terms nearly cancel),
so it is evaluated **exactly** in I512 with `k` carried as the rational
`k_num/k_den` (no division happens). An I512 overflow, a failed test, or a
possible wrap of the leaf's own arithmetic across the evaluation window drops
the rise (cap-only — never looser than the plain cap).

### The entry marginal rate

The invariant is `An·Σx + D = An·D + D^(n+1)/(n^n·Πx)` in the leaf's
`An = amp·n/AMP_PRECISION` convention. An exact-in swap adds the post-fee input
to the input balance and re-solves the output balance against the **same** `D`,
so the out-curve is that invariant's level curve and its entry marginal rate is
closed-form (no invariant solve):

```text
W = D^(n+1)/(n^n·Πx) = m·(Σx − D) + D,   u = x_in·x_out
−dx_out/dx_in = (m·u + W·x_out) / (m·u + W·x_in)
native slope  = rate · sf_in·(ONE − swap_fee) / (ONE·sf_out)
```

the leaf upscaling the input by `sf_in`, taking `mul_up(·, swap_fee)` off it,
and downscaling the output by `sf_out`.

### The zero allowance is derived, not assumed

The rise carries `a = 0` because every rounding in the Balancer leaf is
**pool-favoring**: the fee is taken with `mul_up` (retained input at most the
continuous `x·(1 − f)`), the output Newton divides with `div_up` (its root at
least the invariant's), `calc_out_given_in` subtracts a further `1`, and the
final downscale floors. The leaf's integer output is therefore never above the
ideal curve the tangent bounds, so nothing needs absorbing. The dominance
proptest against the **real** `calc_out_given_in` leaf (with
`calculate_invariant` / `calculate_invariant_deployed` for `D`) confirms it:
~23k assertions on 320+ randomized shapes (2–4 tokens, `amp = A·1000` for
A in 10..=5000, upscaled balances 1e18..1e24, imbalance to 1e4 either direction,
swap fees `{0, 1e15, 1e16, 1e17}`, the scaling-factor pairs `(1,1)/(1e12,1)/(1,1e12)`,
both invariant entry points), with a **minimum margin of exactly 1 native unit**
— never 0, never negative. Per case: `rise_active` 4,000–5,800 probes,
`guard_fired_cap_only` 216–259 shapes.

The guard/shape test: a 10× imbalanced 2-token pool at 1e21 with A = 10 keeps
the rise (the unit-input bound is strictly below the reserve cap); the same 10×
imbalance at 1e30/1e29 trips the faithful-arithmetic window guard (the deployed
numerator's `D²`/`D·P_D` products would wrap `u256` across the post-trade
window) and derives the flat cap alone.

### Hot-path note

`HopMath::BalancerStable` (and `HopMath::CurveStableswap`) carry their balance
vectors as borrowed `&[U256]` slices, so the pre-solve gate path allocates
nothing per hop per candidate path.

### Decision and what would change it

**Decision:** a Balancer V2 stable hop maps to `HopMath::BalancerStable` — the
entry tangent wherever the curvature test holds, beside the native reserve cap,
with a zero allowance.

**Revisit if:** the leaf's rounding stops being pool-favoring (an allowance
would then be required), the invariant changes, or the window guard's bound
moves.

## 5. The drift net: pinned pre-refactor snapshots for behavior-identical refactors

### The methodology

For a behavior-identical refactor, capture the exact `(optimal_input, profit)`
literals from the **pre-refactor** code, verify those snapshots against the
pre-refactor tree in an **isolated worktree** (so they pin the old behavior
rather than whatever the new code produces), then assert equality after the
refactor. The snapshots — not the new code's output — are the oracle.

### The evidence

The four stable-family solve arms (Solidly, Balancer-weighted, Balancer-stable,
Curve) each carried a private copy of the same search — the V2-equivalent
Möbius precheck, the ±5× bracket around the model optimum, the 25-iteration
phi-approximated golden loop, and a ±3 integer verification. They collapsed to
family-specific closures over one shared golden-section template, preserving two
quietly-accumulated probe-order differences verbatim (ascending
`center−3..=center+3` for Solidly/weighted, outward-from-best for stable/Curve)
via an explicit sweep enum, and keeping the `hops < 2` guard on the two arms
that had it. Net −254 lines.

The drift net: each arm is pinned by a synthetic corpus (24 paths spanning
bases, fees, imbalances, and hop placement), 4 corpora × 24 paths = **96 pinned
`(optimal_input, profit)` pairs**. Pre-extraction 4/4 PASS; post-extraction 4/4
PASS — 96/96 pairs byte-identical, 0 wei drift. The committed captured mixed
corpora carry no Solidly/Balancer/Curve legs, so a synthetic corpus is the net.

The salvaged brute-force corpus of §2 is the same idea applied to the golden
arm's **optimality** rather than to a refactor: a fine brute-force sweep pins
the arm's answer against the discrete maximum, and `PROFITABLE_SIZE = 143` pins
its coverage of the matrix.

### Decision and what would change it

**Decision:** any future behavior-identical refactor of the solve arms lands
behind a pinned pre-refactor snapshot corpus, verified against the pre-refactor
tree before the refactor.

**Revisit if:** the committed captured corpora gain real family legs (the
synthetic corpora can then be supplemented by captures), or a change is **not**
behavior-identical (the drift net does not apply — pin the intended delta
explicitly instead).

## 6. Cross-references

- Derivation docs in code:
  `rust/crates/engine/degenbot-solvers/src/profit_envelope/derive.rs`
  (the `SolidlyStable` / `BalancerStable` / `CurveStableswap` variant docs,
  `solidly_stable_lines` / `solidly_stable_rise`, `stableswap_concave_at`,
  `balancer_stable_lines`, `curve_stableswap_lines`).
- Module docs: `rust/crates/engine/degenbot-solvers/src/profit_envelope/mod.rs`.
- Shared solve template: `rust/crates/engine/degenbot-solvers/src/mixed/solve.rs`
  (`solve_golden_section_path_int`).
- Glossary: `GLOSSARY.md`, "Profit envelope".
- Related evaluations: [combinatorial ending-range enumeration in the Möbius
  V3/concentrated-liquidity solver](mobius_v3_ending_range_enumeration_evaluation.md) (the concentrated-liquidity
  concavity / active-set evaluation), [hull-edge seed
  evaluation](hull-edge-seed-evaluation.md).
- Tests: `rust/crates/engine/degenbot-solvers/tests/profit_envelope_tests.rs`,
  `tests/solidly_golden_parity.rs`, `tests/stable_golden_drift_net.rs`.
