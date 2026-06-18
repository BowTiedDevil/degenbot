"""SushiSwap V2/V3 pool implementations.

ADR-005 slice 7 step 4b: ``SushiswapV2Pool`` (a hollow subclass carrying only
a ``variant`` ClassVar) is deleted — the canonical ``LiquidityPool`` is
registered for the SushiSwap V2 factory (see ``sushiswap/__init__.py``), keyed
on the ``sushiswap-v2`` DexIdentity preset. ``SushiswapV3Pool`` is retained
(V3 has its own companion; not part of this collapse).
"""

from typing import ClassVar

from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool


class SushiswapV3Pool(UniswapV3Pool):
    """SushiswapV3Pool class."""

    variant: ClassVar[str | None] = "sushiswap"
