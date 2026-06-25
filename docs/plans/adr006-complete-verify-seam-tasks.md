# Epic: Complete ADR-006 D3+D4 and restore two-step verify at the pump seam

## Inventory (task graph)
- T1 Snapshot + backfill-block capture in `start()`
- T2 Relocate `SnapshotStore` + `register_with_cl_buffers` off the engine
- T3 Relocate `subscribe`/`backfill_from_snapshot`/`resume` onto PyBot
- T4 Relocate verify closures + `EngineVerifyRpc` onto PyBot
- T5 Delete `engine.register_v2/v3/v4_pool` (D3) + `verify_on_register` flag
- T6 Wire two-step verify at `engine_registry.register_*` drain seam
- T7 Add hot-loop recurring `verify_liquidity_maps`
- T8 Example + tests + ADR-006 status correction

Dependencies: T1 before T6. T2 before T5. T3 before T4. T4 before T6. T5 after T2.
T6 after T1,T4. T7 after T6. T8 after T5,T6,T7.

# T1 — Capture snapshot_block + backfill_block in start()
## Goal
`EngineRegistry.start()` (`src/degenbit/arbitrage/engine_registry.py:95`) already
computes `snapshot_block = min(s.newest_block for s in snapshots)` (line ~129) and
receives `backfill_target` from `subscribe()` (line ~121). Both blocks are needed by
the two-step verify (T6) but today are thrown away. Stash them on the registry so T6
can pass them to the verify closures.
## Acceptance
- `EngineRegistry` holds `_verify_snapshot_block: int | None` and
  `_verify_backfill_block: int | None`, set in `start()` from the values it already
  computes (NOT via the orphaned `engine.set_verify_*_block` setters).
- The existing `_verify_rpc_url` / `_verify_state_view` stashes (lines ~145-146)
  stay; this adds the two block fields alongside.
- No behavior change yet (fields set, not read) — T6 reads them.
## Out of scope
- Wiring the blocks into `engine.set_verify_*_block` (those setters are deleted in T5).

# T2 — Relocate SnapshotStore + register_with_cl_buffers off the engine
## Goal
ADR-006 D4: the trapped pure helpers `SnapshotStore`
(`rust/crates/degenbot-bot/src/solvers/uniswap_engine/snapshot_verify.rs:205`) and
`register_with_cl_buffers` (`:279`) move onto `Bot`/the chain pump. They are pure
(testable without pyo3) and were only on the engine for historical ADR-005 reasons.
Move them to `rust/crates/degenbot-bot/src/bot_core/` (e.g. a new
`bot_core/snapshot_store.rs` or fold into `bot_core/mod.rs`), re-export from
`bot_core`, and update the `use` in
`rust/crates/degenbot-python/src/bot/engine/mod.rs:52`.
## Acceptance
- `SnapshotStore<K>`, `register_with_cl_buffers`, `run_cl_verification` compile from
  `degenbot_bot::bot_core::*` (not `...solvers::uniswap_engine::*`).
- The engine's `v3_snapshot`/`v4_snapshot` fields (`engine/register.rs:78-79`) stay
  for now (T3/T5 relocates them); only the *type definitions* move.
- `cargo test -p degenbot-bot --lib` green; `run_cl_verification` unit tests
  (`snapshot_verify.rs:530+`) still pass after the move.
## Out of scope
- Moving the engine *fields* that hold `SnapshotStore` instances (T5 deletes them).

# T3 — Relocate subscribe/backfill_from_snapshot/resume onto PyBot
## Goal
ADR-006 D4: `subscribe`, `backfill_from_snapshot`, `resume` are Bot-owned I/O, not
engine concerns. Today they live on `UniswapArbEngine`
(`rust/crates/degenbot-python/src/bot/engine/register.rs:511/576/706`). Move the pyo3
methods onto `PyBot` (`rust/crates/degenbot-python/src/bot/mod.rs`). They already
operate on the shared `Arc<RwLock<BotState>>` (D1), so the move is mechanical: same
bodies, different `#[pymethods]` impl block.
## Acceptance
- `PyBot` exposes `subscribe`, `backfill_from_snapshot`, `resume` (matching today's
  signatures).
- `UniswapArbEngine` no longer declares them.
- `EngineRegistry.start()` (`engine_registry.py:121,133,886`) calls
  `self.bot.subscribe(...)` / `self.bot.backfill_from_snapshot(...)` / `self.engine.resume()`
  paths updated to hit `PyBot`. (The registry already holds `self.bot`; the engine
  may still get `resume()` as an alias if the pump coord needs the engine lock — judge
  by the existing `EnginePhase` gate semantics in `register.rs`. Prefer pump-on-Bot.)
- The example's `engine_registry.engine.resume()` call
  (`examples/eth_backrun_v2_v3_v4_rust.py:929`) still compiles (either PyBot-resume or
  a thin engine-resume wrapper that delegates — your call, documented inline).
## Out of scope
- Engine `Mutex` fields (`path_pools`, `dirty_*`, etc.) stay on the engine (ADR-006 D2
  explicitly keeps engine-level state). Only the pump/I/O methods move.

# T4 — Relocate verify closures + EngineVerifyRpc onto PyBot
## Goal
ADR-006 D4: the verify plumbing (`EngineVerifyRpc`, `verification_provider`,
`set_verify_rpc_url`, `set_verify_state_view`, `verify_liquidity_maps`,
`verify_v3/v4_liquidity_maps`) move onto `PyBot`. Today they live on the engine
(`rust/crates/degenbot-python/src/bot/engine/verify.rs`). The `verify_on_register`
flag + `set_verify_on_register` are **excluded** — they're deleted in T5.
## Acceptance
- `PyBot` exposes `set_verify_rpc_url`, `set_verify_state_view`,
  `verify_liquidity_maps`, `verify_v3_liquidity_maps`, `verify_v4_liquidity_maps`.
- `EngineVerifyRpc` / `verification_provider` move to a `bot/verify.rs` (sibling of
  `bot/mod.rs`) and operate on the shared `BotState`.
- `EngineRegistry.start()` wires `py_bot.set_verify_rpc_url(...)` /
  `set_verify_state_view(...)` (replacing `engine.set_verify_*` at
  `engine_registry.py:137-139`).
- `EngineRegistry.verify_liquidity_maps()` (`:210`) calls `py_bot.verify_liquidity_maps(...)`.
- The batch `verify_liquidity_maps` still passes (this is the one that works today;
  its new home is PyBot, behavior identical).
## Out of scope
- `verify_on_register` / `set_verify_on_register` / `verify_*_block` setters — T5
  deletes these; do NOT relocate them here.

# T5 — Delete engine.register_v2/v3/v4_pool + verify_on_register flag (D3)
## Goal
ADR-006 D3: delete the orphaned engine-level register methods
(`rust/crates/degenbot-python/src/bot/engine/register.rs:87/132/264`). They have zero
production callers (only `diagnostic.rs` under `#[cfg(test)]`). Their
`verify_on_register` gate (the dead verify) goes with them. Also delete:
- `verify_on_register: AtomicBool` (`engine/mod.rs:105`)
- `set_verify_on_register` pyo3 method (`verify.rs:626`)
- `verify_snapshot_block` / `verify_backfill_block` Mutex fields + their setters
  (`engine/mod.rs:118/123`, `register.rs:682/688`) — zero callers, superseded by T1.
- The dead `VerificationMismatchError` / `VerificationRpcError` exception handlers in
  `build_paths` (`examples/eth_backrun_v2_v3_v4_rust.py:1221-1248`) —
  `engine_registry.register_*` never raises them.
- The `verify_on_register=True/False` kwarg from `EngineRegistry.start()` signature
  (`engine_registry.py:103`) and the example call (`eth_backrun:885`).
## Acceptance
- `grep -rn verify_on_register rust/ src/ examples/` returns nothing (docs excluded).
- `UniswapArbEngine` pyo3 surface no longer has `register_v2/v3/v4_pool`.
- The rust core `BotCore`/`BotState` `register_v2/v3/v4_pool` (the live insert at
  `mod.rs:550/556/569`) is untouched — those stay (the builders call them via
  `PyBot.register_v*`).
- `cargo build` + `just test-rust` + `just test-python` green.
## Out of scope
- Wiring the *replacement* two-step verify (T6 does that).

# T6 — Wire two-step verify at engine_registry.register_* drain seam
## Goal
Restore the two-step, fail-fast verification the dead gate was *meant* to provide — at
the correct seam. `engine_registry.register_v3_pool` / `register_v4_pool` (Python,
`engine_registry.py:283/328`) are called per-pool during `build_paths` and already do
`apply_buffer_*` (drain backfill+pump onto the seed). That drain moment is when
"post-backfill" state first exists — the natural place for the two-step verify:
1. **snapshot verify** (before drain, against a captured seed copy) — seed tick map
   vs on-chain @ `_verify_snapshot_block` (set in T1). Catches bad seed/serialization.
   The seed is available before drain mutates it: capture from the snapshot (the
   `stream_v3/v4_snapshot_to_engine` data, or read `bot_core`'s
   `SnapshotStore::take` result that `register_pool` already consumes).
2. Drain: `apply_buffer_*` (existing).
3. **backfill verify** (after drain) — post-drain state vs on-chain @
   `_verify_backfill_block`. Catches bad backfill/buffer-apply — would have caught the
   V4 register→seed clobber live, at the offending pool, not 18k pools later.
V2 has no tick map — skip verify for V2.
## Acceptance
- `engine_registry.register_v3_pool` / `register_v4_pool` run both verify steps
  around the existing `apply_buffer_*` drain, gated by verify config being set (same
  `enabled()` check `run_cl_verification` uses).
- Both steps use `verify_liquidity_maps`-style async (NOT `runtime.block_on` — the
  tokio deadlock at `ecf576de` came from block_on-in-pyo3); the registry's register
  methods are already async-callable from the `build_paths` async path, or wrap in
  the same `future_into_py` the batch uses.
- Fail-fast: a mismatch raises `VerificationMismatchError` at the offending pool,
  surfacing from `build_paths` (the now-live exception handler T5 left in place IF
  still reachable — else surface as RuntimeError, matching the batch verify's error
  category). Confirm the handler classification matches the batch path.
- A red test: register a pool with a deliberately-wrong seed tick gross → verify_step_1
  raises before drain. (Mirror the red-green shape of the V4 inline-seed test
  `tests/bot/test_register_v4_pool_inline_seed.py`.)
- The pre-fix logs' batch-only MISMATCH is now preceded (in a new run) by a per-pool
  fail-fast at the offending pool.
## Out of scope
- Hot-loop recurring verify (T7).

# T7 — Add hot-loop recurring verify_liquidity_maps
## Goal
The batch verify at `examples/eth_backrun_v2_v3_v4_rust.py:949` runs once, pre-loop.
Bug B (the V3 unregister desync, `3ae6fa04`) developed *after* it passed — during
`release_python_state` (step 4). Anything desyncing after 3b is never detected. Add a
recurring verify in the hot loop so post-release / in-loop desyncs surface before
trading on them.
## Acceptance
- A periodic verify (every N blocks, configurable; or on each reorg) calls
  `verify_liquidity_maps(block_number=last_processed_block)` in the main loop.
- Frequency default conservative (e.g. every 50 blocks) to bound RPC cost; surfaced
  via the same `[verify] V3 + V4 liquidity maps OK at block {}` line, with a
  `[verify] (recurring)` tag to distinguish from the startup batch.
- Mismatch raises fail-fast (same RuntimeError as the batch), halting the bot — never
  trade past a detected desync.
- Behavior: a simulated post-release desync (mirror the Bug B unregister in a test)
  is caught by the recurring verify within N blocks.
## Out of scope
- Caching the on-chain reads across recurring invocations (optimization; out of scope).

# T8 — Example wiring + tests + ADR-006 status correction
## Goal
Land the cumulative changes in the example, restore the test suite, and correct the
ADR-006 "implemented" claim to match reality (D3/D4 now actually done).
## Acceptance
- `examples/eth_backrun_v2_v3_v4_rust.py` `run()` calls the relocated PyBot
  subscribe/backfill/resume/verify methods (no `engine.`-prefixed equivalents).
- `tests/arbitrage/test_engine_registry_start.py`,
  `test_backrun_session.py`, `test_engine_registry_register_v[234]_shared_state.py`
  updated to match (the fakes record PyBot calls, not engine calls, for the relocated
  methods).
- New tests for T6 (red→green fail-fast) and T7 (recurring catches post-release
  desync) land.
- ADR-006 (`docs/adr/ADR-006-bot-as-per-chain-orchestrator.md`) status note appended:
  "D3+D4 completed in a follow-up; the pre-completion stall (engine register methods
  retained + verify_on_register orphaned, gate never fired, batch-only verify) is
  resolved." Remove the now-inaccurate "11 slices landed" completeness claim for
  D3/D4 or annotate that those two landed in a follow-up.
- Full permutation run (`logs/test_all_permutations.sh`) shows per-pool fail-fast
  `[verify]` lines during `build_paths` and the recurring verify in the loop; no
  regression vs the post-`292101f` baseline.
## Out of scope
- Re-running the full 27-permutation matrix end-to-end (a spot-check of V3-V3-V3,
  V3-V4-V3, V2-V4-V4 suffices).