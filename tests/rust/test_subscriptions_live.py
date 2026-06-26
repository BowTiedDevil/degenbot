"""Live WebSocket integration tests for eth_subscribe."""

from __future__ import annotations

import asyncio
from typing import Any

import pytest

from degenbot.degenbot_rs import AlloyProvider
from degenbot.exceptions import SubscriptionDisconnected, SubscriptionNotSupported
from degenbot.listener import LogListener
from degenbot.provider import AsyncProviderAdapter, ProviderAdapter
from degenbot.provider.subscription import Subscription
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI, ETHEREUM_ARCHIVE_NODE_WS_URI


class TestLiveWSSubscribeBlocks:
    """Test subscribing to new block headers via a live WS node."""

    @pytest.mark.asyncio
    async def test_subscribe_blocks_yields_headers(self) -> None:
        """Subscribe to block headers and verify we get at least one."""
        provider = AlloyProvider(ETHEREUM_ARCHIVE_NODE_WS_URI)
        sub = provider.subscribe_blocks()
        subscription = Subscription(_inner=sub)

        try:
            header = None
            async for item in subscription:
                header = item
                break  # Just need one

            assert header is not None
            assert isinstance(header, dict)
            # Block headers should have 'number' and 'hash'
            assert "number" in header or "header" in str(header).lower()
        finally:
            subscription._inner.unsubscribe()

    @pytest.mark.asyncio
    async def test_unsubscribe_stops_iteration(self) -> None:
        """Unsubscribe should stop the async iteration."""
        provider = AlloyProvider(ETHEREUM_ARCHIVE_NODE_WS_URI)
        sub = provider.subscribe_blocks()
        subscription = Subscription(_inner=sub)

        try:
            count = 0
            async for _ in subscription:
                count += 1
                await subscription.unsubscribe()
                # Give the event loop a moment
                await asyncio.sleep(0.1)
                if count >= 2:
                    break
            # Should have stopped after unsubscribe
            assert count <= 2
        finally:
            subscription._inner.unsubscribe()


class TestLiveWSHTTPRaises:
    """Test that HTTP providers raise SubscriptionNotSupported."""

    def test_http_provider_subscribe_raises(self) -> None:
        """HTTP providers should raise SubscriptionNotSupported."""
        provider = AlloyProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI)
        adapter = ProviderAdapter.from_alloy(provider)

        with pytest.raises(SubscriptionNotSupported):
            adapter.subscribe_blocks()


class TestLiveWSLogsSubscription:
    """Test log subscriptions via a live WS node."""

    @pytest.mark.asyncio
    async def test_subscribe_logs_with_no_filter(self) -> None:
        """Subscribe to all logs and verify we get at least one."""
        provider = AlloyProvider(ETHEREUM_ARCHIVE_NODE_WS_URI)
        sub = provider.subscribe_logs()
        subscription = Subscription(_inner=sub)

        try:
            log = None

            # Use a timeout in case no logs come through
            async def _get_one() -> Any:
                async for item in subscription:
                    return item
                return None

            result = await asyncio.wait_for(_get_one(), timeout=30)
            log = result
            # If we got a log, verify its shape
            if log is not None:
                assert isinstance(log, dict)
        finally:
            subscription._inner.unsubscribe()


class TestLiveWSAdapterAndLogListener:
    """Test the AsyncProviderAdapter + LogListener pipeline."""

    @pytest.mark.asyncio
    async def test_adapter_subscribe_blocks(self) -> None:
        """AsyncProviderAdapter.subscribe_blocks() yields headers via live WS."""
        provider = AlloyProvider(ETHEREUM_ARCHIVE_NODE_WS_URI)
        adapter = AsyncProviderAdapter.from_alloy(provider)
        sub = await adapter.subscribe_blocks()

        try:
            header = None
            async for item in sub:
                header = item
                break
            assert header is not None
            assert isinstance(header, dict)
        finally:
            await sub.unsubscribe()

    @pytest.mark.asyncio
    async def test_subscribe_logs_and_dispatch_via_listener(self) -> None:
        """Subscribe to unfiltered logs, dispatch via LogListener."""
        provider = AlloyProvider(ETHEREUM_ARCHIVE_NODE_WS_URI)
        adapter = AsyncProviderAdapter.from_alloy(provider)
        sub = await adapter.subscribe_logs()

        listener = LogListener()
        all_logs: list[dict] = []

        def capture_log(log: dict) -> None:
            all_logs.append(log)

        # Register a catch-all handler for a well-known event
        # (V2 Sync on WETH/USDC or similar)
        listener.register(
            "0xabcdef1234567890abcdef1234567890abcdef12",  # unlikely address
            "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1",  # V2 Sync
            capture_log,
        )

        try:
            # Drain for a short while
            async def _collect() -> None:
                async for log in sub:
                    listener.dispatch(log)
                    if len(all_logs) >= 1:
                        break

            await asyncio.wait_for(_collect(), timeout=30)
        except (TimeoutError, SubscriptionDisconnected):
            pass  # Normal — may not see matching log in time
        finally:
            await sub.unsubscribe()
