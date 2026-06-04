# Plan 087: Rust-Owned Liquidity Snapshot

## Overview

Transfer full ownership of pool liquidity state from Python to the Rust engine. The Rust engine receives the DB snapshot at startup, performs its own `getLogs` backfill against the snapshot address set, and applies all liquidity events (backfill + live) as the single source of truth. Python becomes a lightweight operator that only decides *which* pools participate in the solver via minimal `register_pool()` calls.

## Problem

### Deletion test

If you deleted `fetch_snapshot_events()`, `pending_updates()`, and the per-pool backfill logic in `build_paths()`, Python would no longer apply ModifyLiquidity events to pool objects. Currently this would leave the Rust engine with stale tick_data, because Python passes tick_data at registration time and Rust buffers the same events for later application — causing double-counting (2x liquidityGross). The fix is to make Rust the sole owner: Python provides the snapshot, Rust applies all events.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| Double-counting bug | `v4_block_engine.rs:register_pool` applies buffered events on top of Python's already-applied tick_data | Engine has 2x on-chain values, verification catches it |
| Same bug exists for V3 | `v3_block_engine.rs:register_pool` same pattern | V3 not yet hit in production but same root cause |
| Two event application sites | Python `pending_updates()` + Rust `apply_liquidity_update()` | Must coordinate who applied what — hidden contract |
| Backfill is Python-orchestrated | `fetch_snapshot_events()`, `pending_updates()` in `build_paths()` | Complex, slow, blocks the event loop with synchronous DB + RPC |

## Solution

### 1. New `load_v3_snapshot()` / `load_v4_snapshot()` methods on Rust engine

Python transfers the liquidity snapshot to Rust. The snapshot contains **only** persistent liquidity state: pool identifiers, tick_data (initialized ticks with liquidity_gross/liquidity_net), tick_bitmap (word→bitmap), and the snapshot_block. It does **not** contain mutable scalar state (sqrt_price_x96, liquidity, tick) — those come from the DB pool record and are passed at registration time.

```python
# Python side
engine.load_v3_snapshot(
    pools=[
        {
            "address": "0x...",
            "tick_data": {tick: (liquidity_gross, liquidity_net), ...},
            "tick_bitmap": {word: bitmap, ...},
        },
        ...
    ],
    snapshot_block=25241000,
)

engine.load_v4_snapshot(
    pools=[
        {
            "pool_manager": "0x...",
            "pool_id": "0x...",
            "tick_data": {tick: (liquidity_gross, liquidity_net), ...},
            "tick_bitmap": {word: bitmap, ...},
        },
        ...
    ],
    snapshot_block=25241000,
)
```

Rust stores the snapshot internally for reconstructing tick state at registration time. Requires `subscribe()` to have been called first (needs AlloyProvider + first WS block to determine gap range). Raises `RuntimeError` if called more than once — Python must transfer the full snapshot in a single call.

### 2. Automatic backfill between `load_snapshot()` and `resume()`

After snapshot is loaded, Rust calls `getLogs` for all relevant topics with **no address filter** (catch-all). Returns events for every pool on-chain; Rust matches against the snapshot set. Events for unregistered pools go into the buffer (the "held" concept extended to ModifyLiquidity).

Block range: `snapshot_block + 1` (inclusive) to `first_ws_block` (inclusive). The snapshot is valid at `snapshot_block` — any events *at or before* that block are already reflected in the tick_data, so there is no overlap. Per the Ethereum JSON-RPC spec, `eth_getLogs` ranges are inclusive on both ends.

Pagination: split block range into chunks of ~2000 blocks per request, using existing `AlloyProvider::get_logs()` with retry-backoff.

Topics:
- V3: `Swap`, `Mint`, `Burn`
- V4: `Swap`, `ModifyLiquidity`

### 3. Minimal `register_pool()` / `register_v4_pool()` API

Python calls register with pool identifier + pool_key metadata + scalar state (sqrt_price_x96, liquidity, tick from the DB pool record). No tick_data — Rust reconstructs tick state from its stored snapshot + buffered events.

```python
# V3
engine.register_v3_pool(
    address="0x...",
    token0="0x...", token1="0x...",
    fee=3000, tick_spacing=60, factory="0x...",
    sqrt_price_x96=..., liquidity=..., tick=...,
    block=...,
)

# V4
engine.register_v4_pool(
    pool_manager="0x...",
    pool_id_hex="0x...",
    currency0="0x...", currency1="0x...",
    fee=3000, tick_spacing=60,
    sqrt_price_x96=..., liquidity=..., tick=...,
    block=...,
)
```

The scalars (sqrt_price_x96, liquidity, tick) come from the DB pool record, not the liquidity snapshot. They represent the pool's on-chain state at the DB's last update — sufficient for engine initialization. Live swap events from the pump update them post-registration.

Rust applies buffered liquidity events on top of snapshot tick_data during registration, completing the tick state. This replaces the buffer-discard workaround.

No `hook_flags` parameter — Rust is permissive. Hook filtering (mask 0xCC) and dynamic-fee rejection are done entirely in Python; if a pool is rejected, `register_v4_pool` is simply never called.

### 4. Remove Python-side backfill

`fetch_snapshot_events()` and `pending_updates()` calls are removed from `build_paths()`. Python builds pools with DB snapshot state, registers them, Rust applies the event stream.

### 5. Snapshot pruning and buffer management

- Event buffer: **unlimited** by default during development
- `flush_event_buffer()`: Python calls after path loading completes to discard buffered events for pools that were never registered
- Future TODO: lazy SQLite snapshot loading — Rust reads tick_data from DB on-demand instead of bulk transfer from Python

### Design decisions

- **Rust is permissive at registration**: No `hook_flags` parameter in `register_v4_pool`. Hook filtering and dynamic-fee rejection are Python's responsibility — unaccepted pools are never registered. Rust trusts the caller.
- **Snapshot carries only liquidity positions**: The liquidity snapshot contains tick_data and tick_bitmap — the persistent positions. It does not contain mutable scalar state (sqrt_price_x96, liquidity, tick). Those come from the DB pool record and are passed at registration time via `register_pool()`.
- **Snapshot is a one-time transfer**: Python hands off the liquidity map, Rust owns it after. Python continues as a lightweight operator that passes scalars and metadata at registration time.
- **Catch-all getLogs**: Simpler than targeted address filtering. One request returns everything in a block range; Rust matches against known pools. Works for both V3 (events from individual pool contracts) and V4 (events from PoolManager).
- **Separate `load_v3_snapshot` / `load_v4_snapshot`**: Different data structures, different event topics, different matching logic. Clearer API than a tagged union.
- **`subscribe()` must precede `load_snapshot()`**: Engine needs the AlloyProvider and first WS block to compute the backfill range. Enforced with a runtime check.

## Files Involved

**Primary:**
- `rust/src/optimizers/v3_block_engine.rs` — `load_v3_snapshot()`, backfill logic, re-enable buffer application in `register_pool()`, remove tick_data param, remove `sync_pool_state()`
- `rust/src/optimizers/v4_block_engine.rs` — `load_v4_snapshot()`, backfill logic, re-enable buffer application in `register_pool()`, remove tick_data param, remove `sync_pool_state()`, remove `hook_flags` param from `register_pool()`
- `rust/src/optimizers/uniswap_engine.rs` — PyO3 bindings for new methods, update `register_v3_pool`/`register_v4_pool` signatures
- `examples/eth_backrun_v2_v3_v4_rust.py` — Replace `fetch_snapshot_events()` + `pending_updates()` with `load_v3_snapshot()` / `load_v4_snapshot()`, simplify `register_pool` calls, remove `build_paths` backfill loop

**Secondary:**
- `rust/src/provider.rs` — Ensure `get_logs()` handles large paginated ranges for backfill

**No change needed:**
- `src/degenbot/uniswap/v3_snapshot.py` — Still used for standalone Python usage; bot script stops calling `pending_updates()`
- `src/degenbot/uniswap/v4_snapshot.py` — Same as above
- `rust/src/bot_core/liquidity_verifier.rs` — Verification already working correctly

## Implementation Order

### Slice 1: Rust snapshot storage + `register_pool` uses snapshot + buffer application

1. Add `load_v3_snapshot()` / `load_v4_snapshot()` methods that store tick_data + tick_bitmap per pool and validate `subscribe()` was called
2. Add Rust backfill: paginated `getLogs(snapshot_block + 1 to first_ws_block, inclusive)` for all relevant topics with no address filter. Events for registered pools are applied immediately; events for unregistered pools are buffered in `liquidity_event_buffer`
3. Change `register_pool()` to accept pool identifier + pool_key metadata + scalar state only (no `tick_data`). On registration, Rust constructs initial tick state from its stored snapshot, then applies buffered liquidity events on top — **replacing** the buffer-discard workaround
4. Delete `liquidity_event_buffer.remove()` calls from V3/V4 `register_pool` — replaced by proper buffer application
5. Update PyO3 bindings for new `register_pool` signatures (remove `tick_data` param)
6. Run: `just test-rust` — expect all tests pass (update tests that pass `tick_data`)

### Slice 2: Python integration

1. Replace `fetch_snapshot_events()` + `pending_updates()` with `load_v3_snapshot()` / `load_v4_snapshot()` calls
2. Simplify `register_pool` calls in `build_paths()` — remove `tick_data`, remove per-pool backfill loop (`pending_updates` + `update_liquidity_map`)
3. Add `flush_event_buffer()` call after path loading completes
4. Run: `just test-python` + `just test-rust-python` — expect all pass

### Slice 3: Validate and clean up

1. Run `just test-all` + `just lint`
2. Verify per-pool verification still catches real mismatches
3. Update `CONTEXT.md` if terminology changed
4. Remove any deprecated shims introduced during migration

## Testing

### Per-slice test runs

Each slice runs `just test-rust` for Rust changes, `just test-python` for Python changes.

### New unit tests

```rust
// v3_block_engine tests
#[test]
fn load_v3_snapshot_stores_tick_data_and_bitmap() { ... }

#[test]
fn v3_backfill_applies_events_to_registered_pools() { ... }

#[test]
fn v3_backfill_buffers_events_for_unregistered_pools() { ... }

#[test]
fn v3_register_pool_reconstructs_from_snapshot_plus_buffer() { ... }

// v4_block_engine tests
#[test]
fn load_v4_snapshot_stores_tick_data_and_bitmap() { ... }

#[test]
fn v4_backfill_applies_modify_liquidity_to_registered_pools() { ... }

#[test]
fn v4_backfill_buffers_modify_liquidity_for_unregistered_pools() { ... }

#[test]
fn v4_register_pool_reconstructs_from_snapshot_plus_buffer() { ... }
```

### Integration tests

Existing `just test-rust-python` tests cover the Python↔Rust integration. The bot script test (running against a mainnet fork) validates end-to-end.

## Benefits

- **Depth**: Removes the shallow Python↔Rust coordination seam for event application. Rust owns the full event stream from snapshot to live.
- **Locality**: All liquidity state management lives in one place (Rust engine). No split responsibility.
- **Leverage**: Single `getLogs` catch-all replaces per-pool Python fetch + apply. One RPC call pattern instead of N.
- **Correctness**: Eliminates the double-counting class of bugs entirely. No coordination contract to violate.

## Risks

- **Memory**: Rust holds tick_data for ALL snapshot pools plus buffered events for unregistered pools. Mitigated by `flush_event_buffer()` after path loading and unlimited buffer during development.
- **getLogs latency**: Large block ranges may take time. Backfill is a one-time startup cost, acceptable if it completes in <30s. Can be measured and optimized with chunk sizes.
- **V3 address matching**: Catch-all `getLogs` with no address filter returns V3 events for ALL V3 pools (not just snapshot pools). Rust filters by matching against snapshot set. Slight bandwidth waste but simpler logic.

## Relationship to Other Plans

- **Plan 086** (result batch channel): Completed. This plan builds on the pump + channel infrastructure.
- **Plan 085** (rolling start): Completed. This plan replaces the Python-side rolling-start backfill with Rust-native backfill.
- **Future TODO**: Lazy SQLite snapshot loading in Rust — eliminates the bulk Python→Rust snapshot transfer entirely. Not in scope for this plan.

## Status

[x] Slice 1: Rust snapshot storage + backfill + register_pool uses snapshot + buffer application
[x] Slice 2: Python integration
[x] Slice 3: Validate and clean up
