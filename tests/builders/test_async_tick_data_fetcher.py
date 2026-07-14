"""Tests for the async tick-data fetcher bridge.

``AsyncBot``-built sparse V3/V4 pools need a tick-word fetcher so the Rust
``TickWordFetcher`` seam (sync) can issue async ``AsyncPoolIO.call`` RPCs to
backfill neighbouring tick words on a crossing swap (feature parity with the
sync ``Bot``). ``make_tick_data_fetcher_from_async_io`` wraps an async IO in
a sync ``.call`` adapter backed by a daemon-thread event loop and delegates
to the existing sync ``make_tick_data_fetcher``, preserving the return-data
contract (RETURNS the word's ticks; never writes back).
"""

from __future__ import annotations

from typing import Any
from unittest.mock import MagicMock

import eth_abi.abi
import pytest
from hexbytes import HexBytes

from degenbot.builders.tick_data_fetcher import (
    TickDataTypes,
    make_tick_data_fetcher_from_async_io,
)
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool


class FakeAsyncPoolIO:
    """Minimal async PoolIO double: ``call`` is an ``async def``.

    Mirrors ``FakePoolIO`` in ``test_tick_data_fetcher.py`` but async, so the
    bridge is forced to run the coroutine to completion synchronously.
    """

    def __init__(self) -> None:
        self.call_log: list[tuple[str, bytes, int | None]] = []
        self._responses: list[tuple[bytes, bytes]] = []
        self._call_side_effect: Any = None

    def add_response(self, calldata: bytes, response: bytes) -> None:
        self._responses.append((calldata, response))

    def set_call_side_effect(self, fn: Any) -> None:
        self._call_side_effect = fn

    async def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.call_log.append((to, data, block))
        if self._call_side_effect is not None:
            return self._call_side_effect(to=to, data=data, block=block)
        for calldata, response in self._responses:
            if data == calldata:
                return HexBytes(response)
        msg = f"Unexpected call: to={to}, data={data!r}"
        raise ValueError(msg)


def _make_fake_pool(tick_spacing: int = 60) -> Any:
    pool = MagicMock()
    pool.address = "0x" + "00" * 20
    pool.tick_spacing = tick_spacing
    return pool


V3_TYPES = TickDataTypes(
    bitmap_at_word=BitmapAtWord,
    liquidity_at_tick=LiquidityAtTick,
    tick_struct_types=UniswapV3Pool.TICK_STRUCT_TYPES,
)
V4_TYPES = TickDataTypes(
    bitmap_at_word=BitmapAtWord,
    liquidity_at_tick=LiquidityAtTick,
    tick_struct_types=("uint128", "int128"),
)


def _encode_bitmap_calldata(word_position: int) -> bytes:
    return encode_function_calldata("tickBitmap(int16)", [word_position])


def _encode_ticks_calldata(tick: int) -> bytes:
    return encode_function_calldata("ticks(int24)", [tick])


class TestAsyncBridgeReturnsActiveTicks:
    """A non-zero bitmap word yields the word's active ticks as (gross,net,block)."""

    @pytest.mark.parametrize("types", [V3_TYPES, V4_TYPES])
    def test_returns_active_ticks_on_nonzero_bitmap(self, types):
        pool = _make_fake_pool()
        bitmap_value = 7  # bits 0,1,2

        io = FakeAsyncPoolIO()
        io.add_response(
            _encode_bitmap_calldata(3),
            eth_abi.abi.encode(types=["uint256"], args=[bitmap_value]),
        )
        for i in range(3):
            tick = ((3 << 8) + i) * 60
            if len(types.tick_struct_types) == 8:
                tick_response = eth_abi.abi.encode(
                    types=types.tick_struct_types,
                    args=[500, 100, 0, 0, 0, 0, 0, False],
                )
            else:
                tick_response = eth_abi.abi.encode(types=types.tick_struct_types, args=[500, 100])
            io.add_response(_encode_ticks_calldata(tick), tick_response)

        fetcher = make_tick_data_fetcher_from_async_io(
            pool_lookup=lambda _: pool,
            async_io=io,
            types=types,
        )

        result = fetcher(word_position=3, block_number=200)

        assert result is not None
        assert len(result) == 3
        for i in range(3):
            tick = ((3 << 8) + i) * 60
            assert result[tick] == (500, 100, 200)


class TestAsyncBridgeZeroBitmap:
    """An all-zero bitmap word yields an empty dict (word known, no ticks)."""

    @pytest.mark.parametrize("types", [V3_TYPES, V4_TYPES])
    def test_returns_empty_dict_on_zero_bitmap(self, types):
        pool = _make_fake_pool()
        io = FakeAsyncPoolIO()
        io.add_response(
            _encode_bitmap_calldata(5),
            eth_abi.abi.encode(["uint256"], [0]),
        )

        fetcher = make_tick_data_fetcher_from_async_io(
            pool_lookup=lambda _: pool,
            async_io=io,
            types=types,
        )

        assert fetcher(word_position=5, block_number=200) == {}
        # Only the bitmap call — no tick fetches.
        assert len(io.call_log) == 1


class TestAsyncBridgePoolNotFound:
    """When pool_lookup returns None, the fetcher returns None (no RPC)."""

    @pytest.mark.parametrize("types", [V3_TYPES, V4_TYPES])
    def test_returns_none_when_pool_missing(self, types):
        io = FakeAsyncPoolIO()
        fetcher = make_tick_data_fetcher_from_async_io(
            pool_lookup=lambda _: None,
            async_io=io,
            types=types,
        )
        assert fetcher(word_position=3, block_number=200) is None
        assert len(io.call_log) == 0


class TestAsyncBridgeFetchError:
    """When the async RPC raises, the fetcher returns None (fetch failure)."""

    @pytest.mark.parametrize("types", [V3_TYPES, V4_TYPES])
    def test_returns_none_on_fetch_error(self, types):
        pool = _make_fake_pool()
        io = FakeAsyncPoolIO()
        io.set_call_side_effect(lambda **_: (_ for _ in ()).throw(Exception("RPC error")))

        fetcher = make_tick_data_fetcher_from_async_io(
            pool_lookup=lambda _: pool,
            async_io=io,
            types=types,
        )
        assert fetcher(word_position=3, block_number=200) is None


class TestAsyncBridgeFromWithinRunningLoop:
    """The fetcher works when the caller's thread already has a running loop.

    This is the AsyncBot parity scenario: the swap seam is sync, but a caller
    may invoke it from inside ``async def`` code. ``asyncio.run`` would raise
    "cannot be called from a running event loop"; the daemon-loop bridge must
    still succeed.
    """

    @pytest.mark.parametrize("types", [V3_TYPES, V4_TYPES])
    @pytest.mark.asyncio
    async def test_works_inside_running_loop(self, types):
        pool = _make_fake_pool()
        io = FakeAsyncPoolIO()
        io.add_response(
            _encode_bitmap_calldata(3),
            eth_abi.abi.encode(["uint256"], [7]),
        )
        for i in range(3):
            tick = ((3 << 8) + i) * 60
            if len(types.tick_struct_types) == 8:
                tick_response = eth_abi.abi.encode(
                    types=types.tick_struct_types,
                    args=[500, 100, 0, 0, 0, 0, 0, False],
                )
            else:
                tick_response = eth_abi.abi.encode(types=types.tick_struct_types, args=[500, 100])
            io.add_response(_encode_ticks_calldata(tick), tick_response)

        fetcher = make_tick_data_fetcher_from_async_io(
            pool_lookup=lambda _: pool,
            async_io=io,
            types=types,
        )

        # Call the SYNC fetcher from inside a coroutine (running loop).
        result = fetcher(word_position=3, block_number=200)

        assert result is not None
        assert len(result) == 3
        for i in range(3):
            tick = ((3 << 8) + i) * 60
            assert result[tick] == (500, 100, 200)
