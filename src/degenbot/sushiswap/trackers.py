"""SushiSwap pool trackers for event-driven updates.

ADR-005 slice 7 step 4b: ``SushiswapV2PoolTracker`` is deleted (its pool
class is gone — the canonical ``UniswapV2Pool`` + ``UniswapV2PoolTracker`` is
used). C8b: ``SushiswapV3Pool`` is likewise collapsed — the canonical
``UniswapV3Pool`` (a V3 fork with a byte-identical slot0/tick layout) is
registered for the SushiSwap V3 factory, keyed on the ``sushiswap`` DB-kind
variant in ``deployments.json`` (no ``SushiswapV3`` DexIdentity preset exists;
the V2 one does).
"""

from degenbot.uniswap.trackers import UniswapV3PoolTracker
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool


class SushiswapV3PoolTracker(UniswapV3PoolTracker, pool_factory=UniswapV3Pool):
    """SushiswapV3PoolTracker class."""
