# BHTWBZ Checkpoint — path-5000 RED→GREEN clamp regression test

**Task:** Extend the path-5000 harness to assert RED→GREEN (real PoolManager
fill under 5M). Epic PJLIAE (the final family task).

**Result:** Committed a tier-3 regression test (`degenbot/tests/
tier3_path5000_v4_clamp.rs`) that deploys the real v4-core `PoolManager` (via
the committed `V4SwapOracleHarness` unlocker artifact, no toolchain), seeds the
path-5000 V4 pool slot-for-slot, and proves the CL-hop input clamp turns the
20.7M-gas EMPTY-HALT into a clean byte-exact fill under the executor's 5M
ceiling.

## The RED→GREEN boundary (measured on real PoolManager bytecode)

Path 5000 = V2(MATIC/WETH) → V4(UNI/MATIC) → V3(UNI/WETH); recorded V4 hop
input `15351327867212777` (zfo=false, sell MATIC/currency1). The V4 tracked band
is `[-257352, 35067]`; the tier-3-proven twin reports it converts only
`input_consumed = 15351327867192638` (20,139 wei leftover tips past the last
funded tick into zero liquidity).

| input | price limit | 5M gas verdict | gas_used | amount0 |
|---|---|---|---|---|
| **unclamped** `…212777` | MAX_SQRT_RATIO−1 (executor default) | **REVERTED** (RED) | ~4.77M (OOG truncation of the 20.7M march) | — |
| **clamped** `input_consumed−1 = …192637` | MAX_SQRT_RATIO−1 | **ACCEPTED** (GREEN) | **190,755** | **460882096151249** (byte-exact) |
| unclamped @ 30M (reference) | MAX | ACCEPTED | 20,776,614 | 460882096151249 |

The 5M verdict at the same MAX price limit flips from a revert (20.7M empty-march
truncated) to a 190k clean fill purely by clamping the committed input to
`input_consumed − 1` — with **byte-identical** `BalanceDelta amount0`. The gas
probe (`path5000_v4_gas_probe.rs`, CLI-driven with `CLAMP_INPUT`/`EXECUTOR_LIMIT`)
reproduces this interactively; the committed test asserts it mechanically.

## What the committed test asserts

1. **GREEN**: clamped committed input (`input_consumed − 1`) fills ACCEPTED at
   5M gas with `amount0 == 460882096151249` byte-exact and `gas_used < 5M`.
2. **Clamp bound**: committed input is `≤ input_consumed − margin` (margin = 1,
   the VAASFM/7E5D7W maximum-extraction choice).
3. **RED preserved**: the unclamped recorded input with a MAX price limit does
   NOT complete at 5M (reverts/halts) — so the harness is not silently vacuous:
   the clamp is what turns RED→GREEN.

## Code organization (no copy-paste)

The deploy+seed+drive sequence (extracted from the probe) now lives once in
`degenbot::investigation::real_oracle` (`drive_real_v4_swap` +
`seed_v4_pool_storage` + encoders). Both the `path5000_v4_gas_probe` example and
the committed test call it, so the byte-exact verdicts can't drift between the
interactive probe and the regression guard. `degenbot_simulation::oracle::Output`
is now `pub use`-re-exported (it already leaked through the public `Verdict`).

## Wiring

- `just test-tier3-path5000` runs the pair (rebuild+republish the v4 harness,
  then `cargo test -p degenbot --test tier3_path5000_v4_clamp`).
- Added to the `just test-tier3` umbrella.
- Because the test loads committed bytecode, it also runs in the default
  `just test-rust` / `cargo test --workspace` path (per ADR-020).

## Verification

- `cargo test -p degenbot --test tier3_path5000_v4_clamp` — 2 passed (GREEN +
  RED-preserved).
- `just lint-rust`, `just check-no-pyo3-in-cores`, `cargo test -p degenbot-bot
  -p degenbot-executor -p degenbot-simulation` all green.
