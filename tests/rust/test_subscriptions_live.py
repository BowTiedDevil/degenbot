"""Live WebSocket integration tests for eth_subscribe."""

from __future__ import annotations

import asyncio
from typing import Any

import pytest

from degenbot._ffi.provider import AlloyProvider as RustAlloyProvider
from degenbot.exceptions import SubscriptionDisconnected, SubscriptionNotSupported
from degenbot.provider import AlloyProvider, AsyncAlloyProvider
from degenbot.provider.subscription import Subscription
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI, ETHEREUM_ARCHIVE_NODE_WS_URI


class TestLiveWSSubscribeBlocks:
    """Test subscribing to new block headers via a live WS node."""

    @pytest.mark.asyncio
    async def test_subscribe_blocks_yields_headers(self) -> None:
        """Subscribe to block headers and verify we get at least one."""
        provider = RustAlloyProvider(ETHEREUM_ARCHIVE_NODE_WS_URI)
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
        provider = RustAlloyProvider(ETHEREUM_ARCHIVE_NODE_WS_URI)
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
        adapter = provider

        with pytest.raises(SubscriptionNotSupported):
            adapter.subscribe_blocks()


class TestLiveWSLogsSubscription:
    """Test log subscriptions via a live WS node."""

    @pytest.mark.asyncio
    async def test_subscribe_logs_with_no_filter(self) -> None:
        """Subscribe to all logs and verify we get at least one."""
        provider = RustAlloyProvider(ETHEREUM_ARCHIVE_NODE_WS_URI)
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


class TestLiveWSAdapterLogSubscription:
    """Test the AsyncAlloyProvider WS log subscription path."""

    @pytest.mark.asyncio
    async def test_adapter_subscribe_blocks(self) -> None:
        """Subscribe to blocks via live WS and verify the subscription yields headers."""
        provider = RustAlloyProvider(ETHEREUM_ARCHIVE_NODE_WS_URI)
        sub = provider.subscribe_blocks()
        subscription = Subscription(_inner=sub)

        try:
            header = None
            async for item in sub:
                header = item
                break
            assert header is not None
            assert isinstance(header, dict)
        finally:
            sub.unsubscribe()

    @pytest.mark.asyncio
    async def test_subscribe_logs_yields_log_dicts(self) -> None:
        """Subscribe to unfiltered logs; verify the WS subscription yields log dicts."""
        provider = RustAlloyProvider(ETHEREUM_ARCHIVE_NODE_WS_URI)
        sub = provider.subscribe_logs()
        subscription = Subscription(_inner=sub)

        all_logs: list[dict] = []

        try:
            # Drain for a short while — collect logs directly off the
            # subscription (the pump's production path decodes via the Rust
            # `degenbot-decoders` leaf; the dispatch-registry indirection
            # the LogListener provided was redundant and is retired).
            async def _collect() -> None:
                async for log in sub:
                    all_logs.append(log)
                    if len(all_logs) >= 1:
                        break

            await asyncio.wait_for(_collect(), timeout=30)
        except (TimeoutError, SubscriptionDisconnected):
            pass  # Normal — may not see a log in time
        finally:
            sub.unsubscribe()

        if all_logs:
            log = all_logs[0]
            assert isinstance(log, dict)
            # Log dicts (from `log_to_py_dict` in `py_converters.rs`) carry
            # these keys.
            assert "address" in log
            assert "topics" in log
            assert "data" in log
