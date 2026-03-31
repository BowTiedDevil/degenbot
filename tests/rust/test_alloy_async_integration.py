"""
Async integration tests for the Alloy-based Ethereum RPC provider.

These tests use the async provider with proper tokio runtime support via pyo3-async-runtimes.
"""

import pytest

from degenbot.provider.async_provider import AsyncAlloyProvider
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI


@pytest.mark.asyncio
class TestAsyncProviderWithLiveConnection:
    """Async tests that require a live RPC connection."""

    async def test_async_get_block_number(self):
        """Test fetching current block number asynchronously from live RPC."""
        provider = await AsyncAlloyProvider.create(ETHEREUM_ARCHIVE_NODE_HTTP_URI)
        block_number = await provider.get_block_number()
        assert isinstance(block_number, int)
        assert block_number > 0

    async def test_async_get_chain_id(self):
        """Test fetching chain ID asynchronously from live RPC."""
        provider = await AsyncAlloyProvider.create(ETHEREUM_ARCHIVE_NODE_HTTP_URI)
        chain_id = await provider.get_chain_id()
        assert chain_id == 1  # Ethereum mainnet

    async def test_async_get_logs(self):
        """Test fetching logs with filter asynchronously from live RPC."""
        provider = await AsyncAlloyProvider.create(ETHEREUM_ARCHIVE_NODE_HTTP_URI)
        # WETH contract
        logs = await provider.get_logs(
            from_block=18000000,
            to_block=18000010,
            addresses=["0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"],
        )
        assert isinstance(logs, list)
        # Should have some logs for WETH in that block range
        assert len(logs) > 0

    async def test_async_provider_properties(self):
        """Test async provider property access."""
        provider = await AsyncAlloyProvider.create(ETHEREUM_ARCHIVE_NODE_HTTP_URI)
        assert provider.rpc_url == ETHEREUM_ARCHIVE_NODE_HTTP_URI
