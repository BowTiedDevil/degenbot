"""
Robust fake V3 pools for testing multi-range arbitrage.

These extend the basic MockV3Pool with full tick data support,
enabling testing of _get_cached_tick_ranges() and multi-range
piecewise-Möbius solving.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from degenbot.uniswap.v3_libraries.tick_math import get_sqrt_ratio_at_tick
from degenbot.uniswap.v3_types import (
    UniswapV3LiquidityAtTick,
    UniswapV3PoolState,
)
from tests.arbitrage.mock_pools import MockErc20Token, MockV3Pool

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.types.concrete import Subscriber


@dataclass(frozen=True, slots=True)
class FakeTickInfo:
    """
    Tick information for fake V3 pools.

    Mirrors UniswapV3LiquidityAtTick but is hashable for testing.
    """

    liquidity_net: int
    liquidity_gross: int

    def to_liquidity_at_tick(self) -> UniswapV3LiquidityAtTick:
        """Convert to UniswapV3LiquidityAtTick."""
        return UniswapV3LiquidityAtTick(
            liquidity_net=self.liquidity_net,
            liquidity_gross=self.liquidity_gross,
        )


@dataclass
class TickRangeDefinition:
    """
    Definition of a tick range for building fake pools.

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


class FakeV3PoolWithTicks(MockV3Pool):
    """
    Fake V3 pool with full tick data for multi-range testing.

    Supports _get_cached_tick_ranges() by providing:
    - tick_data: dict mapping tick index to FakeTickInfo
    - tick_bitmap: dict mapping word position to bitmap
    - sparse_liquidity_map: always False (to enable multi-range)

    Example
    -------
    >>> # Create a pool with two adjacent liquidity ranges
    >>> ranges = [
    ...     TickRangeDefinition(tick_lower=-180, tick_upper=0, liquidity=10_000_000),
    ...     TickRangeDefinition(tick_lower=0, tick_upper=180, liquidity=20_000_000),
    ... ]
    >>> pool = FakeV3PoolWithTicks(
    ...     address=pool_address,
    ...     token0=usdc,
    ...     token1=weth,
    ...     tick_spacing=60,
    ...     fee=3000,
    ...     current_tick=-30,  # In first range
    ...     current_liquidity=10_000_000,
    ...     current_sqrt_price_x96=get_sqrt_ratio_at_tick(-30),
    ...     tick_ranges=ranges,
    ... )
    >>> # Now pool.tick_data and pool.tick_bitmap are populated
    >>> # _get_cached_tick_ranges(pool, zero_for_one=True) will work
    """

    def __init__(
        self,
        address: ChecksumAddress,
        token0: MockErc20Token | Erc20Token,
        token1: MockErc20Token | Erc20Token,
        tick_spacing: int,
        fee: int,
        current_tick: int,
        current_liquidity: int,
        current_sqrt_price_x96: int,
        tick_ranges: list[TickRangeDefinition],
    ) -> None:
        """
        Initialize fake pool with tick data.

        Parameters
        ----------
        address : ChecksumAddress
            Pool address
        token0 : MockErc20Token | Erc20Token
            Token0
        token1 : MockErc20Token | Erc20Token
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
        """
        # Validate current_tick aligns with tick_spacing
        if current_tick % tick_spacing != 0:
            msg = f"current_tick ({current_tick}) must be multiple of tick_spacing ({tick_spacing})"
            raise ValueError(msg)

        # Build tick data and bitmap from ranges first
        self.tick_data: dict[int, UniswapV3LiquidityAtTick] = {}
        self.tick_bitmap: dict[int, int] = {}
        self.sparse_liquidity_map = False
        self._tick_ranges = tick_ranges
        self._build_tick_data()

        # Convert tick_bitmap to proper format
        from degenbot.uniswap.v3_types import UniswapV3BitmapAtWord

        tick_bitmap_typed: dict[int, UniswapV3BitmapAtWord] = {}
        for word_pos, bitmap in self.tick_bitmap.items():
            tick_bitmap_typed[word_pos] = UniswapV3BitmapAtWord(
                bitmap=bitmap,
                block=0,
            )

        # Create state with all required fields
        initial_state = UniswapV3PoolState(
            address=address,
            block=0,
            liquidity=current_liquidity,
            sqrt_price_x96=current_sqrt_price_x96,
            tick=current_tick,
            tick_bitmap=tick_bitmap_typed,
            tick_data=self.tick_data,
        )

        # Initialize parent (don't use super().__init__ since that would overwrite state)
        self.address = address
        self.token0 = token0
        self.token1 = token1
        self.tokens = (token0, token1)
        self.tick_spacing = tick_spacing
        self.fee = fee
        self.chain_id = 1
        self.name = f"FakeV3-{address[:10]}"
        self._state = initial_state
        self._subscribers: set[Subscriber] = set()

    @property
    def tick(self) -> int:
        """Current tick (convenience accessor)."""
        return self._state.tick

    def _build_tick_data(self) -> None:
        """
        Build tick_data and tick_bitmap from range definitions.

        For each range, we set liquidity_net at the lower tick (positive)
        and at the upper tick (negative). This matches how real V3 pools
        store liquidity changes.
        """
        for range_def in self._tick_ranges:
            # Lower tick: add liquidity
            self._add_tick_liquidity(
                tick=range_def.tick_lower,
                liquidity_delta=range_def.liquidity,
            )
            # Upper tick: remove liquidity
            self._add_tick_liquidity(
                tick=range_def.tick_upper,
                liquidity_delta=-range_def.liquidity,
            )

        # Build bitmap from initialized ticks
        self._build_tick_bitmap()

    def _add_tick_liquidity(self, tick: int, liquidity_delta: int) -> None:
        """Add liquidity delta to a tick's net liquidity."""
        if tick in self.tick_data:
            existing = self.tick_data[tick]
            new_net = existing.liquidity_net + liquidity_delta
            new_gross = max(existing.liquidity_gross + abs(liquidity_delta), abs(new_net))
            self.tick_data[tick] = UniswapV3LiquidityAtTick(
                liquidity_net=new_net,
                liquidity_gross=new_gross,
            )
        else:
            self.tick_data[tick] = UniswapV3LiquidityAtTick(
                liquidity_net=liquidity_delta,
                liquidity_gross=abs(liquidity_delta),
            )

    def _build_tick_bitmap(self) -> None:
        """
        Build tick_bitmap from initialized ticks.

        The bitmap uses compressed tick indices:
        - word_pos = tick >> 8 (divided by 256)
        - bit_pos = tick % 256 (mod 256)

        This matches how real V3 pools compress tick data.
        """
        for tick in self.tick_data:
            # Compress tick index
            word_pos = tick >> 8
            bit_pos = tick & 0xFF

            if word_pos not in self.tick_bitmap:
                self.tick_bitmap[word_pos] = 0

            # Set bit for this tick
            self.tick_bitmap[word_pos] |= 1 << bit_pos

    def get_tick_liquidity(self, tick: int) -> int:
        """
        Get the liquidity net at a specific tick.

        Returns 0 if tick is not initialized.
        """
        info = self.tick_data.get(tick)
        return info.liquidity_net if info else 0

    def get_current_range(self) -> TickRangeDefinition | None:
        """
        Get the TickRangeDefinition containing current_tick.

        Returns None if current_tick is outside all defined ranges.
        """
        for range_def in self._tick_ranges:
            if range_def.tick_lower <= self.state.tick < range_def.tick_upper:
                return range_def
        return None

    def __repr__(self) -> str:
        return (
            f"FakeV3PoolWithTicks({self.address[:10]}, "
            f"{self.token0.symbol}/{self.token1.symbol}, "
            f"tick={self.state.tick}, "
            f"ranges={len(self._tick_ranges)})"
        )


def create_two_range_v3_pool(
    address: ChecksumAddress,
    token0: MockErc20Token | Erc20Token,
    token1: MockErc20Token | Erc20Token,
    current_tick: int,
    lower_liquidity: int,
    upper_liquidity: int,
    tick_spacing: int = 60,
    fee: int = 3000,
) -> FakeV3PoolWithTicks:
    """
    Create a fake V3 pool with two adjacent liquidity ranges.

    This is a convenience factory for the most common test scenario:
    - Range 1: [tick_lower, 0) with lower_liquidity
    - Range 2: [0, tick_upper) with upper_liquidity

    Parameters
    ----------
    address : ChecksumAddress
        Pool address
    token0 : MockErc20Token | Erc20Token
        Token0
    token1 : MockErc20Token | Erc20Token
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
    FakeV3PoolWithTicks
        Pool with two adjacent liquidity ranges
    """
    # Create symmetric ranges around 0
    range_width = 3 * tick_spacing  # 3 ticks per range
    tick_lower = -range_width
    tick_upper = range_width

    # Determine current liquidity based on which range contains current_tick
    current_liquidity = lower_liquidity if current_tick < 0 else upper_liquidity

    # Get sqrt price for current tick
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

    return FakeV3PoolWithTicks(
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
