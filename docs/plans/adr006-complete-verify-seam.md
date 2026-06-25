# Plan: Complete ADR-006 D3+D4 — restore two-step verify at the pump seam

## Background (read first)

ADR-006 decided a *single* deep orchestrator (`Bot`) with the engine reduced to
math/path (`register_path` + `solve` + `EventSink::on_block`). The migration stalled
after D1 (shared `Arc<RwLock<BotState>>` — landed). Three sub-decisions never executed:

- **D3** said "the engine-level `register_v2/v3/v4_pool` methods **are deleted**." They
  are still present (`rust/crates/degenbot-python/src/bot/engine/register.rs:87/132/264`)
  and someone attached a `verify_on_register` gate to them.
- **D4** said `subscribe`/`backfill_from_snapshot`/`resume`/verify plumbing move onto
  `Bot`. They still live on `UniswapArbEngine` (`engine/register.rs:511/576/706`,
  `engine/verify.rs`). `PyBot` owns only `register_v2/v3/v4_pool`.
- The trapped pure helpers (`SnapshotStore`, `register_with_cl_buffers`, verify closures)
  were meant to move onto Bot/the chain pump — still on the engine
  (`rust/crates/degenbot-bot/src/solvers/uniswap_engine/snapshot_verify.rs`).

Consequence: pool registration was rerouted through `PyBot.register_v*` (the builders,
`src/degenbot/builders/*v[234]_pool_builder.py`) to avoid the duplicate-address panic
D1 made real, so the verify gate on `engine.register_v*` is stranded on an unreachable
method. `set_verify_snapshot_block` / `set_verify_backfill_block`
(`register.rs:682/688`) have **zero callers**. The only verify that runs is a single
batch `verify_liquidity_maps` at `examples/eth_backrun_v2_v3_v4_rust.py:949`, post-
`build_paths`, pre-`release_python_state`. Batch-only means no fail-fast at
registration and blindness after `release_python_state` + in the hot loop.

The V3 unregister desync (`3ae6fa04`) and V4 register→seed clobber (`292101f`) both
evaded verify by design — Bug B developed *after* the batch verify passed; the V4
clobber's lost updates were only caught at the end, not at the offending pool.

## Goal

Complete D3+D4: collapse the `Bot`/`UniswapArbEngine` overlap, relocate verify to the
pump drain seam (fail-fast, two-step), and add a hot-loop recurring verify so post-
release / in-loop desyncs surface instead of trading silently. Delete the dead surface
in one move rather than grafting verify onto a Python short-circuit.

## Non-goals (explicit)

- No change to the V3/V4 swap math, the `TickMap` trait (ADR-004), or the reorg journal.
- No change to `PyLiquidityPool` / `PyErc20Token` (they read the shared `BotState`,
  untouched).
- No multi-engine / multi-chain coordinator (D5 stays as-is — N Bots = N chains).
- The V4-heavy `build_paths` slowness (per-pool `StateView.extsload` in the builder) is
  a separate problem, not in scope.