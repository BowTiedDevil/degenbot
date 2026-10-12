# MGGILN — one shrinking dominance harness and set-based family dispatch

Status: **DONE.** Tree left dirty for review; nothing committed. Scoped gates
green, and the whole-workspace ladder is green too (4794/4794). No PENDING items.

Epic 2AAFZ7 (SC3, the final chunk). Two behavior-identical refactors:

1. the two hand-rolled proptest harnesses in
   `rust/crates/engine/degenbot-solvers/tests/profit_envelope_tests.rs` became
   ONE generic, `Strategy`-driven dominance harness (failures now SHRINK);
2. the eight boolean flags + if/else ladder in `solve_path_inner`
   (`rust/crates/engine/degenbot-solvers/src/mixed/solve.rs`) became a set-based
   family dispatch.

Files touched (only these two):

- `rust/crates/engine/degenbot-solvers/tests/profit_envelope_tests.rs`
- `rust/crates/engine/degenbot-solvers/src/mixed/solve.rs`

---

## 1. The generic dominance harness

The harness owns everything family-agnostic; a family supplies only its shape
strategy, its hop view, and its leaf oracle.

- `DominanceCounters` — the run-wide counters (`shapes`, `asserts`,
  `rise_active`, `guard_fired`, `leaf_wrapped`, `leaf_undefined`, `min_margin`).
- `trait DominanceFamily` — `type Shape: Debug`; `const NAME`;
  `fn strategy() -> BoxedStrategy<Shape>`; `fn hop(&Shape) -> HopMath<'_>`;
  `fn native_out_reserve(&Shape) -> U256`;
  `fn leaf_output(&Shape, U256) -> Option<U256>`; and
  `fn shape_leaf_undefined(&Shape) -> bool` (default `false`).
- `fn dominance_case<F>` — the unit-input guard check (a cap-only shape must
  bound to the flat cap), the dense 40-step log grid (`×464/1000` per step, both
  `x` and `x+1` probes), the point-wise dominance assertion against the REAL
  leaf, the wrapped/undefined probe skips, and the counter updates. It returns
  `Result<(), TestCaseError>`, so `prop_assert!` hands the failing shape to
  proptest's shrinker.
- `fn run_dominance_suite<F>` — builds a `TestRunner`, runs the family strategy
  for `DOMINANCE_CASES` cases, accumulates counters across the WHOLE run, prints
  the telemetry line, and asserts the non-vacuity floors.
- `fn pow10_u256(exp)` — the single decade helper (the old `solidly_pow10` /
  `balancer_pow10` pair was the same function twice; `balancer_pow10`'s other
  callers were repointed at it).

Per-family strategies preserve the OLD LCG matrices exactly:

- **Solidly** (`solidly_shape_strategy`, `prop_compose!`): reserve-0 exponent
  `e0 ∈ 6..=30`, mantissa `m0 ∈ 100..=90099`, imbalance exponent
  `ratio_exp ∈ 0..=12` with direction `up`, mantissa `m1`, the four decimals
  pairs (`18/18, 6/18, 18/6, 6/6`), the three fee pairs (`0%, 0.03%, 0.3%`),
  `token_in ∈ {0,1}`, and the Aerodrome/Camelot leaf choice — the same
  decimals/fees/imbalance/variant matrices the LCG drew.
- **Balancer** (`balancer_shape_strategy`): token count `n ∈ 2..=4`,
  `base_exp ∈ 18..=24`, `ratio_exp ∈ 0..=4` with direction `up`, per-coin
  mantissas `1..=9999`, a distinct token-index pair, `A ∈ 10..=5000` →
  `amp = A·1000`, the four realistic swap fees, the three scaling-factor pairs,
  and the `calculate_invariant` / `calculate_invariant_deployed` entry point.
  The invariant is derived after the draw and a non-converging draw is discarded
  by `prop_filter_map` (the old `balancer_shape` returned `None` there).
  Distinct token indices are produced by an offset in `1..n` rather than a
  reject-based filter, so the run is not throttled.

`tests/common/mod.rs` was **not** needed: this harness builds `HopMath` views
from generated shapes, not `ResolvedHop` corpora, so no builder was added to any
test file.

## 2. Counters vs shrinking — the decision

The old shape was `proptest! { ... seed in any::<u64>() ... for _ in 0..400
{ shape = lcg(&mut rng) ... } }`: proptest saw only the 64-bit seed, so a failure
reported a seed, not a shape. The new shape draws ONE shape per case through a
real `Strategy`, so proptest shrinks the failing draw.

That collides with the counters, which were per-case (`n_shapes >= 300`,
`n_asserts >= 2_000`, `n_rise_active >= 200`, `n_guard_fired > 0` asserted inside
each of 24 cases). Resolution chosen — **keep both, by sizing the run**:

- `DOMINANCE_CASES = 9_600` — the old harness drew 400 shapes inside each of 24
  cases (~9,600 shapes total); one strategy case now draws one shape, so 9,600
  cases reproduces the same total generated-shape volume.
- The counters accumulate across the WHOLE run, so the floors are the old
  PER-CASE floors scaled by the old case count (`DOMINANCE_OLD_CASES = 24`):
  `shapes >= 300·24`, `asserts >= 2_000·24`, `rise_active >= 200·24`, and
  `guard_fired > 0` kept as the qualitative "the wide matrix reaches the guard at
  all" witness. That makes the strategy-driven run's guarantees **at least as
  strong** as before, not relaxed.
- `TestRunner::run` takes an `Fn` closure, so the run-wide counters live behind a
  `RefCell` (interior mutability) rather than a captured `&mut`.
- `failure_persistence: None` — the harness must not write a
  `proptest-regressions` file into the shared workspace; a strategy-driven run is
  reproducible from its seed anyway.
- `max_global_rejects` raised to 1,000,000 for the Balancer invariant filter
  (the default 1,024 cap is far too low for a ~9,600-case run). In the observed
  runs the invariant never actually rejected a draw.

Observed telemetry (both runs green, ~1.2–1.7 s each):

```
solidly_stable proptest: shapes=9600 asserts=274413 rise_active=83754
  guard_fired_cap_only=8310 leaf_wrapped=4076 leaf_undefined=434075 min_margin=0
balancer_stable proptest: shapes=9600 asserts=692006 rise_active=92352
  guard_fired_cap_only=7784 leaf_wrapped=0 leaf_undefined=0 min_margin=1
```

Every floor clears by a wide margin (`shapes 9600 ≥ 7200`, `asserts ≥ 48000`,
`rise_active ≥ 4800`, `guard_fired > 0`). No counter became unattainable, so no
threshold was touched.

## 3. Shrink demonstration (scratch run, restored)

Injected a deliberately wrong expectation into the shared harness assertion —
`bound >= leaf` → `bound >= leaf + 1e18` — ran both families with
`--no-capture`, then restored the correct assertion (verified by grep and by the
subsequent green run).

**Balancer** reported a minimized failure:

```
balancer_stable dominance failed: balancer_stable bound 3495000000000000000
  < leaf 3494000738079798866 at x=3495000000000000000
minimal failing shape: BalancerShape {
    amp: 36000,
    balances: [1000000000000000, 3495000000000000000],
    token_index_in: 0, token_index_out: 1,
    invariant: 1305191602322261690,
    swap_fee: 0, scaling_factor_in: 1, scaling_factor_out: 1,
}
```

**Solidly** reported a minimized failure:

```
solidly_stable dominance failed: solidly_stable bound 10000000000 < leaf 0
  at x=10000000000
minimal failing shape: SolidlyShape {
    reserves_0: 1000000000000, reserves_1: 10000000000,
    decimals_0: 1000000000000000000, decimals_1: 1000000000000000000,
    token_in: 0, fee_numer: 0, fee_denom: 1, camelot: false,
}
```

Both shrank to a minimal, human-readable shape (2-coin Balancer at the bottom of
the amp range, unit scaling, zero fee; Solidly at the bottom of the reserve
range with identity decimals and a zero fee) instead of a 64-bit seed. The
harness prints the minimal shape explicitly
(`minimal failing shape: {shape:#?}`) as well as via proptest's own
`minimal failing input`.

## 4. Set-based family dispatch

`solve_path_inner` no longer computes the eight booleans; it computes the family
set ONCE and matches on it:

```rust
let families: BTreeSet<HopType> = resolved.hops.iter().map(ResolvedHop::hop_type).collect();
let (result, walk_stats) = match family_arm(&families) { ... };
```

`FamilyArm` is the small arm enum (`ExactV2`, `ConcentratedLiquidity`, `Solidly`,
`Weighted`, `Stable`, `Curve`, `Unsupported`, `Walkable`). `FAMILY_DISPATCH` is
an ordered table of `FamilyRow { allowed: Option<&[HopType]>, required:
Option<HopType>, arm }`; `family_arm` takes the FIRST row whose `allowed`
superset contains every present family (and whose `required` family, when set, is
present), falling back to `Walkable`. Each row is a set-membership predicate
(`allowed` = "⊆", `required` = "contains"); the old ladder's `has_*`-only
refusal arms are the `allowed: None` rows that fire once the narrower rows above
them fail.

The arm ORDER is preserved exactly:

| old ladder | table row |
|---|---|
| `all_v2` | `allowed={V2}` |
| `all_cl` | `allowed={V3,V4}` |
| `all_v2_or_solidly && has_solidly` | `allowed={V2,SolidlyStable}`, `required=SolidlyStable` |
| `has_solidly` | `allowed=None`, `required=SolidlyStable` |
| `all_v2_or_weighted && has_balancer_weighted` | `allowed={V2,BalancerWeighted}`, `required=BalancerWeighted` |
| `has_balancer_weighted` | `allowed=None`, `required=BalancerWeighted` |
| `all_v2_or_stable && has_balancer_stable` | `allowed={V2,BalancerStable}`, `required=BalancerStable` |
| `has_balancer_stable` | `allowed=None`, `required=BalancerStable` |
| `all_v2_or_curve && has_curve` | `allowed={V2,CurveStableswap}`, `required=CurveStableswap` |
| `has_curve` | `allowed=None`, `required=CurveStableswap` |
| `else` (mixed V2+CL) | `Walkable` fallback |

The empty path set (`{}`) is `⊆ {V2}`, so it takes `ExactV2` exactly as the
vacuous `all_v2` did. `ConcentratedLiquidity` and `Walkable` call the same
`solve_walkable_path_int` and are merged in the `match`. The V2 arm body, the V4
int128 guard, and the debug pool-state capture are unchanged. `solve_walkable_path_int`'s
own local `all_cl` (a different function, the walkable arm's CL-vs-mixed choice)
was left as-is — it is not the dispatch ladder.

## Gates run

- `cargo nextest run --locked --manifest-path rust/Cargo.toml -p degenbot-solvers`
  → **241 passed, 3 skipped**. This includes `stable_golden_drift_net`'s 96
  pinned snapshots (byte-identical; the snapshot file was not touched — `git
  status` shows only the two source files modified) and `solidly_golden_parity`'s
  tolerance test.
- `cargo clippy --locked -p degenbot-solvers --all-targets --manifest-path
  rust/Cargo.toml -- --deny warnings` → clean.
- `cargo fmt -p degenbot-solvers -- --check` → clean (ran `cargo fmt -p
  degenbot-solvers` once to apply; only the two touched files changed).
- `just test-rust-nextest` (whole workspace, extra) → **4794 tests run: 4794
  passed, 29 skipped** (exit 0, ~2m12s). The run detached past the 120s harness
  bound and completed in the background; the receipt above is its own final
  summary line.

## PENDING

None.

## Foreign tree activity (left untouched)

- `GLOSSARY.md` and `rust/crates/engine/degenbot-solvers/src/profit_envelope/mod.rs`
  doc cross-links, plus
  `docs/architecture/stableswap-gate-bounds-and-solve-method-evaluation.md` and
  `docs/ergo-results/yryknp-result.md` — from the concurrent DOCS worker (YRYKNP),
  already committed in HEAD.
- No uncommitted foreign modifications were present; `git status` showed a clean
  tree on entry and only my two files modified on exit.

## Fix round

Review found ONE gap: `family_arm`'s row selection — including the FOUR
`FamilyArm::Unsupported` reject rows and the `Walkable` default — was pinned by
nothing (the drift net covers only the accept rows, and no test called
`family_arm` directly). One unit test now pins it.

- Added `family_dispatch_table_selects_the_row_the_old_ladder_selected` to the
  existing `#[cfg(test)] mod gate_tests` in
  `rust/crates/engine/degenbot-solvers/src/mixed/solve.rs`. It drives
  `family_arm` over a `(family set, expected arm)` table covering EVERY row of
  `FAMILY_DISPATCH` plus the `Walkable` default: `{V2}` / `{V2,V2}` →
  `ExactV2`; `{V3}` / `{V4}` / `{V3,V4}` → `ConcentratedLiquidity`;
  `{V2,SolidlyStable}` / `{SolidlyStable}` → `Solidly`; `{SolidlyStable,V3}` and
  `{SolidlyStable,V3,V4}` → `Unsupported`; `{V2,BalancerWeighted}` →
  `Weighted`; `{BalancerWeighted,V3}` → `Unsupported`; `{V2,BalancerStable}` →
  `Stable`; `{BalancerStable,BalancerWeighted}` and
  `{V2,BalancerStable,BalancerWeighted}` → `Unsupported`;
  `{V2,CurveStableswap}` → `Curve`; `{CurveStableswap,V3}` → `Unsupported`;
  `{V2,V3}` / `{V2,V3,V4}` / `{V3,V2}` (order-insensitivity) → `Walkable`. The
  doc comment states WHY the reject rows and the default exist (a cross-family
  path has no single engine — the per-hop `swap_fn` solves are incompatible with
  CL tick-range enumeration — so it must be refused, not silently composed by
  falling through to the CL walk).

- **Brief discrepancy, flagged.** The brief listed `{}` (empty) → `Walkable`.
  The ACCEPTED `FAMILY_DISPATCH` does NOT do that: the empty set is vacuously
  `⊆ {V2}`, so it matches the first row and selects `ExactV2` — the same arm the
  old vacuous `all_v2` selected (section 4 above already documents this).
  Verified by probe: `family_arm({})` → `ExactV2`. Since the accepted diff is
  frozen (and the accepted result doc already pins `ExactV2`), the test pins
  `{}` → `ExactV2`, not the brief's `Walkable`. `Walkable` is still covered by
  `{V2,V3}`, `{V2,V3,V4}`, and `{V3,V2}`.

- **Failure path demonstrated (scratch, restored).** Temporarily removed the
  Solidly reject row (`FamilyRow { allowed: None, required: SolidlyStable, arm:
  Unsupported }`) from `FAMILY_DISPATCH`, ran the test, and observed:

  ```
  assertion `left == right` failed: family_arm([SolidlyStable, V3]) selected the wrong arm
    left: Walkable
   right: Unsupported
  ```

  i.e. the removed reject row let `{SolidlyStable,V3}` fall through to the
  `Walkable` arm — exactly the regression this test exists to catch. Restored
  the row (verified the table is the 10 accepted rows in the accepted order) and
  re-ran green.

- Gates re-run (foreground): `cargo nextest run --locked -p degenbot-solvers` →
  **242 passed, 3 skipped** (was 241; +1 = this test); `cargo clippy --locked -p
  degenbot-solvers --all-targets -- --deny warnings` → clean; `cargo fmt -p
  degenbot-solvers -- --check` → clean. The 96 drift-net snapshots are unchanged
  (`git status` shows only the two accepted source files modified).

- Task bookkeeping: the task was already `done`, and ergo's state machine
  forbids `done → doing`, so `ergo claim MGGILN --agent sc3-fix-round` was
  rejected (`error: invalid transition: done → doing`). The task was re-closed
  with the same `done` state and this result path.

