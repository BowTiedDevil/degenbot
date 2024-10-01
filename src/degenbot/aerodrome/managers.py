from typing import TypeAlias

from ..uniswap.managers import UniswapV2PoolManager, UniswapV3PoolManager


class AerodromeV2PoolManager(UniswapV2PoolManager):
    from .pools import AerodromeV2Pool as pool_creator

    PoolCreatorType: TypeAlias = pool_creator


class AerodromeV3PoolManager(UniswapV3PoolManager):
    from .pools import AerodromeV3Pool as pool_creator

    PoolCreatorType: TypeAlias = pool_creator
