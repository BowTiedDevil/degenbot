"""Tests for the unified tick data fetcher factory.

Verifies that ``make_tick_data_fetcher`` produces a callback that RETURNS the
fetched word's tick data (ADR-005 sparse-map parity, slice 3b return-data
contract) for both V3 and V4 type variants.

The fetcher no longer writes back via ``pool.update_tick_data`` — the Rust
``simulate_swap_with_fetch`` loop merges the returned data itself (holding the
write lock), so a write-back fetcher would re-enter that lock and deadlock.
The Python companion's sparse-write-back sites (``_apply_fetched_tick_word``)
also consume the returned data explicitly. These tests assert on the RETURNED
dict, not on pool-state side-effects.
"""

from typing import Any
from unittest.mock import MagicMock

import eth_abi.abi
import pytest

from degenbot.builders.tick_data_fetcher import TickDataTypes, make_tick_data_fetcher
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool


class FakePoolIO:
    """A minimal PoolIO double for tick data fetcher tests.

    Records all call() invocations so tests can assert on them,
    and returns pre-configured responses based on calldata matching.
    """

    def __init__(self):
        self.call_log: list[tuple[str, bytes, int | None]] = []
        self._responses: list[tuple[bytes, bytes]] = []  # (calldata_prefix, response_bytes)
        self._call_side_effect: Any = None

    def add_response(self, calldata: bytes, response: bytes) -> None:
        """Pre-configure a response for a specific calldata payload."""
        self._responses.append((calldata, response))

    def set_call_side_effect(self, fn: Any) -> None:
        """Set a side-effect function for call(), overriding add_response."""
        self._call_side_effect = fn

    def call(self, to: str, data: bytes, block: int | None = None) -> bytes:
        """PoolIO.call — log and return configured response."""
        self.call_log.append((to, data, block))
        if self._call_side_effect is not None:
            return self._call_side_effect(to=to, data=data, block=block)
        for calldata, response in self._responses:
            if data == calldata:
                return response
        msg = f"Unexpected call: to={to}, data={data!r}"
        raise ValueError(msg)

    # Unused PoolIO methods — stubs
    def call_raw(self, tx, block=None) -> bytes:
        msg = "call_raw not expected in tick data fetcher tests"
        raise NotImplementedError(msg)

    def get_block_number(self) -> int:
        msg = "get_block_number not expected in tick data fetcher tests"
        raise NotImplementedError(msg)

    def get_block(self, block_identifier) -> None:
        msg = "get_block not expected in tick data fetcher tests"
        raise NotImplementedError(msg)

    def get_block_timestamp(self, block=None) -> int:
        msg = "get_block_timestamp not expected in tick data fetcher tests"
        raise NotImplementedError(msg)

    def get_code(self, address, block=None) -> bytes:
        msg = "get_code not expected in tick data fetcher tests"
        raise NotImplementedError(msg)

    def get_balance(self, address, block=None) -> int:
        msg = "get_balance not expected in tick data fetcher tests"
        raise NotImplementedError(msg)


def _make_fake_pool(tick_spacing: int = 60) -> Any:
    """Build a fake pool exposing only address + tick_spacing.

    The return-data fetcher no longer reads/writes the pool's tick maps — it
    only needs ``address`` + ``tick_spacing`` to encode the per-tick RPCs.
    """
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
    """Encode a tickBitmap(int16) call."""
    return encode_function_calldata("tickBitmap(int16)", [word_position])


def _encode_ticks_calldata(tick: int) -> bytes:
    """Encode a ticks(int24) call for the given tick value."""
    return encode_function_calldata("ticks(int24)", [tick])


class TestNonZeroBitmapReturnsActiveTicks:
    """When the bitmap value is non-zero, the fetcher should RETURN a dict
    of the word's active ticks as ``(gross, net, block)`` tuples.
    """

    @pytest.mark.parametrize("types", [V3_TYPES, V4_TYPES])
    def test_returns_active_ticks_on_nonzero_bitmap(self, types):
        pool = _make_fake_pool()

        # Bitmap value 7 = bits 0,1,2 set
        # Ticks at positions (3<<8 + 0)*60, (3<<8 + 1)*60, (3<<8 + 2)*60
        bitmap_value = 7

        io = FakePoolIO()
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
                tick_response = eth_abi.abi.encode(
                    types=types.tick_struct_types,
                    args=[500, 100],
                )
            io.add_response(_encode_ticks_calldata(tick), tick_response)

        fetcher = make_tick_data_fetcher(
            pool_lookup=lambda _: pool,
            io=io,
            types=types,
        )

        result = fetcher(word_position=3, block_number=200)

        # The fetcher RETURNS the word's active ticks as (gross, net, block).
        assert result is not None
        assert len(result) == 3
        for i in range(3):
            tick = ((3 << 8) + i) * 60
            assert tick in result
            assert result[tick] == (500, 100, 200)


class TestZeroBitmapReturnsEmptyDict:
    """When the bitmap value is zero, the fetcher should RETURN an empty dict
    (the word is known-but-empty — an all-zero bitmap word).
    """

    @pytest.mark.parametrize("types", [V3_TYPES, V4_TYPES])
    def test_returns_empty_dict_on_zero_bitmap(self, types):
        pool = _make_fake_pool()

        io = FakePoolIO()
        # Return bitmap value 0
        io.add_response(
            _encode_bitmap_calldata(5),
            eth_abi.abi.encode(types=["uint256"], args=[0]),
        )

        fetcher = make_tick_data_fetcher(
            pool_lookup=lambda _: pool,
            io=io,
            types=types,
        )

        result = fetcher(word_position=5, block_number=200)

        # An all-zero bitmap word → empty dict (the Rust merge records the word
        # as known with no initialized ticks).
        assert result == {}

        # Only 1 call should have been made (the bitmap call — no tick fetches).
        assert len(io.call_log) == 1


class TestPoolNotFound:
    """When pool_lookup returns None, the fetcher should return None."""

    @pytest.mark.parametrize("types", [V3_TYPES, V4_TYPES])
    def test_returns_none_when_pool_missing(self, types):
        io = FakePoolIO()

        fetcher = make_tick_data_fetcher(
            pool_lookup=lambda _: None,
            io=io,
            types=types,
        )

        assert fetcher(word_position=3, block_number=200) is None
        assert len(io.call_log) == 0


class TestBitmapFetchRaises:
    """When the bitmap RPC call raises, the fetcher should return None
    (the Rust loop treats this as a fetch failure).
    """

    @pytest.mark.parametrize("types", [V3_TYPES, V4_TYPES])
    def test_returns_none_on_fetch_error(self, types):
        pool = _make_fake_pool()

        io = FakePoolIO()
        # All calls raise
        io.set_call_side_effect(lambda **_: (_ for _ in ()).throw(Exception("RPC error")))

        fetcher = make_tick_data_fetcher(
            pool_lookup=lambda _: pool,
            io=io,
            types=types,
        )

        assert fetcher(word_position=3, block_number=200) is None
