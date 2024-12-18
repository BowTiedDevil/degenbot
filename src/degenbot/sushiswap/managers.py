from typing import TypeAlias

from degenbot.sushiswap.pools import SushiswapV2Pool, SushiswapV3Pool
from degenbot.uniswap.managers import UniswapV2PoolManager, UniswapV3PoolManager


class SushiswapV2PoolManager(UniswapV2PoolManager):
    Pool: TypeAlias = SushiswapV2Pool


class SushiswapV3PoolManager(UniswapV3PoolManager):
    Pool: TypeAlias = SushiswapV3Pool
