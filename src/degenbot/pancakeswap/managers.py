from degenbot.pancakeswap.pools import PancakeswapV2Pool, PancakeswapV3Pool
from degenbot.uniswap.managers import UniswapV2PoolManager, UniswapV3PoolManager


class PancakeswapV2PoolManager(UniswapV2PoolManager, pool_factory=PancakeswapV2Pool): ...


class PancakeswapV3PoolManager(UniswapV3PoolManager, pool_factory=PancakeswapV3Pool): ...
