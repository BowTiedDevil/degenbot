from fractions import Fraction

from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool


class PancakeV2Pool(UniswapV2Pool):
    FEE = Fraction(25, 10000)
    RESERVES_STRUCT_TYPES = (
        "uint112",
        "uint112",
        "uint32",
    )  # type:ignore[assignment]


class PancakeV3Pool(UniswapV3Pool):
    SLOT0_STRUCT_TYPES = (
        "uint160",
        "int24",
        "uint16",
        "uint16",
        "uint16",
        "uint32",
        "bool",
    )
