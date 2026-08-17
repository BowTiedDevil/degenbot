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

Post ADR-005 slice-14 collapse: the fetcher calls ``io.fetch_tick_bitmap()``
/ ``io.fetch_tick_data()`` directly (the Python ``io.call()`` parity-gate
fallback is retired), so ``FakePoolIO`` exposes those methods with canned
return values — no ABI encoding needed.
"""

from typing import Any
from unittest.mock import MagicMock

import pytest

from degenbot.builders.tick_data_fetcher import TickDataTypes, make_tick_data_fetcher
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool


class FakePoolIO:
    """Duck-typed RustBotIo stand-in for tick data fetcher tests.

    Exposes the two seam methods ``_fetch_v3`` invokes
    (``fetch_tick_bitmap`` + ``fetch_tick_data``) with canned returns keyed
    by word position / tick. ``raise_on_fetch`` forces the bitmap fetch to
    raise (exercising the fetch-failure path).
    """

    def __init__(self) -> None:
        self.bitmap_at_word: dict[int, int] = {}
        self.tick_data: dict[int, tuple[int, int]] = {}
        self.raise_on_fetch: bool = False
        self.bitmap_fetches: int = 0
        self.tick_fetches: int = 0

    def set_bitmap(self, word_position: int, value: int) -> None:
        self.bitmap_at_word[word_position] = value

    def set_tick(self, tick: int, gross: int, net: int) -> None:
        self.tick_data[tick] = (gross, net)

    def fetch_tick_bitmap(self, _address: str, word_position: int, block: int | None = None) -> int:
        self.bitmap_fetches += 1
        if self.raise_on_fetch:
            msg = "RPC error"
            raise Exception(msg)  # ruff: ignore[raise-vanilla-class]
        return self.bitmap_at_word.get(word_position, 0)

    def fetch_tick_data(
        self, _address: str, tick: int, block: int | None = None
    ) -> tuple[int, int]:
        self.tick_fetches += 1
        return self.tick_data.get(tick, (0, 0))


def _make_fake_pool(tick_spacing: int = 60) -> Any:
    """Build a fake pool exposing only address + tick_spacing."""
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


class TestNonZeroBitmapReturnsActiveTicks:
    """When the bitmap value is non-zero, the fetcher should RETURN a dict
    of the word's active ticks as ``(gross, net, block)`` tuples.
    """

    @pytest.mark.parametrize("types", [V3_TYPES, V4_TYPES])
    def test_returns_active_ticks_on_nonzero_bitmap(self, types):
        pool = _make_fake_pool()

        # Bitmap value 7 = bits 0,1,2 set
        # Ticks at positions (3<<8 + 0)*60, (3<<8 + 1)*60, (3<<8 + 2)*60
        io = FakePoolIO()
        io.set_bitmap(3, 7)
        for i in range(3):
            tick = ((3 << 8) + i) * 60
            io.set_tick(tick, 500, 100)

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
        io.set_bitmap(5, 0)

        fetcher = make_tick_data_fetcher(
            pool_lookup=lambda _: pool,
            io=io,
            types=types,
        )

        result = fetcher(word_position=5, block_number=200)

        # An all-zero bitmap word → empty dict (the Rust merge records the word
        # as known with no initialized ticks).
        assert result == {}

        # Only 1 fetch should have been made (the bitmap call — no tick fetches).
        assert io.bitmap_fetches == 1
        assert io.tick_fetches == 0


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
        assert io.bitmap_fetches == 0
        assert io.tick_fetches == 0


class TestBitmapFetchRaises:
    """When the bitmap RPC call raises, the fetcher should return None
    (the Rust loop treats this as a fetch failure).
    """

    @pytest.mark.parametrize("types", [V3_TYPES, V4_TYPES])
    def test_returns_none_on_fetch_error(self, types):
        pool = _make_fake_pool()

        io = FakePoolIO()
        io.raise_on_fetch = True

        fetcher = make_tick_data_fetcher(
            pool_lookup=lambda _: pool,
            io=io,
            types=types,
        )

        assert fetcher(word_position=3, block_number=200) is None
