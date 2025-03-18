from degenbot.sushiswap.pools import SushiswapV2Pool, SushiswapV3Pool
from degenbot.uniswap.managers import UniswapV2PoolManager, UniswapV3PoolManager


class SushiswapV2PoolManager(UniswapV2PoolManager):
    type Pool = SushiswapV2Pool


class SushiswapV3PoolManager(UniswapV3PoolManager):
    type Pool = SushiswapV3Pool
