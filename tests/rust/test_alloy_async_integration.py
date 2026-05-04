"""
Async integration tests for the Alloy-based Ethereum RPC provider.

These tests use the Rust async provider with proper tokio runtime support via
pyo3-async-runtimes.
"""

import pytest
from degenbot.degenbot_rs import AsyncAlloyProvider
from hexbytes import HexBytes

from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI

WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"


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
        logs = await provider.get_logs(
            from_block=18000000,
            to_block=18000010,
            addresses=["0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"],
        )
        assert isinstance(logs, list)
        assert len(logs) > 0

    async def test_async_provider_rpc_url(self):
        """Test async provider exposes rpc_url getter."""
        provider = await AsyncAlloyProvider.create(ETHEREUM_ARCHIVE_NODE_HTTP_URI)
        assert provider.rpc_url == ETHEREUM_ARCHIVE_NODE_HTTP_URI

    async def test_async_get_gas_price_returns_int(self):
        """Async get_gas_price should return int (matches sync return type)."""
        provider = await AsyncAlloyProvider.create(ETHEREUM_ARCHIVE_NODE_HTTP_URI)
        result = await provider.get_gas_price()
        assert isinstance(result, int), f"Expected int, got {type(result)}"
        assert result >= 0

    async def test_async_call_returns_hexbytes(self):
        """Async call should return HexBytes."""
        provider = await AsyncAlloyProvider.create(ETHEREUM_ARCHIVE_NODE_HTTP_URI)
        # totalSupply() selector
        result = await provider.call(
            to=WETH_ADDRESS,
            data=bytes.fromhex("18160ddd"),
        )
        assert isinstance(result, HexBytes)
        assert len(result) == 32

    async def test_async_get_code_returns_hexbytes(self):
        """Async get_code should return HexBytes."""
        provider = await AsyncAlloyProvider.create(ETHEREUM_ARCHIVE_NODE_HTTP_URI)
        code = await provider.get_code(WETH_ADDRESS)
        assert isinstance(code, HexBytes)
        assert len(code) > 0

    async def test_async_get_balance_of(self):
        """Async eth_call to balanceOf should decode correctly."""
        provider = await AsyncAlloyProvider.create(ETHEREUM_ARCHIVE_NODE_HTTP_URI)
        # balanceOf(address) selector + WETH address padded to 32 bytes
        calldata = bytes.fromhex(
            "70a08231"
            + "000000000000000000000000C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
        )
        result = await provider.call(
            to=WETH_ADDRESS,
            data=calldata,
        )
        assert isinstance(result, HexBytes)
        # Should be able to decode as uint256
        balance = int.from_bytes(result, "big")
        assert balance >= 0
