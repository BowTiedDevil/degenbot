# Plan 099: Split Event Buffer for Race-Free Verification

## Overview

Split each block engine's single `liquidity_event_buffer` into two — a `backfill_event_buffer` (populated during `backfill_from_snapshot`, never expired) and a `pump_event_buffer` (populated by the WS pump, expired normally). This enables deterministic two-phase verification at `register_pool` time: (1) raw DB tick_data against the snapshot block, and (2) post-backfill state against the backfill boundary block, both clonable at known-deterministic points under the engine lock before the pump can interleave.

## Problem

### Deletion test

If the split were removed (reverted to a single buffer), verification at `register_pool` time would remain racy — the single buffer mixes backfill events (known block range, deterministic) with pump events (unbounded, concurrent), so after buffer application the pool state is at an indeterminate block and cannot be verified against any fixed on-chain block number.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| Verification against wrong block | `uniswap_engine.rs` `register_v3_pool` / `register_v4_pool` | After buffer application, engine state may be past the backfill boundary (pump advanced it), so verification against `backfill_block` produces false failures or false passes |
| False-positive RuntimeError shutdowns | Bot startup during `build_paths` | V4 pool verification against backfill block 25254261 failed with engine value 700276514854662 vs on-chain 304826122264464 — the engine was correct, just at a later block |
| No way to verify event pipeline independently | `v3_block_engine.rs` / `v4_block_engine.rs` | Currently impossible to answer: "did the backfill events apply correctly?" — the single buffer conflates backfill and pump events |
| `verify_backfill_block` field captures wrong value | `uniswap_engine.rs` line ~2541 | Set once during `backfill_from_snapshot` but by the time pools are registered, `last_processed_block()` has advanced past it |
| Verification block must be "tweaked" per incident | Repeated fixes to block selection | Root cause is structural (mixed buffer), not a parameter tuning problem |

## Solution

### Step 1: Split `liquidity_event_buffer` into two containers

Replace the single `HashMap<K, Vec<BufferedEvent>>` with two fields:

```rust
// Before (V3BlockEngine):
liquidity_event_buffer: HashMap<Address, Vec<BufferedV3LiquidityUpdate>>,

// After:
backfill_event_buffer: HashMap<Address, Vec<BufferedV3LiquidityUpdate>>,
pump_event_buffer: HashMap<Address, Vec<BufferedV3LiquidityUpdate>>,
```

Same pattern for `V4BlockEngine` with key type `(Address, PoolId)`.

**Routing rule**: `process_backfill_logs()` appends to `backfill_event_buffer`; `process_block()` appends to `pump_event_buffer`. This is clean because the two methods are called from disjoint phases — `process_backfill_logs` only during `backfill_from_snapshot`, `process_block` only from the pump.

In `apply_liquidity_update()`, when a pool is not registered, the event is routed to `pump_event_buffer` (since `apply_liquidity_update` is only called from `process_block` / the pump path).

### Step 2: Replace inline buffer drain with staged application

Currently `register_pool()` drains the single buffer inline. Replace with two explicit stages:

```rust
// Before (V3BlockEngine::register_pool):
if let Some(buffered) = self.liquidity_event_buffer.remove(&address) {
    for update in buffered {
        // apply tick updates
    }
}

// After: remove inline drain from register_pool.
// New methods called from uniswap_engine.rs under the lock:
pub fn apply_backfill_buffer(&mut self, address: &Address) { ... }
pub fn apply_pump_buffer(&mut self, address: &Address) { ... }
```

`register_pool()` now ONLY creates the pool entry — no buffer application. The caller (`uniswap_engine.rs`) sequences the stages.

### Step 3: Two-phase verification in `register_v3_pool` / `register_v4_pool`

Under the engine lock, the registration now proceeds in three steps:

```
1. register_pool(params)         → pool created with DB tick_data
   Clone tick_data               → snapshot_verify_data (state at snapshot block)
2. apply_backfill_buffer()       → pool at backfill boundary
   Clone pool state              → backfill_verify_data (state at backfill block)
3. apply_pump_buffer()           → pool at pump's current block (ready for solving)
```

After the lock is released, both clones are verified outside the lock:

```
4. Verify snapshot_verify_data against on-chain at snapshot block
5. Verify backfill_verify_data against on-chain at backfill block
6. If either fails → RuntimeError (immediate shutdown)
```

### Step 4: Separate expiry policies

| Buffer | Expiry | Reason |
|--------|--------|--------|
| `backfill_event_buffer` | Never | Fixed block range from backfill. Must survive until all snapshot pools are registered. Bounded by the backfill block range, so no unbounded memory growth. |
| `pump_event_buffer` | Existing `event_buffer_max_age` logic | Same as current single buffer — drop stale events for pools that never get registered. |

`expire_buffered_events()` only touches `pump_event_buffer`. `clear_event_buffer()` clears both.

### Step 5: Remove `verify_backfill_block` field

The verification blocks are now structurally guaranteed:

- **Snapshot block**: the `snapshot_block` parameter to `backfill_from_snapshot()`.
- **Backfill block**: `first_ws_block - 1` (the last backfill block, which is exactly what `last_processed_block()` returns after backfill and before the pump resumes).

Both are already stored internally (`verify_snapshot_block`, `verify_backfill_block`). But `verify_backfill_block` is now truly deterministic (it's always `first_ws_block - 1`), so it's correct. No Python-side setting needed — both are captured in `backfill_from_snapshot()`.

### Design decisions

- **Two separate fields, not a single tagged buffer**: A `Vec<(BufferedEvent, BufferOrigin)>` would avoid duplicating the HashMap but complicates expiry (must filter by origin) and makes the staged application harder to reason about. Two clean containers are simpler.
- **`register_pool()` does NOT apply buffers**: The current inline drain is a side effect hidden inside a "register" method. Separating allocation from buffer application gives the caller explicit control over the staging points where verification clones are captured.
- **New methods `apply_backfill_buffer` / `apply_pump_buffer`**: Public, called by `uniswap_engine.rs` under the lock. Each drains and applies its respective buffer for one pool. This replaces the previous implicit drain.
- **Backfill buffer never expired**: The backfill covers a fixed range (typically ~5000 blocks). The number of pools is bounded. This buffer is small and temporary — it drains pool-by-pool during `build_paths` and is empty once all snapshot pools are registered.
- **Pump pool-registered check**: In `apply_liquidity_update()`, the current code checks if the pool is registered and either applies directly or buffers. This logic stays the same, just routing to `pump_event_buffer` for the unregistered case.

## Files Involved

**Primary:**
- `rust/src/optimizers/v3_block_engine.rs` — split buffer, add `apply_backfill_buffer` / `apply_pump_buffer`, remove inline drain from `register_pool`
- `rust/src/optimizers/v4_block_engine.rs` — same split
- `rust/src/optimizers/uniswap_engine.rs` — update `register_v3_pool` / `register_v4_pool` to use staged application + two-phase verification; remove `set_verify_block` / `set_verify_snapshot_block` / `set_verify_backfill_block` Python methods (blocks captured internally)

**Secondary:**
- `rust/src/bot_core/liquidity_verifier.rs` — already has `verify_v3_liquidity_map` and `verify_v4_liquidity_map` for snapshot check; `verify_v3_pools` / `verify_v4_pools` for full state check. No changes needed.

**No change needed:**
- `examples/eth_backrun_v2_v3_v4_rust.py` — already updated to not call `set_verify_block`; verification is fully engine-owned
- `rust/src/optimizers/uniswap_engine_pump.rs` — pump calls `process_block()` which routes to `pump_event_buffer` automatically

## Implementation Order

### Slice 1: Split V3 buffer with staged application

1. Add `backfill_event_buffer` and `pump_event_buffer` fields to `V3BlockEngine`
2. Route `process_backfill_logs` events to `backfill_event_buffer`; route `process_block` / `apply_liquidity_update` unregistered events to `pump_event_buffer`
3. Remove inline buffer drain from `register_pool()`
4. Add `apply_backfill_buffer(&mut self, address: &Address)` and `apply_pump_buffer(&mut self, address: &Address)`
5. Update `expire_buffered_events()` to only touch `pump_event_buffer`
6. Update `clear_event_buffer()` and `event_buffer_len()` to cover both
7. Run: `just test-rust` — expect all existing tests to pass (V3 buffer behavior unchanged from external perspective)

### Slice 2: Split V4 buffer with staged application

1. Same changes as Slice 1 but for `V4BlockEngine`
2. Run: `just test-rust` — expect all tests green

### Slice 3: Wire staged application into `register_v3_pool` / `register_v4_pool`

1. In `register_v3_pool`: call `register_pool()`, then `apply_backfill_buffer()`, then `apply_pump_buffer()` — all under the engine lock
2. Clone tick_data before `apply_backfill_buffer` for snapshot verification
3. Clone pool state after `apply_backfill_buffer` (before `apply_pump_buffer`) for backfill verification
4. Release lock, then verify both clones against on-chain
5. Same for `register_v4_pool`
6. Remove the `verify_backfill_block` field and `set_verify_snapshot_block` / `set_verify_backfill_block` Python methods — blocks are captured internally in `backfill_from_snapshot()`
7. Run: `just test-rust` — expect green

### Slice 4: End-to-end verification with live bot

1. Start the bot with `verify_on_register=True`
2. Confirm zero false-positive verification failures
3. Confirm snapshot and backfill checks both execute (check log output)
4. Run: `just test-all` — expect green

### Slice 5: Validate and clean up

1. Run `just lint` + `just test-all`
2. Remove any dead code from the single-buffer era
3. Update `v3_block_engine.rs` / `v4_block_engine.rs` doc comments

## Testing

### Per-slice test runs

Each slice runs `just test-rust`. The changes are internal to the Rust engines; Python tests are unaffected until Slice 3.

### New unit tests

```rust
// v3_block_engine.rs — test that backfill and pump events route to separate buffers

#[test]
fn test_backfill_and_pump_buffers_separate() {
    let mut engine = V3BlockEngine::new();
    // Register a pool (no buffers yet)
    let key = engine.register_pool(params);
    // Process backfill logs → routed to backfill_event_buffer
    engine.process_backfill_logs(&backfill_logs, block);
    // Process pump block → routed to pump_event_buffer
    engine.process_block(&pump_logs, block, &metadata);
    // Backfill buffer has events from backfill range only
    // Pump buffer has events from pump range only
    assert!(engine.backfill_event_buffer.contains_key(&addr));
    assert!(engine.pump_event_buffer.contains_key(&addr));
}

#[test]
fn test_staged_application_verification_clones() {
    // Register pool, apply backfill buffer, clone, apply pump buffer
    // Verify cloned state matches what backfill would produce (not pump)
}
```

### Integration tests

Existing bot startup test covers the full flow: snapshot load → backfill → `build_paths` → verification. Slice 4 validates this explicitly with the live bot.

## Benefits

- **Determinism**: Verification clones are captured at known-deterministic points under the lock, eliminating the race condition that caused false-positive RuntimeError shutdowns.
- **Locality**: Each buffer has a single writer (`process_backfill_logs` → backfill, `process_block` → pump), making the data flow obvious.
- **Separate concerns**: Snapshot loading bugs and event pipeline bugs are independently identifiable — the failing check tells you which path is broken.
- **Remove Python config**: Verification blocks are determined by the engine lifecycle, not set by the caller. Removes `set_verify_block`, `set_verify_snapshot_block`, `set_verify_backfill_block` from the Python API.

## Risks

- **Lock held longer during registration**: The staged application (register → apply backfill → apply pump) happens under the engine lock. However, the actual compute is tiny (iterate buffered tick updates) — the RPC calls for verification happen outside the lock. Negligible impact.
- **Backfill buffer memory**: The backfill buffer is never expired, but it covers a fixed block range (typically ~5000 blocks) and a bounded set of pools. It drains pool-by-pool during `build_paths` and is empty once registration completes. No unbounded growth.
- **Existing tests rely on single-buffer behavior**: Tests that call `register_pool()` and expect the buffer to be applied inline will need updating — `register_pool()` no longer applies buffers, the caller must call `apply_backfill_buffer()` / `apply_pump_buffer()` explicitly.

## Relationship to Other Plans

- **Plan 098** (snapshot transfer to Rust): This plan builds on 098's lifecycle phases and snapshot loading. The split buffer operates within the same phase boundaries — `process_backfill_logs` runs during the `Backfilled` phase, `process_block` runs after `Resumed`.
- **Plan 085** (rolling start): Established the rolling-start pattern where the pump runs concurrently with `build_paths`. This plan resolves the race condition that pattern introduced for verification.

## Status

[x] Slice 1: Split V3 buffer with staged application
[x] Slice 2: Split V4 buffer with staged application
[x] Slice 3: Wire staged application into register_v3_pool / register_v4_pool with two-phase verification
[x] Slice 4: End-to-end verification with live bot
[x] Slice 5: Validate and clean up
