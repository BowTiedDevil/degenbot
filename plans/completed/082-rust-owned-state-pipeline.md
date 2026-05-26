# Plan 082: Rust-Owned State Pipeline

## Overview

Activate the Rust engine pumps so Rust owns the entire hot-loop state pipeline for V2, V3, and V4 pools. Today, Python receives WS events, applies handlers to Python pool objects, extracts full tick_data, and pushes it to Rust via `process_logs()` — making Python the state authority and Rust a passive consumer. The Rust engine already has event decoders (`v2_sync_decoder`, `v3_swap_decoder`, `v4_swap_decoder`) and pump skeletons (`V2EnginePump`, `V3EnginePump`) — but the bot doesn't use them. This plan wires them in, adds the missing V4 pump and V3 Mint/Burn decoder, introduces a backfill mechanism for snapshot-to-live-gap, and eliminates Python from the per-event state path.

## Problem

### Deletion test

If you deleted `process_single_event()`, the Python-side `LOG_HANDLERS` dispatch, the full `tick_data` extraction on every event, and the `process_logs()` Python→Rust push — the bot would stop detecting opportunities entirely. Rust has no way to receive events on its own because the pumps are dead code. This is the deletion test failing: the architecture was designed for Rust to own the pipeline, but Python is still the middleman.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| Python is the state middleman for every event | `process_single_event()` → `pool.LOG_HANDLERS[topic0]` → `engine_registry.process_block()` | Every V3/V4 swap crosses PyO3 twice (Python applies handler, Python extracts full tick_data, Python pushes to Rust). Latency: ~200µs/event for pools with large tick maps. Rust decodes the same events in ~50ns. |
| Full tick_data dump per event (O(n) per swap) | `process_single_event()` constructs `tick_priors` from `pool.tick_data.items()` | USDC/WETH has ~2000 initialized ticks → 2000 PyO3 tuple crossings per event. Plan 080 F4 called this "acknowledged, deferred" but the pumps were the intended fix. |
| No V3 Mint/Burn decoder in Rust | `v3_block_engine.rs` `process_block()` only decodes Swap | Swap events update (sqrtPrice, liquidity, tick) but Mint/Burn update tick_data (liquidity_gross, liquidity_net at tick_lower/tick_upper). Without Mint/Burn, the Rust engine's tick_data drifts from reality after any LP position change. |
| No V4 pump | `v4_block_engine.rs` has `apply_swap` but no `process_block(&[Log])` and no `V4EnginePump` | V4 events are routed through Python's `process_single_event()` — same middleman problem as V3, plus V4 also needs ModifyLiquidity handling. |
| V3 snapshot not backfilled to first event block | `UniswapV3LiquiditySnapshot.fetch_new_events()` exists but is never called by the bot | Snapshot at block N. Pool built at block N+1000 with stale tick_data. If Mints/Burns occurred between N and N+1000, the pool's tick_data is wrong — and since Python feeds Rust, Rust is wrong too. |
| No V4 snapshot at all | V4 pools built via `bot.build_managed_pool()` with per-call RPC | No batch tick-data loading for ~102K V4 pools. Each pool fetches its own state — ~102K RPC calls at startup, or lazy per-path construction with potentially stale state. |
| `PyUniswapArbEngine.pump_handle` and `shutdown` are dead code | `uniswap_engine.rs` line 1378-1379 | The struct carries `pump_handle: Mutex<Option<JoinHandle>>` and `shutdown: AtomicBool` — never used. `start()` only freezes registration. |

## Solution

### Architecture: Rust pumps own the hot loop

The intended architecture from Plans 079/080/081:

```
WS subscription → Alloy block headers
    → eth_getLogs(registered addresses, topics)
    → decode events in Rust
    → update engine state in Rust
    → solve paths in Rust
    → store results for Python to read
```

Python's role in the hot loop: **read results and submit transactions**. That's it.

### Current (broken) architecture

```
Python WS → on_event() → process_single_event()
    → Python LOG_HANDLERS → Python pool.update()
    → extract tick_data.items() (O(n))
    → engine_registry.process_block() → PyO3 → Rust engine
    → Rust re-inserts every tick
    → schedule_dispatch() → try_dispatch()
    → Python reads results() → simulates → submits
```

### Target architecture

```
Python: build pools, register with engine, start pump
Rust pump: WS → newHeads → eth_getLogs → decode → process_block → solve
Python: read latest_results() → encode → simulate → submit
```

### Slice 1: V3 Mint/Burn event decoders in Rust

The Rust `V3BlockEngine.process_block()` currently only decodes Swap events. It also needs Mint and Burn, which modify `tick_data` (liquidity_gross, liquidity_net) at tick_lower and tick_upper.

Create `rust/src/bot_core/v3_mint_burn_decoder.rs`:

- `V3_MINT_TOPIC`: keccak256 of `Mint(address,address,int24,int24,uint128,uint256,uint256)`
- `V3_BURN_TOPIC`: keccak256 of `Burn(address,int24,int24,uint128,uint256,uint256)`
- `decode_v3_mint_log()`: returns `V3MintEvent { pool_address, tick_lower, tick_upper, amount, sender }`
- `decode_v3_burn_log()`: returns `V3BurnEvent { pool_address, tick_lower, tick_upper, amount }`

Mint/Burn only affect tick_data. They do NOT change (sqrtPrice, liquidity, tick) — those are only in Swap events. The `apply_mint_burn` method on `V3PoolState` updates `tick_data[tick_lower].liquidity_net += amount`, `tick_data[tick_upper].liquidity_net -= amount`, `tick_data[tick_lower].liquidity_gross += amount`, `tick_data[tick_upper].liquidity_gross -= amount` (with sign flip for Burn).

```rust
// In v3_block_engine.rs
pub struct V3LiquidityUpdate {
    pub pool_address: Address,
    pub tick_lower: i32,
    pub tick_upper: i32,
    pub liquidity_delta: i128,  // positive for Mint, negative for Burn
}

impl V3PoolState {
    pub fn apply_liquidity_update(&mut self, update: &V3LiquidityUpdate) {
        let ticks = [(update.tick_lower, update.liquidity_delta),
                     (update.tick_upper, -update.liquidity_delta)];
        for (tick, net_delta) in ticks {
            let entry = self.tick_data.entry(tick).or_insert(TickInfo::default());
            entry.liquidity_net = I256::try_from(
                entry.liquidity_net.to_i128().saturating_add(net_delta)
            ).unwrap_or(I256::ZERO);
            entry.liquidity_gross = U128::from(
                entry.liquidity_gross.to_u128().saturating_add_signed(net_delta)
            );
        }
    }
}
```

### Slice 2: V3 `process_block` handles Mint/Burn

Extend `V3BlockEngine::process_block()` to also decode Mint and Burn logs, applying `apply_liquidity_update` for each.

The pump currently subscribes to `V3_SWAP_TOPIC` only. Change it to subscribe to all three topics: `[V3_SWAP_TOPIC, V3_MINT_TOPIC, V3_BURN_TOPIC]`. The Alloy `Filter` supports multiple topic0 values.

```rust
// In v3_engine_pump.rs
fn build_v3_log_filter(addresses: &[Address]) -> Filter {
    let mut filter = Filter::new();
    filter = filter
        .event_signature(V3_SWAP_TOPIC)
        .event_signature(V3_MINT_TOPIC)   // NEW
        .event_signature(V3_BURN_TOPIC);   // NEW
    for addr in addresses {
        filter = filter.address(*addr);
    }
    filter
}
```

And update `process_block`:

```rust
pub fn process_block(&mut self, logs: &[alloy::rpc::types::Log], block_number: u64) {
    for log in logs {
        if let Some(event) = decode_v3_swap_log(log) {
            self.apply_swap(event.pool_address, event.sqrt_price_x96,
                extract_u128(event.liquidity), event.tick, block_number, &[]);
        } else if let Some(event) = decode_v3_mint_log(log) {
            self.apply_liquidity_update(&V3LiquidityUpdate {
                pool_address: event.pool_address,
                tick_lower: event.tick_lower,
                tick_upper: event.tick_upper,
                liquidity_delta: event.amount as i128,
            });
        } else if let Some(event) = decode_v3_burn_log(log) {
            self.apply_liquidity_update(&V3LiquidityUpdate {
                pool_address: event.pool_address,
                tick_lower: event.tick_lower,
                tick_upper: event.tick_upper,
                liquidity_delta: -(event.amount as i128),
            });
        }
    }
    self.rebuild_and_solve(block_number);
}
```

### Slice 3: V4 ModifyLiquidity event decoder + `process_block` in Rust

The Rust `V4BlockEngine` has `apply_swap` and `apply_swap_updates` but no `process_block` that takes raw Alloy logs, and no ModifyLiquidity decoder.

Create `rust/src/bot_core/v4_modify_liquidity_decoder.rs`:

- `V4_MODIFY_LIQUIDITY_TOPIC`: keccak256 of `ModifyLiquidity(bytes32,address,int24,int24,int256,bytes32)`
- `decode_v4_modify_liquidity_log()`: returns `V4ModifyLiquidityEvent { pool_id, tick_lower, tick_upper, liquidity_delta }`

Add `process_block(&mut self, logs: &[Log], block_number: u64)` to `V4BlockEngine`:

```rust
pub fn process_block(&mut self, logs: &[alloy::rpc::types::Log], block_number: u64) {
    for log in logs {
        // Only process logs from the monitored PoolManager
        if log.address() != self.pool_manager_address { continue; }
        if let Some(event) = decode_v4_swap_log(log) {
            self.apply_swap(/* from decoded event */);
        } else if let Some(event) = decode_v4_modify_liquidity_log(log) {
            self.apply_liquidity_update(/* from decoded event */);
        }
    }
    self.rebuild_and_solve(block_number);
}
```

V4 `ModifyLiquidity` maps directly to V3 Mint/Burn for tick_data mutation — same `liquidity_gross`/`liquidity_net` update at `tick_lower`/`tick_upper`. V4 `Swap` maps to V3 `Swap` for scalar state (sqrtPrice, liquidity, tick).

### Slice 4: V4EnginePump

Create `rust/src/optimizers/v4_engine_pump.rs` — mirrors `V2EnginePump` and `V3EnginePump`:

- Subscribes to block headers via Alloy WS
- On each new block: fetches logs via `eth_getLogs` for the PoolManager address, filtered to V4_SWAP_TOPIC + V4_MODIFY_LIQUIDITY_TOPIC
- Calls `V4BlockEngine::process_block(logs, block_number)`
- Runs on the shared Tokio runtime, no Python dependency

Register in `rust/src/optimizers/mod.rs`.

### Slice 5: Unified pump — `UniswapEnginePump`

Rather than three independent pumps that each make separate `eth_getLogs` calls, create a single `UniswapEnginePump` that fetches ALL relevant logs (Sync + V3 Swap/Mint/Burn + V4 Swap/ModifyLiquidity) in one `eth_getLogs` call per block and routes them to the appropriate sub-engine.

```rust
pub struct UniswapEnginePump {
    engine: Arc<Mutex<UniswapEngine>>,
    provider: Arc<AlloyProvider>,
    log_filter: Filter,
    shutdown: Arc<AtomicBool>,
}

impl UniswapEnginePump {
    pub fn spawn(rpc_url: String, engine: Arc<Mutex<UniswapEngine>>, ...) -> Result<JoinHandle<()>, String> {
        // Single WS subscription to block headers
        // Single eth_getLogs call per block with all topics
        // Route decoded events to V2/V3/V4 engines
    }

    async fn run(self) {
        // Subscribe to block headers
        loop {
            let header = stream.next().await;
            let logs = self.provider.get_logs(&block_filter).await;
            self.engine.lock().process_block(&logs, block_number);
        }
    }
}
```

`UniswapEngine::process_block(&mut self, logs: &[Log], block_number: u64)` replaces the current `process_logs()` Python-facing method. It decodes and routes internally:

```rust
impl UniswapEngine {
    pub fn process_block(&mut self, logs: &[alloy::rpc::types::Log], block_number: u64) {
        for log in logs {
            let topic0 = log.topics().first();
            match topic0 {
                Some(&V2_SYNC_TOPIC) => { /* route to v2_engine */ }
                Some(&V3_SWAP_TOPIC) | Some(&V3_MINT_TOPIC) | Some(&V3_BURN_TOPIC) => { /* route to v3_engine */ }
                Some(&V4_SWAP_TOPIC) | Some(&V4_MODIFY_LIQUIDITY_TOPIC) => { /* route to v4_engine */ }
                _ => {}
            }
        }
        // Re-solve affected paths across all engines
        self.rebuild_and_solve(block_number);
    }
}
```

The Python-facing `process_logs()` (which takes pre-decoded Python lists) is kept for backward compatibility and testing, but is no longer called in the hot loop.

### Slice 6: Snapshot backfill for V3/V4

The `UniswapV3LiquiditySnapshot` already has `fetch_new_events()` and `fetch_new_events_async()` — they fetch Mint/Burn logs between the snapshot's block and a target block, applying them as `pending_updates`. The bot never calls these methods.

Wire backfill into the startup sequence:

1. Load V3 snapshot from DB at block N
2. Build V3 pools with snapshot tick_data
3. After registration and `freeze()`, call `snapshot.fetch_new_events_async(current_block)` to fetch Mint/Burn events from block N+1 to current
4. Apply `pending_updates` to each Python pool
5. Push the updated tick_data to the Rust engine via `process_logs()` **one final time**
6. Unload the snapshot
7. Start the pump — from this point, Rust owns all updates

For V4: create a `UniswapV4LiquiditySnapshot` (or generalize the V3 snapshot to `CLLiquiditySnapshot` that works for both). The V4 snapshot fetches `ModifyLiquidity` events from the PoolManager between the snapshot block and current, same pattern.

### Slice 7: Wire the pump into `PyUniswapArbEngine.start()`

The `PyUniswapArbEngine` already has `pump_handle: Mutex<Option<JoinHandle>>` and `shutdown: AtomicBool`. Currently `start()` only freezes registration. Wire it to also spawn the unified pump:

```rust
fn start(&self, rpc_url: String) -> PyResult<()> {
    self.engine.lock().start();

    // Spawn the unified pump
    let engine = Arc::clone(&self.engine);
    let shutdown = Arc::clone(&self.shutdown);
    let handle = UniswapEnginePump::spawn(rpc_url, engine, &shutdown)
        .map_err(|e| PyRuntimeError::new_err(e))?;
    *self.pump_handle.lock() = Some(handle);

    Ok(())
}
```

The bot's `main()` calls `engine.freeze()` + `engine.initial_solve()` + `engine.start(rpc_url)`. After `start()`, the pump runs autonomously — Python no longer needs a WS subscription for pool events.

### Slice 8: Eliminate Python from the event path

Replace the bot's Python WS subscription + `process_single_event()` + `schedule_dispatch()` with a result-polling loop:

**Before** (current):
```python
# Python subscribes to logs and newHeads
# Every log → process_single_event → update Python pool → push to Rust
# Every newHead → try_dispatch → read results → simulate → submit
```

**After**:
```python
# Rust pump: WS → newHeads → eth_getLogs → decode → process_block → solve
# Python: subscribe to newHeads only (for fee/nonce updates)
#         on newHead: read engine.latest_results() → simulate → submit
```

The Python `on_event` handler is removed entirely. The Python `on_block` handler is simplified to only update fee/nonce state and try dispatch. The `process_single_event()` function, `pending_v2/v3/v4_updates` lists, `schedule_dispatch()`, and the logs WS subscription are all deleted from the bot.

Python pool objects are still needed for **encoding** (calculating output amounts for payload construction in `encode_payloads()`). But their state is no longer the authority — they become read-through caches that *could* be updated from Rust state if needed, or simply kept frozen at construction time for the encoding math (which only needs current sqrt_price/liquidity/tick, all available from the Rust engine).

### Design decisions

- **Single pump, not three**: One `UniswapEnginePump` fetches all relevant logs in one `eth_getLogs` call, not three separate pumps making three RPC calls per block. This reduces WS load and RPC round-trips. The routing to V2/V3/V4 engines happens inside the engine's `process_block`.
- **Push-based WS, pull-based logs**: The existing pump pattern subscribes to `newHeads` for block boundary notification, then pulls logs via `eth_getLogs`. This is correct — real-time log subscription (`eth_subscribe("logs")`) is fragile (provider-dependent, doesn't handle reconnection well) and unnecessary when we know the address and topic filter. One `eth_getLogs` per block is deterministic and retryable.
- **Mint/Burn decoding in Rust, not tick_data dump from Python**: The Rust engine's `tick_data` is a `HashMap<i32, TickInfo>`. Mint and Burn affect 2–4 tick entries. The Rust `apply_liquidity_update` modifies exactly those entries. This is O(1) per Mint/Burn vs O(n) for the current full-tick-data dump from Python.
- **Python pools kept for encoding only**: The encoding functions (`encode_v3v2_payloads`, etc.) call `pool.calculate_tokens_out_from_tokens_in()` to compute output amounts for the next hop. These reads could come from Rust state instead, but migrating encoding to Rust is Plan 079's scope. For now, Python pools are read-only consumers during encoding.
- **V4 snapshot modeled on V3**: V4 ModifyLiquidity events are structurally identical to V3 Mint/Burn (tick_lower, tick_upper, liquidity_delta). The snapshot infrastructure (source protocol, pending_updates, fetch_new_events) is generalizable with a thin adapter for V4's event format and PoolManager-address scoping.
- **`process_logs()` kept for testing**: The Python-facing `process_logs(v2_updates, v3_updates, v4_updates, block_number)` is useful for synchronous unit tests that don't need a WS connection. It's not removed — it's just not called in the hot loop anymore.
- **Backfill is one-time, not per-block**: The snapshot is loaded once at startup. After `fetch_new_events` + `process_logs`, the pump takes over. There's no periodic backfill — the pump's `eth_getLogs` on each block is the equivalent.
- **No backfill persistence cache**: The backfill cost on restart is bounded by bot downtime (typically 0–10 blocks on Ethereum), not by snapshot age. Writing backfilled tick_data to the snapshot DB would corrupt its point-in-time semantics — the snapshot stores verified on-chain state at block N, not "whatever we last computed." If snapshot age becomes a material startup cost, the fix is to update the snapshot generation pipeline to produce newer snapshots.

## Files Involved

**Primary (new):**
- `rust/src/bot_core/v3_mint_burn_decoder.rs` — V3 Mint/Burn event decoders
- `rust/src/bot_core/v4_modify_liquidity_decoder.rs` — V4 ModifyLiquidity event decoder
- `rust/src/optimizers/v4_engine_pump.rs` — V4 pump (mirrors V2/V3 pump pattern)
- `rust/src/optimizers/uniswap_engine_pump.rs` — Unified pump that fetches all logs and routes

**Primary (modified):**
- `rust/src/bot_core/mod.rs` — Register new decoder modules
- `rust/src/optimizers/v3_block_engine.rs` — Add `apply_liquidity_update`, extend `process_block` for Mint/Burn
- `rust/src/optimizers/v4_block_engine.rs` — Add `apply_liquidity_update`, `process_block`, `registered_addresses()`
- `rust/src/optimizers/v3_engine_pump.rs` — Update log filter to include Mint/Burn topics
- `rust/src/optimizers/uniswap_engine.rs` — Add `process_block(&[Log], u64)` routing method, wire pump into `PyUniswapArbEngine.start()`
- `rust/src/optimizers/mod.rs` — Register new pump modules
- `examples/eth_backrun_v2_v3_v4_rust.py` — Remove `process_single_event`/`on_event`/`schedule_dispatch`, simplify to result-polling
- `src/degenbot/uniswap/v3_snapshot.py` — Wire `fetch_new_events` into bot startup
- `src/degenbot/uniswap/v4_snapshot.py` (new) — V4 liquidity snapshot

**No change needed:**
- `rust/src/bot_core/v2_sync_decoder.rs` — Already complete
- `rust/src/bot_core/v3_swap_decoder.rs` — Already complete
- `rust/src/bot_core/v4_swap_decoder.rs` — Already complete
- `rust/src/optimizers/mobius_int_exact.rs` — Solver is stateless, no change
- `rust/src/optimizers/mobius_v3_int.rs` — Tick range sequence builder is stateless, no change

## Implementation Order

### Slice 1: V3 Mint/Burn event decoders

1. Create `rust/src/bot_core/v3_mint_burn_decoder.rs` with `V3_MINT_TOPIC`, `V3_BURN_TOPIC`, `decode_v3_mint_log()`, `decode_v3_burn_log()`
2. Register in `rust/src/bot_core/mod.rs`
3. Write unit tests: decode synthetic Mint/Burn logs, verify field extraction
4. Run: `just test-rust` — expect all pass

### Slice 2: V3 `apply_liquidity_update` + `process_block` with Mint/Burn

1. Add `V3LiquidityUpdate` struct and `apply_liquidity_update` method on `V3PoolState`
2. Extend `V3BlockEngine::process_block()` to dispatch Mint/Burn events
3. Add `V3BlockEngine::process_liquidity_updates()` for pre-decoded testing (matches `process_swap_updates` pattern)
4. Write unit tests: register pool, apply Mint then Swap, verify tick_data is correct
5. Run: `just test-rust` — expect all pass

### Slice 3: V3EnginePump filter + V4 ModifyLiquidity decoder + V4 `process_block`

1. Update `v3_engine_pump.rs` filter to include Mint/Burn topics
2. Create `rust/src/bot_core/v4_modify_liquidity_decoder.rs` with `V4_MODIFY_LIQUIDITY_TOPIC`, `decode_v4_modify_liquidity_log()`
3. Add `apply_liquidity_update` and `process_block(&[Log], u64)` to `V4BlockEngine`
4. Add `registered_addresses()` and `pool_manager_address()` accessors to `V4BlockEngine` (needed for pump filter construction)
5. Register new decoder module in `rust/src/bot_core/mod.rs`
6. Write unit tests for V4 decoder and engine
7. Run: `just test-rust` — expect all pass

### Slice 4: V4EnginePump

1. Create `rust/src/optimizers/v4_engine_pump.rs` mirroring `V2EnginePump` pattern
2. Subscribes to block headers, fetches V4 Swap + ModifyLiquidity logs for PoolManager address
3. Routes to `V4BlockEngine::process_block()`
4. Register in `rust/src/optimizers/mod.rs`
5. Write unit tests (filter construction, shutdown flag)
6. Run: `just test-rust` — expect all pass

### Slice 5: Unified `UniswapEnginePump`

1. Create `rust/src/optimizers/uniswap_engine_pump.rs`
2. Single `eth_getLogs` per block with all topics (Sync, V3 Swap/Mint/Burn, V4 Swap/ModifyLiquidity)
3. `UniswapEngine::process_block(&[Log], u64)` that routes to `v2_engine.process_block()`, `v3_engine.process_block()`, `v4_engine.process_block()`, then calls unified `rebuild_and_solve()`
4. Wire `PyUniswapArbEngine.start(rpc_url)` to spawn the pump
5. Keep `process_logs()` for testing — mark with doc comment "for testing only, not used in hot loop"
6. Write integration tests: register pools, start pump, feed mock logs, verify results
7. Run: `just test-rust` — expect all pass

### Slice 6: V3 snapshot backfill + V4 snapshot

1. In `build_paths()`, after loading the V3 snapshot and building pools, call `snapshot.fetch_new_events_async(current_block)` to get Mint/Burn events between snapshot block and current
2. Apply `pending_updates` to each Python pool
3. Push updated tick_data to Rust engine one final time via `process_logs()`
4. Unload the snapshot
5. Create `UniswapV4LiquiditySnapshot` (or `CLLiquiditySnapshot` if generalizable) — fetches ModifyLiquidity events from PoolManager, same `pending_updates` pattern
6. Apply same backfill-and-push for V4 pools
7. Run: `just test-python` + `just test-rust` — expect all pass
8. Run bot with `--dry-run` — verify V3/V4 pools have correct tick_data at live block

### Slice 7: Remove Python event path

1. In `main()`, replace the logs WS subscription + `on_event` + `process_single_event` + `schedule_dispatch` with pump-driven state
2. Keep `on_block` for fee/nonce updates and `try_dispatch` (which reads `engine.latest_results()` and dispatches profitable results)
3. Remove `process_single_event()`, `extract_topic0()`, `pending_v2/v3/v4_updates` lists
4. The coalesce mechanism is no longer needed — the pump processes all events per block atomically, and `on_block` triggers dispatch
5. Remove the `DISPATCH_COALESCE_MS` constant and `schedule_dispatch()` coroutine
6. Python WS subscription becomes `newHeads` only (no logs subscription)
7. Run: `just test-python` + manual dry-run test (default mode)

### Slice 8: Validate and clean up

1. Run `just lint` + `just test-all`
2. Update `rust/CONTEXT.md` with pump terminology and state-ownership model
3. Update `plans/080-rust-bot-poc-path-to-profit.md` — mark F4 (tick_priors O(n)) as resolved by this plan
4. Update `plans/completed/081-v4-extension.md` runtime notes with pump reference
5. Remove dead code: `V2EnginePump`/`V3EnginePump` superseded by `UniswapEnginePump` (keep as alternative entry points or consolidate)

### Slice 8: Validate and clean up

### Per-slice test runs

Each slice runs `just test-rust` and/or `just test-python`.

### New unit tests (Rust)

```rust
// v3_mint_burn_decoder.rs
fn decode_v3_mint_valid()
fn decode_v3_mint_wrong_topic()
fn decode_v3_burn_valid()
fn decode_v3_burn_negative_liquidity()

// v4_modify_liquidity_decoder.rs
fn decode_v4_modify_liquidity_valid()
fn decode_v4_modify_liquidity_wrong_topic()

// v3_block_engine.rs
fn apply_mint_updates_tick_data()
fn apply_burn_updates_tick_data()
fn mint_then_swap_produces_correct_sequence()
fn process_block_with_mint_and_swap()

// v4_block_engine.rs
fn apply_modify_liquidity_updates_tick_data()
fn v4_process_block_with_swap_and_modify()
```

### New unit tests (Python)

```python
# tests/uniswap/test_v4_snapshot.py
def test_v4_snapshot_fetches_modify_liquidity_events()
def test_v4_snapshot_pending_updates()
```

### Integration tests

- Register V3 pool → start pump → push mock Swap + Mint logs → verify engine results
- Register V4 pool → start pump → push mock Swap + ModifyLiquidity logs → verify engine results
- Full end-to-end: `--dry-run` mode with Rust pump active, verify opportunities detected

### Existing test coverage

- `v3_swap_decoder.rs` tests (7 tests) — unchanged, Slice 1 adds parallel Mint/Burn tests
- `v4_swap_decoder.rs` tests (7 tests) — unchanged, Slice 3 adds parallel ModifyLiquidity tests
- `test_engine_v3v3_vs_brent.py` (13 tests) — engine solver unchanged, results should be identical
- `test_uniswap_arb_engine.py` (9 tests) — `process_logs()` kept for testing

## Benefits

- **Latency**: Mint/Burn decoding in Rust (~50ns) vs Python handler + full tick_data extraction (~200µs for USDC/WETH). Per-block: one `eth_getLogs` round-trip replacing Python WS event processing + PyO3 crossings. Eliminates O(n) tick_data transfer entirely.
- **Correctness**: Backfill ensures V3/V4 tick_data is current at startup. Mint/Burn handling in Rust ensures tick_data stays current during the hot loop. No more silent drift after LP position changes.
- **Locality**: Rust owns the entire state pipeline — event reception, decoding, state mutation, solving, result storage. Python reads results and submits. Zero state duplication between Python and Rust for tick data.
- **Simplicity**: Remove `process_single_event()`, `schedule_dispatch()`, coalesce window, Python WS log subscription, `pending_updates` lists. The bot's `main()` loop becomes: start pump → on_newHead: read results → simulate → submit.
- **Resilience**: Rust pump handles WS reconnection internally (Alloy provider auto-reconnects). `eth_getLogs` is retryable on failure. No Python-side reconnection + reconciliation needed.

## Risks

- **Alloy WS stability**: The Rust pump depends on Alloy's WS provider for block subscriptions. If Alloy's WS implementation is less stable than web3.py's (used by the current bot), pump failures could go undetected. **Mitigation**: Add a heartbeat check — if no block header received within 30s, restart the pump. Log gaps loudly. The existing Rust `AlloyProvider` is used in production for subscriptions elsewhere.
- **`eth_getLogs` per-block overhead**: One `eth_getLogs` call per block with ~1000 address filters and 6 topic filters. Some RPC providers limit log filter complexity or charge per-log. **Mitigation**: If address filter is too large (>1000 addresses), fall back to unfiltered subscription + address check on decode (same approach as Python's Slice 7 in Plan 080). Also: for V4, only one address (PoolManager) — trivial filter.
- **Tick bitmap porting risk for V3 Mint/Burn**: When a new tick is initialized by a Mint event (tick_lower or tick_upper not previously in tick_data), the tick_bitmap must also be updated. The Rust `apply_liquidity_update` needs to handle bitmap initialization correctly — the Python `update_liquidity_map` does this via `_tick_data_fetcher` lazy fetch. **Mitigation**: The Rust engine receives full tick_data at registration time. New ticks from Mint events are `or_insert`-ed into the HashMap. The tick_bitmap is only needed for tick-range construction (`build_int_v3_sequence`), which reads from `tick_data` directly (the bitmap is an optimization for the Python side). Verify that `build_int_v3_sequence` works correctly without bitmap updates for newly initialized ticks.
- **V3 snapshot generalization to V4**: The `UniswapV3LiquiditySnapshot` is V3-specific (event hashes, pool-address keying). Generalizing to V4 requires changing the key from `ChecksumAddress` (pool contract) to `(Address, PoolId)` (PoolManager + pool ID) and handling V4's `ModifyLiquidity` event. This is mechanically straightforward but touches the snapshot's core data structures. **Mitigation**: Create a separate `UniswapV4LiquiditySnapshot` rather than generalizing — avoids risk of breaking the V3 snapshot which is heavily tested.
- **Encoding uses pool calculate methods**: The `encode_payloads()` functions call `pool.calculate_tokens_out_from_tokens_in()` on Python pool objects. If Python pool state is not updated (because Rust owns updates), the encoding will use stale state for output-amount calculation. **Mitigation**: Two options: (a) keep updating Python pools from Rust state after each `process_block` (adds one PyO3 callback per updated pool), or (b) compute output amounts in Rust and expose them in the result. Option (a) is lower risk for now — the Python pool state is a write-through cache from Rust. Option (b) is Plan 079's territory (Rust-owned calculation).
- **Pump task lifetime**: The pump runs as a Tokio spawned task. If the task panics, the engine stops receiving updates silently. **Mitigation**: `JoinHandle` stored in `PyUniswapArbEngine` — poll it in `latest_results()` and log/restart if the task has finished.

## Relationship to Other Plans

- **Plan 079** (Rust-Owned Bot Core): This plan is a prerequisite for Plan 079's Slice 8 (V3BlockEngine with pump) and Slice 11 (PyV3ArbEngine + V3EnginePump), which are currently marked complete but only partially wired — the engine exists but the pump is not activated. This plan completes the activation. Plan 079's Slice 13 (Python thin handles) depends on Rust owning state — this plan enables that.
- **Plan 080** (Rust Bot POC — Path to Profit): F4 (tick_priors O(n) per event) is resolved by this plan. F8 (process_single_event swallows ExternalUpdateError) is resolved by removing `process_single_event` entirely. Slice 6 (WS reconnection) is simplified — Rust pump handles its own reconnection; Python only needs fee/nonce reconnection.
- **Plan 081** (V4 Extension — completed): Slices 1–3 created `V4BlockEngine` with `apply_swap` but no pump. This plan adds the missing `process_block` and `V4EnginePump`, completing the V4 engine's intended architecture.
- **Independent**: No dependencies on Curve, Balancer, Aerodrome, or database plans.

## Status

- [x] Slice 1: V3 Mint/Burn event decoders
- [x] Slice 2: V3 `apply_liquidity_update` + `process_block` with Mint/Burn
- [x] Slice 3: V3EnginePump filter + V4 ModifyLiquidity decoder + V4 `process_block`
- [x] Slice 4: V4EnginePump
- [x] Slice 5: Unified `UniswapEnginePump`
- [x] Slice 6: V3 snapshot backfill + V4 snapshot
- [x] Slice 7: Remove Python event path
- [x] Slice 8: Validate and clean up
