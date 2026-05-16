from fractions import Fraction
from typing import ClassVar

from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool


class PancakeswapV2Pool(UniswapV2Pool):
    variant: ClassVar[str | None] = "pancakeswap"

    FEE = Fraction(25, 10000)
    RESERVES_STRUCT_TYPES = ("uint112", "uint112", "uint32")


class PancakeswapV3Pool(UniswapV3Pool):
    variant: ClassVar[str | None] = "pancakeswap"

    SLOT0_STRUCT_TYPES = (
        "uint160",
        "int24",
        "uint16",
        "uint16",
        "uint16",
        "uint32",
        "bool",
    )
