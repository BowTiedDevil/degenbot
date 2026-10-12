# 5RCLID — the Solidly faithful-arithmetic floor becomes the leaf's constant

STATUS: **DONE.** All three scoped gates green. The constant now lives once,
beside the leaf it describes, and both consumers import it. `faithful_base`'s
numeric output is byte-identical to the old literal, so the golden-parity
canary (`PROFITABLE_SIZE = 143`) and every drift-net snapshot are green with no
re-pin.

## What changed (files + intent)

- `rust/crates/foundation/degenbot-math/src/solidly/mod.rs` — the constant's new
  home. Added, beside `ONE`:

  ```rust
  /// The minimum scaled reserve below which the leaf's [`ONE`] divisions lose
  /// faithfulness.
  ///
  /// The invariant helpers (`calc_f` / `calc_k` / `calc_d`) divide by [`ONE`]
  /// twice and multiply the floored quotients, so a scaled reserve under `2^60`
  /// carries too little precision for the returned root to be trusted. A
  /// consumer that bounds this leaf's output treats a hop below this floor as
  /// outside the faithful domain and falls back to a precision-agnostic bound.
  pub const SOLIDLY_MIN_SCALED_RESERVE: u128 = 1 << 60;
  ```

  The type widened from the old private `u64` to `u128` so the test corpus (which
  works in `u128`) can express `4 * floor` directly; `U512::from` accepts either
  width, so the gate's arithmetic is unchanged.

- `rust/crates/engine/degenbot-solvers/src/profit_envelope/derive.rs` — the gate
  now imports the leaf's constant (`use degenbot_math::solidly::
  SOLIDLY_MIN_SCALED_RESERVE;`) and the private `const SOLIDLY_MIN_SCALED_RESERVE:
  u64 = 1 << 60;` is deleted. The gate body is byte-identical: `let min_scaled =
  U512::from(SOLIDLY_MIN_SCALED_RESERVE);`. Two doc/comment sites that describe
  the threshold now name the leaf's crate as the constant's home.

- `rust/crates/engine/degenbot-solvers/tests/common/mod.rs` — `faithful_base`
  imports the leaf's constant and derives the corpus floor with a documented
  four-times safety multiple:

  ```rust
  let scaled_floor = 4 * SOLIDLY_MIN_SCALED_RESERVE;
  ```

  The doc comment records why the multiple exists (the corpus sits comfortably
  inside the faithful domain, not on its boundary) and that the coupling to the
  leaf is now textual.

No other file changed. No snapshot, golden, or canary file was touched.

## Numeric-identity proof (the corpus is unchanged)

The old expression and the new one are the same number:

| expression | value |
| --- | --- |
| `1u128 << 60` (leaf constant) | 1152921504606846976 |
| `4u128 << 60` (old literal) | 4611686018427387904 |
| `4 * SOLIDLY_MIN_SCALED_RESERVE` (new) | 4611686018427387904 |

`4 * (1 << 60) == 4 << 60`. The remainder of `faithful_base`
(`saturating_mul(scale) / ONE_E18`, then `.max(2)`) is untouched, so the function
returns the identical value for every scale. The pinned `PROFITABLE_SIZE = 143`
canary in `tests/solidly_golden_parity.rs` passed unchanged, and every
`stable_golden_drift_net` snapshot passed, with no snapshot edits — the corpus
values did not move.

## Gates (foreground, from repo root unless noted)

- `cargo fmt -p degenbot-solvers -p degenbot-math -- --check` (from `rust/`) →
  **PASS (exit 0)**.
- `cargo clippy --locked -p degenbot-solvers -p degenbot-math --all-targets
  --manifest-path rust/Cargo.toml -- --deny warnings` → **PASS (exit 0)**,
  `Finished` clean.
- `cargo nextest run --locked --manifest-path rust/Cargo.toml -p degenbot-solvers
  -p degenbot-math` → **PASS (exit 0)**: 418 run, 418 passed, 3 skipped. Includes
  `solidly_golden_parity::golden_arm_lands_within_tolerance_of_the_discrete_optimum`
  (the 143 canary) and all four `stable_golden_drift_net` snapshots.

## PENDING

None. Every scoped gate ran to completion in the foreground.

## Foreign tree activity

Not mine; left completely alone:

- `docs/ergo-results/gmdx5v-result.md` — modified in the working tree before this
  run (a foreign stream's result doc).
- `docs/architecture/adding-a-hop-family.md` — untracked, foreign.

`git status --short` at end shows only the three files this chunk edited plus the
two foreign entries above.

## Not run (out of scope for a dispatched worker)

The whole-workspace ladder (`just test-rust-nextest`, `just test-python`) is the
manager's. Per-crate nextest for the two touched crates is green, but that is not
a claim of workspace-wide health.
