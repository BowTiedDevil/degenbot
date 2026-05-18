# Plan 046: eth_subscribe Support via AlloyProvider

## Overview

Add `eth_subscribe` support to the provider layer so that `AsyncAlloyProvider` and
`AsyncProviderAdapter` have feature parity with web3.py's async WebSocket subscription
API. The implementation spans four layers: Rust FFI, Python `AsyncAlloyProvider`,
`AsyncProviderAdapter` + `_AsyncWeb3Adapter`, and a `SubscriptionManager` callback layer.

## Problem

Currently, `AlloyProvider` can connect via WS/IPC transports but only for request/response
RPC calls. The Alloy Rust `Provider` trait already exposes `subscribe_blocks()`,
`subscribe_logs()`, etc., but no PyO3 bindings exist. There is no way to receive push
notifications from an Ethereum node via degenbot's provider stack.

Users who need subscriptions must bypass degenbot entirely and use web3.py directly.

## Design Decisions

### D1: Async iterator + callback manager (both in this plan)

Alloy's Rust `SubscriptionStream` maps naturally to Python's `async for` protocol. The
`SubscriptionManager` callback layer is pure Python orchestration built on top of async
iterators. Building both together ensures the primitive API evolves with the callback
layer's needs.

### D2: Runtime error for HTTP providers

Calling `subscribe_*()` on an HTTP-connected provider raises `SubscriptionNotSupported`
at runtime with a clear message naming the transport and URL. No separate provider type.

### D3: Background tokio task + mpsc channel + batch GIL conversion

Each subscription spawns a tokio task that reads from the `SubscriptionStream`, drains
already-buffered items with `now_or_never()`, then converts the entire batch to Python
objects in a single `Python::attach()` call. Items are sent through a `mpsc::Receiver`
as a `SubItem` enum:

```rust
enum SubItem {
    Event(PyObject),
    End,
    Disconnected { message: String },
}
```

### D4: Five subscription methods on async providers only

| Method | Alloy returns | Python yields |
|--------|--------------|---------------|
| `subscribe_blocks()` | `Header` | Block header dict |
| `subscribe_full_blocks()` | `Block` | Full block dict |
| `subscribe_pending_transactions()` | `B256` | Tx hash hex string |
| `subscribe_full_pending_transactions()` | `Transaction` | Transaction dict |
| `subscribe_logs(addresses, topics)` | `Log` | Log receipt dict |

Sync `ProviderBackend` and `ProviderAdapter` get stub methods that raise
`SubscriptionNotSupported` with a message pointing to the async equivalents.

### D5: Separate `LogSubscriptionFilter`

New `LogSubscriptionFilter(addresses, topics)` — no `from_block`/`to_block`. Block ranges
are meaningless for subscriptions. Separate type from polling `LogFilter`.

### D6: Exception hierarchy

```
SubscriptionError (base)
├── SubscriptionNotSupported  — HTTP transport, or sync provider
└── SubscriptionDisconnected   — WS/IPC connection dropped during active subscription
```

Lives in `degenbot.exceptions.infrastructure`.

### D7: `SubscriptionDisconnected` for silent connection drops

When the WS/IPC connection drops silently (network partition, node restart), the pump
task detects it and sends `SubItem::Disconnected`. The Python `Subscription.__anext__()`
raises `SubscriptionDisconnected` instead of `StopAsyncIteration`. The distinction:

| Event | `__anext__()` behavior |
|-------|------------------------|
| User unsubscribed | `StopAsyncIteration` — clean shutdown |
| Connection dropped | `SubscriptionDisconnected` — actionable failure |
| Successful item | Returns the item |

### D8: Web3 adapter — single WS connection, demultiplexed

`_AsyncWeb3Adapter` creates one `AsyncWeb3` WS connection and one background
`process_subscriptions()` loop. Events are routed to per-subscription `asyncio.Queue`s
by subscription ID. Each `_AsyncWeb3Subscription` wraps its queue and exposes the
`Subscription` async iterator interface.

This avoids multiple WS connections (important for RPC connection limits) and preserves
RPC arrival order since `process_subscriptions()` yields events in wire order.

### D9: `SubscriptionManager` — central sequencer + parallel handler dispatch

- Standalone constructor `SubscriptionManager(adapter)` + lazy property
  `adapter.subscription_manager`
- Subscribes via typed config dataclasses, not raw strings
- Central sequencer merges all `Subscription` async iterators preserving RPC arrival
  order, dispatches each event to its handler as a separate `asyncio.Task` (parallel)
- **Ordering guarantee:** handler *invocations* preserve RPC arrival order across all
  subscriptions; handler *completion* order is not guaranteed. Within a single
  subscription, items are always processed in the order received.
- Returns when all subscriptions end (unsubscribe, disconnect, or explicit stop)
- `idle_timeout: float = 0` — liveness guard. If no event arrives from the RPC within
  `idle_timeout` seconds, raise `SubscriptionDisconnected`. Default 0 means no check.
- Handler errors: log at WARNING level and continue. The subscription stays alive.
- `unsubscribe(label)` cancels the handler task immediately. Handler responsible for
  its own cleanup via `try/finally`.

### D13: Ordering investigation — web3.py parallel mode reorders events

Empirical testing against a mainnet WS endpoint (see
`scripts/investigate_web3_parallel_ordering.py`) confirmed that web3.py's
`SubscriptionManager` with `parallelize=True` does **not** preserve per-block
handler execution order:

- **Sequential mode** (`parallelize=False`, default): events are `await`ed in
  queue order. `newHeads` for block N always completes before log handlers for
  block N start. Ordering is preserved but throughput is limited.
- **Parallel mode** (`parallelize=True`): handlers are dispatched as
  `asyncio.create_task()`. The next event is dequeued immediately after dispatch,
  without waiting for the previous handler. Log handlers for block N can start
  executing **before** the `newHeads` handler for block N has finished. In
  testing, log handlers started ~5ms before the heads handler completed (with a
  5ms heads delay / 50ms logs delay). No per-block ordering guarantee.

degenbot's `SubscriptionManager` avoids this by using per-subscription queues
and a central `_wait_for_any_event` merge that only dispatches the next event
after the current one has been *dispatched* (though not completed). The
sequential queue drain preserves RPC arrival order at the dispatch level.

Separate testing (`scripts/investigate_web3_subscription_ordering.py`) confirmed
that web3.py's sequential mode and the RPC node itself preserve per-block
ordering: newHeads events always arrive before logs for the same block number
across multiple runs (~25 blocks tested).

### D10: Typed subscription configs — frozen dataclasses

```python
@dataclass(frozen=True)
class NewHeadsSubscription:
    label: str | None = None
    handler: Callable[[dict], Awaitable[None]] | None = None


@dataclass(frozen=True)
class FullBlocksSubscription:
    label: str | None = None
    handler: Callable[[dict], Awaitable[None]] | None = None


@dataclass(frozen=True)
class PendingTransactionsSubscription:
    label: str | None = None
    full_transactions: bool = False
    handler: Callable[[str | dict], Awaitable[None]] | None = None


@dataclass(frozen=True)
class LogsSubscription:
    label: str | None = None
    addresses: list[str] | None = None
    topics: list[list[str]] | None = None
    handler: Callable[[dict], Awaitable[None]] | None = None
```

No `SyncingSubscription` — Alloy's `Provider` trait doesn't expose it and it's rarely
used.

### D11: Naming

- Python async iterator class: `Subscription`
- Rust PyO3 class: `PySubscription`
- Web3 adapter wrapper: `_AsyncWeb3Subscription`

### D12: Module organization

| File | Contents |
|------|----------|
| `degenbot.provider.subscription` | `Subscription`, `LogSubscriptionFilter` |
| `degenbot.provider.subscription_types` | `NewHeadsSubscription`, `FullBlocksSubscription`, `PendingTransactionsSubscription`, `LogsSubscription` |
| `degenbot.provider.subscription_manager` | `SubscriptionManager` |
| `degenbot.exceptions.infrastructure` | `SubscriptionError`, `SubscriptionNotSupported`, `SubscriptionDisconnected` |
| `rust/src/subscription.rs` | Pump task, `SubItem` enum, `SubscriptionHandle` |
| `rust/src/subscription_py.rs` | `PySubscription` PyO3 class, `add_subscription_module()` |

---

## Implementation Phases

### Phase 1: Rust FFI Layer ✅

- [x] Add `alloy` crate subscription imports to `rust/src/provider.rs`
- [x] Create `rust/src/subscription.rs` with `SubItem`, `SubscriptionHandle`, pump task logic
- [x] Add `subscribe_blocks`, `subscribe_full_blocks`,
      `subscribe_pending_transactions`, `subscribe_full_pending_transactions`,
      `subscribe_logs` methods to `AlloyProvider` in
      `rust/src/provider.rs`
- [x] Create `rust/src/subscription_py.rs` with `PyAlloySubscription` PyO3 class
  - `__aiter__` / `__anext__` via `future_into_py` + `mpsc::Receiver`
  - `unsubscribe()` method
- [x] Register `subscription` + `subscription_py` modules in `rust/src/lib.rs`
- [x] Add `provider_arc()` accessor to `AlloyProvider` in `rust/src/provider.rs`
- [x] Add `header_to_py_dict()` converter in `rust/src/py_converters.rs`
- [x] Add `SubscriptionNotSupported` error variant in `rust/src/errors.rs`
- [x] Add `futures-util` dependency in `rust/Cargo.toml`
- [x] Add `close()` method to `PyAlloyProvider` in `rust/src/provider_py.rs`
- [x] Expose 5 `subscribe_*()` methods on `PyAlloyProvider` in `rust/src/provider_py.rs`
- [x] Verify `cargo clippy` passes

### Phase 2: Python AsyncAlloyProvider Bindings ✅

- [x] Add exception types to `degenbot/exceptions/infrastructure.py`:
      `SubscriptionError`, `SubscriptionNotSupported`, `SubscriptionDisconnected`
- [x] Update `degenbot/exceptions/__init__.py` re-exports
- [x] Create `degenbot/provider/subscription.py` with:
  - `Subscription` class (wraps `AlloySubscription` from Rust)
  - `LogSubscriptionFilter` frozen dataclass
- [x] Create `degenbot/provider/subscription_types.py` with typed subscription configs
- [x] Create `degenbot/provider/subscription_manager.py` with `SubscriptionManager`
- [x] Update `degenbot/degenbot_rs.pyi` type stubs with `AlloySubscription` and subscribe methods
- [x] Update `degenbot/provider/__init__.py` re-exports
- [ ] Write tests against live WS endpoint (guarded by `ETHEREUM_ARCHIVE_NODE_WS_URI`)

### Phase 3: AsyncProviderAdapter Wiring ✅

- [x] Add `subscribe_*()` methods to `AsyncProviderBackend` protocol
- [x] Add `subscribe_*()` methods to `_AsyncAlloyAdapter` (delegate to
      `AsyncAlloyProvider`)
- [x] Add `subscribe_*()` stubs to `_AsyncWeb3Adapter` (raise `SubscriptionNotSupported`) —
      web3.py subscriptions deferred; users should use `from_alloy()` with a WS URL
- [x] Add `subscribe_*()` methods to `AsyncProviderAdapter` (delegate to
      `self._backend`)
- [x] Add `subscription_manager` lazy property to `AsyncProviderAdapter`
- [x] Add `subscribe_*()` stub methods to `ProviderBackend`, `ProviderAdapter`,
      `_Web3Adapter`, `_AlloyAdapter`, `_OfflineAdapter` (raise `SubscriptionNotSupported`)
- [x] Provider pickle: `AsyncProviderAdapter.__getstate__`/`__setstate__` omit
      `_subscription_manager` and null out `_backend`/`_raw_provider`
- [ ] Write integration tests against live WS endpoint (guarded by env var)

### Phase 4: SubscriptionManager + Typed Configs ✅

- [x] Create `degenbot/provider/subscription_types.py` with frozen dataclasses
- [x] Create `degenbot/provider/subscription_manager.py` with `SubscriptionManager`:
  - `__init__(adapter: AsyncProviderAdapter)`
  - `subscribe(subscriptions: list[...])` — creates `Subscription` objects + handler mapping
  - `handle_subscriptions(idle_timeout: float = 0)` — central sequencer loop
  - `unsubscribe(label: str)`, `unsubscribe_all()`, `stop()`
- [x] Add `subscription_manager` lazy property to `AsyncProviderAdapter`
- [x] Update `degenbot/provider/__init__.py` re-exports
- [x] Write tests with synthetic events (33 tests in tests/rust/test_subscriptions.py)

### Phase 5: Integration Tests ✅

- [x] Write live WS integration test (guarded by `DEGENBOT_SKIP_LIVE_WS_TESTS`):
  - `subscribe_blocks` → receive at least one header
  - `subscribe_logs` with no filter → receive a log
  - `unsubscribe` → `StopAsyncIteration`
  - HTTP provider → `SubscriptionNotSupported`
  - `AsyncProviderAdapter.subscribe_blocks()` works via live WS
  - `SubscriptionManager` dispatches to handler via live WS

Note: web3.py's subscription_manager order-preservation comparison is deferred.
Alloy's internal subscription ordering is preserved by the pump task + FIFO
mpsc channel. The `_wait_for_any_event` merge preserves per-subscription FIFO.
Cross-subscription ordering matches RPC arrival order (items are queued in the
order they arrive from the pump tasks).

### Phase 6: Integration & Polish ✅

- [x] Update `degenbot/connection/CONTEXT.md` with subscription terms
- [x] Update `CONTEXT-MAP.md` with cross-module relationships to SubscriptionManager
- [x] Verify `just test-rust-python` passes (202 passed, 6 skipped)
- [x] Verify `cargo clippy` passes
- [x] Verify `ruff check` passes
- [x] Move plan to `plans/completed/`

---

## New Terms (for CONTEXT.md updates)

**Subscription**: An async iterator yielding push events from an Ethereum node via
`eth_subscribe`. Created by `AsyncProviderAdapter.subscribe_*()` methods. Iterated with
`async for`. Terminates with `StopAsyncIteration` (clean) or `SubscriptionDisconnected`
(connection lost).
_Avoid_: subscription stream, subscription handle, event stream

**SubscriptionManager**: Orchestrates multiple Subscriptions with typed config
dataclasses and handler callbacks. Dispatches events in RPC arrival order to parallel
handler tasks. Available as `adapter.subscription_manager` or standalone.
_Avoid_: subscription handler, event dispatcher

**LogSubscriptionFilter**: Filter parameters for log subscriptions with `addresses`
and `topics` only — no block range (meaningless for push subscriptions).
_Avoid_: log filter subscription, subscription filter
