from typing import ClassVar

from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool


class SwapbasedV2Pool(UniswapV2Pool):
    variant: ClassVar[str | None] = "swapbased"
