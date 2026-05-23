"""Tests for subscription support mixins.

Verifies that SyncSubscriptionSupport and AsyncSubscriptionSupport correctly
raise SubscriptionNotSupported for all subscribe methods, and that adapters
inheriting these mixins provide the correct transport/rpc_url in the exception.
"""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest

from degenbot.exceptions import SubscriptionNotSupported
from degenbot.provider.async_adapter import AsyncSubscriptionSupport, _AsyncWeb3Adapter
from degenbot.provider.sync_adapter import (
    ProviderAdapter,
    SyncSubscriptionSupport,
    _AlloyAdapter,
    _OfflineAdapter,
    _Web3Adapter,
)
from tests.fakes.web3 import FakeAsyncW3, FakeW3


class TestSyncSubscriptionSupport:
    """Tests for SyncSubscriptionSupport mixin."""

    def test_subscribe_blocks_raises(self) -> None:
        support = SyncSubscriptionSupport()
        with pytest.raises(SubscriptionNotSupported):
            support.subscribe_blocks()

    def test_subscribe_full_blocks_raises(self) -> None:
        support = SyncSubscriptionSupport()
        with pytest.raises(SubscriptionNotSupported):
            support.subscribe_full_blocks()

    def test_subscribe_pending_transactions_raises(self) -> None:
        support = SyncSubscriptionSupport()
        with pytest.raises(SubscriptionNotSupported):
            support.subscribe_pending_transactions()

    def test_subscribe_full_pending_transactions_raises(self) -> None:
        support = SyncSubscriptionSupport()
        with pytest.raises(SubscriptionNotSupported):
            support.subscribe_full_pending_transactions()

    def test_subscribe_logs_raises(self) -> None:
        support = SyncSubscriptionSupport()
        with pytest.raises(SubscriptionNotSupported):
            support.subscribe_logs()

    def test_default_transport_is_sync(self) -> None:
        support = SyncSubscriptionSupport()
        assert support._subscription_transport == "sync"

    def test_default_rpc_url_is_unknown(self) -> None:
        support = SyncSubscriptionSupport()
        assert support._subscription_rpc_url == "unknown"


class TestAsyncSubscriptionSupport:
    """Tests for AsyncSubscriptionSupport mixin."""

    @pytest.mark.asyncio
    async def test_subscribe_blocks_raises(self) -> None:
        support = AsyncSubscriptionSupport()
        with pytest.raises(SubscriptionNotSupported):
            await support.subscribe_blocks()

    @pytest.mark.asyncio
    async def test_subscribe_full_blocks_raises(self) -> None:
        support = AsyncSubscriptionSupport()
        with pytest.raises(SubscriptionNotSupported):
            await support.subscribe_full_blocks()

    @pytest.mark.asyncio
    async def test_subscribe_pending_transactions_raises(self) -> None:
        support = AsyncSubscriptionSupport()
        with pytest.raises(SubscriptionNotSupported):
            await support.subscribe_pending_transactions()

    @pytest.mark.asyncio
    async def test_subscribe_full_pending_transactions_raises(self) -> None:
        support = AsyncSubscriptionSupport()
        with pytest.raises(SubscriptionNotSupported):
            await support.subscribe_full_pending_transactions()

    @pytest.mark.asyncio
    async def test_subscribe_logs_raises(self) -> None:
        support = AsyncSubscriptionSupport()
        with pytest.raises(SubscriptionNotSupported):
            await support.subscribe_logs()

    def test_default_transport_is_async(self) -> None:
        support = AsyncSubscriptionSupport()
        assert support._subscription_transport == "async"

    def test_default_rpc_url_is_unknown(self) -> None:
        support = AsyncSubscriptionSupport()
        assert support._subscription_rpc_url == "unknown"


class TestSyncAdapterSubscriptionStubs:
    """Tests that sync adapters raise SubscriptionNotSupported via the mixin."""

    def test_web3_adapter_subscribe_raises(self) -> None:
        adapter = _Web3Adapter(FakeW3())
        with pytest.raises(SubscriptionNotSupported) as exc_info:
            adapter.subscribe_blocks()
        assert exc_info.value.transport == "web3"

    def test_alloy_adapter_subscribe_raises(self) -> None:
        alloy = MagicMock()
        alloy.rpc_url = "https://example.com"
        adapter = _AlloyAdapter(alloy)
        with pytest.raises(SubscriptionNotSupported) as exc_info:
            adapter.subscribe_blocks()
        assert exc_info.value.rpc_url == "https://example.com"

    def test_offline_adapter_subscribe_raises(self) -> None:
        offline = MagicMock()
        adapter = _OfflineAdapter(offline)
        with pytest.raises(SubscriptionNotSupported) as exc_info:
            adapter.subscribe_blocks()
        assert exc_info.value.transport == "offline"
        assert exc_info.value.rpc_url == "offline"

    def test_provider_adapter_subscribe_raises(self) -> None:
        alloy = MagicMock()
        alloy.rpc_url = "https://example.com"
        adapter = ProviderAdapter.from_alloy(alloy)
        with pytest.raises(SubscriptionNotSupported) as exc_info:
            adapter.subscribe_blocks()
        assert exc_info.value.transport == "sync"


class TestAsyncAdapterSubscriptionStubs:
    """Tests that async adapters raise SubscriptionNotSupported via the mixin."""

    @pytest.mark.asyncio
    async def test_async_web3_adapter_subscribe_raises(self) -> None:
        adapter = _AsyncWeb3Adapter(FakeAsyncW3())
        with pytest.raises(SubscriptionNotSupported) as exc_info:
            await adapter.subscribe_blocks()
        assert exc_info.value.transport == "web3"
