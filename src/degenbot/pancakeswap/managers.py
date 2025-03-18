from degenbot.pancakeswap.pools import PancakeV2Pool, PancakeV3Pool
from degenbot.uniswap.managers import UniswapV2PoolManager, UniswapV3PoolManager


class PancakeV2PoolManager(UniswapV2PoolManager):
    type Pool = PancakeV2Pool


class PancakeV3PoolManager(UniswapV3PoolManager):
    type Pool = PancakeV3Pool
