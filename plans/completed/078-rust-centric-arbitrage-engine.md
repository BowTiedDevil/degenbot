# Plan 078: Rust-Centric Arbitrage Engine (V2 PoC)

## Overview

Proof-of-concept: move the per-block V2 arbitrage cycle entirely into Rust so that Sync event decoding, pool state updates, path resolution, and Mobius solver dispatch happen without crossing the PyO3 bridge. Python participates only in initial construction (registering pools, paths, and fee parameters) and reading results. If benchmarks confirm the thesis, the architecture extends to V3/Balancer in follow-up plans.

## Problem

### Deletion test

If you deleted `ArbPoolCacheAdapter`, `ArbSolver.update_all_paths()`, and the per-path `solve_registered_ints()` loop, the arbitrage system would have no way to solve paths. These exist solely to shuttle data across the Python↔Rust boundary every block. Every ns of Python-side work in that shuttle is pure overhead — the actual computation is 20ns of Rust.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| 97% of per-block cycle is data transfer, 2% is computation | V2ArbEngine prototype benchmark | Moving the loop into Rust doesn't help when Python must still feed it data each block |
| Python packs buffer in 85µs for 100 pool updates | `bench_v2_engine.py` | Buffer packing alone is 4× the Rust compute for the entire block |
| `pool.to_hop_state()` → `ConstantProductHop` → `SolveInput` → `solver.solve()` → `_try_rust_solve()` | `ArbitragePath.calculate_with_pool()` | 6-layer Python dispatch chain for a 20ns Rust computation |
| Per-path PyO3 call overhead ~800ns | `solve_registered_ints()` | Structural floor: you can't go faster than one PyO3 call per path from Python |
| `ArbPoolCacheAdapter` shuttles reserves to Rust on every pool state change | `pool_cache_adapter.py` | Observer pattern on Python side, but the real consumer is Rust — data should flow there directly |
| PyO3 arg extraction costs ~38ns per value | `batch_update()` tuple parsing | 5 values × 100 pools × 38ns = 19µs just to extract data Python already had |

## Solution

### Architecture: Rust drives, Python observes

```text
CURRENT (Python drives):
  Alloy → Python (subscription drain) → Pool.external_update()
    → PoolStateMessage → ArbPoolCacheAdapter → ArbSolver.update_pool()
    → ArbSolver.update_all_paths() → solve_registered_ints() → Python reads results

PROPOSED (Rust drives):
  Alloy WS → Rust pump (owns AlloyProvider, subscribes to block headers)
    → eth_getLogs(Sync, registered addresses)
    → decode Sync events → update IntHopState (both orientations)
    → rebuild paths → solve all → store results + block_number
    → Python reads via latest_results() (one PyO3 call per block)
```

The key insight from the V2ArbEngine prototype: Rust-internal `solve_all` costs 738 ns/path but the full Python→Rust→Python cycle costs 1,100+ ns/path. The only way to break through is to **eliminate Python from the hot path entirely**. Rust must own the data source (its own Alloy WS connection), the state management (pool updates from decoded Sync events), and the solver dispatch. Python reads results after the fact.

### Step 1: `V2BlockEngine` — the core Rust struct

A pure-Rust struct (no PyO3) that owns the full per-block lifecycle. Written from scratch — the prototype `Engine` in `v2_engine.rs` is deleted (see design decisions).

```rust
struct V2BlockEngine {
    pools: HashMap<u64, IntHopState>,              // pool_id → state (forward + reverse)
    pool_addresses: HashMap<Address, (u64, u64)>,  // pool address → (forward_id, reverse_id)
    paths: HashMap<u64, (V2Path, ResolvedPath)>,   // registered paths
    results: Vec<(u64, U256, U256)>,               // last solved results
    results_block: u64,                             // block number for results
    running: bool,                                  // true after start(), freezes registration
    next_path_id: u64,
    next_pool_id: u64,
}

impl V2BlockEngine {
    /// Register a pool by contract address. Creates entries in both reserve
    /// orientations (forward: reserve0→reserve1, reverse: reserve1→reserve0),
    /// matching ArbPoolCacheAdapter's existing behavior.
    /// Returns the forward pool_id. Reverse pool_id = forward_id + 1.
    /// Panics if called after start().
    fn register_pool(&mut self, address: Address, reserve0: U256, reserve1: U256, gamma_numer: u64, fee_denom: u64) -> u64;

    /// Update reserves for a registered pool from a Sync event.
    /// Sync(uint112,uint112) carries absolute reserves — last-event-wins
    /// per pool per block, no delta accumulation needed.
    fn apply_sync(&mut self, pool_address: Address, reserve0: U256, reserve1: U256);

    /// Decode Sync events from a batch of logs, apply to registered pools,
    /// rebuild all paths, solve all, and store results.
    fn process_block(&mut self, logs: Vec<Log>, block_number: u64);

    /// Register a path by ordered pool IDs. Returns path_id.
    /// Panics if called after start().
    fn register_path(&mut self, pool_ids: Vec<u64>) -> u64;

    /// Solve all registered paths. Returns profitable (path_id, input, profit).
    fn solve_all(&self, max_input: Option<f64>) -> Vec<(u64, U256, U256)>;

    /// Read the last solved results and block number.
    fn latest_results(&self) -> (&Vec<(u64, U256, U256)>, u64);

    /// Return the list of registered pool addresses for filter construction.
    /// Called once at start() time — the list is frozen thereafter.
    fn registered_addresses(&self) -> Vec<Address>;

    /// Mark the engine as running. Freezes registration.
    fn start(&mut self);
}
```

This struct is testable without Python. All state management is pure Rust.

### Step 2: Sync event decoder

Uniswap V2 `Sync` event: `Sync(uint112,uint112)` — absolute reserves, emitted by the pair contract whenever reserves change (swap, mint, burn).

The decoder runs entirely in Rust:

```rust
const V2_SYNC_TOPIC: B256 = b256!("1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1");

/// Decode a V2 Sync event from a log.
/// Returns (pool_address, reserve0, reserve1) or None if the log is not
/// a valid Sync event.
fn decode_sync_log(log: &Log) -> Option<(Address, U256, U256)> {
    // topic[0] must match V2_SYNC_TOPIC
    // data = abi.encode(uint112 reserve0, uint112 reserve1) = 64 bytes (left-padded to 32 each)
}
```

**Why Sync, not Swap**: The existing codebase uses Sync events for V2 reserve tracking (`V2_SYNC_TOPIC` in `log_decoders.py`, `decode_v2_sync()`). Sync carries absolute reserves — no delta accumulation, no orientation tracking from deltas, and self-correcting after missed events. Swap events carry deltas that would require maintaining running state across blocks; a single missed event corrupts all subsequent state permanently.

### Step 3: V2EnginePump — standalone Rust pump

The pump owns its own `AlloyProvider` and subscribes to block headers directly. No dependency on the existing `SubscriptionHandle` / `drain_buffer()` infrastructure — that exists to bridge Rust→Python, which the pump doesn't need.

```rust
struct V2EnginePump {
    engine: Arc<Mutex<V2BlockEngine>>,
    provider: Arc<AlloyProvider>,
    log_filter: Filter,  // built once at construction from registered addresses + V2_SYNC_TOPIC
    shutdown: AtomicBool,
}

impl V2EnginePump {
    /// Spawn the pump on the Tokio runtime.
    /// The pump:
    /// 1. Creates its own AlloyProvider from the RPC URL
    /// 2. Subscribes to block headers via WS
    /// 3. On each new block: fetches Sync logs via eth_getLogs with the pre-built filter
    /// 4. Calls engine.process_block(logs, block_number)
    /// 5. Loops until shutdown is set
    async fn run(rpc_url: String, engine: Arc<Mutex<V2BlockEngine>>) -> ProviderResult<()> {
        let provider = AlloyProvider::new(&rpc_url, 3).await?;

        let stream = provider.inner.subscribe_blocks().await?
            .into_stream();

        let log_filter = {
            let engine = engine.lock();
            build_sync_filter(&engine.registered_addresses())
        };

        while let Some(header) = stream.next().await {
            if engine.shutdown.load(Ordering::Relaxed) { break; }

            let logs = provider.get_logs(&log_filter).await?;
            engine.lock().process_block(logs, header.number);
        }

        Ok(())
    }
}
```

**Why the pump owns its own AlloyProvider** (vs reusing a Python-provided one): Zero coupling to the PyO3 provider layer. The pump takes an RPC URL string — no `Arc` extraction through PyO3, no shared mutable state across the bridge. `AlloyProvider::new()` already handles HTTP/WS/IPC auto-detection, retries, and caching. A single WS connection serves both the block subscription and `eth_getLogs` calls. If Python also needs a block subscription for other work, a second WS connection is acceptable for the PoC; a shared block-header channel (mpsc) can be added later if it matters.

**Why a pre-built filter** (vs rebuilding each block): Pool registration is frozen after `start()` — the address list is stable. The filter is constructed once at pump startup and reused for every block. This avoids re-reading the address map and re-constructing the filter on every block cycle.

### Step 4: Python observer seam

Python constructs the engine, then starts the Rust-side pump with just an RPC URL:

```python
engine = V2ArbEngine()

# Construction (Python-driven, one-time, frozen after start)
fwd_id = engine.register_pool(address="0x...", reserve0=..., reserve1=..., gamma_numer=997, fee_denom=1000)
path_id = engine.register_path([pool_0_fwd, pool_1_fwd])

# Start — just the URL, pump creates its own connection
engine.start("wss://mainnet.infura.io/ws/v3/...")

# Per-block (Rust-driven, Python reads results)
results, block_number = engine.latest_results()
```

Python API surface:

| Method | When | PyO3 crossing? |
|--------|------|-----------------|
| `register_pool(address, reserve0, reserve1, gamma_numer, fee_denom)` | Construction | Yes (one-time) |
| `register_path(pool_ids)` | Construction | Yes (one-time) |
| `start(rpc_url)` | Once | Yes (spawns Tokio task, returns immediately) |
| `stop()` | Teardown | Yes |
| `latest_results()` | Per block | Yes (read only — copies stored `Vec` + `u64`) |

`latest_results()` is the only per-block Python call. It returns pre-computed results and the block number — no Rust computation, just reading stored data. The `block_number` return value lets Python detect stale results if the subscription lags.

For synchronous testing without a live connection, `process_logs(logs, block_number)` is also exposed — useful for correctness tests with known inputs.

### Step 5: Per-block Sync processing semantics

Sync events carry absolute reserves (`Sync(uint112, uint112)`), which simplifies per-block processing dramatically compared to Swap-based delta accumulation:

1. **Decode all Sync events** from the block's logs
2. **For each registered pool, last Sync wins**: if a pool has multiple Sync events in one block (e.g., mint then swap), the final Sync carries the correct absolute reserves
3. **Pools with no Sync event in this block**: reserves unchanged from previous state — no action needed
4. **Update both orientations**: `apply_sync()` updates both the forward and reverse `IntHopState` entries for the pool address
5. **Rebuild all paths + solve**: one pass, no per-block accumulation state needed

No `begin_block`/`end_block` delimiters, no delta accumulation, no orientation tracking from deltas.

### Design decisions

- **Delete `v2_engine.rs`, write `v2_block_engine.rs` from scratch**: The prototype `Engine` is self-declared throwaway code (`# PROTOTYPE — throwaway code`), has zero production callers (only `bench_v2_engine.py`), and its data model is wrong for the new architecture (Python-fed `IntHopState` updates vs Rust-fed Sync events). Copy `V2Path` and `ResolvedPath` types; drop all Python-driven update methods, the binary buffer parser, and the `u256_to_py_fast` duplicate.
- **PoC scope — V2-only, Mobius solver only**: This plan validates the Rust-centric architecture for the simplest case. V3 (tick ranges) and Balancer (weight vectors, rate providers) have fundamentally different state models and are separate plans. Aerodrome/Camelot V2-family pools use the same Sync event and would fit naturally into this engine, but are deferred to keep the PoC tight.
- **Sync events, not Swap events**: Sync carries absolute reserves (self-correcting on missed events, no delta accumulation). Swap carries deltas (one missed event corrupts all subsequent state). The existing codebase already uses Sync (`V2_SYNC_TOPIC`, `decode_v2_sync()`) — this plan follows the same choice.
- **Pump owns its own AlloyProvider**: The pump creates its own connection from an RPC URL string. No dependency on the Python `PyAlloyProvider` wrapper, no `Arc` extraction through PyO3, no shared mutable state across the bridge. A second WS connection (if Python also subscribes to blocks) is acceptable for the PoC.
- **No SubscriptionHandle dependency**: The existing `SubscriptionHandle` / double-buffer / `drain_buffer()` infrastructure exists to bridge Rust→Python. The pump runs entirely in Rust and doesn't need it. The pump subscribes to block headers directly via Alloy's `subscribe_blocks()` stream.
- **Registration frozen after `start()`**: `register_pool()` and `register_path()` may only be called before `start()`. After `start()`, they raise an error. This allows the log filter to be built once and cached for all subsequent blocks — no dynamic registration, no filter rebuilding. Dynamic registration can be added later if the basic performance thesis holds.
- **Block-header subscription + eth_getLogs, not log subscription**: Simpler filter management (built once, frozen), one RPC call per block, no dynamic subscription management. If address-list filtering hits RPC provider limits, chunk the `eth_getLogs` call into batches of ~50 addresses.
- **Dual-orientation registration**: Mirrors `ArbPoolCacheAdapter`'s forward+reverse pattern. Each `register_pool()` call creates two `IntHopState` entries (forward and reverse). `apply_sync()` updates both from the same Sync event. Paths reference forward-orientation pool IDs; the reverse orientations are used by paths that traverse the pool in the opposite direction.
- **Results are stored, not pushed**: `latest_results()` is a pull model. Push model (Python callback) would cross the PyO3 bridge on every block. Python polls when it needs results, using `block_number` to detect staleness.
- **`HashMap` for pools and paths (PoC)**: Registration is controlled by Python at construction time and frozen after `start()` — no dynamic registration in the hot path. Typical workloads have <1000 paths. For the production engine (V3+), revisit `LruCache` vs `HashMap` given the `CONTEXT.md` ruling on memory bounds.
- **`py.detach()` only for subscription I/O**: The `pump()` Tokio task has no access to the GIL — it runs on the Tokio runtime as a pure async task. All engine operations inside `process_block()` (log decoding, reserve updates, path resolution, solving) are sub-µs pure-compute operations that execute under the `Mutex` lock, with GIL state depending on who holds the lock (see GIL discipline below).

### GIL discipline for the pump

The pump's Tokio task never touches the GIL. It acquires `engine.lock()` (a `parking_lot::Mutex`, not the GIL) to call `process_block()`. When Python calls `latest_results()`, it also acquires `engine.lock()` under the GIL. These two callers naturally serialize through the `Mutex` — no GIL release/reacquire dance is needed because the pump never holds the GIL and Python never releases it during `latest_results()` (it's a sub-µs clone + read).

## Files Involved

**Primary:**
- `rust/src/optimizers/v2_engine.rs` — **DELETE** — throwaway prototype, replaced by `v2_block_engine.rs`
- `rust/src/optimizers/v2_block_engine.rs` — **NEW** — `V2BlockEngine` pure-Rust struct with address mapping, Sync decoder, dual-orientation registration, `process_block()`, `latest_results()`, frozen registration
- `rust/src/optimizers/v2_sync_decoder.rs` — **NEW** — `V2_SYNC_TOPIC` constant and `decode_sync_log()` pure function
- `rust/src/optimizers/v2_engine_pump.rs` — **NEW** — `V2EnginePump` that owns its own `AlloyProvider`, subscribes to block headers, fetches Sync logs, drives `V2BlockEngine::process_block()`

**Secondary:**
- `rust/src/optimizers/mod.rs` — Replace `v2_engine` with `v2_block_engine`, add `v2_sync_decoder`, `v2_engine_pump`
- `rust/src/lib.rs` — Register new `PyV2ArbEngine` class (replaces old one)
- `rust/src/provider.rs` — Reuse `AlloyProvider::new()` and `AlloyProvider::get_logs()` unchanged
- `rust/src/optimizers/mobius_int.rs` — Reuse `IntHopState` and `mobius_solve_with_refinement` unchanged
- `rust/src/optimizers/mobius.rs` — Reuse `HopState` unchanged

**Deleted:**
- `rust/src/optimizers/v2_engine.rs` — Prototype, zero production callers
- `tests/perf/bench_v2_engine.py` — Benchmarks the deleted prototype; replaced by new e2e benchmarks

**No change needed:**
- `rust/src/optimizers/mobius_py.rs` — Existing `PyPoolCache` kept for backward compat
- `rust/src/subscription.rs` — Not used by the new pump (it has its own Alloy connection)
- `src/degenbot/arbitrage/optimizers/solver.py` — Existing `ArbSolver` kept for backward compat
- `src/degenbot/uniswap/log_decoders.py` — Existing Python Sync decoder kept for Python-driven pools

## Implementation Order

### Slice 1: Pure-Rust `V2BlockEngine` with Sync support

1. Create `rust/src/optimizers/v2_block_engine.rs`
2. Implement `V2BlockEngine` struct with `pools`, `pool_addresses`, `paths`, `results`, `results_block`, `running` fields
3. Implement `register_pool()` — creates forward+reverse `IntHopState` entries, stores address→(forward_id, reverse_id) mapping
4. Implement `apply_sync()` — looks up address, updates both orientation entries
5. Implement `process_block()` — decodes Sync events from logs, applies updates (last Sync wins per pool), rebuilds paths, solves, stores results + block number
6. Implement `latest_results()`, `registered_addresses()`, `start()`, `register_path()`, `solve_all()`
7. Copy `V2Path` and `ResolvedPath` types + `resolve_path()` logic from old `v2_engine.rs`
8. Write Rust unit tests (no Python needed)
9. Run: `just test-rust` — expect all pass

### Slice 2: Sync event decoder module

1. Create `rust/src/optimizers/v2_sync_decoder.rs`
2. Define `V2_SYNC_TOPIC` constant: `0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1`
3. Implement `decode_sync_log(log: &Log) -> Option<(Address, U256, U256)>` pure function
4. Handle: wrong topic, malformed data (not 64 bytes), truncated, zero reserves (valid for newly created pairs)
5. Write exhaustive Rust tests: valid Sync, wrong topic, truncated data, zero reserves, extra data
6. Run: `just test-rust` — expect all pass

### Slice 3: Python observer seam (PyV2ArbEngine)

1. Implement `PyV2ArbEngine` pyclass wrapping `parking_lot::Mutex<V2BlockEngine>`
2. Expose `register_pool(address, reserve0, reserve1, gamma_numer, fee_denom)` — calls `V2BlockEngine::register_pool()`, panics if engine is running (raises `PanicException` in Python)
3. Expose `register_path(pool_ids)` — calls `V2BlockEngine::register_path()`, panics if engine is running
4. Expose `start()` — calls `V2BlockEngine::start()`, freezes registration
5. Expose `latest_results() -> (list, int)` — flat list `[path_id, input, profit, ...]` + block number
6. Expose `process_logs(sync_updates, block_number)` — accepts list of `(address_str, reserve0, reserve1)` tuples; calls `V2BlockEngine::process_sync_updates()`
7. Update `mod.rs` and `lib.rs` to register new module and class
8. Delete `rust/src/optimizers/v2_engine.rs` (old prototype)
9. Write Python tests in `test_v2_block_engine.py`
10. Run: `just test-rust` + `just test-python` — expect all pass

**Implementation notes**: `process_logs()` accepts pre-decoded `(address, reserve0, reserve1)` tuples rather than raw log dicts — this simplifies the Python test interface and avoids needing a `py_logs_to_rust` converter. The Rust `V2BlockEngine::process_sync_updates()` method is a convenience that applies Sync updates directly and solves, bypassing log decoding. The `start()` Python method currently just calls `V2BlockEngine::start()` (freezes registration); pump spawning is wired in Slice 4.

### Slice 4: V2EnginePump

1. Create `rust/src/optimizers/v2_engine_pump.rs` — **DONE** (file exists, compiles)
2. Implement `V2EnginePump` struct with `engine: Arc<Mutex<V2BlockEngine>>`, `provider: Arc<AlloyProvider>`, `log_filter: Filter`, `shutdown: Arc<AtomicBool>` fields
3. Implement `V2EnginePump::spawn(rpc_url, engine, shutdown)` — creates `AlloyProvider` in async context, builds filter from `engine.registered_addresses()` + `V2_SYNC_TOPIC`, spawns `run()` on `get_runtime()`
4. Implement `run()` async fn:
   - Subscribes to block headers via `provider.provider_arc().subscribe_blocks()`
   - On each block: builds block-specific filter (from/to = block_number), fetches logs via `provider.provider_arc().get_logs()`, calls `engine.lock().process_block(logs, block_number)`
   - Loops until `shutdown` flag is set
5. Wire `PyV2ArbEngine::start(rpc_url)` to store `Arc<Mutex<V2BlockEngine>>` + `Arc<AtomicBool>`, call `V2BlockEngine::start()`, then `V2EnginePump::spawn()`
6. Wire `PyV2ArbEngine::stop()` to set the shutdown flag
7. Write Rust unit tests for `build_sync_filter()` (topic + addresses)
8. Run: `just test-rust` + `just test-python` — expect all pass

**Implementation notes**: The pump uses `provider.provider_arc().get_logs(&alloy_filter)` directly with an Alloy `Filter` (not `LogFilter`) because the pump builds the filter from Rust types (no string parsing needed). The `AlloyProvider::get_logs(&LogFilter)` method requires string-based addresses/topics; the raw `provider_arc().get_logs()` takes an Alloy `Filter` directly, which is more natural for the Rust-only pump.

### Slice 5: End-to-end validation and benchmarks

1. Write `tests/perf/bench_v2_block_engine_e2e.py` that measures `latest_results()` call cost
2. Compare: `latest_results()` (Rust-driven, read-only) vs existing `solve_registered_ints()` (baseline)
3. Verify correctness against anvil mainnet fork: register real V2 pools, let pump run for a block, compare engine results with `ArbSolver` output
4. Record PoC verdict: does the Rust-centric architecture deliver sufficient speedup to justify extending to V3/Balancer?
5. Run: `just lint` + `just test-all` — expect all pass

### Slice 6: Update CONTEXT.md and clean up

1. Update `rust/CONTEXT.md` with new terms: `V2BlockEngine`, `V2EnginePump`, `apply_sync`, `process_block`, `latest_results`, `V2_SYNC_TOPIC` (Rust-side)
2. Update `CONTEXT-MAP.md` if terms cross module boundaries
3. Remove any leftover prototype references in `mod.rs` or `lib.rs`
4. Run: `just lint` + `just test-all` — expect all pass

## Testing

### Per-slice test runs

Each slice runs `just test-rust` and `just test-python`. The `V2BlockEngine` core is pure Rust, so most testing happens in `just test-rust`.

### New unit tests

```rust
// In v2_block_engine.rs #[cfg(test)]

#[test]
fn register_pool_creates_both_orientations() { /* forward_id + 1 == reverse_id */ }

#[test]
fn register_pool_stores_address_mapping() { /* address → (forward_id, reverse_id) */ }

#[test]
fn apply_sync_updates_both_orientations() { /* reserve0/1 → forward, reverse */ }

#[test]
fn process_block_decodes_sync_events() { /* valid Sync logs update pools */ }

#[test]
fn process_block_ignores_unregistered_pools() { /* unknown address → no update */ }

#[test]
fn process_block_ignores_wrong_topic() { /* non-Sync event → no update */ }

#[test]
fn process_block_last_sync_wins() { /* two Syncs for same pool → final reserves */ }

#[test]
fn latest_results_returns_last_solved() { /* empty before solve, populated after */ }

#[test]
fn register_pool_after_start_panics() { /* frozen registration */ }
```

```rust
// In v2_sync_decoder.rs #[cfg(test)]

#[test]
fn decode_valid_sync() { /* reserve0=1.5M USDC, reserve1=800 WETH */ }

#[test]
fn decode_sync_zero_reserves() { /* valid for newly created pairs */ }

#[test]
fn decode_sync_wrong_topic_returns_none() { /* ... */ }

#[test]
fn decode_sync_truncated_data_returns_none() { /* < 64 bytes */ }

#[test]
fn decode_sync_extra_data_decodes_first_two() { /* > 64 bytes — decode reserve0,1 only */ }
```

```python
# tests/arbitrage/test_optimizers/test_v2_block_engine.py

def test_register_pool_by_address():
    """Register a pool by address and verify internal ID assignment."""

def test_latest_results_empty():
    """latest_results() returns empty before any solve."""

def test_latest_results_after_sync():
    """After process_logs with Sync events, latest_results returns profitable paths."""

def test_block_number_tracking():
    """Engine tracks which block the results correspond to."""

def test_values_match_arb_solver():
    """V2BlockEngine results match ArbSolver for identical inputs and reserves."""

def test_dual_orientation_registration():
    """Registering a pool creates forward and reverse entries; both update on Sync."""

def test_process_logs_ignores_unregistered():
    """Sync events for non-registered pool addresses are skipped."""

def test_register_pool_after_start_raises():
    """Calling register_pool after start() panics (PanicException in Python)."""

def test_register_path_after_start_raises():
    """Calling register_path after start() panics (PanicException in Python)."""
```

### Integration tests

- Existing `tests/arbitrage/test_optimizers/test_rust_batch_solve.py` covers `PyPoolCache` / `ArbSolver` (unchanged)
- New `test_v2_block_engine.py` covers the Rust-centric path
- Anvil fork test: register real UniswapV2 pools (USDC/WETH, WETH/USDT), start pump with a fork RPC URL, verify results after a block matches Python `ArbSolver`

## Benefits

- **Depth**: Current `ArbPoolCacheAdapter` is a shallow seam that shuttles data across the PyO3 bridge every block. `V2BlockEngine` is a deep seam — Rust owns the full state lifecycle, Python only reads results.
- **Locality**: Pool state, path resolution, and solver dispatch are co-located in one Rust struct. Currently they're split across `ArbPoolCacheAdapter`, `ArbSolver`, `RustPoolCache`, and Python loop code.
- **Performance elimination, not optimization**: We don't make the PyO3 bridge faster — we stop crossing it. The hot path goes from "Python calls Rust 50 times per block" to "Python calls Rust once per block to read results."
- **Testability**: `V2BlockEngine` is pure Rust with no PyO3 dependency — testable with `cargo test` and no Python interpreter. `process_logs()` provides a synchronous testing entry point for Python correctness tests without requiring a live WS connection.
- **Leverage**: The existing `IntHopState` / `mobius_solve_with_refinement` solver is reused unchanged. `AlloyProvider::new()` and `AlloyProvider::get_logs()` are reused unchanged. The Sync event topic matches the existing Python `V2_SYNC_TOPIC` constant.

## Risks

- **Stale results if subscription lags**: If the Alloy WS connection falls behind, `latest_results()` may return results from an old block. **Mitigation**: `block_number` return value on `latest_results()` lets Python detect staleness. If Python sees a stale block number, it can wait and retry.
- **eth_getLogs address-list limits**: Some RPC providers limit the number of addresses in a single `eth_getLogs` call. **Mitigation**: Chunk the address list into batches of ~50 addresses if the provider returns errors. Typical PoC workloads have <100 pools, well within limits.
- **Reorgs**: A block reorg means the fetched logs and solved results correspond to an orphaned block. **Mitigation**: `block_number` tracking lets Python detect reorgs by comparing against the chain head. The engine re-solves on the next block automatically (Sync is self-correcting).
- **V3/Balancer pools not supported**: This plan is V2-only PoC. V3 tick-range state and Balancer weight vectors require fundamentally different state models. **Mitigation**: The engine is pool-type-specific by design. V3/Balancer support is a separate plan contingent on PoC results.
- **Aerodrome/Camelot V2-family deferred**: These use the same Sync event but have different fee/stable-swap semantics. **Mitigation**: Deferred from PoC scope; extension is straightforward since Sync event format is identical.
- **Second WS connection**: The pump creates its own WS connection independent of any Python-side provider. **Mitigation**: Acceptable for PoC. If it matters later, a shared block-header channel (mpsc) can feed both consumers.

## Relationship to Other Plans

- Independent. This plan supersedes the `V2ArbEngine` prototype (deleting `v2_engine.rs`) and uses the existing `AlloyProvider` infrastructure, but does not depend on any other active plan. The PoC verdict (pass/fail) determines whether follow-up plans for V3/Balancer Rust-centric engines are pursued.

## Status

- [x] Slice 1: Pure-Rust `V2BlockEngine` with Sync support
- [x] Slice 2: Sync event decoder module
- [x] Slice 3: Python observer seam (PyV2ArbEngine)
- [x] Slice 4: V2EnginePump
- [x] Slice 5: End-to-end validation and benchmarks
- [x] Slice 6: Update CONTEXT.md and clean up
