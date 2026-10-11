# FW7G2H — Finalization cleanups: result

## What changed
1. tests/uniswap/v2/conftest.py — the module-level `_PY_BOT = Bot()` (a live,
   I/O-owning engine minted in every xdist worker at collection time) is now a
   session-scoped lazy `py_bot` fixture; `offline_wbtc_weth_v2_pool` consumes it.
2. prek.toml — new pre-commit `stub-drift-staged` hook: when a staged file
   touches the binding crate's src/Cargo.toml or the stub generator, it runs
   `just gen-stubs --check` (non-mutating; fails if the committed stubs would
   change). Fires only on FFI-surface paths (verified with prek --dry-run).
3. prek.toml — pre-push `python-test` now runs `just test-python-noise-gate`
   (the spray tripwire); justfile recipe comment updated to match.
4. rust/crates/shells/degenbot-python/src/bot/engine/strategy.rs — reworded the
   stale "resolve ... once for the hosted process" doc comment and added an
   accurate `boot_host` doc: it mints a fresh `BootedHost` per engine
   construction (per-engine fleets); the process-once parts are the async-runtime
   bind and the config holder.
5. scripts/hooks/comment-hygiene-census.txt + comment-hygiene.sh — documented the
   line-start-deref false-positive shape (a Rust line starting with `*` is read
   as a block-comment continuation) and named it in the bare-token error text.
   Detection semantics unchanged.
6. scripts/pytest_noise_gate.sh — run-mode capture log removed on the SUCCESS
   path only (exit 0 + clean scan); kept on spray failure and on suite failure.
7. .agents/skills/dispatched-agent/SKILL.md — added the ergo close-procedure
   note (FIELD updates are a quoted JSON argument; piped stdin REPLACES the body).

## Gates run (scoped)
- uv run --no-sync pytest tests/uniswap/v2 -> 45 passed, 1 skipped.
- cargo check -p degenbot_rs --locked -> rc 0 (19s); cargo fmt -p degenbot_rs -- --check -> rc 0.
- just lint-comment-hygiene -> rc 0; forced-error demo (CHF=/dev/null) shows the new text.
- pytest_noise_gate.sh scan probe -> spray rc 3 / clean rc 0; run-mode demo -> success rc 0 log removed, failure rc 5 log kept.
- prek run stub-drift-staged --dry-run on FFI vs non-FFI paths -> fires only on the FFI surface.
- ruff format --check + ruff check on conftest.py clean; markdownlint on SKILL.md 0 issues; just --summary parses.

## PENDING
- `just gen-stubs --check` was NOT executed here: it cargo-builds the cdylib with
  the temporary experimental-inspect feature (multi-minute build that would
  detach past the harness bound). The recipe is pre-existing and exercised by
  tests/ffi/test_ffi_stub_drift.py in the pre-push suite; the hook wiring itself
  is verified via prek --dry-run.

## Foreign tree activity
- None observed: `git status --short` was clean at the start of this run and no
  foreign-dirty paths from the brief (degenbot-substrate/*, degenbot-db/*,
  degenbot-math/cl/*, pool_builder/*, construction_io/*, degenbot-pools/*,
  degenbot-python/src/bot/mod.rs + bot/pool.rs, src/db/*) showed edits.
