# 7VFOXX (SR4, epic SLYHMO): the shared golden-section template for the stable-family arms

STATUS: **DONE.** The four copy-pasted golden-section bodies in `mixed/solve.rs` collapse to
family-specific closures over ONE shared helper. A new golden-snapshot drift net
(`tests/stable_golden_drift_net.rs`, 4 corpora x 24 paths = 96 pinned `(optimal_input, profit)`
pairs) is **byte-identical before and after** the extraction. Scoped gates green; the
workspace recipe is **PENDING-foreign** (a pre-existing `degenbot_rs` build break, not mine).

## What changed

### One shared helper (new home)

`rust/crates/engine/degenbot-solvers/src/mixed/solve.rs` gains:

- `const GOLDEN_SECTION_ITERATIONS: usize = 25` (renamed from `SOLIDLY_GOLDEN_SECTION_ITERATIONS`
  — it was always the shared count).
- `enum VerifySweep { Ascending, Outward }`.
- `fn solve_golden_section_path_int(v2_equiv, simulate, simulate_outputs, sweep)` — owns the whole
  search: the V2-equivalent Mobius profitability precheck, the +/-5x bracket around the model
  optimum, the 25-iteration phi-approximated golden loop, and the +/-3 integer verification. The
  per-family simulate closures and the V2-equivalent projection are the only family inputs.
- `fn verify_best(...)` — the integer-verification + result construction (shared `consumed_inputs`
  build, shared profit gate).
- `fn ascending_sweep(...)` / `fn outward_sweep(...)` — the two +/-3 candidate orders, one home
  each.

### The four arms now shrink to their closures

| arm | before (fn lines) | after | family closures |
| --- | --- | --- | --- |
| `solve_solidly_path_int` | 86 | 25 | `simulate_solidly_path` / `_outputs`, `VerifySweep::Ascending` |
| `solve_balancer_weighted_path_int` | 80 | 23 | `simulate_balancer_weighted_path` / `_outputs`, `Ascending` |
| `solve_balancer_stable_path_int` | 115 | 20 | `simulate_balancer_stable_path` / `_outputs`, `Outward` |
| `solve_curve_path_int` | 74 | 19 | `simulate_curve_path` / `_outputs`, `Outward` |

### Deleted private copies (no arm keeps a private loop)

- `solidly_brute_force_best` (41 lines), `balancer_weighted_brute_force_best` (43),
  `balancer_stable_brute_force_best` (25), `curve_brute_force_best` (25) — the four private
  +/-3 verifications.
- `curve_build_result` (22) — folded into the shared `verify_best`.
- The `#[expect(clippy::too_many_lines)]` on the stable arm (the body no longer needs it).

Net: `mixed/solve.rs` **1875 -> 1621 lines** (diffstat +150 / -404; the added block is the helper
plus its docs).

## The behavior detail the task did not anticipate: TWO verify orders

The task text assumed one "±3 verification radius". In fact the four arms used **two different
±3 sweeps**, and this is a genuine (if accidental) behavioral split:

- Solidly and Balancer weighted scanned `center-3 ..= center+3` **ascending** (earliest max wins).
- Balancer stable and Curve walked **outward** `center±delta` from a *moving* best, so the probed
  set depends on the profit trajectory (e.g. it can probe `center+5`).

They are not equivalent, so the helper preserves both exactly via `VerifySweep`; unifying them
would have been a numeric change. This is the only place the shared helper takes a third input.

Also preserved verbatim: the `hops.len() < 2` guard existed only in the Solidly and weighted arms
(not stable/Curve), so it stays in those two arms rather than moving into the helper.

## Drift net (the required addition)

`tests/solidly_golden_parity.rs` (HEAD `289e62338`) is the model: synthetic corpora + brute-force
verification. The committed captured corpora (`heavy_mixed_solve_captures`, `live_gatebursts_mixed`)
carry NO Solidly/Balancer/Curve legs, so a new file is the net.

`tests/stable_golden_drift_net.rs` (new, untracked) builds a 24-path synthetic corpus per arm:

- Solidly: 2 decimal scales x 2 Solidly fees x 3 imbalances x 2 shapes (Solidly first / last).
- Weighted / Stable / Curve: 2 bases x 2 family fees x 3 imbalances x 2 shapes.

Each path is a 2-hop cycle (the family hop balanced ~1:1 + a mispriced V2 hop), so all 96 paths are
profitable and every path drives the full precheck -> bracket -> 25-iteration golden loop -> ±3
verification. Each arm's returned `(optimal_input, profit)` was captured as a **literal pinned
snapshot** from the pre-extraction code (`Some((input, profit))`; `None` would be pinned too), then
asserted `==` after the extraction.

Evidence:

- Pre-extraction (HEAD + new test only): 4/4 `*_arm_matches_golden_snapshot` PASS.
- Post-extraction: 4/4 PASS — 96/96 pairs byte-identical (0 wei drift).
- The pre-existing `solidly_golden_parity` golden-arm test also PASSes unchanged.

## Gates run (foreground)

| gate | result |
| --- | --- |
| `just test-rust-nextest -p degenbot-solvers` | **PENDING-foreign** — build fails in `degenbot_rs` (see below), not my crate |
| `cargo nextest run --locked --manifest-path rust/Cargo.toml -p degenbot-solvers` (documented deviation) | 241 passed, 3 skipped |
| `cargo clippy --locked -p degenbot-solvers --all-targets --manifest-path rust/Cargo.toml -- --deny warnings` | clean |
| `cargo fmt --manifest-path rust/Cargo.toml -p degenbot-solvers -- --check` | clean |
| `cargo fmt --manifest-path rust/Cargo.toml --all -- --check` | clean |

## PENDING / foreign activity

- **PENDING-foreign**: `just test-rust-nextest` builds the whole workspace even with `-p`; it dies
  compiling `degenbot_rs` (lib test) with `E0061: run_pool_update_injected takes 10 arguments but
  9 were supplied` at `crates/shells/degenbot-python/src/pool/mod.rs:677` and `:745` (the missing
  arg is `max_block_range: u64`). This is the `989e30e98` eth_getLogs block-range clamp landing in
  the Python shell; it is pre-existing and untouched by this task. The crate-scoped nextest run
  above is the deviation the manager authorized.
- Known PRE-EXISTING failure not run here: `degenbot-config::no_stray_env_reads::no_stray_python_env_reads`.
- No foreign tree modifications observed: `git status --short` shows only
  `M rust/crates/engine/degenbot-solvers/src/mixed/solve.rs` and the new untracked test file.

Tree left dirty for review; no commit.
