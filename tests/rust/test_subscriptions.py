"""
Tests for eth_subscribe support.

Tests the Python subscription layer (Subscription, LogSubscriptionFilter)
without requiring a live WebSocket endpoint.
"""

from __future__ import annotations

import asyncio
from typing import Any

import pytest

from degenbot.exceptions import (
    DegenbotError,
    SubscriptionDisconnected,
    SubscriptionError,
    SubscriptionNotSupported,
)
from degenbot.provider import AlloyProvider, ProviderAdapter
from degenbot.provider.subscription import LogSubscriptionFilter, Subscription

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


class FakeAsyncIterator:
    """Fake async iterator that yields a sequence of items, then stops."""

    def __init__(self, items: list[Any]) -> None:
        self._items = list(items)
        self._index = 0
        self._unsubscribed = False

    def __aiter__(self) -> FakeAsyncIterator:
        return self

    async def __anext__(self) -> Any:
        if self._unsubscribed:
            raise StopAsyncIteration
        if self._index >= len(self._items):
            raise StopAsyncIteration
        item = self._items[self._index]
        self._index += 1
        return item

    def unsubscribe(self) -> None:
        self._unsubscribed = True


class FakeSubscription:
    """
    Mocks degenbot.degenbot_rs.AlloySubscription for testing.

    Supports drain(), started(), and the async iterator protocol.
    """

    def __init__(self, items: list[Any]) -> None:
        self._inner = FakeAsyncIterator(items)
        self._drain_items: list[list[Any]] = []  # Items returned by successive drain() calls
        self._drain_index = 0
        self._started_event = asyncio.Event()
        self._start_error: str | None = None

    def __aiter__(self) -> FakeAsyncIterator:
        return self._inner.__aiter__()

    async def __anext__(self) -> Any:
        return await self._inner.__anext__()

    def drain(self) -> list[Any]:
        """Return items from the next drain batch, or empty list."""
        if self._drain_index < len(self._drain_items):
            items = self._drain_items[self._drain_index]
            self._drain_index += 1
            return items
        return []

    async def started(self) -> None:
        """Wait for subscription to start, or raise on failure."""
        if self._start_error is not None:
            raise RuntimeError(self._start_error)
        self._started_event.set()

    def unsubscribe(self) -> None:
        self._inner.unsubscribe()

    def __repr__(self) -> str:
        return "FakeSubscription"


# ---------------------------------------------------------------------------
# Subscription tests
# ---------------------------------------------------------------------------


class TestSubscription:
    """Tests for the Subscription wrapper class."""

    def test_repr(self) -> None:
        fake = FakeSubscription([])
        sub = Subscription(_inner=fake)  # type: ignore[arg-type]
        assert "Subscription" in repr(sub)

    def test_aiter_returns_self(self) -> None:
        fake = FakeSubscription([])
        sub = Subscription(_inner=fake)  # type: ignore[arg-type]
        assert aiter(sub) is sub

    @pytest.mark.asyncio
    async def test_async_iteration(self) -> None:
        fake = FakeSubscription([{"number": 1}, {"number": 2}])
        sub = Subscription(_inner=fake)  # type: ignore[arg-type]
        items = [item async for item in sub]
        assert len(items) == 2
        assert items[0] == {"number": 1}
        assert items[1] == {"number": 2}

    @pytest.mark.asyncio
    async def test_unsubscribe_stops_iteration(self) -> None:
        fake = FakeSubscription([{"number": 1}, {"number": 2}])
        sub = Subscription(_inner=fake)  # type: ignore[arg-type]
        items = []
        async for item in sub:
            items.append(item)
            await sub.unsubscribe()
            await asyncio.sleep(0)
        assert len(items) == 1

    @pytest.mark.asyncio
    async def test_disconnected_raises_exception(self) -> None:
        """Test that RuntimeError with 'disconnected' is re-raised as SubscriptionDisconnected."""

        class FakeDisconnected:
            def __aiter__(self) -> FakeDisconnected:
                return self

            async def __anext__(self) -> Any:
                msg = "Subscription disconnected: connection lost"
                raise RuntimeError(msg)

            def unsubscribe(self) -> None:
                pass

        sub = Subscription(_inner=FakeDisconnected())  # type: ignore[arg-type]
        with pytest.raises(SubscriptionDisconnected):
            await anext(sub)

    @pytest.mark.asyncio
    async def test_non_disconnected_runtime_error_passes_through(self) -> None:
        """Test that non-disconnected RuntimeError is not caught."""

        class FakeError:
            def __aiter__(self) -> FakeError:
                return self

            async def __anext__(self) -> Any:
                msg = "Some other error"
                raise RuntimeError(msg)

            def unsubscribe(self) -> None:
                pass

        sub = Subscription(_inner=FakeError())  # type: ignore[arg-type]
        with pytest.raises(RuntimeError, match="Some other error"):
            await anext(sub)

    @pytest.mark.asyncio
    async def test_drain_delegates_to_inner(self) -> None:
        """Test that drain() calls the inner subscription's drain()."""
        fake = FakeSubscription([])
        fake._drain_items = [
            [{"number": 1}, {"number": 2}],
            [{"number": 3}],
        ]
        sub = Subscription(_inner=fake)  # type: ignore[arg-type]

        # First drain returns the first batch
        items1 = sub.drain()
        assert items1 == [{"number": 1}, {"number": 2}]

        # Second drain returns the next batch
        items2 = sub.drain()
        assert items2 == [{"number": 3}]

        # Third drain returns empty
        items3 = sub.drain()
        assert items3 == []

    @pytest.mark.asyncio
    async def test_drain_disconnected_raises(self) -> None:
        """Test that drain() re-raises disconnected RuntimeError."""

        class FakeDrainDisconnected:
            def __aiter__(self) -> FakeDrainDisconnected:
                return self

            async def __anext__(self) -> Any:
                raise StopAsyncIteration

            def drain(self) -> list[Any]:
                msg = "Subscription disconnected: connection lost"
                raise RuntimeError(msg)

            def unsubscribe(self) -> None:
                pass

        sub = Subscription(_inner=FakeDrainDisconnected())  # type: ignore[arg-type]
        with pytest.raises(SubscriptionDisconnected):
            sub.drain()

    @pytest.mark.asyncio
    async def test_started_delegates_to_inner(self) -> None:
        """Test that started() awaits the inner subscription's started()."""
        fake = FakeSubscription([])
        sub = Subscription(_inner=fake)  # type: ignore[arg-type]
        await sub.started()  # Should not raise

    @pytest.mark.asyncio
    async def test_started_disconnected_raises(self) -> None:
        """Test that started() re-raises as SubscriptionDisconnected."""

        class FakeStartFailed:
            def __aiter__(self) -> FakeStartFailed:
                return self

            async def __anext__(self) -> Any:
                raise StopAsyncIteration

            async def started(self) -> None:
                msg = "Connection refused"
                raise RuntimeError(msg)

            def unsubscribe(self) -> None:
                pass

        sub = Subscription(_inner=FakeStartFailed())  # type: ignore[arg-type]
        with pytest.raises(SubscriptionDisconnected, match="Connection refused"):
            await sub.started()


# ---------------------------------------------------------------------------
# LogSubscriptionFilter tests
# ---------------------------------------------------------------------------


class TestLogSubscriptionFilter:
    """Tests for LogSubscriptionFilter."""

    def test_default_empty(self) -> None:
        f = LogSubscriptionFilter()
        assert f.addresses == []
        assert f.topics == []

    def test_frozen(self) -> None:
        f = LogSubscriptionFilter(addresses=["0x1"], topics=[["0xa"]])
        assert f.addresses == ["0x1"]
        assert f.topics == [["0xa"]]
        with pytest.raises(AttributeError):
            f.addresses = []  # type: ignore[misc]

    def test_equality(self) -> None:
        f1 = LogSubscriptionFilter(addresses=["0x1"])
        f2 = LogSubscriptionFilter(addresses=["0x1"])
        assert f1 == f2


# ---------------------------------------------------------------------------
# ProviderAdapter stub tests
# ---------------------------------------------------------------------------


class TestSyncProviderStubs:
    """Tests that sync ProviderAdapter subscribe_* methods raise correctly."""

    def test_provider_adapter_subscribe_blocks_raises(self) -> None:
        provider = AlloyProvider("https://eth.llamarpc.com/")
        adapter = ProviderAdapter.from_alloy(provider)
        with pytest.raises(SubscriptionNotSupported):
            adapter.subscribe_blocks()

    def test_provider_adapter_subscribe_logs_raises(self) -> None:
        provider = AlloyProvider("https://eth.llamarpc.com/")
        adapter = ProviderAdapter.from_alloy(provider)
        with pytest.raises(SubscriptionNotSupported):
            adapter.subscribe_logs()


# ---------------------------------------------------------------------------
# Exception tests
# ---------------------------------------------------------------------------


class TestSubscriptionExceptions:
    """Tests for subscription exception types."""

    def test_subscription_not_supported_message(self) -> None:
        exc = SubscriptionNotSupported(transport="http", rpc_url="https://example.com")
        assert "http" in str(exc)
        assert "https://example.com" in str(exc)

    def test_subscription_disconnected_message(self) -> None:
        exc = SubscriptionDisconnected("Connection lost", rpc_url="wss://example.com")
        assert "Connection lost" in str(exc)
        assert "wss://example.com" in str(exc)

    def test_subscription_error_hierarchy(self) -> None:
        assert issubclass(SubscriptionNotSupported, SubscriptionError)
        assert issubclass(SubscriptionDisconnected, SubscriptionError)
        assert issubclass(SubscriptionError, DegenbotError)
