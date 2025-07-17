from collections.abc import Sequence

from .liquidity_pool import AbstractLiquidityPool


class AbstractArbitrage:
    id: str
    swap_pools: Sequence[AbstractLiquidityPool]
