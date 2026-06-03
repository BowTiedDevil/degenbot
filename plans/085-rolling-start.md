# Plan 085: Rolling-Start Engine Registration

## Summary

Allow the Rust arbitrage engine to begin operating immediately after the first path
is assembled, while additional pools and paths continue loading in the background.
This eliminates the current bottleneck where the bot is idle during the entire
`build_paths()` + `freeze()` + `initial_solve()` + backfill sequence.

## Current Architecture

```
main() → build_paths() → freeze() → initial_solve() → backfill() → pump.start() → loop
         ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
         BOT IS IDLE FOR 60+ SECONDS DURING THIS ENTIRE SEQUENCE
```

- `build_paths()` iterates all paths, registers pools + paths into the Rust engine
- `freeze()` sets `running = true`, permanently locking registration
- `initial_solve()` resolves and solves all paths in one pass
- `backfill_snapshots()` syncs Python pool state to Rust engine
- `pump.start()` spawns WS subscription and begins autonomous event processing

**Hard constraint**: Once `running` is set, `register_pool` and `register_path` panic.
The engine is frozen — no new pools or paths can be added.

## New Architecture

```
main() → subscribe(ws) → [observe first block] → backfill() → resume() → loop
         ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
         PATH LOADING RUNS IN BACKGROUND DURING AND AFTER THIS SEQUENCE
```

- `subscribe()` opens WS connection, buffers Mint/Burn events
- First observed block anchors the backfill target
- `backfill()` closes the gap between snapshot and current state
- `resume()` begins normal pump processing
- Paths are registered at any time via `register_and_solve_path()` (eager solving)
- Fully operational from the first registered path onward

## Design Decisions

| # | Decision | Rationale |
|---|----------|-----------|
| 1 | Registration is always-on — `running` guard removed from Rust engine | Persistent capability; new paths can be added at any time during bot lifetime |
| 2 | Eager solving at path registration via `register_and_solve_path` | No missed opportunity window; one-block staleness is acceptable |
| 3 | Results written to `self.results` at registration, `pending_new_paths` set for `rebuild_and_solve_affected` merge | Immediate visibility to `latest_results()`; pump's result replacement must not discard new paths |
| 4 | Mint/Burn events for unregistered pools are buffered in sub-engines (raw, not collapsed) | Concentrated liquidity maps are incrementally built from events; dropped Mint/Burn would corrupt tick data |
| 5 | Raw events stored in buffer (not collapsed tick deltas) | Supports future reorg unwind; collapsing is fragile |
| 6 | Buffer events routed by topic identity markers to sub-engines immediately | Each sub-engine owns its own buffer; no cross-engine concerns |
| 7 | Configurable buffer staleness (default: unbounded, runtime-adjustable, explicit flush) | Caps steady-state memory growth while allowing full buffering by default |
| 8 | Pump split into `subscribe()` + `resume()` phases | Ensures no missed blocks: WS live → observe block → backfill → resume |
| 9 | `subscribe()` called immediately at startup | Earliest possible temporal anchor for backfill; no reason to wait |
| 10 | Per-block address filter snapshot (not frozen set, not HashMap direct check) | Lock held briefly for snapshot, released during O(n) log filtering, reacquired for processing |
| 11 | `freeze()` and `initial_solve()` removed entirely | Artifacts of phased start; replaced by eager solving |
| 12 | Sub-engines have no `running` concept | Only top-level `UniswapEngine` tracks pump state for coordination |
| 13 | Python key dicts remain plain dicts (not thread-safe) | All access on single asyncio event loop; add note for future contributors |
| 14 | `verify_liquidity_maps` stays, runs per-pool | Catches backfill errors quickly; full report instead of first-failure-stop |
| 15 | Buffer inclusion set uses higher-level `EventType` enum | Ergonomic; maps to topic hashes internally |
| 16 | Minimum-2 paths requirement stays | No use case for single-hop paths |
| 17 | Swap/Sync events for unregistered pools are safely discarded | Stateless — registration provides current state via RPC |

## Implementation Todos (In Order)

### Slice 1: Remove Registration Guards (Rust)

**TODO-c538ee36**: Remove `running` guard from Rust engine pool/path registration

Files:
- `rust/src/optimizers/uniswap_engine.rs` — remove `assert!(!self.running)` from `register_path`, remove `running` field, remove `freeze()`, remove `initial_solve()`
- `rust/src/optimizers/v2_block_engine.rs` — remove `running` field, `start()`, `is_running()`, assertion guards
- `rust/src/optimizers/v3_block_engine.rs` — same as V2
- `rust/src/optimizers/v4_block_engine.rs` — same as V2

Update tests that rely on `register_path_after_start_panics` — these should now succeed.

### Slice 2: Eager Path Registration (Rust)

**TODO-bed3d185**: Add `register_and_solve_path` with eager solving

Files:
- `rust/src/optimizers/uniswap_engine.rs` — new method + `pending_new_paths: HashSet<u64>` field
- Update `rebuild_and_solve_affected` to merge pending new paths, then clear the set
- PyO3 exposure + Python `EngineRegistry` update

### Slice 3: Mint/Burn Event Buffer (Rust)

**TODO-95e9a50c**: Implement Mint/Burn event buffer for unregistered pools

Files:
- `rust/src/optimizers/v3_block_engine.rs` — buffer keyed by contract address
- `rust/src/optimizers/v4_block_engine.rs` — buffer keyed by (pool_manager, pool_id)
- `rust/src/optimizers/uniswap_engine.rs` — route Mint/Burn events to sub-engines regardless of registration status
- Update `register_pool` in sub-engines to claim and apply buffered events eagerly

### Slice 4: Buffer Configuration (Rust)

**TODO-b79ba51b**: Add configurable buffer staleness limit with runtime control

- Construction-time `max_buffer_age` parameter (default: `None` = unbounded)
- Runtime `set_event_buffer_max_age(blocks)` method
- Runtime `flush_event_buffer()` method
- Expiry checked during `process_block`
- PyO3 exposure

### Slice 5: Pump Subscribe/Resume Split (Rust)

**TODO-e4466968**: Split pump `start()` into `subscribe()` + `resume()` phases

Files:
- `rust/src/optimizers/uniswap_engine_pump.rs` — refactor to two-phase lifecycle
- `rust/src/optimizers/uniswap_engine.rs` — new `subscribe()` and `resume()` PyO3 methods
- `EventType` enum for configurable inclusion set
- Buffer events during subscribe phase using same mechanism as Slice 3
- `subscribe()` returns first observed block number

### Slice 6: Per-Block Address Filter (Rust)

**TODO-544feb72**: Replace frozen address filter with per-block snapshot

Files:
- `rust/src/optimizers/uniswap_engine_pump.rs` — replace `collect_relevant_addrs` (once at startup) with per-block snapshot pattern

### Slice 7: Python Bot Refactoring

**TODO-6028afeb**: Refactor Python bot for rolling-start startup sequence

Files:
- `examples/eth_backrun_v2_v3_v4_rust.py` — new startup sequence
- `EngineRegistry` — branch on pump state for `register_path` vs `register_and_solve_path`
- `build_paths()` → background `asyncio.Task`
- Per-pool verification
- Thread-safety note on Python key dicts

### Future (Not on Critical Path)

- **TODO-0324058e**: Engine-level reorg checks with journal-based rollback
- **TODO-9356aaf7**: Eliminate Python pool mutable state for engine-managed pools

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Mutex contention between pump and late registration | Low | Low | Single `parking_lot::Mutex`, serialized, no deadlock. Registration is O(ms). |
| `rebuild_and_solve_affected` discards pending new paths | Medium | High | `pending_new_paths` set tracks them for merge (Slice 2) |
| Stale tick data for late-registered CL pools | Low | Medium | Registration state is current-block RPC data; Mint/Burn buffer covers post-resume gap |
| Missed blocks between backfill and resume | Low | Critical | `subscribe()` → observe block → backfill → `resume()` ordering guarantees no gap |
| Event buffer memory growth | Low | Medium | Configurable staleness limit (Slice 4); unbounded default is acceptable for typical path-loading windows |
| Pump address snapshot races with registration | Low | Low | Swap/Sync safe to discard; Mint/Burn caught by buffer |

## Status

- [x] Slice 1: Remove registration guards
- [x] Slice 2: Eager path registration
- [x] Slice 3: Mint/Burn event buffer
- [x] Slice 4: Buffer configuration
- [x] Slice 5: Pump subscribe/resume split
- [x] Slice 6: Per-block address filter
- [x] Slice 7: Python bot refactoring
