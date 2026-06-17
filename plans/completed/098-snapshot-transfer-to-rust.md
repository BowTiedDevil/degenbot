# Plan 098: Bulk Snapshot Transfer to Rust Engine

## Deletion Test

If this plan's feature were removed, the system would revert to per-pool `tick_data` dict passing across the PyO3 boundary during `register_v3_pool` / `register_v4_pool`. That works, but incurs O(N) GIL-held iterations for N pools, each unpacking a Python dict into a Rust HashMap — hundreds of thousands of individual `extract()` calls for pools with many initialized ticks.

## Friction Table

| Friction | Impact | Frequency | Evidence |
|----------|--------|-----------|----------|
| Per-pool tick_data dict crossing PyO3 boundary | Slow: each dict iterates all ticks, calling `extract()` per entry | Every pool registration (~100-500 V3 pools, ~50-200 V4 pools) | `for (key, value) in tick_data.iter()` loop in `uniswap_engine.rs:2682` |
| Python-side snapshot dissection before registration | Unnecessary Python work: converting `LiquidityAtTick` → `(u128, i128)` tuples per tick | Every pool in `build_paths()` | `engine_tick_data = {idx: (info.liquidity_gross, info.liquidity_net) ...}` in example line 873 |
| `apply_buffer` inferred by Python | Python must determine snapshot availability and set boolean correctly; footgun if wrong | Every V3/V4 pool registration | `if snap_td is not None: ... apply_buffer=True else: ... apply_buffer=False` in example line 866-879 |
| No ordering enforcement | `load_snapshot` / `backfill` / `resume` can be called out of order, causing silent data loss | Startup (once, but catastrophic if wrong) | Ordering only documented in AGENTS.md, no runtime guard |

## Design

### Core idea

Python serializes the complete V3 and V4 snapshots into binary buffers and passes each to Rust via a single `memcpy`. Rust deserializes into typed HashMaps, stores the snapshot internally, and extracts tick_data at registration time. Rust tracks the engine lifecycle via a state machine that enforces correct ordering.

### Binary serialization format

#### V3 format

```
[1 byte: version]
[4 bytes LE: pool_count]
Per pool:
  [20 bytes: pool address]
  [4 bytes LE: tick_count]
  Per tick:
    [4 bytes LE: tick_index (i32)]
    [16 bytes LE: liquidity_gross (u128)]
    [16 bytes LE: liquidity_net (i128)]
```

#### V4 format

```
[1 byte: version]
[4 bytes LE: pool_manager_count]
Per pool_manager:
  [20 bytes: pool_manager address]
  [4 bytes LE: pool_id_count]
  Per pool_id:
    [32 bytes: pool_id]
    [4 bytes LE: tick_count]
    Per tick:
      [4 bytes LE: tick_index (i32)]
      [16 bytes LE: liquidity_gross (u128)]
      [16 bytes LE: liquidity_net (i128)]
```

A `tick_count` of zero is valid — it means the pool has no initialized ticks (genuinely illiquid). The `PoolTickCoverage` enum distinguishes this from a pool absent from the snapshot.

The version byte supports future format changes (e.g., adding `tick_bitmap` records) without breaking deserialization. `tick_bitmap` is omitted from the initial format — it's an on-chain optimization for limiting RPC calls, not needed by the solver which computes tick ranges from `tick_data` directly.

### Pydantic-validated serialization

The snapshot types (`LiquidityAtTick`, `BitmapAtWord`) are Pydantic frozen models with validated `u128`/`i128`/`u256` fields. This guarantee allows the binary writer to serialize values directly without runtime type checking — the Pydantic validation already ensured the correct widths and ranges at construction time.

Python-side serialization lives in a new module (e.g., `degenbot/uniswap/snapshot_binary.py`) with functions:

```python
def serialize_v3_snapshot(
    snapshot: UniswapV3LiquiditySnapshot,
) -> bytes: ...

def serialize_v4_snapshot(
    snapshot: UniswapV4LiquiditySnapshot,
) -> bytes: ...
```

These functions iterate the snapshot's internal `_liquidity_snapshot` dict (non-destructive read — the snapshot remains usable for any consumers that need a separate copy). They write tick_data directly to a `bytearray`/`struct.pack` without constructing intermediate Python objects.

### Rust-side snapshot storage

Add two fields to `PyUniswapArbEngine`:

```rust
/// V3 snapshot: pool address → tick data (consumed at registration)
v3_snapshot: parking_lot::Mutex<Option<HashMap<Address, HashMap<i32, TickInfo>>>>,
/// V4 snapshot: (pool_manager, pool_id) → tick data (consumed at registration)
v4_snapshot: parking_lot::Mutex<Option<HashMap<(Address, PoolId), HashMap<i32, TickInfo>>>>,
```

Snapshot data is a **staging area** — one-way transfer to pool state at registration. `register_v3_pool()` / `register_v4_pool()` calls `remove()` on the snapshot HashMap entry, not `clone()`, so there is never a second copy alongside live state.

After all registrations, `clear_v3_snapshot()` / `clear_v4_snapshot()` drops the (now mostly empty) outer HashMap.

### PoolTickCoverage enum

```rust
/// Describes the completeness of tick data for a registered pool.
enum PoolTickCoverage {
    /// Snapshot provided complete tick data for this pool.
    /// Tick data may be empty, meaning the pool is genuinely illiquid.
    /// Solver results are trustworthy.
    Tracked,
    /// No snapshot data exists for this pool. Tick state is incomplete —
    /// ticks that existed at snapshot time but were not captured will be
    /// missing. Solver results may contain errors or phantom profits.
    Sparse,
}
```

At registration time, coverage is determined by snapshot lookup:
- Pool found in snapshot → `Tracked` (regardless of tick count)
- Pool not in snapshot → `Sparse`

For V4, the two-level namespace `(pool_manager, pool_id)` is handled naturally:
- Pool manager in snapshot, pool_id present → `Tracked`
- Pool manager in snapshot, pool_id absent (new pool) → `Sparse`
- Pool manager not in snapshot → `Sparse`

No further split of `Sparse` is needed — in all cases the engine has incomplete tick data and faces the same solver risk.

The enum is stored per-pool in `V3PoolState` / `V4PoolState`. The solver can use it to:
- Solve `Tracked` pools with full confidence
- Solve `Sparse` pools with a known accuracy caveat (flag results, skip paths, etc. — policy decided separately)

### `apply_buffer` removal

`apply_buffer` is removed as a parameter from `register_v3_pool()` / `register_v4_pool()`. Rust always applies the `liquidity_event_buffer` after looking up snapshot data.

The previous `apply_buffer` had two modes:
- `true`: replay buffered Mint/Burn/ModifyLiquidity events on stale snapshot tick_data
- `false`: skip buffer because tick_data was fetched at current block (avoid double-counting)

In the new design, Rust always owns the snapshot (stale data from DB). There is no case where the engine receives current-RPC tick_data through the snapshot path. The buffer should always be applied to bring stale snapshot data forward. Explicitly-provided tick_data is no longer a registration parameter — if needed in the future, it would be a separate code path.

### Engine phase state machine

```rust
enum EnginePhase {
    Created,
    Subscribed,
    SnapshotLoaded,
    Backfilled,
    Resumed,
}
```

Transitions:

```
Created ──subscribe()──► Subscribed ──load_snapshot()──► SnapshotLoaded ──backfill()──► Backfilled ──resume()──► Resumed
                                                                       │
                                                                       └──resume()──► Resumed
                                                                        (skip backfill)
```

Enforcement (raise `RuntimeError` on violation):

| Method | Requires |
|--------|----------|
| `subscribe()` | `Created` |
| `load_v3_snapshot()` / `load_v4_snapshot()` | `Subscribed` or `SnapshotLoaded` (V3 and V4 can be loaded independently) |
| `backfill_from_snapshot()` | `SnapshotLoaded` |
| `register_v2_pool()` | any (V2 pools don't use snapshots) |
| `register_v3_pool()` / `register_v4_pool()` | `SnapshotLoaded` or later |
| `resume()` | `SnapshotLoaded` or `Backfilled` |

Error cases:
- Double snapshot load for same version (V3 or V4) → `RuntimeError`
- `backfill_from_snapshot()` without snapshot → `RuntimeError`
- Double `backfill_from_snapshot()` → `RuntimeError`
- `resume()` before `SnapshotLoaded` → `RuntimeError`
- `resume()` when already `Resumed` → `RuntimeError`
- Any mutation (load/backfill) after `Resumed` → `RuntimeError`

The `EnginePhase` field lives on `PyUniswapArbEngine` alongside `subscribe_state`:

```rust
phase: std::sync::atomic::AtomicU8, // EnginePhase repr(u8)
```

Using `AtomicU8` avoids needing a separate mutex — phase checks are lock-free reads, and phase transitions only occur at startup (single-threaded in practice). The actual state mutation inside methods still acquires the engine lock.

### New PyO3 methods

#### `load_v3_snapshot(data: bytes)`

Deserializes the binary buffer into `HashMap<Address, HashMap<i32, TickInfo>>`. Stores in `self.v3_snapshot`. Requires `Subscribed` or `SnapshotLoaded` phase. Raises `RuntimeError` if V3 snapshot already loaded. Transitions to `SnapshotLoaded` (if not already there from V4).

#### `load_v4_snapshot(data: bytes)`

Same pattern for V4, keyed by `(Address, PoolId)`. Independent of V3 — either can be loaded first.

#### `clear_v3_snapshot()` / `clear_v4_snapshot()`

Drop the stored snapshot, freeing memory. Callable in any phase (idempotent — no-op if already `None`).

### Modified PyO3 methods

#### `register_v3_pool(address, token0, token1, fee, tick_spacing, factory, sqrt_price_x96, liquidity, tick, block=0)`

The `tick_data` and `apply_buffer` parameters are **removed**. The method:

1. Looks up `self.v3_snapshot` by pool address
2. If found → `remove()` the tick_data entry, set `apply_buffer = true`, `coverage = Tracked`
3. If not found → empty tick_data, `apply_buffer = true` (always), `coverage = Sparse`
4. Applies buffered liquidity events from `liquidity_event_buffer` (always)
5. Creates `V3PoolState` with the resolved tick_data and coverage

#### `register_v4_pool(pool_manager, pool_id_hex, currency0, currency1, fee, tick_spacing, hook_flags, sqrt_price_x96, liquidity, tick, block=0)`

Same pattern: `tick_data` and `apply_buffer` removed. Lookup by `(pool_manager, pool_id)` in `v4_snapshot`.

### Python-side changes

#### Clean break: Rust owns the snapshot

`build_paths()` no longer passes the snapshot to the tracker. The Rust engine owns the snapshot exclusively. If the `UniswapV3PoolTracker` needs a snapshot for continued compatibility during the transition, it receives a **separate copy** constructed independently.

#### `EngineRegistry` wrapper methods

`register_v3_pool` / `register_v4_pool` lose the `override_tick_data` and `apply_buffer` parameters — Rust resolves everything internally.

```python
# Before (build_paths per-pool extraction):
snap_td = v3_snapshot.tick_data(pool.address)
engine_tick_data = {idx: (info.liquidity_gross, info.liquidity_net) for idx, info in snap_td.items()}
engine_registry.register_v3_pool(pool, override_tick_data=engine_tick_data)

# After:
engine_registry.register_v3_pool(pool)  # Rust looks up from stored snapshot
```

#### Startup sequence change

```python
# Before:
get_snapshots() → backfill_from_snapshot() → build_paths() [per-pool extraction in Python]

# After:
get_snapshots() → serialize + load_snapshot_to_engine() → backfill_from_snapshot() → build_paths() [no Python-side extraction]
```

The `build_paths()` function no longer iterates `v3_snapshot.tick_data()` — Rust resolves all tick_data lookups internally from its stored snapshot.

## Relationship to Other Plans

- **Plan 080** (V3 Block Engine Mint/Burn) — completed; this plan builds on its `liquidity_event_buffer` mechanism
- **Plan 087** (Snapshot Backfill) — completed; `backfill_from_snapshot()` is preserved, this plan only changes how tick_data reaches the engine at registration time

## Vertical Slices

### F1: Binary serialization format (Python)
- Implement `serialize_v3_snapshot()` in `snapshot_binary.py`
- Implement `serialize_v4_snapshot()` in `snapshot_binary.py`
- Unit tests: round-trip serialize → deserialize (with Rust F2), edge cases (empty tick data, large pools)

### F2: Rust snapshot storage + `load_v3_snapshot()` / `load_v4_snapshot()`
- Add `v3_snapshot`, `v4_snapshot` fields to `PyUniswapArbEngine`
- Add `EnginePhase` enum + `phase` field
- Add `load_v3_snapshot()` / `load_v4_snapshot()` PyO3 methods with binary deserialization
- Add `clear_v3_snapshot()` / `clear_v4_snapshot()` PyO3 methods
- Phase enforcement: raises on double-load, wrong phase
- Unit tests: load snapshot, verify stored, verify clear drops it, verify double-load raises

### F3: `PoolTickCoverage` enum + auto-lookup in `register_v3_pool()` / `register_v4_pool()`
- Add `PoolTickCoverage` enum to Rust
- Add `coverage` field to `V3PoolState` / `V4PoolState` (optimizer engine)
- Remove `tick_data` and `apply_buffer` parameters from `register_v3_pool()` / `register_v4_pool()`
- Auto-lookup from snapshot: `remove()` entry, set coverage, always apply buffer
- Unit test: register tracked pool → verify `Tracked` coverage + snapshot entry removed
- Unit test: register sparse pool → verify `Sparse` coverage + empty tick_data
- Unit test: register tracked pool with zero ticks → verify `Tracked` coverage + empty tick_data (genuinely illiquid)

### F4: Phase state machine for `backfill_from_snapshot()` and `resume()`
- Add phase transitions to existing `subscribe()`, `backfill_from_snapshot()`, `resume()` methods
- Enforce: backfill requires `SnapshotLoaded`, resume requires `SnapshotLoaded` or `Backfilled`
- Enforce: no mutation after `Resumed`
- Unit tests: each out-of-order call raises `RuntimeError`

### F5: Python integration
- Modify `build_paths()`: serialize snapshots → `load_v3_snapshot()` / `load_v4_snapshot()` → call `backfill_from_snapshot()` → simplified registration
- Remove `override_tick_data` / `apply_buffer` from `EngineRegistry` wrapper methods
- Remove per-pool `v3_snapshot.tick_data()` calls in `build_paths()`
- Add `clear_v3_snapshot()` / `clear_v4_snapshot()` call after `build_paths()` completes
- Pass separate snapshot copy to tracker (or None) for transition compatibility
- Integration test

### F6: Deprecate `override_tick_data` parameter
- After all callers are migrated, mark `override_tick_data` as deprecated in Python wrapper
- Remove explicit `tick_data` from `register_v3_pool` / `register_v4_pool` PyO3 signatures
- Update existing tests that pass explicit tick_data

## Status

- [x] F1: Binary serialization format (Python)
- [x] F2: Rust snapshot storage + `load_v3_snapshot()` / `load_v4_snapshot()`
- [x] F3: `PoolTickCoverage` enum + auto-lookup in registration
- [x] F4: Phase state machine enforcement
- [x] F5: Python integration
- [x] F6: Deprecate `override_tick_data` parameter
