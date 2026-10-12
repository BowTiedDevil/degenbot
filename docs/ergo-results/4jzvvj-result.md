# 4JZVVJ result — a permanent latency bench for the stable solve arms

## What shipped

- `rust/crates/engine/degenbot-solvers/benches/stable_arms.rs` (NEW): a
  criterion bench with five groups — one per stable arm (Solidly, Balancer
  weighted, Balancer stable, Curve) plus the all-V2 exact-Mobius reference.
  Each group drives `degenbot_solvers::mixed::solve_path` (via
  `GateDeps::offline()`) over a fixed, deterministic, documented corpus, with
  one `bench_function` per path so criterion reports per-path wall time.
- `rust/crates/engine/degenbot-solvers/Cargo.toml`: `[[bench]] name =
  "stable_arms" harness = false`, matching the `digest` / `walkmemo_contention`
  registration.

The module doc states WHY the bench exists: the rejected t-parametrized
search's measured 81x regression (a closed-form probe can cost more than the
numeric search it replaces) was caught only by a throwaway bench that was
deleted with the revert, so the regression was never pinned. This bench is the
permanent replacement — a latency shift is caught by criterion's change
detection against the recorded baseline rather than an ad-hoc comparison
nobody reruns.

## Corpus shapes

Mirrors `tests/stable_golden_drift_net.rs` (the correctness drift net for the
same four arms), built inline because benches are separate binaries and cannot
import `tests/common/mod.rs`. All paths use the resolved-state convention (all
nonces one, valid, block zero). Every corpus path is asserted solvable at bench
setup, so an arm that stopped solving would fail loudly instead of quietly
benching its gate-skip.

- **Solidly** (16 paths): 2 decimal scales (`1e6`, `1e18`, the native reserve
  taken from the leaf's faithful-arithmetic floor via `faithful_base`) x 2 fees
  (`3/1000`, `1/1000`) x 2 imbalances (`101`, `110`) x 2 shapes (Solidly first,
  Solidly last), with a V2 leg (`997/1000`, scaled by the imbalance).
- **Balancer weighted** (16 paths): 2 bases (`1e18`, `4e18`) x 2 swap fees
  (`3e15`, `1e15`) x 2 imbalances x 2 shapes, balanced 50/50 hop + V2 leg.
- **Balancer stable** (16 paths): 2 bases x 2 swap fees (`1e13`, `1e15`) x 2
  imbalances x 2 shapes, balanced hop with a pre-computed invariant + V2 leg.
- **Curve** (16 paths): 2 bases x 2 fees (`4e6`, `1e6`, denominator `1e10`) x 2
  imbalances x 2 shapes, identity-rate 2-coin hop + V2 leg.
- **V2 reference** (6 paths): 2 bases x 3 imbalances (`101`, `105`, `110`),
  pure two-hop constant-product path. Both hops quote the same in-to-out ratio,
  so the round trip is profitable and the closed-form arm actually runs.

70 benchmarks total. Sampling is shortened from criterion's defaults
(warm-up 100 ms, measurement 400 ms, 10 samples) so the whole bench completes
inside the foreground harness bound; each solve is microseconds, so the window
still resolves a per-arm shift.

Tags are kept short deliberately: criterion derives its on-disk result
directory from the first 64 characters of the full benchmark id, so two long
ids sharing that prefix would collide and silently overwrite each other's
baseline. The full id (what criterion prints and compares) is unaffected.

## Baseline numbers

`cargo bench -p degenbot-solvers --bench stable_arms` (run from `rust/`), mean
point estimate per path, in microseconds. This run completed in the foreground
(~39 s, exit 0). One shared-workspace run, so treat these as the reference
order of magnitude the next change is judged against; criterion's change
detection against the stored baseline is the actual regression net.

### Solidly (`stable_arms_solidly`)

| path | mean |
| --- | --- |
| solidly_first_s1e18_fee1of1000_i101 | 57.2 us |
| solidly_first_s1e18_fee1of1000_i110 | 63.6 us |
| solidly_first_s1e18_fee3of1000_i101 | 60.1 us |
| solidly_first_s1e18_fee3of1000_i110 | 64.0 us |
| solidly_first_s1e6_fee1of1000_i101 | 57.6 us |
| solidly_first_s1e6_fee1of1000_i110 | 64.7 us |
| solidly_first_s1e6_fee3of1000_i101 | 56.1 us |
| solidly_first_s1e6_fee3of1000_i110 | 63.6 us |
| solidly_last_s1e18_fee1of1000_i101 | 58.1 us |
| solidly_last_s1e18_fee1of1000_i110 | 63.5 us |
| solidly_last_s1e18_fee3of1000_i101 | 58.1 us |
| solidly_last_s1e18_fee3of1000_i110 | 63.9 us |
| solidly_last_s1e6_fee1of1000_i101 | 56.8 us |
| solidly_last_s1e6_fee1of1000_i110 | 64.2 us |
| solidly_last_s1e6_fee3of1000_i101 | 54.4 us |
| solidly_last_s1e6_fee3of1000_i110 | 61.9 us |

### Balancer weighted (`stable_arms_balancer_weighted`)

| path | mean |
| --- | --- |
| weighted_first_b1e18_fee1e15_i101 | 25.2 us |
| weighted_first_b1e18_fee1e15_i110 | 25.2 us |
| weighted_first_b1e18_fee3e15_i101 | 25.0 us |
| weighted_first_b1e18_fee3e15_i110 | 25.2 us |
| weighted_first_b4e18_fee1e15_i101 | 25.1 us |
| weighted_first_b4e18_fee1e15_i110 | 25.0 us |
| weighted_first_b4e18_fee3e15_i101 | 25.4 us |
| weighted_first_b4e18_fee3e15_i110 | 25.1 us |
| weighted_last_b1e18_fee1e15_i101 | 25.8 us |
| weighted_last_b1e18_fee1e15_i110 | 26.0 us |
| weighted_last_b1e18_fee3e15_i101 | 25.8 us |
| weighted_last_b1e18_fee3e15_i110 | 25.7 us |
| weighted_last_b4e18_fee1e15_i101 | 25.7 us |
| weighted_last_b4e18_fee1e15_i110 | 26.1 us |
| weighted_last_b4e18_fee3e15_i101 | 26.0 us |
| weighted_last_b4e18_fee3e15_i110 | 25.8 us |

### Balancer stable (`stable_arms_balancer_stable`)

| path | mean |
| --- | --- |
| stable_first_b1e18_fee1e13_i101 | 47.2 us |
| stable_first_b1e18_fee1e13_i110 | 48.2 us |
| stable_first_b1e18_fee1e15_i101 | 48.4 us |
| stable_first_b1e18_fee1e15_i110 | 48.1 us |
| stable_first_b4e18_fee1e13_i101 | 48.0 us |
| stable_first_b4e18_fee1e13_i110 | 52.2 us |
| stable_first_b4e18_fee1e15_i101 | 48.0 us |
| stable_first_b4e18_fee1e15_i110 | 51.4 us |
| stable_last_b1e18_fee1e13_i101 | 51.2 us |
| stable_last_b1e18_fee1e13_i110 | 50.8 us |
| stable_last_b1e18_fee1e15_i101 | 51.1 us |
| stable_last_b1e18_fee1e15_i110 | 50.9 us |
| stable_last_b4e18_fee1e13_i101 | 50.6 us |
| stable_last_b4e18_fee1e13_i110 | 54.4 us |
| stable_last_b4e18_fee1e15_i101 | 50.5 us |
| stable_last_b4e18_fee1e15_i110 | 54.1 us |

### Curve (`stable_arms_curve`)

| path | mean |
| --- | --- |
| curve_first_b1e18_fee1e6_i101 | 52.7 us |
| curve_first_b1e18_fee1e6_i110 | 52.6 us |
| curve_first_b1e18_fee4e6_i101 | 52.7 us |
| curve_first_b1e18_fee4e6_i110 | 52.7 us |
| curve_first_b4e18_fee1e6_i101 | 52.6 us |
| curve_first_b4e18_fee1e6_i110 | 52.7 us |
| curve_first_b4e18_fee4e6_i101 | 52.6 us |
| curve_first_b4e18_fee4e6_i110 | 52.4 us |
| curve_last_b1e18_fee1e6_i101 | 55.8 us |
| curve_last_b1e18_fee1e6_i110 | 55.4 us |
| curve_last_b1e18_fee4e6_i101 | 55.8 us |
| curve_last_b1e18_fee4e6_i110 | 55.7 us |
| curve_last_b4e18_fee1e6_i101 | 55.7 us |
| curve_last_b4e18_fee1e6_i110 | 55.1 us |
| curve_last_b4e18_fee4e6_i101 | 55.6 us |
| curve_last_b4e18_fee4e6_i110 | 55.5 us |

### V2 exact-Mobius reference (`stable_arms_v2_reference`)

| path | mean |
| --- | --- |
| v2_reference_b1e18_i101 | 18.6 us |
| v2_reference_b1e18_i105 | 18.6 us |
| v2_reference_b1e18_i110 | 18.6 us |
| v2_reference_b4e18_i101 | 18.8 us |
| v2_reference_b4e18_i105 | 18.7 us |
| v2_reference_b4e18_i110 | 18.6 us |

Rough per-arm order: Solidly ~54-65 us, Curve ~52-56 us, Balancer stable
~47-54 us, Balancer weighted ~25-26 us, V2 reference ~18.6-18.8 us. The stable
arms sit roughly 3x the closed-form V2 reference, which is the shape the bench
pins.

## Gates run

| gate | command | result |
| --- | --- | --- |
| bench | `cargo bench -p degenbot-solvers --bench stable_arms` (from `rust/`) | GREEN — 70 benchmarks, exit 0, ~39 s foreground |
| clippy | `cargo clippy --locked -p degenbot-solvers --all-targets --manifest-path rust/Cargo.toml -- --deny warnings` | GREEN |
| fmt | `cargo fmt -p degenbot-solvers -- --check` (from `rust/`) | GREEN |
| tests | `cargo nextest run --locked --manifest-path rust/Cargo.toml -p degenbot-solvers` | GREEN — 242 passed, 3 skipped |

`--test` mode was also run once as a sanity check (all 70 paths solve and
report Success).

## PENDING

None. The full-length bench run (shortened sampling window, as documented in
the bench) completed in the foreground; nothing was left detached.

## Foreign activity

Tree was clean at HEAD `1bce8ce27` at dispatch. No foreign modifications were
observed during this run; the only working-tree changes are the two files
listed above.
