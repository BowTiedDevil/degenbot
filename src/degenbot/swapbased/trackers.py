"""SwapBased pool tracker for event-driven updates."""

from degenbot.swapbased.pools import SwapbasedV2Pool
from degenbot.uniswap.trackers import UniswapV2PoolTracker


class SwapbasedV2PoolTracker(UniswapV2PoolTracker, pool_factory=SwapbasedV2Pool):
    """SwapbasedV2PoolTracker class."""

    type Pool = SwapbasedV2Pool
