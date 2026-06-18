"""PancakeSwap pool trackers for event-driven updates.

ADR-005 slice 7 step 4b: ``PancakeswapV2PoolTracker`` is deleted (its pool
class is gone — the canonical ``LiquidityPool`` + ``UniswapV2PoolTracker`` is
used). ``PancakeswapV3PoolTracker`` is retained.
"""

from degenbot.pancakeswap.pools import PancakeswapV3Pool
from degenbot.uniswap.trackers import UniswapV3PoolTracker


class PancakeswapV3PoolTracker(UniswapV3PoolTracker, pool_factory=PancakeswapV3Pool):
    """Track PancakeSwap V3 pool events."""
