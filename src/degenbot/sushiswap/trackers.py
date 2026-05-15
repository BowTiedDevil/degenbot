from degenbot.sushiswap.pools import SushiswapV2Pool, SushiswapV3Pool
from degenbot.uniswap.trackers import UniswapV2PoolTracker, UniswapV3PoolTracker


class SushiswapV2PoolTracker(UniswapV2PoolTracker, pool_factory=SushiswapV2Pool):
    type Pool = SushiswapV2Pool


class SushiswapV3PoolTracker(UniswapV3PoolTracker, pool_factory=SushiswapV3Pool):
    type Pool = SushiswapV3Pool
