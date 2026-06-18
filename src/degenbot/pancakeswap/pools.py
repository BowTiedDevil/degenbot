"""PancakeSwap V2/V3 pool implementations.

ADR-005 slice 7 step 4b: ``PancakeswapV2Pool`` (a hollow subclass carrying
only a ``variant`` ClassVar + V2 fee/ABI constants, both superseded by the
``pancakeswap-v2`` DexIdentity preset) is deleted — the canonical
``LiquidityPool`` is registered for the PancakeSwap V2 factory (see
``pancakeswap/__init__.py``). ``PancakeswapV3Pool`` is retained.
"""

from typing import ClassVar

from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool


class PancakeswapV3Pool(UniswapV3Pool):
    """PancakeswapV3Pool class."""

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
