"""Subscription primitives for eth_subscribe support.

Provides the `Subscription` async iterator (wrapping the Rust-backed
`AlloySubscription`) and the `LogSubscriptionFilter` dataclass.

The Rust layer uses a double-buffer pattern:
- The pump task writes raw items to the active buffer (no GIL)
- `drain()` atomically swaps buffers, takes the stale buffer,
  and bulk-converts to Python objects in a single GIL acquisition
- `__anext__()` uses drain internally with a local batch for
  `async for` iteration

The `Subscription` class exposes both iteration and bulk drain:
    sub = await provider.subscribe_logs(addresses=[], topics=[[]])
    await sub.started()  # Wait for WS confirmation

    # Iteration mode
    async for log in sub:
        listener.dispatch(log)

    # Or bulk drain mode
    items = sub.drain()  # list[dict]
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from degenbot.exceptions import SubscriptionDisconnected

if TYPE_CHECKING:
    from degenbot.degenbot_rs import AlloySubscription


@dataclass(frozen=True)
class LogSubscriptionFilter:
    """Filter parameters for log subscriptions.

    Unlike the polling `LogFilter`, this type has no block range fields
    because block ranges are meaningless for push subscriptions — you
    listen from "now" onward.

    Args:
        addresses: Contract addresses to filter.
        topics: Event topic filters (nested: [[topic0], [topic1], ...]).

    """

    addresses: list[str] = field(default_factory=list)
    topics: list[list[str]] = field(default_factory=list)


class Subscription:
    """Async iterator wrapping an AlloySubscription.

    Created by `AsyncAlloyProvider.subscribe_*()` methods. Iterate with
    `async for`:

        async for header in subscription:
            print(header)

    Or use `drain()` for bulk consumption:

        items = subscription.drain()

    Raises:
        StopAsyncIteration: Clean unsubscribe or stream end.
        SubscriptionDisconnected: WS/IPC connection dropped.

    """

    def __init__(self, _inner: AlloySubscription) -> None:
        """Initialize the instance."""
        self._inner = _inner

    def __aiter__(self) -> Subscription:
        """Implement __aiter__.

        Returns:
            The subscription instance.

        """
        return self

    async def __anext__(self) -> Any:  # noqa: ANN401
        """Return the next event from the subscription.

        Re-raises `RuntimeError` from the Rust layer as
        `SubscriptionDisconnected` when the connection drops.

        Returns:
            The next event from the subscription.

        Raises:
            StopAsyncIteration: Clean unsubscribe or stream end.
            SubscriptionDisconnected: WS/IPC connection dropped.
            RuntimeError: Unrecognized Rust-layer errors not matching
                disconnection patterns.

        """
        try:
            return await self._inner.__anext__()
        except RuntimeError as e:
            msg_lower = str(e).lower()
            if "disconnected" in msg_lower:
                raise SubscriptionDisconnected(str(e)) from e
            if "already closed" in msg_lower or "closed" in msg_lower:
                raise StopAsyncIteration(str(e)) from e
            raise

    def drain(self) -> list[Any]:
        """Drain accumulated items from the subscription.

        Swaps the internal double-buffer and bulk-converts all
        accumulated items to Python dicts. Returns a list of items.
        The list may be empty if no items have arrived since the
        last drain.

        Returns:
            List of accumulated items from the subscription.

        Raises:
            StopAsyncIteration: Subscription has ended.
            SubscriptionDisconnected: Connection dropped.
            RuntimeError: Unrecognized Rust-layer errors not matching
                disconnection patterns.

        """
        try:
            return self._inner.drain()
        except RuntimeError as e:
            msg_lower = str(e).lower()
            if "disconnected" in msg_lower:
                raise SubscriptionDisconnected(str(e)) from e
            if "already closed" in msg_lower or "closed" in msg_lower:
                raise StopAsyncIteration(str(e)) from e
            raise

    async def started(self) -> None:
        """Wait for the subscription to be confirmed by the node.

        Resolves when the WS subscription is active. Raises
        `SubscriptionDisconnected` if the subscription failed to
        establish.

        Raises:
            SubscriptionDisconnected: If the subscription failed to establish.

        """
        try:
            await self._inner.started()
        except RuntimeError as e:
            raise SubscriptionDisconnected(str(e)) from e

    async def unsubscribe(self) -> None:
        """Unsubscribe from the event stream."""
        self._inner.unsubscribe()

    def __repr__(self) -> str:
        """Return a string representation.

        Returns:
            A string representation of the subscription.

        """
        return f"Subscription({self._inner!r})"
