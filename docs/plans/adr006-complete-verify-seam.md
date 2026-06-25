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
## Postmortem (epic GAYTBA): the rolling-start verify race + silent swallowing

**Symptom** (`logs/perm-V2-V3-V2.log`, 2026-06-25): every V3-containing
permutation emitted a repeating false verify failure during `build_paths`:

```
[dbg-verify] MISMATCH 0x99ac8c… tick=62520 block=25392802 engine=6554974444 onchain=6509876110 update_block=25393698 journal_len=1 total_ticks=342
[build_paths] Engine registration failed (RuntimeError): V3 liquidity map verification FAILED: …
```

**Root cause.** Two compounding bugs:

1. **Step-1 read the wrong state.** The two-step verify compared
   engine-*current* tick data against on-chain@snapshot_block. Under the bot's
   rolling start, `engine.resume()` runs *before* `build_paths`, so the live
   pump had applied a Mint/Burn onto engine-current between registration and
   step-1. Engine-current = (seed + journal); on-chain@snapshot = (pre-journal
   seed) → false mismatch on every active pool. The log fingerprint pins this:
   mismatches **only** at the snapshot block, `journal_len=1`, `update_block`
   postdating the snapshot, on high-activity pools; **never** at the
   live/backfill block (step-2, post-drain).

2. **The failure was silently swallowed.** `Pump::verify_v3/v4_liquidity_maps`
   mapped `LiquidityVerifyError` → plain `PyRuntimeError`. `build_paths`'
   `except RuntimeError` arm (the non-fatal skip) caught it; the fatal
   `VerificationMismatchError` arm never fired. Even a genuine mismatch would
   have been dropped as a skipped path.

**Fix** (epic GAYTBA, commits `1d24dd18`, `1d0ae8ad`, `5e7419bc`):

- **AGVGNH** — route `LiquidityVerifyError` through `map_liquidity_verify_error`
  (`Mismatch → VerificationMismatchError`, `Rpc → VerificationRpcError`) in
  `verify_v3/v4_liquidity_maps` (and the batch `verify_liquidity_maps`), mirroring
  the batch path's `map_verify_err`. Restores fail-fast classification by type.
- **CBCH6H** — pin the snapshot seed at registration. `V3PoolState`/
  `V4PoolState` retain `snapshot_seed` (a copy of the registration `tick_data`)
  for `Tracked` pools, immutable across `apply_*_liquidity_update`. Step-1 now
  calls `PyBot.verify_v3/v4_snapshot_seed`, which takes the seed and compares it
  via the raw-tick-data `verify_v3/v4_liquidity_map` functions; the seed is
  consumed once so memory is bounded. Step-2 (backfill, post-drain) is
  unchanged — engine-current vs on-chain@backfill.
- **OVVLGO** — Rust regression: `v3_snapshot_seed_survives_pump_liquidity_update`
  + the V4 twin pin the seed's immutability across pump events + the take-once
  semantics. The Python two-step-verify suite pins step-1=seed / step-2=current
  routing.

The rolling-start design is preserved; the race is closed at its cause.

**Live verification (pending the operator's run).** Re-run a V3-containing
permutation (`./logs/test_all_permutations.sh`, or a single `uv run python
examples/eth_backrun_v2_v3_v4_rust.py --permutation V2-V3-V2`) and confirm:
no `[dbg-verify] MISMATCH … block=<snapshot> journal_len=1` lines; step
emits `[verify-seed] V3 snapshot seed OK …` and `[verify] V3 liquidity maps
OK at block …` for both snapshot and backfill blocks.
