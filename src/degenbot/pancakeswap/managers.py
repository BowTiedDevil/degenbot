from typing import TypeAlias

from degenbot.pancakeswap.pools import PancakeV2Pool, PancakeV3Pool
from degenbot.uniswap.managers import UniswapV2PoolManager, UniswapV3PoolManager


class PancakeV2PoolManager(UniswapV2PoolManager):
    Pool: TypeAlias = PancakeV2Pool


class PancakeV3PoolManager(UniswapV3PoolManager):
    Pool: TypeAlias = PancakeV3Pool
