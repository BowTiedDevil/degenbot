# Event Middleware Architecture for SubscriptionManager (Superseded)

> **Superseded by `EVENT_DRIVEN_ARCHITECTURE.md`** which recommends an
> unfiltered log subscription that eliminates the need for a
> LogIndexSorter middleware. The EventMiddleware protocol design in this
> document is still valid as a general extensibility mechanism, but the
> specific LogIndexSorter use case is no longer the recommended approach.

## Design Goal

Allow users to plug in a log-index sorter (or any other event transformation)
between the subscription merge queue and handler dispatch, without the
SubscriptionManager taking responsibility for it.

## Current Pipeline

```
pump → per-sub queue → _wait_for_any_event → handler dispatch
```

Events flow in RPC-arrival order. Each `(label, item)` pair is dispatched
immediately to the handler as a fire-and-forget `asyncio.Task`.

## Proposed: EventMiddleware Protocol

A protocol that sits between the merge step and handler dispatch. The
SubscriptionManager calls the middleware for every event and awaits its
decision on what (if anything) to dispatch.

```python
from typing import Protocol, Any

class EventMiddleware(Protocol):
    """Pluggable middleware for subscription event dispatch.

    Installed on SubscriptionManager via the `middleware` constructor
    argument. Called once for every event before handler dispatch.

    The middleware can:
    - Pass events through unchanged (identity middleware)
    - Buffer and reorder events (log-index sorter)
    - Drop or transform events (deduplication, filtering)
    - Inject synthetic events (heartbeats, block-boundary markers)

    The middleware does NOT replace the merge step — it receives events
    in RPC-arrival order across all subscriptions. It can reorder before
    dispatch, but it cannot change which events arrive from the node.
    """

    async def ingest(self, label: str, item: dict[str, Any]) -> list[tuple[str, dict[str, Any]]]:
        """Process a single event.

        Called by SubscriptionManager for each event in merge order.

        Args:
            label: Subscription label that produced this event.
            item: The event payload (block header, log receipt, etc.).

        Returns:
            A list of (label, item) pairs to dispatch to handlers.
            Return an empty list to suppress dispatch.
            Return the input [(label, item)] to pass through unchanged.
            Return multiple pairs to inject synthetic events.
            Return a reordered list to change dispatch sequence.

            The returned pairs are dispatched to their respective
            handlers in list order.
        """
        ...

    async def flush(self) -> list[tuple[str, dict[str, Any]]]:
        """Flush any buffered events.

        Called when all subscriptions are gone (handle_subscriptions
        is about to return) or when the SubscriptionManager is stopped.

        Returns:
            A list of (label, item) pairs to dispatch before shutdown.
        """
        ...
```

## Why a Protocol, not a base class

- Composition over inheritance — users implement one method (`ingest`) or two
  (`ingest` + `flush`).
- `Protocol` is `runtime_checkable` via `hasattr` — doesn't force a base class
  into user code.
- Matches the project's existing pattern (see `ProviderBackend`, `AsyncProviderBackend`).

## Why `ingest` returns a list, not a single item

Three reasons:

1. **Buffering**: The log-index sorter absorbs events for the current block
   and returns an empty list. When it sees the next block, it returns the
   entire sorted block's worth of events. This is a multi-event return.

2. **Deduplication**: When overlapping subscriptions produce duplicate logs,
   the middleware can return just the unique ones.

3. **Injection**: A middleware could inject synthetic events (e.g., a
   "block boundary" marker when newHeads arrives).

The cost (allocating a list for every event) is negligible compared to
the async I/O overhead of WebSocket message processing.

## Identity Middleware (default)

```python
class _IdentityMiddleware:
    """Default pass-through middleware."""

    async def ingest(self, label: str, item: dict[str, Any]) -> list[tuple[str, dict[str, Any]]]:
        return [(label, item)]

    async def flush(self) -> list[tuple[str, dict[str, Any]]]:
        return []
```

When no middleware is provided, SubscriptionManager uses this internally.
Zero overhead — the list is immediate, no buffering.

## Integration Point in SubscriptionManager

```python
class SubscriptionManager:
    def __init__(
        self,
        adapter: AsyncProviderAdapter,
        middleware: EventMiddleware | None = None,
    ) -> None:
        self._adapter = adapter
        self._subscriptions: dict[str, _ManagedSubscription] = {}
        self._middleware = middleware  # None → _IdentityMiddleware at handle_subscriptions time
```

Inside `handle_subscriptions()`, the dispatch section changes from:

```python
# Current: immediate dispatch
label, item = result
managed_sub = self._subscriptions.get(label)
if managed_sub is not None and managed_sub.handler is not None:
    task = asyncio.create_task(
        _invoke_handler(managed_sub.handler, item),
        name=f"sub-handler-{label}",
    )
```

to:

```python
# Proposed: route through middleware
label, item = result
to_dispatch = await self._middleware.ingest(label, item)
for dispatch_label, dispatch_item in to_dispatch:
    managed_sub = self._subscriptions.get(dispatch_label)
    if managed_sub is not None and managed_sub.handler is not None:
        task = asyncio.create_task(
            _invoke_handler(managed_sub.handler, dispatch_item),
            name=f"sub-handler-{dispatch_label}",
        )
```

After the main loop (in the `finally` block):

```python
finally:
    # Flush any remaining buffered events
    remaining = await self._middleware.flush()
    for dispatch_label, dispatch_item in remaining:
        managed_sub = self._subscriptions.get(dispatch_label)
        if managed_sub is not None and managed_sub.handler is not None:
            await _invoke_handler(managed_sub.handler, dispatch_item)

    for task in pump_tasks:
        task.cancel()
    await asyncio.gather(*pump_tasks, return_exceptions=True)
```

## LogIndexSorter as a Middleware Implementation

```python
class LogIndexSorter:
    """Reorders subscription-batched log events by logIndex within each block.

    Log events (those with a 'logIndex' key) are buffered per-block.
    When a new block number is seen — either from a log event for block N+1
    or a newHeads event for block N+1 — the previous block's events are
    sorted by logIndex and emitted.

    Non-log events (newHeads, pending transactions) pass through immediately.
    A newHeads event also triggers a flush of the previous block's logs.

    Usage::

        sorter = LogIndexSorter()
        manager = SubscriptionManager(adapter, middleware=sorter)
        await manager.subscribe([...])
        await manager.handle_subscriptions()
    """

    def __init__(self) -> None:
        self._buffer: dict[int, list[tuple[str, dict[str, Any]]]] = {}

    async def ingest(self, label: str, item: dict[str, Any]) -> list[tuple[str, dict[str, Any]]]:
        # newHeads: pass through immediately, flush previous block
        if self._is_new_heads(item):
            block_number = self._parse_int(item, "number")
            if block_number is not None:
                flushed = self._flush_block(block_number - 1)
                return [(label, item), *flushed]
            return [(label, item)]

        # Log events: buffer by blockNumber
        if self._is_log(item):
            block_number = self._parse_int(item, "blockNumber")
            if block_number is None:
                return [(label, item)]

            self._buffer.setdefault(block_number, []).append((label, item))

            # Block transition: flush the previous block
            if (block_number - 1) in self._buffer:
                return self._flush_block(block_number - 1)
            return []

        # Other event types: pass through
        return [(label, item)]

    async def flush(self) -> list[tuple[str, dict[str, Any]]]:
        results: list[tuple[str, dict[str, Any]]] = []
        for block_number in sorted(self._buffer):
            results.extend(self._flush_block(block_number))
        return results

    def _flush_block(self, block_number: int) -> list[tuple[str, dict[str, Any]]]:
        events = self._buffer.pop(block_number, None)
        if events is None:
            return []
        events.sort(key=lambda pair: self._parse_int(pair[1], "logIndex") or 0)
        return events

    @staticmethod
    def _is_new_heads(item: dict[str, Any]) -> bool:
        return "number" in item and "logIndex" not in item and "parentHash" in item

    @staticmethod
    def _is_log(item: dict[str, Any]) -> bool:
        return "logIndex" in item

    @staticmethod
    def _parse_int(item: dict[str, Any], key: str) -> int | None:
        val = item.get(key)
        if val is None:
            return None
        if isinstance(val, str):
            return int(val, 16) if val.startswith("0x") else int(val)
        if isinstance(val, int):
            return val
        if hasattr(val, "__int__"):
            return int(val)
        return None
```

## Why on SubscriptionManager, not on the provider

The user asked: "allow the provider to use this sorting mechanism without
needing to take responsibility of it." The question is: what takes responsibility?

The **provider** (AsyncProviderAdapter) creates subscriptions — it's the factory.
The **SubscriptionManager** dispatches events — it's the orchestrator.
The **middleware** sorts/buffers — it's a pluggable strategy.

The middleware is installed on SubscriptionManager, but it's *provided by the
user*. Neither the provider nor the SubscriptionManager takes responsibility
for sorting — the middleware does. The SubscriptionManager just calls
`middleware.ingest()` and dispatches whatever comes back.

The provider's only involvement is that its `subscription_manager` property
creates the manager — and the user can install the middleware at that point:

```python
adapter = AsyncProviderAdapter.from_alloy(async_alloy)
sorter = LogIndexSorter()
manager = SubscriptionManager(adapter, middleware=sorter)
# — or, using the lazy property pattern:
# adapter.subscription_manager  (no middleware — identity)
# But users who want sorting construct the manager themselves.
```

This keeps the provider a pure factory with no sorting logic, the
SubscriptionManager a pure dispatcher with no sorting logic, and the
middleware a self-contained strategy that the user owns.

## Why not on the adapter property

The `subscription_manager` lazy property on AsyncProviderAdapter creates a
SubscriptionManager without a middleware. This is the zero-config default
that preserves current behavior. Users who want sorting construct their own
SubscriptionManager with the middleware they choose.

Alternative considered: middleware on the adapter constructor or a setter.
Rejected because:
- The adapter is a general-purpose provider, not subscription-specific.
- A user might want different middleware for different managers.
- The adapter's `subscription_manager` property is a convenience for the
  common case (no middleware).

## Composability

Middlewares compose naturally via chaining:

```python
class ChainedMiddleware:
    def __init__(self, middlewares: list[EventMiddleware]) -> None:
        self._middlewares = middlewares

    async def ingest(self, label: str, item: dict[str, Any]) -> list[tuple[str, dict[str, Any]]]:
        pairs = [(label, item)]
        for mw in self._middlewares:
            next_pairs = []
            for lbl, itm in pairs:
                next_pairs.extend(await mw.ingest(lbl, itm))
            pairs = next_pairs
        return pairs

    async def flush(self) -> list[tuple[str, dict[str, Any]]]:
        pairs = []
        for mw in self._middlewares:
            pairs.extend(await mw.flush())
        return pairs


# Usage: deduplicate, then sort
sorter = LogIndexSorter()
dedup = DeduplicationMiddleware()
manager = SubscriptionManager(adapter, middleware=ChainedMiddleware([dedup, sorter]))
```

## Summary

| Component | Responsibility |
|-----------|---------------|
| `AsyncProviderAdapter` | Factory for subscriptions + lazy `subscription_manager` (no middleware) |
| `SubscriptionManager` | Orchestrates subscriptions, calls `middleware.ingest()` before dispatch |
| `EventMiddleware` (Protocol) | Pluggable strategy — user implements `ingest()` and `flush()` |
| `LogIndexSorter` | Concrete middleware that reorders log events by logIndex within each block |
| `_IdentityMiddleware` | Default pass-through (used when no middleware is provided) |

The provider doesn't know about sorting. The SubscriptionManager doesn't know
about sorting. The middleware does the sorting. The user owns the middleware.
