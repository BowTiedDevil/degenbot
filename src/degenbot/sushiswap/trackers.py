"""SushiSwap pool trackers for event-driven updates.

ADR-005 slice 7 step 4b: ``SushiswapV2PoolTracker`` is deleted (its pool
class is gone — the canonical ``LiquidityPool`` + ``UniswapV2PoolTracker`` is
used). ``SushiswapV3PoolTracker`` is retained.
"""

from degenbot.sushiswap.pools import SushiswapV3Pool
from degenbot.uniswap.trackers import UniswapV3PoolTracker


class SushiswapV3PoolTracker(UniswapV3PoolTracker, pool_factory=SushiswapV3Pool):
    """SushiswapV3PoolTracker class."""
