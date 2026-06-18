"""SushiSwap V2/V3 pool implementations."""

from typing import ClassVar

from degenbot.uniswap.liquidity_pool import LiquidityPool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool


class SushiswapV2Pool(LiquidityPool):
    """SushiswapV2Pool class."""

    variant: ClassVar[str | None] = "sushiswap"


class SushiswapV3Pool(UniswapV3Pool):
    """SushiswapV3Pool class."""

    variant: ClassVar[str | None] = "sushiswap"
