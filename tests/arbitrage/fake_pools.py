"""Robust fake V3 pools for testing multi-range arbitrage.

These build on production UniswapV3Pool with full tick data support,
enabling testing of _get_cached_tick_ranges() and multi-range
piecewise-Möbius solving.

Production V3 pools are fully I/O-free when constructed directly —
no RPC calls or provider references needed.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v3_libraries.tick_math import get_sqrt_ratio_at_tick
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from tests.fakes.tokens import FakeToken

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.erc20.erc20 import Erc20Token


@dataclass(frozen=True, slots=True)
class FakeTickInfo:
    """Tick information for fake V3 pools.

    Mirrors LiquidityAtTick but is Hashable for testing.
    """

    liquidity_net: int
    liquidity_gross: int

    def to_liquidity_at_tick(self) -> LiquidityAtTick:
        """Convert to LiquidityAtTick."""
        return LiquidityAtTick(
            liquidity_net=self.liquidity_net,
            liquidity_gross=self.liquidity_gross,
        )


@dataclass
class TickRangeDefinition:
    """Definition of a tick range for building fake pools.

    tick_lower: Lower tick boundary (inclusive)
    tick_upper: Upper tick boundary (exclusive)
    liquidity: Liquidity in this range
    """

    tick_lower: int
    tick_upper: int
    liquidity: int

    def __post_init__(self) -> None:
        """Validate range definition."""
        if self.tick_lower >= self.tick_upper:
            msg = f"tick_lower ({self.tick_lower}) must be < tick_upper ({self.tick_upper})"
            raise ValueError(msg)
        if self.liquidity < 0:
            msg = f"liquidity must be non-negative, got {self.liquidity}"
            raise ValueError(msg)


def build_v3_pool_with_ticks(
    address: ChecksumAddress,
    token0: FakeToken | Erc20Token,
    token1: FakeToken | Erc20Token,
    tick_spacing: int,
    fee: int,
    current_tick: int,
    current_liquidity: int,
    current_sqrt_price_x96: int,
    tick_ranges: list[TickRangeDefinition],
) -> UniswapV3Pool:
    """Build a production UniswapV3Pool with tick data from range definitions.

    Constructs tick_data and tick_bitmap from the specified ranges, then
    passes them to UniswapV3Pool constructor. The pool is fully I/O-free.

    Parameters
    ----------
    address : ChecksumAddress
        Pool address
    token0 : FakeToken | Erc20Token
        Token0
    token1 : FakeToken | Erc20Token
        Token1
    tick_spacing : int
        Tick spacing (e.g., 60 for 0.3% pool)
    fee : int
        Fee in hundredths of bip (e.g., 3000 = 0.3%)
    current_tick : int
        Current tick (must align with tick_spacing)
    current_liquidity : int
        Current liquidity at current_tick
    current_sqrt_price_x96 : int
        Current sqrt price as Q64.96
    tick_ranges : list[TickRangeDefinition]
        Adjacent liquidity ranges defining the pool's liquidity profile

    Returns
    -------
    UniswapV3Pool
        Production V3 pool with tick data populated

    """
    if current_tick % tick_spacing != 0:
        msg = f"current_tick ({current_tick}) must be multiple of tick_spacing ({tick_spacing})"
        raise ValueError(msg)

    tick_data: dict[int, LiquidityAtTick] = {}
    tick_bitmap_raw: dict[int, int] = {}

    # Build tick_data from range definitions
    for range_def in tick_ranges:
        _add_tick_liquidity(tick_data, range_def.tick_lower, range_def.liquidity)
        _add_tick_liquidity(tick_data, range_def.tick_upper, -range_def.liquidity)

    # Build bitmap from initialized ticks
    for tick in tick_data:
        word_pos = tick >> 8
        bit_pos = tick & 0xFF
        if word_pos not in tick_bitmap_raw:
            tick_bitmap_raw[word_pos] = 0
        tick_bitmap_raw[word_pos] |= 1 << bit_pos

    # Convert to proper types
    tick_bitmap_typed: dict[int, BitmapAtWord] = {
        word_pos: BitmapAtWord(bitmap=bitmap, block=0)
        for word_pos, bitmap in tick_bitmap_raw.items()
    }

    return UniswapV3Pool(
        address=address,
        token0=token0,  # type: ignore[arg-type]
        token1=token1,  # type: ignore[arg-type]
        factory="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        fee=fee,
        tick_spacing=tick_spacing,
        sqrt_price_x96=current_sqrt_price_x96,
        tick=current_tick,
        liquidity=current_liquidity,
        tick_bitmap=tick_bitmap_typed,
        tick_data=tick_data,
        state_block=1,
    )


def _add_tick_liquidity(
    tick_data: dict[int, LiquidityAtTick],
    tick: int,
    liquidity_delta: int,
) -> None:
    """Add liquidity delta to a tick's net liquidity."""
    if tick in tick_data:
        existing = tick_data[tick]
        new_net = existing.liquidity_net + liquidity_delta
        new_gross = max(existing.liquidity_gross + abs(liquidity_delta), abs(new_net))
        tick_data[tick] = LiquidityAtTick(
            liquidity_net=new_net,
            liquidity_gross=new_gross,
        )
    else:
        tick_data[tick] = LiquidityAtTick(
            liquidity_net=liquidity_delta,
            liquidity_gross=abs(liquidity_delta),
        )


def create_two_range_v3_pool(
    address: ChecksumAddress,
    token0: FakeToken | Erc20Token,
    token1: FakeToken | Erc20Token,
    current_tick: int,
    lower_liquidity: int,
    upper_liquidity: int,
    tick_spacing: int = 60,
    fee: int = 3000,
) -> UniswapV3Pool:
    """Create a production UniswapV3Pool with two adjacent liquidity ranges.

    This is a convenience factory for the most common test scenario:
    - Range 1: [tick_lower, 0) with lower_liquidity
    - Range 2: [0, tick_upper) with upper_liquidity

    Parameters
    ----------
    address : ChecksumAddress
        Pool address
    token0 : FakeToken | Erc20Token
        Token0
    token1 : FakeToken | Erc20Token
        Token1
    current_tick : int
        Current tick (determines which range contains current price)
    lower_liquidity : int
        Liquidity in the lower range
    upper_liquidity : int
        Liquidity in the upper range
    tick_spacing : int
        Tick spacing (default 60 for 0.3% pool)
    fee : int
        Fee in hundredths of bip (default 3000 = 0.3%)

    Returns
    -------
    UniswapV3Pool
        Production V3 pool with two adjacent liquidity ranges

    """
    range_width = 3 * tick_spacing
    tick_lower = -range_width
    tick_upper = range_width

    current_liquidity = lower_liquidity if current_tick < 0 else upper_liquidity
    sqrt_price_x96 = get_sqrt_ratio_at_tick(current_tick)

    ranges = [
        TickRangeDefinition(
            tick_lower=tick_lower,
            tick_upper=0,
            liquidity=lower_liquidity,
        ),
        TickRangeDefinition(
            tick_lower=0,
            tick_upper=tick_upper,
            liquidity=upper_liquidity,
        ),
    ]

    return build_v3_pool_with_ticks(
        address=address,
        token0=token0,
        token1=token1,
        tick_spacing=tick_spacing,
        fee=fee,
        current_tick=current_tick,
        current_liquidity=current_liquidity,
        current_sqrt_price_x96=sqrt_price_x96,
        tick_ranges=ranges,
    )
