"""PancakeSwap pool trackers for event-driven updates."""

from degenbot.pancakeswap.pools import PancakeswapV2Pool, PancakeswapV3Pool
from degenbot.uniswap.trackers import UniswapV2PoolTracker, UniswapV3PoolTracker


class PancakeswapV2PoolTracker(UniswapV2PoolTracker, pool_factory=PancakeswapV2Pool):
    """Track PancakeSwap V2 pool events."""

    ...


class PancakeswapV3PoolTracker(UniswapV3PoolTracker, pool_factory=PancakeswapV3Pool):
    """Track PancakeSwap V3 pool events."""

    ...
