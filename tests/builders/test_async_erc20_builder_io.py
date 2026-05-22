"""
Tests for AsyncErc20Builder I/O methods (Plan 065).

These verify that AsyncErc20Builder's I/O methods mirror Erc20Builder's
behavior: resolve block → check cache → call via AsyncPoolIO → decode →
update cache.
"""

from __future__ import annotations

import pathlib

import eth_abi.abi
import pytest
from hexbytes import HexBytes

from degenbot.builders.async_erc20_builder import AsyncErc20Builder
from degenbot.builders.pool_io import AsyncPoolIO
from degenbot.database.operations import get_scoped_sqlite_session
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.erc20.erc20 import Erc20Token
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.registry import TokenRegistry


def _make_weth() -> Erc20Token:
    return Erc20Token(
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        chain_id=1,
        name="Wrapped Ether",
        symbol="WETH",
        decimals=18,
    )


class FakeAsyncProvider:
    """Minimal fake async provider for AsyncPoolIO delegation."""

    def __init__(self) -> None:
        self._responses: dict[str, HexBytes] = {}
        self.block_number = 18_000_000
        self.balance = 10**18

    def set_response(self, data_hex: str, response: HexBytes) -> None:
        self._responses[data_hex] = response

    async def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:  # noqa: ARG002
        key = data.hex()
        if key in self._responses:
            return self._responses[key]
        msg = f"No mock response for data={data.hex()}"
        raise ValueError(msg)

    async def get_block_number(self) -> int:
        return self.block_number

    async def get_balance(self, address: str, block: int | None = None) -> int:  # noqa: ARG002
        return self.balance


class TestAsyncErc20BuilderGetTokenBalance:
    """AsyncErc20Builder.get_token_balance mirrors Erc20Builder."""

    @pytest.mark.asyncio
    async def test_returns_balance_from_chain_on_cache_miss(self) -> None:
        """When the cache is empty, fetches balance from chain and caches it."""
        token = _make_weth()
        holder = "0x" + "11" * 20

        provider = FakeAsyncProvider()
        holder_checksum = holder  # already in correct format
        balance_calldata = encode_function_calldata(
            "balanceOf(address)", [holder_checksum]
        )
        expected_balance = 5 * 10**18
        provider.set_response(
            balance_calldata.hex(),
            HexBytes(eth_abi.abi.encode(types=["uint256"], args=[expected_balance])),
        )
        io = AsyncPoolIO(provider)

        builder = AsyncErc20Builder(
            default_chain_id=1,
            db=DatabaseSessionManager(get_scoped_sqlite_session(pathlib.Path(":memory:"))),
            tokens=TokenRegistry(),
        )

        result = await builder.get_token_balance(
            token, holder, block_identifier=None, io=io
        )
        assert result == expected_balance
        # Should be cached now
        assert token.get_cached_balance(holder, provider.block_number) == expected_balance

    @pytest.mark.asyncio
    async def test_returns_cached_balance_without_chain_call(self) -> None:
        """When the cache has a value, returns it without calling the chain."""
        token = _make_weth()
        holder = "0x" + "11" * 20
        cached_balance = 3 * 10**18
        token.set_cached_balance(holder, block_number=100, balance=cached_balance)

        provider = FakeAsyncProvider()
        # No responses set — if provider.call is triggered, it will raise
        io = AsyncPoolIO(provider)

        builder = AsyncErc20Builder(
            default_chain_id=1,
            db=DatabaseSessionManager(get_scoped_sqlite_session(pathlib.Path(":memory:"))),
            tokens=TokenRegistry(),
        )

        result = await builder.get_token_balance(
            token, holder, block_identifier=100, io=io
        )
        assert result == cached_balance

    @pytest.mark.asyncio
    async def test_resolves_block_identifier_to_number(self) -> None:
        """When block_identifier is an int, uses it directly."""
        token = _make_weth()
        holder = "0x" + "22" * 20
        block_number = 99

        provider = FakeAsyncProvider()
        balance_calldata = encode_function_calldata(
            "balanceOf(address)", [holder]
        )
        expected_balance = 7 * 10**18
        provider.set_response(
            balance_calldata.hex(),
            HexBytes(eth_abi.abi.encode(types=["uint256"], args=[expected_balance])),
        )
        io = AsyncPoolIO(provider)

        builder = AsyncErc20Builder(
            default_chain_id=1,
            db=DatabaseSessionManager(get_scoped_sqlite_session(pathlib.Path(":memory:"))),
            tokens=TokenRegistry(),
        )

        result = await builder.get_token_balance(
            token, holder, block_identifier=block_number, io=io
        )
        assert result == expected_balance
        # Cached at the explicit block number
        assert token.get_cached_balance(holder, block_number) == expected_balance


class TestAsyncErc20BuilderGetEtherBalance:
    """AsyncErc20Builder.get_ether_balance delegates to io.get_balance."""

    @pytest.mark.asyncio
    async def test_returns_ether_balance_from_io(self) -> None:
        """get_ether_balance fetches ETH balance from io.get_balance."""
        provider = FakeAsyncProvider()
        provider.balance = 42 * 10**18
        io = AsyncPoolIO(provider)

        builder = AsyncErc20Builder(
            default_chain_id=1,
            db=DatabaseSessionManager(get_scoped_sqlite_session(pathlib.Path(":memory:"))),
            tokens=TokenRegistry(),
        )

        holder = "0x" + "33" * 20
        result = await builder.get_ether_balance(1, holder, block_identifier=None, io=io)
        assert result == 42 * 10**18

    @pytest.mark.asyncio
    async def test_get_ether_balance_with_explicit_block(self) -> None:
        """get_ether_balance passes block identifier through."""
        provider = FakeAsyncProvider()
        provider.balance = 99 * 10**18
        # Override get_balance to track the block param
        tracked_block: int | None = None
        original_get_balance = provider.get_balance

        async def tracking_get_balance(address: str, block: int | None = None) -> int:
            nonlocal tracked_block
            tracked_block = block
            return await original_get_balance(address, block=block)

        provider.get_balance = tracking_get_balance
        io = AsyncPoolIO(provider)

        builder = AsyncErc20Builder(
            default_chain_id=1,
            db=DatabaseSessionManager(get_scoped_sqlite_session(pathlib.Path(":memory:"))),
            tokens=TokenRegistry(),
        )

        holder = "0x" + "33" * 20
        result = await builder.get_ether_balance(1, holder, block_identifier=50, io=io)
        assert result == 99 * 10**18
        assert tracked_block == 50


class TestAsyncErc20BuilderGetTokenApproval:
    """AsyncErc20Builder.get_token_approval mirrors Erc20Builder."""

    @pytest.mark.asyncio
    async def test_returns_approval_from_chain_on_cache_miss(self) -> None:
        """When the cache is empty, fetches approval from chain and caches it."""
        token = _make_weth()
        owner = "0x" + "11" * 20
        spender = "0x" + "22" * 20

        provider = FakeAsyncProvider()
        approval_calldata = encode_function_calldata(
            "allowance(address,address)", [owner, spender]
        )
        expected_approval = 500000000
        provider.set_response(
            approval_calldata.hex(),
            HexBytes(eth_abi.abi.encode(types=["uint256"], args=[expected_approval])),
        )
        io = AsyncPoolIO(provider)

        builder = AsyncErc20Builder(
            default_chain_id=1,
            db=DatabaseSessionManager(get_scoped_sqlite_session(pathlib.Path(":memory:"))),
            tokens=TokenRegistry(),
        )

        result = await builder.get_token_approval(
            token, owner, spender, block_identifier=None, io=io
        )
        assert result == expected_approval
        assert token.get_cached_approval(provider.block_number, owner, spender) == expected_approval

    @pytest.mark.asyncio
    async def test_returns_cached_approval_without_chain_call(self) -> None:
        """When the cache has a value, returns it without calling the chain."""
        token = _make_weth()
        owner = "0x" + "11" * 20
        spender = "0x" + "22" * 20
        cached_approval = 42
        token.set_cached_approval(block_number=100, owner=owner, spender=spender, amount=cached_approval)

        provider = FakeAsyncProvider()
        io = AsyncPoolIO(provider)

        builder = AsyncErc20Builder(
            default_chain_id=1,
            db=DatabaseSessionManager(get_scoped_sqlite_session(pathlib.Path(":memory:"))),
            tokens=TokenRegistry(),
        )

        result = await builder.get_token_approval(
            token, owner, spender, block_identifier=100, io=io
        )
        assert result == cached_approval


class TestAsyncErc20BuilderGetTokenTotalSupply:
    """AsyncErc20Builder.get_token_total_supply mirrors Erc20Builder."""

    @pytest.mark.asyncio
    async def test_returns_total_supply_from_chain_on_cache_miss(self) -> None:
        """When the cache is empty, fetches total supply from chain and caches it."""
        token = _make_weth()

        provider = FakeAsyncProvider()
        total_supply_calldata = encode_function_calldata("totalSupply()", None)
        expected_total_supply = 10**27
        provider.set_response(
            total_supply_calldata.hex(),
            HexBytes(eth_abi.abi.encode(types=["uint256"], args=[expected_total_supply])),
        )
        io = AsyncPoolIO(provider)

        builder = AsyncErc20Builder(
            default_chain_id=1,
            db=DatabaseSessionManager(get_scoped_sqlite_session(pathlib.Path(":memory:"))),
            tokens=TokenRegistry(),
        )

        result = await builder.get_token_total_supply(
            token, block_identifier=None, io=io
        )
        assert result == expected_total_supply
        assert token.get_cached_total_supply(provider.block_number) == expected_total_supply

    @pytest.mark.asyncio
    async def test_returns_cached_total_supply_without_chain_call(self) -> None:
        """When the cache has a value, returns it without calling the chain."""
        token = _make_weth()
        cached_supply = 10**26
        token.set_cached_total_supply(block_number=100, total_supply=cached_supply)

        provider = FakeAsyncProvider()
        io = AsyncPoolIO(provider)

        builder = AsyncErc20Builder(
            default_chain_id=1,
            db=DatabaseSessionManager(get_scoped_sqlite_session(pathlib.Path(":memory:"))),
            tokens=TokenRegistry(),
        )

        result = await builder.get_token_total_supply(
            token, block_identifier=100, io=io
        )
        assert result == cached_supply
