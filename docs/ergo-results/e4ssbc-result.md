# E4SSBC — Solidly-stable sound entry-tangent bound in the profit-envelope gate

Agent: `sr1-solidly-tangent` (dispatched worker). Tree left dirty; nothing committed.

## What changed (files + intent)

### `rust/crates/engine/degenbot-solvers/src/profit_envelope/derive.rs`
- New `HopMath::SolidlyStable { reserves_0, reserves_1, decimals_0, decimals_1, token_in,
  fee_numer, fee_denom }` — the field set `mixed::SolidlyHopState` exposes, with the
  reserves NATIVE and UN-ORIENTED; the arm orients them from `token_in`.
- New `solidly_stable_lines` / `solidly_stable_rise`: derives `[rise, flat cap]` following
  the `Weighted` arm's shape (U512 intermediates, I512 narrowing, `None` only on a zero
  output reserve).
  - Entry slope (native units) =
    `Rb·(3Ra² + Rb²)/(Ra·(Ra² + 3Rb²)) · γ/fee_denom · decimals_out/decimals_in`,
    with `Ra/Rb` the 1e18-scaled pre-trade reserves and `γ = fee_denom − fee_numer`.
  - The rise carries an ADDITIVE allowance `a` composed as `A = a·C`, so the line
    evaluates to `a + ceil(b·x/c)`. `a = ceil(price_entry · decimals_out/decimals_in) +
    ceil(4 · decimals_out/ONE)`.
  - A "faithful-arithmetic" gate drops the rise (cap-only fallback) when the scaled
    reserves are below `2^60`, or when the leaf's `calc_k` product
    `(Ra·Rb/ONE)·(Ra²/ONE + Rb²/ONE)` would wrap `u256`. **Never `None`, never
    `Envelope::Unsupported`** for a non-degenerate hop.
- HopMath module docs rewritten: the old "Solidly-stable marginal rate is non-monotone"
  claim is removed; the concavity derivation is carried in the new variant/helper docs.

### `rust/crates/engine/degenbot-solvers/src/mixed/solve.rs`
- `solve_path_with_min_profit` maps `ResolvedHop::SolidlyStable { state }` (stable) to the
  new `HopMath::SolidlyStable` instead of `HopMath::ReserveCap`. The volatile branch is
  unchanged.
- New RED-then-GREEN gate test `solidly_stable_tangent_prunes_where_reserve_cap_cannot`.

### `rust/crates/engine/degenbot-solvers/src/profit_envelope/{gate,capture}.rs`
- `gate.rs`: the degenerate-hop diagnostic `family` match gains the `SolidlyStable` arm.
- `capture.rs`: `DegeneratePathEvent::from_hops` gains the `solidly_stable(...)` family arm.

### `rust/crates/engine/degenbot-solvers/src/profit_envelope/mod.rs`, `GLOSSARY.md`
- Soundness-core bullet + GLOSSARY "Profit envelope" entry now describe the
  Solidly/Aerodrome/Camelot stable entry tangent beside the reserve cap (and that
  Curve / Balancer stable keep the cap alone).

### `rust/crates/engine/degenbot-solvers/tests/profit_envelope_tests.rs`
- `solidly_stable_entry_tangent_dominates_real_leaves` — proptest, oracle = the REAL leaves.
- `solidly_stable_tangent_tightens_below_reserve_cap` — tightening test.
- `solidly_stable_tangent_raises_skip_count_on_synthesized_corpus` — skip-count delta.

## The slack is NOT a small constant (important)

The task brief attributes the leaf/curve gap to the invariant-root floor alone and starts
the allowance at `ceil(4·out_decimals/ONE)`. That is not sufficient. The leaf's fee floor
retains `x − ⌊x·fee_numer/fee_denom⌋ ≥ (1−f)·x`, so the scaled post-fee input can exceed
its continuous value by up to `ONE/decimals_in`, worth up to
`price_entry·decimals_out/decimals_in` native output — e.g. a fair (6,18) pool has
`price_entry·decimals_out/decimals_in ≈ 1e12` native units of excess, against an
allowance of 4. Measured with the real leaf: `max_x (leaf(x) − slope·x) = 9.3e11` on a
fair 6→18 pool (slope 9.997e11). The implemented allowance therefore carries BOTH terms:
`ceil(price_entry·decimals_out/decimals_in) + ceil(4·decimals_out/ONE)`. With that, the
proptest's guarded regime shows zero under-cuts over ~260k assertions.

## Proptest matrix actually generated

`ProptestConfig::with_cases(24)`, each case generating 400 matrix rows from its seed
(9,600 shapes total): reserves log-uniform 1e6..1e30, imbalance ratio up to 1e6 in both
directions, decimals pairs {(18,18),(6,18),(18,6),(6,6)}, fee pairs {0, 3/10000,
30/10000}, `token_in` ∈ {0,1}, Aerodrome + Camelot. Per-case tallies (representative):
`shapes=400 asserts≈10k–12.5k rise_active≈3.1k–4.5k leaf-wrapped≈44–517
leaf-undefined≈1.6k–3.8k`. Total ≈ 260k point-wise assertions; ≈ 90k of them exercise
the tangent rise as the active line. Non-vacuity asserts: `n_shapes >= 300`,
`n_asserts >= 2000`, `n_rise_active >= 200`.

Two principled oracle skips (documented in the test, NOT "tune to pass"):
- **leaf-undefined**: the Solidly leaf's Newton step divides by `calc_d`, which floors to
  zero when the scaled input reserve is below `ONE^(2/3)`; the leaf then REVERTS (panics).
  Those probes are skipped. Camelot guards the same division and is not skipped.
- **leaf-wrapped**: a leaf reporting more than the pool's output reserve has wrapped its
  own `Rb − y` subtraction; no invariant-based bound is defined there.

## Before/after skip-count delta (GateStats.skipped)

Neither committed mixed corpus carries a Solidly leg (verified in-test:
`heavy_mixed_solve_captures.jsonl.zst` = 369 paths, V2/CL only; `live_gatebursts_mixed.jsonl`
= 24 paths, CL only), so the Solidly corpus is SYNTHESIZED in-test: 64 single-hop
Solidly-stable paths, reserves ≈ 1e23–5e24 (18-dec), fee 3/10000, floor 1e18.

```
solidly_stable corpus: 64 paths, skip count 0 (reserve cap) -> 32 (entry tangent + cap)
```

i.e. today's reserve-cap-only mapping skips none of them; the entry-tangent mapping skips
the 32 whose direction cannot profit. The 32 not skipped are the profitable-direction
cases (imbalanced pools where the gate must NOT skip).

## Gates run (foreground)

| Gate | Result |
| --- | --- |
| `just test-rust-nextest -p degenbot-solvers` | see below (long-running) |
| `cargo clippy --locked -p degenbot-solvers --all-targets --manifest-path rust/Cargo.toml -- --deny warnings` | PASS (clean) |
| `cargo fmt --manifest-path rust/Cargo.toml --all -- --check` | PASS (clean) |

Red-green loop (per-crate, not a declared gate):
`cargo test -p degenbot-solvers --test profit_envelope_tests` → 23 passed / 0 failed;
`cargo test -p degenbot-solvers --lib mixed::solve` → 10 passed / 0 failed (incl. the new
RED test).

## PENDING

`just test-rust-nextest -p degenbot-solvers` exceeded the harness foreground bound and
detached; its result is **PENDING** for the manager to re-run.

## Foreign tree activity

None observed. `git status --short` at dispatch was empty; the only modifications now are
the seven files listed above (all mine). No unrelated edits were reverted or touched.

## Fix round

Agent: `sr1-fix-round`. Inherited the previous agent's 7-file dirty diff as-is; changed only
what FIX 1 and FIX 2 required. Nothing committed.

### FIX 1 — GLOSSARY.md paragraph corruption

`GLOSSARY.md` "Profit envelope" entry: the previous agent's edit split the paragraph with
three stray blank lines, producing mid-sentence fragments. Removed those blank lines so the
entry reads as ONE continuous block (`...pool-static tangent fan. A Solidly/Aerodrome/Camelot
stable hop contributes its entry tangent (its output curve is concave in the input) beside
its reserve cap; the Curve / Balancer stable families keep the reserve cap alone.`), with the
trailing `Extending a single piece's validity window is not a sound bound.` sentence left on
its own line exactly as before. No other GLOSSARY entry touched. `npx --yes
markdownlint-cli2 GLOSSARY.md` -> 0 issues.

### FIX 2 — the rise is now guarded across the WHOLE evaluation window

`solidly_stable_rise` previously guarded PRE-trade arithmetic only (scaled reserves >= 2^60
and the pre-trade `calc_k` product inside u256). But the gate sweeps a hop's lines over
`x in [0, reserve_out]`, while the leaf's Newton runs at the POST-trade scale
`x0 = Ra + x*ONE/decimals_in`. There the leaf's `calc_d` products wrap u256 and a wrapped
Newton denominator returns a garbage root that can inflate the leaf's output ABOVE the
tangent. The guard now also drops the rise unless the leaf's arithmetic is non-wrapping over
the entire window.

Added conditions (all computed in U512 so the check itself cannot overflow; any U512
overflow or failed bound -> drop the rise -> cap-only fallback, i.e. today's behavior, so the
bound may only tighten):

- `reserve_out * ONE <= u256::MAX` (the leaf's own input upscaling `amount_in * ONE`).
- `x0_max = Ra + reserve_out * ONE / decimals_in` (gamma <= 1 absorbed conservatively),
  `y <= Rb` (the Newton seed is `Rb` and each step strictly descends), then require each of:
  - `x0_max * x0_max <= u256::MAX`
  - `(x0_max * x0_max / ONE) * x0_max <= u256::MAX`
  - `Rb * Rb <= u256::MAX`
  - `3 * x0_max * (Rb * Rb / ONE) <= u256::MAX`

Derived thresholds (u256::MAX = 2^256 - 1 ~ 1.1579e77, sqrt ~ 3.4028e38):

- `reserve_out <= u256::MAX / ONE ~ 1.1579e59` native.
- `x0_max <= 2^128 ~ 3.4028e38` (from `x0_max^2`).
- `x0_max <= (ONE * u256::MAX)^(1/3) ~ 4.87e31` (from `(x0_max^2/ONE)*x0_max`).
- `Rb <= 2^128 ~ 3.4028e38` (from `Rb^2`).
- `3 * x0_max * (Rb^2/ONE) <= u256::MAX` is the binding mixed term; e.g. an 18-dec pool with
  `Rb ~ 1e30` already caps `x0_max` near `3.9e46`.

Pre-trade checks (scaled reserves >= 2^60 ~ 1.1529e18, pre-trade `calc_k` product inside
u256) are retained. Doc comments on `solidly_stable_lines` / `solidly_stable_rise` explain WHY
the post-trade window matters; no comment carries a task ID.

### Tests pinned for FIX 2

1. Guard unit test `solidly_stable_window_guard_drops_rise_only_past_the_wrap_threshold`
   (`profit_envelope/derive.rs` tests module): a balanced 1e24/1e24 18-dec pool derives the
   two-line `[rise, flat]` set (rise active, `lines[0].b > 0`); a 6-dec in-reserve 1e12 native
   with an 18-dec out-reserve 1e29 native pool (`x0_max ~ 1e41`, `x0_max^2 ~ 1e82 > u256::MAX`)
   derives cap-ONLY (one line, flat cap only). Asserts on the derived line-set shape, not a
   downstream number.
2. Proptest matrix widened: imbalance-ratio exponent cap raised from 6 to 12
   (`ratio_exp = next % 13`), so the extreme-ratio mixed-decimals regime is actually probed.
   Added `n_guard_fired` (a shape is guard-fired when the bound at unit input equals the flat
   output reserve, i.e. the rise was dropped), asserted `> 0` for non-vacuity.

Extended-proptest telemetry (24 cases x 400 shapes = 9,600 shapes; each case prints one
line): total **8,526 guard-fired (cap-only) shapes** (341-362 per case, ~89% of shapes),
`n_rise_active` probes 1,920-3,124 per case, `n_asserts` ~9.6k-11.8k per case (~250k point-wise
dominance assertions total). The dominance assertion passes for both rise-active and
guard-fired (cap-only) shapes, so the guard-fired coverage is pinned as non-vacuous too.

### Gates (foreground)

| Gate | Result |
| --- | --- |
| `just test-rust-nextest -p degenbot-solvers` | PASS for `degenbot-solvers` (all new tests green). NOTE: the recipe hardcodes `--workspace`, so it ran the whole workspace: 4,783 tests, 4,782 passed, 1 failed = the known PRE-EXISTING `degenbot-config::no_stray_env_reads::no_stray_python_env_reads` (not this work). |
| `cargo clippy --locked -p degenbot-solvers --all-targets --manifest-path rust/Cargo.toml -- --deny warnings` | PASS (clean) |
| `cargo fmt --manifest-path rust/Cargo.toml --all -- --check` | PASS (clean) |
| `npx --yes markdownlint-cli2 GLOSSARY.md` | 0 issues |

### PENDING

None. All gates ran to completion in the foreground.

### Foreign tree activity / notes

- HEAD carries the foreign commit `46454bbca` (aerodrome trackers, Python); it is disjoint
  from these files and was left alone. `git status --short` shows only the 7 modified files
  (all from this task, previous agent + this fix round).
- Flagged follow-up (NOT fixed, out of scope): the proptest's `leaf > out_reserve` oracle skip
  is a PRE-EXISTING exposure of the reserve-cap bound itself — a wrapped leaf can report more
  than the pool holds, and no invariant-based bound is defined there.

