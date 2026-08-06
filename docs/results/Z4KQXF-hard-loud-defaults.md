# Z4KQXF result — conservative HARD/LOUD env flags are now the CODE default

Commit: `3d0fdaca` ("refactor(rust): hard-loud env flags default ON (Z4KQXF)")

## What changed

`run_bot.sh` held ~10 conservative DEGENBOT_* flags; a hand-run (or any harness
that doesn't source the launcher) used the code's liberal defaults and silently
masked real problems (e.g. V3/V4 pools stuck `Quarantined` by registration-
liquidity verification → zero dirty pools / zero sims with no visible cause).
Now the code defaults to the conservative posture for every invocation.

## Added convention

- `bot_env_flag_default_on(name) -> bool` (bot_core/mod.rs) + `flag_default_on`
  (backrun-strategy/simulator.rs): `true` unless the env var is set to an
  explicit falsey value (`""`, `0`, `false`, `off`, `no`, `n`). Default-on so a
  hand-run never silently drops failure visibility; opt OUT explicitly with `=0`.
  Both delegate to a pure `parse_*_flag_value` so they're unit-testable without
  process-global env mutation.

## Flipped to default-ON

- `DEGENBOT_VERIFY_DBG` — structural verify diagnostics / `[verify-dbg]` divergence set.
- `DEGENBOT_DUMP_CALL_TRACE` — full revm call trace on sim failure.
- `DEGENBOT_DEBUG_V4_SOLVE` — V4 solver intermediates.
- `DEGENBOT_V2_CALC_TRACE` — V2 reserves slot8 before each sim.
- `DEGENBOT_SIM_LOG_REVERTED_SWAPS` — per-hop actual-vs-predicted on revert.
- `DEGENBOT_SIM_EXIT_IGNORE_BUCKETS` — now defaults to `""` (empty allowlist =
  trap on EVERY sim failure bucket, sys.exit(3)); used to default
  `CurrencyNotSettled`.
- `DEGENBOT_ASSERT_SOLVER_STATE` — the ADR-021 solver-state tripwire. Now a
  per-pump `solver_state_verify: bool` field (NOT a global env read): conservative
  default `true` in production (`subscribe`) and explicitly `false` in the test
  constructor (`for_test`/`pump_for_test`) so TDD tests are immune to the global
  env and can opt in deterministically when they exercise the verifier.
- `DEGENBOT_SIM_EXIT_ON_FAIL` — was already default `"1"` at the parse site; verified.

## Stays OFF (not fail-fast / address-keyed / high-noise)

- `DEGENBOT_DRAIN_DBG`, `DEGENBOT_TRACE_REGISTER_SEED`.

## run_bot.sh

Trimmed to a thin launcher: the redundant flag exports are replaced with a comment
explaining the code is now the single source of truth, plus the soak-mode override
note (`DEGENBOT_SIM_EXIT_ON_FAIL=0 ./run_bot.sh` to trade through thin-margin
reverts). `start/stop/status/foreground` behavior unchanged.

## Verification

- New tests: `conservative_bot_flag_default_on` (bot), `conservative_flag_default_on_and_parse`
  (backrun), `test_pump_disables_solver_state_verify_by_default` (tripwire field).
- `cargo test -p degenbot-bot --lib`: 406 pass. `cargo test -p degenbot-backrun-strategy --lib`: 91 pass.
- clippy + fmt clean; pre-commit gate (rust fmt/clippy/no-pyo3, python fmt/lint) clean.
- Python render tests pass; example edit is lint-clean (remaining long lines are pre-existing).
