# Epic Z5CNPB — Rust pool builder + continuous-background discovery

## Delivered
The complete migration of pool building and discovery into the Rust core,
driven continuously in the background, is **done**. All 20 children are
complete (one canceled as superseded):

- **Part 1 — Rust PoolBuilder + verify lifecycle (core ownership).** `3FVZF4`
  (probe-dispatch-assemble for V2/V3/V4), `4GQWZ4` (Python builders retired;
  `Bot.build_pool` delegates), `IKGQ6F` (verify-lifecycle core ownership:
  sparse-live-immediate / tracked-after-verify), `A2QRWO` (Tier-1 reachability
  + Tier-2 dual-driver parity), `SSSXG6` (lingering Python builders ported),
  `6ZGF4V` (7 OfflineProvider-mock tests ported to Rust). `A4YORC`/`C5BAZ3`/
  `W5DXGB` spikes scoped the choreography absorption and PyPool companion
  seams; `DWOE67`, `F2R2OC`, `TF7RZB` (Sub-A2) land the construction seam +
  27 wrappers core-side. `5E7ZRF` (Sub-A ConstructionContext) + `BGKZDQ`/
  `XVZRKS` (Sub-B/C background registration on the pump tokio runtime).

- **Part 2 — continuous-background discovery.** `6VZN7H` (unbounded forever
  discovery producer), `NWTUM3` (operator add-a-path-at-any-time surface),
  `WSLCD2` (per-path release to Live replaces terminal batch-release),
  `U6TKNU` (terminal rolling/e2e verification). `AF6OCC` (original decoupled
  run) canceled as superseded by the Sub-B/C + NWTUM3 evolution.

## Epic-level outcome (per U6TKNU, the terminal gate)
- **Rust is the engine:** pool building, pool state, registration lifecycle,
  and solve run entirely in the Rust core; Python is a driver shell.
- **Path release is per-path:** a newly-registered path's pools reach `Live`
  as their verification completes (sparse immediate, tracked after verify),
  without any discovery completion; the terminal batch is an orphan sweep only.
- **Endless discovery never harms the bot:** pumps advance, pools are marked
  dirty + solved, dispatches flow, recurring verify runs, and mid-run added
  paths continue alongside — all pinned by deterministic tests
  (`test_forever_discovery_plus_mid_run_add_compose_without_stall`,
  `per_path_released_pool_is_untouched_by_orphan_sweep`, plus the prior
  forever-discover/mid-run-add/recurring-verify cluster).

## Validation
- `just test-rust` — full Rust workspace suite green (incl. the WSLCD2 core
  test + Tier-1/2 parity + standalone reachability).
- `uv run pytest tests/arbitrage/` — 384 passed (incl. the U6TKNU composition
  test + all session/registry/discovery/add tests).
