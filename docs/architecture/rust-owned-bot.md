# Rust-Owned Backrun Bot Architecture

> Covers the Rust extension, unified engine, pump, executor, and Python orchestration layer for Uniswap V2/V3/V4 same-block arbitrage on Ethereum mainnet.
>
> Developed under Plans 079–082 (all complete).

## 1. Guiding Principle

**Rust is the engine, Python is the cockpit.**

Every hot-path operation — event decoding, pool state mutation, tick-range construction, solver dispatch, result storage — lives in Rust. Python participates only in construction (pool discovery, engine registration), simulation (`eth_simulateV1`), and transaction submission. During the per-block loop, Python reads results from Rust and encodes swap payloads using Python pool objects; it does **not** receive events, update pool state, or push data to the engine.

```
┌─────────────────────────────────────────────────────────────────────┐
│                          Startup (Python)                           │
│  Bot.from_config_file → build_paths_async → register_pool/path     │
│  → backfill_snapshots → engine.freeze → engine.initial_solve       │
│  → engine.start(rpc_url)                                            │
└─────────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────────┐
│                      Hot Loop (Rust Pump)                           │
│  WS newHeads+logs → process_block → solve → store results              │
│  ↳ send BlockNotification via watch channel                        │
└─────────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────────┐
│                    Dispatch (Python)                                 │
│  wait_for_block → latest_results → encode_payloads                 │
│  → eth_simulateV1 → sign → send_raw_transaction                   │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 2. Component Map

| Component | Language | File(s) | Role |
|-----------|----------|---------|------|
| UniswapEngine | Rust | `rust/src/optimizers/uniswap_engine.rs` | Unified V2+V3+V4 engine: state, solving, result storage |
| V2BlockEngine | Rust | `rust/src/optimizers/v2_block_engine.rs` | V2 pool state, Sync decoding, constant-product solving |
| V3BlockEngine | Rust | `rust/src/optimizers/v3_block_engine.rs` | V3 pool state, Swap/Mint/Burn decoding, tick-range construction, piecewise solving |
| V4BlockEngine | Rust | `rust/src/optimizers/v4_block_engine.rs` | V4 pool state, Swap/ModifyLiquidity decoding, same CL math as V3 |
| UniswapEnginePump | Rust | `rust/src/optimizers/uniswap_engine_pump.rs` | Unified async pump: dual WS subscription (newHeads + logs), backfill on timeout/empty block, routes to sub-engines |
| BotCore | Rust | `rust/src/bot_core/mod.rs` | Single owner of pool/token state (future all-state owner, currently V2+V3 partial) |
| ReorgJournal | Rust | `rust/src/bot_core/state_history.rs` | Bounded deque of per-block deltas for rollback (V2: 2 reserves; V3: scalars + tick priors) |
| Tick bitmap walk & tick mutation | Rust | `rust/src/bot_core/tick_bitmap.rs` | `gen_ticks()` port + shared `update_tick_liquidity` / `apply_liquidity_to_tick_range` helpers used by both V3 and V4 engines |
| Event decoders | Rust | `rust/src/bot_core/v*_decoder.rs` | Decode Sync, Swap, Mint/Burn, ModifyLiquidity from Alloy logs |
| Möbius solvers | Rust | `rust/src/optimizers/mobius_*.rs` | Integer-exact arbitrage solvers (V2-V2, mixed V2-V3, V3-V3) |
| V2 swap encoding | Rust | `rust/src/bot_core/v2_encoding.rs` | Pre-encoded `swap()` calldata production |
| PyUniswapArbEngine | Rust/PyO3 | `uniswap_engine.rs` (bottom) | PyO3 wrapper exposing engine to Python |
| Executor contract | Vyper | `contracts/tstore_executor.vy` | Generic payload queue with V2/V3/V4 callbacks |
| Backrun bot | Python | `examples/eth_backrun_v2_v3_v4_rust.py` | Pool discovery, encoding, simulation, submission |

---

## 3. The UniswapEngine

### 3.1 Composition

`UniswapEngine` composes three sub-engines behind a single API:

```rust
pub struct UniswapEngine {
    v2_engine: V2BlockEngine,
    v3_engine: V3BlockEngine,
    v4_engine: V4BlockEngine,
    paths: HashMap<u64, (MixedPath, ResolvedMixedPath)>,
    pool_to_paths: HashMap<(HopType, u64), HashSet<u64>>,  // reverse index
    results: Vec<(u64, U256, U256)>,                        // (path_id, opt_input, profit)
    results_block: u64,
    running: bool,
}
```

`HopType` (`V2` | `V3` | `V4`) tags each pool reference in a path. `V3` and `V4` both produce `IntV3TickRangeSequence` — the solver cannot distinguish them and should not need to.

### 3.2 Registration

Pools are registered individually by type, then paths combine them:

| Method | Sub-engine | Key | Dual orientation? |
|--------|-----------|-----|--------------------|
| `register_v2_pool` | V2 | contract address → forward pool_id; reverse = forward + 1 | Yes |
| `register_v3_pool` | V3 | contract address → pool_key | No (zfo in path ref) |
| `register_v4_pool` | V4 | (pool_manager, pool_id) → forward pool_key; reverse = forward + 1 | Yes |

`register_path` takes a `Vec<MixedPoolRef>` (hop_type, pool_key, zero_for_one) and returns a path_id. After `freeze()`, no more registration is allowed.

### 3.3 Block Processing

`process_block(&[Log], block_number)` is the single entry point called by the pump each block:

1. **Categorise logs** by topic0: `V2_SYNC_TOPIC` → V2, `V3_SWAP/MINT/BURN_TOPIC` → V3, `V4_SWAP/MODIFY_LIQUIDITY_TOPIC` → V4
2. **Route** each log batch to the appropriate sub-engine's `process_block`
3. **Collect affected pool keys** from each sub-engine (dual-orientation for V2/V4)
4. **Rebuild and re-solve** only paths that reference affected pools (via `pool_to_paths` reverse index)
5. **Merge** new results with unchanged path results

### 3.4 Solver Dispatch

The engine matches on path composition to select the solver:

| Path type | Solver | Method |
|-----------|--------|--------|
| V2-V2 | `exact_mobius_solve` | Closed-form √(K·M) via U512 isqrt; ±2 neighborhood search |
| V3-V3, V4-V4 | `int_solve_v3_v3` | Piecewise integer-Möbius per (k₁,k₂) tick-range pair; closed-form per segment |
| V2-V3, V3-V2, V2-V4, V4-V2, V3-V4, V4-V3 | `exact_solve_mixed_v2_v3_sequence` | V3/V4 effective reserves + piecewise tick-range enumeration; closed-form per range |

**All paths use integer-exact arithmetic.** Zero f64 on any solve path. The former golden-section search and f64 Möbius solver are deleted.

---

## 4. Sub-Engines

### 4.1 V2BlockEngine

Owns V2 pool state as `IntHopState` pairs (forward + reverse). Each pool registration creates two entries with `pool_id` and `pool_id + 1`. Sync events update both entries. The constant-product calc uses U512 arithmetic matching EVM semantics:

```
amount_out = (gamma * reserve_out * amount_in) / (fee_denom * reserve_in + gamma * amount_in)
```

**Dual-orientation**: `apply_sync_updates` returns both forward and reverse pool keys so the unified engine can track V2 dependencies for `zfo=False` paths.

### 4.2 V3BlockEngine

Owns V3 pool state including `tick_data: HashMap<i32, TickInfo>` where each `TickInfo` holds `liquidity_gross` (u128) and `liquidity_net` (I256). Process block handles three event types:

| Event | Effect on pool state |
|-------|---------------------|
| Swap | Updates `sqrt_price_x96`, `liquidity`, `tick` (scalar fields) |
| Mint | Delegates to `apply_liquidity_to_tick_range` (lower: `net += Δ`, upper: `net -= Δ`, both: `gross += Δ`) |
| Burn | Same as Mint with negated `Δ`; ticks with zero `liquidity_gross` are removed (de-initialised) |

Tick update logic (`update_tick_liquidity` and `apply_liquidity_to_tick_range`) lives in `tick_bitmap.rs` — shared with V4BlockEngine. Tick-range sequences are built via `build_int_v3_sequence()` which:
1. Calls `compute_tick_ranges()` — the Rust port of `gen_ticks()` that walks `tick_bitmap: HashMap<i16, U256>` and interleaves boundary ticks with initialised ticks
2. For each range, constructs `IntV3TickRangeHop` with U256 `sqrt_price_*` and u128 `liquidity`
3. Computes integer effective reserves: `R₀ = L · 2⁹⁶ / √P`, `R₁ = L · √P / 2⁹⁶` (U512 intermediates)

### 4.3 V4BlockEngine

Mirrors V3BlockEngine exactly but identifies pools by `(pool_manager: Address, pool_id: [u8; 32])` instead of contract address. Two registration-time filters reject unusable pools:

- **Hook filtering**: `(hook_flags & AMOUNT_MODIFYING_HOOK_MASK) != 0` where `0xCC = BEFORE_SWAP | AFTER_SWAP | BEFORE_SWAP_RETURNS_DELTA | AFTER_SWAP_RETURNS_DELTA`. Hooked pools can modify swap amounts, violating the solver's V3-math assumption.
- **Dynamic fee exclusion**: `fee == 0x100000` indicates swap-dependent fees that the fixed-fee solver cannot handle.

V4 `ModifyLiquidity` events replace V3's separate Mint/Burn with a signed `liquidity_delta` (I256). Both V3 and V4 engines delegate tick mutations to the shared `update_tick_liquidity` (single tick) and `apply_liquidity_to_tick_range` (range with zero-gross cleanup) helpers in `tick_bitmap.rs`.

---

## 5. The Möbius Solvers

### 5.1 Mathematical Basis

Every constant-product swap `y = γ·s·x / (r + γ·x)` is a Möbius transformation fixing the origin. An n-hop path composes into:

```
l(x) = K·x / (M + N·x)
```

The optimal input is the closed-form `x_opt = (√(K·M) - M) / N`, solvable via integer square root on U512.

### 5.2 Integer-Exact Solver (`mobius_int_exact.rs`)

- Computes K, M, N as U512 integers via `compute_int_mobius_coefficients`
- Checks `K > M` for profitability
- Computes `x_opt = (isqrt(K·M) - M) / N` using `isqrt_u512` (Newton's method in U512, ~100ns)
- EVM-simulates at `x_opt` and ±2 neighbors to handle floor-division rounding
- Returns best result with `used_closed_form: bool`

**Performance**: 131ns for a 2-hop V2 path (17× faster than the iterative `int_mobius_solve_with_refinement`).

### 5.3 V3/V4 Integer Solver (`mobius_v3_int.rs`)

Concentrated-liquidity hops produce `IntV3TickRangeSequence` — a vector of `IntV3TickRangeHop` structs, each carrying integer sqrt-price bounds and liquidity. The solver:

1. For each (k₁, k₂) pair of ending tick ranges, computes a single (K, M, N) triple by composing the shifted Möbius transforms
2. Calls `exact_mobius_solve` for closed-form optimal input — **no golden-section search**
3. Validates via `int_simulate_v3_v3_path` with full piecewise simulation

Crossing constants (additive amounts from tick-range boundaries) fold into the Möbius coefficient recurrence, so each (k₁, k₂) segment is still a single closed-form solve.

### 5.4 Mixed V2-V3/V4 Solver (`exact_solve_mixed_v2_v3_sequence`)

Combines V3/V4 effective reserves with V2 integer reserves:

1. Enumerate V3/V4 ending ranges (like `int_solve_v3_v3`)
2. For each candidate ending range k, compute crossing data
3. Build mixed hop list with the ending range's effective reserves
4. Solve via closed-form Möbius
5. Validate with crossing-aware simulation

Replaced the single-range `exact_solve_mixed_v2_v3` which produced false positives when swaps exceeded the current range capacity.

---

## 6. The Pump: Rust-Owned State Pipeline

### 6.1 Architecture: Dual-Subscription with Backfill Safety Net

The `UniswapEnginePump` maintains two concurrent WS subscriptions against the same provider:

1. **`newHeads`** — block boundary notifications (block number, timestamp, base fee, gas)
2. **`logs`** — all log events, unfiltered (topic + address filtering happens in Rust)

The pump assumes the WS/IPC connection is live and delivering events correctly. Logs arrive in real-time and are buffered per block. When a `newHeads` event arrives, the buffered logs for the just-completed block are processed atomically.

```
AlloyProvider (WS/IPC)
    │
    ├─ subscribe("newHeads") ──────────────────────┐
    │                                               │
    ├─ subscribe("logs") ──┐                       │
    │   (no filter)         │                       │
    │                       ▼                       ▼
    │               ┌─────────────────────────────────────┐
    │               │  Buffer logs by block_number        │
    │               │  (log.block_number from WS context  │
    │               │   or matched against pending block)  │
    │               └──────────┬──────────────────────────┘
    │                          │
    └─ on newHeads(block N):  │
         │                    │
         ├─ Take all buffered logs for block N-1
         │   (logs that arrived since the previous newHeads)
         │
         ├─ Filter logs in Rust:
         │   topic0 ∈ {V2_SYNC, V3_SWAP/MINT/BURN, V4_SWAP/MODIFY_LIQ}
         │   address ∈ {registered V2+V3 pools, V4 PoolManagers}
         │
         ├─ engine.lock().process_block(filtered_logs, block_number)
         │   → route to V2/V3/V4 sub-engines
         │   → rebuild affected sequences
         │   → solve affected paths
         │   → store results
         │
         └─ block_tx.send(BlockNotification) — Python reads this
```

### 6.2 Why No Filter on the Logs Subscription

The `logs` subscription carries no address or topic filter. All filtering happens in Rust after receipt. This avoids three classes of provider-specific failure:

1. **Address filter limits** — some providers reject subscriptions with >1000 address constraints, or silently ignore them
2. **Topic filter truncation** — some providers handle 6-topic OR filters incorrectly
3. **Provider-dependent filter semantics** — different providers interpret `address` and `topic` constraints differently (AND vs OR, ordering requirements)

The traffic cost is bounded: Ethereum mainnet produces ~200–500 logs per block, of which typically 5–50 are relevant to our monitored pools. The Rust-side topic+address filter is O(1) per log (hash comparison + HashMap lookup).

### 6.3 Backfill Triggers

The pump assumes the WS connection is live, but two conditions trigger a verification backfill via `eth_getLogs`:

#### Trigger 1: Timeout — 60s with nothing received

If neither a `newHeads` nor a `logs` event arrives within 60 seconds, the connection is likely dead. The pump:

1. Logs a warning with the last-seen block
2. Calls `eth_getLogs` from `last_processed_block + 1` to the latest block (determined via `eth_blockNumber`)
3. Processes any logs found, filling the gap
4. Resets the timeout watchdog

This catches: WS disconnects, provider restarts, network partitions, and local socket errors that don't immediately surface as `Err` in the stream.

#### Trigger 2: Block with no received logs

When a `newHeads` event arrives, the pump checks whether any logs were received since the previous `newHeads`. If zero logs arrived for the just-completed block, this could mean:

- The block genuinely has no relevant events (common — many blocks have zero pool events)
- The WS dropped some or all log events for this block

The pump cannot distinguish these cases from the subscription alone. It calls `eth_getLogs(from=block, to=block)` to verify:

- **Empty result** → block truly had no events. No harm done — one extra RPC call, no state change.
- **Non-empty result** → logs were missed. Process them. The engine now has correct state.

This provides **protection against false positives**: if the WS silently dropped events, the backfill catches them before the engine solves on stale state. If the block was truly empty, the backfill is a no-op with a small RPC cost.

#### Why this is better than `eth_getLogs` every block

| | `eth_getLogs` every block | Push + backfill on suspicion |
|---|---|---|
| Latency | Block header + ~50–100ms RPC | Block header only (logs already buffered) |
| RPC calls per block | Always 1 | 0 for blocks with events, 1 for empty blocks |
| Data freshness | Stale by one RPC round-trip | Real-time (logs arrive as they're mined) |
| Safety | Guaranteed complete (by construction) | Guaranteed complete (backfill covers gaps) |
| Empty-block cost | Same as any block | One `eth_getLogs` call (cheap — empty response) |

The key advantage: **events are processed as they arrive**, not after a round-trip delay. For same-block reactivity (where another searcher might see the same event and act first), this latency reduction is material.

### 6.4 Why One Pump, Not Three

- 2 WS subscriptions instead of 6 (3 `newHeads` + 3 `logs`)
- 1 lock acquisition per block instead of 3
- Single `eth_getLogs` backfill call covers all protocols

### 6.5 BlockNotification

```rust
pub struct BlockNotification {
    pub block_number: u64,
    pub timestamp: u64,
    pub base_fee_per_gas: Option<u64>,
    pub gas_used: u64,
    pub gas_limit: u64,
}
```

Published via `tokio::sync::watch` channel. Python reads it via `engine.wait_for_block()` (blocking call, releases GIL). This replaces Python's WS `newHeads` subscription for fee/nonce computation — the pump is the sole source of block data.

### 6.6 Startup Sequence

Python's `main()` follows this exact order:

1. `Bot.from_config_file()` — load config, DB, connections
2. `build_paths_async` — discover V2/V3/V4 pools, build Python pool objects, register with engine
3. `engine.freeze()` — lock registration
4. `engine.initial_solve()` — solve all paths from current state
5. `backfill_snapshots()` — V3: fetch Mint/Burn events from snapshot block to current; V4: fetch ModifyLiquidity events. Push updated tick_data to Rust engine one final time.
6. `engine.start(node_ws)` — spawn the Rust pump
7. **Main loop**: `wait_for_block → latest_results → dispatch_profitable_results`

The backfill closes the gap between the DB snapshot and the first pump block. After backfill, Rust owns all state updates — Python never pushes pool state again.

### 6.7 Consumer Subscription Model

The pump's `watch` channel provides a continuous stream of `BlockNotification`s to Python. Because backfill covers gaps, consumers see an **unbroken, block-ordered sequence** of notifications — even if the WS connection dropped and recovered during a short window. The engine's `latest_results()` always reflects the most recent processed state, regardless of whether it came from the WS subscription or a backfill call.

For future consumers that want per-event granularity (not just per-block), the pump can expose a second channel carrying `DecodedEvent` objects — block-ordered, deduplicated (backfill results merged with WS-received events), filtered to relevant pools only.

---

## 7. The Reorg Journal

### 7.1 Design

Each pool in `BotCore` carries a `ReorgJournal<D: BlockDelta>` — a bounded `VecDeque` of per-block deltas storing **prior** values of modified state fields.

**Forward progress**: stash "before" values → update current state.

**Reorg rollback**: pop deltas at/after the target block → restore "before" values into current state.

**Hot path**: swap calculations and the engine never touch the journal. They always read current mutable fields. Zero penalty.

### 7.2 V2 Delta (Degenerate Case)

```rust
struct V2BlockDelta {
    block: u64,
    reserve0_before: U256,
    reserve1_before: U256,
}
```

V2 "delta = full state" — two reserves. Memory equivalent to full-state cloning, but restores in O(1) instead of O(n) for V3.

### 7.3 V3 Delta (Efficient Case)

```rust
struct V3BlockDelta {
    block: u64,
    sqrt_price_x96_before: U256,
    liquidity_before: U128,
    tick_before: i32,
    tick_priors: Vec<(i32, TickBefore)>,  // typically 0–4 entries
}
```

Only **modified** tick priors are stored. A typical V3 swap modifies 0–4 ticks (at the crossing boundary). Memory: `2000 entries + 8×4 ≈ 2032` vs full-tick-map cloning at `8 × 2000 + scalars ≈ 16000`.

### 7.4 Operations

| Method | Effect |
|--------|--------|
| `push_delta` | Append same-block replacement, reject older blocks, evict beyond max_depth |
| `discard_before_block` | Remove deltas older than the target (no longer rollback-reachable) |
| `restore_before_block` | Pop deltas at/after target, restore scalar + tick priors, remove de-initialised ticks |

Property-based tests (via proptest) verify the journal matches a faithful model after arbitrary sequences of push/discard/restore operations.

---

## 8. Event Decoders

All decoders live in `rust/src/bot_core/` and decode from Alloy `Log` objects:

| Decoder | File | Event signature | Topic constant |
|---------|------|-----------------|---------------|
| V2 Sync | `v2_sync_decoder.rs` (in `optimizers/`) | `Sync(uint112,uint112)` | `0x1c411e9a...` |
| V3 Swap | `v3_swap_decoder.rs` | `Swap(address,address,int256,int256,uint160,uint128,int24)` | `0xc42079f9...` |
| V3 Mint | `v3_mint_burn_decoder.rs` | `Mint(address,address,int24,int24,uint128,uint256,uint256)` | `0x7a53080b...` |
| V3 Burn | `v3_mint_burn_decoder.rs` | `Burn(address,int24,int24,uint128,uint256,uint256)` | `0x0c396cd9...` |
| V4 Swap | `v4_swap_decoder.rs` | `Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)` | `0x40e9cecb...` |
| V4 ModifyLiquidity | `v4_modify_liquidity_decoder.rs` | `ModifyLiquidity(bytes32,address,int24,int24,int256,bytes32)` | `0xf208f491...` |

Key differences from Python:
- **~50ns** decode time per event vs ~5-10µs via Python `eth_abi`
- Zero-allocation for common paths; returns `Option<Event>` (None for wrong topic/malformed data)
- V4 pools identified by `(PoolManager address, pool_id)` not contract address

---

## 9. The Executor Contract

### 9.1 Overview

A single Vyper contract (`contracts/tstore_executor.vy`) handles all V2+V3+V4 arbitrage paths. It uses a generic payload queue stored in **transient storage** (EIP-1153, cleared every transaction):

```vyper
struct Payload:
    target: address
    calldata: Bytes[MAX_PAYLOAD_BYTES]
    will_callback: bool
```

### 9.2 Payload Delivery

`execute_payloads(payloads, bribe_bips)` stores the queue in transient storage, then iterates:

1. Read `t_payloads[index]`
2. If `will_callback=True`, register target in `t_allowed_callback_addresses`
3. `raw_call(target, calldata)` —_no return value check
4. Advance queue index
5. Repeat until `t_all_payloads_delivered`

Callbacks (`uniswapV2Call`, `uniswapV3SwapCallback`, `unlockCallback`) assert `msg.sender` is registered, then resume queue delivery from where the payload left off.

### 9.3 Callback Types

| Callback | Protocol | Registration trigger |
|----------|----------|---------------------|
| `uniswapV2Call` | Uniswap/SushiSwap V2 | `will_callback=True` on V2 swap payload |
| `hook` | Aerodrome/Velodrome V2 | Same |
| `pancakeCall` | PancakeSwap V2 | Same |
| `uniswapV3SwapCallback` | Uniswap/SushiSwap V3 | `will_callback=True` on V3 swap payload |
| `pancakeV3SwapCallback` | PancakeSwap V3 | Same |
| `unlockCallback` | Uniswap V4 | `will_callback=True` on `PoolManager.unlock` payload |

### 9.4 V3 Auto-Pay

After delivering all queued payloads inside a V3 callback, the contract checks whether the calling pool is owed WETH and auto-transfers it:

```vyper
if amount1_delta > 0:
    if token1() == WETH_ADDR:
        WETH.transfer(msg.sender, amount1_delta)
elif amount0_delta > 0:
    if token0() == WETH_ADDR:
        WETH.transfer(msg.sender, amount0_delta)
```

The Python encoder must **not** include explicit WETH transfer payloads for pools where auto-pay fires (would cause double-payment and revert).

### 9.5 V4 Settlement

V4 swaps happen inside `unlockCallback`. The Python encoder pre-computes all amounts and encodes settlement operations as raw calldata payloads:

- `PoolManager.swap(PoolKey, SwapParams, hookData)` — the V4 swap
- `ERC20.transfer(PoolManager, amount)` — pay debt to PM
- `PoolManager.sync(currency)` — update PM's internal balance tracking
- `PoolManager.settle()` — credit tokens to our delta
- `PoolManager.take(currency, to, amount)` — receive tokens from PM

### 9.6 Profit Measurement

`execute_payloads` asserts `WETH.balanceOf(self)` does not decrease over the transaction. No prefunding required — V3's callback is the flash borrow mechanism.

---

## 10. Swap Encoding

### 10.1 Path Types and Their Payload Sequences

The bot supports all 9 two-hop path combinations across V2/V3/V4. Each has a dedicated encoder function:

| Path type | Encoder | Entry point | Settlement |
|-----------|---------|-------------|------------|
| V3-V2 | `encode_v3v2_payloads` | V3.swap (callback) | Token transfers |
| V3-V3 | `encode_v3v3_payloads` | V3.swap (callback) | Token transfers (nested callbacks) |
| V2-V2 | `encode_v2v2_payloads` | V2.swap (flash borrow) | Token transfers |
| V2-V3 | `encode_v2v3_payloads` | V2.swap (flash borrow) | Token transfers (nested callbacks) |
| V4-V4 | `encode_v4v4_payloads` | PM.unlock (callback) | sync/settle/take |
| V4-V3 | `encode_v4v3_payloads` | PM.unlock (callback) | sync/settle/take + token transfers |
| V3-V4 | `encode_v3v4_payloads` | V3.swap (callback) → PM.unlock (nested) | sync/settle/take |
| V4-V2 | `encode_v4v2_payloads` | PM.unlock (callback) | sync/settle/take + token transfers |
| V2-V4 | `encode_v2v4_payloads` | V2.swap (callback) → PM.unlock (nested) | sync/settle/take |

### 10.2 V3 vs V4 Sign Convention

V3 and V4 use **opposite** sign conventions for `amountSpecified`:

| Mode | V3 | V4 |
|------|----|----|
| Exact INPUT | `amountSpecified > 0` | `amountSpecified < 0` |
| Exact OUTPUT | `amountSpecified < 0` | `amountSpecified > 0` |

For arbitrage (always exact-input): V3 encoding uses **positive** values, V4 encoding uses **negative** values. Getting this wrong produces V3 "IIA" (Insufficient Input Amount) reverts.

### 10.3 NATIVE_ADDRESS Handling

V4 PoolKey uses `currency0 = address(0)` for ETH pools. The executor wraps received ETH to WETH via `IWETH.deposit()` during settlement. For direction resolution, the bot treats `address(0)` as equivalent to WETH.

---

## 11. Dispatch Pipeline

### 11.1 Flow

```
latest_results() → sort by profit desc → parallel simulation
    → staleness check → encode → simulate (3-call pattern)
    → market-aware fee → mutual exclusivity → submit
```

### 11.2 Slices (from Plan 080)

| # | Feature | Detail |
|---|---------|--------|
| 0.5 | Dispatch serialisation | `asyncio.Lock` prevents concurrent dispatches |
| 1 | Parallel simulation | `asyncio.gather` with `MAX_SIMULATE_CONCURRENT=50` |
| 2 | Staleness tracking | `solve_block` from Rust engine (not Python `update_block`); `STALENESS_TOLERANCE=5` blocks |
| 3 | Market-aware fees | Target from profit ratio, age decay, feeHistory percentile bounds |
| 4 | Best-path selection | Sort by profit desc, mutual exclusivity via `committed_pools` |
| 5 | Gas from simulation | `gasUsed * 1.1` instead of `1.5×` heuristic |
| 6 | WS reconnection | Exponential backoff + pool state reconciliation (handled by Rust pump's Alloy provider) |
| 7 | Subscription filtering | Address filter on WS `LogsSubscription` when ≤1000 pools |

### 11.3 Simulation: 3-Call Pattern

```python
eth_simulateV1({
    "blockStateCalls": [{
        "calls": [
            WETH.balanceOf(executor),           # [0] Before
            execute_payloads(payloads, 0),        # [1] Arbitrage
            WETH.balanceOf(executor),           # [2] After
        ],
        "stateOverrides": {
            executor_owner: {balance: 100 ETH},   # Gas funding
            injected_address: {code: runtime},     # Code injection (if enabled)
        }
    }]
})
```

Gross profit = WETH balance after − WETH balance before.

### 11.4 Code Injection

When `INJECT_EXECUTOR_CODE=True`, the bot loads the executor's runtime bytecode from `contracts/tstore_executor_runtime_bytecode.txt` and injects it at a fresh address via `stateOverrides.code`. This enables simulation of undeployed contracts without mainnet deployment. The Vyper immutables (`OWNER_ADDR`, `WETH_ADDR`) are embedded in the code section — no storage slot overrides needed.

---

## 12. Path Discovery

### 12.1 Token Pair Model

All paths are two-hop WETH-centred cycles: `WETH ↔ intermediate ↔ WETH`. A path is profitable when pool A sells WETH for the intermediate at a better rate than pool B buys WETH with the intermediate (or vice versa).

### 12.2 Pool Sources

| DEX | Pool table | Factory |
|-----|-----------|---------|
| Uniswap V2 | `UniswapV2PoolTable` | `0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f` |
| Uniswap V3 | `UniswapV3PoolTable` | `0x1F98431c8aD98523631AE4a59f267346ea31F984` |
| Sushiswap V2 | `SushiswapV2PoolTable` | `0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac` |
| Sushiswap V3 | `SushiswapV3PoolTable` | `0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F` |
| PancakeSwap V3 | `PancakeswapV3PoolTable` | `0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865` |
| Uniswap V4 | `UniswapV4PoolTable` | PoolManager `0x000000000004444c5dc75cB358380D2e3De08A90` |

V4 pools are identified by `(PoolManager, pool_id)` — discovered via `find_paths_async` with `step.hash` carrying the pool_id.

### 12.3 Token Quality Filtering

| Mode | Environment variable | Behaviour |
|------|---------------------|-----------|
| Blacklist (default) | `TOKEN_BLACKLIST_MODE=1` | Skip paths with known scam/tax tokens |
| Whitelist | `TOKEN_WHITELIST_MODE=1` | Only allow paths with known-good intermediates (USDC, USDT, DAI, WBTC, etc.) |

Eliminates ~95%+ of simulation failures from scam/tax/honeypot tokens.

---

## 13. BotCore: The Future State Owner

### 13.1 Current State

`BotCore` (in `rust/src/bot_core/mod.rs`) is a partial implementation of the full Rust-owned state vision from Plan 079. Currently it owns:

- `pools: HashMap<u64, PoolEntry>` — V2 and V3 pool state
- `pool_addresses: HashMap<Address, u64>` — address → pool_id lookup
- `tokens: HashMap<Address, TokenEntry>` — ERC20 metadata
- Reorg journal integration for V2 and V3
- `calculate_tokens_out/in` for V2 (V3 returns 0 — not yet implemented)
- `encode_swap` for V2 (V3 encoding not yet in BotCore)

### 13.2 Intended Architecture

Plan 079 envisions `BotCore` as the **single owner** of all runtime state. Python objects become thin `PyO3` handles:

```python
class Pool:
    _core: PyBotCore   # Arc<BotCore>
    _pool_id: u64       # Key into BotCore.pools

    @property
    def reserves_token0(self) -> int:
        # PyO3 → BotCore.pools[pool_id].reserves_token0
```

### 13.3 Deferred Items

The following Plan 079 slices are deferred (not blocking the current bot):

| Slice | Description | Status |
|-------|-------------|--------|
| 5 | Solidly-stable math in Rust | Deferred |
| 9 | Stableswap math in Rust | Deferred |
| 10 | Balancer math in Rust | Deferred |
| 13 | Python thin handles + backward compat | Deferred |

These are not needed for the current V2/V3/V4 backrun bot because the engines own their own state independently of `BotCore`.

---

## 14. GIL Discipline

| Function type | GIL policy | Rationale |
|---------------|-----------|-----------|
| Tick math (`get_sqrt_ratio_at_tick`, `get_tick_at_sqrt_ratio`) | **Hold** | ~20ns compute; GIL release/reacquire costs ~200ns |
| Address utils (`to_checksum_address`) | **Hold** | ~50ns compute |
| Provider I/O (`raw_call`, async operations) | **Release** via `py.detach()` | I/O-bound; holding the GIL blocks all Python threads |
| Pump main loop | **Never acquires** | Runs on Tokio worker threads; communicates via `parking_lot::Mutex` and `watch` channel |
| `drain_buffer()` | **Attach** in wrapper | Wraps `drain_raw()` which is pure Rust; `Python::attach()` only at the boundary |

Every `Python::attach()` call site has a `// SAFETY:` comment documenting the no-circular-wait guarantee.

---

## 15. Relationship Between Components

```
                    ┌────────────────────┐
                    │    Python main()   │
                    └──────┬─────────────┘
                           │
              ┌────────────▼──────────────┐
              │     PyUniswapArbEngine    │
              │  (PyO3 wrapper, GIL gate) │
              └────────────┬──────────────┘
                           │ Arc<Mutex<...>>
              ┌────────────▼──────────────┐
              │     UniswapEngine         │
              │  ┌──────┐┌──────┐┌──────┐ │
              │  │V2 Eng││V3 Eng││V4 Eng│ │
              │  └──────┘└──────┘└──────┘ │
              │  paths  pool_to_paths     │
              │  results results_block    │
              └────────────┬──────────────┘
                           │ Arc<Mutex<...>>
              ┌────────────▼──────────────┐
              │   UniswapEnginePump       │
              │  (Tokio async task)       │
              │  WS newHeads + logs →     │
              │  process_block → solve    │
              │  → BlockNotification      │
              │  backfill on gap/timeout  │
              └──────────────────────────┘

  ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─

              ┌──────────────────────────┐
              │      BotCore (partial)   │
              │  pools: HashMap<u64,     │
              │    PoolEntry>            │
              │  ReorgJournal per pool   │
              │  calculate_tokens_out/in │
              │  encode_swap (V2 only)  │
              └──────────────────────────┘

  ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─

              ┌──────────────────────────┐
              │   Möbius Solvers        │
              │  exact_mobius_solve     │ ← V2-V2, closed-form √(K·M)
              │  int_solve_v3_v3        │ ← V3-V3/V4-V4, piecewise integer-Möbius
              │  exact_solve_mixed_     │ ← V2-V3/V2-V4/V3-V4, sequence + crossing
              │    v2_v3_sequence       │
              └──────────────────────────┘

  ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─

              ┌──────────────────────────┐
              │  tstore_executor.vy      │
              │  Generic payload queue   │
              │  V2/V3/V4 callbacks      │
              │  V3 auto-pay WETH       │
              │  V4 unlockCallback      │
              └──────────────────────────┘
```

---

## 16. Test Coverage

### 16.1 Rust Tests (409 tests)

| Module | Test count | Coverage |
|--------|-----------|----------|
| `mobius_int_exact` | 25+ | isqrt correctness, exact vs f64, never-panics |
| `mobius_v3_int` | 14+ | Effective reserves, mixed sequence solver, crossing computation |
| `v2_block_engine` | 15+ | Registration, dual-orientation, solve, dependency tracking |
| `v3_block_engine` | 10+ | Registration, swap + mint/burn updates, tick-range construction |
| `v4_block_engine` | 10+ | Registration, hook filtering, dynamic-fee exclusion, swap + modify_liquidity |
| `uniswap_engine` | 20+ | Mixed path resolution, mixed V2-V3/V3-V2, V4 integration, freeze |
| `uniswap_engine_pump` | 3+ | Filter construction, shutdown flag |
| `state_history` | 15+ unit + property tests | Push/discard/restore, proptest model equivalence |
| `tick_bitmap` | 10+ | gen_ticks edge cases, boundary ticks, MIN_TICK/MAX_TICK |
| Decoders | 7+ each | Valid/wrong-topic decode, field extraction |

### 16.2 Python Tests (3223+ tests)

- `tests/arbitrage/test_optimizers/test_uniswap_arb_engine.py` — 9 integration tests for mixed V2-V3 engine
- `tests/arbitrage/test_optimizers/test_engine_v3v3_vs_brent.py` — 13 tests comparing integer-exact V3-V3 against Brent (float) and brute-force (integer gold standard)
- Full library test suite (3000+ tests) covers pool construction, swap calculations, state management

### 16.3 Validation Method

Each math port follows the pattern:
1. Generate test vectors from the Python reference
2. Write Rust tests asserting identical outputs for identical inputs
3. Include edge cases: zero amounts, max uint256, fee boundaries, single-coin pools
4. Engine integration tests verify end-to-end: register → update → solve → results match Python

---

## 17. Known Limitations

| Limitation | Impact | Mitigation |
|------------|--------|------------|
| V2 asymmetric fees: register_v2_pool only uses `_fee_token0` | Wrong fee for one direction if asymmetric | Runtime warning on detection; full fix requires Rust engine API change |
| Python pool objects used for encoding | Encoding calls `calculate_tokens_out_from_tokens_in()` on Python pools — may use stale state if Rust owns updates | Encoding uses amounts from the same block (before dispatch); long-term fix is Rust-owned encoding |
| `BotCore` is partial | V3 calculation/encoding not in BotCore; Curve/Balancer math not ported | Engines own their own state; BotCore is a future consolidation point |
| V4 encoding not validated on anvil fork | ABI selectors and amountSpecified verified in unit tests, not against real node | Dry-run mode validates via `eth_simulateV1` before live submission |
| No three-hop paths | Path discovery limited to `max_depth=2` | Would require solver extension (3-hop Möbius composition) and more complex encoding |
| WS stability depends on Alloy | Rust pump uses Alloy's WS provider for dual subscriptions | Timeout backfill (60s) covers dead connections; empty-block backfill covers silent event drops; Alloy auto-reconnects |
