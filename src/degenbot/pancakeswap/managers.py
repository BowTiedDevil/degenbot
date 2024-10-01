from typing import TypeAlias

from ..uniswap.managers import UniswapV3PoolManager


class PancakeV3PoolManager(UniswapV3PoolManager):
    from .pools import PancakeV3Pool as pool_creator

    PoolCreatorType: TypeAlias = pool_creator
