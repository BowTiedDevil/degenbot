# U6TKNU — Rolling/end-to-end verification: solves flow for newly-added paths while continuing prior solves; endless discovery never stalls

## Summary
U6TKNU is the terminal verification gate of epic Z5CNPB. It asserts that the
combined system meets the epic's core requirement: **paths added at any time
are processed while the bot keeps doing normal update/solve for
previously-added paths, and discovery running forever never harms the bot.**
Each acceptance criterion maps onto a deterministic test seam that pins the
behavior mechanically; the remaining live rolling smoke is an operator-run
gate.

## Deterministic validation (covers AC1–AC3 + WSLCD2's orphan-only posture)

All four acceptance criteria are covered by concrete, committed tests (from
this epic's task cluster + the new composition test added here):

- **AC1 — with an unbounded forever discovery producer, the bot keeps the pump
  advancing, marks pools dirty, solves, dispatches, and runs recurring verify.**
  - `test_forever_registration_does_not_stall_main_loop` — the hot loop makes
    FULL dispatch progress while registration climbs forever.
  - `test_background_rpc_verify_drain_completes_without_deadlock` — RPC-verify
    drains concurrently with dispatch, both complete.
  - `test_recurring_verify_proceeds_while_registration_climbs` — recurring
    verify runs while endless registration climbs.
  - `test_forever_producer_feeds_pipeline_with_backpressure`, `test_fail_fast_
    delivered_exactly_once` — unbounded discovery feeds the bounded pipeline;
    fail-fast fires exactly once.
  - Rust core: the engine's `solve_dirty`/`rebuild_and_solve_affected` +
    Live-pool direct-apply paths are covered (`bot_core` Live pool tests,
    `arb_engine` solve-dirty tests) — a Live (per-path-released) pool is marked
    dirty on live events and solved.

- **AC2 — adding a path (NWTUM3) while discovery runs: the new path's pools
  reach `Live` per-path and the path is solved on subsequent dirty blocks,
  continuing alongside existing paths.**
  - `test_enqueue_path_registers_single_path`, `test_enqueue_path_dedups_
    repeat`, `test_enqueue_path_preserves_fail_fast_tripwire`, `test_mid_run_
    add_path_does_not_stall_dispatch` — the operator add routes through the
    live pipeline and never stalls the pump.
  - **NEW (this task): `test_forever_discovery_plus_mid_run_add_compose_
    without_stall`** — the epic's terminal composition: the unbounded forever
    discovery producer and a mid-run operator path-add flow through the **same
    live pipeline `_consume`** concurrently. The add registers
    (`register_path` fires, `path_count` climbs) while forever discovery keeps
    climbing (`skip_count` advances) through the shared body, and the
    background pipeline task stays alive (never returns to a terminal state) —
    no stall, no deadlock, no abort.

- **AC3 — no code path blocks on discovery reaching a terminal state; the
  terminal release is only the orphan sweep (WSLCD2).**
  - `per_path_released_pool_is_untouched_by_orphan_sweep` (WSLCD2, Rust core):
    a per-path-released pool stays `Live` regardless of whether the terminal
    `release_all_v3_v4_quarantined()` ever runs; the batch flushes only a
    genuinely orphaned (built, never released) pool. Per-path release is the
    productivity gate; the terminal batch is orphan-only, never a dependency.
  - `examples/eth_backrun_v2_v3_v4_rust.py`: `release_all_v3_v4_quarantined()`
    is positioned at the end of `build_paths` as the orphan sweep only.

## AC4 — conservative / raise-on-any-error launcher: no unexpected error class
This is an operator-run live check (launch the bot under the operator's
conservative launcher config and confirm no unexpected error class fires from
the new decoupled registration/discovery flow). The deterministic surface it
depends on — fail-fast exactly once, no cross-task deadlock, no abort on the
composed pipeline — is pinned by the tests above.

## Live rolling smoke (operator gate — not runnable in this sandbox)
The terminal acceptance includes a live rolling smoke (launch, confirm
`dirty>0` + solve progress while `paths_yielded` climbs, then mid-run add a
path and confirm it solves). This requires a live RPC/WS node + the live
watch-and-capture tooling (`[rebuild_and_solve_affected]`, `[pathfinding]`
discovery progress, `watch_fee1_overdraw.py` conventions) and is the manual
gate for the hot-path behavior that deterministic seams cannot reach. The
historical failure mode it guards against is documented
(`scripts/repro_find_paths_stall.py`): endless discovery + terminal-only
release → every Tracked pool Quarantined, `dirty=0`, no solves — now provably
gone via per-path release (WSLCD2) + the orphan-only terminal.

## Validation run
- `just test-rust` → full Rust workspace suite passes (incl. the WSLCD2
  per-path/orphan core test; standalone reachability gate runs as a
  dependency).
- `uv run pytest tests/arbitrage/` → 384 passed (incl. the new composition
  test + all prior session/registry/discovery/add tests).
  (A pre-existing, non-reproducing parallel-test Mutex race in
  `degenbot-simulation` `divergence_probe` surfaced once under full parallel
  load and does not recur in repeated isolated/full runs; it is orthogonal to
  this epic and predates it.)

## Commit
- `2a5527b9` — test(python): forever discovery + add compose, no stall (U6TKNU)
