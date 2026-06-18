"""SwapBased V2 pool implementation."""

from typing import ClassVar

from degenbot.uniswap.liquidity_pool import LiquidityPool


class SwapbasedV2Pool(LiquidityPool):
    """SwapbasedV2Pool class."""

    variant: ClassVar[str | None] = "swapbased"
