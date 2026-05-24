# Plan 079: Rust-Owned Bot Core

## Overview

Move all runtime state and computation into a single Rust `BotCore` struct, following the Polars model: Python is the cockpit (construction, configuration, transaction submission), Rust is the engine (all data, all computation, all I/O during the hot loop). Every Python object becomes a thin PyO3 handle over a Rust-owned key.

Plan 078 proved the thesis for V2 detection. This plan extends Rust ownership to *everything*: pool state, token metadata, calculation methods, swap encoding, state history, and subscription management.

## Problem

### Deletion test

If you deleted `ArbPoolCacheAdapter`, `ArbitragePath.notify()`, `LogListener`, `_notify_subscribers()`, `StateCache`, `ConcentratedLiquidityStateManager`, every pool's `external_update()`, and the `Bot.start_listening()` consume loop, the arbitrage system would still work — because the V2BlockEngine already does all of that in Rust. These Python structures exist solely to shuttle data between Python objects that shouldn't own data in the first place.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| 4 copies of reserve state (pool._state_cache, adapter, PyPoolCache, V2BlockEngine) | `UniswapV2Pool`, `ArbPoolCacheAdapter`, `PyPoolCache`, `V2BlockEngine` | Sync bugs, wasted memory, 6+ PyO3 crossings per pool per block |
| Python pub/sub chain: pool → adapter → solver → path → subscriber | `PublisherMixin._notify_subscribers()` | 5-deep method call chain to keep Rust cache in sync with Python state |
| V3 tick bitmap walk in Python | `UniswapV3Pool._compute_tick_ranges()` | `dict[int, TickInfo]` traversal + `StateCache` lock — 5-10µs/path, must happen every block |
| Curve `get_dy()` calculation in Python | `calculations/stableswap.py` | Newton iteration over Python ints — 50-100µs, the hottest Curve path |
| Balancer invariant calculation in Python | `libraries/stable_math.py`, `libraries/weighted_math.py` | ~100µs, multiple `PowVersion` dispatch paths |
| Solidly stable swap math in Python | `calculations/solidly_stable.py` | ~20µs, blocks Aerodrome engine |
| Log decoding via `eth_abi` Python library | `log_decoders.py` | ~5-10µs per event; Rust decodes Sync in ~50ns |
| Swap encoding crosses back into Python | `ArbitragePath.build_swap_amounts()` | Rust solves the path, Python reads results, Python calls `calculate_tokens_out_from_tokens_in()`, Python calls `build_swap_amount()`, Python calls `encode()` — 4+ Python steps to produce calldata |

## Solution

### Architecture: BotCore owns everything

```rust
/// The single owner of all runtime state.
/// All pool data, token metadata, engines, and encoded results live here.
/// Python holds PyBotCore — an Arc pointing here.
pub struct BotCore {
    // -- Registries (Rust owns the data) --
    pools: HashMap<Address, PoolEntry>,
    tokens: HashMap<Address, TokenEntry>,
    paths: HashMap<u64, PathEntry>,

    // -- Per-type engines (read from pools, no state duplication) --
    v2_engine: V2BlockEngine,
    // v3_engine: V3BlockEngine,
    // curve_engine: CurveBlockEngine,
    // balancer_engine: BalancerBlockEngine,

    // -- Connections --
    rpc_url: String,
    provider: Arc<AlloyProvider>,
    chain_id: u64,

    // -- Lifecycle --
    running: AtomicBool,
    pump_handles: Vec<JoinHandle<()>>,
}
```

```rust
/// A single pool's state — replaces UniswapV2Pool, UniswapV3Pool, etc.
/// Pool-type-specific fields are in the enum variants.
pub enum PoolEntry {
    V2(V2PoolState),
    V3(V3PoolState),
    Curve(CurvePoolState),
    BalancerWeighted(BalancerWeightedState),
    BalancerStable(BalancerStableState),
}

pub struct V2PoolState {
    address: Address,
    token0: Address,
    token1: Address,
    fee_token0: (u64, u64),  // (gamma_numer, fee_denom)
    fee_token1: (u64, u64),
    factory: Address,
    deployer: Address,

    // Mutable state
    reserves_token0: U256,
    reserves_token1: U256,
    update_block: u64,

    // Reorg journal — "before" values for rollback (V2 degenerate: delta = full state)
    journal: ReorgJournal<V2BlockDelta>,
}

pub struct V3PoolState {
    address: Address,
    token0: Address,
    token1: Address,
    fee: u64,
    tick_spacing: i32,
    factory: Address,

    // Mutable state
    sqrt_price_x96: U256,
    liquidity: u128,
    tick: i32,
    update_block: u64,

    // Tick data (replaces Python dict traversal)
    tick_bitmap: BTreeMap<i16, u256>,
    tick_data: BTreeMap<i32, TickInfo>,

    // Reorg journal — scalars + per-tick priors (typically 0-4 entries per block)
    journal: ReorgJournal<V3BlockDelta>,
}

pub struct TokenEntry {
    address: Address,
    name: String,
    symbol: String,
    decimals: u8,
    chain_id: u64,
}

pub struct PathEntry {
    path_id: u64,
    input_token: Address,
    pool_ids: Vec<(u64, bool)>,  // (pool_id, zero_for_one)
    max_input: Option<U256>,
}
```

### Python handles

```python
# Thin wrappers — data lives in Rust, Python holds keys
class Bot:
    _core: PyBotCore  # Arc<BotCore>

    def build_pool(self, address: str) -> Pool:
        """PyO3 → BotCore.build_pool() → fetch from chain → insert into BotCore.pools → return Pool handle"""

    def start(self) -> None:
        """PyO3 → BotCore.start() → spawn V2EnginePump, V3EnginePump, etc."""

    def results(self) -> list[Opportunity]:
        """PyO3 → BotCore.results() → pre-encoded calldata + profit"""

class Pool:
    _core: PyBotCore   # shared Arc
    _pool_id: u64       # key into BotCore.pools

    @property
    def reserves_token0(self) -> int: ...   # PyO3 → BotCore.pools[pool_id].reserves_token0

    def calculate_tokens_out_from_tokens_in(self, token_in, amount) -> int:
        """PyO3 → BotCore.calculate_tokens_out(pool_id, zero_for_one, amount)"""

    def external_update(self, update) -> None:
        """PyO3 → BotCore.update_pool(pool_id, reserves0, reserves1, block)"""

class Token:
    _core: PyBotCore
    _address: str

    @property
    def symbol(self) -> str: ...   # PyO3 → BotCore.tokens[addr].symbol
```

### The per-block lifecycle

```rust
// Everything happens inside BotCore — no Python in the loop
//
// Log subscription strategy:
//   - Primary: `eth_subscribe("logs", filter)` — events are pushed in real-time
//   - Block boundary: `eth_subscribe("newHeads")` — signals block completion
//   - Catch-up/backfill: `eth_getLogs()` — only used on startup to fill from
//     last-seen block, or to reconcile missed events under heavy load
//   - No per-block `eth_getLogs` in the hot path
impl EnginePump {
    async fn run(&self) {
        // Subscribe to log events (pushed) — Sync for V2, Swap/Mint/Burn for V3
        let log_stream = self.provider.subscribe_logs(&self.log_filter).await;
        // Subscribe to block headers (boundary signal)
        let block_stream = self.provider.subscribe_blocks().await;

        // Buffer incoming logs per block until block header arrives
        let mut pending_logs: Vec<Log> = Vec::new();

        loop {
            tokio::select! {
                Some(log) = log_stream.next() => {
                    pending_logs.push(log);
                }
                Some(header) = block_stream.next() => {
                    // Block complete — process all accumulated logs
                    // 1. Decode + update pool state in BotCore.pools
                    for log in &pending_logs {
                        if let Some(event) = decode_sync_log(log) {
                            self.core.update_v2_pool(event.pool_address, event.reserve0, event.reserve1, header.number);
                        }
                        if let Some(event) = decode_v3_swap_log(log) {
                            self.core.update_v3_pool(...);
                        }
                    }
                    pending_logs.clear();

                    // 2. Solve
                    let results = self.core.v2_engine.solve_all(None);

            // 3. Encode swap calldata for profitable paths
            let opportunities = results.iter()
                .filter(|r| !r.profit.is_zero())
                .map(|r| {
                    let path = &self.core.paths[&r.path_id];
                    let calldata = self.core.encode_swap(path, r.optimal_input);
                    Opportunity { path_id: r.path_id, input: r.optimal_input, profit: r.profit, calldata }
                })
                .collect();

            // 4. Store for Python to read
            self.core.store_opportunities(header.number, opportunities);
        }
    }
}
```

```python
# Python per-block loop — one call
for opp in bot.results():
    if opp.profit > THRESHOLD:
        submit_transaction(opp.calldata)
```

### Calculation methods owned by Rust

Every pool's `calculate_tokens_out_from_tokens_in()` and `calculate_tokens_in_from_tokens_out()` lives in Rust. The Python pool handle calls through to it:

```rust
impl BotCore {
    pub fn calculate_tokens_out(&self, pool_id: u64, zero_for_one: bool, amount_in: U256) -> U256 {
        let pool = &self.pools[&pool_id];
        match pool {
            PoolEntry::V2(state) => {
                let (r_in, r_out, gamma, fee) = if zero_for_one {
                    (state.reserves_token0, state.reserves_token1, state.fee_token0.0, state.fee_token0.1)
                } else {
                    (state.reserves_token1, state.reserves_token0, state.fee_token1.0, state.fee_token1.1)
                };
                constant_product_calc_exact_in(amount_in, r_in, r_out, gamma, fee)
            }
            PoolEntry::V3(state) => { /* ... */ }
            PoolEntry::Curve(state) => { /* ... */ }
            PoolEntry::BalancerWeighted(state) => { /* ... */ }
            PoolEntry::BalancerStable(state) => { /* ... */ }
        }
    }
}
```

### Swap encoding owned by Rust

The pump produces pre-encoded calldata. No Python ABI encoding needed:

```rust
pub struct EncodedOpportunity {
    pub path_id: u64,
    pub input_token: Address,
    pub profit: U256,
    pub block_number: u64,
    pub calls: Vec<EncodedCall>,  // pre-encoded swap calls
}

impl BotCore {
    pub fn encode_swap(&self, path: &PathEntry, optimal_input: U256) -> Vec<EncodedCall> {
        let mut calls = Vec::new();
        let mut amount_in = optimal_input;

        for &(pool_id, zero_for_one) in &path.pool_ids {
            let amount_out = self.calculate_tokens_out(pool_id, zero_for_one, amount_in);
            let call = match &self.pools[&pool_id] {
                PoolEntry::V2(state) => encode_v2_swap(state.address, zero_for_one, amount_in, amount_out),
                PoolEntry::V3(state) => encode_v3_swap(state.address, zero_for_one, amount_in, amount_out),
                // ...
            };
            calls.push(call);
            amount_in = amount_out;
        }

        calls
    }
}
```

### Reorg journal — delta-based rollback

Every pool carries a **reorg journal**: a bounded deque of per-block deltas storing the *prior* values of modified state fields. On forward progress, the current mutable state is updated and the "before" values are stashed in the journal. On reorg, journal entries are popped and their prior values restored into the current state.

Swap calculations and the hot path **never touch the journal** — they always read the current mutable fields. Zero penalty.

```rust
/// Per-block delta for a V2 pool.
/// Stores the reserve values *before* the update at this block.
struct V2BlockDelta {
    block: u64,
    reserve0_before: U256,
    reserve1_before: U256,
}

/// Per-block delta for a V3 pool.
/// Stores the *prior* tick-level state for each modified tick.
struct V3BlockDelta {
    block: u64,
    /// Scalar fields before this block's update.
    sqrt_price_x96_before: U256,
    liquidity_before: u128,
    tick_before: i32,
    /// Per-tick prior state. Only ticks modified by Mint/Burn events
    /// in this block appear here — typically 0–4 entries.
    tick_priors: Vec<(i32, TickBefore)>,
}

struct TickBefore {
    liquidity_gross: u128,
    liquidity_net: i128,
}
```

**On forward progress** (new Sync/Mint/Burn at block N):
1. Capture "before" values of fields that will change
2. Push `BlockDelta { block: N, ..._before }` into journal
3. Apply update to current mutable state
4. If journal exceeds `max_depth`, pop the oldest entry (it is no longer rollback-reachable)

**On reorg** (rollback to block B):
1. Pop each delta with `block >= B` from the journal
2. For each delta, restore `*before` values into the current mutable state
3. For V3: if a tick prior has `liquidity_gross == 0`, remove the tick entry from the map
4. Done — current state is exactly consistent with the reorged chain

**Memory profile vs full-state cloning:**

| | Full clone per block | Delta journal |
|---|---|---|
| V2 per pool (depth 8) | 8 × (2 U256) = 16 values | 8 × (2 U256) = 16 values (degenerate — delta = full state) |
| V3 per pool (2000 ticks, depth 8) | 8 × (2000 tick entries + scalars) ≈ 16,000 entries | 2000 entries + 8 × 4 ≈ 2,032 entries |

V2 is the degenerate case where the delta *is* the full state (two reserves). V3 is where the delta journal vastly outperforms cloning — the per-block delta is typically 2–4 tick modifications, not the full tick map.

Python drives reorg through the thin handle:

```python
pool.discard_before_block(19000000)  # PyO3 → BotCore.discard_before_block()
pool.restore_before_block(19000000)   # PyO3 → BotCore.restore_before_block()
```

### Engines read from BotCore directly — no state duplication

V2BlockEngine today copies `IntHopState` into its own HashMap. In the BotCore model, the engine reads from `BotCore.pools` directly:

```rust
impl V2BlockEngine {
    /// Resolve a path's pool IDs into hop states, reading from BotCore.pools.
    fn resolve_path(pools: &HashMap<u64, PoolEntry>, pool_ids: &[u64], resolved: &mut ResolvedPath) {
        for &pool_id in pool_ids {
            let PoolEntry::V2(state) = &pools[&pool_id] else { return };
            let (r_in, r_out, gamma, fee) = /* read from state */;
            resolved.int_hops.push(IntHopState::new(r_in, r_out, gamma, fee));
            // ...
        }
    }
}
```

No adapter. No pub/sub. The engine and the pool state share the same owner.

## Design decisions

- **BotCore is the single owner**: All mutable state lives in `BotCore`. Every Python object is a thin handle carrying a key (pool_id, address) into BotCore's HashMaps. This eliminates state duplication entirely — one copy of reserves, one copy of tick data, one source of truth.
- **PoolEntry enum, not trait objects**: Pool-type dispatch is a `match` on the enum variant. No vtable indirection, no dyn dispatch. Each variant's fields are exactly what that pool type needs — no wasted fields from a unified struct.
- **Engines are readers, not state owners**: V2BlockEngine, the future V3BlockEngine, etc. read from `BotCore.pools` instead of maintaining their own state copies. `process_block()` applies updates to `BotCore.pools` directly, then the engine resolves paths from the same HashMap.
- **Calculation methods on BotCore, not on pool objects**: `calculate_tokens_out_from_tokens_in()` is a method on `BotCore` that matches on `PoolEntry` variant and dispatches to the appropriate pure function. This replaces the current `Calc` mixin pattern (which requires Python method dispatch through MRO).
- **Swap encoding in Rust**: The pump produces `EncodedOpportunity` with pre-encoded calldata. Python never calls `eth_abi.encode()` or `Web3.keccak()`. The Rust `abi_encoder` module already supports this.
- **Delta-journal reorg**: Each pool carries a bounded deque of per-block deltas storing the *prior* values of modified state fields. On forward progress, the current mutable state is updated and "before" values are stashed. On reorg, deltas are popped and prior values restored. Swap calculations never touch the journal — they always read the current mutable fields. V2 is the degenerate case (delta = full state); V3 is where the delta journal vastly outperforms cloning (2–4 tick modifications per block vs 2000-entry full clone).
- **Authoritative fields, not derived**: Pool state fields (`reserve0`, `reserve1`, `sqrt_price_x96`, etc.) are the authoritative current values. The journal is an append-only log for rollback — it is never the source of truth for reads. This keeps the write path simple (update fields, then stash "before" values) and the read path fast (no indirection through VecDeque).
- **Construction still driven by Python**: `bot.build_pool("0x...")` crosses PyO3, does RPC/DB I/O in Rust, inserts into BotCore. Python never sees intermediate data. The builder pattern (V2Builder, V3Builder, etc.) becomes a Rust-side implementation detail.
- **LogListener eliminated**: BotCore's pump subscribes to block headers and log events directly, decodes them, and updates pools. The Python LogListener, LOG_HANDLERS, and the consume loop are all replaced by the pump loop.
- **Log subscription strategy — push, not pull**: Events arrive via `eth_subscribe("logs", filter)` in real-time. Block headers via `eth_subscribe("newHeads")` signal block completion. Logs are buffered per block and processed when the block header arrives. `eth_getLogs` is **only** used for startup backfill (fetch historical logs from last-seen block) or reconciliation (verify none were missed under heavy load). No per-block `eth_getLogs` in the hot path — that would add a round-trip per block for no benefit when the WS connection already pushes events.
- **Pub/sub eliminated for hot path**: The `_notify_subscribers` → `adapter.notify()` → `path.notify()` chain is replaced by the pump solving paths after updating pools. Pub/sub may still exist for cold-path consumers (UI, logging), but it's no longer in the arbitrage hot path.

## Files involved

### New Rust modules

| File | Purpose |
|------|---------|
| `rust/src/bot_core/mod.rs` | `BotCore` struct + `PoolEntry`, `TokenEntry`, `PathEntry` enums/structs |
| `rust/src/bot_core/pool_state.rs` | `V2PoolState`, `V3PoolState`, `CurvePoolState`, `BalancerWeightedState`, `BalancerStableState` |
| `rust/src/bot_core/token_state.rs` | `TokenEntry` |
| `rust/src/bot_core/path_state.rs` | `PathEntry` + `EncodedOpportunity` |
| `rust/src/bot_core/calculations.rs` | `calculate_tokens_out`, `calculate_tokens_in` dispatch methods |
| `rust/src/bot_core/encoding.rs` | `encode_swap`, `encode_v2_swap`, `encode_v3_swap`, etc. |
| `rust/src/bot_core/history.rs` | `ReorgJournal<D>` management, `push_delta`, `discard_before_block`, `restore_before_block` |
| `rust/src/bot_core/py_bot.rs` | `PyBotCore` pyclass (thin PyO3 handle) |
| `rust/src/bot_core/py_pool.rs` | `PyPool` pyclass (thin handle over pool_id) |
| `rust/src/bot_core/py_token.rs` | `PyToken` pyclass (thin handle over address) |

### New Rust math ports

| File | Source | What it replaces |
|------|--------|-----------------|
| `rust/src/calculations/constant_product.rs` | `v2_functions.py` | `constant_product_calc_exact_in`, `constant_product_calc_exact_out` |
| `rust/src/calculations/solidly_stable.rs` | `calculations/solidly_stable.py` | `calc_d`, `calc_k`, `calc_f`, `calc_exact_in_stable`, `calc_exact_in_volatile`, `get_y_solidly` |
| `rust/src/calculations/camelot.rs` | `calculations/camelot.py` | `f_camelot`, `k_camelot`, `get_y_camelot` |
| `rust/src/calculations/stableswap.rs` | `calculations/stableswap.py` | `calc_d`, `calc_dp`, `stableswap_get_d`, `stableswap_get_y`, `stableswap_newton_y` |
| `rust/src/calculations/weighted_math.rs` | `balancer/libraries/weighted_math.py` | `calculate_invariant`, `_calc_out_given_in`, `_calc_in_given_out` |
| `rust/src/calculations/stable_math.rs` | `balancer/libraries/stable_math.py` | `_calculate_invariant`, `_calc_out_given_in`, `_calc_in_given_out` |
| `rust/src/calculations/v3_swap.rs` | `v3_libraries/swap.py`, concentrated liquidity | Tick bitmap walk, virtual reserves, swap calculation |

### Modified Python modules

All pool classes (`UniswapV2Pool`, `UniswapV3Pool`, etc.) become thin PyO3 handles. Their current internal state and calculation methods are deleted — they delegate to `BotCore` through their `_core` reference. The class hierarchy flattens: no more mixin composition, no more `StateMixin`, `CalcMixin`, `PublisherMixin`.

### Deleted Python modules

| Module | Why deleted |
|--------|------------|
| `types/state_cache.py` | Replaced by Rust `VecDeque<Snapshot>` |
| `types/pool_pickle.py` | Rust state isn't pickled |
| `types/concrete.py` (`PublisherMixin`, `Subscriber`) | No pub/sub for hot path |
| `arbitrage/optimizers/pool_cache_adapter.py` | Engine reads BotCore directly |
| `listener/log_listener.py` | Pump drives log processing |
| `uniswap/log_decoders.py` | Rust decodes events |
| `calculations/*.py` | Ported to Rust |
| `balancer/libraries/*.py` | Ported to Rust |

## Implementation order

### Slice 1: BotCore struct + V2 pool state owner

1. Create `rust/src/bot_core/` module with `BotCore`, `PoolEntry::V2`, `TokenEntry`, `PathEntry`
2. Implement `BotCore::register_v2_pool()`, `update_v2_pool()`, `calculate_tokens_out()` for V2
3. Wire V2BlockEngine to read from `BotCore.pools` instead of its own HashMap
4. Expose `PyBotCore`, `PyPool`, `PyToken` pyclasses
5. Write tests: register pool → update → calculate tokens out → matches Python
6. Run: `just test-rust` + `just test-python` — expect all pass

### Slice 2: V2 constant-product math in Rust

1. Port `constant_product_calc_exact_in` and `constant_product_calc_exact_out` from `v2_functions.py`
2. Use the existing `IntHopState::swap()` method (already U512-based) as the foundation
3. Add `BotCore::calculate_tokens_in()` for V2
4. Write tests: match Python outputs for edge cases (zero input, max reserves, fee rounding)
5. Run: `just test-rust` + `just test-python`

### Slice 3: V2 swap encoding in Rust

1. Implement `encode_v2_swap()` using the existing `abi_encoder` module
2. `V2_SWAP_SELECTOR = keccak256("swap(uint256,uint256,address,bytes)")[:4]`
3. Add `BotCore::encode_swap()` for V2 paths
4. Pump produces `EncodedOpportunity` with pre-encoded calldata
5. Write tests: encoded calldata matches Python `UniswapV2PoolSwapAmounts.encode()`
6. Run: `just test-rust` + `just test-python`

### Slice 4: V2 reorg journal in Rust

1. `ReorgJournal<V2BlockDelta>` — bounded VecDeque storing `(block, reserve0_before, reserve1_before)` per block
2. `push_delta()` — stash "before" values, then update current state
3. `discard_before_block()` — pop oldest deltas beyond max_depth (no longer rollback-reachable)
4. `restore_before_block()` — pop deltas at/after target block, restore "before" values into current state
5. V2 is the degenerate case: every delta captures the full prior state (two reserves)
6. Write tests: push deltas, discard, restore, restore syncs current reserves
7. Run: `just test-rust` + `just test-python`

### Slice 5: Solidly-stable math in Rust

1. Port `calculations/solidly_stable.py` to `rust/src/calculations/solidly_stable.rs`
2. `calc_d`, `calc_k`, `calc_f`, `calc_exact_in_stable`, `calc_exact_in_volatile`, `get_y_solidly`
3. All integer arithmetic — direct port from Python `//` to Rust `/`
4. Write tests: match Python outputs for known inputs
5. This unblocks Aerodrome engine registration in V2BlockEngine
6. Run: `just test-rust` + `just test-python`

### Slice 6: V3 pool state + Swap event decoder

1. Add `PoolEntry::V3(V3PoolState)` with `sqrt_price_x96`, `liquidity`, `tick`, `tick_bitmap`, `tick_data`
2. `ReorgJournal<V3BlockDelta>` — scalars (`sqrt_price_x96_before`, `liquidity_before`, `tick_before`) + per-tick priors (`Vec<(i32, TickBefore)>`, typically 0–4 entries). Tick data map is **not** cloned per snapshot; only modified tick priors are stashed in the delta.
3. Port V3 Swap event decoder: `V3_SWAP_TOPIC` + decode `(address, address, int256, int256, uint160, uint128, int24)` from log data
4. Add `BotCore::register_v3_pool()`, `update_v3_pool()`
5. `restore_before_block()` for V3: restore scalar fields + reverse-apply tick priors (re-add ticks with `liquidity_gross_before > 0`, remove those with 0)
6. Write tests: decode known V3 Swap events, update pool state, reorg rollback restores tick map correctly
7. Run: `just test-rust` + `just test-python`

### Slice 7: V3 tick bitmap walk in Rust

1. Port `gen_ticks()` from Python to Rust — walk `BTreeMap<i16, u256>` (tick_bitmap) → `BTreeMap<i32, TickInfo>` (tick_data)
2. Port `_compute_tick_ranges()` — produce `Vec<V3TickRangeInfo>` from current tick + bitmap + data
3. Build `V3TickRangeSequence` from Rust tick ranges — feed directly into `solve_piecewise()`
4. This is the hardest slice — the Python tick bitmap is a `dict[int, int]` with word/index bit manipulation
5. Tick walk reads from the current mutable `tick_data` map — no journal interaction needed (journal is only for rollback)
6. Write tests: tick walks match Python for known V3 pool states
7. Run: `just test-rust` + `just test-python`

### Slice 8: V3BlockEngine

1. Create `V3BlockEngine` mirroring `V2BlockEngine` pattern
2. Pump uses `eth_subscribe("logs", filter)` for real-time Swap/Mint/Burn events (not `eth_getLogs` per block). `eth_getLogs` is only used for startup backfill from last-seen block or reconciliation after missed events.
3. Log buffer pattern: incoming logs accumulate per block; block header from `eth_subscribe("newHeads")` signals completion → solve with all buffered logs → clear
4. V3 paths carry tick-range sequences, not just `IntHopState`
5. Add `PoolEntry::V3` dispatch to `BotCore::calculate_tokens_out()`, `encode_swap()`
6. Write tests: V3 engine results match Python `ArbSolver` for known pool states
7. Run: `just test-rust` + `just test-python`

### Slice 9: Stableswap math in Rust

1. Port `calculations/stableswap.py` to `rust/src/calculations/stableswap.rs`
2. All Newton iteration variants (D, Y, YD) — direct integer-arithmetic port
3. Port `DVariant`, `YVariant`, `YDVariant` enums
4. Add `PoolEntry::Curve(CurvePoolState)` + `CurvePoolState` with coin balances, amp, gamma, etc.
5. This unblocks Curve engine
6. Run: `just test-rust` + `just test-python`

### Slice 10: Balancer math in Rust

1. Port `libraries/weighted_math.py` and `libraries/stable_math.py`
2. Handle `PowVersion` dispatch (V1 vs V2 invariant)
3. Add `PoolEntry::BalancerWeighted` and `PoolEntry::BalancerStable`
4. Rate provider cache management
5. Run: `just test-rust` + `just test-python`

### Slice 11: PyV3ArbEngine + V3EnginePump

1. Create `PyV3ArbEngine` — PyO3 wrapper for `V3BlockEngine` (analogous to `PyV2ArbEngine`)
2. `register_pool(RegisterV3PoolParams)` → `register_path(Vec<V3PoolRef>)` → `start(rpc_url)` → `latest_results()`
3. Create `V3EnginePump` — async task driving `V3BlockEngine` (analogous to `V2EnginePump`)
4. Pump subscribes to `eth_subscribe("newHeads")` + `eth_subscribe("logs", V3_SWAP_TOPIC filter)`
5. Log buffer pattern: incoming Swap logs accumulate per block; block header signals completion → solve → clear
6. `freeze()` + `process_logs()` for synchronous testing
7. Write tests: register V3 pools, process Swap logs, verify results
8. Run: `just test-rust` + `just test-python`

### Slice 12: V2-V3 mixed-path engine

1. Create `UniswapEngine` — composes `V2BlockEngine` + `V3BlockEngine` (not extending either)
2. `HopType` enum (`V2`, `V3`) + `MixedPoolRef` (hop_type, pool_key, zero_for_one) identify each hop
3. `process_block()` routes Sync logs → V2 engine, Swap logs → V3 engine
4. Solver dispatch: V2-V2 → `mobius_solve_with_refinement`, V3-V3 → `solve_v3_v3`, V2-V3/V3-V2 → golden-section search over piecewise profit function
5. `start()` freezes registration on both sub-engines
6. `PyUniswapArbEngine` — PyO3 wrapper with `register_v2_pool`, `register_v3_pool`, `register_path`, `process_logs`, `latest_results`
7. Added `get_pool()` accessors to `V2BlockEngine` and `V3BlockEngine` for state reads from outside
8. Made `V3PoolState::build_sequence()` public
9. Write tests: 11 Rust tests + 9 Python tests covering registration, mixed V2-V3 and V3-V2 resolution, missing pool invalidation, process updates, pure V2 arb, freeze behavior
10. Run: `just test-rust` + `just test-python` — 362 Rust + 3212 Python pass, clippy clean

### Slice 13: Python thin handles + backward compat (V2/V3 only)

1. Replace Python pool class internals with PyO3 → BotCore delegation
2. `pool.reserves_token0` → `PyO3 → BotCore.pools[pool_id].reserves_token0`
3. `pool.external_update()` → `PyO3 → BotCore.update_v2_pool()`
4. `pool.calculate_tokens_out_from_tokens_in()` → `PyO3 → BotCore.calculate_tokens_out()`
5. Maintain backward-compatible API surface — existing tests pass without modification
6. Delete adapter, pub/sub, StateCache, LogListener (now unused)
7. Run: `just test-rust` + `just test-python` — all 3000+ tests still pass

## Testing

### Per-slice test runs

Each slice runs `just test-rust` and `just test-python`. For math ports (Slices 2, 5, 9, 10), exhaustive correctness tests against Python reference outputs are mandatory before proceeding.

### Math porting test pattern

For each calculation port:

1. Generate test vectors from the Python implementation with boundary values
2. Write Rust tests that assert the same outputs for the same inputs
3. Include edge cases: zero amounts, maximum uint256, fee boundary (gamma=0, gamma=fee_denom), single-coin pools

### Integration test continuity

The 3000+ existing Python tests must pass at every slice boundary. They become integration tests for the PyO3 handles — each `pool.reserves_token0` call exercises the Python → Rust → Python path.

## Benefits

- **Single source of truth**: One copy of reserve state, tick data, and pool parameters. No sync bugs, no adapter middlemen, no pub/sub fanout keeping 4 copies in lockstep.
- **All hot paths in Rust**: Detection (engine), calculation (`calculate_tokens_out`), encoding (`encode_swap`), state management (`external_update`, state history). Python never touches data during the per-block loop.
- **V3 unblocked**: Tick bitmap walk in Rust is the prerequisite for a `V3BlockEngine` that's faster than the Python path. Without Rust-owned tick state, a V3 engine would cross PyO3 per tick-range — worse than current Python.
- **Curve/Balancer acceleration**: These are the slowest per-update pools (50-100µs for Newton iteration). Pure Rust integer arithmetic on the same algorithms should be 10-50x faster.
- **Code reduction**: Delete `StateCache`, `PublisherMixin`, `ArbPoolCacheAdapter`, `LogListener`, `log_decoders.py`, all `calculations/*.py`, all `balancer/libraries/*.py`. Estimated 5000+ lines of Python replaced by Rust structs and match arms.
- **Extensibility**: Adding a new pool family (e.g., Maverick) means adding a `PoolEntry::Maverick` variant and implementing `calculate_tokens_out` + `encode_swap` for it. No new adapter, no new subscriber chain, no new builder registration.

## Risks

- **Migration scope**: 11 slices over multiple months. Must maintain backward compatibility at each slice boundary — Python tests are the continuity guarantee.
- **Tick bitmap porting complexity**: The V3 tick bitmap uses word/index bit manipulation with 256-bit words. The Python implementation has subtle edge cases around `MIN_TICK`/`MAX_TICK` clamping. Thorough testing required.
- **Curve variant explosion**: The `DyCalculator` protocol has 6+ enum variants with subtle numeric differences. Each must be ported individually and tested against Python reference outputs.
- **Balancer `PowVersion`**: Two invariant versions with different rounding behavior. Getting this wrong produces 1-wei systematic errors that only surface in production.
- **PyO3 handle overhead**: Every `pool.reserves_token0` read crosses PyO3 (one `u64` extraction = ~50ns). For Python-side consumers that read many attributes in a loop (e.g., logging, analytics), this may be slower than the current pure-Python attribute access. **Mitigation**: provide batch-read methods on the handle (e.g., `pool.snapshot() → V2PoolSnapshot` reads all state in one PyO3 call).
- **Construction I/O**: Moving `build_pool()` into Rust means Rust must do RPC calls, DB queries, and contract reads. The `AlloyProvider` already supports this, but the builder pattern (V2Builder, V3Builder with their `SyncPoolIO` + DB + decode logic) must be ported. This is mechanically straightforward but voluminous.

## Relationship to other plans

- **Plan 078**: This plan extends Plan 078's V2BlockEngine to own all state, not just the solver's `IntHopState` copies. Plan 078's `V2BlockEngine` is retained but refactored to read from `BotCore.pools` instead of its own HashMap.
- **Independent of other active plans**: No dependencies on plans for Balancer rate providers, Curve DyCalculator refactoring, etc. Those plans' Python-side changes will eventually be subsumed by the Rust math ports in Slices 9-10.

## Status

- [x] Slice 1: BotCore struct + V2 pool state owner
- [x] Slice 2: V2 constant-product math in Rust
- [x] Slice 3: V2 swap encoding in Rust
- [x] Slice 4: V2 reorg journal in Rust
- [ ] Slice 5: Solidly-stable math in Rust *(deferred)*
- [x] Slice 6: V3 pool state + Swap event decoder
- [x] Slice 7: V3 tick bitmap walk in Rust
- [x] Slice 8: V3BlockEngine
- [ ] Slice 9: Stableswap math in Rust *(deferred)*
- [ ] Slice 10: Balancer math in Rust *(deferred)*
- [x] Slice 11: PyV3ArbEngine + V3EnginePump (Python-accessible V3 engine)
- [x] Slice 12: V2-V3 mixed-path engine
- [ ] Slice 13: Python thin handles + backward compat (V2/V3 only)
