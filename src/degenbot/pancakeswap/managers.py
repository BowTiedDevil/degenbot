from typing import TypeAlias

from ..uniswap.managers import UniswapV2PoolManager, UniswapV3PoolManager
from .pools import PancakeV2Pool, PancakeV3Pool


class PancakeV2PoolManager(UniswapV2PoolManager):
    Pool: TypeAlias = PancakeV2Pool


class PancakeV3PoolManager(UniswapV3PoolManager):
    Pool: TypeAlias = PancakeV3Pool
