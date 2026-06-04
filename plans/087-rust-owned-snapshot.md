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

Python transfers the full snapshot (pool identifiers + tick_data + tick_bitmaps + metadata + snapshot_block) to Rust in one call. Rust owns the snapshot after transfer.

```python
# Python side
engine.load_v3_snapshot(
    pools=[
        V3SnapshotPool(address=..., tick_data=..., tick_bitmap=..., sqrt_price=..., liquidity=..., tick=...),
    ],
    snapshot_block=25241000,
)

engine.load_v4_snapshot(
    pools=[
        V4SnapshotPool(pool_manager=..., pool_id=..., tick_data=..., tick_bitmap=..., sqrt_price=..., liquidity=..., tick=..., pool_key=...),
    ],
    snapshot_block=25241000,
)
```

Rust stores the snapshot internally for reconstructing pool state at registration time. Requires `subscribe()` to have been called first (needs AlloyProvider + first WS block to determine gap range).

### 2. Automatic backfill between `load_snapshot()` and `resume()`

After snapshot is loaded, Rust calls `getLogs(snapshot_block → first_ws_block)` for all relevant topics with **no address filter** (catch-all). Returns events for every pool on-chain; Rust matches against the snapshot set. Events for unregistered pools go into the buffer (the "held" concept extended to ModifyLiquidity).

Pagination: split block range into chunks of ~2000 blocks per request, using existing `AlloyProvider::get_logs()` with retry-backoff.

Topics:
- V3: `Swap`, `Mint`, `Burn`
- V4: `Swap`, `ModifyLiquidity`

### 3. Minimal `register_pool()` / `register_v4_pool()` API

Python calls register with just the pool identifier + pool_key metadata (no tick_data). Rust reconstructs initial state from snapshot + buffered events.

```python
# V3
engine.register_v3_pool(
    address="0x...",
    token0="0x...", token1="0x...",
    fee=3000, tick_spacing=60, factory="0x...",
    sqrt_price_x96=..., liquidity=..., tick=...,
)

# V4
engine.register_v4_pool(
    pool_id_hex="0x...",
    pool_manager="0x...",
    currency0="0x...", currency1="0x...",
    fee=3000, tick_spacing=60, hook_flags=0,
    sqrt_price_x96=..., liquidity=..., tick=...,
)
```

Rust applies buffered events on top of snapshot state during registration, completing the tick_data. Re-enables the buffer-application code that was temporarily disabled.

### 4. Python filtering stays in Python

Hook filtering (mask 0xCC) and dynamic-fee rejection remain in Python. Python simply doesn't call `register_pool()` for rejected pools. Rust is permissive — it optimistically registers any pool requested.

### 5. Remove Python-side backfill

`fetch_snapshot_events()` and `pending_updates()` calls are removed from `build_paths()`. Python builds pools with DB snapshot state, registers them, Rust applies the event stream.

### 6. Snapshot pruning and buffer management

- Event buffer: **unlimited** by default during development
- `flush_event_buffer()`: Python calls after path loading completes to discard buffered events for pools that were never registered
- Future TODO: lazy SQLite snapshot loading — Rust reads tick_data from DB on-demand instead of bulk transfer from Python

### Design decisions

- **Snapshot is a one-time transfer**: Python hands off complete data, Rust owns it after. Python can continue as a lightweight operator.
- **Catch-all getLogs**: Simpler than targeted address filtering. One request returns everything in a block range; Rust matches against known pools. Works for both V3 (events from individual pool contracts) and V4 (events from PoolManager).
- **Separate `load_v3_snapshot` / `load_v4_snapshot`**: Different data structures, different event topics, different matching logic. Clearer API than a tagged union.
- **`subscribe()` must precede `load_snapshot()`**: Engine needs the AlloyProvider and first WS block to compute the backfill range. Enforced with a runtime check.
- **Buffer-application is re-enabled**: The double-counting bug is fixed by removing `pending_updates()` from Python, not by discarding events. Rust is the single event applier.

## Files Involved

**Primary:**
- `rust/src/optimizers/v3_block_engine.rs` — `load_v3_snapshot()`, backfill logic, re-enable buffer application in `register_pool()`, remove tick_data param
- `rust/src/optimizers/v4_block_engine.rs` — `load_v4_snapshot()`, backfill logic, re-enable buffer application in `register_pool()`, remove tick_data param
- `rust/src/optimizers/uniswap_engine.rs` — PyO3 bindings for new methods, update `register_v3_pool`/`register_v4_pool` signatures
- `examples/eth_backrun_v2_v3_v4_rust.py` — Replace `fetch_snapshot_events()` + `pending_updates()` with `load_v3_snapshot()` / `load_v4_snapshot()`, simplify `register_pool` calls, remove `build_paths` backfill loop

**Secondary:**
- `rust/src/provider.rs` — Ensure `get_logs()` handles large paginated ranges for backfill
- `rust/src/bot_core/liquidity_verifier.rs` — No changes needed (verification already working correctly)

**No change needed:**
- `src/degenbot/uniswap/v3_snapshot.py` — Still used for standalone Python usage; bot script stops calling `pending_updates()`
- `src/degenbot/uniswap/v4_snapshot.py` — Same as above

## Implementation Order

### Slice 1: Rust snapshot storage + `load_v3_snapshot` / `load_v4_snapshot`

1. Add `V3SnapshotPool` / `V4SnapshotPool` structs to Rust engines
2. Add `load_v3_snapshot()` / `load_v4_snapshot()` methods that store the snapshot data and validate `subscribe()` was called
3. Run: `just test-rust` — expect all 528 tests pass

### Slice 2: Rust backfill via `getLogs`

1. Add backfill method to each engine: paginated `getLogs` from `snapshot_block` to `first_ws_block`
2. Apply matching events to registered pools, buffer for unregistered
3. Integrate into `load_snapshot()` (runs automatically after snapshot is loaded)
4. Run: `just test-rust` — expect all tests pass + new backfill tests

### Slice 3: Minimal `register_pool()` with Rust-owned state reconstruction

1. Change `register_pool()` to accept pool identifier + pool_key metadata only (no tick_data)
2. Re-enable buffer application: on registration, Rust constructs initial state from snapshot tick_data, then applies buffered events
3. Remove the buffer-discard workaround from V3/V4 engines
4. Update PyO3 bindings for new signatures
5. Run: `just test-rust` — expect all tests pass (update tests that pass tick_data)

### Slice 4: Python integration

1. Replace `fetch_snapshot_events()` + `pending_updates()` with `load_v3_snapshot()` / `load_v4_snapshot()` calls
2. Simplify `register_pool` calls in `build_paths()` — remove tick_data, remove per-pool backfill loop
3. Add `flush_event_buffer()` call after path loading completes
4. Run: `just test-python` + `just test-rust-python` — expect all pass

### Slice 5: Validate and clean up

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
fn load_v3_snapshot_stores_pool_data() { ... }

#[test]
fn v3_backfill_applies_events_to_registered_pools() { ... }

#[test]
fn v3_backfill_buffers_events_for_unregistered_pools() { ... }

#[test]
fn v3_register_pool_reconstructs_from_snapshot_plus_buffer() { ... }

// v4_block_engine tests
#[test]
fn load_v4_snapshot_stores_pool_data() { ... }

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

[ ] Slice 1: Rust snapshot storage + `load_v3_snapshot` / `load_v4_snapshot`
[ ] Slice 2: Rust backfill via `getLogs`
[ ] Slice 3: Minimal `register_pool()` with Rust-owned state reconstruction
[ ] Slice 4: Python integration
[ ] Slice 5: Validate and clean up
