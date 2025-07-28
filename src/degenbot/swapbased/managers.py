from degenbot.swapbased.pools import SwapbasedV2Pool
from degenbot.uniswap.managers import UniswapV2PoolManager


class SwapbasedV2PoolManager(UniswapV2PoolManager, pool_factory=SwapbasedV2Pool):
    type Pool = SwapbasedV2Pool
