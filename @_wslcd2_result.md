# WSLCD2 — Per-path release to Live is the productivity gate; terminal batch stays orphan-only

## Summary
The per-path release to `Live` is the productivity gate in the live run. A newly
registered path's pools become `Live` (and solvable) as their verification
completes, per-path, without waiting for any discovery completion or the
terminal batch. The terminal `release_all_v3_v4_quarantined()` remains **only**
an orphan sweep (pools built but whose path never completed registration) and
is never a productivity dependency.

## How per-path release is wired (already landed by IKGQ6F + the pipeline cluster)
- `engine_registry.register_v3_pool` / `register_v4_pool` are thin delegating
  shells: each runs the core-owned `run_v3/v4_registration_lifecycle`
  (`bot_core/registration_lifecycle.rs`), which sequences quarantine →
  seed-verify → drain+pin → post-drain-verify → `set_live`, with the mismatch
  tripwire as the final gate. A sparse pool is an immediate no-op (`Live`, no
  RPC); a tracked pool is `Live` only after verification.
- The registration pipeline (`examples/eth_backrun_v2_v3_v4_rust.py`
  `_consume_step`) calls these per-path for every pool in a discovered/added
  path — so release happens during registration, concurrent with ongoing
  discovery, never deferred to the terminal flush.
- The terminal `engine_registry.engine.release_all_v3_v4_quarantined()`
  (end of `build_paths`) remains only the orphan sweep for Tracked pools built
  but never reached by a `register_v3/v4_pool`.

## New deterministic test (this task)
`bot_core::registration_lifecycle::tests::per_path_released_pool_is_untouched_by_orphan_sweep`
pins the WSLCD2 core acceptance mechanically:

- A tracked V3 pool released to `Live` by the per-path lifecycle stays `Live`
  regardless of whether the terminal batch ever runs.
- The terminal `release_all_v3_v4_quarantined()` is a no-op for already-`Live`
  (per-path-released + sparse) pools — proving the per-path lifecycle does not
  duplicate or depend on the batch release policy.
- The batch flushes **only** a genuinely orphaned Tracked V4 (built, never
  released by any per-path lifecycle) — the orphan sweep's legitimate, sole use.

This proves the core consequence WSLCD2 drives: per-path release is
self-sufficient as the productivity gate, and the terminal batch is
orphan-only (it never re-releases a pool the per-path path already released).

## Validation
- `cargo test -p degenbot-bot --lib` → 402 passed (incl. the new WSLCD2 test).
- `cargo clippy -p degenbot-bot --lib -- -D warnings` → clean; `cargo fmt --check` → clean.
- Python registry/session suites (`test_engine_registry_two_step_verify.py`,
  `test_backrun_session.py`) → 39 passed.

## Commit
`d8f6f08e` — test(rust): per-path release is the productivity gate (WSLCD2)
