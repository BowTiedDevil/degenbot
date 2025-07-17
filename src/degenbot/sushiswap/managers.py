from degenbot.sushiswap.pools import SushiswapV2Pool, SushiswapV3Pool
from degenbot.uniswap.managers import UniswapV2PoolManager, UniswapV3PoolManager


class SushiswapV2PoolManager(UniswapV2PoolManager, pool_factory=SushiswapV2Pool):
    type Pool = SushiswapV2Pool


class SushiswapV3PoolManager(UniswapV3PoolManager, pool_factory=SushiswapV3Pool):
    type Pool = SushiswapV3Pool
