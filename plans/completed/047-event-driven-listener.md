# Plan 047: Event-Driven Log Listener

## Summary

Replace the `SubscriptionManager` with a simpler two-layer architecture: `Subscription` (Rust-backed double-buffer async iterator) + `LogListener` (pure Python dispatch registry). Wire pool types to the listener via `LOG_HANDLERS` class variables containing event decoders that produce update-applying closures.

## Motivation

Plan 046 built `SubscriptionManager` as a callback orchestration layer. But for MEV searchers using unfiltered log subscriptions, it adds complexity without value — there's only one subscription, no cross-subscription ordering needed, and the startup pattern (buffer → build → replay → live) requires explicit control that the manager hides.

The event-driven listener architecture is:
1. **Simpler** — two primitives instead of three (Subscription + LogListener, no SubscriptionManager)
2. **Performant** — Rust double-buffer with GIL-free accumulation, one bulk conversion on drain
3. **Explicit** — user owns the startup pattern, drain/replay timing, and live loop
4. **Extensible** — pools declare `LOG_HANDLERS`, user wires them to the listener after `build_pool()`

## Architecture

### Subscription: Double-Buffer Drain

Replaces the current channel-based `__anext__` with a double-buffer swap pattern:

```
Rust pump task → writes to active buffer (Mutex<Vec<RawLog>>)
                → sets AtomicBool signal (false→true only)
                → sends wake-up via mpsc channel

Python drain() → clears signal flag
              → atomically swaps active buffer (fetch_xor)
              → takes stale buffer contents (mem::take)
              → acquires GIL once for bulk conversion
              → returns list[dict]
```

**Lifecycle:** `start → running → close` (no mode transitions, the double-buffer is always active)

**`__anext__`** uses `drain()` internally — when local batch is empty, wait for signal, drain, pop first item from local list.

**`started()`** — awaitable that resolves when the WS subscription is confirmed by the node. Raises on failure.

### LogListener: Dispatch Registry

```python
class LogListener:
    def __init__(self): ...
    def register(self, address: str, topic0: str, handler: Callable[[dict], None]) -> None: ...
    def unregister(self, address: str, topic0: str, handler: Callable[[dict], None]) -> None: ...
    def dispatch(self, log: dict) -> None: ...
```

- Exact `(address, topic0)` match only — no wildcards
- Sequential handler execution in `dispatch()`
- Exceptions propagate to caller (fail loudly)
- Returns `None`
- ~200ns per miss (dict lookup + empty list), ~160μs/block for 800 discarded events

### Pool LOG_HANDLERS

Each pool type declares its event decoders as a class variable:

```python
class UniswapV2Pool:
    LOG_HANDLERS: ClassVar[dict[str, Callable[[dict], Callable[[Self], None]]]] = {
        SYNC_TOPIC: decode_v2_sync,
    }
```

Decoder signature: `Callable[[dict], Callable[[PoolType], None]]` — takes a raw log dict, returns a closure that applies the decoded update to a pool instance.

Pool types and their handlers:

| Pool Type | Events | Handler returns closure calling |
|-----------|--------|-------------------------------|
| UniswapV2Pool | Sync | `external_update()` |
| UniswapV3Pool | Swap, Mint, Burn | `external_update()` or `update_liquidity_map()` |
| UniswapV4Pool | Swap, ModifyLiquidity | `external_update()` or `update_liquidity_map()` |
| AerodromeV2Pool | Sync | `external_update()` |
| CamelotLiquidityPool | Sync | `external_update()` |
| CurveStableswapPool | (none) | `{}` — stays on polling |

### Bot Integration

```python
class Bot:
    _async_adapters: dict[ChainId, AsyncProviderAdapter]

    async def start_listening(
        self, chain_id: ChainId | None = None
    ) -> tuple[Subscription, Subscription]:
        """Create newHeads + unfiltered logs subscriptions for a chain.
        Returns (heads_subscription, logs_subscription).
        Raises SubscriptionNotSupported if no WS URI configured.
        Raises on WS connection failure.
        """
```

- Creates `AsyncProviderAdapter` from configured WS URI
- Stores in `_async_adapters[chain_id]`
- Returns `(heads_sub, logs_sub)` — user owns them
- No `SubscriptionManager` — deleted
- No `subscription_manager` property on `AsyncProviderAdapter` — removed
- User wires pools to `LogListener` manually after `build_pool()`

### User Wiring Pattern

```python
# After building pools
listener = LogListener()
for pool in pools:
    for topic, decoder in pool.LOG_HANDLERS.items():

        def make_handler(d=decoder, p=pool):
            def handler(log):
                d(log)(p)

            return handler

        listener.register(pool.address, topic, make_handler())
```

### Startup Pattern (Gapless)

```python
bot = Bot.from_config_file()
heads_sub, logs_sub = await bot.start_listening(chain_id=1)

# Wait for WS subscription confirmation (raises on failure)
await heads_sub.started()
await logs_sub.started()

# Drain newHeads to find starting block
buffered_heads = heads_sub.drain()
b_start = int(buffered_heads[0]["number"], 16)

# Build pools at b_start - 1
pools = build_my_pools(bot, block=b_start - 1)

# Create listener, register handlers
listener = LogListener()
for pool in pools:
    for topic, decoder in pool.LOG_HANDLERS.items():

        def make_handler(d=decoder, p=pool):
            def handler(log):
                d(log)(p)

            return handler

        listener.register(pool.address, topic, make_handler())

# Replay buffered logs
buffered_logs = logs_sub.drain()
for log in buffered_logs:
    listener.dispatch(log)

# Live loop
async for log in logs_sub:
    listener.dispatch(log)
```

### Update Sources Coexisting

Three update paths, all converge on `pool.external_update()`:

1. **Polling** — `bot.update(pool)` via builder (unchanged, RPC call)
2. **Manual** — `pool.external_update(update)` directly (unchanged)
3. **Log-driven** — `listener.dispatch(log)` → handler → decode → `external_update()`

User chooses which source(s) to use. No mutual exclusion enforced.

## Changes

### Rust (`rust/src/subscription.rs`)

- Replace single `mpsc::Receiver` channel with double-buffer:
  - `buffers: [Mutex<Vec<RawLog>>; 2]`
  - `active: AtomicUsize`
  - `signaled: AtomicBool`
- Pump task: write to active buffer, set signal flag (false→true only), send wake-up
- `drain()`: clear signal, swap active buffer (`fetch_xor(1)`), take stale buffer, bulk convert with GIL
- `__anext__()`: when local batch empty, wait for signal, call `drain()`, pop from local list
- Remove accumulate/dispatch mode transitions (no state machine — double-buffer is always active)
- `started()`: async method that resolves when WS subscription is confirmed

### Python (`src/degenbot/provider/subscription.py`)

- Update `Subscription` class to use `drain()` method delegating to Rust
- Add `drain() -> list[dict]` public method
- `__anext__` uses drain internally with local batch
- Remove mode-transition methods

### Python (`src/degenbot/provider/subscription_manager.py`)

- **Delete entire file**

### Python (`src/degenbot/provider/subscription_types.py`)

- Remove `SubscriptionConfig`, `BlocksSubscriptionConfig`, `FullBlocksSubscriptionConfig`, `PendingTransactionsSubscriptionConfig`, `FullPendingTransactionsSubscriptionConfig`, `LogsSubscriptionConfig`
- These were only used by SubscriptionManager

### Python (`src/degenbot/provider/interface.py`)

- Remove `subscription_manager` property from `AsyncProviderAdapter`
- Remove `SubscriptionManager` import
- Keep `subscribe_blocks()`, `subscribe_logs()`, etc. as public methods

### Python (`src/degenbot/listener/`)

New module:

- `__init__.py` — exports `LogListener`
- `log_listener.py` — `LogListener` class with `register()`, `unregister()`, `dispatch()`

### Python pool types — add `LOG_HANDLERS`

- `src/degenbot/uniswap/v2_liquidity_pool.py` — `LOG_HANDLERS = {SYNC_TOPIC: decode_v2_sync}`
- `src/degenbot/uniswap/v3_liquidity_pool.py` — `LOG_HANDLERS = {SWAP_TOPIC: decode_v3_swap, MINT_TOPIC: decode_v3_mint, BURN_TOPIC: decode_v3_burn}`
- `src/degenbot/uniswap/v4_liquidity_pool.py` — `LOG_HANDLERS = {SWAP_TOPIC: decode_v4_swap, MODIFY_LIQUIDITY_TOPIC: decode_v4_modify_liquidity}`
- `src/degenbot/aerodrome/pools.py` — `LOG_HANDLERS = {SYNC_TOPIC: decode_aerodrome_sync}`
- `src/degenbot/camelot/pools.py` — `LOG_HANDLERS = {SYNC_TOPIC: decode_camelot_sync}`
- New decoder modules in each pool package (e.g., `src/degenbot/uniswap/v2_log_decoders.py`)

### Python (`src/degenbot/bot.py`)

- Add `_async_adapters: dict[ChainId, AsyncProviderAdapter]`
- Add `async def start_listening(chain_id) -> tuple[Subscription, Subscription]`
- Add `ws_uri` to config (or accept in constructor)

### Config

- Add `ws_uri: str | None` field to DegenbotConfig

### Tests

- Remove 37 SubscriptionManager tests
- Update existing Subscription tests for double-buffer drain
- Add LogListener unit tests (register, unregister, dispatch, miss path)
- Add LOG_HANDLER decoder tests per pool type (decode known log → correct update)
- Add integration test: startup pattern (drain → build → replay → live)
- Add live WS test against `ws://node:8546`

### Context docs

- Update `src/degenbot/connection/CONTEXT.md` — remove SubscriptionManager, add LogListener, update Subscription description
- Update `CONTEXT-MAP.md` — update cross-module relationships
- Create `src/degenbot/listener/CONTEXT.md`

## Phases

### Phase 1: Rust Double-Buffer

Replace channel-based subscription with double-buffer swap. Add `drain()`, `started()`, signal coalescing. Remove mode transitions. Clippy-clean.

### Phase 2: Python Subscription Update + SubscriptionManager Deletion

Update `Subscription` Python class for drain API. Delete `SubscriptionManager`, `subscription_types.py` configs, `subscription_manager` property. Fix all imports.

### Phase 3: LogListener Module

New `src/degenbot/listener/` module with pure Python `LogListener`. Unit tests.

### Phase 4: Pool LOG_HANDLERS + Decoders

Add `LOG_HANDLERS` class var and decoder functions to each pool type. Decoder tests with known log fixtures.

### Phase 5: Bot Integration

`start_listening()`, `_async_adapters`, config `ws_uri`. Integration tests. Context docs.

### Phase 6: Cleanup + Live Tests

Remove SubscriptionManager tests. Live WS tests. Lint. Move plan to completed.

## Key Decisions

1. **Double-buffer over channel** — GIL-free accumulation, one bulk conversion on drain, no mode transitions
2. **Signal coalescing** — AtomicBool flag, send wake-up only on false→true transition
3. **LogListener is a dispatch registry** — owns no subscriptions, user drives the loop
4. **Exact `(address, topic0)` match** — no wildcards, single interface even if handler count goes up
5. **Sequential handler execution** — ordering matters, no asyncio.Task spawning in dispatch
6. **Fail loudly** — handler exceptions propagate, no catch-and-log
7. **Sync handlers** — `Callable[[dict], None]`, consistent with `external_update()` being sync
8. **Pool owns decode via LOG_HANDLERS** — `ClassVar[dict[str, Callable[[dict], Callable[[Self], None]]]]`
9. **Builder wires, pool declares** — builder reads `LOG_HANDLERS` from pool, user registers with listener (no build_pool() API change)
10. **User wires manually** — no `listener` kwarg on `build_pool()`, explicit 5-line wiring
11. **Curve stays on polling** — `LOG_HANDLERS = {}`, TokenExchange events are differentials not full state
12. **Delete SubscriptionManager** — one layer: Subscription + LogListener
13. **Per-chain start_listening()** — one call per chain, returns tuple of subscriptions
14. **Bot stores async adapters** — `dict[ChainId, AsyncProviderAdapter]` for debugging and lifetime
15. **WS URI from config** — `start_listening()` creates AsyncProviderAdapter from configured WS URI
16. **V3 tick data fetch is acceptable blocking** — rare, bounded, sync RPC in handler is ok

## Empirical Data

### Subscription ordering

| Metric | Value | Source |
|--------|-------|--------|
| Inversions (unfiltered) | **0** | `investigate_unfiltered_volume.py` |
| Inversions (3 separate subs) | 3-5% of pairs | `investigate_raw_node_ordering.py` |
| Inversions (1 combined, 3 topics) | **0** | `investigate_combined_subscription.py` |
| Events per block (mainnet, all logs) | ~800 (range 388-1252) | `investigate_unfiltered_volume.py` |
| Block spread (first→last event) | 8.6ms avg, 13.2ms max | Same |
| Dispatch lookup cost | 376ns/event (set membership) | `investigate_unfiltered_dispatch_cost.py` |
| Subscribe/unsubscribe RTT | 0.4ms | `investigate_dynamic_subscriptions.py` |

### Subscription topology comparison

| Approach | Ordering | Dynamic add | Add disruption | Latency | Complexity |
|----------|----------|-------------|----------------|---------|------------|
| Separate per-topic | ✗ (3-5% inversions) | ✓ (new sub) | None | +0ms | Low |
| Separate + Sorter | ✓ | ✓ (new sub) | None | +12s (or +100-200ms with heads) | Medium |
| Combined topics only | ✓ | ✗ (re-sub all) | Miss events in gap | +0ms | Low |
| **Unfiltered + dispatch** | **✓** | **✓ (dict mutation)** | **None** | **+0ms** | **Low** |

### Raw WS ordering (bypassing web3.py)

| Block | Events | WS transitions | Chain transitions | % inverted | Same-sub inversions |
|-------|--------|---------------|-------------------|------------|-------------------|
| 25117804 | 791 | 2 | 70 | 4.3% | 0 |
| 25117805 | 777 | 16 | 59 | 2.8% | 0 |
| 25117806 | 431 | 4 | 54 | 4.4% | 0 |
| 25117807 | 763 | 12 | 52 | 3.2% | 0 |
| 25117808 | 858 | 39 | 84 | 3.0% | 0 |

All inversions are **cross-subscription**; same-subscription ordering is always correct.

### Unfiltered volume breakdown

- Transfer: 76.1% of events
- V3 Swap: 2.0%
- V2 Sync: 1.3%
- Other (Approval, Mint, Burn, etc.): 20.6%
- Discard cost: ~376ns/event × 800 events/block ≈ 300μs/block total; only ~160 discards/block at 376ns = 60μs/block net discard cost

### web3.py subscription behavior

- **Sequential mode**: 0 ordering violations within a single subscription; cross-subscription batching preserved
- **Parallel mode** (`parallelize=True`): log handlers can start ~5ms BEFORE heads handler for same block finishes — handler completion order is undefined
- web3.py batches events by subscription internally; the node itself sends per-subscription bursts
