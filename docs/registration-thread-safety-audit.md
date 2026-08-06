# Registration pool-builder thread-safety audit (QGNACI)

Epic: RYE5DG — "Registration crawl: decouple pool-build blocking RPC from the
event loop & cut per-pool RPC" (task 1: QGNACI).

## Verdict
**GO-WITH-GUARDS.** The Rust core (`BotState`) is the locked single source of
truth; the only *private* shared state we found on the build path is the Python
registries, which are check-then-act but **benign-and-lossy, not corrupting**.
`asyncio.to_thread` offload (Option A) is therefore viable **provided** the
registry duplicate races are neutralized first.

## Scope audited
The synchronous blocking-RPC build chain invoked from the registration
`_consume` (`examples/eth_backrun_v2_v3_v4_rust.py`, `build_pool`:
`src/degenbot/bot/_bot.py:515`, `build_managed_pool` `:857` /
`_build_v4_managed`):
`io.get_block_number()` → `resolve_v4_identity` (core DB two-step) →
`_erc20_builder.build(currency0/1)` → `io.fetch_v4_slot0_liquidity` →
`PyBot.build_v4_pool` (fresh slot0 + tick map assemble + `register_v4_pool`),
then `self.managed_pools.add(...)`.

## Shared mutable state inventory + classification

| Component | Location | Concurrency | Classification |
|---|---|---|---|
| `BotState` (core) | `rust/crates/degenbot-bot/src/bot_core/mod.rs` | `parking_lot` `RwLock` (`state_arc.read()/write()`); `register_v2/v3/v4_pool` return `AlreadyRegistered`, `register_v4_state_view` under write lock | **SAFE** — core is locked + idempotent-guarded (verified tests: `register_v4_pool_rejects_duplicate_with_already_registered_variant`) |
| `PoolRegistry` | `src/degenbot/registry/pool.py` (`base.py`) | unlocked plain dict; `get` → build → `add`; `_add` raises on duplicate (`on_duplicate="error"`) | **check-then-act race** → duplicate build → second `_add` raises → pool path incorrectly **skipped** (benign-but-lossy) |
| `ManagedPoolRegistry` | `src/degenbot/registry/pool.py` | same unlocked dict + duplicate-raise | same benign-but-lossy race; `_build_v4_managed:1095` `managed_pools.add` raises on concurrent duplicate |
| `TokenRegistry` / `Erc20Builder` | `src/degenbot/registry/token.py`, `src/degenbot/builders/erc20_builder.py` | `_erc20_builder.build` is `self._tokens.get` → Rust `register_token` → `self._tokens.add` (duplicate-raise) | same benign-but-lossy race (token build for a concurrently-built token raises → V4 build fails → skip) |
| `PyBot.build_v4_pool` | `rust/crates/degenbot-python/src/bot/mod.rs:1017` | `py.detach(|| block_on(...))` releases GIL for the whole blocking build; each thread runs its own `block_on` | **structurally OK** under concurrency (GIL released); duplicate propagates as `AlreadyRegistered` Python error, not panic |
| `tick_data_fetcher` callback | `_build_v4_managed` passes it | **not invoked at build** (`builder.rs` `build_v4` sets `fetcher: None` at :1070; it is a lazy swap-time mechanism) | **no call-from-worker-thread GIL hazard at build time** (removed from the guard list) |

## Why offload is safe at its core
The authoritative pool/token state lives in `BotState`, which serializes every
registration under its `RwLock` and rejects duplicates by typed error — so the
*state* can never be corrupted by concurrent build threads. The entire risk
surface is the Python-side `get → build → add` dedupe: two threads miss the
`get`, both build, and the second `add` raises.

## Consequence of the race (benign-but-lossy, NOT corrupting)
A concurrent duplicate build does not corrupt anything, but the second thread's
registry `add` raises a `DegenbotValueError` that bubbles to the registration
`_consume` `except Exception` handler → the valid pool's path is **dropped as a
skip** and its RPC work is **wasted**. Under today's single-threaded workers
this never happens (builds are serialized); under `asyncio.to_thread` it would
become routine for pools shared across paths (the common case in the crawl).

## Guards required before/when offloading (for T1)
1. **Neutralize the registry duplicate race** — cheapest + most correct: set
   the pool/token registries to `on_duplicate="ignore"` (duplicate `add` is a
   no-op) **or** pre-set a "pending" marker before the build so a concurrent
   duplicate short-circuits (also cuts the wasted RPC). Applying this to
   `PoolRegistry`, `ManagedPoolRegistry`, `TokenRegistry` (and the
   `Erc20Builder` `add`) removes the lossy-skip behavior entirely.
   - Parallel to the `EngineRegistry.register_vN_pool` TOCTOU fix (DMZ3DD):
     same class of check-then-act; prefer the pending-marker pattern for both
     so it doubles as the dedupe (saves RPC).
2. **Bound the parallelism** — run workers on a dedicated
   `ThreadPoolExecutor(max_workers=REG_WORKERS)` so concurrency is bounded and
   equals REG_WORKERS (not the default unbounded executor).
3. **No GIL callback hazard at build** — confirmed non-issue (`fetcher: None`);
   re-check only if a future build path wires a live Python fetcher.

## Not audited here
- The `EngineRegistry.register_v2/v3/v4_pool` verify lifecycle (separate
  TOCTOU, task DMZ3DD) — offload makes that registration step concurrent too,
  so DMZ3DD pairs with T1.
- RPC batching/multicall (task CDJEPJ) — orthogonal wall-clock win.

## Hand-off to T1 (35NMBX)
`asyncio.to_thread` on the `build_pool`/`build_managed_pool` call, preceded by
Guard 1 (duplicate-tolerant registry add, ideally pending-marker dedupe) and a
bounded `ThreadPoolExecutor(max_workers=REG_WORKERS)`. Confirm with a
concurrent-same-pool unit test (RED before guard, GREEN after) the way the
existing `solver_state_change_set_scopes_to_resolved_paths_and_clears` gates
scoping.
