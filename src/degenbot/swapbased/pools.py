"""SwapBased V2 pool implementation."""

from typing import ClassVar

from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool


class SwapbasedV2Pool(UniswapV2Pool):
    """SwapbasedV2Pool class."""

    variant: ClassVar[str | None] = "swapbased"
