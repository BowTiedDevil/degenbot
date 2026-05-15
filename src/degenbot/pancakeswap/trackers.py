from degenbot.pancakeswap.pools import PancakeswapV2Pool, PancakeswapV3Pool
from degenbot.uniswap.trackers import UniswapV2PoolTracker, UniswapV3PoolTracker


class PancakeswapV2PoolTracker(UniswapV2PoolTracker, pool_factory=PancakeswapV2Pool): ...


class PancakeswapV3PoolTracker(UniswapV3PoolTracker, pool_factory=PancakeswapV3Pool): ...
