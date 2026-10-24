"""PancakeSwap V2/V3 pool implementations.

There is no ``PancakeswapV2Pool`` subclass: the ``pancakeswap-v2`` DexIdentity
preset carries the V2 variant + fee/ABI constants, so the canonical
``UniswapV2Pool`` is registered for the PancakeSwap V2 factory (see
``pancakeswap/__init__.py``). ``PancakeswapV3Pool`` is retained.
"""

from typing import ClassVar

from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool


class PancakeswapV3Pool(UniswapV3Pool):
    """PancakeswapV3Pool class."""

    variant: ClassVar[str | None] = "pancakeswap"
