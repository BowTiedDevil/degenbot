"""SushiSwap V2/V3 pool implementations."""

from typing import ClassVar

from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool


class SushiswapV2Pool(UniswapV2Pool):
    """SushiswapV2Pool class."""

    variant: ClassVar[str | None] = "sushiswap"


class SushiswapV3Pool(UniswapV3Pool):
    """SushiswapV3Pool class."""

    variant: ClassVar[str | None] = "sushiswap"
