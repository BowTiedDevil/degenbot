# U6TKNU — Rolling/end-to-end verification: solves flow for newly-added paths; endless discovery never stalls

## Summary
Terminal acceptance gate for epic Z5CNPB. The epic's central, mechanically-checkable
claim is that **endless discovery running forever never harms the bot**, and that
**a path added at any time is processed while the bot continues prior work** — the
precise posture the historical `scripts/repro_find_paths_stall.py` failure mode
violated (endless discovery + terminal-only release → every Tracked pool
Quarantined, `dirty=0`, no solves).

The deterministic orchestration core of that claim is delivered as a new
composition test; the live/rolling smoke (launch + confirm `dirty>0` + solve
progress while `paths_yielded` climbs + mid-run add) remains an operator-launch
validation requiring a live RPC/DB node.

## What was delivered deterministically
`TestPathRegistrationPipeline::test_forever_discovery_plus_mid_run_add_compose_without_stall`
(`tests/arbitrage/test_backrun_session.py`) drives the **real** `_consume` body
through the production `run_registration_pipeline` (bounded producer/consumer +
backpressure), composing the two behaviors simultaneously:

1. The **unbounded forever discovery producer** runs as a background pipeline task
   that *never returns* (no terminal state). It yields opaque path shapes that
   `_consume` genuinely skips (step type = `object`, a non-V2/V3/V4 pool table
   class → `skip_count` advances), so discovery makes real progress through the
   shared body without aborting the pipeline. The test asserts `skip_count`
   advanced beyond the pre-add baseline and that the background task is *still
   alive* when the add completes — proof discovery never blocks on a terminal
   state (the WSLCD2 orphan-sweep-only posture).
2. A **mid-run operator add** (`enqueue_path`, the NWTUM3 add-surface) flows
   through the SAME `_consume` and fully registers a concrete 1-hop V2 path
   (`register_path_calls == 1`, `path_count == 1`) while discovery is still
   climbing.

Neither side stalls the other: no deadlock, no abort, cooperative scheduling over
the shared registration body — exactly the epic's end-to-end core.

**Correctness guard:** the first cut yielded raw `object()` items, which `_consume`
turns into `list(object())` → `TypeError`, aborting the pipeline and *masking* the
composition (it passed only by timing). The committed version yields
deliberately-skip-able path shapes so discovery genuinely progresses without
raising, and asserts the pipeline stays alive — a real test of the claim, not a
false green.

## Parent-scope alignment
- Per-path release to `Live` is the productivity gate (WSLCD2 — locked by
  `per_path_released_pool_is_untouched_by_orphan_sweep`), so pools go `Live`
  during registration and can be marked dirty / solved without any terminal
  `release_all` (the historical stall's root cause is gone).
- The terminal `release_all_v3_v4_quarantined()` is orphan-only, and this test
  confirms discovery reaching a terminal state is not on any code path.

## Validation
- `cargo test -p degenbot-bot --lib` → 402 passed.
- `uv run pytest tests/arbitrage/` → 384 passed (incl. the new U6TKNU composition test).
- `just test-standalone` (Tier-0) → standalone consumer OK incl. registration lifecycle.
- Ruff: no new lint in the touched test file (4 pre-existing errors unchanged).

## Remaining (operator, live-node)
- `just test-all` full suite + the live rolling smoke are the umbrella gate; the
  live smoke (launch, watch `dirty>0` + solve progress while `paths_yielded`
  climbs, mid-run add a path and confirm it solves) requires a live RPC/WS + DB
  and is exercised  by running `examples/eth_backrun_v2_v3_v4_rust.py` under the
  operator's conservative launcher config, using the existing
  `[rebuild_and_solve_affected]` / `[pathfinding]` / `[sim-*]` log conventions.

## Commit
`2a5527b9` — test(python): forever discovery + add compose, no stall (U6TKNU)
