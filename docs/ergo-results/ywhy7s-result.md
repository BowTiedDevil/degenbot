# YWHY7S — SR2b: pin the golden arm's Solidly optimality with a salvaged brute-force regression corpus

## What changed

One new file, no production edits:

- `rust/crates/engine/degenbot-solvers/tests/solidly_golden_parity.rs` (new, untracked)

It re-derives the rejected t-arm's synthetic one-Solidly-hop corpus, its hop
builders, its `faithful_base` helper, and its fine brute-force verifier — all
self-contained, with no dependency on the rejected module. It calls the
committed public API (`degenbot_solvers::mixed::solve_path` with
`GateDeps::offline()`, reading `.result`) and asserts the golden-section arm's
returned profit is within 8 wei of the brute-force discrete maximum over a
unit-resolution window around the arm's own answer. Nothing in `src/` was
renamed, re-exported, or otherwise touched; `git status` on the crate shows
only the new untracked test file.

### Test home

`tests/solidly_golden_parity.rs` — the suggested name. The crate's `tests/`
directory already holds sibling solver-parity integration tests
(`v3_crossing_solver_vs_sim_parity.rs`, `v4_*`, `bounded_brent.rs`), so this
is the fitting home; a unit test inside `src/mixed/solve.rs` was rejected
because that file is another worker's territory and the task asks for a NEW
test file.

## Corpus shape counts

The matrix is exactly as pinned: **288 paths** = 4 shapes × 3 decimal scales
× 3 Solidly fees × 2 V2 fees × 4 basis-point imbalances.

| axis | values |
| --- | --- |
| shapes | `solidly-first` (Solidly A→B, V2 B→A), `solidly-last` (V2 A→B, Solidly B→A), `solidly-middle` (V2→Solidly→V2), `solidly-imbalanced` (Solidly hop's own B reserve heavier) |
| decimal scales (`10^decimals`) | `1`, `1_000_000` (6-dec), `1_000_000_000_000_000_000` (18-dec) |
| Solidly fees (`numer/denom`) | `3/1000`, `1/1000`, `3/10000` |
| V2 fees (`gamma/denom`) | `997/1000`, `999/1000` |
| imbalances (bps) | `101`, `102`, `105`, `110` |

Both profitable travel directions are covered: shapes 2 and 4 harvest the
mispricing on the outbound leg (and inside the Solidly hop), shapes 1 and 3
on the return leg.

**Profitable-path count (pinned): 143 of 288.** "Profitable" means the
committed `solve_path` dispatch returns `Some` with non-zero profit and
non-zero `optimal_input`. The test asserts this exact count
(`PROFITABLE_SIZE = 143`), so a change in the arm's coverage of the matrix
fails loudly rather than silently shrinking the corpus.

Deterministic sample: **32 profitable paths** (`SAMPLE_SIZE = 32`, within the
16–32 brief), picked at evenly spaced indices over the profitable subset so
the sample tracks the matrix spread rather than one corner.

## Sweep radius, tolerance, and measured runtime

- Sweep half-width: **8_000 native input units** around the arm's returned
  `optimal_input`, swept at unit resolution (16_001 candidate inputs per
  path; 512_032 path simulations across the 32-path sample). The arm's own
  integer verify only scans ±3, so this window is an independent check.
- Tolerance: **8 wei**. The observed worst residual over the sampled paths is
  **1 wei** (and the failure-proof run below shows the arm losing exactly
  1 wei on one path when the tolerance is tightened to 0).
- Measured runtime: **0.72 s** isolated
  (`cargo nextest run -p degenbot-solvers -E 'test(golden_arm)'`), **0.84 s**
  in the full crate run under nextest's parallel load — comfortably under the
  ~2 s budget. The test asserts no wall-clock bound, so it stays deterministic
  on a loaded machine.

## How the test's failure path was proven (live, then restored)

In a scratch run (not committed) I set `TOLERANCE_WEI` from `8` to `0` and
ran the test. It FAILED, naming the offending path's parameters:

```
thread 'golden_arm_lands_within_tolerance_of_the_discrete_optimum' panicked at
crates/engine/degenbot-solvers/tests/solidly_golden_parity.rs:259:9:
golden arm lost 1 wei to the discrete optimum (tolerance 0 wei):
solidly-first scale=1000000 solidly_fee=3/1000 v2_fee=997/1000 imbalance_bps=101
arm_input=8362 arm_profit=17 brute_input=8985 brute_profit=18
```

`TOLERANCE_WEI` was then restored to `8` and the test re-run green. So the
assertion is live: it can fail, and when it does it quotes the path's shape,
scale, both fees, imbalance, and the arm-vs-brute inputs and profits.

## Gates

| gate | result |
| --- | --- |
| `just test-rust-nextest -p degenbot-solvers` | **PENDING-foreign** — the workspace recipe fails to BUILD on another stream's mid-edit files, before reaching any test. See below. |
| `cargo nextest run --locked --manifest-path rust/Cargo.toml -p degenbot-solvers` (documented deviation) | **PASS** — 233 run, 233 passed, 3 skipped; my test 0.838 s |
| `cargo clippy --locked -p degenbot-solvers --all-targets --manifest-path rust/Cargo.toml -- --deny warnings` | **PASS** (exit 0) |
| `cargo fmt --manifest-path rust/Cargo.toml --all -- --check` | **PASS** (exit 0) |

### PENDING-foreign detail

`just test-rust-nextest -p degenbot-solvers` forwards to
`cargo nextest run --workspace ... -p degenbot-solvers`, which compiles every
workspace target. It aborted with `error: recipe 'test-rust-nextest' failed
with exit code 101` while compiling **foreign, uncommitted** files:

- `rust/crates/integrations/degenbot-pool-updater/src/run.rs`
  (`E0061`: `run_pool_update` takes 10 args, callers pass 11; `E0277`:
  `Atomic<bool>` no longer satisfies `ProgressSink`) — and the same shape
  breaks `degenbot-pool-updater`'s `tests/wave2_capture_replay.rs`;
- `rust/crates/shells/degenbot-cli-core/src/pool.rs` (same two errors).

Neither file is mine (both are ` M` in `git status`, uncommitted, from the
concurrent pool-updater / CLI stream). Per the task's documented deviation I
verified the crate directly with `cargo nextest run -p degenbot-solvers`,
which builds only that package and its dependencies and is fully green.

Re-run once more after HEAD advanced (foreign commits `4ecd11eb2`,
`416d1c670`, `c698aad1d` landed mid-run): the recipe still fails to build on
the same foreign stream, now also via `degenbot-cli-core`'s
`tests/pool_arms.rs` (`E0063`: `PoolCommand::Update` missing
`max_block_range`). The `degenbot-solvers` crate is byte-identical to the
dispatch HEAD (`git diff --stat 1cd9df0d3..HEAD -- degenbot-solvers` is
empty), so the crate-level verification stands at both HEADs.

## Rejected production code

Not reintroduced. `grep` over the crate's `src/` and `tests/` for
`SolidlyTModel`, `MobiusMap`, `solve_solidly_golden_arm`, `try_solve_counted`,
and `sim_counter` finds nothing. `git status` on the crate shows only the new
untracked test file — `src/` is byte-identical to HEAD.

## Foreign activity observed (left completely alone)

- `rust/crates/integrations/degenbot-pool-updater/**` (src + 5 test files) and
  `rust/crates/shells/degenbot-cli-core/src/pool.rs` — mid-edit; the source of
  the PENDING-foreign recipe failure.
- The aave/cli/facade/Python stream named in the brief had its aave/Python
  half committed mid-run (HEAD advanced to `4ecd11eb2`); the still-dirty
  remainder is `rust/crates/facade/degenbot/{benches,examples}/**`,
  `rust/crates/shells/degenbot-cli/src/argv.rs`, and
  `rust/crates/shells/degenbot-python/src/pool/mod.rs`.
- The second dispatched worker's three files
  (`src/profit_envelope/derive.rs`, `src/mixed/solve.rs`,
  `tests/profit_envelope_tests.rs`) were CLEAN for the whole run — no edits
  landed there while I worked, so the pinned `PROFITABLE_SIZE = 143` reflects
  the committed dispatch alone.
- `docs(skill)` commit `4ecd11eb2` updated the dispatched-agent skill (adds a
  ruff-format scoped gate for Python work); not applicable to this Rust-only
  task.
- Untracked: `docs/ergo-results/gmdx5v-result.md`, `src/degenbot/discovery.py`,
  `tests/database/test_pool_discovery.py` — not mine.
